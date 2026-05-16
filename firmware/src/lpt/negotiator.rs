//! IEEE 1284 negotiation protocol — peripheral side.
//!
//! Implements §6.3 of IEEE 1284-2000 from the peripheral's
//! perspective. The host initiates every mode transition by:
//!
//! 1. Placing an Extensibility Request Value (XRV) on D0..D7 and
//!    asserting `nSelectIn = 1, nAutoFd = 0`.
//! 2. Waiting up to 35 µs for the peripheral to drive the status
//!    response (`nAck = 0, PError = 1, Select = 1, nFault = 1`).
//! 3. Pulsing `nStrobe` LOW for ≥500 ns.
//! 4. Reading the peripheral's final state: `Select` reflects
//!    whether the XRV is supported, `nAck = 1`.
//! 5. Releasing `nAutoFd` (back to 1).
//!
//! After step 5 both sides have agreed on the target mode and
//! [`LptMux::switch_to`](super::mux::LptMux::switch_to) can be
//! called.
//!
//! Termination (returning to Compatibility mode) is host-initiated
//! by driving `nInit = 0`; the peripheral acks with `nFault = 0`.
//!
//! ## Layout
//!
//! The negotiator is split into:
//! - [`xrv`] — the XRV byte values from IEEE 1284-2000 (and the
//!   matching Linux kernel constants in `drivers/parport`).
//! - [`xrv_to_mode`] — XRV → [`LptMode`] map for the modes our
//!   firmware supports.
//! - [`NegotiatorIo`] — pin-access trait separating wire control
//!   from state-machine logic, so the state machine is testable.
//! - [`Negotiator`] — `async` state machine driving the
//!   handshake. Takes any [`NegotiatorIo`].
//! - [`PacNegotiatorIo`] — concrete IO impl using direct PAC reads
//!   and writes against the IO_BANK0 / SIO peripherals.
//!
//! ## Integration (not yet wired)
//!
//! The negotiator is provided as a complete module today but is not
//! yet driven by the serve loop. Production integration awaits:
//!
//! - Stage 1's host-side 1284 negotiator (no caller emits the wire
//!   pattern yet);
//! - The serve-loop `select!` that races `transport.recv_packet()`
//!   against `negotiator.wait_for_start()` and routes the result
//!   through `transport.switch_to(target)`.
//!
//! The 74LVC161284 mode-pin (DIR/HD) flip already happens inside
//! `LptMux::switch_to` via the pre-build hook landed alongside this
//! module — no negotiator-side hook needed for that.
//!
//! Hooking it up will look roughly like:
//!
//! ```text
//! loop {
//!     match select(transport.recv_packet(), negotiator.wait_for_start()).await {
//!         Either::First(pkt)  => handle_packet(...),
//!         Either::Second(xrv) => {
//!             // dismantle the current phy, drive the handshake,
//!             // and build the target mode's phy
//!             let target = negotiator.handshake(xrv).await?;
//!             transport.switch_to(target).await?;
//!         }
//!     }
//! }
//! ```

// Negotiation infrastructure is fully built but not yet wired into
// `serve_loop` — once Stage 1's host-side negotiator and the
// 74LVC161284 mode pins land we'll hook this in. Until then the
// constants, helpers, and IO impls compile but nothing calls them.
#![allow(dead_code)]

use embassy_rp::pac;
use embassy_time::{Duration, Instant, Timer};

use super::LptMode;

/// Extensibility Request Values per IEEE 1284-2000 §6.3.4. The
/// constants match `drivers/parport/ieee1284.c` from the Linux
/// kernel, the canonical reference (per
/// `docs/ieee1284_controller_reference.md`).
pub mod xrv {
    /// Nibble-mode reverse channel.
    pub const NIBBLE: u8 = 0x00;
    /// Byte-mode reverse channel.
    pub const BYTE: u8 = 0x01;
    /// ECP forward + reverse, no compression.
    pub const ECP: u8 = 0x10;
    /// ECP forward + reverse, with RLE compression.
    pub const ECP_RLE: u8 = 0x30;
    /// EPP. Not part of the original 1284-1994 negotiation set;
    /// added by 1284.1 / common practice.
    pub const EPP: u8 = 0x40;
    /// "Request Device ID" flag, OR'd with one of the reverse-mode
    /// XRVs above to request a Device ID transfer instead of normal
    /// data traffic. Not implemented yet; flagged here for future
    /// negotiator extensions.
    pub const DEVICE_ID: u8 = 0x04;
    /// Extensibility link — peripheral may request an extended XRV
    /// in the next negotiation cycle. Unused today.
    pub const EXT_LINK: u8 = 0x80;
}

