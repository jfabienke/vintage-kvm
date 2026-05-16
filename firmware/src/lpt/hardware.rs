//! Persistent LPT hardware ownership for the mode-swap path.
//!
//! Per `docs/pio_state_machines_design.md` §10.6 a mode transition is
//! "drain → disable → unload → load → reconfigure → enable", not
//! "drop and rebuild from raw peripherals". The Pico peripherals
//! (Common, state machines, DMA channels, GPIOs) only get consumed
//! once at boot — they have to live somewhere that outlasts any one
//! active phy.
//!
//! [`LptHardware`] is that home. It holds:
//!
//! - The PIO0 `Common`, used by every phy to load programs and bind
//!   pins.
//! - 14 `Pin<'static, PIO0>` handles — strobe + D0..D7 (forward) and
//!   the 5 reverse status pins. `make_pio_pin` is one-way (consumes
//!   `Peri<PIN_*>`, returns a `Pin` that lives for the program's
//!   duration), so we do all 14 once at boot.
//! - Parked SM0/SM1 + the nibble-out DMA channel and a Peri token for
//!   the compat-in ring DMA. Phys `take()` these on construction and
//!   `dismantle()` returns them to the parking lot.
//!
//! Phys hold whatever `LoadedProgram` handles they need; on dismantle
//! they pass `LoadedProgram::used_memory` back through
//! `unsafe { common.free_instr(...) }` so the instruction-memory
//! slots are recycled and the next mode's programs fit.

use embassy_rp::Peri;
use embassy_rp::dma;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::pac;
use embassy_rp::peripherals::{
    DMA_CH3, DMA_CH4, PIN_0, PIN_11, PIN_12, PIN_13, PIN_14, PIN_15, PIN_16, PIN_17, PIN_18,
    PIN_19, PIN_20, PIN_22, PIN_23, PIN_24, PIN_25, PIN_26, PIN_27, PIN_29, PIO0,
};
use embassy_rp::pio::{Common, Pin, StateMachine};

use crate::irqs::DmaIrqs;

use super::LptMode;

/// FUNCSEL value for SIO (CPU-driven GPIO) on IO_BANK0. Matches
/// `negotiator::PacNegotiatorIo::FUNCSEL_SIO` and
/// `pio_dir_follower::FUNCSEL_SIO`.
const FUNCSEL_SIO: u8 = 5;
/// GP29 absolute number — DIR pin on the 74LVC161284.
const GP_DIR: u8 = 29;

/// The 16 PIO0-mapped pins the LPT side uses across all 1284 modes.
/// Made once at boot so mode swaps don't have to deal with un-mapping
/// or re-mapping pins (embassy-rp has no `release_pio_pin` — pins
/// return to GPIO only when the owning `Common` itself drops).
///
/// Field names follow `docs/hardware_reference.md` §3.3 and are
/// signal-multiplexed across modes:
/// - `strobe`  GP11 = nStrobe / HostClk
/// - `d0..d7`  GP12..19 = Parallel Data 0..7 (bidirectional)
/// - `auto_fd` GP20 = nAutoFd / HostAck / nDataStb
/// - `slctin`  GP22 = nSelectIn / 1284Active
/// - `nack`    GP23 = nAck / PeriphClk / nAckReverse
/// - `busy`    GP24 = Busy / PeriphAck / nWait
/// - `perror`  GP25 = PError / nAckReverse
/// - `select`  GP26 = Select / Xflag
/// - `nfault`  GP27 = nFault / nPeriphRequest
pub struct LptPins {
    pub strobe: Pin<'static, PIO0>,
    pub d0: Pin<'static, PIO0>,
    pub d1: Pin<'static, PIO0>,
    pub d2: Pin<'static, PIO0>,
    pub d3: Pin<'static, PIO0>,
    pub d4: Pin<'static, PIO0>,
    pub d5: Pin<'static, PIO0>,
    pub d6: Pin<'static, PIO0>,
    pub d7: Pin<'static, PIO0>,
    pub auto_fd: Pin<'static, PIO0>,
    // GP22 = nSelectIn / 1284Active / EPP nAddrStb. Reserved for
    // future EPP address-cycle support and for asserting 1284Active
    // during negotiation; no current consumer.
    #[allow(dead_code)]
    pub slctin: Pin<'static, PIO0>,
    pub nack: Pin<'static, PIO0>,
    pub busy: Pin<'static, PIO0>,
    pub perror: Pin<'static, PIO0>,
    pub select: Pin<'static, PIO0>,
    pub nfault: Pin<'static, PIO0>,
    /// GP29 = 74LVC161284 DIR. Dual-mode: SIO when EPP is *not*
    /// active (CPU drives via [`LptHardware::drive_dir_sio`]); PIO0
    /// when EPP is active (the DIR follower SM mirrors `nWrite`).
    /// `make_pio_pin` happens once at boot so the follower has a
    /// `Pin` handle for its `set_out_pins`; the FUNCSEL flips
    /// between SIO and PIO0 as the active phy changes.
    pub dir: Pin<'static, PIO0>,
}

