//! `MultiEmit<A, B>` — forwards each event to two underlying emitters.
//!
//! Use case: `MultiEmit(DefmtEmit, CdcEmit { ... })` once the CDC telemetry
//! channel is live, so every event lands on both the dev probe and the
//! modern host's CDC stream.

use vintage_kvm_telemetry_schema::{Event, TelemetryEmit};

#[allow(dead_code)] // wires up when CDC telemetry sink lands
pub struct MultiEmit<A: TelemetryEmit, B: TelemetryEmit>(pub A, pub B);

impl<A: TelemetryEmit, B: TelemetryEmit> TelemetryEmit for MultiEmit<A, B> {
    fn emit(&self, event: Event) {
        self.0.emit(event.clone());
        self.1.emit(event);
    }
}
