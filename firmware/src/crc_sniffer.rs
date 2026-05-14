//! RP2350 DMA Sniffer — hardware-accelerated CRC-16 + CRC-32 engines.
//!
//! Shares DMA_CH5 between both algorithms. Each `compute_*` call:
//!   1. Configures `SNIFF_CTRL` for the right `Calc` mode + output
//!      transforms.
//!   2. Seeds `SNIFF_DATA` with the algorithm's INIT value.
//!   3. Kicks DMA_CH5 to memcopy the slice through the sniffer at
//!      `treq_sel = PERMANENT` (memory-bus speed).
//!   4. Reads `SNIFF_DATA`; the read-side `out_rev` / `out_inv`
//!      transforms (if any) apply on-the-fly.
//!
//! Both engines are validated at boot against their canonical test
//! vector — `compute("123456789")` = 0xCBF43926 (CRC-32) / 0x29B1
//! (CRC-16-CCITT/FALSE).
//!
//! ## Mode mapping vs the protocol crate's software impls
//!
//! | Algorithm  | Calc      | Init      | out_rev | out_inv | Match against        |
//! |------------|-----------|-----------|---------|---------|----------------------|
//! | CRC-32/IEEE| CRC32R    | 0xFFFFFFFF| true    | true    | `crc32::compute`     |
//! | CRC-16-CCITT/FALSE | CRC16 | 0xFFFF | false   | false   | `crc16::compute`     |
//!
//! CRC-32 is "reflected" (input bit-reversed, output bit-reversed,
//! final XOR = 0xFFFFFFFF) — the variant used by zlib/PNG/IEEE 802.3
//! and Stage 1's host-side `crc32_*` routines.
//!
//! CRC-16-CCITT is "FALSE" (no reflection, no final XOR) — the variant
//! used by every packet CRC in our wire protocol.

use core::sync::atomic::{compiler_fence, Ordering};

use embassy_rp::Peri;
use embassy_rp::pac;
use embassy_rp::pac::dma::vals::{Calc, DataSize, TransCountMode, TreqSel};
use embassy_rp::peripherals::DMA_CH5;

const DMA_CH_NUM: usize = 5;

const CRC32_INIT: u32 = 0xFFFF_FFFF;
const CRC16_INIT: u32 = 0x0000_FFFF;

/// Discard sink for the memcopy. DMA writes here; CPU never reads it.
#[repr(C, align(4))]
struct Sink(u32);
static mut DISCARD: Sink = Sink(0);

/// Owns DMA_CH5 and the shared sniffer-control register. Both CRC
/// engines route through it.
pub struct DmaSniffer {
    _dma: Peri<'static, DMA_CH5>,
}

impl DmaSniffer {
    /// Claim DMA_CH5 for the sniffer. The actual sniffer setup happens
    /// per `compute_*` call so multiple algorithms can share one
    /// channel without leaking state between calls.
    pub fn new(dma_ch: Peri<'static, DMA_CH5>) -> Self {
        Self { _dma: dma_ch }
    }

    /// One-shot CRC-32/IEEE reflected over `data`. Matches
    /// [`vintage_kvm_protocol::packet::crc32::compute`].
    pub fn compute_crc32(&mut self, data: &[u8]) -> u32 {
        configure(Calc::CRC32R, true, true);
        pac::DMA.sniff_data().write_value(CRC32_INIT);
        if !data.is_empty() {
            run(data);
        }
        pac::DMA.sniff_data().read()
    }

    /// One-shot CRC-16-CCITT/FALSE over `data`. Matches
    /// [`vintage_kvm_protocol::packet::crc16::compute`].
    pub fn compute_crc16(&mut self, data: &[u8]) -> u16 {
        configure(Calc::CRC16, false, false);
        pac::DMA.sniff_data().write_value(CRC16_INIT);
        if !data.is_empty() {
            run(data);
        }
        (pac::DMA.sniff_data().read() & 0xFFFF) as u16
    }
}

fn configure(calc: Calc, out_rev: bool, out_inv: bool) {
    pac::DMA.sniff_ctrl().write(|w| {
        w.set_en(false);
        w.set_dmach(DMA_CH_NUM as u8);
        w.set_calc(calc);
        w.set_bswap(false);
        w.set_out_rev(out_rev);
        w.set_out_inv(out_inv);
    });
}

fn run(data: &[u8]) {
    let ch = pac::DMA.ch(DMA_CH_NUM);
    let read_addr = data.as_ptr() as u32;
    // DMA always writes the same dummy address; CPU never reads it.
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

    // Spin until completion. At ~150 MHz bus speed this is ~1 µs per
    // ~150 bytes — well below any sleep cadence, so a busy loop is the
    // right choice.
    while ch.ctrl_trig().read().busy() {}

    pac::DMA.sniff_ctrl().modify(|w| w.set_en(false));

    compiler_fence(Ordering::SeqCst);
}
