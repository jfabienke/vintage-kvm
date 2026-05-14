//! DMA-sniffer-backed [`Crc32Engine`].
//!
//! Configures the RP2350 DMA Sniffer in CRC-32R mode (bit-reversed input
//! + reflected polynomial 0xEDB88320) with `out_rev` and `out_inv` set
//! so the result matches the reflected/inverted CRC-32/IEEE used by zlib,
//! PNG, and the Stage 1 host-side `crc32_*` routines.
//!
//! Mechanism: DMA_CH5 reads from the input slice and writes to a discard
//! sink at memory-bus speed (`treq_sel = PERMANENT`). Each byte flows
//! through the sniffer hardware, which updates the SNIFF_DATA register.
//! `out_rev` and `out_inv` only transform on read; the internal
//! accumulator stays raw, so back-to-back `update()` calls chain
//! naturally.
//!
//! Verified against the standard test vector
//! `compute(b"123456789") == 0xCBF43926` at boot.

use core::sync::atomic::{compiler_fence, Ordering};

use embassy_rp::Peri;
use embassy_rp::pac;
use embassy_rp::pac::dma::vals::{Calc, DataSize, TransCountMode, TreqSel};
use embassy_rp::peripherals::DMA_CH5;
use vintage_kvm_protocol::packet::crc32;
use vintage_kvm_protocol::Crc32Engine;

const DMA_CH_NUM: usize = 5;

/// Discard sink for the memcopy. DMA writes here; CPU never reads it.
#[repr(C, align(4))]
struct Sink(u32);
static mut DISCARD: Sink = Sink(0);

pub struct SnifferCrc32 {
    // Consumed as an ownership marker — actual register access goes
    // through pac.
    _dma: Peri<'static, DMA_CH5>,
}

impl SnifferCrc32 {
    /// Initialize the sniffer in CRC-32R mode with output reflection +
    /// inversion enabled. Seeds SNIFF_DATA with the standard CRC-32
    /// initial value (0xFFFFFFFF) so a first `update()` call after
    /// construction starts from a clean accumulator.
    pub fn new(dma_ch: Peri<'static, DMA_CH5>) -> Self {
        pac::DMA.sniff_ctrl().write(|w| {
            w.set_en(false);
            w.set_dmach(DMA_CH_NUM as u8);
            w.set_calc(Calc::CRC32R);
            w.set_bswap(false);
            w.set_out_rev(true);
            w.set_out_inv(true);
        });
        pac::DMA.sniff_data().write_value(crc32::INIT);
        Self { _dma: dma_ch }
    }

    /// One-shot computation. Equivalent to `reset() + update(data) + finalize()`.
    pub fn compute(&mut self, data: &[u8]) -> u32 {
        self.reset();
        self.update(data);
        self.finalize()
    }
}

impl Crc32Engine for SnifferCrc32 {
    fn reset(&mut self) {
        pac::DMA.sniff_data().write_value(crc32::INIT);
    }

    fn update(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let ch = pac::DMA.ch(DMA_CH_NUM);
        let read_addr = data.as_ptr() as u32;
        // DMA always writes the same dummy address; CPU never reads
        // DISCARD's value.
        let write_addr = core::ptr::addr_of_mut!(DISCARD) as u32;

        ch.read_addr().write_value(read_addr);
        ch.write_addr().write_value(write_addr);
        ch.trans_count().write(|w| {
            w.set_mode(TransCountMode::NORMAL);
            w.set_count(data.len() as u32);
        });

        pac::DMA.sniff_ctrl().modify(|w| w.set_en(true));

        compiler_fence(Ordering::SeqCst);

        ch.ctrl_trig().write(|w| {
            w.set_treq_sel(TreqSel::PERMANENT);
            w.set_data_size(DataSize::SIZE_BYTE);
            w.set_incr_read(true);
            w.set_incr_write(false);
            w.set_chain_to(DMA_CH_NUM as u8);
            w.set_bswap(false);
            w.set_en(true);
        });

        // Spin until the transfer completes. At ~150 MHz bus speed this
        // is ~1 µs per ~150 bytes — well below any sleep cadence, so a
        // busy loop is the right choice.
        while ch.ctrl_trig().read().busy() {}

        pac::DMA.sniff_ctrl().modify(|w| w.set_en(false));

        compiler_fence(Ordering::SeqCst);
    }

    fn finalize(&self) -> u32 {
        // SNIFF_DATA returns the raw accumulator with out_rev + out_inv
        // applied as the read transform — that's the final CRC-32.
        pac::DMA.sniff_data().read()
    }
}
