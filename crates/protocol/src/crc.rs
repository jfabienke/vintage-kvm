//! CRC engine traits.
//!
//! Per [`firmware_crate_and_trait_design.md` §4.2](../../../docs/firmware_crate_and_trait_design.md):
//! every CRC has at least two implementations — a portable software engine
//! that works on any target, and (on RP2350) a DMA-sniffer-backed hardware
//! engine that costs zero CPU cycles. Both implement the same trait so the
//! caller doesn't have to care which is in use.
//!
//! Hardware-accelerated impls live in `firmware/src/crc_sniffer.rs` because
//! they depend on RP2350-specific peripherals.

use crate::packet::{crc16, crc32};

/// CRC-16-CCITT (poly 0x1021, init 0xFFFF, no refl, no xor-out).
pub trait Crc16Engine {
    fn reset(&mut self);
    fn update(&mut self, data: &[u8]);
    fn finalize(&self) -> u16;
}

/// CRC-32/IEEE reflected (poly 0xEDB88320, init 0xFFFFFFFF, xor-out 0xFFFFFFFF).
pub trait Crc32Engine {
    fn reset(&mut self);
    fn update(&mut self, data: &[u8]);
    fn finalize(&self) -> u32;
}

// --- Software implementations ---------------------------------------------

/// Bit-by-bit CRC-16-CCITT. Always available. Suitable for small packets
/// (≤256 B in our protocol) where the DMA-sniffer setup cost would exceed
/// the compute saving.
#[derive(Debug, Clone)]
pub struct SoftwareCrc16Ccitt {
    state: u16,
}

impl SoftwareCrc16Ccitt {
    pub const fn new() -> Self {
        Self { state: crc16::INIT }
    }
}

impl Default for SoftwareCrc16Ccitt {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc16Engine for SoftwareCrc16Ccitt {
    fn reset(&mut self) {
        self.state = crc16::INIT;
    }

    fn update(&mut self, data: &[u8]) {
        let mut crc = self.state;
        for &b in data {
            crc ^= (b as u16) << 8;
            for _ in 0..8 {
                if crc & 0x8000 != 0 {
                    crc = (crc << 1) ^ crc16::POLY;
                } else {
                    crc <<= 1;
                }
            }
        }
        self.state = crc;
    }

    fn finalize(&self) -> u16 {
        self.state
    }
}

/// Bit-by-bit CRC-32/IEEE reflected. Always available. The big win on the
/// firmware side is the DMA-sniffer impl that accumulates this CRC for free
/// over the Stage 2 download stream — but the software impl is correct on
/// any target and is the reference for tests.
#[derive(Debug, Clone)]
pub struct SoftwareCrc32Reflected {
    state: u32,
}

impl SoftwareCrc32Reflected {
    pub const fn new() -> Self {
        Self { state: crc32::INIT }
    }
}

impl Default for SoftwareCrc32Reflected {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32Engine for SoftwareCrc32Reflected {
    fn reset(&mut self) {
        self.state = crc32::INIT;
    }

    fn update(&mut self, data: &[u8]) {
        let mut state = self.state;
        for &b in data {
            state ^= b as u32;
            for _ in 0..8 {
                let lsb_set = state & 1 != 0;
                state >>= 1;
                if lsb_set {
                    state ^= crc32::POLY;
                }
            }
        }
        self.state = state;
    }

    fn finalize(&self) -> u32 {
        self.state ^ crc32::XOROUT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_crc16_matches_compute() {
        let mut e = SoftwareCrc16Ccitt::new();
        e.update(b"123456789");
        assert_eq!(e.finalize(), crc16::compute(b"123456789"));
    }

    #[test]
    fn software_crc16_split_updates_match() {
        let mut e = SoftwareCrc16Ccitt::new();
        e.update(b"123");
        e.update(b"456");
        e.update(b"789");
        assert_eq!(e.finalize(), crc16::compute(b"123456789"));
    }

    #[test]
    fn software_crc32_matches_compute() {
        let mut e = SoftwareCrc32Reflected::new();
        e.update(b"123456789");
        assert_eq!(e.finalize(), crc32::compute(b"123456789"));
    }

    #[test]
    fn software_crc32_split_updates_match() {
        let mut e = SoftwareCrc32Reflected::new();
        e.update(b"123");
        e.update(b"456");
        e.update(b"789");
        assert_eq!(e.finalize(), crc32::compute(b"123456789"));
    }

    #[test]
    fn reset_returns_to_init() {
        let mut e = SoftwareCrc32Reflected::new();
        e.update(b"junk");
        e.reset();
        e.update(b"123456789");
        assert_eq!(e.finalize(), crc32::compute(b"123456789"));
    }
}
