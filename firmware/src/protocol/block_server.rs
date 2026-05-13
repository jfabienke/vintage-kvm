//! Stage 2 block server.
//!
//! Serves `STAGE2_PLACEHOLDER` to DOS Stage 1 via the
//! `SEND_BLOCK` → `RECV_BLOCK` → `BLOCK_ACK` / `BLOCK_NAK` protocol defined in
//! `dos/stage1/stage1.asm:231-251`.
//!
//! Per-block flow:
//!
//! ```text
//! DOS  → Pico:  SEND_BLOCK    payload = u32 block_no (BE)
//! Pico → DOS:   RECV_BLOCK    payload = u32 block_no (BE)
//!                                     + u8  byte_count (1..=64)
//!                                     + byte_count data bytes
//! DOS  → Pico:  BLOCK_ACK     payload = u32 block_no (BE)     (success)
//!              BLOCK_NAK     payload = u32 block_no (BE)     (retry)
//! ```

use crate::protocol::stage_blobs::stage2_block;

pub const BLOCK_SIZE: usize = 64;

/// `RECV_BLOCK` payload header: `u32 block_no` (BE) + `u8 byte_count`.
pub const RECV_HDR_LEN: usize = 5;

/// Maximum `RECV_BLOCK` payload length (5 B header + up to 64 B data).
pub const RECV_MAX_PAYLOAD: usize = RECV_HDR_LEN + BLOCK_SIZE;

#[derive(Debug, Clone, defmt::Format)]
pub enum BlockError {
    BadRequestPayload,
    BlockOutOfRange,
}

/// Server-side per-session state. Tracks which block Stage 1 is expected to
/// request next so we can log out-of-order requests without rejecting them
/// (Stage 1's retry logic re-sends the same block_no on NAK).
#[derive(Default, Debug, defmt::Format)]
pub struct BlockServer {
    pub expected_block: u16,
}

impl BlockServer {
    pub const fn new() -> Self {
        Self { expected_block: 0 }
    }

    /// Reset to start-of-image. Used when a CAP_REQ resets the session.
    pub fn reset(&mut self) {
        self.expected_block = 0;
    }

    /// Parse `SEND_BLOCK` payload (u32 BE block_no).
    pub fn parse_send_block(payload: &[u8]) -> Result<u16, BlockError> {
        if payload.len() != 4 {
            return Err(BlockError::BadRequestPayload);
        }
        // High 16 bits must be zero (current_block is a u16 in Stage 1).
        if payload[0] != 0 || payload[1] != 0 {
            return Err(BlockError::BadRequestPayload);
        }
        Ok(u16::from_be_bytes([payload[2], payload[3]]))
    }

    /// Build `RECV_BLOCK` payload for `block_no`. Returns the byte count
    /// written into `out`, or `Err(BlockOutOfRange)` if the block is past
    /// end-of-image.
    pub fn build_recv_block(
        &self,
        block_no: u16,
        out: &mut [u8],
    ) -> Result<usize, BlockError> {
        debug_assert!(out.len() >= RECV_MAX_PAYLOAD);

        let (data, count) = stage2_block(block_no, BLOCK_SIZE)
            .ok_or(BlockError::BlockOutOfRange)?;

        let bn = block_no as u32;
        out[0..4].copy_from_slice(&bn.to_be_bytes());
        out[4] = count;
        out[RECV_HDR_LEN..RECV_HDR_LEN + count as usize].copy_from_slice(data);
        Ok(RECV_HDR_LEN + count as usize)
    }

    /// Handle `BLOCK_ACK(block_no)`. Idempotent on repeats.
    pub fn handle_ack(&mut self, payload: &[u8]) {
        if let Ok(bn) = Self::parse_send_block(payload) {
            if bn == self.expected_block {
                self.expected_block = self.expected_block.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::stage_blobs::STAGE2_SIZE;

    #[test]
    fn parses_block_no_be() {
        assert_eq!(
            BlockServer::parse_send_block(&[0, 0, 0x12, 0x34]).unwrap(),
            0x1234
        );
    }

    #[test]
    fn rejects_high_bits_set() {
        assert!(BlockServer::parse_send_block(&[1, 0, 0, 0]).is_err());
        assert!(BlockServer::parse_send_block(&[0, 1, 0, 0]).is_err());
    }

    #[test]
    fn rejects_wrong_payload_length() {
        assert!(BlockServer::parse_send_block(&[0, 0, 0]).is_err());
        assert!(BlockServer::parse_send_block(&[0, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn first_block_contains_entire_placeholder() {
        let server = BlockServer::new();
        let mut buf = [0u8; RECV_MAX_PAYLOAD];
        let n = server.build_recv_block(0, &mut buf).unwrap();
        assert_eq!(n, RECV_HDR_LEN + STAGE2_SIZE);
        // Echoed block_no = 0
        assert_eq!(&buf[..4], &[0, 0, 0, 0]);
        assert_eq!(buf[4] as usize, STAGE2_SIZE);
    }

    #[test]
    fn out_of_range_block_errors() {
        let server = BlockServer::new();
        let mut buf = [0u8; RECV_MAX_PAYLOAD];
        assert!(server.build_recv_block(1, &mut buf).is_err());
    }

    #[test]
    fn ack_advances_expected() {
        let mut s = BlockServer::new();
        s.handle_ack(&[0, 0, 0, 0]);
        assert_eq!(s.expected_block, 1);
    }

    #[test]
    fn ack_idempotent_on_repeat() {
        let mut s = BlockServer::new();
        s.handle_ack(&[0, 0, 0, 0]);
        s.handle_ack(&[0, 0, 0, 0]); // duplicate; should not advance again
        assert_eq!(s.expected_block, 1);
    }
}
