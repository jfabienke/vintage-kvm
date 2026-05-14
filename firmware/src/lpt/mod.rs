//! LPT physical-layer abstraction.
//!
//! Phase 3+ only implements `compat::SppNibblePhy` (SPP-nibble bit-bang).
//! Phase 4+ adds PIO-native impls (compat / byte / EPP / ECP-DMA), all of
//! which implement the same `LptPhy` trait so the rest of the firmware
//! treats them uniformly. See `docs/firmware_crate_and_trait_design.md` §3.1.

pub mod compat;
pub mod pio_compat_in;

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

    /// Currently active IEEE 1284 mode.
    fn current_mode(&self) -> LptMode;
}
