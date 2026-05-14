//! PS/2 frame packing — CPU-side companion to the firmware's PIO TX path.
//!
//! The TX PIO program consumes one 32-bit word per frame and shifts it
//! LSB-first onto the wire. These helpers build that word for both
//! protocol variants:
//!
//!   * **AT / PS-2** — start(0) + 8 data + odd parity + stop(1) = 11 bits.
//!   * **XT**         — start(1) + 8 data                            = 9 bits.
//!
//! Unused high bits are padded with 1s so the open-drain wire idles HIGH
//! if the PIO program over-consumes its loop counter.

/// AT/PS-2 frame: 11 bits, start(0) + D0..D7 LSB-first + odd parity + stop(1).
///
/// Bit layout (LSB → MSB):
///   * bit 0     = start (0)
///   * bits 1..8 = D0..D7 (LSB-first)
///   * bit 9     = odd parity
///   * bit 10    = stop (1)
///   * bits 11.. = 1s (idle-high padding)
pub fn pack_at_frame(byte: u8) -> u32 {
    let mut w: u32 = 0xFFFF_F800; // bits 11..31 padded HIGH
    // bit 0 = start = 0 (already clear)
    w |= (byte as u32) << 1;
    let parity: u32 = if byte.count_ones() & 1 == 0 { 1 } else { 0 };
    w |= parity << 9;
    w |= 1 << 10; // stop
    w
}

/// XT frame: 9 bits, start(1) + D0..D7 LSB-first. No parity, no stop.
pub fn pack_xt_frame(byte: u8) -> u32 {
    let mut w: u32 = 0xFFFF_FE00; // bits 9..31 padded HIGH
    w |= 1; // start bit
    w |= (byte as u32) << 1;
    w
}
