//! Session lifecycle (Phase 3 subset).
//!
//! The comprehensive state machine is described in
//! `docs/pico_firmware_design.md` §7. Phase 3 only needs three states:
//!
//! ```text
//! BOOT → SERVE_LPT → ERROR (recoverable)
//! ```
//!
//! Pre-Phase 1 (PS/2 / DEBUG injection) work, the firmware assumes the DOS
//! host already has Stage 0/1 loaded and is driving LPT traffic. We jump
//! straight into `SERVE_LPT`.

#[allow(dead_code)] // Wired in once we have a recoverable-error path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum Phase3State {
    Boot,
    ServeLpt,
    Error,
}
