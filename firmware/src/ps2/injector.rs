//! PS/2 keyboard bootstrap injector.
//!
//! Waits on [`super::supervisor::INJECT_TRIGGER`] for the supervisor's
//! `Detected(class)` signal, then types a DEBUG-script byte sequence
//! into the host's keyboard port via [`super::tx::KbdTx`]. The DOS
//! BIOS interprets the keystrokes, runs DEBUG, and ultimately writes
//! Stage 0 machine code into memory.
//!
//! ## Frame format per host class
//!
//! - **AT / PS-2** (Set 2): make = `[scancode]`, break = `[0xF0, scancode]`.
//! - **XT** (Set 1): make = `[scancode]`, break = `[scancode | 0x80]`.
//!
//! Each scancode is sent at ~16.7 kHz wire rate; we insert a small
//! inter-keystroke delay so the host's keyboard ISR has time to consume
//! and forward each event to the BIOS keyboard buffer (`0040:001E`).
//!
//! ## Phase 2 v1 scope
//!
//! This commit wires the supervisor → injector → KbdTx pipeline and
//! types a 3-byte placeholder script ("ABC"). The real Stage 0 DEBUG
//! script is a follow-up; for now this proves the injector + lifecycle
//! transitions work end-to-end and the wire output is visible to the
//! oversampler's loopback.
//!
//! ## Self-classification caveat
//!
//! Our own TX is visible to the KBD oversampler because GP2 (CLK_IN)
//! and GP4 (DATA_IN) sit downstream of the open-drain buffer driven by
//! GP3 (CLK_PULL) and GP5 (DATA_PULL). When the injector types frames
//! the classifier sees them and may flip to `Confirmed(At)` based on
//! our own output. Mitigation lives in a follow-up: either mask
//! classifier ingest during `InjectDebug`, or compare frame source
//! using the CLK_PULL register that the 3-bit oversampler already
//! captures.

use embassy_time::Timer;
use vintage_kvm_signatures::MachineClass;

use super::supervisor::INJECT_TRIGGER;
use super::tx::KbdTx;
use crate::lifecycle::{self, SupervisorState};

/// Inter-keystroke pacing. 5 ms is well above the keyboard ISR's
/// per-event latency on every host class we care about.
const KEYSTROKE_GAP_MS: u64 = 5;

/// Placeholder script for Phase 2 v1: scancodes 'A', 'B', 'C' in Set
/// 2 / Set 1 (they happen to be the same low byte across the two
/// scancode sets for these letters: 0x1C, 0x32, 0x21).
const PLACEHOLDER_SCRIPT: &[u8] = &[0x1C, 0x32, 0x21];

pub struct BootstrapInjector {
    kbd_tx: KbdTx,
}

impl BootstrapInjector {
    pub fn new(kbd_tx: KbdTx) -> Self {
        Self { kbd_tx }
    }

    async fn type_scancode_at(&mut self, scancode: u8) {
        // Make.
        self.kbd_tx.send_at_byte(scancode).await;
        Timer::after_millis(KEYSTROKE_GAP_MS).await;
        // Break = 0xF0 prefix + same scancode.
        self.kbd_tx.send_at_byte(0xF0).await;
        Timer::after_millis(KEYSTROKE_GAP_MS).await;
        self.kbd_tx.send_at_byte(scancode).await;
        Timer::after_millis(KEYSTROKE_GAP_MS).await;
    }

    async fn type_scancode_xt(&mut self, scancode: u8) {
        // Make.
        self.kbd_tx.send_xt_byte(scancode).await;
        Timer::after_millis(KEYSTROKE_GAP_MS).await;
        // Break = make-code with bit 7 set.
        self.kbd_tx.send_xt_byte(scancode | 0x80).await;
        Timer::after_millis(KEYSTROKE_GAP_MS).await;
    }

    async fn type_script(&mut self, script: &[u8], class: MachineClass) {
        for &code in script {
            match class {
                MachineClass::Xt => self.type_scancode_xt(code).await,
                MachineClass::At | MachineClass::Ps2 => self.type_scancode_at(code).await,
            }
        }
    }
}

#[embassy_executor::task]
pub async fn run(mut me: BootstrapInjector) {
    loop {
        let class = INJECT_TRIGGER.wait().await;
        INJECT_TRIGGER.reset();

        defmt::info!("injector: starting bootstrap for {}", class);
        lifecycle::set(SupervisorState::InjectDebug);

        me.type_script(PLACEHOLDER_SCRIPT, class).await;

        defmt::info!("injector: placeholder script complete");
        lifecycle::set(SupervisorState::ServeStage0Download);
    }
}
