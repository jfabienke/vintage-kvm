//! Protocol dispatcher (firmware-side).
//!
//! Pure protocol logic (packet codec, CAP_RSP payload builder, block server,
//! command IDs) lives in the `vintage-kvm-protocol` crate. This module is the
//! firmware-specific glue: it wires the embedded Stage 2 blob into the
//! protocol-side block server, owns the session state, and emits defmt
//! diagnostics on every event.

pub mod stage_blobs;

use defmt::{debug, info, warn};
use heapless::Vec;

use vintage_kvm_protocol::{
    block_server::{BlockServer, RECV_MAX_PAYLOAD},
    cap::{build_cap_rsp_payload, ACTIVE_MODE_SPP, PAYLOAD_LEN as CAP_PAYLOAD_LEN},
    encode, IncomingPacket, MAX_PACKET,
    packet::commands::*,
};

use stage_blobs::EmbeddedStage2;

/// Session-scope state. Single instance owned by the dispatcher task.
pub struct SessionState {
    pub block_server: BlockServer,
    pub blob: EmbeddedStage2,
    /// Next SEQ number to use for outgoing packets.
    pub tx_seq: u8,
    /// Expected SEQ number on the next incoming packet. Mismatch is logged
    /// but does not drop (Stage 1 retries via BLOCK_NAK).
    pub rx_seq_expected: u8,
    /// Has Stage 1 finished its CAP handshake?
    pub cap_acked: bool,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            block_server: BlockServer::new(),
            blob: EmbeddedStage2::new(),
            tx_seq: 0,
            rx_seq_expected: 0,
            cap_acked: false,
        }
    }

    pub fn reset(&mut self) {
        self.block_server.reset();
        self.tx_seq = 0;
        self.rx_seq_expected = 0;
        self.cap_acked = false;
    }

    fn next_seq(&mut self) -> u8 {
        let s = self.tx_seq;
        self.tx_seq = self.tx_seq.wrapping_add(1);
        s
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of handling one incoming packet.
pub enum DispatchOutcome {
    /// Caller should send these bytes back to the host.
    Reply(Vec<u8, MAX_PACKET>),
    /// No reply for this command.
    Silent,
    /// Command was malformed or unknown; logged via defmt.
    Ignored,
}

pub fn handle_packet(p: &IncomingPacket, state: &mut SessionState) -> DispatchOutcome {
    if p.seq != state.rx_seq_expected {
        warn!(
            "seq gap: expected {}, got {} (cmd 0x{:02X})",
            state.rx_seq_expected, p.seq, p.cmd
        );
    }
    state.rx_seq_expected = p.seq.wrapping_add(1);

    match p.cmd {
        CMD_CAP_REQ => handle_cap_req(p, state),
        CMD_CAP_ACK => handle_cap_ack(p, state),
        CMD_PING => handle_ping(p, state),
        CMD_SEND_BLOCK => handle_send_block(p, state),
        CMD_BLOCK_ACK => handle_block_ack(p, state),
        CMD_BLOCK_NAK => handle_block_nak(p, state),
        other => {
            warn!("unknown cmd 0x{:02X}", other);
            DispatchOutcome::Ignored
        }
    }
}

fn handle_cap_req(p: &IncomingPacket, state: &mut SessionState) -> DispatchOutcome {
    if !p.payload.is_empty() {
        debug!("CAP_REQ has non-empty payload ({}); ignoring", p.payload.len());
    }
    info!("CAP_REQ received");
    // CAP_REQ resets the session as the bootstrap protocol contract.
    state.reset();

    let mut payload = [0u8; CAP_PAYLOAD_LEN];
    let n = build_cap_rsp_payload(&state.blob, ACTIVE_MODE_SPP, &mut payload);
    encode_reply(CMD_CAP_RSP, state.next_seq(), &payload[..n])
}

fn handle_cap_ack(_p: &IncomingPacket, state: &mut SessionState) -> DispatchOutcome {
    info!("CAP_ACK received; cleared for block download");
    state.cap_acked = true;
    DispatchOutcome::Silent
}

fn handle_ping(p: &IncomingPacket, state: &mut SessionState) -> DispatchOutcome {
    debug!("PING ({} B payload)", p.payload.len());
    encode_reply(CMD_PONG, state.next_seq(), &p.payload)
}

fn handle_send_block(p: &IncomingPacket, state: &mut SessionState) -> DispatchOutcome {
    let block_no = match BlockServer::parse_send_block(&p.payload) {
        Ok(n) => n,
        Err(e) => {
            warn!("SEND_BLOCK parse error: {}", e);
            return DispatchOutcome::Ignored;
        }
    };
    if block_no != state.block_server.expected_block {
        debug!(
            "SEND_BLOCK out-of-order: got {}, expected {} (Stage 1 retry)",
            block_no, state.block_server.expected_block
        );
    }

    let mut payload = [0u8; RECV_MAX_PAYLOAD];
    let n = match state.block_server.build_recv_block(&state.blob, block_no, &mut payload) {
        Ok(n) => n,
        Err(e) => {
            warn!("RECV_BLOCK build error: {}", e);
            return DispatchOutcome::Ignored;
        }
    };
    encode_reply(CMD_RECV_BLOCK, state.next_seq(), &payload[..n])
}

fn handle_block_ack(p: &IncomingPacket, state: &mut SessionState) -> DispatchOutcome {
    state.block_server.handle_ack(&p.payload);
    debug!("BLOCK_ACK; expected_block now {}", state.block_server.expected_block);
    DispatchOutcome::Silent
}

fn handle_block_nak(_p: &IncomingPacket, _state: &mut SessionState) -> DispatchOutcome {
    debug!("BLOCK_NAK; Stage 1 will SEND_BLOCK again");
    DispatchOutcome::Silent
}

fn encode_reply(cmd: u8, seq: u8, payload: &[u8]) -> DispatchOutcome {
    let mut out = [0u8; MAX_PACKET];
    match encode(cmd, seq, payload, &mut out) {
        Ok(n) => {
            let mut v: Vec<u8, MAX_PACKET> = Vec::new();
            v.extend_from_slice(&out[..n]).ok();
            DispatchOutcome::Reply(v)
        }
        Err(e) => {
            warn!("encode failed: {}", e);
            DispatchOutcome::Ignored
        }
    }
}
