//! Protocol dispatcher.
//!
//! State machine + handlers for the packet commands Stage 1 v1.0 sends:
//! `CAP_REQ`, `CAP_ACK`, `PING`, `SEND_BLOCK`, `BLOCK_ACK`, `BLOCK_NAK`.
//!
//! Each `handle_packet` invocation is a small pure-state-transition function
//! that consumes one `IncomingPacket` and either:
//!   - emits a reply packet (`CAP_RSP`, `PONG`, `RECV_BLOCK`), or
//!   - silently advances internal state (`CAP_ACK`, `BLOCK_ACK`, `BLOCK_NAK`),
//!   - or logs an unknown / unexpected command for telemetry.
//!
//! No I/O here. The caller (`main` task) shuttles bytes between this and the
//! LPT phy.

pub mod block_server;
pub mod cap;
pub mod stage_blobs;

use crate::packet::{commands::*, encode, IncomingPacket, MAX_PACKET};
use block_server::{BlockServer, RECV_MAX_PAYLOAD};
use defmt::{debug, info, warn};
use heapless::Vec;

/// Session-scope state. Single instance owned by the dispatcher task.
pub struct SessionState {
    pub block_server: BlockServer,
    /// Next SEQ number to use for outgoing packets.
    pub tx_seq: u8,
    /// Expected SEQ number on the next incoming packet. Mismatch is logged
    /// but does not drop (Stage 1 retries via BLOCK_NAK).
    pub rx_seq_expected: u8,
    /// Has Stage 1 finished its CAP handshake? Used only for diagnostics
    /// (Stage 1 may send PING any time, so we don't strictly gate on this).
    pub cap_acked: bool,
}

impl SessionState {
    pub const fn new() -> Self {
        Self {
            block_server: BlockServer::new(),
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

/// Outcome of handling one incoming packet. The dispatcher returns either a
/// framed reply packet or a hint to take no action.
pub enum DispatchOutcome {
    /// Caller should send these bytes back to the host.
    Reply(Vec<u8, MAX_PACKET>),
    /// No reply for this command (e.g. `CAP_ACK`, `BLOCK_ACK`, `BLOCK_NAK`).
    Silent,
    /// Command was malformed or unknown; logged via defmt for diagnostics.
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

    let mut payload = [0u8; cap::PAYLOAD_LEN];
    let n = cap::build_cap_rsp_payload(&mut payload);
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
    let n = match state.block_server.build_recv_block(block_no, &mut payload) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{commands::*, decode, encode};

    fn wrap_request(cmd: u8, seq: u8, payload: &[u8]) -> IncomingPacket {
        let mut out = [0u8; MAX_PACKET];
        let n = encode(cmd, seq, payload, &mut out).unwrap();
        decode(&out[..n]).unwrap()
    }

    #[test]
    fn cap_req_produces_rsp_with_correct_size_and_crc() {
        let mut state = SessionState::new();
        let req = wrap_request(CMD_CAP_REQ, 0, &[]);
        let outcome = handle_packet(&req, &mut state);
        let bytes = match outcome {
            DispatchOutcome::Reply(b) => b,
            _ => panic!("expected reply"),
        };
        let rsp = decode(&bytes).unwrap();
        assert_eq!(rsp.cmd, CMD_CAP_RSP);
        assert_eq!(rsp.payload.len(), cap::PAYLOAD_LEN);
        assert_eq!(rsp.payload[0], cap::VERSION_MAJOR);
    }

    #[test]
    fn ping_echoes_payload_as_pong() {
        let mut state = SessionState::new();
        let req = wrap_request(CMD_PING, 0, b"hello");
        let outcome = handle_packet(&req, &mut state);
        let bytes = match outcome {
            DispatchOutcome::Reply(b) => b,
            _ => panic!("expected reply"),
        };
        let rsp = decode(&bytes).unwrap();
        assert_eq!(rsp.cmd, CMD_PONG);
        assert_eq!(rsp.payload.as_slice(), b"hello");
    }

    #[test]
    fn send_block_zero_returns_first_block() {
        let mut state = SessionState::new();
        let req = wrap_request(CMD_SEND_BLOCK, 0, &[0, 0, 0, 0]);
        let outcome = handle_packet(&req, &mut state);
        let bytes = match outcome {
            DispatchOutcome::Reply(b) => b,
            _ => panic!("expected reply"),
        };
        let rsp = decode(&bytes).unwrap();
        assert_eq!(rsp.cmd, CMD_RECV_BLOCK);
        // First 4 bytes echo block_no
        assert_eq!(&rsp.payload[..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn block_ack_advances_expected_block() {
        let mut state = SessionState::new();
        let req = wrap_request(CMD_BLOCK_ACK, 0, &[0, 0, 0, 0]);
        let outcome = handle_packet(&req, &mut state);
        assert!(matches!(outcome, DispatchOutcome::Silent));
        assert_eq!(state.block_server.expected_block, 1);
    }
}
