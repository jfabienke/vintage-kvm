//! `control-client` — host-side one-shot CLI for the vintage-kvm
//! `control` CDC ACM RPC channel.
//!
//! Connects to the Pico's `control` CDC interface, writes one verb +
//! arguments line, reads reply lines until the terminator (`ok` or
//! `err` prefix), and emits either:
//!
//! - **Text mode** (default): each reply line on its own line.
//! - **JSON mode** (`--json`): one structured JSON object on stdout
//!   summarizing success, parsed per-verb data, the raw reply lines,
//!   and the error message if any. Designed for LLM / scripted callers
//!   that want a stable schema instead of grepping firmware text.
//!
//! Exit code: `0` on `ok`, `1` on `err`. Verb grammar lives in the
//! firmware (`firmware/src/usb/control.rs`).
//!
//! ```sh
//! control-client ping
//! # → pong 12345
//!
//! control-client --json ping
//! # → {"ok":true,"verb":"ping","data":{"uptime_ms":12345},"lines":["pong 12345"]}
//!
//! control-client --json stats
//! # → {"ok":true,"verb":"stats","data":{"uptime_ms":…,"kbd":{…},…},…}
//!
//! control-client --json mouse_move 10 -5
//! # → {"ok":true,"verb":"mouse_move","data":{},"lines":["ok"]}
//!
//! control-client --port /dev/cu.usbmodemX --json bulk_test hello
//! ```

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{Map, Value};

const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;

/// Wait this long for the firmware to reply. `inject 32 bytes` is the
/// slowest landed verb at ~150 ms; 3 seconds is generous for anything
/// short of a Stage 0 typing run (those don't go through this CLI).
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Args {
    port_override: Option<String>,
    json: bool,
    rest: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let mut out = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => {
                out.port_override = Some(
                    it.next()
                        .context("--port needs a path argument")?,
                );
            }
            "--json" => out.json = true,
            "-h" | "--help" => {
                eprintln!("{USAGE}");
                std::process::exit(0);
            }
            _ => {
                out.rest.push(a);
                // Everything after the verb is verbatim — don't try to
                // re-interpret as flags.
                out.rest.extend(it);
                break;
            }
        }
    }
    if out.rest.is_empty() {
        bail!("missing verb\n\n{USAGE}");
    }
    Ok(out)
}

const USAGE: &str = "\
usage: control-client [--port <path>] [--json] <verb> [args...]

verbs: ping
       stats
       inject <hex> [hex ...]
       mouse_move <dx> <dy>
       mouse_button l|r|m up|down
       bulk_test [text]

--json   emit a single JSON object summarizing the reply
--port   override CDC auto-detection (e.g. /dev/cu.usbmodem01)";

#[derive(Serialize)]
struct JsonReply {
    ok: bool,
    verb: String,
    /// Per-verb parsed payload. Empty object when there's nothing
    /// structured to extract (mouse_move/mouse_button success).
    data: Value,
    /// First trimmed reply line that begins with `err ` (minus the
    /// prefix). Absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Every reply line we received, in order, trimmed of trailing
    /// CR/LF. The connection banner is filtered out.
    lines: Vec<String>,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let line = args.rest.join(" ");
    let verb = args.rest[0].clone();

