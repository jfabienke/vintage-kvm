//! CDC ACM `control` interface — bidirectional RPC channel.
//!
//! Per `docs/usb_interface_design.md` §4.2 the v1 protocol is one
//! request per line, `\r` or `\n` terminated, plain-text verbs. The
//! design doc names JSON-lines as the eventual format; we stay text
//! for now so an operator with `screen /dev/ttyACM1` can drive the
//! Pico interactively without any client tooling.
//!
//! ## Verbs landed in this revision
//!
//! - `ping` → `pong <uptime_ms>`
//! - `stats` → multi-line counter dump
//! - `inject <hex> [hex ...]` → enqueue raw scancode bytes for the KBD
//!   wire. AT framing only; XT inject is deferred. Up to
//!   `INJECT_RAW_MAX` bytes per command.
//! - `mouse_move <dx> <dy>` → send a PS/2 mouse-movement packet on
//!   the AUX wire. Deltas clamp to 9-bit signed (-256..=255).
//! - `mouse_button l|r|m up|down` → press / release a mouse button.
//!   Button state is sticky across move commands.
//! - `bulk_test [text]` → enqueue a `BulkKind::Test` frame on the
//!   vendor bulk IN endpoint. Default payload is `"hello"`; with an
//!   argument, the rest of the line is the payload.
//! - everything else → `err unknown verb: <name>`
//!
//! Future verbs (`dump_ring`, `set`, …) plug in by adding arms to
//! `dispatch`.

use core::fmt::Write;
use core::sync::atomic::Ordering;

use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_time::Instant;
use embassy_usb::class::cdc_acm::CdcAcmClass;
use embassy_usb::driver::EndpointError;
use heapless::String;

use crate::ps2::aux_oversampler::AUX_COUNTERS;
use crate::ps2::injector::{INJECT_RAW, INJECT_RAW_MAX};
use crate::ps2::mouse_input::{MouseBtn, MouseCmd, MOUSE_CMD};
use crate::ps2::oversampler::{KBD_COUNTERS, KBD_SELF_TX_FRAMES};
use crate::usb::bulk::{build_frame, next_seq, BulkKind, BULK_OUT};
use crate::usb::events::USB_EVENT_DROPS;

/// Inbound buffer: we accept ASCII commands up to one line.
const LINE_CAP: usize = 128;
/// Outbound: a stats dump is several short lines; the writer chunks
/// to 64-byte CDC packets so this only bounds one logical message.
const OUT_CAP: usize = 512;

#[embassy_executor::task]
pub async fn run(mut class: CdcAcmClass<'static, Driver<'static, USB>>) -> ! {
    let mut line: String<LINE_CAP> = String::new();
    let mut buf = [0u8; 64];

    loop {
        class.wait_connection().await;
        defmt::info!("usb control: host connected");

        line.clear();
        let _ = send_line(&mut class, "vintage-kvm control v1 — type 'ping' or 'stats'").await;

        loop {
            let n = match class.read_packet(&mut buf).await {
                Ok(n) => n,
                Err(_) => {
                    defmt::warn!("usb control: read failed; reconnect");
                    break;
                }
            };

            for &b in &buf[..n] {
                if b == b'\r' || b == b'\n' {
                    if !line.is_empty() {
                        dispatch(&line, &mut class).await;
                        line.clear();
                    }
                } else if line.push(b as char).is_err() {
                    // Overflow → drop the line and report.
                    let _ = send_line(&mut class, "err line too long").await;
                    line.clear();
                }
            }
        }
    }
}