pub struct LptHardware {
    pub common: Common<'static, PIO0>,
    pub pins: LptPins,
    pub parked_sm0: Option<StateMachine<'static, PIO0, 0>>,
    pub parked_sm1: Option<StateMachine<'static, PIO0, 1>>,
    /// SM2 is reserved for the EPP "DIR follower" mirror — see
    /// `super::pio_dir_follower`. Parked here when EPP is not the
    /// active mode; taken by [`super::epp::EppPhy::build`] when EPP
    /// comes up and returned on dismantle.
    pub parked_sm2: Option<StateMachine<'static, PIO0, 2>>,
    /// `dma::Channel` for the nibble-out path. Phys borrow ownership
    /// of it for their lifetime and hand it back via `dismantle`.
    pub parked_dma_ch4: Option<dma::Channel<'static>>,
    /// Compat-in ring DMA token. Operated entirely through PAC writes
    /// in `super::ring_dma`; held here for compile-time exclusivity.
    /// `Option` only so initial construction can move it in.
    _dma_ch3: Option<Peri<'static, DMA_CH3>>,
    /// 74LVC161284 high-drive control (GP0 → chip pin 1). LOW =
    /// open-drain (IEEE 1284-I); HIGH = totem-pole (IEEE 1284-II).
    /// Set per target mode by [`Self::set_transceiver_mode`].
    hd: Output<'static>,
}

