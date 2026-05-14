//! Consolidated shared interrupt bindings.
//!
//! All DMA channels share the single `DMA_IRQ_0` vector (embassy-rp's
//! `Channel::new` hardcodes `inte0`), so every channel that needs the
//! async completion future must register its `dma::InterruptHandler` in
//! ONE place — multiple `bind_interrupts!` invocations would generate
//! conflicting ISR symbols.
//!
//! Channels that don't need IRQ-driven completion (the ring DMAs in
//! `ps2::ring_dma` and `lpt::ring_dma`) bypass `Channel::new` entirely
//! and program registers directly via pac, so they don't appear here.

use embassy_rp::dma::InterruptHandler as DmaIH;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH4};

embassy_rp::bind_interrupts!(pub(crate) struct DmaIrqs {
    DMA_IRQ_0 => DmaIH<DMA_CH0>, DmaIH<DMA_CH4>;
});
