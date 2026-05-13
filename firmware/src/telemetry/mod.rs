//! Firmware-side concrete implementations of `TelemetryEmit`.
//!
//! Trait + Event enum live in `vintage-kvm-telemetry-schema`. This module
//! provides the wire-side bridges:
//!
//! - `DefmtEmit` — formats events per `instrumentation_surface.md` §3 and
//!   writes via `defmt::info!`. Always available; primary dev surface.
//! - `NoopEmit` — drops events silently. Used in tests and when CDC is
//!   the only desired sink (rare).
//! - `MultiEmit<A, B>` — forwards to both `A` and `B`. Combine a `DefmtEmit`
//!   and a future `CdcEmit` to send everything everywhere.
//!
//! `CdcEmit` (CDC interface 1 JSON-line serializer) lands when the USB CDC
//! task does (Phase 6 for the polished surface; earlier as a stub).

pub mod defmt_emit;
pub mod multi;
pub mod noop;

pub use defmt_emit::DefmtEmit;
#[allow(unused_imports)] // ready for Phase 4+ multi-sink wiring and tests
pub use multi::MultiEmit;
#[allow(unused_imports)]
pub use noop::NoopEmit;

// Re-export for ergonomic call sites:
//   use crate::telemetry::{Event, TelemetryEmit};
pub use vintage_kvm_telemetry_schema::{Event, ResyncReason, TelemetryEmit};