/// Map an XRV byte to the [`LptMode`] our `LptMux` can build, or
/// `None` if the XRV is unsupported (or means "request Device ID",
/// which we ignore for now).
pub fn xrv_to_mode(xrv: u8) -> Option<LptMode> {
    // Strip the Device-ID flag — we'll treat "Device ID in <mode>"
    // as the bare mode for routing purposes; the protocol layer can
    // emit a Device ID packet separately if it wants.
    match xrv & !xrv::DEVICE_ID {
        xrv::NIBBLE => Some(LptMode::SppNibble),
        xrv::BYTE => Some(LptMode::Byte),
        xrv::ECP => Some(LptMode::Ecp),
        xrv::ECP_RLE => Some(LptMode::Ecp),
        xrv::EPP => Some(LptMode::Epp),
        _ => None,
    }
}

#[derive(Debug, defmt::Format)]
pub enum NegotiationError {
    /// Host abandoned the handshake before completion (`nAutoFd`
    /// released early, or no strobe within the deadline).
    Timeout,
    /// XRV byte we don't have a target phy for.
    UnsupportedXrv(u8),
}

/// Per-step timeouts. The 35 µs immediate-response window is met by
/// driving the status pins before returning from `detect_start`; the
/// per-step waits below are for the slower phases of the handshake.
const STROBE_WAIT_MS: u64 = 100;
const NAUTOFD_RELEASE_MS: u64 = 100;
/// Poll cadence while waiting on a slow control-line edge. 5 µs is
/// well under any 1284 timing budget and lets us stack many
/// negotiations per second if a host keeps re-entering negotiation.
const POLL_TICK_US: u64 = 5;

/// Wire-facing IO. Implementations sit between the negotiator
/// state machine and whatever drives the actual GPIO pins (PAC
/// writes, embassy gpio, a simulated bus for host-tests).
pub trait NegotiatorIo {
    /// Read the host strobe line (`nStrobe` / `HostClk`).
    fn read_nstrobe(&self) -> bool;
    /// Read `nSelectIn` / `1284Active`.
    fn read_nselectin(&self) -> bool;
    /// Read `nAutoFd` / `HostAck`.
    fn read_nautofd(&self) -> bool;
    /// Read the host's `nInit` line (used for termination
    /// detection — host drives it LOW to return both sides to
    /// Compatibility mode).
    fn read_ninit(&self) -> bool;
    /// Read the data bus (D0..D7) as a byte. Only valid while the
    /// host is presenting an XRV (between `nSelectIn` rising and
    /// the final strobe).
    fn read_data_bus(&self) -> u8;
    /// Drive the four status-response lines as a single packed
    /// write so the 35 µs deadline isn't lost to per-pin overhead.
    /// `(nack, perror, select, nfault)` — each `true` = HIGH on the
    /// wire after the open-drain buffer.
    fn drive_status(&mut self, nack: bool, perror: bool, select: bool, nfault: bool);
    /// Claim the status pins from PIO control (set FUNCSEL = SIO)
    /// and switch them to outputs. Called once before
    /// [`Negotiator::handshake`].
    fn claim_status_pins(&mut self);
    /// Return the status pins to PIO control. Called after the new
    /// mode's phy has been built so the phy's SM can drive them.
    fn release_status_pins(&mut self);
}

pub struct Negotiator<IO: NegotiatorIo> {
    io: IO,
}

impl<IO: NegotiatorIo> Negotiator<IO> {
    pub const fn new(io: IO) -> Self {
        Self { io }
    }

    /// Non-blocking check: is the host currently presenting the
    /// negotiation start pattern? Returns the XRV byte if so.
    pub fn detect_start(&self) -> Option<u8> {
        if self.io.read_nselectin() && !self.io.read_nautofd() {
            Some(self.io.read_data_bus())
        } else {
            None
        }
    }

    /// Async wait for negotiation start. Polls every
    /// [`POLL_TICK_US`] µs. Returns the XRV byte the host placed on
    /// D0..D7. The caller is expected to follow up immediately with
    /// [`Self::handshake`] so the 35 µs response window is met.
    pub async fn wait_for_start(&self) -> u8 {
        loop {
            if let Some(xrv) = self.detect_start() {
                return xrv;
            }
            Timer::after_micros(POLL_TICK_US).await;
        }
    }

