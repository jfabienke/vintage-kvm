//! DMA-driven ring buffer for the LPT `lpt_compat_in` PIO program.
//!
//! Mirrors the pattern from [`crate::ps2::ring_dma`]: a single DMA
//! channel reads continuously from PIO0 SM0's RX FIFO into a 1 KB
//! aligned ring; the CPU consumes bytes by polling `write_addr`.
//!
//! Sized for LPT: at SPP-nibble rates (~5 kHz bytes) the ring holds
//! ~50 ms of slack; for future EPP/ECP modes (250 kHz bytes) it's still
//! ~1 ms — more than enough for a 200 µs CPU poll cadence.

use core::sync::atomic::AtomicU32;

use embassy_rp::pac;
use embassy_rp::pac::dma::vals::{DataSize, TransCountMode, TreqSel};

pub const RING_WORDS: usize = 256;
const RING_BYTES: usize = RING_WORDS * 4; // 1024 = 2^10
const RING_SIZE_LOG2: u8 = 10;

const DMA_CH_LPT_IN: usize = 3;
/// embassy-rp computes DREQ as `PIO_NO * 8 + SM + 4`. PIO0 SM0 RX = 4.
const PIO0_SM0_RX_DREQ: u8 = 4;

#[repr(C, align(1024))]
struct RingStorage {
    words: [AtomicU32; RING_WORDS],
}

static RING: RingStorage = RingStorage {
    words: [const { AtomicU32::new(0) }; RING_WORDS],
};

/// Arm DMA_CH3 to stream PIO0 SM0's RX FIFO into the LPT ring.
///
/// The Peri<DMA_CH3> ownership token lives on `LptHardware` so that
/// mode swaps can arm/disarm without re-consuming a peripheral that
/// embassy can't hand back. The channel ID and treq are fixed.
pub fn arm() -> RingHandle {
    let ch = pac::DMA.ch(DMA_CH_LPT_IN);

    let read_addr = pac::PIO0.rxf(0).as_ptr() as u32;
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
        w.set_treq_sel(TreqSel::from(PIO0_SM0_RX_DREQ));
        w.set_data_size(DataSize::SIZE_WORD);
        w.set_incr_read(false);
        w.set_incr_write(true);
        w.set_ring_size(RING_SIZE_LOG2);
        w.set_ring_sel(true);
        w.set_chain_to(DMA_CH_LPT_IN as u8);
        w.set_bswap(false);
        w.set_en(true);
    });

    defmt::info!(
        "lpt compat-in ring DMA armed: CH{} → {} B @ {:#010X}, treq={}",
        DMA_CH_LPT_IN,
        RING_BYTES,
        write_addr,
        PIO0_SM0_RX_DREQ
    );

    RingHandle {
        base_addr: write_addr,
        last_tail: 0,
    }
}

/// Stop the LPT-in ring DMA. After return, the PIO RX FIFO will fill
/// and the SM will stall — caller is expected to disable the SM next.
pub fn disarm() {
    let ch = pac::DMA.ch(DMA_CH_LPT_IN);
    ch.ctrl_trig().modify(|w| {
        w.set_en(false);
    });
}

pub struct RingHandle {
    base_addr: u32,
    last_tail: usize,
}

impl RingHandle {
    pub fn head(&self) -> usize {
        let waddr = pac::DMA.ch(DMA_CH_LPT_IN).write_addr().read();
        ((waddr - self.base_addr) as usize / 4) % RING_WORDS
    }

    #[allow(dead_code)] // exposed for future overrun-detection consumers
    pub fn pending(&self) -> usize {
        (self.head() + RING_WORDS - self.last_tail) % RING_WORDS
    }

    /// Read the oldest unread word, advancing tail. Returns `None` if
    /// the ring has no new data.
    pub fn try_pop(&mut self) -> Option<u32> {
        if self.last_tail == self.head() {
            return None;
        }
        let word = RING.words[self.last_tail]
            .load(core::sync::atomic::Ordering::Relaxed);
        self.last_tail = (self.last_tail + 1) % RING_WORDS;
        Some(word)
    }

    /// Skip ahead to within one slot behind head, dropping older
    /// samples. Used to recover from overrun.
    #[allow(dead_code)] // exposed for future overrun-detection consumers
    pub fn resync(&mut self) {
        let head = self.head();
        self.last_tail = (head + RING_WORDS - 1) % RING_WORDS;
    }
}
