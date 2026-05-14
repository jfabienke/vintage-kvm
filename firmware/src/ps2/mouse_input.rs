//! PS/2 mouse (AUX channel) input from the USB control RPC.
//!
//! Owns `AuxTx`. Consumes [`MOUSE_CMD`] from the USB control task:
//! relative move events get packed into 3-byte PS/2 mouse packets and
//! emitted via `AuxTx::send_at_byte`; button state is sticky and
//! tracked here.
//!
//! ## Packet format (PS/2 standard 3-byte)
//!
//! ```text
//! byte 0: [ Yov | Xov | Ysign | Xsign | 1 | midBtn | rightBtn | leftBtn ]
//! byte 1: X movement (low 8 bits; 9th bit lives in Xsign)
//! byte 2: Y movement (low 8 bits; 9th bit lives in Ysign)
//! ```
//!
//! Movement range is 9-bit signed (-256..=255); the injector clamps
//! larger control-supplied deltas to that range. Overflow bits stay
//! 0 because we always clamp.

use core::sync::atomic::{AtomicU8, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::Timer;

use super::tx::AuxTx;

/// 3-byte packet × small inter-byte gap. 5 ms matches the keyboard
/// path's pacing, well above the AUX ISR's drain rate.
const BYTE_GAP_MS: u64 = 5;

#[derive(Debug, Clone, Copy, defmt::Format)]
pub enum MouseBtn {
    Left,
    Right,
    Middle,
}

impl MouseBtn {
    fn mask(self) -> u8 {
        match self {
            MouseBtn::Left => 0x01,
            MouseBtn::Right => 0x02,
            MouseBtn::Middle => 0x04,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MouseCmd {
    Move { dx: i16, dy: i16 },
    Button { btn: MouseBtn, down: bool },
}

/// Inbox for mouse events from the USB control task. Depth 8 absorbs
/// burst typing of move commands; the AUX wire is fast enough that
/// drains keep up under normal operator interaction.
pub static MOUSE_CMD: Channel<CriticalSectionRawMutex, MouseCmd, 8> = Channel::new();

/// Sticky button mask. Bit 0 = left, 1 = right, 2 = middle. Move
/// commands ship with the current mask so the host sees button-held +
/// movement as a single event.
static BUTTONS: AtomicU8 = AtomicU8::new(0);

pub struct MouseInjector {
    aux_tx: AuxTx,
}

impl MouseInjector {
    pub fn new(aux_tx: AuxTx) -> Self {
        Self { aux_tx }
    }

    async fn send_packet(&mut self, dx: i16, dy: i16, buttons: u8) {
        let pkt = pack_packet(dx, dy, buttons);
        for &b in &pkt {
            self.aux_tx.send_at_byte(b).await;
            Timer::after_millis(BYTE_GAP_MS).await;
        }
    }
}

fn pack_packet(dx: i16, dy: i16, buttons: u8) -> [u8; 3] {
    let dx9 = dx.clamp(-256, 255);
    let dy9 = dy.clamp(-256, 255);
    let x_sign = ((dx9 >> 8) & 1) as u8;
    let y_sign = ((dy9 >> 8) & 1) as u8;
    let byte0 = 0x08                  // bit 3 always set
        | (buttons & 0x07)            // L/R/M buttons
        | (x_sign << 4)               // X sign
        | (y_sign << 5); // Y sign
    let byte1 = (dx9 & 0xFF) as u8;
    let byte2 = (dy9 & 0xFF) as u8;
    [byte0, byte1, byte2]
}

#[embassy_executor::task]
pub async fn run(mut me: MouseInjector) -> ! {
    defmt::info!("ps2 mouse_input: running");
    loop {
        let cmd = MOUSE_CMD.receive().await;
        match cmd {
            MouseCmd::Move { dx, dy } => {
                let buttons = BUTTONS.load(Ordering::Relaxed);
                me.send_packet(dx, dy, buttons).await;
            }
            MouseCmd::Button { btn, down } => {
                let mask = btn.mask();
                let new = if down {
                    BUTTONS.fetch_or(mask, Ordering::Relaxed) | mask
                } else {
                    BUTTONS.fetch_and(!mask, Ordering::Relaxed) & !mask
                };
                me.send_packet(0, 0, new).await;
            }
        }
    }
}
