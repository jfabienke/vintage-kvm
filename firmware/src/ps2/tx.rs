//! PS/2 TX path — device-to-host frame emitter.
//!
//! Generic over PIO1 SM index so the same program drives both the
//! keyboard side (SM1, GP3/GP5) and the AUX/mouse side (SM3, GP28/GP10).
//! Per `docs/pio_state_machines_design.md` §9, the program drives
//! CLK_PULL via `set pins` and DATA_PULL via `mov pins`, both at 100 kHz
//! PIO clock → 80 µs/bit (12.5 kHz wire rate, mid-band of the PS/2 spec).
//!
//! ```text
//! .program ps2_kbd_tx
//!     set y, 10              ; 11 bits to send (loop while y >= 0)
//!     pull block             ; wait for CPU-packed frame
//! bit_loop:
//!     out x, 1               ; X = next bit (LSB-first)
//!     set pins, 0 [3]        ; CLK_PULL=0 → drive CLK low, 4-cycle hold
//!     mov pins, x            ; DATA_PULL = bit (0 → drive low; 1 → release)
//!     set pins, 1 [3]        ; CLK_PULL=1 → release CLK, 4-cycle hold
//!     jmp y-- bit_loop
//!     set pins, 1            ; idle: CLK released
//! .wrap                       ; back to set y, 10
//! ```
//!
//! Wire convention through the 74LVC07A open-drain buffer:
//! pin HIGH = release (wire pulled high by host); pin LOW = drive wire LOW.
//! So a "1" bit on the wire = `mov pins, 1` = release DATA_PULL; "0" bit =
//! drive DATA_PULL LOW. Non-inverting.
//!
//! ## Frame formats
//!
//! AT / PS-2 — 11 bits: start(0) + D0..D7 + odd parity + stop(1).
//! XT       —  9 bits: start(1) + D0..D7.
//!
//! [`pack_at_frame`] and [`pack_xt_frame`] pack the frame LSB-first into
//! the low bits of a u32; remaining bits are padded with 1s (idle-high).
//! The TX SM consumes the first N bits and stalls on the next `pull block`
//! when the loop wraps.
//!
//! ## Phase 1 status
//!
//! Constructed in `main` so the SM is loaded and idle, but no consumer
//! is wired up yet. The bootstrap injection logic (Phase 2) and i8042
//! private-channel TX path will both call `send_at_byte` / `send_xt_byte`
//! when they land.

#![allow(dead_code)] // No consumer yet; ready for Phase 2.

use embassy_rp::gpio::Level;
use embassy_rp::peripherals::PIO1;
use embassy_rp::pio::{
    Common, Config, Direction, FifoJoin, LoadedProgram, Pin, ShiftConfig, ShiftDirection,
    StateMachine,
};
use fixed::types::U24F8;
use vintage_kvm_ps2_framer::{pack_at_frame, pack_xt_frame};

/// PIO clock target: 100 kHz → 10 µs/cycle. Bit time = 8 cycles = 80 µs.
const PIO_CLK_HZ: u32 = 100_000;

/// AT/PS-2 frame is 11 bits — loop runs `set y, AT_LOOP_INIT` then jmp y--.
const AT_LOOP_INIT: u8 = 10;
/// XT frame is 9 bits.
const XT_LOOP_INIT: u8 = 8;

pub struct TxProgram<'d> {
    prg: LoadedProgram<'d, PIO1>,
}

impl<'d> TxProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO1>) -> Self {
        let mut a: pio::Assembler<32> = pio::Assembler::new();
        let mut wrap_target = a.label();
        let mut bit_loop = a.label();
        let mut wrap_source = a.label();

        a.bind(&mut wrap_target);
        // set y, 10 — loop counter for 11 iterations (y from 10 down to 0).
        // Overwritten via `exec_instr` from `send_at_byte` / `send_xt_byte`
        // when the count differs (XT mode).
        a.set(pio::SetDestination::Y, AT_LOOP_INIT as u8);
        // pull block — stall until CPU pushes a frame.
        a.pull(false, true);

        a.bind(&mut bit_loop);
        // out x, 1 — pull next bit out of OSR (LSB-first) into X.
        a.out(pio::OutDestination::X, 1);
        // set pins, 0 [3] — CLK_PULL=0 (drive CLK low), hold 4 cycles.
        a.set_with_delay(pio::SetDestination::PINS, 0, 3);
        // mov pins, x — write bit value to DATA_PULL.
        a.mov(
            pio::MovDestination::PINS,
            pio::MovOperation::None,
            pio::MovSource::X,
        );
        // set pins, 1 [3] — CLK_PULL=1 (release CLK), hold 4 cycles.
        a.set_with_delay(pio::SetDestination::PINS, 1, 3);
        // jmp y-- bit_loop — decrement Y; loop while non-zero.
        a.jmp(pio::JmpCondition::YDecNonZero, &mut bit_loop);
        // set pins, 1 — idle: CLK released between frames.
        a.set(pio::SetDestination::PINS, 1);
        a.bind(&mut wrap_source);

        let assembled = a.assemble_with_wrap(wrap_source, wrap_target);
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

