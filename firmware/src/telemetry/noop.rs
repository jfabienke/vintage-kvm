//! `NoopEmit` — drops events silently. Used in tests and when no surface is
//! wanted at all.

use vintage_kvm_telemetry_schema::{Event, TelemetryEmit};

#[allow(dead_code)] // used in host-side tests once dispatcher migrates to protocol crate
pub struct NoopEmit;

impl TelemetryEmit for NoopEmit {
    fn emit(&self, _event: Event) {}
}
