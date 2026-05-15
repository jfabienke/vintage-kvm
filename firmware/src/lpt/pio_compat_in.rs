//! IEEE 1284 SPP / nibble-mode forward byte capture, PIO + DMA-ring.
//!
//! Implements `lpt_compat_in` from `docs/pio_state_machines_design.md`
//! §10.1: a 3-instruction PIO program that waits for the host-strobe
//! falling edge, samples (strobe, D0..D7) into the ISR via `in pins, 9`
//! with autopush, then waits for the strobe to return high.
//!
//! Pin map:
//!   * IN_BASE = host-strobe (assumed GP11; flagged TBD in
//!     `docs/pio_state_machines_design.md` §4.4).
//!   * IN_BASE+1..IN_BASE+8 = D0..D7 (GP12..GP19, consecutive ✓).
//!
//! Decode on the CPU side: `byte = ((word >> 1) & 0xFF) as u8` — the
//! low bit is the strobe (always 0 at the sample point because the
//! `wait 0` just fired), bits 1..8 are D0..D7.
//!
//! ## DMA-ring offload
//!
//! Bytes pushed by the PIO are drained by DMA_CH3 into a 1 KB ring
//! (see [`super::ring_dma`]) rather than CPU-pulled per word. The PIO
//! RX FIFO is never CPU-visible while DMA is running, so `recv_byte`
//! polls the ring with a 200 µs Timer when empty. This frees the
//! per-byte `wait_pull` future overhead and gives the byte stream
//! ~50 ms of slack at SPP-nibble rates (~5 kHz). For the future
//! Phase 5 EPP/ECP modes (250 kHz), the same ring still has ~1 ms of
//! slack — well above the poll interval.

use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{
    Common, Config, FifoJoin, LoadedProgram, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_time::Timer;
use fixed::types::U24F8;

use super::hardware::LptPins;
use super::ring_dma::{self, RingHandle};
use super::{LptError, LptMode, LptPhy};

/// 3-instruction `lpt_compat_in` program. Loops:
///
/// ```text
/// .wrap_target
///     wait 0 pin 0     ; strobe falling edge
///     in pins, 9       ; sample strobe + D0..D7 (autopush)
///     wait 1 pin 0     ; strobe back to idle
/// .wrap
/// ```
pub struct CompatInProgram<'d> {
    prg: LoadedProgram<'d, PIO0>,
}

impl<'d> CompatInProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO0>) -> Self {
        let mut a: pio::Assembler<32> = pio::Assembler::new();
        let mut wrap_target = a.label();
        let mut wrap_source = a.label();
        a.bind(&mut wrap_target);
        // wait 0 pin 0 — block until pin at IN_BASE+0 (host strobe) is LOW.
        a.wait(0, pio::WaitSource::PIN, 0, false);
        // in pins, 9 — sample 9 consecutive pins starting at IN_BASE.
        a.r#in(pio::InSource::PINS, 9);
        // wait 1 pin 0 — block until host strobe returns HIGH (idle).
        a.wait(1, pio::WaitSource::PIN, 0, false);
        a.bind(&mut wrap_source);
        let assembled = a.assemble_with_wrap(wrap_source, wrap_target);
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

/// Owns the running PIO state machine + the loaded program slot +
/// the DMA-ring drain. On dismantle, the SM and the program's
/// instruction-memory slot are returned to the caller; the SM keeps
/// running until disabled.
pub struct PioCompatIn {
    sm0: StateMachine<'static, PIO0, 0>,
    program: LoadedProgram<'static, PIO0>,
    ring: RingHandle,
}

/// Inter-poll latency floor. At SPP-nibble rates (~5 kHz bytes / 200 µs
/// per byte) this matches one byte's worth of wire time, so a calling
/// task awaiting `recv_byte` sleeps at most one byte behind real-time.
const POLL_INTERVAL_US: u64 = 200;

