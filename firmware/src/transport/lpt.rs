//! `LptTransport<P, T>` — packet-level transport over an `LptPhy`.
//!
//! Wraps a phy + a `PacketReassembler` for receive, and the packet encoder
//! for send. Generic over both the phy impl (Phase 3: `SppNibblePhyBitBang`;
//! Phase 4: any PIO impl that implements `LptPhy`) and the telemetry sink.

use crate::lpt::LptPhy;
use crate::telemetry::{Event, TelemetryEmit};
use crate::transport::{
    packet_stream::PacketReassembler, Transport, TransportError,
};
use vintage_kvm_protocol::{encode, IncomingPacket, OutgoingPacket, MAX_PACKET};
use vintage_kvm_telemetry_schema::{Plane, Port};

pub struct LptTransport<P: LptPhy, T: TelemetryEmit> {
    phy: P,
    reassembler: PacketReassembler,
    telemetry: T,
}

impl<P: LptPhy, T: TelemetryEmit> LptTransport<P, T> {
    /// Build a transport binding the data plane to LPT. Most Phase 3 work
    /// uses this; the control plane lives on PS/2 in steady state and gets
    /// its own transport.
    pub const fn new(phy: P, telemetry: T) -> Self {
        Self {
            phy,
            reassembler: PacketReassembler::new(),
            telemetry,
        }
    }
}

impl<P: LptPhy, T: TelemetryEmit> Transport for LptTransport<P, T> {
    async fn send_packet(&mut self, p: &OutgoingPacket) -> Result<(), TransportError> {
        let mut buf = [0u8; MAX_PACKET];
        let n = match encode(p.cmd, p.seq, &p.payload, &mut buf) {
            Ok(n) => n,
            Err(e) => {
                self.telemetry.emit(Event::EncodeError);
                return Err(TransportError::Encode(e));
            }
        };

        if self.phy.send_bytes(&buf[..n]).await.is_err() {
            return Err(TransportError::PhyHardware);
        }
        Ok(())
    }

    async fn recv_packet(&mut self) -> Result<IncomingPacket, TransportError> {
        Ok(self
            .reassembler
            .next_packet(&mut self.phy, &self.telemetry)
            .await)
    }

    fn plane(&self) -> Plane {
        Plane::Data
    }

    fn port(&self) -> Port {
        Port::Lpt
    }
}
