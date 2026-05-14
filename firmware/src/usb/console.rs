//! CDC ACM `console` interface — vintage-host text-mode console proxy.
//!
//! Per `docs/usb_interface_design.md` §4.3 the eventual shape is a
//! bidirectional VT100/xterm stream:
//!
//! - **IN**  (Pico → laptop): a VT100-encoded diff of the vintage
//!   host's text-mode VRAM, so an operator can `screen /dev/ttyACM2`
//!   and watch the host as if it were a serial console.
//! - **OUT** (laptop → Pico): every byte the operator types is forwarded
//!   into the vintage host as a PS/2 keystroke via `KbdTx`.
//!
//! ## Phase 5a scope (this revision)
//!
//! Neither end of the real plumbing exists yet — the private-channel
//! text-VRAM read path isn't online and the operator-keystroke ASCII
//! relay needs the classifier-driven scancode dispatch wired up. We
//! still land the CDC endpoint today so:
//!
//! - The composite USB descriptor is final (no host-side rebind churn
//!   when 5b lands).
//! - Operators can attach `screen /dev/ttyACM2` and confirm the wire
//!   works end-to-end via a loopback echo: every byte they type comes
//!   back on the IN endpoint, with `\r` rewritten to `\r\n` so the
//!   terminal renders newlines correctly.
//!
//! Phase 5b replaces the loopback with the real text-VRAM-diff producer
//! (IN) and the ASCII→PS/2 forwarder (OUT).

use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_usb::class::cdc_acm::CdcAcmClass;

const BANNER: &str = "vintage-kvm console v1 (Phase 5a loopback)\r\n\
                       — type to verify the wire. Phase 5b will swap this for\r\n\
                         text-VRAM diffs and PS/2 keystroke relay.\r\n";

#[embassy_executor::task]
pub async fn run(mut class: CdcAcmClass<'static, Driver<'static, USB>>) -> ! {
    let mut rx = [0u8; 64];
    let mut tx = [0u8; 128];

    loop {
        class.wait_connection().await;
        defmt::info!("usb console: host connected");

        // Banner is best-effort; if the host disconnects mid-banner we
        // pick it back up on the next wait_connection().
        for chunk in BANNER.as_bytes().chunks(64) {
            if class.write_packet(chunk).await.is_err() {
                break;
            }
        }

        loop {
            let n = match class.read_packet(&mut rx).await {
                Ok(n) => n,
                Err(_) => {
                    defmt::warn!("usb console: read failed; reconnect");
                    break;
                }
            };

            // Loopback with CR→CRLF expansion. screen/cu send a bare
            // `\r` when the operator hits Enter; without the LF the
            // cursor returns to column 0 and overwrites the line.
            let mut out_len = 0usize;
            for &b in &rx[..n] {
                if b == b'\r' {
                    if out_len + 2 > tx.len() {
                        let _ = class.write_packet(&tx[..out_len]).await;
                        out_len = 0;
                    }
                    tx[out_len] = b'\r';
                    tx[out_len + 1] = b'\n';
                    out_len += 2;
                } else {
                    if out_len + 1 > tx.len() {
                        let _ = class.write_packet(&tx[..out_len]).await;
                        out_len = 0;
                    }
                    tx[out_len] = b;
                    out_len += 1;
                }
            }

            // Drain in 64-byte packets; CDC FS max is 64.
            for chunk in tx[..out_len].chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    defmt::warn!("usb console: write failed; reconnect");
                    break;
                }
            }
        }
    }
}