    let port_path = match args.port_override {
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
    let mut writer = port.try_clone().context("clone port for writer")?;
    writer.write_all(line.as_bytes()).context("write verb")?;
    writer.write_all(b"\r").context("write terminator")?;
    writer.flush().ok();

    let mut reader = BufReader::new(port);
    let mut buf = String::new();
    let mut lines: Vec<String> = Vec::new();
    let mut ok = true;
    let mut error: Option<String> = None;

    loop {
        buf.clear();
        let n = reader
            .read_line(&mut buf)
            .context("read reply (timed out?)")?;
        if n == 0 {
            bail!("port closed before reply complete");
        }
        let trimmed = buf.trim_end_matches(['\r', '\n']).to_string();

        if trimmed.starts_with("vintage-kvm control") {
            continue;
        }

        lines.push(trimmed.clone());

        if let Some(rest) = trimmed.strip_prefix("err") {
            ok = false;
            error = Some(rest.trim_start().to_string());
            break;
        }
        if trimmed == "ok" || trimmed.starts_with("ok ") {
            break;
        }
        // Otherwise: body line (stats etc.). Keep reading.
    }

    if args.json {
        let data = if ok {
            parse_data(&verb, &lines).unwrap_or_else(|| Value::Object(Map::new()))
        } else {
            Value::Object(Map::new())
        };
        let reply = JsonReply {
            ok,
            verb,
            data,
            error,
            lines,
        };
        println!("{}", serde_json::to_string(&reply)?);
    } else {
        for line in &lines {
            println!("{line}");
        }
    }

    std::process::exit(if ok { 0 } else { 1 });
}

/// Per-verb reply parsing. Returns `None` when no structured shape is
/// known for the verb (caller emits an empty object so the JSON schema
/// stays stable). Returns `Some(Value::Object(_))` on success.
///
/// Parsers are intentionally permissive: if the firmware tweaks its
/// reply wording the JSON degrades to "data is empty + lines preserved"
/// instead of failing.
fn parse_data(verb: &str, lines: &[String]) -> Option<Value> {
    match verb {
        "ping" => {
            // "pong <uptime_ms>"
            let rest = lines.first()?.strip_prefix("pong")?.trim_start();
            let n: u64 = rest.parse().ok()?;
            let mut m = Map::new();
            m.insert("uptime_ms".into(), Value::from(n));
            Some(Value::Object(m))
        }
        "stats" => Some(parse_stats(lines)),
        "inject" => {
            // "ok queued N"
            let rest = lines.last()?.strip_prefix("ok queued")?.trim_start();
            let n: u64 = rest.parse().ok()?;
            let mut m = Map::new();
            m.insert("queued".into(), Value::from(n));
            Some(Value::Object(m))
        }
        "bulk_test" => {
            // "ok bulk queued NB"
            let rest = lines.last()?.strip_prefix("ok bulk queued")?.trim();
            let num = rest.trim_end_matches('B');
            let n: u64 = num.parse().ok()?;
            let mut m = Map::new();
            m.insert("queued_bytes".into(), Value::from(n));
            Some(Value::Object(m))
        }
        // mouse_move / mouse_button / unknown verbs → bare "ok".
        _ => None,
    }
}

/// Parse the multi-line `stats` reply into a nested JSON object.
///
/// Body lines come in two shapes:
/// - Bare `key=val` (top-level scalar, e.g. `uptime_ms=12345`).
/// - `section key=val key=val ...` (one section per line, e.g.
///   `kbd words=… frames=…`).
///
/// Numeric values become JSON numbers, everything else stays as a
/// string. Unknown shapes are dropped (they show up in `lines`).
fn parse_stats(lines: &[String]) -> Value {
    let mut top = Map::new();
    for line in lines {
        if line.starts_with("ok") {
            continue;
        }
        let mut parts = line.split_ascii_whitespace();
        let first = match parts.next() {
            Some(t) => t,
            None => continue,
        };
        if let Some((k, v)) = first.split_once('=') {
            // Bare `key=val` — top-level entry.
            top.insert(k.to_string(), kv_value(v));
            continue;
        }
        // Otherwise treat `first` as a section name and the rest as kv pairs.
        let mut section = Map::new();
        for tok in parts {
            if let Some((k, v)) = tok.split_once('=') {
                section.insert(k.to_string(), kv_value(v));
            }
        }
        top.insert(first.to_string(), Value::Object(section));
    }
    Value::Object(top)
}

fn kv_value(s: &str) -> Value {
    if let Ok(n) = s.parse::<u64>() {
        Value::from(n)
    } else if let Ok(n) = s.parse::<i64>() {
        Value::from(n)
    } else {
        Value::from(s.to_string())
    }
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
