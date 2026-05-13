//! Supervisor lifecycle state.
//!
//! Distinct from `protocol::SessionState` (which tracks per-packet dispatcher
//! state — seq numbers, CAP-acked flag, block server). This enum is the
//! coarse-grained visual lifecycle state surfaced through the NeoPixel
//! indicator and used by future telemetry; it is the "SessionState" that
//! `docs/pico_firmware_design.md` §7 and `docs/instrumentation_surface.md`
//! §4.7 refer to.
//!
//! Phase 3 only meaningfully reaches `Boot` → `ServeCapHandshake` →
//! `ServeStage2Download`. The pre-LPT states (host-power detect, machine-
//! class detect, DEBUG injection, Stage 0/1 download over PS/2) are
//! reserved for Phase 1; the post-handoff DP states are reserved for
//! Phase 4+.

use core::sync::atomic::{AtomicU8, Ordering};

/// Coarse-grained supervisor lifecycle. The NeoPixel task maps each state
/// to a color / blink pattern via `status::animations::for_state`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum SupervisorState {
    Boot = 0,
    Selftest = 1,
    AwaitHostPower = 2,
    DetectMachineClass = 3,
    InjectDebug = 4,
    ServeStage0Download = 5,
    ServeStage1Handoff = 6,
    ServeCapHandshake = 7,
    ServeStage2Download = 8,
    DpReady = 9,
    DpActive = 10,
    ErrorRecoverable = 11,
    FaultUnrecoverable = 12,
}

impl SupervisorState {
    /// Decode a value previously stored in `SUPERVISOR_STATE`. Any
    /// out-of-range value collapses to `FaultUnrecoverable` so a corrupted
    /// state byte fails visibly rather than silently masking as `Boot`.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Boot,
            1 => Self::Selftest,
            2 => Self::AwaitHostPower,
            3 => Self::DetectMachineClass,
            4 => Self::InjectDebug,
            5 => Self::ServeStage0Download,
            6 => Self::ServeStage1Handoff,
            7 => Self::ServeCapHandshake,
            8 => Self::ServeStage2Download,
            9 => Self::DpReady,
            10 => Self::DpActive,
            11 => Self::ErrorRecoverable,
            _ => Self::FaultUnrecoverable,
        }
    }
}

/// Global supervisor-state cell. Read by the NeoPixel task; written by
/// `main` and the dispatcher loop on state transitions. `Relaxed` is
/// sufficient — the NeoPixel only needs eventual visibility (refresh
/// cadence is tens of ms), and there is no other state ordered against it.
pub static SUPERVISOR_STATE: AtomicU8 = AtomicU8::new(SupervisorState::Boot as u8);

pub fn set(state: SupervisorState) {
    SUPERVISOR_STATE.store(state as u8, Ordering::Relaxed);
}

pub fn get() -> SupervisorState {
    SupervisorState::from_u8(SUPERVISOR_STATE.load(Ordering::Relaxed))
}
