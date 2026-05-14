//! IEEE 1284 SPP nibble-mode reverse byte send, PIO-driven.
//!
//! Implements `lpt_nibble_out` from `docs/pio_state_machines_design.md`
//! §10.2 on PIO0 SM1. 5-instruction program drives one byte as two
//! nibbles with a 100 µs settle per nibble; CPU pre-packs both nibbles
//! into one 10-bit value and pushes a single u32 to the TX FIFO.
//!
//! ```text
//! .program lpt_nibble_out
//! .wrap_target
//!     pull block       ; receive 10-bit packed nibble pair
//!     out pins, 5      ; drive low nibble + new phase
//!     nop [9]          ; 100 µs settle (PIO clock = 100 kHz)
//!     out pins, 5      ; drive high nibble + new phase
//!     nop [9]          ; 100 µs settle
//! .wrap
//! ```
//!
//! Pin map (OUT_BASE = GP23, width 5; consecutive ✓):
//!
//! | Pin offset | GPIO | Status bit | Nibble role |
//! |---|---|---|---|
//! | 0 | GP23 | nAck    (bit 6) | nibble bit 3 |
//! | 1 | GP24 | Busy    (bit 7) | phase        |
//! | 2 | GP25 | PError  (bit 5) | nibble bit 2 |
//! | 3 | GP26 | Select  (bit 4) | nibble bit 1 |
//! | 4 | GP27 | nFault  (bit 3) | nibble bit 0 |
//!
//! PIO clock = 100 kHz (10 µs/cycle); `nop [9]` = 10 cycles = 100 µs.
//! The PIO instruction delay field is only 5 bits (max 31 cycles), so
//! the 100 µs settle target forces this clock-divider choice — a
//! 1 MHz clock + `nop [99]` won't fit in one instruction.
//!
//! ## Phase invariant
//!
//! Phase toggles on every nibble emission. Over a full byte (two
//! nibbles), phase toggles twice → returns to its original value. So
//! the byte-boundary phase is invariant: every `send_byte` starts and
//! ends with the same phase state, which is whatever we initialized to
//! at boot (LOW). No CPU-side state tracking needed.

use embassy_rp::Peri;
use embassy_rp::gpio::Level;
use embassy_rp::peripherals::{PIN_23, PIN_24, PIN_25, PIN_26, PIN_27, PIO0};
use embassy_rp::pio::{
    Common, Config, Direction, FifoJoin, LoadedProgram, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_time::Timer;
use fixed::types::U24F8;

use super::{LptError, LptMode, LptPhy};

/// Total wire-time for one byte: 2 × (100 µs settle + ~1 cycle out).
/// We sleep this long after pushing so a back-to-back `send_byte` never
/// races the PIO. Could be reduced by polling `sm.tx().stalled()` but
/// SPP-nibble rates make the extra ~10 µs irrelevant.
const BYTE_WIRE_TIME_US: u64 = 210;

/// PIO clock target: 100 kHz → 10 µs/cycle, lets `nop [9]` cover the
/// 100 µs nibble settle in a single instruction.
const PIO_CLK_HZ: u32 = 100_000;

pub struct NibbleOutProgram<'d> {
    prg: LoadedProgram<'d, PIO0>,
}

impl<'d> NibbleOutProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO0>) -> Self {
        let mut a: pio::Assembler<32> = pio::Assembler::new();
        let mut wrap_target = a.label();
        let mut wrap_source = a.label();
        a.bind(&mut wrap_target);
        // pull block — wait for CPU to provide the next packed nibble pair.
        a.pull(false, true);
        // out pins, 5 — drive low nibble + new phase (OSR LSB-first).
        a.out(pio::OutDestination::PINS, 5);
        // nop [9] — 10 cycles = 100 µs settle at 100 kHz.
        a.nop_with_delay(9);
        // out pins, 5 — drive high nibble + new phase.
        a.out(pio::OutDestination::PINS, 5);
        // nop [9] — 100 µs settle, then wrap back to pull.
        a.nop_with_delay(9);
        a.bind(&mut wrap_source);
        let assembled = a.assemble_with_wrap(wrap_source, wrap_target);
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

