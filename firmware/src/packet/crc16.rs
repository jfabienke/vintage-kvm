//! CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, no refl, no xor-out).
//!
//! Byte-for-byte equivalent of `dos/stage1/stage1.asm:705` (`crc16_ccitt`).
//! Bit-by-bit; suitable for small packet headers / payloads at SPP-nibble
//! data rates. A table-driven variant is trivial to add later if Phase 5+
//! line rates demand it.

pub const POLY: u16 = 0x1021;
pub const INIT: u16 = 0xFFFF;

pub fn compute(buf: &[u8]) -> u16 {
    let mut crc: u16 = INIT;
    for &b in buf {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ POLY;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buf_returns_init() {
        assert_eq!(compute(&[]), INIT);
    }

    #[test]
    fn known_vector() {
        // Standard CCITT-FALSE test vector: CRC("123456789") == 0x29B1.
        assert_eq!(compute(b"123456789"), 0x29B1);
    }
}