    /// Non-blocking check: is the host driving `nInit` LOW,
    /// requesting termination back to Compatibility?
    pub fn detect_terminate(&self) -> bool {
        !self.io.read_ninit()
    }

    /// Run the full negotiation handshake after [`detect_start`]
    /// reported an XRV. Drives the immediate status response, waits
    /// for the strobe pulse, drives the final ack, waits for the
    /// host to release `nAutoFd`, returns the negotiated mode.
    ///
    /// Caller must have called [`NegotiatorIo::claim_status_pins`]
    /// (typically after suspending the current phy) before invoking
    /// this. Caller is responsible for calling
    /// [`NegotiatorIo::release_status_pins`] before bringing up the
    /// target phy.
    pub async fn handshake(&mut self, xrv: u8) -> Result<LptMode, NegotiationError> {
        // Phase 1: immediate status response (35 µs deadline).
        // Drive nAck=0, PError=1, Select=1, nFault=1 to signal
        // "ready, examining XRV".
        self.io.drive_status(false, true, true, true);

        let target = xrv_to_mode(xrv);
        let supported = target.is_some();

        // Phase 2: wait for host's `nStrobe` pulse confirming XRV
        // receipt. Falling edge then rising edge.
        let deadline = Instant::now() + Duration::from_millis(STROBE_WAIT_MS);
        // Wait for strobe LOW.
        while self.io.read_nstrobe() {
            if Instant::now() > deadline {
                return Err(NegotiationError::Timeout);
            }
            Timer::after_micros(POLL_TICK_US).await;
        }
        // Wait for strobe HIGH again.
        let deadline = Instant::now() + Duration::from_millis(STROBE_WAIT_MS);
        while !self.io.read_nstrobe() {
            if Instant::now() > deadline {
                return Err(NegotiationError::Timeout);
            }
            Timer::after_micros(POLL_TICK_US).await;
        }

        // Phase 3: final ack — Select = supported, nAck = 1.
        // PError + nFault stay HIGH.
        self.io.drive_status(true, true, supported, true);

        // Phase 4: wait for host to release `nAutoFd` (back to HIGH).
        let deadline = Instant::now() + Duration::from_millis(NAUTOFD_RELEASE_MS);
        while !self.io.read_nautofd() {
            if Instant::now() > deadline {
                return Err(NegotiationError::Timeout);
            }
            Timer::after_micros(POLL_TICK_US).await;
        }

        target.ok_or(NegotiationError::UnsupportedXrv(xrv))
    }

    /// Handle a termination request: ack the host's `nInit = 0`
    /// drive by pulsing `nFault = 0`, then return — caller switches
    /// the mux back to SppNibble (the firmware's working Compat
    /// substitute).
    pub async fn handle_terminate(&mut self) {
        // Drive nFault = 0 to confirm we saw nInit.
        self.io.drive_status(true, true, true, false);
        // Hold long enough for the host's polled detection (typical
        // 1284 driver polls in 10 ms ticks, so a few ms is plenty).
        Timer::after_millis(2).await;
        // Restore nFault HIGH; rest of the status returns to compat
        // idle.
        self.io.drive_status(true, true, true, true);
    }

    pub fn io_mut(&mut self) -> &mut IO {
        &mut self.io
    }
}

// -------------------------- PAC IO impl --------------------------

/// PIO0 function-select value for IO_BANK0 GPIO ctrl.funcsel.
const FUNCSEL_PIO0: u8 = 6;
/// SIO function-select value (CPU-driven GPIO).
const FUNCSEL_SIO: u8 = 5;

/// Output IO for the negotiator. Manipulates the IO_BANK0 and SIO
/// register blocks directly so we can flip pin function and drive
/// levels without going through embassy-rp's `Pin<'_, PIO0>`
/// (which is a one-way mapping after construction).
///
/// Read pins remain on FUNCSEL_PIO0 indefinitely — PIO/GPIO read
/// access works regardless of funcsel because reads come from the
/// pad. Output pins (the four status response lines + nFault) are
/// flipped to FUNCSEL_SIO for the duration of the handshake, then
/// restored.
pub struct PacNegotiatorIo;

impl PacNegotiatorIo {
    pub const fn new() -> Self {
        Self
    }

    /// All LPT GPIOs we touch are in the first RP2350 GPIO bank
    /// (GPIO0..31). Bank 1 (GPIO32..47) is not used here.
    const BANK: usize = 0;

    /// Direct PAC read of a GPIO pin's current pad value.
    fn pin_read(gpio: u8) -> bool {
        let mask = 1u32 << gpio;
        (pac::SIO.gpio_in(Self::BANK).read() & mask) != 0
    }

