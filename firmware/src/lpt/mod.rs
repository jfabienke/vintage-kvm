//! LPT physical-layer abstraction.
//!
//! Phase 3+ only implements `compat::SppNibblePhy` (SPP-nibble bit-bang).
//! Phase 4+ will add EPP / ECP PIO impls; Phase 5+ adds DMA.

pub mod compat;

#[allow(dead_code)] // `Timeout` is constructed once timeouts land in Phase 4.
#[derive(Debug, Clone, Copy, defmt::Format)]
pub enum LptError {
    /// Timed out waiting for the host strobe (forward) or for the wire to
    /// idle (reverse).
    Timeout,
}

#[allow(dead_code)] // Multi-mode dispatch lands in Phase 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum LptMode {
    /// SPP-compat forward + nibble reverse. Only mode used in Phase 3.
    SppNibble,
    /// Byte (IEEE 1284 bidirectional). Phase 4+.
    Byte,
    /// EPP forward + reverse. Phase 5+.
    Epp,
    /// ECP forward + reverse, optionally DMA-backed. Phase 5+.
    Ecp,
}
