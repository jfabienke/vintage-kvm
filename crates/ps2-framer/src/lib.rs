//! PS/2 wire decode pipeline: framer + classifier.
//!
//! The framer ([`Framer`]) consumes a 1 µs (CLK, DATA, timestamp) stream
//! and emits one [`Ps2Frame`] per wire frame. Handles both protocol
//! variants — XT (9-bit, start=1, no parity/stop) and AT/PS-2 (11-bit,
//! start=0 + 8 data + odd parity + stop=1) — distinguished by the start
//! bit's polarity.
//!
//! The classifier ([`classifier::Classifier`]) consumes the resulting
//! [`Ps2Frame`] stream (plus an optional AUX-channel-activity signal) and
//! converges on a [`vintage_kvm_signatures::MachineClass`].
//!
//! `no_std` + no alloc; tested on host and consumed by the firmware
//! oversampler task. Reference design lives in
//! [`docs/pio_state_machines_design.md`](https://github.com/jfabienke/vintage-kvm/blob/master/docs/pio_state_machines_design.md)
//! §§7–8 and [`docs/ps2_eras_reference.md`](https://github.com/jfabienke/vintage-kvm/blob/master/docs/ps2_eras_reference.md).

#![no_std]

mod framer;
pub mod classifier;

pub use framer::{Framer, FrameKind, FrameTiming, Ps2Frame, GLITCH_THRESHOLD_US, IDLE_TIMEOUT_US};
pub use classifier::{Classifier, Event as ClassifierEvent, State as ClassifierState};
