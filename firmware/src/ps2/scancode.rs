//! ASCII → PS/2 scancode lookup tables.
//!
//! Phase 2 v2 supports Set 2 (AT/PS-2) only. Set 1 (XT) is a follow-up.
//!
//! Each table entry is one byte:
//!   - `0x00` = no mapping (caller skips the character).
//!   - non-zero = scancode value with bit 7 clear if no shift is
//!     required. Shifted characters (uppercase, punctuation that's
//!     `shift+other`) are not in this table; v2 sticks to lowercase
//!     ASCII + digits + a few control codes so the bootstrap script can
//!     run without keyboard-LED state tracking.
//!
//! Reference: Set 2 scancodes documented in the IBM PS/2 Technical
//! Reference; mirrored across multiple modern collections (osdev.org,
//! kbd-data archives).

/// Set 2 scancode for `c` (ASCII), or 0 if unsupported.
pub const fn ascii_to_set2(c: u8) -> u8 {
    SET2_TABLE[c as usize]
}

const fn build_table() -> [u8; 128] {
    let mut t = [0u8; 128];

    // Lowercase letters.
    t[b'a' as usize] = 0x1C;
    t[b'b' as usize] = 0x32;
    t[b'c' as usize] = 0x21;
    t[b'd' as usize] = 0x23;
    t[b'e' as usize] = 0x24;
    t[b'f' as usize] = 0x2B;
    t[b'g' as usize] = 0x34;
    t[b'h' as usize] = 0x33;
    t[b'i' as usize] = 0x43;
    t[b'j' as usize] = 0x3B;
    t[b'k' as usize] = 0x42;
    t[b'l' as usize] = 0x4B;
    t[b'm' as usize] = 0x3A;
    t[b'n' as usize] = 0x31;
    t[b'o' as usize] = 0x44;
    t[b'p' as usize] = 0x4D;
    t[b'q' as usize] = 0x15;
    t[b'r' as usize] = 0x2D;
    t[b's' as usize] = 0x1B;
    t[b't' as usize] = 0x2C;
    t[b'u' as usize] = 0x3C;
    t[b'v' as usize] = 0x2A;
    t[b'w' as usize] = 0x1D;
    t[b'x' as usize] = 0x22;
    t[b'y' as usize] = 0x35;
    t[b'z' as usize] = 0x1A;

    // Digits (top row, not numpad).
    t[b'0' as usize] = 0x45;
    t[b'1' as usize] = 0x16;
    t[b'2' as usize] = 0x1E;
    t[b'3' as usize] = 0x26;
    t[b'4' as usize] = 0x25;
    t[b'5' as usize] = 0x2E;
    t[b'6' as usize] = 0x36;
    t[b'7' as usize] = 0x3D;
    t[b'8' as usize] = 0x3E;
    t[b'9' as usize] = 0x46;

    // Whitespace + return + a few unshifted punctuation.
    t[b' ' as usize] = 0x29;
    t[b'\r' as usize] = 0x5A;
    t[b'\t' as usize] = 0x0D;
    t[b'\x08' as usize] = 0x66; // backspace
    t[b'-' as usize] = 0x4E;
    t[b'=' as usize] = 0x55;
    t[b'[' as usize] = 0x54;
    t[b']' as usize] = 0x5B;
    t[b';' as usize] = 0x4C;
    t[b'\'' as usize] = 0x52;
    t[b',' as usize] = 0x41;
    t[b'.' as usize] = 0x49;
    t[b'/' as usize] = 0x4A;
    t[b'`' as usize] = 0x0E;
    t[b'\\' as usize] = 0x5D;

    t
}

const SET2_TABLE: [u8; 128] = build_table();
