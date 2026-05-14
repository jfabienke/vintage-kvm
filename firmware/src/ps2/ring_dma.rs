//! DMA-driven ring buffers for the PS/2 oversamplers.
//!
//! Replaces per-word `wait_pull` drains with a continuous DMA transfer
//! from a PIO RX FIFO into a 4 KB SRAM ring. The CPU then polls
//! `write_addr` at a ~2 ms cadence and batches new words through the
//! framer. ~7× CPU reduction vs per-word futures.
//!
//! Mechanism (per channel):
//!   * One DMA channel, hardcoded per PIO SM.
//!   * `data_size = SIZE_WORD`, `incr_read = 0`, `incr_write = 1`.
//!   * `ring_size = 12, ring_sel = true` → write_addr wraps at 4 KB.
//!   * `trans_count.mode = ENDLESS` → DMA runs forever.
//!   * `treq_sel = PIO1 SMx RX` (DMA only advances when the FIFO has data).
//!
//! Each ring storage is `#[repr(align(4096))]` so the wrap is naturally
//! aligned, as required by the RP2350 DMA. Slots are `AtomicU32` so CPU
//! loads are well-defined alongside DMA writes (no cache on this chip,
//! so the atomic is purely for Rust's abstract machine).

use core::sync::atomic::AtomicU32;

use embassy_rp::Peri;
use embassy_rp::pac;
use embassy_rp::pac::dma::vals::{DataSize, TransCountMode, TreqSel};
use embassy_rp::peripherals::{DMA_CH1, DMA_CH2};

pub const RING_WORDS: usize = 1024;
const RING_BYTES: usize = RING_WORDS * 4;
const RING_SIZE_LOG2: u8 = 12; // 2^12 = RING_BYTES

const DMA_CH_KBD: usize = 1;
const DMA_CH_AUX: usize = 2;

/// embassy-rp computes DREQ as `PIO_NO * 8 + SM + 4`.
const PIO1_SM0_RX_DREQ: u8 = 12; // KBD oversampler
const PIO1_SM2_RX_DREQ: u8 = 14; // AUX oversampler

#[repr(C, align(4096))]
struct RingStorage {
    words: [AtomicU32; RING_WORDS],
}

static RING_KBD: RingStorage = RingStorage {
    words: [const { AtomicU32::new(0) }; RING_WORDS],
};
static RING_AUX: RingStorage = RingStorage {
    words: [const { AtomicU32::new(0) }; RING_WORDS],
};

/// Configure DMA_CH1 to stream PIO1 SM0's RX FIFO into the KBD ring.
pub fn arm_kbd(_dma_ch: Peri<'static, DMA_CH1>) -> RingHandle {
    arm(DMA_CH_KBD, PIO1_SM0_RX_DREQ, &RING_KBD, "kbd")
}

/// Configure DMA_CH2 to stream PIO1 SM2's RX FIFO into the AUX ring.
pub fn arm_aux(_dma_ch: Peri<'static, DMA_CH2>) -> RingHandle {
    arm(DMA_CH_AUX, PIO1_SM2_RX_DREQ, &RING_AUX, "aux")
}

fn arm(
    ch_num: usize,
    dreq: u8,
    storage: &'static RingStorage,
    label: &'static str,
) -> RingHandle {
    let ch = pac::DMA.ch(ch_num);

    let read_addr = match dreq {
        PIO1_SM0_RX_DREQ => pac::PIO1.rxf(0).as_ptr() as u32,
        PIO1_SM2_RX_DREQ => pac::PIO1.rxf(2).as_ptr() as u32,
        _ => unreachable!("unknown DREQ"),
    };
    let write_addr = storage.words.as_ptr() as u32;

    debug_assert!(
        write_addr & (RING_BYTES as u32 - 1) == 0,
        "ring buffer must be aligned to its size"
    );

    ch.read_addr().write_value(read_addr);
    ch.write_addr().write_value(write_addr);
    ch.trans_count().write(|w| {
        w.set_mode(TransCountMode::ENDLESS);
        w.set_count(0);
    });
    ch.ctrl_trig().write(|w| {
        w.set_treq_sel(TreqSel::from(dreq));
        w.set_data_size(DataSize::SIZE_WORD);
        w.set_incr_read(false);
        w.set_incr_write(true);
        w.set_ring_size(RING_SIZE_LOG2);
        w.set_ring_sel(true);
        w.set_chain_to(ch_num as u8);
        w.set_bswap(false);
        w.set_en(true);
    });

    defmt::info!(
        "ps2 {} ring DMA armed: CH{} → {} B @ {:#010X}, treq={}",
        label,
        ch_num,
        RING_BYTES,
        write_addr,
        dreq
    );

    RingHandle {
        base_addr: write_addr,
        last_tail: 0,
        ch_num,
        words: &storage.words,
    }
}

pub struct RingHandle {
    base_addr: u32,
    last_tail: usize,
    ch_num: usize,
    words: &'static [AtomicU32; RING_WORDS],
}

impl RingHandle {
    pub fn head(&self) -> usize {
        let waddr = pac::DMA.ch(self.ch_num).write_addr().read();
        ((waddr - self.base_addr) as usize / 4) % RING_WORDS
    }

    pub fn pending(&self) -> usize {
        (self.head() + RING_WORDS - self.last_tail) % RING_WORDS
    }

    /// Drain up to `max` new words, calling `f` for each. Capped at
    /// `RING_WORDS - 1` because head == tail can't distinguish empty
    /// from full — the resync path handles real overruns separately.
    pub fn drain<F: FnMut(u32)>(&mut self, max: usize, mut f: F) -> usize {
        let head = self.head();
        let pending = (head + RING_WORDS - self.last_tail) % RING_WORDS;
        let n = pending.min(max);
        for _ in 0..n {
            let word = self.words[self.last_tail]
                .load(core::sync::atomic::Ordering::Relaxed);
            f(word);
            self.last_tail = (self.last_tail + 1) % RING_WORDS;
        }
        n
    }

    pub fn resync(&mut self) {
        let head = self.head();
        self.last_tail = (head + RING_WORDS - 1) % RING_WORDS;
    }
}
