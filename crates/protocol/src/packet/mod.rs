//! Wire packet encode / decode.
//!
//! Format (matches `dos/stage1/stage1.asm:97-122`):
//!
//! ```text
//! SOH | CMD | SEQ | LEN_HI | LEN_LO | PAYLOAD | CRC_HI | CRC_LO | ETX
//! ```
//!
//! - `SOH = 0x01`, `ETX = 0x03`.
//! - `LEN` is big-endian u16 payload length.
//! - CRC-16-CCITT covers `CMD..end-of-payload` (4 + payload_len bytes).
//!
//! The decoder accepts a complete framed packet (the caller is responsible for
//! framing/de-framing on the wire). The encoder writes into a caller-provided
//! buffer and returns the byte count.

pub mod commands;
pub mod crc16;
pub mod crc32;

use heapless::Vec;

pub const SOH: u8 = 0x01;
pub const ETX: u8 = 0x03;
pub const HEADER_LEN: usize = 5;
pub const TRAILER_LEN: usize = 3;
pub const OVERHEAD: usize = HEADER_LEN + TRAILER_LEN;

/// Maximum payload Stage 1 will encode/decode. Matches `PACKET_BUF_SIZE` in
/// `stage1.asm:102`. Larger frames are an error.
pub const MAX_PAYLOAD: usize = 256 - OVERHEAD;

/// Maximum total wire length of a single packet.
pub const MAX_PACKET: usize = MAX_PAYLOAD + OVERHEAD;

#[derive(Debug, Clone)]
pub struct IncomingPacket {
    pub cmd: u8,
    pub seq: u8,
    pub payload: Vec<u8, MAX_PAYLOAD>,
}

/// A packet produced by the protocol dispatcher, awaiting transport-side
/// encoding and transmission. The transport assigns the wire byte position
/// (encoding via `encode()`); SEQ is set by the dispatcher.
#[derive(Debug, Clone)]
pub struct OutgoingPacket {
    pub cmd: u8,
    pub seq: u8,
    pub payload: Vec<u8, MAX_PAYLOAD>,
}

impl OutgoingPacket {
    /// Construct from cmd + seq + payload slice. Returns `None` if the
    /// payload is too large.
    pub fn new(cmd: u8, seq: u8, payload_slice: &[u8]) -> Option<Self> {
        if payload_slice.len() > MAX_PAYLOAD {
            return None;
        }
        let mut payload = Vec::new();
        payload.extend_from_slice(payload_slice).ok()?;
        Some(Self { cmd, seq, payload })
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for OutgoingPacket {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(
            f,
            "OutgoingPacket {{ cmd=0x{:02X} seq={} payload={}B }}",
            self.cmd,
            self.seq,
            self.payload.len()
        );
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for IncomingPacket {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(
            f,
            "IncomingPacket {{ cmd=0x{:02X} seq={} payload={}B }}",
            self.cmd,
            self.seq,
            self.payload.len()
        );
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum DecodeError {
    TooShort,
    BadSoh,
    BadLen,
    BadEtx,
    BadCrc,
    PayloadOverflow,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum EncodeError {
    PayloadOverflow,
    OutputTooSmall,
}

/// Decode a complete wire packet. `buf.len()` must equal the framed packet
/// length (`8 + payload_len`); shorter or longer is an error.
pub fn decode(buf: &[u8]) -> Result<IncomingPacket, DecodeError> {
    if buf.len() < OVERHEAD {
        return Err(DecodeError::TooShort);
    }
    if buf[0] != SOH {
        return Err(DecodeError::BadSoh);
    }

    let payload_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    if payload_len > MAX_PAYLOAD {
        return Err(DecodeError::PayloadOverflow);
    }
    if buf.len() != payload_len + OVERHEAD {
        return Err(DecodeError::BadLen);
    }

    let etx_off = HEADER_LEN + payload_len + 2;
    if buf[etx_off] != ETX {
        return Err(DecodeError::BadEtx);
    }

    // CRC over CMD..end-of-payload (4 + payload_len bytes at offset 1)
    let crc_start = 1;
    let crc_end = HEADER_LEN + payload_len;
    let computed = crc16::compute(&buf[crc_start..crc_end]);
    let on_wire = u16::from_be_bytes([buf[crc_end], buf[crc_end + 1]]);
    if computed != on_wire {
        return Err(DecodeError::BadCrc);
    }

    let cmd = buf[1];
    let seq = buf[2];
    let mut payload: Vec<u8, MAX_PAYLOAD> = Vec::new();
    payload
        .extend_from_slice(&buf[HEADER_LEN..HEADER_LEN + payload_len])
        .ok();
    Ok(IncomingPacket { cmd, seq, payload })
}

/// Encode a packet into `out`. Returns the framed byte count or `Err` if the
/// payload is too large or the output buffer can't hold the result.
pub fn encode(
    cmd: u8,
    seq: u8,
    payload: &[u8],
    out: &mut [u8],
) -> Result<usize, EncodeError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(EncodeError::PayloadOverflow);
    }
    let total = payload.len() + OVERHEAD;
    if out.len() < total {
        return Err(EncodeError::OutputTooSmall);
    }

    out[0] = SOH;
    out[1] = cmd;
    out[2] = seq;
    let len_be = (payload.len() as u16).to_be_bytes();
    out[3] = len_be[0];
    out[4] = len_be[1];
    out[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);

    let crc_start = 1;
    let crc_end = HEADER_LEN + payload.len();
    let crc = crc16::compute(&out[crc_start..crc_end]).to_be_bytes();
    out[crc_end] = crc[0];
    out[crc_end + 1] = crc[1];
    out[crc_end + 2] = ETX;

    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_empty() {
        let mut buf = [0u8; MAX_PACKET];
        let n = encode(commands::CMD_PING, 7, &[], &mut buf).unwrap();
        assert_eq!(n, OVERHEAD);
        let p = decode(&buf[..n]).unwrap();
        assert_eq!(p.cmd, commands::CMD_PING);
        assert_eq!(p.seq, 7);
        assert_eq!(p.payload.len(), 0);
    }

    #[test]
    fn round_trip_payload() {
        let mut buf = [0u8; MAX_PACKET];
        let payload = b"hello";
        let n = encode(commands::CMD_PONG, 42, payload, &mut buf).unwrap();
        let p = decode(&buf[..n]).unwrap();
        assert_eq!(p.cmd, commands::CMD_PONG);
        assert_eq!(p.seq, 42);
        assert_eq!(p.payload.as_slice(), payload);
    }

    #[test]
    fn corrupt_crc_rejected() {
        let mut buf = [0u8; MAX_PACKET];
        let n = encode(commands::CMD_PING, 0, b"x", &mut buf).unwrap();
        buf[n - 2] ^= 0xFF; // flip CRC low byte
        assert!(matches!(decode(&buf[..n]), Err(DecodeError::BadCrc)));
    }

    #[test]
    fn corrupt_etx_rejected() {
        let mut buf = [0u8; MAX_PACKET];
        let n = encode(commands::CMD_PING, 0, b"x", &mut buf).unwrap();
        buf[n - 1] = 0xAA;
        assert!(matches!(decode(&buf[..n]), Err(DecodeError::BadEtx)));
    }
}
