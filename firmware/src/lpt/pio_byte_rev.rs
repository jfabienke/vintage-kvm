//! IEEE 1284 byte-mode reverse byte send, PIO-driven.
//!
//! Implements `lpt_byte_rev` from `docs/pio_state_machines_design.md`
//! §10.3 on PIO0 SM1. 5-instruction program drives one byte per host
//! request:
//!
//! ```text
//! .side_set 1                  ; nAck pulses via side-set
//! .wrap_target
//!     pull block      side 1   ; get byte, nAck idle HIGH
//!     wait 0 pin 0    side 1   ; wait for nAutoFd LOW (host request)
//!     out pins, 8     side 1   ; drive D0..D7
//!     nop [4]         side 0   ; nAck LOW pulse (5 cycles)
//!     wait 1 pin 0    side 1   ; wait for host to release nAutoFd
//! .wrap
//! ```
//!
//! Pin map (per `docs/hardware_reference.md` §3.3):
//! - OUT_BASE = D0 (GP12), width 8 → GP12..GP19
//! - SIDE_BASE = nAck (GP23)
//! - IN_BASE = nAutoFd (GP20), width 1 (only used by the `wait` op)
//!
//! ## Direction control
//!
//! Byte mode flips D0..D7 to peripheral-drives via the 74LVC161284's
//! DIR pin (`docs/hardware_reference.md` §11.3). The chip uses
//! `DIR=H, HD=H` for byte reverse. `BytePhy::build` does not yet
//! drive those pins itself — the `LptMux::switch_to` path will gain
//! a pre-build hook for it as part of the Phase 5 integration.
//!
//! ## Clock
//!
//! PIO clock target: 50 MHz (20 ns/cycle). `nop [4]` = 5 cycles =
//! 100 ns nAck LOW pulse — well above the 750 ns IEEE 1284 byte-mode
//! minimum but compatible with our 1.7 MB/s wire-rate ceiling.

use embassy_rp::dma;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{
    Common, Config, Direction, FifoJoin, LoadedProgram, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_rp::gpio::Level;
use embassy_time::Timer;
use fixed::types::U24F8;
use pio::SideSet;

use super::hardware::LptPins;

const PIO_CLK_HZ: u32 = 50_000_000;

pub struct ByteRevProgram<'d> {
    prg: LoadedProgram<'d, PIO0>,
}

impl<'d> ByteRevProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO0>) -> Self {
        // bits=1, opt=false, pindirs=false — always-set side-set on nAck.
        let mut a: pio::Assembler<32> =
            pio::Assembler::new_with_side_set(SideSet::new(false, 1, false));
        let mut wrap_target = a.label();
        let mut wrap_source = a.label();
        a.bind(&mut wrap_target);
        // pull block (side nAck=1): wait for CPU byte.
        a.pull_with_side_set(false, true, 1);
        // wait 0 pin 0 (side nAck=1): wait for nAutoFd LOW.
        a.wait_with_side_set(0, pio::WaitSource::PIN, 0, false, 1);
        // out pins, 8 (side nAck=1): drive D0..D7.
        a.out_with_side_set(pio::OutDestination::PINS, 8, 1);
        // nop [4] (side nAck=0): 100 ns LOW pulse @ 50 MHz.
        a.nop_with_delay_and_side_set(4, 0);
        // wait 1 pin 0 (side nAck=1): host releases nAutoFd.
        a.wait_with_side_set(1, pio::WaitSource::PIN, 0, false, 1);
        a.bind(&mut wrap_source);
        let assembled = a.assemble_with_wrap(wrap_source, wrap_target);
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

pub struct PioByteRev {
    sm1: StateMachine<'static, PIO0, 1>,
    dma: dma::Channel<'static>,
    program: LoadedProgram<'static, PIO0>,
}

impl PioByteRev {
    pub fn new(
        common: &mut Common<'static, PIO0>,
        mut sm1: StateMachine<'static, PIO0, 1>,
        dma: dma::Channel<'static>,
        pins: &LptPins,
    ) -> Self {
        let data_pins = [
            &pins.d0, &pins.d1, &pins.d2, &pins.d3, &pins.d4, &pins.d5, &pins.d6, &pins.d7,
        ];
        let side_pins = [&pins.nack];

        // Idle nAck HIGH (released through the open-drain buffer) and
        // park data lines LOW before flipping pindirs to Out so the
        // wire never glitches.
        sm1.set_pins(Level::High, &side_pins);
        sm1.set_pins(Level::Low, &data_pins);
        sm1.set_pin_dirs(Direction::Out, &data_pins);
        sm1.set_pin_dirs(Direction::Out, &side_pins);

        let program = ByteRevProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &side_pins);
        cfg.set_out_pins(&data_pins);
        cfg.set_in_pins(&[&pins.auto_fd]);

        cfg.clock_divider =
            U24F8::from_num(embassy_rp::clocks::clk_sys_freq()) / U24F8::from_num(PIO_CLK_HZ);

        cfg.fifo_join = FifoJoin::TxOnly;
        cfg.shift_out = ShiftConfig {
            auto_fill: false,
            // 8 bits per byte; `out pins, 8` consumes the OSR's first byte.
            threshold: 8,
            direction: ShiftDirection::Right,
        };

        sm1.set_config(&cfg);
        sm1.set_enable(true);

        defmt::info!(
            "lpt byte-rev PIO armed: PIO0 SM1, OUT GP12..GP19, nAck GP23, {} MHz wire-rate",
            PIO_CLK_HZ / 1_000_000
        );

        Self {
            sm1,
            dma,
            program: program.prg,
        }
    }

    /// Send one byte. SM does a single pull/drive/handshake cycle.
    pub async fn send_byte(&mut self, byte: u8) {
        let word = [byte as u32];
        self.sm1.tx().dma_push(&mut self.dma, &word, false).await;
    }

    /// DMA-batched send. Each byte takes one TX FIFO entry; SM clocks
    /// them out at host pace.
    pub async fn send_bytes(&mut self, bytes: &[u8]) {
        const MAX_BATCH: usize = 256;
        assert!(bytes.len() <= MAX_BATCH, "byte-rev batch too large");
        let mut words = [0u32; MAX_BATCH];
        for (i, &b) in bytes.iter().enumerate() {
            words[i] = b as u32;
        }
        self.sm1
            .tx()
            .dma_push(&mut self.dma, &words[..bytes.len()], false)
            .await;
    }

    /// Tear down: drain TX, disable SM, free instr memory, return
    /// SM and DMA channel for re-parking on `LptHardware`.
    pub async fn dismantle(
        mut self,
        common: &mut Common<'static, PIO0>,
    ) -> (StateMachine<'static, PIO0, 1>, dma::Channel<'static>) {
        // FIFO depth (~8) × per-byte wire pace. Byte mode runs ~500
        // ns/byte at wire ceiling; pad to 5 ms for headroom against
        // a host that's pausing in mid-cycle.
        Timer::after_millis(5).await;
        self.sm1.set_enable(false);
        unsafe {
            common.free_instr(self.program.used_memory);
        }
        defmt::info!("lpt byte-rev dismantled");
        (self.sm1, self.dma)
    }
}