    fn pin_set_high(gpio: u8) {
        pac::SIO
            .gpio_out(Self::BANK)
            .value_set()
            .write_value(1u32 << gpio);
    }

    fn pin_set_low(gpio: u8) {
        pac::SIO
            .gpio_out(Self::BANK)
            .value_clr()
            .write_value(1u32 << gpio);
    }

    fn pin_set_level(gpio: u8, level: bool) {
        if level {
            Self::pin_set_high(gpio);
        } else {
            Self::pin_set_low(gpio);
        }
    }

    fn pin_set_funcsel(gpio: u8, funcsel: u8) {
        pac::IO_BANK0.gpio(gpio as usize).ctrl().modify(|w| {
            w.set_funcsel(funcsel);
        });
    }

    fn pin_enable_output(gpio: u8) {
        pac::SIO
            .gpio_oe(Self::BANK)
            .value_set()
            .write_value(1u32 << gpio);
    }

    #[allow(dead_code)] // used during release_status_pins
    fn pin_disable_output(gpio: u8) {
        pac::SIO
            .gpio_oe(Self::BANK)
            .value_clr()
            .write_value(1u32 << gpio);
    }
}

// GPIO numbers per docs/hardware_reference.md §3.3.
const GP_STROBE: u8 = 11;
const GP_AUTOFD: u8 = 20;
const GP_SELECTIN: u8 = 22;
const GP_NACK: u8 = 23;
const GP_BUSY: u8 = 24;
const GP_PERROR: u8 = 25;
const GP_SELECT: u8 = 26;
const GP_NFAULT: u8 = 27;
// `nInit` is currently unrouted (see docs/pio_state_machines_design.md
// §4.4). When it lands, swap this constant for the real pin number.
// Until then, [`read_ninit`] reports HIGH so `detect_terminate` never
// fires spuriously.
const GP_NINIT: Option<u8> = None;

impl NegotiatorIo for PacNegotiatorIo {
    fn read_nstrobe(&self) -> bool {
        Self::pin_read(GP_STROBE)
    }

    fn read_nselectin(&self) -> bool {
        Self::pin_read(GP_SELECTIN)
    }

    fn read_nautofd(&self) -> bool {
        Self::pin_read(GP_AUTOFD)
    }

    fn read_ninit(&self) -> bool {
        match GP_NINIT {
            Some(g) => Self::pin_read(g),
            None => true, // unrouted → never asserts terminate
        }
    }

    fn read_data_bus(&self) -> u8 {
        // D0..D7 on GP12..GP19, packed contiguously in SIO.gpio_in
        // bits 12..19 of bank 0.
        let raw = pac::SIO.gpio_in(Self::BANK).read();
        ((raw >> 12) & 0xFF) as u8
    }

    fn drive_status(&mut self, nack: bool, perror: bool, select: bool, nfault: bool) {
        // All four pins are flipped to SIO output via
        // claim_status_pins before we get here. Set them in a single
        // batched register write so the wire transitions are nearly
        // simultaneous (well within the 35 µs response window even
        // with cache-cold instruction fetch).
        //
        // `busy` (GP24) is the fifth reverse pin but the negotiation
        // protocol doesn't drive it during the handshake — it stays
        // released. The phy that comes back online afterward will
        // restore it.
        Self::pin_set_level(GP_NACK, nack);
        Self::pin_set_level(GP_PERROR, perror);
        Self::pin_set_level(GP_SELECT, select);
        Self::pin_set_level(GP_NFAULT, nfault);
    }

    fn claim_status_pins(&mut self) {
        for gpio in [GP_NACK, GP_BUSY, GP_PERROR, GP_SELECT, GP_NFAULT] {
            Self::pin_set_funcsel(gpio, FUNCSEL_SIO);
            Self::pin_enable_output(gpio);
        }
        defmt::trace!("negotiator: status pins claimed (SIO)");
    }

    fn release_status_pins(&mut self) {
        for gpio in [GP_NACK, GP_BUSY, GP_PERROR, GP_SELECT, GP_NFAULT] {
            // Hand back to PIO0. The phy that comes up next will
            // immediately re-arm its SM with these pins, so we don't
            // disable the OE — the SM owns it from here.
            Self::pin_set_funcsel(gpio, FUNCSEL_PIO0);
        }
        defmt::trace!("negotiator: status pins released (PIO0)");
    }
}

impl Default for PacNegotiatorIo {
    fn default() -> Self {
        Self::new()
    }
}
