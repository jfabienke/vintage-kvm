//! `CAP_REQ` → `CAP_RSP` → `CAP_ACK` capability handshake.
//!
//! `CAP_RSP` payload layout matches `dos/stage1/stage1.asm:191-205`:
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

use crate::block_source::BlockSource;

pub const VERSION_MAJOR: u8 = 1;
pub const VERSION_MINOR: u8 = 0;

/// Negotiated mode reported back to Stage 1. Phase 3 only supports SPP, so we
/// always report `NEG_MODE_SPP = 1`. Phase 5 will plumb the actual mode in.
pub const ACTIVE_MODE_SPP: u8 = 1;

pub const PAYLOAD_LEN: usize = 36;

/// Fill `out` with the CAP_RSP payload for `blob`. `out.len()` must be at
/// least `PAYLOAD_LEN`. Returns the number of bytes written.
///
/// The `active_mode` byte is supplied by the caller because it depends on
/// the current LPT mode, not on the blob itself.
pub fn build_cap_rsp_payload<B: BlockSource>(
    blob: &B,
    active_mode: u8,
    out: &mut [u8],
) -> usize {
    debug_assert!(out.len() >= PAYLOAD_LEN);
    let slice = &mut out[..PAYLOAD_LEN];
    slice.fill(0);

    slice[0] = VERSION_MAJOR;
    slice[1] = VERSION_MINOR;
    slice[23] = active_mode;

    let size = blob.total_size() as u32;
    slice[28..32].copy_from_slice(&size.to_be_bytes());

    let crc = blob.crc32();
    slice[32..36].copy_from_slice(&crc.to_be_bytes());

    PAYLOAD_LEN
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_source::SliceBlob;

    static TEST_BLOB: &[u8] = b"abcdef";

    #[test]
    fn payload_layout_matches_stage1_expectations() {
        let blob = SliceBlob::new(TEST_BLOB, 0xDEADBEEF);
        let mut buf = [0xAAu8; PAYLOAD_LEN];
        let n = build_cap_rsp_payload(&blob, ACTIVE_MODE_SPP, &mut buf);
        assert_eq!(n, PAYLOAD_LEN);
        assert_eq!(buf[0], VERSION_MAJOR);
        assert_eq!(buf[1], VERSION_MINOR);
        assert!(buf[2..23].iter().all(|&b| b == 0));
        assert_eq!(buf[23], ACTIVE_MODE_SPP);
        assert!(buf[24..28].iter().all(|&b| b == 0));
        let size = u32::from_be_bytes([buf[28], buf[29], buf[30], buf[31]]);
        assert_eq!(size as usize, TEST_BLOB.len());
        let crc = u32::from_be_bytes([buf[32], buf[33], buf[34], buf[35]]);
        assert_eq!(crc, 0xDEADBEEF);
    }
}
