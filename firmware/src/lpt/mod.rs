//! LPT physical-layer abstraction.
//!
//! Phase 3+ only implements `compat::SppNibblePhy` (SPP-nibble bit-bang).
//! Phase 4+ adds PIO-native impls (compat / byte / EPP / ECP-DMA), all of
//! which implement the same `LptPhy` trait so the rest of the firmware
//! treats them uniformly. See `docs/firmware_crate_and_trait_design.md` §3.1.

pub mod compat;
pub mod pio_compat_in;
pub mod pio_nibble_out;
pub mod ring_dma;

// Single PIO0 IRQ binding shared by every PIO0 SM (compat-in on SM0,
// nibble-out on SM1). embassy's bind_interrupts! disallows re-binding the
// same IRQ from multiple sites, so it has to live in one place.
embassy_rp::bind_interrupts!(pub(crate) struct Pio0Irqs {
    PIO0_IRQ_0 => embassy_rp::pio::InterruptHandler<embassy_rp::peripherals::PIO0>;
});

#[allow(dead_code)] // `Timeout` is constructed once timeouts land in Phase 4.
#[derive(Debug, Clone, Copy, defmt::Format)]
pub enum LptError {
    /// Timed out waiting for the host strobe (forward) or for the wire to
    /// idle (reverse).
    Timeout,
    /// A phy method was called for a mode this impl doesn't support.
    ModeMismatch,
    /// Unrecoverable hardware fault (PIO panic, DMA error).
    Hardware,
}

#[allow(dead_code)] // Multi-mode dispatch lands in Phase 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum LptMode {
    /// SPP forward-only.
    Spp,
    /// SPP forward + nibble reverse. Phase 3 default.
    SppNibble,
    /// Byte (IEEE 1284 bidirectional). Phase 4+.
    Byte,
    /// EPP forward + reverse. Phase 5+.
    Epp,
    /// ECP forward + reverse. Phase 5+.
    Ecp,
    /// ECP with DMA-backed forward + reverse. Phase 5+.
    EcpDma,
}

/// LPT phy contract. Every concrete LPT phy (bit-bang, PIO compat, PIO byte,
/// PIO EPP, ECP-DMA) implements this trait so the protocol layer doesn't
/// have to care which is in use. See `docs/firmware_crate_and_trait_design.md`
/// §3.1 for the impl matrix.
///
/// Phase 3 uses the concrete `compat::SppNibblePhy` directly; the trait
/// lights up at Phase 4 when `LptPhyMux` and the second concrete impl land.
#[allow(dead_code)]
#[allow(async_fn_in_trait)] // single-impl-per-mode; no Send bound needed yet
pub trait LptPhy {
    /// Wait for one inbound byte on the LPT bus. Cancellable.
    async fn recv_byte(&mut self) -> Result<u8, LptError>;

    /// Send one outbound byte. Cancellable.
    async fn send_byte(&mut self, b: u8) -> Result<(), LptError>;

    /// Send a contiguous byte slice. Default loops over `send_byte`;
    /// PIO-DMA phys override to batch the whole slice into one DMA
    /// transfer so the CPU isn't paced byte-by-byte by wire timing.
    async fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), LptError> {
        for &b in bytes {
            self.send_byte(b).await?;
        }
        Ok(())
    }

    /// Currently active IEEE 1284 mode.
    fn current_mode(&self) -> LptMode;
}
