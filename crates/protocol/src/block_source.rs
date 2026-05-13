//! `BlockSource` trait.
//!
//! Abstracts "the Stage 2 image" so the block server (and CAP_RSP builder)
//! can be tested against an in-memory slice on the host, while the firmware
//! plugs in an `include_bytes!`-backed embedded blob.

/// A blob of bytes that can be served in fixed-size blocks.
///
/// Implementations advertise the total size and the CRC-32 expected for the
/// whole image, and supply per-block byte slices on demand. The last block
/// may be shorter than `block_size`.
pub trait BlockSource {
    /// Total image size in bytes.
    fn total_size(&self) -> usize;

    /// CRC-32/IEEE (reflected, init 0xFFFFFFFF, xor-out 0xFFFFFFFF) of the
    /// whole image. The host (DOS Stage 1) compares its running CRC against
    /// this at end-of-download.
    fn crc32(&self) -> u32;

    /// Return the bytes for `block_no` (0-indexed) and the actual byte count.
    /// `None` if `block_no` is past end-of-image.
    fn block(&self, block_no: u16, block_size: usize) -> Option<(&[u8], u8)>;
}

/// Adapter that turns a `&'static [u8]` (and a pre-computed CRC) into a
/// `BlockSource`. Useful for both embedded blobs and host-side tests.
#[derive(Debug, Clone, Copy)]
pub struct SliceBlob {
    data: &'static [u8],
    crc32: u32,
}

impl SliceBlob {
    pub const fn new(data: &'static [u8], crc32: u32) -> Self {
        Self { data, crc32 }
    }
}

impl BlockSource for SliceBlob {
    fn total_size(&self) -> usize {
        self.data.len()
    }

    fn crc32(&self) -> u32 {
        self.crc32
    }

    fn block(&self, block_no: u16, block_size: usize) -> Option<(&[u8], u8)> {
        let start = (block_no as usize).checked_mul(block_size)?;
        if start >= self.data.len() {
            return None;
        }
        let end = core::cmp::min(start + block_size, self.data.len());
        let slice = &self.data[start..end];
        Some((slice, slice.len() as u8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::crc32 as crc32_mod;

    static BLOB: &[u8] = b"Hello, vintage-kvm protocol!";

    fn make() -> SliceBlob {
        SliceBlob::new(BLOB, crc32_mod::compute(BLOB))
    }

    #[test]
    fn total_size_matches_data_len() {
        assert_eq!(make().total_size(), BLOB.len());
    }

    #[test]
    fn crc32_matches() {
        assert_eq!(make().crc32(), crc32_mod::compute(BLOB));
    }

    #[test]
    fn first_block_covers_data_when_block_size_exceeds_blob() {
        let s = make();
        let (data, n) = s.block(0, 64).unwrap();
        assert_eq!(n as usize, BLOB.len());
        assert_eq!(data, BLOB);
    }

    #[test]
    fn last_block_may_be_short() {
        // 28-byte blob, 8-byte blocks → last block is 4 bytes (28 mod 8)
        let s = make();
        let (data, n) = s.block(3, 8).unwrap();
        assert_eq!(n, 4);
        assert_eq!(data, &BLOB[24..28]);
    }

    #[test]
    fn past_end_returns_none() {
        let s = make();
        // blob is 28 B, 8-byte blocks → blocks 0..3 valid (0,1,2,3), block 4 None
        assert!(s.block(4, 8).is_none());
    }
}
