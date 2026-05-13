//! vintage-kvm CDC telemetry schema.
//!
//! Newline-delimited JSON events emitted by the firmware on CDC interface 1
//! and consumed by host-side tooling (the `tools/tui/` dashboard, log
//! aggregators, etc.). Full event taxonomy and command set:
//! [`docs/instrumentation_surface.md` §5](https://github.com/jfabienke/vintage-kvm/blob/master/docs/instrumentation_surface.md).
//!
//! This is scaffold; the event enum is populated incrementally as each
//! phase's emitter lands (Phase 1: PS/2 events; Phase 3: LPT events; Phase 6:
//! all events finalized for TUI consumption).

#![no_std]

/// Stream-schema version. Bumped on **breaking** schema changes. New event
/// types and new fields can be added without bumping.
pub const SCHEMA_VERSION: u8 = 1;

/// Physical port a plane is bound to. Matches the `bound_to` JSON field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Port {
    Kbd,
    Aux,
    Lpt,
}

/// Logical channel for the two-plane transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Plane {
    Control,
    Data,
}

/// Plane binding state for the `plane` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum PlaneState {
    Active,
    Idle,
    IdlePlanned,
    Degraded,
    Fallback,
    NotApplicable,
}
