//! Packet command IDs.
//!
//! Must match `dos/stage1/stage1.asm:106-117` byte-for-byte. Stage 1's
//! `packet_validate` reads `cmd` from offset 1 of the wire packet; the values
//! below appear on the wire exactly.

#![allow(dead_code)]

pub const CMD_CAP_REQ: u8 = 0x00;
pub const CMD_CAP_RSP: u8 = 0x0F;
pub const CMD_CAP_ACK: u8 = 0x0E;

pub const CMD_PING: u8 = 0x10;
pub const CMD_PONG: u8 = 0x11;

pub const CMD_ERROR: u8 = 0x13;
pub const CMD_ACK: u8 = 0x15;
pub const CMD_NAK: u8 = 0x16;

pub const CMD_SEND_BLOCK: u8 = 0x20;
pub const CMD_RECV_BLOCK: u8 = 0x21;
pub const CMD_BLOCK_ACK: u8 = 0x22;
pub const CMD_BLOCK_NAK: u8 = 0x23;
