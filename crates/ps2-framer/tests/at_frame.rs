//! Synthetic-wire tests for the framer state machine.
//!
//! Each test drives `Framer::ingest` with a sample sequence generated from
//! a high-level frame description. The simulator runs at 1 µs/sample and
//! uses an 80 µs nominal bit time (40 µs CLK low + 40 µs CLK high), which
//! sits in the middle of the spec'd 10–16.7 kHz range.

use vintage_kvm_ps2_framer::{FrameKind, Framer, Ps2Frame, GLITCH_THRESHOLD_US, IDLE_TIMEOUT_US};

const BIT_HALF_US: u32 = 40;
const POST_FRAME_IDLE_US: u32 = (IDLE_TIMEOUT_US + 50) as u32;

/// Compute odd-parity bit for AT/PS2 frame: parity = 1 iff data has even
/// number of 1s (so total ones across data+parity is odd).
fn odd_parity(byte: u8) -> u8 {
    if byte.count_ones() & 1 == 0 {
        1
    } else {
        0
    }
}

/// Drive an AT/PS2-shaped 11-bit frame: start(0) + 8 data LSB-first +
/// parity + stop(1). Returns the first frame the framer emits.
fn drive_at(framer: &mut Framer, byte: u8, t0: u64) -> (u64, Option<Ps2Frame>) {
    let bits = [
        0u8,
        (byte >> 0) & 1,
        (byte >> 1) & 1,
        (byte >> 2) & 1,
        (byte >> 3) & 1,
        (byte >> 4) & 1,
        (byte >> 5) & 1,
        (byte >> 6) & 1,
        (byte >> 7) & 1,
        odd_parity(byte),
        1,
    ];
    drive_frame(framer, &bits, t0)
}

/// Drive an XT-shaped 9-bit frame: start(1) + 8 data LSB-first.
fn drive_xt(framer: &mut Framer, byte: u8, t0: u64) -> (u64, Option<Ps2Frame>) {
    let bits = [
        1u8,
        (byte >> 0) & 1,
        (byte >> 1) & 1,
        (byte >> 2) & 1,
        (byte >> 3) & 1,
        (byte >> 4) & 1,
        (byte >> 5) & 1,
        (byte >> 6) & 1,
        (byte >> 7) & 1,
    ];
    drive_frame(framer, &bits, t0)
}

fn drive_frame(framer: &mut Framer, bits: &[u8], t0: u64) -> (u64, Option<Ps2Frame>) {
    let mut t = t0;
    let mut frame = None;

    // Idle high before frame.
    for _ in 0..100 {
        frame = frame.or(framer.ingest(true, true, t));
        t += 1;
    }

    // For each bit: CLK low for 40 µs with DATA at the bit value, then
    // CLK high for 40 µs.
    for &bit in bits {
        let data = bit != 0;
        for _ in 0..BIT_HALF_US {
            frame = frame.or(framer.ingest(false, data, t));
            t += 1;
        }
        for _ in 0..BIT_HALF_US {
            frame = frame.or(framer.ingest(true, data, t));
            t += 1;
        }
    }

    // Idle after frame to let timeout fire if the framer is waiting on
    // bit-count.
    for _ in 0..POST_FRAME_IDLE_US {
        frame = frame.or(framer.ingest(true, true, t));
        t += 1;
    }

    (t, frame)
}

// -------------------------------------------------------------------------
// AT / PS2 — 11-bit frames
// -------------------------------------------------------------------------

#[test]
fn at_round_trip_0x55() {
    let mut f = Framer::new();
    let (_, frame) = drive_at(&mut f, 0x55, 0);
    let frame = frame.expect("frame should emit");
    assert_eq!(frame.kind, FrameKind::At);
    assert_eq!(frame.data, 0x55);
    assert!(frame.parity_ok);
    assert!(frame.framing_ok);
    assert_eq!(frame.timing.glitch_count, 0);
}

#[test]
fn at_round_trip_0xaa() {
    let mut f = Framer::new();
    let (_, frame) = drive_at(&mut f, 0xAA, 0);
    let frame = frame.expect("frame should emit");
    assert_eq!(frame.data, 0xAA);
    assert!(frame.parity_ok);
    assert!(frame.framing_ok);
}

#[test]
fn at_round_trip_0xff() {
    let mut f = Framer::new();
    let (_, frame) = drive_at(&mut f, 0xFF, 0);
    let frame = frame.expect("frame should emit");
    assert_eq!(frame.data, 0xFF);
    assert!(frame.parity_ok);
    assert!(frame.framing_ok);
}

#[test]
fn at_round_trip_0x00() {
    let mut f = Framer::new();
    let (_, frame) = drive_at(&mut f, 0x00, 0);
    let frame = frame.expect("frame should emit");
    assert_eq!(frame.data, 0x00);
    assert!(frame.parity_ok);
    assert!(frame.framing_ok);
}

#[test]
fn at_scancode_set2_a_keycode_is_0x1c() {
    let mut f = Framer::new();
    let (_, frame) = drive_at(&mut f, 0x1C, 0);
    let frame = frame.expect("frame should emit");
    assert_eq!(frame.data, 0x1C);
    assert!(frame.parity_ok);
    assert!(frame.framing_ok);
}

#[test]
fn back_to_back_at_frames_both_emit() {
    let mut f = Framer::new();
    let (t, frame_a) = drive_at(&mut f, 0x12, 0);
    let (_, frame_b) = drive_at(&mut f, 0x34, t);
    assert_eq!(frame_a.unwrap().data, 0x12);
    assert_eq!(frame_b.unwrap().data, 0x34);
}

