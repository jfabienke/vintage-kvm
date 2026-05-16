//! EPP `DIR` follower — drives the 74LVC161284's DIR (GP29) from
//! the host's `nWrite` line (GP11) via PIO0 SM2.
//!
//! In EPP mode the host signals data-cycle direction on `nWrite`:
//! `L` = host writes (forward), `H` = host reads (peripheral writes
//! reverse). The chip's DIR input has the same semantics. So
//! mirroring `nWrite → DIR` at PIO speed gives us a per-cycle
//! direction flip with zero CPU involvement.
//!
//! ```text
//! .wrap_target
//!     mov pins, pins   ; OUT_BASE := IN_BASE
//! .wrap
//! ```
//!
//! One instruction. IN_BASE = GP11 (`nWrite`), width 1. OUT_BASE =
//! GP29 (`DIR`), width 1. Clocked at full PIO speed (150 MHz / 6.7
//! ns per loop). The two-flop input synchronizer adds ~2 cycles, so
//! the effective `nWrite ↦ DIR` latency is ~20 ns — three orders of
//! magnitude under EPP's 500 ns cycle ceiling.
//!
//! ## Lifecycle
//!
//! Built by [`super::epp::EppPhy::build`] after `LptMux` has flipped
//! the chip's HD pin to totem-pole (via the pre-build hook on
//! `LptMux::switch_to`). On dismantle the SM is disabled and its
//! instruction-memory slot is freed, then DIR's FUNCSEL flips back to
//! SIO so [`super::hardware::LptHardware::set_transceiver_mode`] can
//! resume CPU-driven level control for the next mode.

use embassy_rp::pac;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{
    Common, Config, FifoJoin, LoadedProgram, ShiftConfig, ShiftDirection, StateMachine,
};
use fixed::types::U24F8;

use super::hardware::LptPins;

/// FUNCSEL value selecting PIO0 for an IO_BANK0 GPIO. Matches the
/// constant in `negotiator.rs`.
const FUNCSEL_PIO0: u8 = 6;
const FUNCSEL_SIO: u8 = 5;

pub struct DirFollowerProgram<'d> {
    prg: LoadedProgram<'d, PIO0>,
}

impl<'d> DirFollowerProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO0>) -> Self {
        let mut a: pio::Assembler<32> = pio::Assembler::new();
        let mut wrap_target = a.label();
        let mut wrap_source = a.label();
        a.bind(&mut wrap_target);
        // mov pins, pins — read IN_BASE (GP11/nWrite) and write
        // OUT_BASE (GP29/DIR) in one cycle.
        a.mov(
            pio::MovDestination::PINS,
            pio::MovOperation::None,
            pio::MovSource::PINS,
        );
        a.bind(&mut wrap_source);
        let assembled = a.assemble_with_wrap(wrap_source, wrap_target);
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

pub struct PioDirFollower {
    sm2: StateMachine<'static, PIO0, 2>,
    program: LoadedProgram<'static, PIO0>,
}

impl PioDirFollower {
    /// Claim PIO0 SM2, flip GP29 (DIR) to PIO function, configure
    /// the mirror program, and enable the SM. The mirror starts
    /// immediately — the very first `nWrite` transition after this
    /// call drives DIR.
    pub fn new(
        common: &mut Common<'static, PIO0>,
        mut sm2: StateMachine<'static, PIO0, 2>,
        pins: &LptPins,
    ) -> Self {
        let program = DirFollowerProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &[]);
        // IN_BASE = nWrite (GP11), width 1.
        cfg.set_in_pins(&[&pins.strobe]);
        // OUT_BASE = DIR (GP29), width 1.
        cfg.set_out_pins(&[&pins.dir]);

        // Full PIO clock. Mirror latency is dominated by the input
        // synchronizer, not by program length.
        cfg.clock_divider = U24F8::from_num(1u32);

        // Neither FIFO is used; join them to free up depth even
        // though we never push or pull.
        cfg.fifo_join = FifoJoin::Duplex;
        cfg.shift_in = ShiftConfig {
            auto_fill: false,
            threshold: 32,
            direction: ShiftDirection::Right,
        };
        cfg.shift_out = ShiftConfig {
            auto_fill: false,
            threshold: 32,
            direction: ShiftDirection::Right,
        };

        // Set DIR as a PIO output. SET pindirs to Out before flipping
        // FUNCSEL so the line never floats during the handoff.
        sm2.set_pin_dirs(embassy_rp::pio::Direction::Out, &[&pins.dir]);
        // Flip GP29's FUNCSEL from SIO to PIO0 so the SM's output
        // actually reaches the pad.
        Self::set_dir_funcsel(FUNCSEL_PIO0);

        sm2.set_config(&cfg);
        sm2.set_enable(true);

        defmt::info!("lpt dir-follower PIO armed: PIO0 SM2, GP11 → GP29");

        Self {
            sm2,
            program: program.prg,
        }
    }

    /// Tear down: disable SM2, free the program's instruction slot,
    /// and hand DIR back to SIO so the static-mode CPU driver
    /// (`LptHardware::drive_dir_sio`) resumes control.
    pub fn dismantle(
        mut self,
        common: &mut Common<'static, PIO0>,
    ) -> StateMachine<'static, PIO0, 2> {
        self.sm2.set_enable(false);
        // Safety: SM2 is disabled above, so no in-flight instruction
        // can still reference the freed slot.
        unsafe {
            common.free_instr(self.program.used_memory);
        }
        // Hand DIR back to SIO. The next CPU-driven level write
        // takes effect immediately because SIO is the active source
        // for the pad from this point on.
        Self::set_dir_funcsel(FUNCSEL_SIO);
        defmt::info!("lpt dir-follower dismantled");
        self.sm2
    }

    fn set_dir_funcsel(funcsel: u8) {
        const GP_DIR: usize = 29;
        pac::IO_BANK0.gpio(GP_DIR).ctrl().modify(|w| {
            w.set_funcsel(funcsel);
        });
    }
}
