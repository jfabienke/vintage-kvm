//! IEEE 1284 EPP forward + reverse (combined) data-cycle handler.
//!
//! Implements `lpt_epp` from `docs/pio_state_machines_design.md` §10.4
//! on PIO0 SM0. One SM runs both directions, dispatched on a
//! direction bit the CPU prepends to each TX word.
//!
//! ```text
//! .side_set 1                       ; nWait drives via side-set
//!
//! start:
//!     pull block       side 1       ; cmd word
//!     out x, 1         side 1       ; X = dir bit  (0=fwd, 1=rev)
//!     out null, 23     side 1       ; discard padding
//!     jmp !x forward   side 1
//!
//! reverse:                          ; Pico → host
//!     out pins, 8      side 1       ; drive D0..D7
//!     wait 0 gpio 20   side 1       ; host strobes nDataStb LOW
//!     nop [4]          side 0       ; nWait LOW (latch / handshake)
//!     wait 1 gpio 20   side 1       ; host releases strobe
//!     jmp start        side 1
//!
//! forward:                          ; host → Pico
//!     wait 0 gpio 20   side 1       ; host strobes nDataStb LOW
//!     in pins, 8       side 0       ; sample D0..D7, nWait LOW
//!     push             side 0
//!     wait 1 gpio 20   side 0
//!     jmp start        side 1       ; release nWait on next iter
//! ```
//!
//! Pin map:
//! - OUT_BASE / IN_BASE = D0 (GP12), width 8 → GP12..GP19
//! - SIDE_BASE = nWait (GP24)
//! - Absolute `wait gpio 20` polls nDataStb (GP20)
//!
//! ## TX word layout
//!
//! 32 bits LSB-first via right-shift OSR:
//! ```text
//!  bit 0      = direction (0 = forward, 1 = reverse)
//!  bits 1..23 = padding (consumed by `out null, 23`, ignored)
//!  bits 24..31 = data byte (only meaningful for reverse)
//! ```
//!
//! Helpers [`encode_forward_cmd`] and [`encode_reverse_cmd`] pack the
//! word so callers don't fight the bit layout.
//!
//! ## Direction control
//!
//! D0..D7 bidirectionality is selected by the 74LVC161284's DIR
//! pin (`docs/hardware_reference.md` §11.3). EPP's per-cycle
//! direction flip is handled by [`super::pio_dir_follower`] on
//! PIO0 SM2 — a one-instruction `mov pins, pins` mirror that drives
//! DIR from the host's `nWrite` line at PIO clock speed. The CPU
//! never touches DIR while EPP is active.
//!
//! ## Address cycles
//!
//! EPP distinguishes data cycles (nDataStb) from address cycles
//! (nAddrStb = nSelectIn / GP22). Only data cycles are handled
//! here today; an address-cycle variant would be a second SM
//! (or a parallel branch in this program) — out of scope until
//! Stage 1 needs address-space access.
//!
//! ## Clock
//!
//! PIO clock target: 30 MHz (33 ns/cycle). One round-trip is
//! ~5 cycles ≈ 165 ns. EPP wire ceiling is ~500 kB/s = 2 µs/byte,
//! so PIO is far from the bottleneck.

use embassy_rp::dma;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{
    Common, Config, FifoJoin, LoadedProgram, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_time::Timer;
use fixed::types::U24F8;
use pio::SideSet;

use super::hardware::LptPins;

const PIO_CLK_HZ: u32 = 30_000_000;

/// Absolute GPIO number for nDataStb. Matches `LptPins::auto_fd` =
/// GP20 per `docs/hardware_reference.md` §3.3.
const NDATASTB_GPIO: u8 = 20;

pub fn encode_forward_cmd() -> u32 {
    // direction=0 in bit 0; data bits irrelevant (consumed but discarded).
    0
}

pub fn encode_reverse_cmd(byte: u8) -> u32 {
    // direction=1 in bit 0; bits 1..23 padding (zeros); byte in bits 24..31.
    1u32 | ((byte as u32) << 24)
}

pub struct EppProgram<'d> {
    prg: LoadedProgram<'d, PIO0>,
}

