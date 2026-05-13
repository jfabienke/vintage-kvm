//! vintage-kvm wire protocol.
//!
//! `no_std + no_alloc` — builds on the cortex-m firmware target and on any
//! host target for unit testing and the TUI dashboard. Hardware-specific
//! impls (DMA-sniffer-backed CRC, etc.) live in the firmware crate and
//! plug into the traits defined here.
//!
//! Design: [`docs/firmware_crate_and_trait_design.md`](https://github.com/jfabienke/vintage-kvm/blob/master/docs/firmware_crate_and_trait_design.md).

#![no_std]

pub mod block_server;
pub mod block_source;
pub mod cap;
pub mod crc;
pub mod packet;

pub use block_server::{BlockError, BlockServer, BLOCK_SIZE, RECV_HDR_LEN, RECV_MAX_PAYLOAD};
pub use block_source::{BlockSource, SliceBlob};
pub use crc::{Crc16Engine, Crc32Engine, SoftwareCrc16Ccitt, SoftwareCrc32Reflected};
pub use packet::{
    commands, crc16, crc32, decode, encode, DecodeError, EncodeError, IncomingPacket,
    HEADER_LEN, MAX_PACKET, MAX_PAYLOAD, OVERHEAD, SOH, TRAILER_LEN, ETX,
};
