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
//! Concrete impls land in `ps2/{oversampler.rs, demodulator.rs, tx.rs,
//! framer.rs, classifier.rs, instrumentation.rs}` when Phase 1 lands.

#![allow(dead_code)] // Phase 1 scaffold; concrete impls are placeholders.

pub mod framer;
pub mod oversampler;

pub use vintage_kvm_signatures::MachineClass;
#[allow(unused_imports)] // exposed for Phase 1 classifier
pub use vintage_kvm_signatures::KeyboardFeatures;

/// One PS/2 frame extracted from the wire. Carries timing metadata from the
/// oversampler (when populated; demodulator-only path leaves `timing` zeroed).
#[derive(Debug, Clone, Copy, defmt::Format)]
pub struct Ps2Frame {
    pub data: u8,
    pub parity_ok: bool,
    pub framing_ok: bool,
    pub start_timestamp_us: u64,
    pub timing: FrameTiming,
}

#[derive(Debug, Clone, Copy, Default, defmt::Format)]
pub struct FrameTiming {
    /// Measured period between CLK falling edges for each bit slot.
    pub bit_periods_us: [u16; 11],
    /// Signed CLK→DATA edge skew at the start bit (positive = DATA settles
    /// after CLK).
    pub clk_data_skew_us: i8,
    /// CLK transitions shorter than the 4 µs glitch threshold.
    pub glitch_count: u8,
}

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