async fn dispatch(line: &str, class: &mut CdcAcmClass<'static, Driver<'static, USB>>) {
    let trimmed = line.trim();
    let mut parts = trimmed.split_ascii_whitespace();
    let verb = parts.next().unwrap_or("");

    match verb {
        "" => {}
        "ping" => {
            let mut out: String<32> = String::new();
            let _ = write!(out, "pong {}", Instant::now().as_millis());
            let _ = send_line(class, &out).await;
        }
        "stats" => {
            let _ = send_stats(class).await;
        }
        "inject" => handle_inject(parts, class).await,
        "mouse_move" => handle_mouse_move(parts, class).await,
        "mouse_button" => handle_mouse_button(parts, class).await,
        "bulk_test" => handle_bulk_test(trimmed, class).await,
        other => {
            let mut out: String<64> = String::new();
            let _ = write!(out, "err unknown verb: {other}");
            let _ = send_line(class, &out).await;
        }
    }
}

async fn handle_inject(
    parts: core::str::SplitAsciiWhitespace<'_>,
    class: &mut CdcAcmClass<'static, Driver<'static, USB>>,
) {
    let mut bytes: heapless::Vec<u8, INJECT_RAW_MAX> = heapless::Vec::new();
    for tok in parts {
        match u8::from_str_radix(tok, 16) {
            Ok(b) => {
                if bytes.push(b).is_err() {
                    let _ = send_line(class, "err inject too long").await;
                    return;
                }
            }
            Err(_) => {
                let mut out: String<64> = String::new();
                let _ = write!(out, "err bad hex byte: {tok}");
                let _ = send_line(class, &out).await;
                return;
            }
        }
    }
    if bytes.is_empty() {
        let _ = send_line(class, "err inject needs at least one hex byte").await;
        return;
    }
    let n = bytes.len();
    if INJECT_RAW.try_send(bytes).is_err() {
        let _ = send_line(class, "err inject queue full").await;
        return;
    }
    let mut out: String<32> = String::new();
    let _ = write!(out, "ok queued {n}");
    let _ = send_line(class, &out).await;
}

async fn handle_mouse_move(
    mut parts: core::str::SplitAsciiWhitespace<'_>,
    class: &mut CdcAcmClass<'static, Driver<'static, USB>>,
) {
    let dx = match parts.next().and_then(|s| s.parse::<i16>().ok()) {
        Some(v) => v,
        None => {
            let _ = send_line(class, "err mouse_move: bad dx").await;
            return;
        }
    };
    let dy = match parts.next().and_then(|s| s.parse::<i16>().ok()) {
        Some(v) => v,
        None => {
            let _ = send_line(class, "err mouse_move: bad dy").await;
            return;
        }
    };
    if MOUSE_CMD.try_send(MouseCmd::Move { dx, dy }).is_err() {
        let _ = send_line(class, "err mouse queue full").await;
        return;
    }
    let _ = send_line(class, "ok").await;
}

async fn handle_mouse_button(
    mut parts: core::str::SplitAsciiWhitespace<'_>,
    class: &mut CdcAcmClass<'static, Driver<'static, USB>>,
) {
    let btn = match parts.next() {
        Some("l") | Some("L") => MouseBtn::Left,
        Some("r") | Some("R") => MouseBtn::Right,
        Some("m") | Some("M") => MouseBtn::Middle,
        _ => {
            let _ = send_line(class, "err mouse_button: expected l|r|m").await;
            return;
        }
    };
    let down = match parts.next() {
        Some("down") => true,
        Some("up") => false,
        _ => {
            let _ = send_line(class, "err mouse_button: expected up|down").await;
            return;
        }
    };
    if MOUSE_CMD
        .try_send(MouseCmd::Button { btn, down })
        .is_err()
    {
        let _ = send_line(class, "err mouse queue full").await;
        return;
    }
    let _ = send_line(class, "ok").await;
}

