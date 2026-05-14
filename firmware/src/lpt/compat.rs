//! SPP-compat forward (PIO) + nibble-reverse bit-bang LPT phy.
//!
//! Mirrors the wire protocol of `dos/stage0/lpt_nibble.inc` byte-for-byte
//! from the peripheral side. The forward (host → Pico) path is driven by
//! the PIO `lpt_compat_in` program in [`super::pio_compat_in`]; the
//! reverse (Pico → host) nibble path stays bit-bang for now — PIO
//! `lpt_nibble_out` lands in a follow-up.
//!
//! ## Wire protocol
//!
//! ### Forward (DOS → Pico)
//!
//! 1. DOS writes byte to `LPT_DATA` (drives D0-D7).
//! 2. DOS pulses the host-strobe line low then high.
//! 3. Pico observes the falling edge, samples D0-D7, queues the byte.
//!
//! ### Reverse (Pico → DOS)
//!
//! Sends one byte as two nibbles, low nibble first:
//!
//! 1. Present nibble bits on status[3..=6] (LSB at bit 3).
//! 2. Toggle status bit 7 (phase) — DOS polls until phase changes.
//! 3. Hold ~100 µs so DOS's debounce-and-sample sees a stable nibble.
//! 4. Repeat for the high nibble.
//!
//! ### Persistent phase invariant
//!
//! Both sides track `last_phase`. The Pico flips phase **only on a new
//! nibble** so the host's `lpt_recv_nibble` (which waits for `phase !=
//! last_phase` then commits the new phase) reliably distinguishes "new
//! data" from "stale wire."
//!
//! ## Status-bit → GPIO mapping
//!
//! IEEE 1284 status register bits 3..=7 map to GPIOs per
//! `docs/hardware_reference.md` §3.3:
//!
//! | Status bit | Signal name | GPIO | Role in nibble mode |
//! |------------|-------------|------|---------------------|
//! | 3          | nFault      | GP27 | nibble bit 0 (LSB)  |
//! | 4          | Select      | GP26 | nibble bit 1        |
//! | 5          | PError      | GP25 | nibble bit 2        |
//! | 6          | nAck        | GP23 | nibble bit 3 (MSB)  |
//! | 7          | Busy        | GP24 | phase toggle        |
//!
//! ## Open hardware question: host-strobe pin
//!
//! DOS Stage 0/1 pulse **nInit** (LPT control register bit 2) as the host
//! strobe in nibble mode. The hardware-ref table does not enumerate which
//! Pico GPIO carries nInit through the 74LVC161284 — only nStrobe (GP11),
//! nAutoFd (GP20), and nSelectIn (GP22) are listed. For Phase 3 MVP we
//! assume the strobe lands on `HOST_STROBE_PIN` and flag this as a
//! bring-up TODO: confirm with a logic analyzer which control line goes
//! low on the DOS `OUT dx, al` for `CTRL_INIT`. If it's not GP11, change
//! the constant below.

use defmt::trace;
use embassy_rp::gpio::{Level, Output};
use embassy_time::Timer;

use super::pio_compat_in::PioCompatIn;
use super::{LptError, LptMode, LptPhy};

/// Nibble-output pin (status[3] = nFault, LSB of nibble).
pub type NibbleBit0 = Output<'static>;
/// Nibble-output pin (status[4] = Select).
pub type NibbleBit1 = Output<'static>;
/// Nibble-output pin (status[5] = PError).
pub type NibbleBit2 = Output<'static>;
/// Nibble-output pin (status[6] = nAck, MSB of nibble).
pub type NibbleBit3 = Output<'static>;
/// Phase-toggle pin (status[7] = Busy).
pub type PhasePin = Output<'static>;

/// Time the Pico holds a nibble + phase stable so DOS's polling /
/// debounce loop has a clean window to sample. 100 µs is a generous
/// envelope vs. DOS's ~80 µs polling cadence (`TIMEOUT_INNER = 0x10`).
const NIBBLE_SETTLE_US: u64 = 100;

pub struct SppNibblePhy {
    /// PIO-driven forward path (host strobe + D0..D7). Owns PIO0 SM0.
    compat_in: PioCompatIn,

    nibble_bits: (NibbleBit0, NibbleBit1, NibbleBit2, NibbleBit3),
    phase: PhasePin,

    /// Persistent phase state; flipped on every emitted nibble.
    last_phase: bool,
}

impl SppNibblePhy {
    pub fn new(
        compat_in: PioCompatIn,
        nibble_bit0: NibbleBit0,
        nibble_bit1: NibbleBit1,
        nibble_bit2: NibbleBit2,
        nibble_bit3: NibbleBit3,
        phase: PhasePin,
    ) -> Self {
        let mut me = Self {
            compat_in,
            nibble_bits: (nibble_bit0, nibble_bit1, nibble_bit2, nibble_bit3),
            phase,
            last_phase: false,
        };
        // Idle: drive a known phase + zeroed nibble so DOS's
        // `init_lpt_control` captures a stable starting state.
        me.drive_nibble(0);
        me.phase.set_level(Level::Low);
        me.last_phase = false;
        me
    }

    /// Wait for the next host-strobe falling edge, then return the byte
    /// captured by the PIO state machine.
    pub async fn recv_byte(&mut self) -> Result<u8, LptError> {
        let byte = self.compat_in.recv_byte().await;
        trace!("LPT recv 0x{:02X}", byte);
        Ok(byte)
    }

    /// Send one byte as two nibbles (low first), with phase toggles.
    pub async fn send_byte(&mut self, byte: u8) -> Result<(), LptError> {
        let low = byte & 0x0F;
        let high = (byte >> 4) & 0x0F;

        self.present_nibble(low);
        Timer::after_micros(NIBBLE_SETTLE_US).await;

        self.present_nibble(high);
        Timer::after_micros(NIBBLE_SETTLE_US).await;

        trace!("LPT send 0x{:02X}", byte);
        Ok(())
    }

    /// Drive the nibble pins and toggle phase. Order matters: nibble bits
    /// first so the value is stable before DOS notices the phase change.
    fn present_nibble(&mut self, nibble: u8) {
        self.drive_nibble(nibble);
        self.last_phase = !self.last_phase;
        self.phase.set_level(if self.last_phase {
            Level::High
        } else {
            Level::Low
        });
    }

    fn drive_nibble(&mut self, nibble: u8) {
        self.nibble_bits.0.set_level(level_from_bit(nibble, 0));
        self.nibble_bits.1.set_level(level_from_bit(nibble, 1));
        self.nibble_bits.2.set_level(level_from_bit(nibble, 2));
        self.nibble_bits.3.set_level(level_from_bit(nibble, 3));
    }
}

impl LptPhy for SppNibblePhy {
    async fn recv_byte(&mut self) -> Result<u8, LptError> {
        SppNibblePhy::recv_byte(self).await
    }

    async fn send_byte(&mut self, b: u8) -> Result<(), LptError> {
        SppNibblePhy::send_byte(self, b).await
    }

    fn current_mode(&self) -> LptMode {
        LptMode::SppNibble
    }
}

#[inline]
fn level_from_bit(value: u8, bit: u8) -> Level {
    if value & (1 << bit) != 0 {
        Level::High
    } else {
        Level::Low
    }
}
