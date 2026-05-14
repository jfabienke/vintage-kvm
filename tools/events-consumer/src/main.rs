//! `events-consumer` — host-side reader for the vintage-kvm `events`
//! CDC ACM stream.
//!
//! Opens the CDC serial port (auto-detected by VID/PID 0xC0DE:0xCAFE
//! unless `--port <path>` is given), reads bytes, splits the stream on
//! the COBS terminator (0x00), decodes each frame with
//! `postcard::from_bytes_cobs`, and prints one line per event:
//!
//! ```text
//! [12:34:56.789] Boot { fw_version: "0.1.0", phase: 3 }
//! [12:34:56.802] DownloadBegin { total_blocks: 17, ... }
//! ```
//!
//! Decode errors print a one-line warning and skip the bad frame; the
//! reader stays in sync because the COBS terminator is unambiguous.
//!
//! ## Usage
//!
//! ```sh
//! cargo run -p vintage-kvm-events-consumer            # auto-detect Pico
//! cargo run -p vintage-kvm-events-consumer -- /dev/cu.usbmodem01
//! ```

use std::io::{self, Read};
use std::time::Duration;

use anyhow::{Context, Result};

mod event;
use event::Event;

const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;

fn main() -> Result<()> {
    let port_path = match std::env::args().nth(1) {
        Some(p) => p,
        None => auto_detect().context("auto-detect failed; pass /dev/... explicitly")?,
    };
    eprintln!("events-consumer: opening {port_path}");

    let mut port = serialport::new(&port_path, 115_200)
        .timeout(Duration::from_secs(3600))
        .open()
        .with_context(|| format!("open {port_path}"))?;

    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 64];

    loop {
        match port.read(&mut chunk) {
            Ok(0) => continue,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                drain_frames(&mut buf);
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e).context("serial read"),
        }
    }
}

/// Find the first `0x00` in the buffer (COBS terminator) and choose
/// the largest contiguous prefix of complete frames to decode. Each
/// frame is passed to postcard, which decodes the COBS encoding
/// in-place and deserializes the `Event`.
fn drain_frames(buf: &mut Vec<u8>) {
    while let Some(pos) = buf.iter().position(|&b| b == 0x00) {
        // Drain `0..=pos` so the terminator goes with the frame and the
        // next iteration's `pos` is for the next frame.
        let mut frame: Vec<u8> = buf.drain(..=pos).collect();
        // A stray 0x00 (empty frame from a host-side stutter) decodes
        // to nothing useful — skip it without complaining.
        if frame.len() <= 1 {
            continue;
        }
        match postcard::from_bytes_cobs::<Event>(&mut frame) {
            Ok(event) => println!("[{}] {:?}", timestamp(), event),
            Err(e) => eprintln!(
                "[{}] decode error: {} ({} byte frame)",
                timestamp(),
                e,
                frame.len()
            ),
        }
    }
}

/// `HH:MM:SS.mmm` local time. Cheap and dependency-free via std::time —
/// not monotonic, but the operator wants a wall-clock for correlating
/// with other logs.
fn timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let total = now.as_secs();
    let h = (total / 3600) % 24;
    let m = (total / 60) % 60;
    let s = total % 60;
    let ms = now.subsec_millis();
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// Walk the system's serial port list and return the second CDC ACM
/// port whose USB descriptor matches the Pico's VID/PID — the *first*
/// CDC is `events` (interface 0), the second is `control` (interface 2),
/// etc. We want the first one (events).
///
/// On macOS the same Pico exposes two device paths per CDC ACM
/// interface (`/dev/tty.usbmodemNNN1` and `/dev/cu.usbmodemNNN1`); the
/// `cu.` one is the call-up (non-blocking) variant the OS hands to
/// foreign callers and is what we want.
fn auto_detect() -> Result<String> {
    let ports = serialport::available_ports().context("enumerate serial ports")?;
    let mut matches: Vec<String> = ports
        .into_iter()
        .filter_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(info) if info.vid == VID && info.pid == PID => {
                Some(p.port_name)
            }
            _ => None,
        })
        .collect();

    // Prefer `cu.` over `tty.` on macOS; both refer to the same
    // endpoint but only `cu.` opens non-blocking.
    matches.sort_by(|a, b| {
        let prio = |s: &str| if s.contains("cu.") { 0 } else { 1 };
        prio(a).cmp(&prio(b)).then_with(|| a.cmp(b))
    });

    matches
        .into_iter()
        .next()
        .with_context(|| format!("no serial port with VID:PID {VID:04X}:{PID:04X}"))
}