pub struct PioNibbleOut {
    sm1: StateMachine<'static, PIO0, 1>,
}

impl PioNibbleOut {
    pub fn new(
        common: &mut Common<'static, PIO0>,
        mut sm1: StateMachine<'static, PIO0, 1>,
        nack: Peri<'static, PIN_23>,
        busy: Peri<'static, PIN_24>,
        perror: Peri<'static, PIN_25>,
        select: Peri<'static, PIN_26>,
        nfault: Peri<'static, PIN_27>,
    ) -> Self {
        let nack = common.make_pio_pin(nack);
        let busy = common.make_pio_pin(busy);
        let perror = common.make_pio_pin(perror);
        let select = common.make_pio_pin(select);
        let nfault = common.make_pio_pin(nfault);

        let pins = [&nack, &busy, &perror, &select, &nfault];

        // Drive a known idle (LOW everywhere) before enabling the SM, so
        // DOS's first phase-edge detect sees a clean transition out of
        // LOW on the first emitted nibble. Order matters: set the level
        // first, then flip pindir to Out so the line never floats.
        sm1.set_pins(Level::Low, &pins);
        sm1.set_pin_dirs(Direction::Out, &pins);

        let program = NibbleOutProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &[]);
        cfg.set_out_pins(&pins);
        cfg.set_set_pins(&pins);

        cfg.clock_divider =
            U24F8::from_num(embassy_rp::clocks::clk_sys_freq()) / U24F8::from_num(PIO_CLK_HZ);

        cfg.fifo_join = FifoJoin::TxOnly;
        cfg.shift_out = ShiftConfig {
            auto_fill: false,
            // 10 bits per byte: bits 0..4 = lo nibble pattern, bits 5..9
            // = hi nibble pattern. `out pins, 5` × 2 consumes both.
            threshold: 10,
            direction: ShiftDirection::Right,
        };

        sm1.set_config(&cfg);
        sm1.set_enable(true);

        defmt::info!(
            "lpt nibble-out PIO armed: PIO0 SM1, GP23..GP27, {} kHz wire-rate",
            PIO_CLK_HZ / 1000
        );

        Self { sm1 }
    }

    /// Pack one nibble + phase bit into the 5-bit pin field driven by
    /// `out pins, 5` with OUT_BASE=GP23.
    fn pack_nibble(nibble: u8, phase: bool) -> u8 {
        ((nibble >> 3) & 1)             // pin 0 (GP23 / nAck)    = nib bit 3
            | ((phase as u8) << 1)      // pin 1 (GP24 / Busy)    = phase
            | (((nibble >> 2) & 1) << 2)// pin 2 (GP25 / PError)  = nib bit 2
            | (((nibble >> 1) & 1) << 3)// pin 3 (GP26 / Select)  = nib bit 1
            | (((nibble) & 1) << 4)     // pin 4 (GP27 / nFault)  = nib bit 0
    }

    /// Send one byte as two nibbles (low first), each with phase toggled
    /// against the previous wire state.
    pub async fn send_byte(&mut self, byte: u8) {
        // Phase invariant: byte boundaries always end at LOW phase (see
        // module docs), so the first nibble of every byte toggles to
        // HIGH and the second toggles back to LOW.
        let lo_bits = Self::pack_nibble(byte & 0x0F, true) as u32;
        let hi_bits = Self::pack_nibble((byte >> 4) & 0x0F, false) as u32;
        let word = lo_bits | (hi_bits << 5);

        self.sm1.tx().wait_push(word).await;
        // Hold off the next push until the PIO has finished driving both
        // nibbles. Without this the TX FIFO would queue ahead of the
        // wire and DOS would see two phase toggles before its polling
        // loop has even registered the first.
        Timer::after_micros(BYTE_WIRE_TIME_US).await;
    }
}

impl LptPhy for PioNibbleOut {
    async fn recv_byte(&mut self) -> Result<u8, LptError> {
        Err(LptError::ModeMismatch)
    }

    async fn send_byte(&mut self, b: u8) -> Result<(), LptError> {
        PioNibbleOut::send_byte(self, b).await;
        Ok(())
    }

    fn current_mode(&self) -> LptMode {
        LptMode::SppNibble
    }
}
