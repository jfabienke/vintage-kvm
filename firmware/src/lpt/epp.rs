//! IEEE 1284 EPP phy: combined fwd/rev on a single SM.
//!
//! [`super::pio_epp::PioEpp`] runs both data-cycle directions; SM1
//! sits idle in this mode but stays parked on the phy so the
//! `LptHardware` invariant (every phy owns SM0 + SM1 + DMA_CH4) is
//! preserved.
//!
//! ## Hardware prerequisites
//!
//! `LptMux::switch_to(Epp)` drives the 74LVC161284's HD (GP0) HIGH
//! and DIR (GP29) LOW (host-writes default) before invoking
//! [`EppPhy::build`]. Per-cycle DIR flipping for EPP read/write
//! interleaving is handled by [`super::pio_dir_follower`] on PIO0
//! SM2 — a one-instruction `mov pins, pins` mirror that drives DIR
//! from the host's `nWrite` line at ~20 ns latency. `EppPhy::build`
//! brings up that follower; `EppPhy::dismantle` tears it down and
//! hands DIR's FUNCSEL back to SIO.
//!
//! Address-cycle support (nAddrStb on nSelectIn / GP22) is not
//! implemented here; only data cycles. Stage 1's EPP driver does
//! not currently emit address cycles.

use defmt::trace;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::StateMachine;

use super::hardware::LptHardware;
use super::pio_dir_follower::PioDirFollower;
use super::pio_epp::PioEpp;
use super::{LptError, LptMode, LptPhy};

pub struct EppPhy {
    epp: PioEpp,
    /// SM2 + 1-instruction mirror loop that drives DIR (GP29) from
    /// the host's nWrite (GP11) at PIO clock speed. EPP-only — the
    /// follower owns DIR's FUNCSEL while this phy is alive and
    /// hands it back on dismantle.
    dir_follower: Option<PioDirFollower>,
    // SM1 stays held on the phy while EPP runs (one-SM mode), so we
    // return it cleanly on dismantle. SM1 is not running any program.
    parked_sm1: Option<StateMachine<'static, PIO0, 1>>,
}

impl EppPhy {
    pub fn build(hw: &mut LptHardware) -> Self {
        let sm0 = hw.parked_sm0.take().expect("parked_sm0 must be available");
        let sm1 = hw.parked_sm1.take().expect("parked_sm1 must be available");
        let sm2 = hw.parked_sm2.take().expect("parked_sm2 must be available");
        let dma = hw
            .parked_dma_ch4
            .take()
            .expect("parked_dma_ch4 must be available");
        let epp = PioEpp::new(&mut hw.common, sm0, dma, &hw.pins);
        // Bring up the DIR follower *after* the EPP SM is enabled so
        // the chip already sees a stable D-bus pattern when DIR
        // starts mirroring nWrite. Order doesn't matter for
        // correctness — both SMs run independently — but it keeps
        // the bring-up sequence easy to reason about.
        let dir_follower = PioDirFollower::new(&mut hw.common, sm2, &hw.pins);
        Self {
            epp,
            dir_follower: Some(dir_follower),
            parked_sm1: Some(sm1),
        }
    }

    pub async fn dismantle(mut self, hw: &mut LptHardware) {
        let sm2 = self
            .dir_follower
            .take()
            .expect("dir_follower must be populated")
            .dismantle(&mut hw.common);
        let (sm0, dma) = self.epp.dismantle(&mut hw.common).await;
        hw.parked_sm0 = Some(sm0);
        hw.parked_sm1 = self.parked_sm1.take();
        hw.parked_sm2 = Some(sm2);
        hw.parked_dma_ch4 = Some(dma);
    }
}

impl LptPhy for EppPhy {
    async fn recv_byte(&mut self) -> Result<u8, LptError> {
        // EPP forward: tell the SM to wait for the host write strobe,
        // then read the byte the SM captured.
        self.epp.send_forward_cmd().await;
        let b = self.epp.recv_byte().await;
        trace!("LPT[epp] recv 0x{:02X}", b);
        Ok(b)
    }

    async fn send_byte(&mut self, b: u8) -> Result<(), LptError> {
        self.epp.send_reverse_byte(b).await;
        trace!("LPT[epp] send 0x{:02X}", b);
        Ok(())
    }

    async fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), LptError> {
        // Per-byte loop is fine here: every byte needs its own
        // direction-tagged cmd word, and PioEpp's TX FIFO holds 4
        // entries deep so the CPU is rarely the bottleneck.
        for &b in bytes {
            self.epp.send_reverse_byte(b).await;
        }
        trace!("LPT[epp] send {}B", bytes.len());
        Ok(())
    }

    fn current_mode(&self) -> LptMode {
        LptMode::Epp
    }
}
