//! `DefmtEmit` — writes telemetry events to defmt-RTT.
//!
//! One line per event, formatted to roughly match the on-the-wire CDC text
//! format from `instrumentation_surface.md` §3. The timestamp comes from
//! defmt's own `defmt-timestamp-uptime` feature; we don't repeat it here.

use defmt::{debug, info, warn};
use vintage_kvm_telemetry_schema::{Event, TelemetryEmit};

pub struct DefmtEmit;

impl TelemetryEmit for DefmtEmit {
    fn emit(&self, event: Event) {
        match event {
            Event::Boot { fw_version, phase } => {
                info!("vintage-kvm firmware {}; phase={}", fw_version, phase);
            }
            Event::DownloadBegin {
                total_blocks,
                expected_crc32,
                size_bytes,
            } => {
                info!(
                    "LPT: Stage 2 download begin (size={} B, CRC-32=0x{:08X}, {} blocks)",
                    size_bytes, expected_crc32, total_blocks
                );
            }
            Event::BlockAck {
                block_no,
                running_crc32,
            } => {
                debug!(
                    "LPT blk {} ACK; running CRC-32 = 0x{:08X}",
                    block_no, running_crc32
                );
            }
            Event::DownloadComplete {
                crc_match,
                final_crc32,
            } => {
                if crc_match {
                    info!(
                        "LPT: Stage 2 download OK — CRC match (0x{:08X})",
                        final_crc32
                    );
                } else {
                    warn!(
                        "LPT: Stage 2 download FAIL — CRC mismatch (got 0x{:08X})",
                        final_crc32
                    );
                }
            }
            Event::SeqGap { expected, got, cmd } => {
                warn!(
                    "seq gap: expected {}, got {} (cmd 0x{:02X})",
                    expected, got, cmd
                );
            }
            Event::UnknownCmd { cmd } => {
                warn!("unknown cmd 0x{:02X}", cmd);
            }
            Event::PacketStreamResync { reason } => {
                warn!("packet stream resync: {}", reason);
            }
            Event::EncodeError => {
                warn!("encode failed");
            }
        }
    }
}
