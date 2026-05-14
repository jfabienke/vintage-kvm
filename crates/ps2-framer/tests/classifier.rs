//! Unit tests for the XT/AT/PS-2 classifier state machine.

use vintage_kvm_ps2_framer::classifier::{Classifier, Event, State, CONFIDENCE_THRESHOLD};
use vintage_kvm_ps2_framer::{FrameKind, FrameTiming, Ps2Frame};
use vintage_kvm_signatures::MachineClass;

fn frame(kind: FrameKind, parity_ok: bool, framing_ok: bool) -> Ps2Frame {
    Ps2Frame {
        kind,
        data: 0x1C,
        parity_ok,
        framing_ok,
        start_timestamp_us: 0,
        timing: FrameTiming::default(),
    }
}

fn good_at() -> Ps2Frame {
    frame(FrameKind::At, true, true)
}
fn good_xt() -> Ps2Frame {
    frame(FrameKind::Xt, true, true)
}

#[test]
fn unknown_until_threshold_reached() {
    let mut c = Classifier::new();
    assert_eq!(c.state(), State::Unknown);
    assert!(c.class().is_none());

    for i in 1..CONFIDENCE_THRESHOLD {
        let ev = c.ingest_kbd_frame(&good_at());
        assert!(ev.is_none(), "still candidate at streak {}", i);
        assert!(c.class().is_none());
    }

    let ev = c.ingest_kbd_frame(&good_at());
    assert_eq!(ev, Some(Event::Detected(MachineClass::At)));
    assert_eq!(c.class(), Some(MachineClass::At));
}

#[test]
fn xt_confirmation_after_three_frames() {
    let mut c = Classifier::new();
    let mut last = None;
    for _ in 0..CONFIDENCE_THRESHOLD {
        last = c.ingest_kbd_frame(&good_xt());
    }
    assert_eq!(last, Some(Event::Detected(MachineClass::Xt)));
    assert_eq!(c.class(), Some(MachineClass::Xt));
}

#[test]
fn contradictory_frame_during_candidate_restarts_streak() {
    let mut c = Classifier::new();
    c.ingest_kbd_frame(&good_at());
    c.ingest_kbd_frame(&good_at());
    assert!(matches!(c.state(), State::AtCandidate { streak: 2 }));

    // A clean XT frame mid-AT-candidate streak should drop us into the
    // XT candidate state with streak=1, not promote either side.
    let ev = c.ingest_kbd_frame(&good_xt());
    assert!(ev.is_none());
    assert!(matches!(c.state(), State::XtCandidate { streak: 1 }));
}

#[test]
fn confirmed_xt_then_at_traffic_resets_and_reclassifies() {
    let mut c = Classifier::new();
    for _ in 0..CONFIDENCE_THRESHOLD {
        c.ingest_kbd_frame(&good_xt());
    }
    assert_eq!(c.class(), Some(MachineClass::Xt));

    // Host warm-reset into AT-mode BIOS — first AT frame produces Reset
    // and starts the AT candidate streak.
    let ev = c.ingest_kbd_frame(&good_at());
    assert_eq!(ev, Some(Event::Reset));
    assert!(matches!(c.state(), State::AtCandidate { streak: 1 }));

    // Two more AT frames confirm AT.
    c.ingest_kbd_frame(&good_at());
    let ev = c.ingest_kbd_frame(&good_at());
    assert_eq!(ev, Some(Event::Detected(MachineClass::At)));
    assert_eq!(c.class(), Some(MachineClass::At));
}

#[test]
fn parity_error_frames_are_ignored() {
    let mut c = Classifier::new();
    let bad = frame(FrameKind::At, false, true);
    for _ in 0..10 {
        let ev = c.ingest_kbd_frame(&bad);
        assert!(ev.is_none());
    }
    assert_eq!(c.state(), State::Unknown);
}

#[test]
fn framing_error_frames_are_ignored() {
    let mut c = Classifier::new();
    let bad = frame(FrameKind::At, true, false);
    for _ in 0..10 {
        let ev = c.ingest_kbd_frame(&bad);
        assert!(ev.is_none());
    }
    assert_eq!(c.state(), State::Unknown);
}

#[test]
fn invalid_frame_kind_is_ignored() {
    let mut c = Classifier::new();
    let bad = frame(FrameKind::Invalid, false, false);
    for _ in 0..10 {
        let ev = c.ingest_kbd_frame(&bad);
        assert!(ev.is_none());
    }
    assert_eq!(c.state(), State::Unknown);
}

#[test]
fn aux_activity_promotes_confirmed_at_to_ps2() {
    let mut c = Classifier::new();
    for _ in 0..CONFIDENCE_THRESHOLD {
        c.ingest_kbd_frame(&good_at());
    }
    assert_eq!(c.class(), Some(MachineClass::At));

    let ev = c.ingest_aux_activity();
    assert_eq!(ev, Some(Event::Detected(MachineClass::Ps2)));
    assert_eq!(c.class(), Some(MachineClass::Ps2));
}

#[test]
fn aux_activity_before_at_confirmation_is_noop() {
    let mut c = Classifier::new();
    let ev = c.ingest_aux_activity();
    assert!(ev.is_none());
    assert_eq!(c.state(), State::Unknown);

    // Mid-candidate state shouldn't promote either.
    c.ingest_kbd_frame(&good_at());
    let ev = c.ingest_aux_activity();
    assert!(ev.is_none());
    assert!(matches!(c.state(), State::AtCandidate { streak: 1 }));
}

#[test]
fn aux_activity_after_xt_confirmation_is_noop() {
    let mut c = Classifier::new();
    for _ in 0..CONFIDENCE_THRESHOLD {
        c.ingest_kbd_frame(&good_xt());
    }
    let ev = c.ingest_aux_activity();
    assert!(ev.is_none());
    assert_eq!(c.class(), Some(MachineClass::Xt));
}

#[test]
fn ps2_state_is_sticky_against_extra_at_frames() {
    let mut c = Classifier::new();
    for _ in 0..CONFIDENCE_THRESHOLD {
        c.ingest_kbd_frame(&good_at());
    }
    c.ingest_aux_activity();
    assert_eq!(c.class(), Some(MachineClass::Ps2));

    let ev = c.ingest_kbd_frame(&good_at());
    assert!(ev.is_none());
    assert_eq!(c.class(), Some(MachineClass::Ps2));
}
