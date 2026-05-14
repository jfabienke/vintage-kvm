//! Bit-layout assertions + round-trip tests for the PS/2 frame packer.

use vintage_kvm_ps2_framer::{
    pack_at_frame, pack_xt_frame, FrameKind, Framer, Ps2Frame,
};

// -------------------------------------------------------------------------
// AT/PS-2 — bit-layout assertions
// -------------------------------------------------------------------------

fn at_start(w: u32) -> u8 {
    (w & 1) as u8
}
fn at_data(w: u32) -> u8 {
    ((w >> 1) & 0xFF) as u8
}
fn at_parity(w: u32) -> u8 {
    ((w >> 9) & 1) as u8
}
fn at_stop(w: u32) -> u8 {
    ((w >> 10) & 1) as u8
}
fn at_padding(w: u32) -> u32 {
    w >> 11
}

#[test]
fn at_frame_0x00_layout() {
    // 0x00 → 0 data ones → parity must keep total odd → parity = 1.
    let w = pack_at_frame(0x00);
    assert_eq!(at_start(w), 0);
    assert_eq!(at_data(w), 0x00);
    assert_eq!(at_parity(w), 1);
    assert_eq!(at_stop(w), 1);
    assert_eq!(at_padding(w), 0x1F_FFFF, "padding must be all 1s");
}

#[test]
fn at_frame_0xff_layout() {
    // 0xFF → 8 data ones (even count) → parity = 1 to make total 9 (odd).
    let w = pack_at_frame(0xFF);
    assert_eq!(at_data(w), 0xFF);
    assert_eq!(at_parity(w), 1);
    assert_eq!(at_stop(w), 1);
}

#[test]
fn at_frame_0x55_layout() {
    // 0x55 = 0b01010101 → 4 data ones → even → parity = 1 → total 5 (odd).
    let w = pack_at_frame(0x55);
    assert_eq!(at_data(w), 0x55);
    assert_eq!(at_parity(w), 1);
}

#[test]
fn at_frame_0x80_layout() {
    // 0x80 → 1 data one → odd → parity = 0 → total 1 (odd).
    let w = pack_at_frame(0x80);
    assert_eq!(at_data(w), 0x80);
    assert_eq!(at_parity(w), 0);
}

#[test]
fn at_frame_0x1c_layout() {
    // 0x1C = 0b00011100 → 3 ones → odd → parity = 0 → total 3 (odd).
    let w = pack_at_frame(0x1C);
    assert_eq!(at_data(w), 0x1C);
    assert_eq!(at_parity(w), 0);
}

#[test]
fn at_parity_keeps_total_odd_for_every_byte() {
    // Exhaustive check: for every input byte, total ones in {data, parity}
    // must be odd.
    for byte in 0u8..=255 {
        let w = pack_at_frame(byte);
        let total_ones = at_data(w).count_ones() + at_parity(w) as u32;
        assert_eq!(
            total_ones & 1,
            1,
            "byte {:02X}: total ones = {} (must be odd)",
            byte,
            total_ones
        );
    }
}

// -------------------------------------------------------------------------
// XT — bit-layout assertions
// -------------------------------------------------------------------------

#[test]
fn xt_frame_start_high() {
    let w = pack_xt_frame(0xAA);
    assert_eq!(w & 1, 1, "XT start bit = 1");
    assert_eq!((w >> 1) & 0xFF, 0xAA);
}

#[test]
fn xt_frame_padding_all_ones() {
    // Bits 9..31 must be 1 so over-consumption idles HIGH.
    let w = pack_xt_frame(0x00);
    assert_eq!(w >> 9, 0x7F_FFFF);
}

#[test]
fn xt_frame_round_trip_layout() {
    // Confirm the bit layout for a few representative bytes.
    let cases = [0x00u8, 0xFF, 0x55, 0xAA, 0x1E];
    for &byte in &cases {
        let w = pack_xt_frame(byte);
        assert_eq!(w & 1, 1, "start = 1 for {:02X}", byte);
        assert_eq!((w >> 1) & 0xFF, byte as u32, "data round-trips for {:02X}", byte);
    }
}

// -------------------------------------------------------------------------
// Packer → Framer round-trip
//
// Walk the packed word LSB-first, emit (clk, data) edges that match what
// the firmware's PIO TX program would drive on the wire, and verify the
// Framer reconstructs the original byte.
// -------------------------------------------------------------------------

const BIT_HALF_US: u32 = 40;

fn drive_bits_from_word(framer: &mut Framer, word: u32, n_bits: u8, t0: u64) -> Option<Ps2Frame> {
    let mut t = t0;
    let mut frame = None;

    // Idle high before frame so the framer leaves Idle state cleanly.
    for _ in 0..100 {
        frame = frame.or(framer.ingest(true, true, t));
        t += 1;
    }

    for i in 0..n_bits {
        let bit = ((word >> i) & 1) != 0;
        // CLK low half: DATA is stable at the bit value.
        for _ in 0..BIT_HALF_US {
            frame = frame.or(framer.ingest(false, bit, t));
            t += 1;
        }
        // CLK high half: still holding the bit.
        for _ in 0..BIT_HALF_US {
            frame = frame.or(framer.ingest(true, bit, t));
            t += 1;
        }
    }

    // Idle gap to flush XT frames via timeout.
    for _ in 0..250 {
        frame = frame.or(framer.ingest(true, true, t));
        t += 1;
    }

    frame
}

#[test]
fn at_packer_framer_round_trip_exhaustive() {
    for byte in 0u8..=255 {
        let word = pack_at_frame(byte);
        let mut f = Framer::new();
        let frame = drive_bits_from_word(&mut f, word, 11, 0)
            .unwrap_or_else(|| panic!("byte {:02X}: framer did not emit", byte));
        assert_eq!(frame.kind, FrameKind::At, "byte {:02X}", byte);
        assert_eq!(frame.data, byte, "byte {:02X} round-trip mismatch", byte);
        assert!(frame.parity_ok, "byte {:02X}: parity check failed", byte);
        assert!(frame.framing_ok, "byte {:02X}: framing check failed", byte);
    }
}

#[test]
fn xt_packer_framer_round_trip_sample_bytes() {
    let cases = [0x00u8, 0x01, 0x55, 0xAA, 0xFF, 0x1E, 0x9C];
    for &byte in &cases {
        let word = pack_xt_frame(byte);
        let mut f = Framer::new();
        let frame = drive_bits_from_word(&mut f, word, 9, 0)
            .unwrap_or_else(|| panic!("byte {:02X}: XT framer did not emit", byte));
        assert_eq!(frame.kind, FrameKind::Xt, "byte {:02X}", byte);
        assert_eq!(frame.data, byte, "byte {:02X} round-trip mismatch", byte);
        assert!(frame.framing_ok, "byte {:02X}: framing check failed", byte);
    }
}
