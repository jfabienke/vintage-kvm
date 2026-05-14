//! PS/2 supervisor task.
//!
//! Owns the *shared* [`Classifier`] and bridges the KBD and AUX
//! oversamplers. Each oversampler's drain loop publishes events into
//! channels owned by this module; the supervisor consumes them and
//! drives the classifier state machine.
//!
//! Decoupling rationale: the KBD classifier and AUX classifier are
//! conceptually one state machine — only AUX *activity* (not its
//! framing) promotes `Confirmed(At) → Confirmed(Ps2)`. Each oversampler
//! shouldn't know the other exists; the supervisor is the single
//! consumer that ties them together.
//!
//! ## Phase 1 scope
//!
//! - Receive KBD frames; feed [`Classifier::ingest_kbd_frame`].
//! - Receive AUX activity signal; feed [`Classifier::ingest_aux_activity`].
//! - Log classifier transitions via defmt.
//!
//! Does *not* yet write to `lifecycle::SUPERVISOR_STATE` — the Phase 3
//! LPT serve loop still owns that. Once PS/2 bootstrap injection lands,
//! this task will drive the lifecycle transitions (DetectMachineClass
//! → InjectDebug → ServeStage0Download → ...).

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use vintage_kvm_ps2_framer::{Classifier, ClassifierEvent, Ps2Frame};
use vintage_kvm_signatures::MachineClass;

use crate::lifecycle::{self, SupervisorState};

/// Inbox for KBD frames from the KBD oversampler drain loop. Depth 8
/// gives ~5 ms of slack at the worst-case ~1.5 kHz frame rate before
/// `try_send` starts dropping events; the classifier doesn't need
/// every frame to converge so dropping is acceptable backpressure.
pub static KBD_FRAMES: Channel<CriticalSectionRawMutex, Ps2Frame, 8> = Channel::new();

/// Edge-triggered AUX activity. The AUX drain loop calls `.signal(())`
/// on any well-formed frame; the supervisor consumes the signal and
/// only really cares about the first one after Confirmed(At).
pub static AUX_ACTIVITY: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Fires once per Confirmed classification. The bootstrap injector
/// task waits on this and starts typing the DEBUG script for the
/// detected class.
pub static INJECT_TRIGGER: Signal<CriticalSectionRawMutex, MachineClass> = Signal::new();

#[embassy_executor::task]
pub async fn run() {
    // Don't seed `SupervisorState::DetectMachineClass` here — Phase 3
    // currently boots straight into the LPT serve loop (lifecycle set
    // by main), and overwriting it would flicker the indicator. The
    // injector takes over the lifecycle on `Detected`.
    let mut classifier = Classifier::new();
    defmt::info!("ps2 supervisor: running");

    loop {
        match select(KBD_FRAMES.receive(), AUX_ACTIVITY.wait()).await {
            Either::First(frame) => {
                if let Some(ev) = classifier.ingest_kbd_frame(&frame) {
                    handle_event(ev);
                }
            }
            Either::Second(()) => {
                AUX_ACTIVITY.reset();
                if let Some(ev) = classifier.ingest_aux_activity() {
                    handle_event(ev);
                }
            }
        }
    }
}

fn handle_event(ev: ClassifierEvent) {
    match ev {
        ClassifierEvent::Detected(class) => {
            defmt::info!("ps2 classifier: Detected({})", class);
            // Hand off to the injector task. Idempotent: if the
            // injector hasn't consumed the previous signal yet, the
            // new value overwrites it.
            INJECT_TRIGGER.signal(class);
        }
        ClassifierEvent::Reset => {
            defmt::info!("ps2 classifier: Reset (host re-classifying)");
            // Only roll the indicator back if we previously promoted
            // past detect (i.e., the injector or its successor states).
            // For Phase 3 where lifecycle stays at LPT states, this is
            // a no-op.
            let cur = lifecycle::get();
            if matches!(
                cur,
                SupervisorState::InjectDebug
                    | SupervisorState::ServeStage0Download
                    | SupervisorState::ServeStage1Handoff
            ) {
                lifecycle::set(SupervisorState::DetectMachineClass);
            }
        }
    }
}
