//! `CAP_REQ` → `CAP_RSP` → `CAP_ACK` capability handshake.
//!
//! `CAP_RSP` payload layout matches `stage1.asm:191-205`:
//!
//! | Offset | Size  | Field |
//! |--------|-------|-------|
//! | 0      | u8    | `version_major` (must be 1) |
//! | 1      | u8    | `version_minor` |
//! | 2-22   | 21 B  | reserved (zeros) |
//! | 23     | u8    | `active_parallel_mode` (NEG_MODE_*) |
//! | 24-27  | 4 B   | reserved (zeros) |
//! | 28     | u32 BE| `stage2_image_size` |
//! | 32     | u32 BE| `stage2_image_crc32` |
//!
//! Minimum payload length expected by Stage 1: `CAP_RSP_MIN_PAYLOAD = 36`.

use crate::protocol::stage_blobs::{stage2_crc32, STAGE2_SIZE};

pub const VERSION_MAJOR: u8 = 1;
pub const VERSION_MINOR: u8 = 0;

/// Negotiated mode reported back to Stage 1. Phase 3 only supports SPP, so we
/// always report `NEG_MODE_SPP = 1`. Phase 5 will plumb the actual mode in.
pub const ACTIVE_MODE_SPP: u8 = 1;

pub const PAYLOAD_LEN: usize = 36;

/// Fill `out` with the CAP_RSP payload. `out.len()` must be at least
/// `PAYLOAD_LEN`. Returns the number of bytes written.
pub fn build_cap_rsp_payload(out: &mut [u8]) -> usize {
    debug_assert!(out.len() >= PAYLOAD_LEN);
    let slice = &mut out[..PAYLOAD_LEN];
    slice.fill(0);

    slice[0] = VERSION_MAJOR;
    slice[1] = VERSION_MINOR;
    slice[23] = ACTIVE_MODE_SPP;

    let size = STAGE2_SIZE as u32;
    slice[28..32].copy_from_slice(&size.to_be_bytes());

    let crc = stage2_crc32();
    slice[32..36].copy_from_slice(&crc.to_be_bytes());

    PAYLOAD_LEN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_layout_matches_stage1_expectations() {
        let mut buf = [0xAAu8; PAYLOAD_LEN];
        let n = build_cap_rsp_payload(&mut buf);
        assert_eq!(n, PAYLOAD_LEN);
        assert_eq!(buf[0], VERSION_MAJOR);
        assert_eq!(buf[1], VERSION_MINOR);
        // Reserved range must be zero
        assert!(buf[2..23].iter().all(|&b| b == 0));
        assert_eq!(buf[23], ACTIVE_MODE_SPP);
        assert!(buf[24..28].iter().all(|&b| b == 0));
        // Size big-endian
        let size = u32::from_be_bytes([buf[28], buf[29], buf[30], buf[31]]);
        assert_eq!(size as usize, STAGE2_SIZE);
        // CRC big-endian
        let crc = u32::from_be_bytes([buf[32], buf[33], buf[34], buf[35]]);
        assert_eq!(crc, stage2_crc32());
    }
}
