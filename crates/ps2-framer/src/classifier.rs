//! XT / AT / PS-2 host classifier.
//!
//! Consumes a stream of [`Ps2Frame`]s from the framer and a separate
//! "AUX channel produced traffic" signal, and converges on a
//! [`MachineClass`]. Direct implementation of the state machine in
//! `docs/pio_state_machines_design.md` §8.

use vintage_kvm_signatures::MachineClass;

use crate::{FrameKind, Ps2Frame};

/// Number of consecutive same-kind frames required before tentative
/// classification flips to `Confirmed`. Design doc §8.2.
pub const CONFIDENCE_THRESHOLD: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum State {
    Unknown,
    XtCandidate { streak: u8 },
    AtCandidate { streak: u8 },
    Confirmed(MachineClass),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Event {
    /// Tentative classification has crossed the confidence threshold.
    Detected(MachineClass),
    /// A contradictory frame reset the classifier from `Confirmed` (or
    /// from `*Candidate`) back to `Unknown`. The dashboard should treat
    /// this as a re-classification in progress.
    Reset,
}

#[derive(Debug, Clone, Copy)]
pub struct Classifier {
    state: State,
}

impl Classifier {
    pub const fn new() -> Self {
        Self {
            state: State::Unknown,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn class(&self) -> Option<MachineClass> {
        match self.state {
            State::Confirmed(c) => Some(c),
            _ => None,
        }
    }

    /// Feed one frame from the keyboard channel. Returns an `Event` if
    /// the classification changed in a way the supervisor cares about.
    ///
    /// Invalid frames (`FrameKind::Invalid`) and frames with bad
    /// parity/framing are *ignored* — they don't reset, they don't
    /// advance. The classifier is robust to a noisy wire by design.
    pub fn ingest_kbd_frame(&mut self, frame: &Ps2Frame) -> Option<Event> {
        let usable = frame.framing_ok && frame.parity_ok && frame.kind != FrameKind::Invalid;
        if !usable {
            return None;
        }
        match frame.kind {
            FrameKind::At => self.observe_at(),
            FrameKind::Xt => self.observe_xt(),
            FrameKind::Invalid => None,
        }
    }

    /// Signal that the AUX channel produced a valid frame. Only meaningful
    /// once the keyboard channel has been confirmed as AT — at that point
    /// AUX traffic promotes the classification to PS-2. No-op otherwise.
    pub fn ingest_aux_activity(&mut self) -> Option<Event> {
        if matches!(self.state, State::Confirmed(MachineClass::At)) {
            self.state = State::Confirmed(MachineClass::Ps2);
            return Some(Event::Detected(MachineClass::Ps2));
        }
        None
    }

    fn observe_at(&mut self) -> Option<Event> {
        match self.state {
            State::Unknown => {
                self.state = State::AtCandidate { streak: 1 };
                None
            }
            State::AtCandidate { streak } => {
                let streak = streak.saturating_add(1);
                if streak >= CONFIDENCE_THRESHOLD {
                    self.state = State::Confirmed(MachineClass::At);
                    Some(Event::Detected(MachineClass::At))
                } else {
                    self.state = State::AtCandidate { streak };
                    None
                }
            }
            State::XtCandidate { .. } => {
                // Contradictory mid-classification. Restart with this
                // frame as the first AT candidate.
                self.state = State::AtCandidate { streak: 1 };
                None
            }
            State::Confirmed(MachineClass::Xt) => {
                // The host changed beneath us (warm-reset into AT-mode
                // BIOS, for example). Reset and re-classify.
                self.state = State::AtCandidate { streak: 1 };
                Some(Event::Reset)
            }
            State::Confirmed(MachineClass::At) | State::Confirmed(MachineClass::Ps2) => None,
        }
    }

    fn observe_xt(&mut self) -> Option<Event> {
        match self.state {
            State::Unknown => {
                self.state = State::XtCandidate { streak: 1 };
                None
            }
            State::XtCandidate { streak } => {
                let streak = streak.saturating_add(1);
                if streak >= CONFIDENCE_THRESHOLD {
                    self.state = State::Confirmed(MachineClass::Xt);
                    Some(Event::Detected(MachineClass::Xt))
                } else {
                    self.state = State::XtCandidate { streak };
                    None
                }
            }
            State::AtCandidate { .. } => {
                self.state = State::XtCandidate { streak: 1 };
                None
            }
            State::Confirmed(MachineClass::At) | State::Confirmed(MachineClass::Ps2) => {
                self.state = State::XtCandidate { streak: 1 };
                Some(Event::Reset)
            }
            State::Confirmed(MachineClass::Xt) => None,
        }
    }
}

impl Default for Classifier {
    fn default() -> Self {
        Self::new()
    }
}
