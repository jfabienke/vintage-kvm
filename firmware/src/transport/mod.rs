//! Packet-level transport abstraction over the phy layer.
//!
//! `Transport` wraps a phy (LPT or PS/2) with packet semantics: encode →
//! send raw bytes → wait for an inbound packet → decode. The protocol
//! dispatcher consumes `IncomingPacket` and produces `OutgoingPacket`; the
//! transport handles every byte that lands on the wire in between.
//!
//! See `docs/firmware_crate_and_trait_design.md` §5 for the full design.

pub mod lpt;
pub mod packet_stream;

pub use lpt::LptTransport;

use vintage_kvm_protocol::{
    DecodeError, EncodeError, IncomingPacket, OutgoingPacket,
};
use vintage_kvm_telemetry_schema::{Plane, Port};

#[allow(dead_code)] // PhyTimeout + Decode constructed once timeouts + non-transparent decode failures land
#[derive(Debug, Clone, defmt::Format)]
pub enum TransportError {
    PhyTimeout,
    PhyHardware,
    Encode(EncodeError),
    /// A fully-framed packet was rejected; the reassembler resynced
    /// transparently and the caller can simply retry `recv_packet`.
    Decode(DecodeError),
}

impl From<EncodeError> for TransportError {
    fn from(e: EncodeError) -> Self {
        TransportError::Encode(e)
    }
}

/// Packet-level wire abstraction. Every concrete transport binds **one
/// plane** to **one port** at a time. Re-binding (e.g. control plane moves
/// from PS/2 KBD to LPT during XT fallback) is done by swapping the
/// transport instance, not by mutating it.
///
/// `plane()` / `port()` exist so the future `SessionSupervisor` can emit
/// `plane` telemetry events without the supervisor knowing the concrete
/// transport type.
#[allow(async_fn_in_trait, dead_code)] // single-impl-per-plane×port; no Send bound today
pub trait Transport {
    async fn send_packet(&mut self, p: &OutgoingPacket) -> Result<(), TransportError>;
    async fn recv_packet(&mut self) -> Result<IncomingPacket, TransportError>;
    fn plane(&self) -> Plane;
    fn port(&self) -> Port;
}

/// Send-only half — useful when the dispatcher only needs to write back.
#[allow(async_fn_in_trait, dead_code)]
pub trait PacketSink {
    async fn send(&mut self, p: &OutgoingPacket) -> Result<(), TransportError>;
}

/// Receive-only half — useful for fan-in / dispatcher loops.
#[allow(async_fn_in_trait, dead_code)]
pub trait PacketSource {
    async fn recv(&mut self) -> Result<IncomingPacket, TransportError>;
}

/// Any `Transport` is both a `PacketSink` and a `PacketSource`. Lets a
/// caller take only the half it needs without pulling the full trait.
impl<T: Transport> PacketSink for T {
    async fn send(&mut self, p: &OutgoingPacket) -> Result<(), TransportError> {
        self.send_packet(p).await
    }
}

impl<T: Transport> PacketSource for T {
    async fn recv(&mut self) -> Result<IncomingPacket, TransportError> {
        self.recv_packet().await
    }
}
