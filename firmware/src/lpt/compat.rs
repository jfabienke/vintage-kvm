//! SPP-compat forward + nibble-reverse LPT phy, fully PIO-driven.
//!
//! Mirrors the wire protocol of `dos/stage0/lpt_nibble.inc` byte-for-byte
//! from the peripheral side. Both directions now run on PIO0:
//!   * forward (host → Pico)  → SM0 / `lpt_compat_in` ([`super::pio_compat_in`]);
//!   * reverse (Pico → host)  → SM1 / `lpt_nibble_out` ([`super::pio_nibble_out`]).
//!
//! ## Wire protocol
//!
//! ### Forward (DOS → Pico)
//!
//! 1. DOS writes byte to `LPT_DATA` (drives D0-D7).
//! 2. DOS pulses the host-strobe line low then high.
//! 3. PIO0 SM0 captures (strobe + D0..D7) and pushes to the RX FIFO.
//!
//! ### Reverse (Pico → DOS)
//!
//! Sends one byte as two nibbles, low nibble first:
//!
//! 1. Present nibble bits on status[3..=6] (MSB at bit 6).
//! 2. Toggle status bit 7 (phase) — DOS polls until phase changes.
//! 3. Hold 100 µs so DOS's debounce-and-sample sees a stable nibble.
//! 4. Repeat for the high nibble.
//!
//! PIO0 SM1 holds the timing autonomously; the CPU just pre-packs both
//! nibble patterns into a 10-bit value and pushes one u32 per byte. See
//! [`super::pio_nibble_out`] for details.
//!
//! ### Persistent phase invariant
//!
//! Phase toggles on every nibble emission. Across a full byte (two
//! nibbles), it toggles twice — returning to the byte-boundary phase
//! state. We initialize the wire to LOW phase before enabling the SM, so
//! every `send_byte` ends back at LOW. No CPU-side phase tracking needed.
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
//! assume the strobe lands on GP11 and flag this as a bring-up TODO:
//! confirm with a logic analyzer which control line goes low on the DOS
//! `OUT dx, al` for `CTRL_INIT`.

use defmt::trace;

use super::pio_compat_in::PioCompatIn;
use super::pio_nibble_out::PioNibbleOut;
use super::{LptError, LptMode, LptPhy};

pub struct SppNibblePhy {
    compat_in: PioCompatIn,
    nibble_out: PioNibbleOut,
}

impl SppNibblePhy {
    pub fn new(compat_in: PioCompatIn, nibble_out: PioNibbleOut) -> Self {
        Self {
            compat_in,
            nibble_out,
        }
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
        self.nibble_out.send_byte(byte).await;
        trace!("LPT send 0x{:02X}", byte);
        Ok(())
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