// -------------------------------------------------------------------------
// AT — error paths
// -------------------------------------------------------------------------

#[test]
fn at_bad_parity_is_flagged() {
    // Construct an 11-bit frame with the WRONG parity bit.
    let byte = 0x55u8;
    let wrong_parity = odd_parity(byte) ^ 1;
    let bits = [
        0u8,
        (byte >> 0) & 1,
        (byte >> 1) & 1,
        (byte >> 2) & 1,
        (byte >> 3) & 1,
        (byte >> 4) & 1,
        (byte >> 5) & 1,
        (byte >> 6) & 1,
        (byte >> 7) & 1,
        wrong_parity,
        1,
    ];
    let mut f = Framer::new();
    let (_, frame) = drive_frame(&mut f, &bits, 0);
    let frame = frame.expect("frame should still emit");
    assert_eq!(frame.data, byte);
    assert!(!frame.parity_ok, "parity_ok should be false");
    assert!(frame.framing_ok, "framing is intact");
}

#[test]
fn at_bad_stop_bit_is_flagged() {
    // Stop bit zero — framing error.
    let byte = 0x55u8;
    let bits = [
        0u8,
        (byte >> 0) & 1,
        (byte >> 1) & 1,
        (byte >> 2) & 1,
        (byte >> 3) & 1,
        (byte >> 4) & 1,
        (byte >> 5) & 1,
        (byte >> 6) & 1,
        (byte >> 7) & 1,
        odd_parity(byte),
        0, // bad stop
    ];
    let mut f = Framer::new();
    let (_, frame) = drive_frame(&mut f, &bits, 0);
    let frame = frame.expect("frame should emit");
    assert!(!frame.framing_ok);
}

// -------------------------------------------------------------------------
// XT — 9-bit frames (timeout-emitted)
// -------------------------------------------------------------------------

#[test]
fn xt_round_trip_0x55() {
    let mut f = Framer::new();
    let (_, frame) = drive_xt(&mut f, 0x55, 0);
    let frame = frame.expect("XT frame should emit on timeout");
    assert_eq!(frame.kind, FrameKind::Xt);
    assert_eq!(frame.data, 0x55);
    assert!(frame.framing_ok);
}

#[test]
fn xt_round_trip_0x1e_a_scancode_set1() {
    // Set 1 'A' = 0x1E.
    let mut f = Framer::new();
    let (_, frame) = drive_xt(&mut f, 0x1E, 0);
    let frame = frame.expect("XT frame should emit");
    assert_eq!(frame.data, 0x1E);
    assert!(frame.framing_ok);
}

// -------------------------------------------------------------------------
// Idle / partial frames
// -------------------------------------------------------------------------

#[test]
fn no_frame_when_clk_idle() {
    let mut f = Framer::new();
    let mut t = 0u64;
    let mut frame = None;
    for _ in 0..10_000 {
        frame = frame.or(f.ingest(true, true, t));
        t += 1;
    }
    assert!(frame.is_none());
}

#[test]
fn partial_frame_times_out_as_framing_error() {
    // Send only 5 bits then go idle — framer should emit a framing-error
    // frame so the dashboard observes the event.
    let bits = [0u8, 1, 0, 1, 1];
    let mut f = Framer::new();
    let (_, frame) = drive_frame(&mut f, &bits, 0);
    let frame = frame.expect("partial frame should still emit");
    assert_eq!(frame.kind, FrameKind::Invalid);
    assert!(!frame.framing_ok);
}

// -------------------------------------------------------------------------
// Glitch filter
// -------------------------------------------------------------------------

#[test]
fn short_clk_glitches_do_not_advance_bit_count() {
    // Walk a valid AT frame but interleave 1 µs CLK glitches between
    // real bit edges. The frame must still decode correctly and
    // glitch_count must be > 0.
    let byte = 0x55u8;
    let bits = [
        0u8,
        (byte >> 0) & 1,
        (byte >> 1) & 1,
        (byte >> 2) & 1,
        (byte >> 3) & 1,
        (byte >> 4) & 1,
        (byte >> 5) & 1,
        (byte >> 6) & 1,
        (byte >> 7) & 1,
        odd_parity(byte),
        1,
    ];

    let mut f = Framer::new();
    let mut t = 0u64;
    let mut frame = None;

    for _ in 0..100 {
        frame = frame.or(f.ingest(true, true, t));
        t += 1;
    }

    for &bit in &bits {
        let data = bit != 0;

        // CLK low half, with one 1 µs glitch HIGH 5 µs in.
        for k in 0..BIT_HALF_US {
            let clk = if k == 5 { true } else { false };
            frame = frame.or(f.ingest(clk, data, t));
            t += 1;
        }
        for _ in 0..BIT_HALF_US {
            frame = frame.or(f.ingest(true, data, t));
            t += 1;
        }
    }

    for _ in 0..POST_FRAME_IDLE_US {
        frame = frame.or(f.ingest(true, true, t));
        t += 1;
    }

    let frame = frame.expect("frame should still emit despite glitches");
    assert_eq!(frame.data, byte);
    assert!(frame.parity_ok);
    assert!(frame.framing_ok);
    // We inject 1 glitch per bit (the synthetic "HIGH for 1 µs in the
    // middle of CLK low") — should produce roughly 2 short transitions
    // per glitch (rising then falling).
    assert!(
        frame.timing.glitch_count > 0,
        "glitch_count should reflect injected noise, got {}",
        frame.timing.glitch_count
    );
}

#[test]
fn glitch_threshold_constant_is_4us() {
    // Documentation guard — the design picks 4 µs deliberately.
    assert_eq!(GLITCH_THRESHOLD_US, 4);
}
