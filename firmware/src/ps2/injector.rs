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
//! ## Phase 2 v5 scope (this revision)
//!
//! Types a real DOS DEBUG session that loads the production Stage 0
//! binary matched to the detected host class:
//!
//!   - AT / PS-2 → `S0_AT.COM` (1635 bytes, Set 2 scancodes).
//!   - XT        → `S0_XT.COM` (1082 bytes, Set 1 scancodes).
//!
//! Both binaries are NASM-assembled from `dos/stage0/` by `build.rs`.
//!
//! ```text
//! debug<CR>
//! e 100 ...16 bytes...<CR>               ; many such lines
//! ...
//! g 100<CR>                              ; run from 0100h
//! q<CR>                                  ; quit DEBUG
//! ```
//!
//! Once Stage 0 takes over it owns LPT + the i8042 keyboard private
//! channel, sends a CAP_REQ to the Pico, and bootstraps Stage 1 over
//! the LPT data plane.
//!
//! ## Build dependency
//!
//! `build.rs` NASM-assembles `dos/stage0/s0_at.asm` into the cargo
//! `OUT_DIR` at firmware compile time; `include_bytes!` pulls the
//! result. NASM must be on `PATH`. Source changes to the .asm or its
//! includes (`s0_atps2_core.inc`, `lpt_nibble.inc`) automatically
//! invalidate the firmware build via `cargo:rerun-if-changed`.
//!
//! ## Bootstrap duration
//!
//! At 1635 bytes / 16 bytes per `e` line / 5 ms per scancode pair
//! event / 50 ms per-line cool-down, the full type-out takes ~90
//! seconds. This is a one-time setup cost; once Stage 0 hands off to
//! Stage 1 over LPT, the system runs at full wire rate.
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

use core::fmt::Write;

use embassy_time::Timer;
use heapless::String;
use vintage_kvm_signatures::MachineClass;

use super::scancode;
use super::supervisor::INJECT_TRIGGER;
use super::tx::KbdTx;
use crate::lifecycle::{self, SupervisorState};

/// Inter-keystroke pacing. 5 ms is well above the keyboard ISR's
/// per-event latency on every host class we care about.
const KEYSTROKE_GAP_MS: u64 = 5;

/// Cool-down between DEBUG command lines. Lets DEBUG parse + echo the
/// previous line before we slam the keyboard buffer with the next one.
const DEBUG_LINE_GAP_MS: u64 = 50;

/// Pause after launching DEBUG and after the `g` command, so DOS has
/// time to load DEBUG and Stage 0 has time to finish before we send
/// the `q` quit.
const DEBUG_STARTUP_MS: u64 = 500;

/// DEBUG entry-command chunk size: bytes per `e <addr> ...` line. 16
/// gives a max line length of `"e ffff bb×16\r"` ≈ 55 chars, well
/// under DEBUG's ~80-char input limit.
const E_CHUNK: usize = 16;

/// Where DEBUG should load Stage 0. Standard .COM origin.
const STAGE0_ORIGIN: u16 = 0x0100;

/// Production AT-class Stage 0 binary, NASM-assembled from
/// `dos/stage0/s0_at.asm` by the firmware's `build.rs`. The .asm is
/// the canonical artifact (committed to git); the .COM/.bin output
/// lives in `OUT_DIR` and is rebuilt automatically when the source
/// changes.
const STAGE0_AT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/s0_at.bin"));

/// Production XT-class Stage 0 binary. NASM-built from
/// `dos/stage0/s0_xt.asm`. Smaller than the AT variant (no i8042
/// mastery — XT keyboards are hardware-unidirectional, so the
/// keyboard private channel of `s0_at.asm` doesn't apply).
const STAGE0_XT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/s0_xt.bin"));

