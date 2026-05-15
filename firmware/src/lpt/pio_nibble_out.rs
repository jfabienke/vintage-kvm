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
//! the byte-boundary phase is invariant: every send starts and ends
//! with the same phase state, which is whatever we initialized to at
//! boot (LOW). No CPU-side state tracking needed.
//!
//! ## DMA-driven batched send
//!
//! `send_bytes` packs each byte into one u32 and hands the whole slice
//! to DMA_CH4 via `sm.tx().dma_push`. The CPU does pack + arm in
//! microseconds; PIO consumes from the TX FIFO at its own 210 µs/byte
//! cadence. The Transfer future completes as soon as the last word is
//! written to the FIFO — the wire output continues for up to ~1.7 ms
//! after (8-deep FIFO × 210 µs), but that's fine: the caller's next
//! `recv_byte` blocks waiting for the host's response, which can't
//! arrive until our last byte reaches it.

use embassy_rp::dma;
use embassy_rp::gpio::Level;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{
    Common, Config, Direction, FifoJoin, LoadedProgram, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_time::Timer;
use fixed::types::U24F8;

use super::hardware::LptPins;
use super::{LptError, LptMode, LptPhy};

/// PIO clock target: 100 kHz → 10 µs/cycle, lets `nop [9]` cover the
/// 100 µs nibble settle in a single instruction.
const PIO_CLK_HZ: u32 = 100_000;

/// Maximum bytes packable into one DMA batch — sized to fit `MAX_PACKET`
/// from the protocol crate. One byte = one u32 (two 5-bit nibbles +
/// padding), so the buffer is 256 × 4 = 1024 bytes on the caller's
/// stack frame.
const MAX_BATCH_BYTES: usize = 256;

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
    dma: dma::Channel<'static>,
    program: LoadedProgram<'static, PIO0>,
}

impl PioNibbleOut {
    pub fn new(
        common: &mut Common<'static, PIO0>,
        mut sm1: StateMachine<'static, PIO0, 1>,
        dma: dma::Channel<'static>,
        lpt_pins: &LptPins,
    ) -> Self {
        let pins = [
            &lpt_pins.nack,
            &lpt_pins.busy,
            &lpt_pins.perror,
            &lpt_pins.select,
            &lpt_pins.nfault,
        ];

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

        Self {
            sm1,
            dma,
            program: program.prg,
        }
    }

    /// Tear down: wait for the TX FIFO + wire to drain, disable SM,
    /// free the program's instruction-memory slot. Returns the SM and
    /// the DMA channel for re-parking in `LptHardware`.
    pub async fn dismantle(
        mut self,
        common: &mut Common<'static, PIO0>,
    ) -> (StateMachine<'static, PIO0, 1>, dma::Channel<'static>) {
        // FIFO depth 8 × 210 µs per byte ≈ 1.7 ms worst-case to drain
        // a full FIFO to wire. 5 ms is comfortable headroom and still
        // imperceptible at the 1284 negotiation cadence we use it at.
        Timer::after_millis(5).await;
        self.sm1.set_enable(false);
        // Safety: SM1 has been disabled, so the freed instruction-memory
        // slot can't still be referenced by an in-flight PC fetch.
        unsafe {
            common.free_instr(self.program.used_memory);
        }
        defmt::info!("lpt nibble-out dismantled");
        (self.sm1, self.dma)
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

    fn pack_byte(byte: u8) -> u32 {
        // Phase invariant: byte boundaries always end at LOW phase (see
        // module docs), so the first nibble of every byte toggles to
        // HIGH and the second toggles back to LOW.
        let lo = Self::pack_nibble(byte & 0x0F, true) as u32;
        let hi = Self::pack_nibble((byte >> 4) & 0x0F, false) as u32;
        lo | (hi << 5)
    }

    /// Send one byte. Single-word DMA transfer — mostly used for tests
    /// or one-shot replies; bulk packet sends go through `send_bytes`.
    pub async fn send_byte(&mut self, byte: u8) {
        let words = [Self::pack_byte(byte)];
        self.sm1.tx().dma_push(&mut self.dma, &words, false).await;
    }

    /// Send a slice of bytes as one DMA batch. Caller's slice may not
    /// exceed `MAX_BATCH_BYTES` (= `protocol::MAX_PACKET`). Returns
    /// after the last byte has been queued into the PIO TX FIFO;
    /// physical wire output trails by up to ~1.7 ms (FIFO depth × byte
    /// wire time). For LPT this is benign because the host can't
    /// respond until the wire output completes.
    pub async fn send_bytes(&mut self, bytes: &[u8]) {
        assert!(
            bytes.len() <= MAX_BATCH_BYTES,
            "send_bytes batch too large: {} > {}",
            bytes.len(),
            MAX_BATCH_BYTES
        );
        let mut words = [0u32; MAX_BATCH_BYTES];
        for (i, &b) in bytes.iter().enumerate() {
            words[i] = Self::pack_byte(b);
        }
        self.sm1
            .tx()
            .dma_push(&mut self.dma, &words[..bytes.len()], false)
            .await;
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

    async fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), LptError> {
        PioNibbleOut::send_bytes(self, bytes).await;
        Ok(())
    }

    fn current_mode(&self) -> LptMode {
        LptMode::SppNibble
    }
}
