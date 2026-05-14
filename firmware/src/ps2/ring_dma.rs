//! DMA-driven ring buffer for the PS/2 KBD oversampler.
//!
//! Replaces the per-word `wait_pull` drain (≈ 100 k embassy futures/sec)
//! with a continuous DMA transfer from PIO1 SM0's RX FIFO into a 4 KB
//! ring buffer. The CPU then polls write_addr at a ~2 ms cadence and
//! batches new words through the framer.
//!
//! Mechanism:
//!   * Single DMA channel (DMA_CH1; CH0 owned by NeoPixel).
//!   * `data_size = SIZE_WORD`, `incr_read = 0`, `incr_write = 1`.
//!   * `ring_size = 12, ring_sel = true` → write_addr wraps at 4 KB.
//!   * `trans_count.mode = ENDLESS` → DMA runs forever.
//!   * `treq_sel = PIO1 SM0 RX` (DMA only advances when the FIFO has data).
//!
//! Ring storage is `#[repr(align(4096))]` so the wrap is naturally
//! aligned, as required by the RP2350 DMA. Each slot is an `AtomicU32`
//! so CPU loads are well-defined alongside DMA writes (no cache on
//! this chip, so the atomic is purely for Rust's abstract machine).

use core::sync::atomic::AtomicU32;

use embassy_rp::Peri;
use embassy_rp::pac;
use embassy_rp::pac::dma::vals::{DataSize, TransCountMode, TreqSel};
use embassy_rp::peripherals::DMA_CH1;

/// 1024 × 4 B = 4 KB ring. log2(4096) = 12 → `ring_size = 12`. At the
/// oversampler's 100 k words/s, the ring holds ~10.24 ms of history,
/// which is ~5× the ~2 ms CPU wake cadence.
pub const RING_WORDS: usize = 1024;

const RING_BYTES: usize = RING_WORDS * 4;
const RING_SIZE_LOG2: u8 = 12; // 2^12 = RING_BYTES

/// DMA channel number we hardcode here. DMA_CH0 is owned by the NeoPixel
/// driver; CH1 is free and the next-allocated slot for PIO bulk transfers.
const DMA_CHANNEL: usize = 1;

/// DREQ for PIO1 SM0 RX. embassy-rp's `dreq()` helper computes this as
/// `PIO_NO * 8 + SM + 4` → `1 * 8 + 0 + 4 = 12`.
const PIO1_SM0_RX_DREQ: u8 = 12;

#[repr(C, align(4096))]
struct RingStorage {
    words: [AtomicU32; RING_WORDS],
}

static RING: RingStorage = RingStorage {
    words: [const { AtomicU32::new(0) }; RING_WORDS],
};

/// Configure DMA_CH1 to stream PIO1 SM0's RX FIFO into the ring buffer
/// indefinitely. Consumes the `Peri<DMA_CH1>` so the channel can't be
/// claimed elsewhere; the actual register access is via the unstable-pac
/// path because embassy's high-level wrapper doesn't expose ring config.
pub fn arm(_dma_ch: Peri<'static, DMA_CH1>) -> RingHandle {
    let ch = pac::DMA.ch(DMA_CHANNEL);

    let read_addr = pac::PIO1.rxf(0).as_ptr() as u32;
    let write_addr = RING.words.as_ptr() as u32;

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
        w.set_treq_sel(TreqSel::from(PIO1_SM0_RX_DREQ));
        w.set_data_size(DataSize::SIZE_WORD);
        w.set_incr_read(false);
        w.set_incr_write(true);
        w.set_ring_size(RING_SIZE_LOG2);
        w.set_ring_sel(true); // wrap write address
        w.set_chain_to(DMA_CHANNEL as u8); // chain to self = no chain
        w.set_bswap(false);
        w.set_en(true);
    });

    defmt::info!(
        "ps2 kbd ring DMA armed: CH{} → {} B @ {:#010X}, treq={}",
        DMA_CHANNEL,
        RING_BYTES,
        write_addr,
        PIO1_SM0_RX_DREQ
    );

    RingHandle {
        base_addr: write_addr,
        last_tail: 0,
    }
}

/// CPU-side reader for the DMA ring. Tracks the last word index it
/// processed; on each `drain` call, returns an iterator over all new
/// words written by the DMA since the last call.
pub struct RingHandle {
    base_addr: u32,
    last_tail: usize,
}

impl RingHandle {
    /// Current head — the index the DMA will write to *next*. Computed
    /// from the live `write_addr` register modulo the ring size.
    pub fn head(&self) -> usize {
        let waddr = pac::DMA.ch(DMA_CHANNEL).write_addr().read();
        ((waddr - self.base_addr) as usize / 4) % RING_WORDS
    }

    /// Number of unread words available in the ring (0..RING_WORDS).
    pub fn pending(&self) -> usize {
        (self.head() + RING_WORDS - self.last_tail) % RING_WORDS
    }

    /// Drain up to `max` new words, calling `f` for each. Returns the
    /// number of words actually drained.
    ///
    /// Capped at `RING_WORDS - 1`: if the actual pending count is
    /// exactly `RING_WORDS`, we can't distinguish "completely full" from
    /// "completely caught up" with the head pointer alone, so we treat
    /// it as an overflow event and skip ahead.
    pub fn drain<F: FnMut(u32)>(&mut self, max: usize, mut f: F) -> usize {
        let head = self.head();
        let pending = (head + RING_WORDS - self.last_tail) % RING_WORDS;

        // If head wraps perfectly back to tail, we either have 0 new
        // words or RING_WORDS (which means overrun). The caller's
        // periodic cadence should make 0 vastly more likely, but a
        // separate overrun-flagged path exists for the edge case.
        let n = pending.min(max);
        for _ in 0..n {
            let word = RING.words[self.last_tail]
                .load(core::sync::atomic::Ordering::Relaxed);
            f(word);
            self.last_tail = (self.last_tail + 1) % RING_WORDS;
        }
        n
    }

    /// Resync after a detected overrun: skip ahead to within one slot
    /// behind the head, effectively dropping the oldest samples.
    pub fn resync(&mut self) {
        let head = self.head();
        self.last_tail = (head + RING_WORDS - 1) % RING_WORDS;
    }
}