/// Rough wall-clock estimate for typing N bytes of Stage 0 binary via
/// DEBUG `e` commands at our current pacing. Used only for an
/// informational defmt log so the operator knows what to expect.
const fn estimated_seconds(bin_len: usize) -> u32 {
    // Per chunk: "e XXXX" (~6 chars) + 16 × " bb" (48 chars) + "\r"
    // = ~55 chars = 55 × 3 scancode events = 165 events × 5 ms = 825 ms,
    // plus 50 ms line gap = 875 ms per chunk. Round to a flat 900 ms.
    let chunks = (bin_len + E_CHUNK - 1) / E_CHUNK;
    (chunks as u32 * 900 + DEBUG_STARTUP_MS as u32) / 1000
}

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

    /// Type an ASCII string on an AT/PS-2 host by translating each
    /// character through the Set 2 scancode table. Unmapped bytes are
    /// skipped with a warning.
    async fn type_ascii_at(&mut self, ascii: &[u8]) {
        for &c in ascii {
            let code = scancode::ascii_to_set2(c);
            if code == 0 {
                defmt::warn!("injector: ASCII 0x{:02X} has no Set 2 mapping", c);
                continue;
            }
            self.type_scancode_at(code).await;
        }
    }

    /// Type an ASCII string on an XT host by translating each character
    /// through the Set 1 scancode table. Unmapped bytes are skipped
    /// with a warning.
    async fn type_ascii_xt(&mut self, ascii: &[u8]) {
        for &c in ascii {
            let code = scancode::ascii_to_set1(c);
            if code == 0 {
                defmt::warn!("injector: ASCII 0x{:02X} has no Set 1 mapping", c);
                continue;
            }
            self.type_scancode_xt(code).await;
        }
    }

    /// Drive a full DEBUG session: launch DEBUG, enter the Stage 0
    /// bytes via `e` commands, then `g 100` to hand control over. Each
    /// line gets a short cool-down so DEBUG can echo and parse. Once
    /// Stage 0 runs it owns the host — we don't bother typing `q` to
    /// quit DEBUG; Stage 0 won't return to it on the production path.
    ///
    /// `class` selects the scancode table: AT/PS-2 use Set 2, XT uses
    /// Set 1. `stage0_bin` is whichever production .COM is matched to
    /// the host class.
    async fn type_debug_stage0(&mut self, class: MachineClass, stage0_bin: &[u8]) {
        defmt::info!(
            "injector: typing Stage 0 ({} bytes) via DEBUG — ~{} sec",
            stage0_bin.len(),
            estimated_seconds(stage0_bin.len())
        );

        self.type_ascii(class, b"debug\r").await;
        Timer::after_millis(DEBUG_STARTUP_MS).await;

        let mut addr = STAGE0_ORIGIN;
        for chunk in stage0_bin.chunks(E_CHUNK) {
            // "e XXXX BB×16\r" → max ~55 chars.
            let mut cmd: String<64> = String::new();
            let _ = write!(cmd, "e {:x}", addr);
            for &b in chunk {
                let _ = write!(cmd, " {:02x}", b);
            }
            let _ = cmd.push('\r');
            self.type_ascii(class, cmd.as_bytes()).await;
            addr += chunk.len() as u16;
            Timer::after_millis(DEBUG_LINE_GAP_MS).await;
        }

        // Run Stage 0. On AT/PS-2 it takes over LPT + i8042 and starts
        // the CAP_REQ handshake; on XT it owns LPT only (no i8042
        // mastery possible). Control does not return to DEBUG.
        let mut go: String<16> = String::new();
        let _ = write!(go, "g {:x}\r", STAGE0_ORIGIN);
        self.type_ascii(class, go.as_bytes()).await;
    }

    /// Class-dispatched ASCII typer used by the DEBUG driver.
    async fn type_ascii(&mut self, class: MachineClass, ascii: &[u8]) {
        match class {
            MachineClass::At | MachineClass::Ps2 => self.type_ascii_at(ascii).await,
            MachineClass::Xt => self.type_ascii_xt(ascii).await,
        }
    }

    async fn type_script(&mut self, class: MachineClass) {
        let bin = match class {
            MachineClass::At | MachineClass::Ps2 => STAGE0_AT_BIN,
            MachineClass::Xt => STAGE0_XT_BIN,
        };
        self.type_debug_stage0(class, bin).await;
    }
}

#[embassy_executor::task]
pub async fn run(mut me: BootstrapInjector) {
    loop {
        let class = INJECT_TRIGGER.wait().await;
        INJECT_TRIGGER.reset();

        defmt::info!("injector: starting bootstrap for {}", class);
        lifecycle::set(SupervisorState::InjectDebug);

        me.type_script(class).await;

        defmt::info!("injector: bootstrap script complete");
        lifecycle::set(SupervisorState::ServeStage0Download);
    }
}
