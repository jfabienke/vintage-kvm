//! Vendor-class bulk interface — high-bandwidth blobs between the
//! Pico and the operator's laptop.
//!
//! Per `docs/usb_interface_design.md` §4.4 this carries the data that
//! doesn't fit the CDC abstractions: graphics-mode framebuffers, ring
//! dumps, file chunks pushed into the vintage host's filesystem. v1
//! lands the framing and a smoke-test `bulk_test` round-trip; concrete
//! producers come online as their sources do.
//!
//! ## Wire framing
//!
//! Each frame is a 16-byte header followed by `len` payload bytes:
//!
//! ```text
//!  0..4   magic   = b"VKVN"           (`BULK_MAGIC`)
//!  4..6   kind    u16 little-endian   (`BulkKind`)
//!  6..10  len     u32 little-endian   payload length
//! 10..12  seq     u16 little-endian   monotonic per-stream
//! 12..16  reserved (zero in v1)
//! ```
//!
//! Frames are split into wMaxPacketSize (64-byte) USB packets by the
//! writer; a zero-length packet flushes when `total % 64 == 0` so the
//! host's libusb stack delivers without waiting.
//!
//! ## OUT direction
//!
//! The OUT endpoint exists so the laptop can stream framebuffers /
//! file chunks *into* the vintage host. v1 has no consumer wired up
//! yet — the reader task drains and logs, so the wire works end-to-end
//! and a future producer plugs in without restructuring.

use core::sync::atomic::{AtomicU16, Ordering};

use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Endpoint, In, Out};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_usb::driver::{EndpointIn as _, EndpointOut as _, Endpoint as _};
use heapless::Vec;

/// Magic prefix `b"VKVN"` — distinguishes a valid frame from random
/// noise on the wire and helps a host that loses sync recover by
/// scanning forward.
pub const BULK_MAGIC: [u8; 4] = *b"VKVN";

/// Max frame payload + header that fits one Channel slot. 512 is a
/// compromise: large enough for a chunky text-mode VRAM diff or a
/// short graphics row, small enough to keep four queued slots within
/// the firmware's RAM budget. Producers larger than this must chunk.
pub const BULK_FRAME_MAX: usize = 512;

pub type BulkFrame = Vec<u8, BULK_FRAME_MAX>;

#[derive(Debug, Clone, Copy)]
#[repr(u16)]
#[allow(dead_code)] // GraphicsFrame/RingDump/FileChunk land as their producers do
pub enum BulkKind {
    GraphicsFrame = 0x0001,
    RingDump = 0x0002,
    FileChunk = 0x0003,
    /// Loopback smoke-test payload; lets the operator confirm the wire
    /// works before any real producer is online.
    Test = 0x00FF,
}

/// Outbound queue for bulk frames. Each entry is a fully-built frame
/// (header + payload concatenated) ready for the writer to chunk onto
/// the wire. Depth 4 absorbs a small burst without holding producers.
pub static BULK_OUT: Channel<CriticalSectionRawMutex, BulkFrame, 4> = Channel::new();

/// Monotonic per-stream sequence number. Producers `fetch_add(1)` so
/// each frame on the wire carries an increasing seq, regardless of
/// which task built it. Wraps at u16::MAX; the host treats wrap as a
/// normal occurrence.
pub static BULK_SEQ: AtomicU16 = AtomicU16::new(0);

pub fn next_seq() -> u16 {
    BULK_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Build a header + payload into a [`BulkFrame`]. Returns `Err(())`
/// if the combined size exceeds [`BULK_FRAME_MAX`].
pub fn build_frame(kind: BulkKind, seq: u16, payload: &[u8]) -> Result<BulkFrame, ()> {
    if 16 + payload.len() > BULK_FRAME_MAX {
        return Err(());
    }
    let mut buf: BulkFrame = Vec::new();
    let _ = buf.extend_from_slice(&BULK_MAGIC);
    let _ = buf.extend_from_slice(&(kind as u16).to_le_bytes());
    let _ = buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let _ = buf.extend_from_slice(&seq.to_le_bytes());
    let _ = buf.extend_from_slice(&[0u8; 4]);
    let _ = buf.extend_from_slice(payload);
    Ok(buf)
}

/// Writer task — drains [`BULK_OUT`] and streams each frame onto the
/// vendor bulk IN endpoint in 64-byte packets, with a ZLP flush when
/// the frame is an exact multiple of the max packet size.
#[embassy_executor::task]
pub async fn run_writer(mut ep_in: Endpoint<'static, USB, In>) -> ! {
    ep_in.wait_enabled().await;
    defmt::info!("usb bulk: writer enabled");

    loop {
        let frame = BULK_OUT.receive().await;
        let mut sent_full = false;
        for chunk in frame.chunks(64) {
            if ep_in.write(chunk).await.is_err() {
                defmt::warn!("usb bulk: write failed; host disconnected?");
                ep_in.wait_enabled().await;
                break;
            }
            if chunk.len() == 64 {
                sent_full = true;
            }
        }
        if sent_full && frame.len().is_multiple_of(64) {
            let _ = ep_in.write(&[]).await;
        }
    }
}

/// Reader task — drains the vendor bulk OUT endpoint. v1 has no
/// consumers, so received bytes are logged at info level (size only)
/// and discarded. The endpoint exists today so a later FileChunk
/// producer plugs in without rebuilding descriptors.
#[embassy_executor::task]
pub async fn run_reader(mut ep_out: Endpoint<'static, USB, Out>) -> ! {
    ep_out.wait_enabled().await;
    defmt::info!("usb bulk: reader enabled");

    let mut buf = [0u8; 64];
    loop {
        match ep_out.read(&mut buf).await {
            Ok(n) => defmt::info!("usb bulk: rx {} bytes (discarded)", n),
            Err(_) => {
                defmt::warn!("usb bulk: read failed; waiting for host");
                ep_out.wait_enabled().await;
            }
        }
    }
}
