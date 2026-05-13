//! Byte-stream → packet reassembler.
//!
//! Drains bytes from the LPT phy and emits whole packets to the dispatcher.
//! Stream framing: look for `SOH`, accumulate header (4 more bytes) to learn
//! `LEN`, accumulate `LEN + TRAILER_LEN = LEN + 3` more bytes, hand off the
//! complete frame for CRC + ETX validation.
//!
//! Bytes received before a `SOH` are dropped silently (recovers from any
//! mid-stream noise after a power glitch or interrupted transfer).

use defmt::{debug, warn};

use crate::lpt::compat::SppNibblePhy;
use crate::packet::{decode, DecodeError, IncomingPacket, HEADER_LEN, MAX_PACKET, SOH};

pub struct PacketReassembler {
    /// Working buffer for the currently-accumulating frame.
    buf: [u8; MAX_PACKET],
    /// Bytes valid in `buf`.
    len: usize,
}

impl PacketReassembler {
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_PACKET],
            len: 0,
        }
    }

    /// Read bytes from the LPT phy until a complete packet is decoded.
    ///
    /// Returns the decoded packet. Bytes that arrive before a `SOH`, or that
    /// form an invalid packet (bad CRC / ETX / length), are dropped and the
    /// reassembler resyncs on the next `SOH`.
    pub async fn next_packet(&mut self, phy: &mut SppNibblePhy) -> IncomingPacket {
        loop {
            // Find SOH
            let b = phy.recv_byte().await.unwrap();
            if b != SOH {
                debug!("dropped pre-SOH byte 0x{:02X}", b);
                continue;
            }
            self.buf[0] = SOH;
            self.len = 1;

            // Pull the rest of the 5-byte header
            for i in 1..HEADER_LEN {
                self.buf[i] = phy.recv_byte().await.unwrap();
            }
            self.len = HEADER_LEN;

            let payload_len = u16::from_be_bytes([self.buf[3], self.buf[4]]) as usize;
            let total = HEADER_LEN + payload_len + 3;
            if total > MAX_PACKET {
                warn!("payload too long ({}); resyncing", payload_len);
                self.len = 0;
                continue;
            }

            // Pull payload + CRC + ETX
            for i in HEADER_LEN..total {
                self.buf[i] = phy.recv_byte().await.unwrap();
            }
            self.len = total;

            match decode(&self.buf[..self.len]) {
                Ok(p) => {
                    self.len = 0;
                    return p;
                }
                Err(DecodeError::BadCrc) => {
                    warn!("CRC mismatch on inbound packet; dropping");
                    self.len = 0;
                    continue;
                }
                Err(e) => {
                    warn!("decode error {}; resyncing", e);
                    self.len = 0;
                    continue;
                }
            }
        }
    }
}