async fn handle_bulk_test(
    line: &str,
    class: &mut CdcAcmClass<'static, Driver<'static, USB>>,
) {
    // Strip the verb itself; whatever remains (possibly empty) is the
    // payload. Default to b"hello" so a bare `bulk_test` is the
    // simplest smoke test.
    let rest = line.strip_prefix("bulk_test").unwrap_or("").trim_start();
    let payload: &[u8] = if rest.is_empty() { b"hello" } else { rest.as_bytes() };

    let frame = match build_frame(BulkKind::Test, next_seq(), payload) {
        Ok(f) => f,
        Err(()) => {
            let _ = send_line(class, "err bulk_test payload too long").await;
            return;
        }
    };
    let len = frame.len();
    if BULK_OUT.try_send(frame).is_err() {
        let _ = send_line(class, "err bulk queue full").await;
        return;
    }
    let mut out: String<48> = String::new();
    let _ = write!(out, "ok bulk queued {len}B");
    let _ = send_line(class, &out).await;
}

async fn send_stats(class: &mut CdcAcmClass<'static, Driver<'static, USB>>) -> Result<(), EndpointError> {
    let mut out: String<OUT_CAP> = String::new();
    let _ = writeln_into(
        &mut out,
        format_args!("uptime_ms={}", Instant::now().as_millis()),
    );

    let _ = writeln_into(
        &mut out,
        format_args!(
            "kbd words={} clk_active={} frames={} errored={} self_tx={} fifo_ovr={} glitches={}",
            KBD_COUNTERS.words.load(Ordering::Relaxed),
            KBD_COUNTERS.clk_active_words.load(Ordering::Relaxed),
            KBD_COUNTERS.frames_total.load(Ordering::Relaxed),
            KBD_COUNTERS.frames_errored.load(Ordering::Relaxed),
            KBD_SELF_TX_FRAMES.load(Ordering::Relaxed),
            KBD_COUNTERS.fifo_overrun.load(Ordering::Relaxed),
            KBD_COUNTERS.glitches_total.load(Ordering::Relaxed),
        ),
    );
    let _ = writeln_into(
        &mut out,
        format_args!(
            "aux words={} clk_active={} frames={} errored={} fifo_ovr={} glitches={}",
            AUX_COUNTERS.words.load(Ordering::Relaxed),
            AUX_COUNTERS.clk_active_words.load(Ordering::Relaxed),
            AUX_COUNTERS.frames_total.load(Ordering::Relaxed),
            AUX_COUNTERS.frames_errored.load(Ordering::Relaxed),
            AUX_COUNTERS.fifo_overrun.load(Ordering::Relaxed),
            AUX_COUNTERS.glitches_total.load(Ordering::Relaxed),
        ),
    );
    let _ = writeln_into(
        &mut out,
        format_args!("usb event_drops={}", USB_EVENT_DROPS.load(Ordering::Relaxed)),
    );
    let _ = out.push_str("ok\n");

    write_chunks(class, out.as_bytes()).await
}

fn writeln_into(s: &mut impl core::fmt::Write, args: core::fmt::Arguments<'_>) -> core::fmt::Result {
    s.write_fmt(args)?;
    s.write_char('\n')
}

async fn send_line(
    class: &mut CdcAcmClass<'static, Driver<'static, USB>>,
    line: &str,
) -> Result<(), EndpointError> {
    let mut buf: String<{ LINE_CAP + 2 }> = String::new();
    // If the caller's line is too long, truncate rather than fail —
    // the operator's REPL shouldn't crash on a long diagnostic.
    let take = line.len().min(LINE_CAP);
    let _ = buf.push_str(&line[..take]);
    let _ = buf.push('\n');
    write_chunks(class, buf.as_bytes()).await
}

async fn write_chunks(
    class: &mut CdcAcmClass<'static, Driver<'static, USB>>,
    bytes: &[u8],
) -> Result<(), EndpointError> {
    for chunk in bytes.chunks(64) {
        class.write_packet(chunk).await?;
    }
    if !bytes.is_empty() && bytes.len().is_multiple_of(64) {
        // Zero-length packet flush so the host's CDC stack doesn't sit
        // on a packet-aligned buffer waiting for the next byte.
        class.write_packet(&[]).await?;
    }
    Ok(())
}
