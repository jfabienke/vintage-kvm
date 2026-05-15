//! IEEE 1284 ECP forward + reverse, PIO-driven.
//!
//! Implements `lpt_ecp_fwd_dma` and `lpt_ecp_rev_dma` from
//! `docs/pio_state_machines_design.md` §10.5. Two separate state
//! machines run the two directions; the negotiated mode locks one
//! direction at a time, but having both programs preloaded lets the
//! direction-change phase complete without an additional load.
//!
//! ## ECP forward (host → Pico)
//!
//! ```text
//! .side_set 1                       ; PeriphAck via side-set
//! .wrap_target
//!     wait 0 pin 0      side 1      ; HostClk LOW
//!     in pins, 9        side 1      ; sample HostClk + D0..D7
//!     nop [2]           side 0      ; PeriphAck LOW pulse
//!     push              side 0
//!     wait 1 pin 0      side 1      ; HostClk HIGH (host releases)
//! .wrap
//! ```
//!
//! - IN_BASE = HostClk (GP11), width 9 → GP11..GP19
//! - SIDE_BASE = PeriphAck (GP24) – note this is the same GPIO as
//!   nWait in EPP; the 74LVC161284's mode pins re-purpose it.
//!
//! HostAck (cmd/data flag) is NOT captured in the same FIFO word — it
//! lives on GP20 (HostAck = nAutoFd in ECP). The CPU reads GP20
//! directly via GPIO when consuming each FIFO entry. This is Option A
//! from §10.5; the FIFO carries the 9-bit (strobe + data) payload
//! only.
//!
//! ## ECP reverse (Pico → host)
//!
//! ```text
//! .side_set 1                       ; PeriphClk via side-set
//! .wrap_target
//!     pull block        side 1      ; next byte
//!     out pins, 8       side 1      ; drive D0..D7
//!     wait 0 gpio 20    side 1      ; host HostAck LOW (host ready)
//!     nop [2]           side 0      ; PeriphClk LOW pulse
//!     wait 1 gpio 20    side 1      ; host releases HostAck
//! .wrap
//! ```
//!
//! - OUT_BASE = D0 (GP12), width 8
//! - SIDE_BASE = PeriphClk (GP23)
//!
//! Both SMs run at 30 MHz PIO clock — about 33 ns/cycle, well over
//! 5× the ECP wire's 200 ns/byte ceiling, so neither direction is
//! PIO-bound.
//!
//! ## Direction control
//!
//! ECP's data-bus direction is set by the 74LVC161284's DIR pin
//! (`docs/hardware_reference.md` §11.3). Unlike EPP, ECP holds a
//! direction stable for an entire burst — so CPU-driven DIR via
//! `LptMux::switch_to(Ecp)` is sufficient (`L` for forward bursts,
//! `H` for reverse bursts). The pre-build hook on `LptMux` will
//! drive GP29 (DIR) and GP0 (HD=H, totem-pole) before building this
//! phy.

use embassy_rp::dma;
use embassy_rp::gpio::Level;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{
    Common, Config, Direction, FifoJoin, LoadedProgram, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_time::Timer;
use fixed::types::U24F8;
use pio::SideSet;

use super::hardware::LptPins;

const PIO_CLK_HZ: u32 = 30_000_000;

/// Absolute GPIO for HostAck in ECP reverse handshake (GP20).
const HOSTACK_GPIO: u8 = 20;

// ---------------------------- forward ----------------------------

pub struct EcpFwdProgram<'d> {
    prg: LoadedProgram<'d, PIO0>,
}

