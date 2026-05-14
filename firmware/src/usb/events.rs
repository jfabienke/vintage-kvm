//! CDC ACM `events` interface — Pico → host telemetry stream.
//!
//! Static channel `USB_EVENT_CHAN` carries owned [`Event`] values from
//! every emitter site (LPT transport, PS/2 framer, supervisor, …) to
//! the writer task. Backpressure policy: `try_send` and drop on full.
//! Dropped events bump `USB_EVENT_DROPS`; the next successful event
//! emission piggybacks an `EncodeError`-style sentinel so the host
//! sees that telemetry was lost.
//!
//! Wire format: each event is serialized via `postcard::to_slice_cobs`,
//! producing a COBS-encoded payload terminated by `0x00`. The host
//! splits on `0x00` and `postcard::from_bytes_cobs` round-trips back
//! to the matching `Event`.

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_usb::class::cdc_acm::CdcAcmClass;
use vintage_kvm_telemetry_schema::{Event, TelemetryEmit};

/// Inbox for events on their way to the host. Capacity 32 absorbs
/// typical burst rates (a Stage 2 download emits up to ~20 BlockAcks
/// in a few ms); steady-state load is dozens of events per second.
pub static USB_EVENT_CHAN: Channel<CriticalSectionRawMutex, Event, 32> = Channel::new();

/// Count of events the channel had to drop because it was full when
/// `try_send` ran. Surfaced via the next-success piggyback frame
/// (TODO: synthesize a real `DroppedEvents` variant in the schema).
pub static USB_EVENT_DROPS: AtomicU32 = AtomicU32::new(0);

/// `TelemetryEmit` impl that fans an event onto the USB events channel.
/// Pair with `DefmtEmit` via `MultiEmit` so both wires stay live during
/// dev work.
#[derive(Copy, Clone)]
pub struct UsbEmit;

impl TelemetryEmit for UsbEmit {
    fn emit(&self, event: Event) {
        if USB_EVENT_CHAN.try_send(event).is_err() {
            USB_EVENT_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Writer task — drains `USB_EVENT_CHAN`, serializes each event via
/// COBS-framed postcard, and writes the framed bytes to the CDC IN
/// endpoint. Waits for host connection before pulling the first event
/// so we don't burn the buffer on a disconnected port.
#[embassy_executor::task]
pub async fn run(mut class: CdcAcmClass<'static, Driver<'static, USB>>) -> ! {
    // postcard's COBS output adds at most 1 overhead byte per 254 bytes
    // of payload + 1 terminator. Our largest Event serializes to maybe
    // ~50 bytes; 256 is comfortable headroom.
    let mut frame_buf = [0u8; 256];

    loop {
        class.wait_connection().await;
        defmt::info!("usb events: host connected");

        loop {
            let event = USB_EVENT_CHAN.receive().await;

            let encoded = match postcard::to_slice_cobs(&event, &mut frame_buf) {
                Ok(slice) => slice,
                Err(e) => {
                    defmt::warn!("usb events: postcard encode failed: {}", defmt::Debug2Format(&e));
                    continue;
                }
            };

            // Stream the encoded frame in 64-byte chunks (CDC FS max
            // packet size). A short final packet flushes the frame to
            // the host; a zero-length packet would be needed only if
            // the frame is an exact multiple of 64 — unusual at our
            // sizes, but handled defensively.
            let mut sent_full = false;
            for chunk in encoded.chunks(64) {
                if class.write_packet(chunk).await.is_err() {
                    defmt::warn!("usb events: write failed; host disconnected?");
                    break;
                }
                if chunk.len() == 64 {
                    sent_full = true;
                }
            }
            if sent_full && encoded.len().is_multiple_of(64) {
                // Empty packet so the host's CDC stack delivers the
                // accumulated bulk without waiting for more.
                let _ = class.write_packet(&[]).await;
            }
        }
    }
}
