//! Embedded Stage 0 / 1 / 2 blobs served over the bootstrap channel.
//!
//! Phase 3 MVP: only Stage 2 is needed, and it's a placeholder. Stage 0/1
//! blobs are flagged TODO until the DEBUG-injection layer (Phase 1) lands.
//!
//! The Stage 2 placeholder is a tiny DOS .COM-style program that prints a
//! recognition banner via `INT 21h AH=09h`, then exits via `INT 21h
//! AH=4Ch AL=00h`. DOS treats a non-`MZ`-prefixed file as a .COM regardless
//! of extension, so this works as `PICO1284.EXE`.
//!
//! The placeholder is small (well under 64 B = 1 download block) so the
//! Stage 1 → Stage 2 path exercises the *single-block* corner of the
//! block server, including the last-block-short case.
//!
//! This module wraps the embedded blob in a `BlockSource` impl (defined in
//! the `vintage-kvm-protocol` crate) so the protocol-side block server is
//! generic over the source.

use vintage_kvm_protocol::block_source::BlockSource;
use vintage_kvm_protocol::packet::crc32;

/// Stage 2 placeholder: prints "PICO1284 Stage 2 placeholder v0.1\r\n" and
/// exits with errorlevel 0.
///
/// Disassembly (offsets are CS:0100 since this is treated as a .COM):
///
/// ```text
/// 0100  B4 09           MOV  AH, 09h          ; DOS print-string
/// 0102  BA 0D 01        MOV  DX, 010Dh        ; string offset
/// 0105  CD 21           INT  21h
/// 0107  B8 00 4C        MOV  AX, 4C00h        ; exit, AL = errorlevel 0
/// 010A  CD 21           INT  21h
/// 010C  CC              INT3                  ; safety net; never reached
/// 010D  'PICO1284 Stage 2 placeholder v0.1\r\n$'
/// ```
pub static STAGE2_PLACEHOLDER: &[u8] = &[
    // entry / code (13 bytes)
    0xB4, 0x09, // MOV AH, 09h
    0xBA, 0x0D, 0x01, // MOV DX, 010Dh
    0xCD, 0x21, // INT 21h
    0xB8, 0x00, 0x4C, // MOV AX, 4C00h
    0xCD, 0x21, // INT 21h
    0xCC, // INT3 (unreachable)
    // string at offset 010Dh (35 bytes incl. $ terminator)
    b'P', b'I', b'C', b'O', b'1', b'2', b'8', b'4', b' ', b'S', b't', b'a',
    b'g', b'e', b' ', b'2', b' ', b'p', b'l', b'a', b'c', b'e', b'h', b'o',
    b'l', b'd', b'e', b'r', b' ', b'v', b'0', b'.', b'1', b'\r', b'\n', b'$',
];

pub const STAGE2_SIZE: usize = STAGE2_PLACEHOLDER.len();

/// Re-computed at startup to confirm the embedded blob's CRC matches the
/// declared value. Replaces the v1-style separate `stage2_crc32()` accessor.
pub fn stage2_crc32() -> u32 {
    crc32::compute(STAGE2_PLACEHOLDER)
}

/// Concrete `BlockSource` for the embedded Stage 2 placeholder.
///
/// CRC is computed once at construction so the trait's `crc32()` call is
/// free at serve time.
pub struct EmbeddedStage2 {
    crc: u32,
}

impl EmbeddedStage2 {
    pub fn new() -> Self {
        Self {
            crc: stage2_crc32(),
        }
    }
}

impl Default for EmbeddedStage2 {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockSource for EmbeddedStage2 {
    fn total_size(&self) -> usize {
        STAGE2_SIZE
    }

    fn crc32(&self) -> u32 {
        self.crc
    }

    fn block(&self, block_no: u16, block_size: usize) -> Option<(&[u8], u8)> {
        let start = (block_no as usize).checked_mul(block_size)?;
        if start >= STAGE2_SIZE {
            return None;
        }
        let end = core::cmp::min(start + block_size, STAGE2_SIZE);
        let slice = &STAGE2_PLACEHOLDER[start..end];
        Some((slice, slice.len() as u8))
    }
}
