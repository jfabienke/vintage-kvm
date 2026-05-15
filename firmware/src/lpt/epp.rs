//! IEEE 1284 EPP phy: combined fwd/rev on a single SM.
//!
//! [`super::pio_epp::PioEpp`] runs both data-cycle directions; SM1
//! sits idle in this mode but stays parked on the phy so the
//! `LptHardware` invariant (every phy owns SM0 + SM1 + DMA_CH4) is
//! preserved.
//!
//! ## Hardware prerequisites
//!
//! EPP needs the 74LVC161284 set to `HD=H` (totem-pole outputs) and
//! `DIR` flipping per bus cycle (`L` for host writes, `H` for host
//! reads) — see `docs/hardware_reference.md` §11.3. Per-cycle DIR
//! flipping from the CPU can't meet the ~500 ns EPP cycle time;
//! the production path is a small PIO "DIR follower" SM that drives
//! GP29 (DIR) from the host's `nWrite` line. Until that lands,
//! building this phy is allowed (lifecycle is correct) but the wire
//! data direction won't follow the host's intent at speed.
//!
//! Address-cycle support (nAddrStb on nSelectIn / GP22) is not
//! implemented here; only data cycles. Stage 1's EPP driver does
//! not currently emit address cycles.

use defmt::trace;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::StateMachine;

use super::hardware::LptHardware;
use super::pio_epp::PioEpp;
use super::{LptError, LptMode, LptPhy};

pub struct EppPhy {
    epp: PioEpp,
    // SM1 stays held on the phy while EPP runs (one-SM mode), so we
    // return it cleanly on dismantle. SM1 is not running any program.
    parked_sm1: Option<StateMachine<'static, PIO0, 1>>,
}

impl EppPhy {
    pub fn build(hw: &mut LptHardware) -> Self {
        let sm0 = hw.parked_sm0.take().expect("parked_sm0 must be available");
        let sm1 = hw.parked_sm1.take().expect("parked_sm1 must be available");
        let dma = hw
            .parked_dma_ch4
            .take()
            .expect("parked_dma_ch4 must be available");
        let epp = PioEpp::new(&mut hw.common, sm0, dma, &hw.pins);
        Self {
            epp,
            parked_sm1: Some(sm1),
        }
    }

    pub async fn dismantle(mut self, hw: &mut LptHardware) {
        let (sm0, dma) = self.epp.dismantle(&mut hw.common).await;
        hw.parked_sm0 = Some(sm0);
        hw.parked_sm1 = self.parked_sm1.take();
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