impl<'d> EcpFwdProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO0>) -> Self {
        let mut a: pio::Assembler<32> =
            pio::Assembler::new_with_side_set(SideSet::new(false, 1, false));
        let mut wrap_target = a.label();
        let mut wrap_source = a.label();
        a.bind(&mut wrap_target);
        // wait 0 pin 0 (PeriphAck=1): HostClk LOW.
        a.wait_with_side_set(0, pio::WaitSource::PIN, 0, false, 1);
        // in pins, 9 (PeriphAck=1): sample strobe + D0..D7.
        a.r#in_with_side_set(pio::InSource::PINS, 9, 1);
        // nop [2] (PeriphAck=0): LOW pulse.
        a.nop_with_delay_and_side_set(2, 0);
        // push (PeriphAck=0): deliver to RX FIFO.
        a.push_with_side_set(false, true, 0);
        // wait 1 pin 0 (PeriphAck=1): HostClk HIGH.
        a.wait_with_side_set(1, pio::WaitSource::PIN, 0, false, 1);
        a.bind(&mut wrap_source);
        let assembled = a.assemble_with_wrap(wrap_source, wrap_target);
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

pub struct PioEcpFwd {
    sm0: StateMachine<'static, PIO0, 0>,
    program: LoadedProgram<'static, PIO0>,
}

impl PioEcpFwd {
    pub fn new(
        common: &mut Common<'static, PIO0>,
        mut sm0: StateMachine<'static, PIO0, 0>,
        pins: &LptPins,
    ) -> Self {
        let in_pins = [
            &pins.strobe, &pins.d0, &pins.d1, &pins.d2, &pins.d3, &pins.d4, &pins.d5, &pins.d6,
            &pins.d7,
        ];
        let side_pins = [&pins.busy]; // GP24 = PeriphAck in ECP forward

        sm0.set_pins(Level::High, &side_pins);
        sm0.set_pin_dirs(Direction::Out, &side_pins);

        let program = EcpFwdProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &side_pins);
        cfg.set_in_pins(&in_pins);

        cfg.clock_divider =
            U24F8::from_num(embassy_rp::clocks::clk_sys_freq()) / U24F8::from_num(PIO_CLK_HZ);

        cfg.fifo_join = FifoJoin::RxOnly;
        cfg.shift_in = ShiftConfig {
            auto_fill: false,
            threshold: 9,
            // Left = strobe in bit 0, D0..D7 in bits 1..8 of the FIFO
            // word (mirrors lpt_compat_in's layout).
            direction: ShiftDirection::Left,
        };

        sm0.set_config(&cfg);
        sm0.set_enable(true);

        defmt::info!(
            "lpt ecp-fwd PIO armed: PIO0 SM0, IN GP11..GP19, PeriphAck GP24, {} MHz",
            PIO_CLK_HZ / 1_000_000
        );

        Self {
            sm0,
            program: program.prg,
        }
    }

    /// Block until the next host-clocked byte arrives. Returns the
    /// 9-bit (strobe, D0..D7) word as captured; bit 0 is the strobe
    /// (always 0 at sample time), bits 1..8 are the data byte.
    pub async fn recv_byte(&mut self) -> u8 {
        let word = self.sm0.rx().wait_pull().await;
        ((word >> 1) & 0xFF) as u8
    }

    pub fn dismantle(
        mut self,
        common: &mut Common<'static, PIO0>,
    ) -> StateMachine<'static, PIO0, 0> {
        self.sm0.set_enable(false);
        unsafe {
            common.free_instr(self.program.used_memory);
        }
        defmt::info!("lpt ecp-fwd dismantled");
        self.sm0
    }
}

// ---------------------------- reverse ----------------------------

pub struct EcpRevProgram<'d> {
    prg: LoadedProgram<'d, PIO0>,
}

