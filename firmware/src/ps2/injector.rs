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
//! ## Phase 2 v3 scope (this revision)
//!
//! Types a real DOS DEBUG session that loads a Stage 0 placeholder
//! .COM-style program into memory and runs it:
//!
//! ```text
//! debug<CR>
//! e 100 b4 09 ba 0d 01 cd 21 b8<CR>     ; mov ah,9 / mov dx,010d / int 21 / mov ax (lo)
//! e 108 00 4c cd 21 cc 50 69 63<CR>     ; (4C ax) int 21 / int3 / "Pic"
//! e 110 6f 31 32 38 34 20 53 74<CR>     ; "o1284 St"
//! ...
//! g 100<CR>                              ; run from 0100h
//! q<CR>                                  ; quit DEBUG
//! ```
//!
//! Stage 0 prints "Pico1284 Stage 0 ok\r\n" via INT 21h AH=09 and
//! exits cleanly. On a connected AT/PS-2 host this is end-to-end
//! visible: type debug commands → DEBUG assembles + runs → banner
//! appears on screen.
//!
//! The real production Stage 0 (per `dos/stage0/s0_at.asm`) is far
//! larger and pivots to the LPT bootstrap channel; replacing this
//! placeholder is a separate task once the AT/PS-2 wire round-trip is
//! confirmed.
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

/// DEBUG entry-command chunk size: bytes per `e <addr> ...` line. 8
/// keeps each command well under DEBUG's ~80-char input limit.
const E_CHUNK: usize = 8;

/// Where DEBUG should load Stage 0. Standard .COM origin.
const STAGE0_ORIGIN: u16 = 0x0100;

/// Phase 2 v3 Stage 0 placeholder. Prints
/// "Pico1284 Stage 0 ok\r\n" via INT 21h AH=09 and exits via
/// INT 21h AH=4Ch AL=00. Layout (offsets from CS:0100h):
///
/// ```text
/// 0100  B4 09           MOV  AH, 09h
/// 0102  BA 0D 01        MOV  DX, 010Dh
/// 0105  CD 21           INT  21h
/// 0107  B8 00 4C        MOV  AX, 4C00h
/// 010A  CD 21           INT  21h
/// 010C  CC              INT3            ; never reached
/// 010D  "Pico1284 Stage 0 ok\r\n$"
/// ```
const STAGE0_PLACEHOLDER: &[u8] = &[
    0xB4, 0x09,             // mov ah, 09h
    0xBA, 0x0D, 0x01,       // mov dx, 010Dh
    0xCD, 0x21,             // int 21h
    0xB8, 0x00, 0x4C,       // mov ax, 4C00h
    0xCD, 0x21,             // int 21h
    0xCC,                   // int3
    // banner at 010Dh:
    b'P', b'i', b'c', b'o', b'1', b'2', b'8', b'4',
    b' ', b'S', b't', b'a', b'g', b'e', b' ', b'0',
    b' ', b'o', b'k', b'\r', b'\n', b'$',
];

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

    #[allow(dead_code)] // wired in once Set 1 ASCII table lands
    async fn type_scancode_xt(&mut self, scancode: u8) {
        // Make.
        self.kbd_tx.send_xt_byte(scancode).await;
        Timer::after_millis(KEYSTROKE_GAP_MS).await;
        // Break = make-code with bit 7 set.
        self.kbd_tx.send_xt_byte(scancode | 0x80).await;
        Timer::after_millis(KEYSTROKE_GAP_MS).await;
    }

    /// Type an ASCII string on an AT/PS-2 host by translating each
    /// character through the Set 2 scancode table. Unmapped bytes (e.g.,
    /// shifted punctuation that v2 doesn't cover) are skipped with a
    /// warning.
    async fn type_ascii_at(&mut self, ascii: &[u8]) {
        for &c in ascii {
            let code = scancode::ascii_to_set2(c);
            if code == 0 {
                defmt::warn!("injector: ASCII 0x{:02X} has no scancode mapping", c);
                continue;
            }
            self.type_scancode_at(code).await;
        }
    }

    /// Drive a full DEBUG session: launch DEBUG, enter the Stage 0
    /// bytes via `e` commands, run via `g`, then quit. Each line is
    /// followed by a short cool-down so DEBUG can echo and parse.
    async fn type_debug_stage0_at(&mut self) {
        self.type_ascii_at(b"debug\r").await;
        Timer::after_millis(DEBUG_STARTUP_MS).await;

        let mut addr = STAGE0_ORIGIN;
        for chunk in STAGE0_PLACEHOLDER.chunks(E_CHUNK) {
            // "e XXXX BB BB BB BB BB BB BB BB\r" → max ~32 chars.
            let mut cmd: String<40> = String::new();
            let _ = write!(cmd, "e {:x}", addr);
            for &b in chunk {
                let _ = write!(cmd, " {:02x}", b);
            }
            let _ = cmd.push('\r');
            self.type_ascii_at(cmd.as_bytes()).await;
            addr += chunk.len() as u16;
            Timer::after_millis(DEBUG_LINE_GAP_MS).await;
        }

        // Run Stage 0.
        let mut go: String<16> = String::new();
        let _ = write!(go, "g {:x}\r", STAGE0_ORIGIN);
        self.type_ascii_at(go.as_bytes()).await;
        Timer::after_millis(DEBUG_STARTUP_MS).await;

        // Quit DEBUG; control returns to COMMAND.COM.
        self.type_ascii_at(b"q\r").await;
    }

    async fn type_script(&mut self, class: MachineClass) {
        match class {
            MachineClass::At | MachineClass::Ps2 => self.type_debug_stage0_at().await,
            MachineClass::Xt => {
                // Set 1 scancode table is a follow-up. For now log and
                // skip so we don't drive nonsense onto the XT wire.
                defmt::warn!(
                    "injector: XT class not yet supported by the DEBUG typer; \
                     {} bytes of Stage 0 skipped",
                    STAGE0_PLACEHOLDER.len()
                );
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

        me.type_script(class).await;

        defmt::info!("injector: bootstrap script complete");
        lifecycle::set(SupervisorState::ServeStage0Download);
    }
}
