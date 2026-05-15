//! IEEE 1284 ECP phy: PIO forward + PIO reverse on separate SMs.
//!
//! Forward (host → Pico) on SM0 via [`super::pio_ecp::PioEcpFwd`];
//! reverse (Pico → host) on SM1 via [`super::pio_ecp::PioEcpRev`].
//! ECP is fundamentally half-duplex on the wire — direction is
//! negotiated and stays put for a burst — but both SMs run
//! concurrently and the protocol layer alternates which one to feed
//! based on the negotiated direction.
//!
//! ## Hardware prerequisites
//!
//! ECP needs the 74LVC161284 set to `HD=H` (totem-pole), with
//! `DIR` driven by the current burst direction: `L` for forward
//! bursts (host writes), `H` for reverse bursts (peripheral writes).
//! DIR is *stable for the whole burst* unlike EPP, so the CPU-driven
//! setter in `LptMux::switch_to(Ecp)` is sufficient. See
//! `docs/hardware_reference.md` §11.3 for the function table and
//! pin assignments.
//!
//! ## Command/data flag (HostAck)
//!
//! ECP encodes a per-byte command/data flag on HostAck. This phy
//! captures only the 9-bit (HostClk + D0..D7) word in the RX FIFO;
//! HostAck is read from GPIO when the CPU consumes each entry.
//! [`Self::recv_byte`] returns only the data byte; an "ECP-aware"
//! caller that needs the flag will read `gpio::Input::is_high()` on
//! GP20 between successive `recv_byte` calls.

use defmt::trace;

use super::hardware::LptHardware;
use super::pio_ecp::{PioEcpFwd, PioEcpRev};
use super::{LptError, LptMode, LptPhy};

pub struct EcpPhy {
    fwd: PioEcpFwd,
    rev: PioEcpRev,
}

impl EcpPhy {
    pub fn build(hw: &mut LptHardware) -> Self {
        let sm0 = hw.parked_sm0.take().expect("parked_sm0 must be available");
        let sm1 = hw.parked_sm1.take().expect("parked_sm1 must be available");
        let dma = hw
            .parked_dma_ch4
            .take()
            .expect("parked_dma_ch4 must be available");
        let fwd = PioEcpFwd::new(&mut hw.common, sm0, &hw.pins);
        let rev = PioEcpRev::new(&mut hw.common, sm1, dma, &hw.pins);
        Self { fwd, rev }
    }

    pub async fn dismantle(self, hw: &mut LptHardware) {
        let (sm1, dma) = self.rev.dismantle(&mut hw.common).await;
        let sm0 = self.fwd.dismantle(&mut hw.common);
        hw.parked_sm0 = Some(sm0);
        hw.parked_sm1 = Some(sm1);
        hw.parked_dma_ch4 = Some(dma);
    }
}

impl LptPhy for EcpPhy {
    async fn recv_byte(&mut self) -> Result<u8, LptError> {
        let b = self.fwd.recv_byte().await;
        trace!("LPT[ecp] recv 0x{:02X}", b);
        Ok(b)
    }

    async fn send_byte(&mut self, b: u8) -> Result<(), LptError> {
        self.rev.send_byte(b).await;
        trace!("LPT[ecp] send 0x{:02X}", b);
        Ok(())
    }

    async fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), LptError> {
        self.rev.send_bytes(bytes).await;
        trace!("LPT[ecp] send {}B", bytes.len());
        Ok(())
    }

    fn current_mode(&self) -> LptMode {
        LptMode::Ecp
    }
}
