//! PS/2 keyboard wire oversampler.
//!
//! Implements `ps2_kbd_oversample` from `docs/pio_state_machines_design.md`
//! §6: a 2-instruction PIO program that samples GP2 (CLK_IN), GP3
//! (CLK_PULL), GP4 (DATA_IN) at 1 MS/s and autopushes 10 samples per word
//! (30 bits) into the RX FIFO.
//!
//! Phase 1 scope (this file): get the PIO program loaded, samples landing
//! in firmware-side memory, basic stats updated. The frame extractor /
//! classifier from §7–8 of the design doc lands in follow-up commits, as
//! does DMA-ring offload (the embassy task currently CPU-pulls each word).
//!
//! PIO clock = 1 MHz so each sample = 1 µs of wire time. RP2350 sys_clk =
//! 150 MHz, so clkdiv = 150.0.

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::{PIN_2, PIN_3, PIN_4, PIO1};
use embassy_rp::pio::{
    Common, Config, FifoJoin, InterruptHandler, LoadedProgram, Pio, ShiftConfig, ShiftDirection,
};
use fixed::types::U24F8;

use super::Framer;

bind_interrupts!(struct Irqs {
    PIO1_IRQ_0 => InterruptHandler<PIO1>;
});

/// Number of 3-bit samples packed into one RX-FIFO word. 10 × 3 = 30 bits,
/// matching the autopush threshold; the top 2 bits of each word are zero.
pub const SAMPLES_PER_WORD: usize = 10;

/// PIO clock target for the oversampler. 1 MS/s gives 1 µs/sample.
const PIO_CLK_HZ: u32 = 1_000_000;

/// Rolling counters published by the oversampler task. Updated with
/// `Relaxed` ordering — consumers tolerate a few-µs lag on each field.
pub struct OversamplerCounters {
    /// Total RX-FIFO words drained since boot. One word = 10 samples = 10 µs.
    pub words: AtomicU32,
    /// Words observed where the CLK bit toggled at least once — a rough
    /// "wire activity" indicator distinct from the per-frame counters.
    pub clk_active_words: AtomicU32,
    /// PIO RX-FIFO overrun events — incremented when the SM stalls trying
    /// to push because the CPU drain task fell behind. Should stay at 0 in
    /// steady state.
    pub fifo_overrun: AtomicU32,
    /// Total frames emitted by the framer (any parity/framing result).
    pub frames_total: AtomicU32,
    /// Frames that failed odd-parity or start/stop framing checks.
    pub frames_errored: AtomicU32,
    /// Sum of `FrameTiming::glitch_count` across all emitted frames.
    pub glitches_total: AtomicU32,
}

impl OversamplerCounters {
    const fn new() -> Self {
        Self {
            words: AtomicU32::new(0),
            clk_active_words: AtomicU32::new(0),
            fifo_overrun: AtomicU32::new(0),
            frames_total: AtomicU32::new(0),
            frames_errored: AtomicU32::new(0),
            glitches_total: AtomicU32::new(0),
        }
    }
}

pub static KBD_COUNTERS: OversamplerCounters = OversamplerCounters::new();

/// 2-instruction oversample program. Reads `in pins, 3` with autopush=30,
/// LSB-first into ISR. After 10 samples ISR autopushes to RX FIFO.
pub struct KbdOversampleProgram<'d> {
    prg: LoadedProgram<'d, PIO1>,
}

impl<'d> KbdOversampleProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO1>) -> Self {
        let mut a: pio::Assembler<32> = pio::Assembler::new();
        let mut wrap_target = a.label();
        let mut wrap_source = a.label();
        a.bind(&mut wrap_target);
        a.r#in(pio::InSource::PINS, 3);
        a.bind(&mut wrap_source);
        let assembled = a.assemble_with_wrap(wrap_source, wrap_target);
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

/// Mask of the CLK bit within each 3-bit sample (LSB = first input pin =
/// IN_BASE = GP2 = CLK_IN).
const SAMPLE_BIT_CLK: u32 = 0b001;

/// Each 30-bit word in the RX FIFO packs 10 samples × 3 bits. With
/// `ShiftDirection::Right` the *first* sample lands in bits 0..2, second in
/// 3..5, etc. Detect any CLK toggle inside the word by XOR'ing adjacent
/// samples' CLK bits.
fn clk_toggled_in_word(word: u32) -> bool {
    let mut last = word & SAMPLE_BIT_CLK;
    for i in 1..SAMPLES_PER_WORD {
        let clk = (word >> (i * 3)) & SAMPLE_BIT_CLK;
        if clk != last {
            return true;
        }
        last = clk;
    }
    false
}