pub struct Tx<const SM: usize> {
    sm: StateMachine<'static, PIO1, SM>,
    label: &'static str,
}

impl<const SM: usize> Tx<SM> {
    pub fn new(
        common: &mut Common<'static, PIO1>,
        mut sm: StateMachine<'static, PIO1, SM>,
        clk_pull: &Pin<'static, PIO1>,
        data_pull: &Pin<'static, PIO1>,
        label: &'static str,
    ) -> Self {
        // Idle both lines HIGH (release through the open-drain buffer)
        // before flipping pindirs to Out, so the wire never glitches LOW
        // during init.
        sm.set_pins(Level::High, &[clk_pull, data_pull]);
        sm.set_pin_dirs(Direction::Out, &[clk_pull, data_pull]);

        let program = TxProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &[]);
        // SET_BASE = CLK_PULL, 1 bit wide.
        cfg.set_set_pins(&[clk_pull]);
        // OUT_BASE = DATA_PULL, 1 bit wide. `mov pins, x` writes to
        // OUT pins; `out pins, N` would too. Both honor `out_count`.
        cfg.set_out_pins(&[data_pull]);

        cfg.clock_divider =
            U24F8::from_num(embassy_rp::clocks::clk_sys_freq()) / U24F8::from_num(PIO_CLK_HZ);

        cfg.fifo_join = FifoJoin::TxOnly;
        cfg.shift_out = ShiftConfig {
            auto_fill: false,
            threshold: 32,
            direction: ShiftDirection::Right,
        };

        sm.set_config(&cfg);
        sm.set_enable(true);

        defmt::info!(
            "ps2 {} tx armed: PIO1 SM{}, {} kHz wire-rate",
            label,
            SM,
            PIO_CLK_HZ / 80
        );

        Self { sm, label }
    }

    /// Send one AT/PS-2 byte. Blocks until the TX FIFO accepts the packed
    /// frame; the SM serializes onto the wire over ~880 µs (11 bits × 80
    /// µs). A subsequent `send_at_byte` call queues behind it in the
    /// FIFO; the wire timing is enforced by the PIO program.
    pub async fn send_at_byte(&mut self, byte: u8) {
        let _ = self.label; // silence unused-field warning
        let frame = pack_at_frame(byte);
        self.sm.tx().wait_push(frame).await;
    }

    /// Send one XT byte (9-bit frame, no parity). Mode selection happens
    /// CPU-side; the PIO program is the same shape, just runs fewer loop
    /// iterations because the packed frame's "active" bits stop at 9.
    pub async fn send_xt_byte(&mut self, byte: u8) {
        let frame = pack_xt_frame(byte);
        // Override the loop init Y register so the SM clocks only 9 bits.
        // exec_instr injects an instruction out-of-band; valid only when
        // the SM is stalled on `pull block`.
        //
        // PIO `set y, N` opcode = 0b111_00000_010_NNNNN (set y, immediate).
        let set_y_xt: u16 = 0b111_00000_010_00000 | XT_LOOP_INIT as u16;
        // Safety: exec_instr is unsafe because it can desync the SM if
        // the instruction conflicts with what's executing. Here the SM
        // is guaranteed stalled on `pull block` because the TX FIFO has
        // been empty (caller serializes sends).
        unsafe {
            self.sm.exec_instr(set_y_xt);
        }
        self.sm.tx().wait_push(frame).await;
    }
}

/// AT/PS-2 keyboard TX on PIO1 SM1 (GP3=CLK_PULL, GP5=DATA_PULL).
pub type KbdTx = Tx<1>;
/// AT/PS-2 mouse/AUX TX on PIO1 SM3 (GP28=CLK_PULL, GP10=DATA_PULL).
pub type AuxTx = Tx<3>;

// Frame packing lives in `crates/ps2-framer::packer` — see that crate's
// tests/packer.rs for the layout + round-trip coverage.
