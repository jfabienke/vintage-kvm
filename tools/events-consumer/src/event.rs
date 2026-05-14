//! Host-side mirror of `vintage_kvm_telemetry_schema::Event`.
//!
//! The schema crate is `no_std` and uses `&'static str` for fields the
//! firmware passes as program literals (`Event::Boot { fw_version }`).
//! `&'static str` can't be deserialized generically, so this host
//! mirror swaps it for `String`. Variant declaration order MUST match
//! the schema crate exactly — postcard encodes the variant as a varint
//! index, so re-ordering or inserting variants here without matching
//! the firmware silently mis-decodes future events.

// Fields are read by the `Debug` impl when we print, but the dead-code
// lint doesn't count derive-driven uses. Suppress at module scope.
#![allow(dead_code)]

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub enum Event {
    Boot {
        fw_version: String,
        phase: u8,
    },
    DownloadBegin {
        total_blocks: u16,
        expected_crc32: u32,
        size_bytes: u32,
    },
    BlockAck {
        block_no: u16,
        running_crc32: u32,
    },
    DownloadComplete {
        crc_match: bool,
        final_crc32: u32,
    },
    SeqGap {
        expected: u8,
        got: u8,
        cmd: u8,
    },
    UnknownCmd {
        cmd: u8,
    },
    PacketStreamResync {
        reason: ResyncReason,
    },
    EncodeError,
}

#[derive(Debug, Deserialize)]
pub enum ResyncReason {
    PreSohByte,
    PayloadTooLong,
    BadCrc,
    BadEtx,
    DecodeError,
}
