//! vintage-kvm CDC telemetry schema + emission trait.
//!
//! Events emitted by the firmware over the `events` CDC ACM interface
//! (`docs/usb_interface_design.md` §4.1), wire-encoded as postcard +
//! COBS-framed bytes by `firmware/src/usb/events.rs`. Consumed host-side
//! by `tools/events-consumer/` and (later) the TUI dashboard. Full
//! event taxonomy and command set:
//! [`docs/instrumentation_surface.md` §5](https://github.com/jfabienke/vintage-kvm/blob/master/docs/instrumentation_surface.md).
//!
//! The trait `TelemetryEmit` is the firmware-side abstraction: every layer
//! (phy, transport, dispatcher, session) takes an `&impl TelemetryEmit` and
//! calls `.emit(event)` at observation points. Concrete impls in the
//! firmware crate select the wire (defmt-RTT, CDC, both, or noop).
//!
//! Populated incrementally as each phase's emitter lands. Phase 3 emitters
//! cover boot, seq gap, unknown cmd, encode error; PS/2 (frame, stats,
//! anomaly, fingerprint) lands at Phase 1.
//!
//! ## Wire format
//!
//! postcard's externally-tagged enum representation: a varint variant
//! index followed by the variant's fields. Deliberately *not* using
//! `serde(tag = "kind")` — that forces a self-describing map shape and
//! postcard doesn't support `deserialize_any`, so round-trip wouldn't
//! work.
//!
//! ## Feature `serde`
//!
//! Enables `Serialize` derives only. `Deserialize` isn't derived here
//! because `Event::Boot` carries `&'static str` (firmware passes
//! program literals), which has no generic deserialize impl. Host
//! consumers mirror the schema with owned-string variants — see
//! `tools/events-consumer`.

#![no_std]

/// Stream-schema version. Bumped on **breaking** schema changes. New event
/// types and new fields can be added without bumping.
pub const SCHEMA_VERSION: u8 = 1;

/// Physical port a plane is bound to. Matches the `bound_to` JSON field.
/// Also used as the `ch` (channel) discriminator on per-channel events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Port {
    Kbd,
    Aux,
    Lpt,
}

/// Logical channel for the two-plane transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Plane {
    Control,
    Data,
}

/// Plane binding state for the `plane` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum PlaneState {
    Active,
    Idle,
    IdlePlanned,
    Degraded,
    Fallback,
    NotApplicable,
}

/// Telemetry events emitted by the firmware.
///
/// Variants are added per phase as their emitters land. Consumers ignore
/// unknown variants; producers may add variants without bumping
/// `SCHEMA_VERSION`. Only **breaking** changes (renaming a field, changing a
/// type) bump the version.
///
/// Lifetime: `'static` references only. Field types stay primitive so
/// embedded emission is alloc-free; richer string content uses
/// `heapless::String<N>` (added when needed).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Event {
    /// Firmware boot. `t` is seconds since power-on; `phase` is the
    /// design.md §22 phase number.
    Boot {
        fw_version: &'static str,
        phase: u8,
    },

    /// Stage-2-download lifecycle. `total_blocks` is `ceil(size / 64)`.
    DownloadBegin {
        total_blocks: u16,
        expected_crc32: u32,
        size_bytes: u32,
    },

    /// Per-block ACK from Stage 1.
    BlockAck {
        block_no: u16,
        running_crc32: u32,
    },

    /// Stage-2 download finished (or failed). `crc_match=false` means the
    /// host computed a different CRC than the Pico claimed.
    DownloadComplete {
        crc_match: bool,
        final_crc32: u32,
    },

    /// Receiver saw a SEQ that didn't match `rx_seq_expected`. Logged but
    /// doesn't drop the packet — Stage 1's retry logic handles re-sync.
    SeqGap {
        expected: u8,
        got: u8,
        cmd: u8,
    },

    /// Receiver saw a CMD byte we don't have a handler for.
    UnknownCmd { cmd: u8 },

    /// Packet-stream reassembler dropped a byte before SOH or rejected a
    /// fully-framed packet for CRC/ETX/length reasons.
    PacketStreamResync { reason: ResyncReason },

    /// `packet::encode` returned an error. Should never happen in practice
    /// (output buffer is statically MAX_PACKET-sized); logged for paranoia.
    EncodeError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ResyncReason {
    /// Byte received outside packet boundary (before SOH).
    PreSohByte,
    /// LEN field claimed a payload bigger than MAX_PAYLOAD.
    PayloadTooLong,
    /// CRC mismatch on a fully-framed packet.
    BadCrc,
    /// ETX byte was wrong.
    BadEtx,
    /// Other decode error (corrupt header, etc.).
    DecodeError,
}

/// Firmware-side emission trait. Every layer that observes events takes an
/// `&impl TelemetryEmit`. Implementations select the wire (defmt-RTT, CDC,
/// both, or noop).
///
/// `emit` is fire-and-forget — must not block, must not return errors
/// visible to the caller. Backpressure (CDC stall) is handled internally
/// in the concrete impl (drop-oldest policy on per-frame events).
pub trait TelemetryEmit {
    fn emit(&self, event: Event);
}