impl<'d> EppProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO0>) -> Self {
        let mut a: pio::Assembler<32> =
            pio::Assembler::new_with_side_set(SideSet::new(false, 1, false));
        let mut start = a.label();
        let mut forward = a.label();

        a.bind(&mut start);
        // pull block — wait for next cmd word.
        a.pull_with_side_set(false, true, 1);
        // out x, 1 — direction bit → X.
        a.out_with_side_set(pio::OutDestination::X, 1, 1);
        // out null, 23 — discard padding.
        a.out_with_side_set(pio::OutDestination::NULL, 23, 1);
        // jmp !x forward — X==0 → forward.
        a.jmp_with_side_set(pio::JmpCondition::XIsZero, &mut forward, 1);

        // reverse branch.
        a.out_with_side_set(pio::OutDestination::PINS, 8, 1);
        a.wait_with_side_set(0, pio::WaitSource::GPIO, NDATASTB_GPIO, false, 1);
        a.nop_with_delay_and_side_set(4, 0);
        a.wait_with_side_set(1, pio::WaitSource::GPIO, NDATASTB_GPIO, false, 1);
        a.jmp_with_side_set(pio::JmpCondition::Always, &mut start, 1);

        // forward branch.
        a.bind(&mut forward);
        a.wait_with_side_set(0, pio::WaitSource::GPIO, NDATASTB_GPIO, false, 1);
        a.r#in_with_side_set(pio::InSource::PINS, 8, 0);
        a.push_with_side_set(false, true, 0);
        a.wait_with_side_set(1, pio::WaitSource::GPIO, NDATASTB_GPIO, false, 0);
        a.jmp_with_side_set(pio::JmpCondition::Always, &mut start, 1);

        let assembled = a.assemble_program();
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

pub struct PioEpp {
    sm0: StateMachine<'static, PIO0, 0>,
    dma: dma::Channel<'static>,
    program: LoadedProgram<'static, PIO0>,
}

impl PioEpp {
    pub fn new(
        common: &mut Common<'static, PIO0>,
        mut sm0: StateMachine<'static, PIO0, 0>,
        dma: dma::Channel<'static>,
        pins: &LptPins,
    ) -> Self {
        let data_pins = [
            &pins.d0, &pins.d1, &pins.d2, &pins.d3, &pins.d4, &pins.d5, &pins.d6, &pins.d7,
        ];
        let side_pins = [&pins.busy]; // GP24 = nWait in EPP

        let program = EppProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &side_pins);
        cfg.set_out_pins(&data_pins);
        cfg.set_in_pins(&data_pins);

        cfg.clock_divider =
            U24F8::from_num(embassy_rp::clocks::clk_sys_freq()) / U24F8::from_num(PIO_CLK_HZ);

        cfg.fifo_join = FifoJoin::Duplex;
        // Forward: ISR autopushes 8 bits per host cycle.
        cfg.shift_in = ShiftConfig {
            auto_fill: false,
            threshold: 8,
            direction: ShiftDirection::Left,
        };
        // Reverse: OSR consumes 32-bit cmd words as described in the
        // module docstring.
        cfg.shift_out = ShiftConfig {
            auto_fill: false,
            threshold: 32,
            direction: ShiftDirection::Right,
        };

        sm0.set_config(&cfg);
        sm0.set_enable(true);

        defmt::info!(
            "lpt epp PIO armed: PIO0 SM0, D0..D7 GP12..GP19, nWait GP24, {} MHz",
            PIO_CLK_HZ / 1_000_000
        );

        Self {
            sm0,
            dma,
            program: program.prg,
        }
    }

    /// Push a forward-direction command (request from host).
    /// Caller must then `recv_byte` to get the actual data byte.
    pub async fn send_forward_cmd(&mut self) {
        let word = [encode_forward_cmd()];
        self.sm0.tx().dma_push(&mut self.dma, &word, false).await;
    }

    /// Push a reverse-direction byte for the SM to drive at the next
    /// host strobe.
    pub async fn send_reverse_byte(&mut self, byte: u8) {
        let word = [encode_reverse_cmd(byte)];
        self.sm0.tx().dma_push(&mut self.dma, &word, false).await;
    }

    /// Read the next byte the SM captured in the forward direction.
    /// Blocks until the RX FIFO has data.
    pub async fn recv_byte(&mut self) -> u8 {
        let word = self.sm0.rx().wait_pull().await;
        (word & 0xFF) as u8
    }

    pub async fn dismantle(
        mut self,
        common: &mut Common<'static, PIO0>,
    ) -> (StateMachine<'static, PIO0, 0>, dma::Channel<'static>) {
        Timer::after_millis(5).await;
        self.sm0.set_enable(false);
        unsafe {
            common.free_instr(self.program.used_memory);
        }
        defmt::info!("lpt epp dismantled");
        (self.sm0, self.dma)
    }
}
