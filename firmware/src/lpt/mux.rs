//! Active-phy multiplexer for IEEE 1284 mode switching.
//!
//! Implements the lifecycle from `docs/pio_state_machines_design.md`
//! §10.6: at any moment exactly one mode-specific phy owns the PIO0
//! state machines and DMA channels; mode transitions go through
//! drain → disable → free instr memory → reload → reconfigure.
//!
//! Mode coverage:
//! - `SppNibble` — bootstrap default, runs against real DOS hardware.
//! - `Byte`, `Epp`, `Ecp` — phys built and lifecycle-tested, but
//!   meaningful only once the 74LVC161284 mode pins and the host-side
//!   1284 negotiator land (see each phy's module docstring).
//! - `Spp` (forward-only), `EcpDma` — no phy yet; switch_to returns
//!   [`LptError::ModeMismatch`] and reverts to `SppNibble`.

use defmt::trace;

use super::byte::BytePhy;
use super::compat::SppNibblePhy;
use super::ecp::EcpPhy;
use super::epp::EppPhy;
use super::hardware::LptHardware;
use super::{LptError, LptMode, LptPhy};

/// Currently-active phy. Variant discriminator matches the
/// [`LptMode`] the phy implements.
enum ActivePhy {
    SppNibble(SppNibblePhy),
    Byte(BytePhy),
    Epp(EppPhy),
    Ecp(EcpPhy),
}

pub struct LptMux {
    hw: LptHardware,
    active: Option<ActivePhy>,
}

impl LptMux {
    /// Build the mux starting in SPP-nibble mode (the boot default,
    /// per `docs/two_plane_transport.md` and the Phase 3 design).
    pub fn new(mut hw: LptHardware) -> Self {
        let phy = SppNibblePhy::build(&mut hw);
        defmt::info!("lpt mux: initial mode = SppNibble");
        Self {
            hw,
            active: Some(ActivePhy::SppNibble(phy)),
        }
    }

    pub fn current_mode(&self) -> LptMode {
        match self.active.as_ref().expect("phy slot must be populated") {
            ActivePhy::SppNibble(_) => LptMode::SppNibble,
            ActivePhy::Byte(_) => LptMode::Byte,
            ActivePhy::Epp(_) => LptMode::Epp,
            ActivePhy::Ecp(_) => LptMode::Ecp,
        }
    }

    /// Transition to `target`. Drains and dismantles the current phy,
    /// then builds the target. Returns [`LptError::ModeMismatch`] for
    /// modes that don't yet have a phy implementation; the SppNibble
    /// phy is rebuilt in that case so the transport stays operational.
    pub async fn switch_to(&mut self, target: LptMode) -> Result<(), LptError> {
        if target == self.current_mode() {
            trace!("lpt mux: self-reload {}", target);
        } else {
            trace!("lpt mux: switching {} -> {}", self.current_mode(), target);
        }

        // Take the active phy out — dismantle requires `self.hw`
        // mutably and consumes the phy.
        let old = self.active.take().expect("phy slot must be populated");
        match old {
            ActivePhy::SppNibble(phy) => phy.dismantle(&mut self.hw).await,
            ActivePhy::Byte(phy) => phy.dismantle(&mut self.hw).await,
            ActivePhy::Epp(phy) => phy.dismantle(&mut self.hw).await,
            ActivePhy::Ecp(phy) => phy.dismantle(&mut self.hw).await,
        }

        // Pre-build hook: drive the 74LVC161284's DIR/HD inputs to
        // whatever `target` needs so the chip's data-bus direction
        // and driver style are correct *before* the new phy's SMs
        // start clocking. See `docs/hardware_reference.md` §11.3 for
        // the per-mode (DIR, HD) values. Modes that have no phy fall
        // back to Compat values — see the SppNibble revert below.
        let phy_target = match target {
            LptMode::Spp | LptMode::EcpDma => LptMode::SppNibble,
            other => other,
        };
        self.hw.set_transceiver_mode(phy_target);

        match target {
            LptMode::SppNibble => {
                let phy = SppNibblePhy::build(&mut self.hw);
                self.active = Some(ActivePhy::SppNibble(phy));
                Ok(())
            }
            LptMode::Byte => {
                let phy = BytePhy::build(&mut self.hw);
                self.active = Some(ActivePhy::Byte(phy));
                Ok(())
            }
            LptMode::Epp => {
                let phy = EppPhy::build(&mut self.hw);
                self.active = Some(ActivePhy::Epp(phy));
                Ok(())
            }
            LptMode::Ecp => {
                let phy = EcpPhy::build(&mut self.hw);
                self.active = Some(ActivePhy::Ecp(phy));
                Ok(())
            }
            LptMode::Spp | LptMode::EcpDma => {
                defmt::warn!(
                    "lpt mux: target {} has no phy yet; reverting to SppNibble",
                    target
                );
                let phy = SppNibblePhy::build(&mut self.hw);
                self.active = Some(ActivePhy::SppNibble(phy));
                Err(LptError::ModeMismatch)
            }
        }
    }
}

impl LptPhy for LptMux {
    async fn recv_byte(&mut self) -> Result<u8, LptError> {
        match self.active.as_mut().expect("phy slot must be populated") {
            ActivePhy::SppNibble(p) => p.recv_byte().await,
            ActivePhy::Byte(p) => p.recv_byte().await,
            ActivePhy::Epp(p) => p.recv_byte().await,
            ActivePhy::Ecp(p) => p.recv_byte().await,
        }
    }

    async fn send_byte(&mut self, b: u8) -> Result<(), LptError> {
        match self.active.as_mut().expect("phy slot must be populated") {
            ActivePhy::SppNibble(p) => p.send_byte(b).await,
            ActivePhy::Byte(p) => p.send_byte(b).await,
            ActivePhy::Epp(p) => p.send_byte(b).await,
            ActivePhy::Ecp(p) => p.send_byte(b).await,
        }
    }

    async fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), LptError> {
        match self.active.as_mut().expect("phy slot must be populated") {
            ActivePhy::SppNibble(p) => p.send_bytes(bytes).await,
            ActivePhy::Byte(p) => p.send_bytes(bytes).await,
            ActivePhy::Epp(p) => p.send_bytes(bytes).await,
            ActivePhy::Ecp(p) => p.send_bytes(bytes).await,
        }
    }

    fn current_mode(&self) -> LptMode {
        LptMux::current_mode(self)
    }
}