impl<'d> EcpRevProgram<'d> {
    pub fn new(common: &mut Common<'d, PIO0>) -> Self {
        let mut a: pio::Assembler<32> =
            pio::Assembler::new_with_side_set(SideSet::new(false, 1, false));
        let mut wrap_target = a.label();
        let mut wrap_source = a.label();
        a.bind(&mut wrap_target);
        // pull block (PeriphClk=1): get byte.
        a.pull_with_side_set(false, true, 1);
        // out pins, 8 (PeriphClk=1): drive D0..D7.
        a.out_with_side_set(pio::OutDestination::PINS, 8, 1);
        // wait 0 gpio 20 (PeriphClk=1): HostAck LOW (host ready).
        a.wait_with_side_set(0, pio::WaitSource::GPIO, HOSTACK_GPIO, false, 1);
        // nop [2] (PeriphClk=0): LOW pulse → host samples here.
        a.nop_with_delay_and_side_set(2, 0);
        // wait 1 gpio 20 (PeriphClk=1): host releases HostAck.
        a.wait_with_side_set(1, pio::WaitSource::GPIO, HOSTACK_GPIO, false, 1);
        a.bind(&mut wrap_source);
        let assembled = a.assemble_with_wrap(wrap_source, wrap_target);
        let prg = common.load_program(&assembled);
        Self { prg }
    }
}

pub struct PioEcpRev {
    sm1: StateMachine<'static, PIO0, 1>,
    dma: dma::Channel<'static>,
    program: LoadedProgram<'static, PIO0>,
}

impl PioEcpRev {
    pub fn new(
        common: &mut Common<'static, PIO0>,
        mut sm1: StateMachine<'static, PIO0, 1>,
        dma: dma::Channel<'static>,
        pins: &LptPins,
    ) -> Self {
        let data_pins = [
            &pins.d0, &pins.d1, &pins.d2, &pins.d3, &pins.d4, &pins.d5, &pins.d6, &pins.d7,
        ];
        let side_pins = [&pins.nack]; // GP23 = PeriphClk in ECP reverse

        sm1.set_pins(Level::High, &side_pins);
        sm1.set_pins(Level::Low, &data_pins);
        sm1.set_pin_dirs(Direction::Out, &data_pins);
        sm1.set_pin_dirs(Direction::Out, &side_pins);

        let program = EcpRevProgram::new(common);

        let mut cfg = Config::default();
        cfg.use_program(&program.prg, &side_pins);
        cfg.set_out_pins(&data_pins);

        cfg.clock_divider =
            U24F8::from_num(embassy_rp::clocks::clk_sys_freq()) / U24F8::from_num(PIO_CLK_HZ);

        cfg.fifo_join = FifoJoin::TxOnly;
        cfg.shift_out = ShiftConfig {
            auto_fill: false,
            threshold: 8,
            direction: ShiftDirection::Right,
        };

        sm1.set_config(&cfg);
        sm1.set_enable(true);

        defmt::info!(
            "lpt ecp-rev PIO armed: PIO0 SM1, OUT GP12..GP19, PeriphClk GP23, {} MHz",
            PIO_CLK_HZ / 1_000_000
        );

        Self {
            sm1,
            dma,
            program: program.prg,
        }
    }

    pub async fn send_byte(&mut self, byte: u8) {
        let word = [byte as u32];
        self.sm1.tx().dma_push(&mut self.dma, &word, false).await;
    }

    pub async fn send_bytes(&mut self, bytes: &[u8]) {
        const MAX_BATCH: usize = 256;
        assert!(bytes.len() <= MAX_BATCH, "ecp-rev batch too large");
        let mut words = [0u32; MAX_BATCH];
        for (i, &b) in bytes.iter().enumerate() {
            words[i] = b as u32;
        }
        self.sm1
            .tx()
            .dma_push(&mut self.dma, &words[..bytes.len()], false)
            .await;
    }

    pub async fn dismantle(
        mut self,
        common: &mut Common<'static, PIO0>,
    ) -> (StateMachine<'static, PIO0, 1>, dma::Channel<'static>) {
        Timer::after_millis(5).await;
        self.sm1.set_enable(false);
        unsafe {
            common.free_instr(self.program.used_memory);
        }
        defmt::info!("lpt ecp-rev dismantled");
        (self.sm1, self.dma)
    }
}