impl PioCompatIn {
    /// Load the compat-in program into `common`, configure SM0 with
    /// the strobe + D0..D7 pin window, enable it, and arm the
    /// compat-in ring DMA.
    pub fn new(
        common: &mut Common<'static, PIO0>,
        mut sm0: StateMachine<'static, PIO0, 0>,
        pins: &LptPins,
    ) -> Self {
        let program = CompatInProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &[]);
        // IN_BASE = strobe; subsequent 8 are D0..D7. PIO requires
        // consecutive pin range; the assertion lives in set_in_pins.
        cfg.set_in_pins(&[
            &pins.strobe,
            &pins.d0,
            &pins.d1,
            &pins.d2,
            &pins.d3,
            &pins.d4,
            &pins.d5,
            &pins.d6,
            &pins.d7,
        ]);

        // 150 MHz PIO clock — instruction-paced; the host strobe pulse is
        // wide enough (DOS Stage 0/1 holds it for tens of µs) that no
        // divider is needed.
        cfg.clock_divider =
            U24F8::from_num(embassy_rp::clocks::clk_sys_freq()) / U24F8::from_num(150_000_000u32);

        cfg.fifo_join = FifoJoin::RxOnly;
        cfg.shift_in = ShiftConfig {
            auto_fill: true,
            threshold: 9,
            // Left = first sampled bit lands in the LSB of the FIFO word;
            // strobe at bit 0, D0..D7 at bits 1..8. CPU decode is then
            // `(word >> 1) & 0xFF`.
            direction: ShiftDirection::Left,
        };

        sm0.set_config(&cfg);
        sm0.set_enable(true);

        defmt::info!(
            "lpt compat-in PIO armed: PIO0 SM0, GP11..GP19, {} MHz wire-rate",
            embassy_rp::clocks::clk_sys_freq() / 1_000_000
        );

        // Arm DMA AFTER the SM is enabled so the very first byte the
        // PIO captures lands in the ring rather than stalling the SM.
        let ring = ring_dma::arm();

        Self {
            sm0,
            program: program.prg,
            ring,
        }
    }

    /// Tear down: stop DMA, disable SM, free the program's
    /// instruction-memory slot. Returns ownership of SM0 to the
    /// caller (typically `LptHardware`'s parking slot).
    pub fn dismantle(mut self, common: &mut Common<'static, PIO0>) -> StateMachine<'static, PIO0, 0> {
        ring_dma::disarm();
        self.sm0.set_enable(false);
        // Safety: SM0 has been disabled above, so no in-flight
        // instruction can still reference the freed slot. The ring
        // DMA was also stopped before disabling the SM, so the FIFO
        // isn't being drained concurrently.
        unsafe {
            common.free_instr(self.program.used_memory);
        }
        defmt::info!("lpt compat-in dismantled");
        self.sm0
    }

    /// Block until the next host-strobe falling edge, then return the
    /// captured byte. Polls the DMA ring with a 200 µs Timer fallback
    /// when no bytes are available; bursts return back-to-back without
    /// sleeping.
    pub async fn recv_byte(&mut self) -> u8 {
        loop {
            if let Some(word) = self.ring.try_pop() {
                return ((word >> 1) & 0xFF) as u8;
            }
            Timer::after_micros(POLL_INTERVAL_US).await;
        }
    }
}

impl LptPhy for PioCompatIn {
    async fn recv_byte(&mut self) -> Result<u8, LptError> {
        Ok(PioCompatIn::recv_byte(self).await)
    }

    async fn send_byte(&mut self, _b: u8) -> Result<(), LptError> {
        // This phy only owns the forward path. The reverse path is
        // implemented by `compat::SppNibblePhy`, which wraps a
        // `PioCompatIn` and adds the nibble-out bit-bang.
        Err(LptError::ModeMismatch)
    }

    fn current_mode(&self) -> LptMode {
        LptMode::SppNibble
    }
}
