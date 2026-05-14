//! IEEE 1284 SPP / nibble-mode forward byte capture, PIO-driven.
//!
//! Implements `lpt_compat_in` from `docs/pio_state_machines_design.md`
//! §10.1: a 3-instruction PIO program that waits for the host-strobe
//! falling edge, samples (strobe, D0..D7) into the ISR via `in pins, 9`
//! with autopush, then waits for the strobe to return high.
//!
//! Replaces the bit-bang `recv_byte` path that shipped with Phase 3 v0.9.
//! The reverse (nibble-out) path stays bit-bang for now; PIO
//! `lpt_nibble_out` lands in a follow-up.
//!
//! Pin map:
//!   * IN_BASE = host-strobe (assumed GP11; flagged TBD in
//!     `docs/pio_state_machines_design.md` §4.4).
//!   * IN_BASE+1..IN_BASE+8 = D0..D7 (GP12..GP19, consecutive ✓).
//!
//! Decode on the CPU side: `byte = ((word >> 1) & 0xFF) as u8` — the
//! low bit is the strobe (always 0 at the sample point because the
//! `wait 0` just fired), bits 1..8 are D0..D7.

use embassy_rp::Peri;
use embassy_rp::peripherals::{
    PIN_11, PIN_12, PIN_13, PIN_14, PIN_15, PIN_16, PIN_17, PIN_18, PIN_19, PIO0,
};
use embassy_rp::pio::{
    Common, Config, FifoJoin, LoadedProgram, ShiftConfig, ShiftDirection, StateMachine,
};
use fixed::types::U24F8;

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

/// Owns the loaded PIO state machine + the nine consumed pin handles.
pub struct PioCompatIn {
    sm0: StateMachine<'static, PIO0, 0>,
}

impl PioCompatIn {
    /// Claim PIO0 SM0 + the nine input pins (host strobe at IN_BASE,
    /// then D0..D7) and arm the program. Pins are configured as PIO
    /// inputs; callers must not have already wrapped them in
    /// `gpio::Input` or driven them as outputs.
    pub fn new(
        common: &mut Common<'static, PIO0>,
        mut sm0: StateMachine<'static, PIO0, 0>,
        strobe: Peri<'static, PIN_11>,
        d0: Peri<'static, PIN_12>,
        d1: Peri<'static, PIN_13>,
        d2: Peri<'static, PIN_14>,
        d3: Peri<'static, PIN_15>,
        d4: Peri<'static, PIN_16>,
        d5: Peri<'static, PIN_17>,
        d6: Peri<'static, PIN_18>,
        d7: Peri<'static, PIN_19>,
    ) -> Self {
        let strobe = common.make_pio_pin(strobe);
        let d0 = common.make_pio_pin(d0);
        let d1 = common.make_pio_pin(d1);
        let d2 = common.make_pio_pin(d2);
        let d3 = common.make_pio_pin(d3);
        let d4 = common.make_pio_pin(d4);
        let d5 = common.make_pio_pin(d5);
        let d6 = common.make_pio_pin(d6);
        let d7 = common.make_pio_pin(d7);

        let program = CompatInProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &[]);
        // IN_BASE = strobe; subsequent 8 are D0..D7. PIO requires
        // consecutive pin range; the assertion lives in set_in_pins.
        cfg.set_in_pins(&[&strobe, &d0, &d1, &d2, &d3, &d4, &d5, &d6, &d7]);

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

        Self { sm0 }
    }

    /// Block until the next host-strobe falling edge, then return the
    /// captured byte.
    pub async fn recv_byte(&mut self) -> u8 {
        let word = self.sm0.rx().wait_pull().await;
        ((word >> 1) & 0xFF) as u8
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