impl LptHardware {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut common: Common<'static, PIO0>,
        sm0: StateMachine<'static, PIO0, 0>,
        sm1: StateMachine<'static, PIO0, 1>,
        sm2: StateMachine<'static, PIO0, 2>,
        dma_ch3: Peri<'static, DMA_CH3>,
        dma_ch4: Peri<'static, DMA_CH4>,
        hd_pin: Peri<'static, PIN_0>,
        strobe: Peri<'static, PIN_11>,
        d0: Peri<'static, PIN_12>,
        d1: Peri<'static, PIN_13>,
        d2: Peri<'static, PIN_14>,
        d3: Peri<'static, PIN_15>,
        d4: Peri<'static, PIN_16>,
        d5: Peri<'static, PIN_17>,
        d6: Peri<'static, PIN_18>,
        d7: Peri<'static, PIN_19>,
        auto_fd: Peri<'static, PIN_20>,
        slctin: Peri<'static, PIN_22>,
        nack: Peri<'static, PIN_23>,
        busy: Peri<'static, PIN_24>,
        perror: Peri<'static, PIN_25>,
        select: Peri<'static, PIN_26>,
        nfault: Peri<'static, PIN_27>,
        dir_pin: Peri<'static, PIN_29>,
    ) -> Self {
        let pins = LptPins {
            strobe: common.make_pio_pin(strobe),
            d0: common.make_pio_pin(d0),
            d1: common.make_pio_pin(d1),
            d2: common.make_pio_pin(d2),
            d3: common.make_pio_pin(d3),
            d4: common.make_pio_pin(d4),
            d5: common.make_pio_pin(d5),
            d6: common.make_pio_pin(d6),
            d7: common.make_pio_pin(d7),
            auto_fd: common.make_pio_pin(auto_fd),
            slctin: common.make_pio_pin(slctin),
            nack: common.make_pio_pin(nack),
            busy: common.make_pio_pin(busy),
            perror: common.make_pio_pin(perror),
            select: common.make_pio_pin(select),
            nfault: common.make_pio_pin(nfault),
            dir: common.make_pio_pin(dir_pin),
        };

        let dma_ch4_channel = dma::Channel::new(dma_ch4, DmaIrqs);

        // Boot defaults match Compatibility mode (the initial SppNibble
        // phy): open-drain outputs, host writes data forward. The first
        // `LptMux::switch_to` after boot reasserts these explicitly via
        // `set_transceiver_mode`.
        let hd = Output::new(hd_pin, Level::Low);

        // DIR was just `make_pio_pin`'d (FUNCSEL=PIO0). Flip it back
        // to SIO for the boot default; the DIR follower SM flips it
        // to PIO0 only when EPP enters.
        Self::set_dir_funcsel(FUNCSEL_SIO);
        Self::enable_dir_sio_output();
        Self::drive_dir_sio(Level::Low);

        Self {
            common,
            pins,
            parked_sm0: Some(sm0),
            parked_sm1: Some(sm1),
            parked_sm2: Some(sm2),
            parked_dma_ch4: Some(dma_ch4_channel),
            _dma_ch3: Some(dma_ch3),
            hd,
        }
    }

    /// Raw PAC: flip GP29's FUNCSEL between SIO (CPU drive) and
    /// PIO0 (DIR-follower drive). Used by [`super::pio_dir_follower`]
    /// during build and dismantle.
    fn set_dir_funcsel(funcsel: u8) {
        pac::IO_BANK0.gpio(GP_DIR as usize).ctrl().modify(|w| {
            w.set_funcsel(funcsel);
        });
    }

    /// Enable GP29 as an SIO output. Idempotent — called once at
    /// boot and again whenever the DIR follower hands the pin back.
    fn enable_dir_sio_output() {
        pac::SIO
            .gpio_oe(0)
            .value_set()
            .write_value(1u32 << GP_DIR);
    }

    /// Drive GP29's pad LOW or HIGH via the SIO output register.
    /// Only meaningful when GP29's FUNCSEL is SIO — i.e. when the
    /// DIR follower SM is *not* active.
    fn drive_dir_sio(level: Level) {
        let mask = 1u32 << GP_DIR;
        match level {
            Level::High => pac::SIO.gpio_out(0).value_set().write_value(mask),
            Level::Low => pac::SIO.gpio_out(0).value_clr().write_value(mask),
        }
    }

    /// Drive the 74LVC161284's `DIR` (GP29) and `HD` (GP0) inputs to
    /// the values [`docs/hardware_reference.md` §11.3] prescribes for
    /// `target`. Must be called *between* the old phy's dismantle and
    /// the target phy's build, so the chip's data-bus direction and
    /// driver style match whatever the new phy expects on the wire.
    ///
    /// `dir` is set to whichever side normally drives the data bus
    /// *first* in the target mode:
    /// - Compat/Nibble: host writes → `DIR = L`
    /// - Byte:          start in forward → `DIR = L`; the byte_rev
    ///   sub-phase flips it via [`Self::set_data_direction`]
    /// - EPP:           start in forward → `DIR = L`; per-cycle flip
    ///   is the responsibility of the future "DIR follower" PIO SM
    /// - ECP:           start in forward burst → `DIR = L`; reverse
    ///   bursts flip via [`Self::set_data_direction`]
    pub fn set_transceiver_mode(&mut self, target: LptMode) {
        let (dir_level, hd_level) = match target {
            LptMode::Spp | LptMode::SppNibble => (Level::Low, Level::Low),
            LptMode::Byte | LptMode::Epp | LptMode::Ecp | LptMode::EcpDma => {
                (Level::Low, Level::High)
            }
        };
        // DIR is only CPU-driven when the DIR follower SM is not
        // active. The follower itself is brought up *after* this
        // call by [`super::epp::EppPhy::build`] when target == EPP,
        // so the SIO write here is harmless even for EPP — the
        // follower's FUNCSEL flip overrides it microseconds later.
        Self::drive_dir_sio(dir_level);
        self.hd.set_level(hd_level);
        defmt::trace!(
            "lpt transceiver: {} → DIR={}, HD={}",
            target,
            match dir_level {
                Level::Low => "L",
                Level::High => "H",
            },
            match hd_level {
                Level::Low => "L",
                Level::High => "H",
            }
        );
    }

    /// Flip the data-bus direction *within* the current mode. Used by
    /// Byte and ECP phases that switch between forward and reverse
    /// without leaving the negotiated mode. `peripheral_drives = true`
    /// = `DIR = H` (peripheral writes), `false` = `DIR = L` (host
    /// writes). HD stays where the mode put it.
    ///
    /// **Not safe to call while the EPP DIR follower SM is active** —
    /// the follower owns DIR's FUNCSEL and will override the SIO
    /// write. Use only in Byte / ECP / SppNibble.
    #[allow(dead_code)] // Wired up when Byte/ECP phase changes land
    pub fn set_data_direction(&mut self, peripheral_drives: bool) {
        Self::drive_dir_sio(if peripheral_drives {
            Level::High
        } else {
            Level::Low
        });
    }
}
