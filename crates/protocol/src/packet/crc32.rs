//! CRC-32/IEEE (zlib / PNG): poly 0xEDB88320 reflected, init 0xFFFFFFFF,
//! xor-out 0xFFFFFFFF.
//!
//! Byte-for-byte equivalent of `dos/stage1/stage1.asm:1692` (`crc32_*`).
//! The DMA-sniffer hardware variant implements the same `Crc32Engine`
//! trait — see `crate::crc`.

pub const POLY: u32 = 0xEDB88320;
pub const INIT: u32 = 0xFFFFFFFF;
pub const XOROUT: u32 = 0xFFFFFFFF;

/// Compute CRC-32 over `buf`. Convenience wrapper around the trait;
/// suitable for one-shot use over short slices.
pub fn compute(buf: &[u8]) -> u32 {
    let mut state: u32 = INIT;
    for &b in buf {
        state ^= b as u32;
        for _ in 0..8 {
            let lsb_set = state & 1 != 0;
            state >>= 1;
            if lsb_set {
                state ^= POLY;
            }
        }
    }
    state ^ XOROUT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buf() {
        assert_eq!(compute(&[]), 0);
    }

    #[test]
    fn known_vector() {
        // Standard test vector: CRC32("123456789") == 0xCBF43926.
        assert_eq!(compute(b"123456789"), 0xCBF43926);
    }
}
