//! IEEE 1284 byte-mode phy: SPP-compat forward + PIO byte reverse.
//!
//! Forward direction (host → Pico) reuses [`super::pio_compat_in`]
//! verbatim — byte mode's forward handshake is bit-identical to
//! Centronics / SPP. Reverse direction (Pico → host) drives the full
//! 8-bit data bus per cycle via [`super::pio_byte_rev`].
//!
//! ## Hardware prerequisites
//!
//! `LptMux::switch_to(Byte)` drives the 74LVC161284's HD (GP0) HIGH
//! before invoking [`BytePhy::build`] so the cable-side outputs use
//! totem-pole drivers (per `docs/hardware_reference.md` §11.3). DIR
//! (GP29) starts LOW (forward); the byte_rev sub-phase flips it via
//! `LptHardware::set_data_direction(true)` when the host hands the
//! reverse channel to the peripheral. That phase-change wiring is
//! still TODO — until it lands, `send_byte` works only when the
//! board's mode-pin defaults already select reverse.

use defmt::trace;

use super::hardware::LptHardware;
use super::pio_byte_rev::PioByteRev;
use super::pio_compat_in::PioCompatIn;
use super::{LptError, LptMode, LptPhy};

pub struct BytePhy {
    compat_in: PioCompatIn,
    byte_rev: PioByteRev,
}

impl BytePhy {
    pub fn build(hw: &mut LptHardware) -> Self {
        let sm0 = hw
            .parked_sm0
            .take()
            .expect("LptHardware::parked_sm0 must be available");
        let sm1 = hw
            .parked_sm1
            .take()
            .expect("LptHardware::parked_sm1 must be available");
        let dma = hw
            .parked_dma_ch4
            .take()
            .expect("LptHardware::parked_dma_ch4 must be available");
        let compat_in = PioCompatIn::new(&mut hw.common, sm0, &hw.pins);
        let byte_rev = PioByteRev::new(&mut hw.common, sm1, dma, &hw.pins);
        Self {
            compat_in,
            byte_rev,
        }
    }

    pub async fn dismantle(self, hw: &mut LptHardware) {
        let (sm1, dma) = self.byte_rev.dismantle(&mut hw.common).await;
        let sm0 = self.compat_in.dismantle(&mut hw.common);
        hw.parked_sm0 = Some(sm0);
        hw.parked_sm1 = Some(sm1);
        hw.parked_dma_ch4 = Some(dma);
    }
}

impl LptPhy for BytePhy {
    async fn recv_byte(&mut self) -> Result<u8, LptError> {
        let b = self.compat_in.recv_byte().await;
        trace!("LPT[byte] recv 0x{:02X}", b);
        Ok(b)
    }

    async fn send_byte(&mut self, b: u8) -> Result<(), LptError> {
        self.byte_rev.send_byte(b).await;
        trace!("LPT[byte] send 0x{:02X}", b);
        Ok(())
    }

    async fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), LptError> {
        self.byte_rev.send_bytes(bytes).await;
        trace!("LPT[byte] send {}B", bytes.len());
        Ok(())
    }

    fn current_mode(&self) -> LptMode {
        LptMode::Byte
    }
}
