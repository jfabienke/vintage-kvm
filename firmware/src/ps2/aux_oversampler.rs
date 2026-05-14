//! PS/2 AUX (mouse) wire oversampler.
//!
//! AUX-channel companion to [`super::oversampler`]. Implements
//! `ps2_aux_oversample` from `docs/pio_state_machines_design.md` §6.2: a
//! 2-instruction PIO program (`in pins, 4` + autopush) on PIO1 SM2,
//! reading GP6 (CLK_IN), GP7 (formerly LED), GP8 (PSRAM_CS), GP9
//! (DATA_IN) at 1 MS/s.
//!
//! Width 4 is required because GP7/GP8 sit between CLK_IN and DATA_IN.
//! GP7 and GP8 values are ignored on the CPU side. We sample 4 bits ×
//! 7 samples = 28 bits per RX-FIFO word (autopush threshold 28).
//!
//! Detected AUX wire activity is forwarded to the classifier as a
//! `Confirmed(At) → Confirmed(Ps2)` promotion signal. See
//! `ps2-framer::classifier::Classifier::ingest_aux_activity`.

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_rp::Peri;
use embassy_rp::peripherals::{DMA_CH2, PIO1};
use embassy_rp::pio::{
    Common, Config, FifoJoin, LoadedProgram, Pin, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_time::Timer;
use fixed::types::U24F8;

use super::ring_dma::{self, RingHandle, RING_WORDS};
use super::supervisor::AUX_ACTIVITY;
use super::Framer;
use vintage_kvm_ps2_framer::FrameKind;

/// 4-bit samples packed into 28-bit autopush threshold = 7 samples / word.
pub const SAMPLES_PER_WORD: usize = 7;

/// PIO clock target — same 1 MS/s as the KBD oversampler.
const PIO_CLK_HZ: u32 = 1_000_000;

pub struct OversamplerCounters {
    pub words: AtomicU32,
    pub clk_active_words: AtomicU32,
    pub fifo_overrun: AtomicU32,
    pub frames_total: AtomicU32,
    pub frames_errored: AtomicU32,
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

pub static AUX_COUNTERS: OversamplerCounters = OversamplerCounters::new();

pub struct AuxOversampleProgram<'d> {
    prg: LoadedProgram<'d, PIO1>,
}

impl<'d> AuxOversampleProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO1>) -> Self {
        let mut a: pio::Assembler<32> = pio::Assembler::new();
        let mut wrap_target = a.label();
        let mut wrap_source = a.label();
        a.bind(&mut wrap_target);
        a.r#in(pio::InSource::PINS, 4);
        a.bind(&mut wrap_source);
        let assembled = a.assemble_with_wrap(wrap_source, wrap_target);
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

/// Sample bit positions within each 4-bit slice. CLK = pin offset 0
/// (GP6), DATA = pin offset 3 (GP9). The middle two bits (GP7/GP8) are
/// ignored.
const SAMPLE_BIT_CLK: u32 = 0b0001;
const SAMPLE_BIT_DATA: u32 = 0b1000;

fn clk_toggled_in_word(word: u32) -> bool {
    let mut last = word & SAMPLE_BIT_CLK;
    for i in 1..SAMPLES_PER_WORD {
        let clk = (word >> (i * 4)) & SAMPLE_BIT_CLK;
        if clk != last {
            return true;
        }
        last = clk;
    }
    false
}

pub struct AuxOversampler {
    #[allow(dead_code)] // held to keep the SM alive even though we don't poll it
    sm2: StateMachine<'static, PIO1, 2>,
    ring: RingHandle,
}

impl AuxOversampler {
    pub fn new(
        common: &mut Common<'static, PIO1>,
        mut sm2: StateMachine<'static, PIO1, 2>,
        clk_in: &Pin<'static, PIO1>,
        gap0: &Pin<'static, PIO1>, // GP7 — read but discarded
        gap1: &Pin<'static, PIO1>, // GP8 — ditto
        data_in: &Pin<'static, PIO1>,
        dma_ch: Peri<'static, DMA_CH2>,
    ) -> Self {
        let program = AuxOversampleProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &[]);
        // IN_BASE = GP6; width 4 → reads GP6, GP7, GP8, GP9.
        cfg.set_in_pins(&[clk_in, gap0, gap1, data_in]);

        let clock_freq = U24F8::from_num(embassy_rp::clocks::clk_sys_freq());
        cfg.clock_divider = clock_freq / U24F8::from_num(PIO_CLK_HZ);

        cfg.fifo_join = FifoJoin::RxOnly;
        cfg.shift_in = ShiftConfig {
            auto_fill: true,
            // 7 samples × 4 bits = 28 bits per autopushed word.
            threshold: 28,
            direction: ShiftDirection::Right,
        };

        sm2.set_config(&cfg);
        sm2.set_enable(true);

        defmt::info!(
            "ps2 aux oversampler armed: PIO1 SM2, GP6/7/8/9, {} MHz / {} = {} kHz sample",
            embassy_rp::clocks::clk_sys_freq() / 1_000_000,
            cfg.clock_divider.to_num::<f32>(),
            PIO_CLK_HZ / 1000,
        );

        let ring = ring_dma::arm_aux(dma_ch);

        Self { sm2, ring }
    }
}

const POLL_INTERVAL_MS: u64 = 2;

/// AUX drain task. Signals the supervisor on each well-formed frame so
/// the shared classifier can promote `Confirmed(At) → Confirmed(Ps2)`.
/// Frame *content* on the AUX channel is mouse-protocol traffic, which
/// the classifier doesn't care about — only its existence.
#[embassy_executor::task]
pub async fn run(mut me: AuxOversampler) {
    let mut framer = Framer::new();
    let mut t_us: u64 = 0;

    loop {
        Timer::after_millis(POLL_INTERVAL_MS).await;

        let pending = me.ring.pending();
        if pending >= RING_WORDS - 1 {
            AUX_COUNTERS.fifo_overrun.fetch_add(1, Ordering::Relaxed);
            me.ring.resync();
        }

        me.ring.drain(RING_WORDS - 1, |word| {
            AUX_COUNTERS.words.fetch_add(1, Ordering::Relaxed);
            if clk_toggled_in_word(word) {
                AUX_COUNTERS
                    .clk_active_words
                    .fetch_add(1, Ordering::Relaxed);
            }

            for i in 0..SAMPLES_PER_WORD {
                let sample = (word >> (i * 4)) & 0xF;
                let clk = (sample & SAMPLE_BIT_CLK) != 0;
                let data = (sample & SAMPLE_BIT_DATA) != 0;

                if let Some(frame) = framer.ingest(clk, data, t_us) {
                    AUX_COUNTERS.frames_total.fetch_add(1, Ordering::Relaxed);
                    if !frame.parity_ok || !frame.framing_ok {
                        AUX_COUNTERS.frames_errored.fetch_add(1, Ordering::Relaxed);
                    }
                    AUX_COUNTERS.glitches_total.fetch_add(
                        u32::from(frame.timing.glitch_count),
                        Ordering::Relaxed,
                    );
                    defmt::info!(
                        "ps2 aux frame: kind={} data=0x{:02X} parity_ok={} framing_ok={}",
                        frame.kind,
                        frame.data,
                        frame.parity_ok,
                        frame.framing_ok,
                    );

                    // Signal supervisor on any well-formed frame —
                    // existence is the only thing the classifier needs.
                    let usable = frame.framing_ok
                        && frame.parity_ok
                        && frame.kind != FrameKind::Invalid;
                    if usable {
                        AUX_ACTIVITY.signal(());
                    }
                }

                t_us += 1;
            }
        });
    }
}
