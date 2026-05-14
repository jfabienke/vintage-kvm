//! PS/2 keyboard + AUX phy traits.
//!
//! Phase 1+ scaffold. The two-pipeline architecture per
//! [`docs/pio_state_machines_design.md`](../../../docs/pio_state_machines_design.md)
//! splits the receive path into:
//!
//! - **Demodulator** (`Ps2Receiver`) — production byte stream from PIO
//!   edge-triggered SM. Lean and cheap; emits one `Ps2Frame` per push.
//! - **Oversampler** (`Ps2Sampler`) — 1 MS/s raw stream of (CLK, DATA)
//!   pairs into a DMA ring. Used for instrumentation, classifier, and
//!   keyboard/chipset fingerprinting. Always-on alongside the demodulator.
//!
//! TX uses a separate PIO SM that bit-bangs CLK + drives DATA via the
//! 74LVC07A open-drain buffer; one `Ps2Frame` per emit.
//!
//! The wire-frame state machine lives in `crates/ps2-framer` so it can be
//! host-tested. Re-exported here for the rest of the firmware to consume
//! via `crate::ps2::{Framer, Ps2Frame, FrameTiming}`.

#![allow(dead_code)] // Phase 1 scaffold; concrete impls are placeholders.

pub mod aux_oversampler;
pub mod injector;
pub mod oversampler;
pub mod ring_dma;
pub mod supervisor;
pub mod tx;

// Single PIO1 IRQ binding shared by both PIO1 state machines (oversampler
// on SM0, KBD TX on SM1). embassy's bind_interrupts! disallows re-binding
// from multiple sites.
embassy_rp::bind_interrupts!(pub(crate) struct Pio1Irqs {
    PIO1_IRQ_0 => embassy_rp::pio::InterruptHandler<embassy_rp::peripherals::PIO1>;
});

#[allow(unused_imports)] // FrameTiming re-exported for downstream consumers
pub use vintage_kvm_ps2_framer::{Framer, FrameTiming, Ps2Frame};
pub use vintage_kvm_signatures::MachineClass;
#[allow(unused_imports)] // exposed for Phase 1 classifier
pub use vintage_kvm_signatures::KeyboardFeatures;

#[derive(Debug, Clone, Copy, defmt::Format)]
pub enum Ps2Error {
    Parity,
    Framing,
    BusContention,
    Hardware,
}

/// Production byte stream — clean frames from the PIO demodulator.
#[allow(async_fn_in_trait)]
pub trait Ps2Receiver {
    async fn recv_frame(&mut self) -> Ps2Frame;
    fn machine_class(&self) -> Option<MachineClass>;
}

/// Instrumentation stream — raw oversampled timing data.
///
/// The implementation runs continuously into a DMA ring; consumers borrow
/// snapshots of the ring without taking ownership.
pub trait Ps2Sampler {
    /// Take a 1-second rolling snapshot. Subsequent calls return updated
    /// stats from the same ring.
    fn stats(&self) -> Ps2Stats;
}

#[derive(Debug, Clone, Copy, Default, defmt::Format)]
pub struct Ps2Stats {
    pub frames_total: u64,
    pub frames_errored: u64,
    pub glitches_total: u64,
    pub bit_period_p50_us: u16,
    pub bit_period_p99_us: u16,
    pub duty_pct: u8,
    pub skew_us: i8,
}

/// Outbound frames (keyboard emulation / private-channel TX).
#[allow(async_fn_in_trait)]
pub trait Ps2Transmitter {
    async fn send_frame(&mut self, frame: Ps2Frame) -> Result<(), Ps2Error>;
}
