//! vintage-kvm keyboard / chipset signature database.
//!
//! Phase 1+ scaffolding. The full match algorithm and seed entries are
//! specified in [`docs/instrumentation_surface.md` §6](https://github.com/jfabienke/vintage-kvm/blob/master/docs/instrumentation_surface.md).
//!
//! This is currently a stub — types and algorithm land when the PS/2
//! classifier work begins (Phase 1).

#![no_std]

/// Host machine class — output of the PS/2 classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MachineClass {
    Xt,
    At,
    Ps2,
}

/// Keyboard signature feature vector. Each field has a canonical range
/// described in `instrumentation_surface.md §6.2`.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KeyboardFeatures {
    pub bit_period_p50_us: u16,
    pub bit_period_p99_us: u16,
    pub duty_pct: u8,
    pub skew_us: i8,
    pub inhibit_avg_us: u16,
}
