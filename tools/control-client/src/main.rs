//! `control-client` — host-side one-shot CLI for the vintage-kvm
//! `control` CDC ACM RPC channel.
//!
//! Connects to the Pico's `control` CDC interface, writes one verb +
//! arguments line, reads reply lines until the terminator (`ok` or
//! `err` prefix), prints everything to stdout, exits 0 / 1.
//!
//! ```sh
//! control-client ping
//! # → pong 12345
//!
//! control-client stats
//! # → uptime_ms=…
//! # → kbd words=… clk_active=… …
//! # → aux words=… …
//! # → usb event_drops=…
//! # → ok
//!
//! control-client mouse_move 10 -5
//! # → ok
//!
//! control-client bulk_test hello
//! # → ok bulk queued 21B
//!
//! control-client --port /dev/cu.usbmodemX  ping
//! ```
//!
//! Verb grammar lives in the firmware (`firmware/src/usb/control.rs`),
//! not here — the client is intentionally a dumb pipe so verb additions
//! don't need a host-side bump.

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use anyhow::{bail, Context, Result};

const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;

/// Wait this long for the firmware to reply. `inject 32 bytes` is the
/// slowest landed verb at ~150 ms; 3 seconds is generous for anything
/// short of a Stage 0 typing run (those don't go through this CLI).
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    let mut port_override: Option<String> = None;
    if args.first().map(|s| s == "--port").unwrap_or(false) {
        args.remove(0);
        if args.is_empty() {
            bail!("--port needs a path argument");
        }
        port_override = Some(args.remove(0));
    }

    if args.is_empty() {
        bail!(
            "usage: control-client [--port <path>] <verb> [args...]\n\
             verbs: ping, stats, inject <hex>..., mouse_move <dx> <dy>,\n\
                    mouse_button l|r|m up|down, bulk_test [text]"
        );
    }

    let port_path = match port_override {
        Some(p) => p,
        None => auto_detect_control()
            .context("auto-detect failed; pass --port <path> explicitly")?,
    };
    eprintln!("control-client: opening {port_path}");

    let port = serialport::new(&port_path, 115_200)
        .timeout(REPLY_TIMEOUT)
        .open()
        .with_context(|| format!("open {port_path}"))?;

    // The firmware sends a one-line banner on every DTR-up transition
    // (`class.wait_connection().await`), so a fresh open will deliver
    // it before any reply. We could drain it with a short timeout
    // first, but it's simpler and race-free to send our verb and
    // filter the banner line out of the reply stream.
    let line = args.join(" ");
    let mut writer = port.try_clone().context("clone port for writer")?;
    writer.write_all(line.as_bytes()).context("write verb")?;
    writer.write_all(b"\r").context("write terminator")?;
    writer.flush().ok();

    let mut reader = BufReader::new(port);
    let mut buf = String::new();
    let mut exit_code = 0;
    loop {
        buf.clear();
        let n = reader
            .read_line(&mut buf)
            .context("read reply (timed out?)")?;
        if n == 0 {
            bail!("port closed before reply complete");
        }
        let line = buf.trim_end_matches(['\r', '\n']);

        // Banner is harmless but noisy — filter once.
        if line.starts_with("vintage-kvm control") {
            continue;
        }

        println!("{line}");

        if line.starts_with("ok") {
            break;
        }
        if line.starts_with("err") {
            exit_code = 1;
            break;
        }
        // Anything else is a stats body line; keep reading until ok/err.
    }

    std::process::exit(exit_code);
}

/// Pick the second CDC ACM device matching our VID/PID — interface
/// order in the composite is events (0), control (2), console (4), so
/// sorting the serialport list by USB interface number puts control at
/// index 1.
///
/// macOS exposes both a `cu.usbmodem*` (non-blocking) and a
/// `tty.usbmodem*` (blocking) node per CDC; we filter to `cu.*`. Linux
/// uses `ttyACM*` and has no duplicate.
fn auto_detect_control() -> Result<String> {
    let ports = serialport::available_ports().context("enumerate serial ports")?;
    let mut matches: Vec<(Option<u8>, String)> = ports
        .into_iter()
        .filter_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(info) if info.vid == VID && info.pid == PID => {
                Some((info.interface, p.port_name))
            }
            _ => None,
        })
        .filter(|(_, name)| !name.contains("/tty."))
        .collect();

    // Sort by (interface, name). interface is the OS-reported USB
    // bInterfaceNumber when serialport supports it; falls back to name
    // ordering, which matches the descriptor order on every platform
    // we've tested.
    matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    matches
        .into_iter()
        .nth(1)
        .map(|(_, name)| name)
        .with_context(|| {
            format!("couldn't find the second CDC ACM on VID:PID {VID:04X}:{PID:04X}")
        })
}