/// Spawn target — owns PIO1 SM0 + GP2/3/4 and runs forever, draining
/// oversampled words into stats counters.
#[embassy_executor::task]
pub async fn run(
    pio: Peri<'static, PIO1>,
    clk_in: Peri<'static, PIN_2>,
    clk_pull: Peri<'static, PIN_3>,
    data_in: Peri<'static, PIN_4>,
) {
    let Pio {
        mut common,
        mut sm0,
        ..
    } = Pio::new(pio, Irqs);

    let clk_in_pin = common.make_pio_pin(clk_in);
    let clk_pull_pin = common.make_pio_pin(clk_pull);
    let data_in_pin = common.make_pio_pin(data_in);

    let program = KbdOversampleProgram::new(&mut common);

    let mut cfg = Config::default();
    cfg.use_program(&program.prg, &[]);
    // IN_BASE = GP2; width 3 → reads GP2, GP3, GP4.
    cfg.set_in_pins(&[&clk_in_pin, &clk_pull_pin, &data_in_pin]);

    let clock_freq = U24F8::from_num(embassy_rp::clocks::clk_sys_freq());
    cfg.clock_divider = clock_freq / U24F8::from_num(PIO_CLK_HZ);

    cfg.fifo_join = FifoJoin::RxOnly;
    cfg.shift_in = ShiftConfig {
        auto_fill: true,
        threshold: 30,
        direction: ShiftDirection::Right,
    };

    sm0.set_config(&cfg);
    sm0.set_enable(true);

    defmt::info!(
        "ps2 kbd oversampler armed: PIO1 SM0, GP2/3/4, {} MHz / {} = {} kHz sample",
        embassy_rp::clocks::clk_sys_freq() / 1_000_000,
        cfg.clock_divider.to_num::<f32>(),
        PIO_CLK_HZ / 1000,
    );

    let mut framer = Framer::new();
    // Monotonic 1 µs-resolution timestamp. One word = 10 samples = 10 µs.
    // u64 wraps after ~580k years; never our problem.
    let mut t_us: u64 = 0;

    loop {
        let word = sm0.rx().wait_pull().await;

        let words = KBD_COUNTERS.words.fetch_add(1, Ordering::Relaxed) + 1;
        if clk_toggled_in_word(word) {
            KBD_COUNTERS
                .clk_active_words
                .fetch_add(1, Ordering::Relaxed);
        }

        // Walk 10 samples, oldest-first. With ShiftDirection::Right the
        // earliest sample sits at the LSB of the word.
        for i in 0..SAMPLES_PER_WORD {
            let sample = (word >> (i * 3)) & 0b111;
            let clk = (sample & 0b001) != 0;
            // bit 1 = CLK_PULL (our own register, ignored)
            let data = (sample & 0b100) != 0;

            if let Some(frame) = framer.ingest(clk, data, t_us) {
                KBD_COUNTERS.frames_total.fetch_add(1, Ordering::Relaxed);
                if !frame.parity_ok || !frame.framing_ok {
                    KBD_COUNTERS.frames_errored.fetch_add(1, Ordering::Relaxed);
                }
                KBD_COUNTERS.glitches_total.fetch_add(
                    u32::from(frame.timing.glitch_count),
                    Ordering::Relaxed,
                );
                defmt::info!(
                    "ps2 kbd frame: data=0x{:02X} parity_ok={} framing_ok={} glitches={} t={}us",
                    frame.data,
                    frame.parity_ok,
                    frame.framing_ok,
                    frame.timing.glitch_count,
                    frame.start_timestamp_us,
                );
            }

            t_us += 1;
        }

        // Heuristic FIFO-overrun check. The SM has no error flag we can
        // poll cheaply here without racing the next push; rely on the
        // PIO `rxstall` debug bit via the `stalled()` helper periodically
        // — once every 1024 words is enough.
        if words & 0x3FF == 0 && sm0.rx().stalled() {
            KBD_COUNTERS.fifo_overrun.fetch_add(1, Ordering::Relaxed);
        }
    }
}
