//! Small helpers shared across phys / transports.
//!
//! Phase 3 has nothing here yet. Phase 4+ will add `pin.rs` (GPIO direction
//! / pull-up helpers used by the PIO LPT impls) and `time.rs` (low-jitter
//! delay primitives for nibble settling and EPP handshake budgets).

#![allow(dead_code)]
