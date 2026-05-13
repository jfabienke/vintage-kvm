//! Byte-stream → packet reassembler.
//!
//! Drains bytes from a `LptPhy` and reassembles whole packets. Stream
//! framing: look for `SOH`, accumulate header (4 more bytes) to learn
//! `LEN`, accumulate `LEN + TRAILER_LEN = LEN + 3` more bytes, hand off
//! the complete frame for CRC + ETX validation.
//!
//! Bytes received before a `SOH` are dropped silently (recovers from any
//! mid-stream noise after a power glitch or interrupted transfer); the
//! drop is reported via telemetry so an operator can see a delta.

use crate::lpt::LptPhy;
use crate::telemetry::{Event, ResyncReason, TelemetryEmit};
use vintage_kvm_protocol::{
    decode, DecodeError, IncomingPacket, HEADER_LEN, MAX_PACKET, SOH,
};

pub struct PacketReassembler {
    buf: [u8; MAX_PACKET],
}

impl PacketReassembler {
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_PACKET],
        }
    }

    /// Read bytes from `phy` until a complete packet is decoded.
    ///
    /// Returns the decoded packet. Bytes that arrive before a `SOH`, or that
    /// form an invalid packet (bad CRC / ETX / length), are dropped and the
    /// reassembler resyncs on the next `SOH`. Each drop emits a
    /// `PacketStreamResync` event with the reason.
    pub async fn next_packet<P: LptPhy, T: TelemetryEmit>(
        &mut self,
        phy: &mut P,
        telemetry: &T,
    ) -> IncomingPacket {
        loop {
            // Find SOH
            let b = phy.recv_byte().await.unwrap_or(0);
            if b != SOH {
                telemetry.emit(Event::PacketStreamResync {
                    reason: ResyncReason::PreSohByte,
                });
                continue;
            }
            self.buf[0] = SOH;

            // Pull the rest of the 5-byte header
            for i in 1..HEADER_LEN {
                self.buf[i] = phy.recv_byte().await.unwrap_or(0);
            }

            let payload_len = u16::from_be_bytes([self.buf[3], self.buf[4]]) as usize;
            let total = HEADER_LEN + payload_len + 3;
            if total > MAX_PACKET {
                telemetry.emit(Event::PacketStreamResync {
                    reason: ResyncReason::PayloadTooLong,
                });
                continue;
            }

            // Pull payload + CRC + ETX
            for i in HEADER_LEN..total {
                self.buf[i] = phy.recv_byte().await.unwrap_or(0);
            }

            match decode(&self.buf[..total]) {
                Ok(p) => return p,
                Err(DecodeError::BadCrc) => {
                    telemetry.emit(Event::PacketStreamResync {
                        reason: ResyncReason::BadCrc,
                    });
                }
                Err(DecodeError::BadEtx) => {
                    telemetry.emit(Event::PacketStreamResync {
                        reason: ResyncReason::BadEtx,
                    });
                }
                Err(_) => {
                    telemetry.emit(Event::PacketStreamResync {
                        reason: ResyncReason::DecodeError,
                    });
                }
            }
        }
    }
}

impl Default for PacketReassembler {
    fn default() -> Self {
        Self::new()
    }
}
