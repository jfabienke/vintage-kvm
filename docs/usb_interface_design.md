# USB Interface Design

**Status:** Design doc — not yet implemented.
**Last updated:** 2026-05-14.

The RP2350's USB port is the boundary between vintage-kvm's vintage side
(PS/2 + IEEE 1284 to the DOS host) and its modern side (operator's
laptop). This doc picks the device descriptor, justifies each interface,
and lays out the protocols layered on top.

## 1. Goals

The USB side has to carry five distinct conversations between the
operator's laptop and the Pico:

| # | Conversation                              | Direction              | Shape           |
|---|-------------------------------------------|------------------------|-----------------|
| 1 | Instrumentation / event stream            | Pico → laptop          | structured log  |
| 2 | Control / RPC (start, stop, set, dump)    | laptop ↔ Pico          | request/reply   |
| 3 | Text-mode console of the vintage host     | Pico → laptop          | VT100 stream    |
| 4 | Keystroke relay (operator typing through) | laptop → vintage host  | byte stream     |
| 5 | Video framebuffer / file transfer         | Pico ↔ laptop          | bulk blobs      |

Cross-cutting constraints:

- **No custom host driver on the common path.** Operators should be able
  to attach `screen` / a serial terminal / a CDC-aware TUI without
  signing INFs or installing libusb shims for the basics.
- **Composite-device tolerable.** Modern macOS, Linux, Windows all bind
  multi-interface CDC devices automatically; that's fine.
- **Full-Speed only.** RP2350's USB controller is FS (12 Mbit/s); the
  design assumes ~1 MB/s sustained bulk ceiling and doesn't try to
  exceed it.

## 2. Composite device layout

The Pico exposes itself as a **composite USB device** with one
configuration containing four interface bundles:

| Interface bundle      | Class       | OS auto-binds? | Pipe shape          |
|-----------------------|-------------|----------------|---------------------|
| `events`              | CDC ACM     | yes            | bulk IN (Pico→host) |
| `control`             | CDC ACM     | yes            | bulk IN + OUT       |
| `console`             | CDC ACM     | yes            | bulk IN + OUT       |
| `bulk`                | Vendor      | no (WinUSB)    | bulk IN + OUT       |

Three CDC ACM functions appear as three independent serial ports
(`/dev/ttyACM0`, `…1`, `…2` on Linux). The vendor interface needs a
WinUSB descriptor on Windows so it can be claimed via libusb without
manual driver installs; macOS/Linux pick it up via libusb directly.

```
USB device (composite)
├── interface: events  (CDC ACM, bulk IN only)
│       Telemetry events serialized via
│       vintage-kvm-telemetry-schema. defmt-RTT stays as the dev-probe
│       channel; this is the production stream every operator sees.
│
├── interface: control (CDC ACM, bulk IN/OUT)
│       JSON-lines or CBOR RPC: start/stop, reset classifier, set
│       config, dump ring buffer, query stats, mouse-move/button.
│
├── interface: console (CDC ACM, bulk IN/OUT)
│       Bidirectional VT100/ANSI stream. OUT: vintage host's text
│       VRAM (B800:0000) diff'd against the previous frame and
│       encoded as cursor-positioning + character escapes. IN: every
│       byte the operator types is forwarded to the vintage host as
│       a PS/2 keystroke via the existing `KbdTx`.
│
└── interface: bulk    (vendor, bulk IN/OUT)
        Higher-bandwidth blobs that don't fit a terminal abstraction:
        graphics-mode framebuffers, file transfer, eventually
        firmware-side captured Stage 2 images for post-mortem
        inspection.
```

## 3. Why these choices

### 3.1 Why three CDC ACMs instead of one

Could collapse all three CDC functions into one and multiplex by
escape codes. We don't, for two reasons:

- **Driverless operation per concern.** `screen /dev/ttyACM2` to look
  at the vintage host's text screen, separately `cat /dev/ttyACM0` to
  watch the event log, separately a TUI process binding `…1` for RPC.
  Multiplexing forces every consumer to share one client.
- **Backpressure isolation.** A slow operator tail on the event log
  must never block the keystroke-relay path. Independent endpoints
  guarantee that at the USB layer.

Cost: three interface descriptors and three endpoint pairs. The Pico
has plenty of endpoints (USB 2.0 FS allows 16 IN + 16 OUT); three
CDC pairs = 3 interrupt + 3×2 bulk = 9 endpoints, well within budget.

### 3.2 Why not HID

The obvious "Pico is a keyboard" framing has the wrong handedness.
The operator already has a keyboard — their laptop's. We want
keystrokes from *that* keyboard to reach the vintage host through the
Pico, which means:

- The operator's TUI captures their local keystrokes (terminal raw
  mode does this for free) and forwards bytes over the `console`
  CDC.
- The Pico converts those bytes to PS/2 scancodes via the existing
  `ps2::injector` + `KbdTx` path and emits them on the vintage wire.

A HID *device* on the Pico-USB side would mean the laptop sees a
keyboard, which is backwards. A HID *host* on the Pico-USB side
would mean plugging a physical keyboard into the Pico and passing
through — possible (RP2350 supports USB host mode), but doubles the
bring-up surface for a use case the laptop-TUI path already covers.
Deferred until a concrete user asks for it.

### 3.3 Why not UVC for video

UVC would let the captured framebuffer show up as a webcam in any
video app. Tempting, but:

- The data isn't from a real-time sensor — it's framebuffer blobs
  the vintage host pushes down LPT. UVC's pacing model fights this.
- Standard UVC formats (YUYV, MJPEG, NV12) don't match VGA palette
  modes or text-mode VRAM at all. We'd transcode to fit a format
  nobody else uses, gaining nothing.
- Text mode dominates the bring-up phase and has its own much better
  surface (the `console` CDC).

Vendor bulk + custom viewer is more honest and more flexible. Once
we know the bandwidth and frame-format constraints in practice we
can layer formats on top.

### 3.4 Why not MSC

A USB Mass Storage class device implies "I am a disk." The Pico
isn't one. The vintage host's filesystem isn't on USB. The closest
real use case — push a file from the laptop *to* the vintage host's
DOS filesystem — runs over LPT through Stage 1's existing block
transfer, with the operator's laptop staging the file via the bulk
interface. MSC adds no value.

(BOOTSEL → MSC for firmware update is unchanged; that's the
bootloader, not our runtime descriptor.)

## 4. Per-interface protocols

### 4.1 `events`

- Direction: bulk IN only (Pico → laptop).
- Wire format: length-delimited frames, each carrying one
  serialized [`Event`](../crates/telemetry-schema) value (postcard or
  CBOR).
- Backpressure: drop-on-full at the Pico side; oldest events lost
  first. A `DroppedEvents { count }` event is emitted when the
  laptop next drains.
- Read pattern: a single long-lived reader process. Multiple readers
  is undefined; do not attach two.

### 4.2 `control`

- Direction: bulk IN + OUT, request/reply.
- Wire format: JSON-lines for human-debuggable v1; switch to CBOR
  later if line-length pressure shows up.
- Verbs (initial set):
  - `ping` → `pong { uptime_ms }`
  - `stats` → `{ kbd_frames, aux_frames, lpt_packets, ... }`
  - `inject <hex>` — type raw bytes through KbdTx; bypasses the
    classifier-driven script.
  - `mouse_move { dx, dy }` / `mouse_button { btn, down }` —
    generates PS/2 AUX packets via `AuxTx`.
  - `dump_ring kbd|aux|lpt N` → bulk transfer of the last N words
    over the `bulk` interface.
  - `set { classifier_threshold, … }`.
- Per-line max: 4 KB (heapless buffer on the Pico side).

### 4.3 `console`

- Direction: bulk IN + OUT.
- IN (Pico → laptop): bytes look like a VT100/xterm stream the
  laptop's terminal renders directly. Encoding:
  - On each text-mode frame capture, diff against the previous
    frame; for each changed cell, emit `ESC [ row ; col H` + the new
    character (and `ESC [ Nm` for attribute changes when the colour
    or intensity flips).
  - On mode-change events (vintage host enters graphics mode),
    emit a sentinel + a human-readable banner so the operator's
    terminal renders "vintage host left text mode" and the bulk
    viewer takes over.
- OUT (laptop → Pico): bytes are forwarded straight into the PS/2
  keystroke relay. ASCII → PS/2 scancode via the same
  [`scancode`](../firmware/src/ps2/scancode.rs) tables the injector
  already uses; unsupported bytes (shifted punctuation, multi-byte
  ESC sequences for arrow keys) get logged and dropped at v1.
- Read pattern: any standard terminal emulator. The expected
  invocation is literally `screen /dev/ttyACM2 115200` (the baud
  rate is ignored — CDC ACM is a packet stream, not UART — but
  some clients refuse to open without it).

### 4.4 `bulk`

- Direction: bulk IN + OUT, vendor-specific class.
- Framing: a 16-byte header (`{magic: 4, kind: 2, len: 4, seq: 2,
  reserved: 4}`) followed by `len` payload bytes.
- Kinds (initial set):
  - `kind = 0x0001 GraphicsFrame { mode, width, height, bpp, … }`
  - `kind = 0x0002 RingDump { which, base_t_us, words[] }`
  - `kind = 0x0003 FileChunk { path, offset, bytes[] }` — for
    pushing files into the vintage host's filesystem via the
    Stage 1 block transfer.
- WinUSB descriptor: install GUID
  `{e7c8e8e8-...}` so libusb on Windows attaches without an INF.

## 5. Bandwidth budget

USB Full Speed gives nominally 12 Mbit/s; realistically ~9.6 Mbit/s
useful after bulk overhead = ~1.15 MB/s. Distributed across our four
interfaces under worst-case simultaneous load:

| Interface | Worst-case data rate         | Comment                           |
|-----------|------------------------------|-----------------------------------|
| `events`  | ~10 kB/s                     | dozens of events/sec, ~200 B each |
| `control` | bursty, < 1 kB/s steady      | RPC, low duty cycle               |
| `console` | ~4 kB/s text-mode diffs      | 80×25 × 30 fps × 5% changed       |
| `bulk`    | up to 1 MB/s if alone        | dominates if active               |

The bulk channel takes the lion's share when active; the three CDC
channels combined fit easily in the remaining ~150 kB/s. Concurrent
bulk-video + active text console + heavy event stream is the
worst case and still fits within FS at degraded video frame rate.

## 6. Phasing

| Phase | Interface(s) landed           | Trigger                                     |
|-------|-------------------------------|---------------------------------------------|
| 4a    | `events`                      | first CDC enabled; replaces defmt-RTT       |
|       |                               | dependence for non-developer operators      |
| 4b    | `control`                     | TUI dashboard begins consuming RPC          |
| 5a    | `console` (IN only)           | text-VRAM read path over private channel    |
|       |                               | is working; OUT is software-loopback only   |
| 5b    | `console` (IN + OUT)          | keystroke-relay → PS/2 wire round-trip      |
|       |                               | confirmed on a real host                    |
| 6     | `bulk`                        | first graphics-mode capture lands           |

Nothing in this list blocks the current Phase 3 (LPT bootstrap) or
Phase 2 (PS/2 injection) work; USB lands incrementally on top.

## 7. Open questions

- **WinUSB descriptor specifics.** Need to nail down the OS-string +
  MS-OS-2.0 descriptor blob the Pico advertises so Windows attaches
  the vendor interface via WinUSB without manual INF install. The
  TinyUSB examples are the reference.
- **Serial number stability.** When the laptop sees three CDC
  interfaces, the operator wants the same ttyACM number for the same
  function across reboots. Either advertise a stable serial number
  (from the RP2350's chip ID) and use udev rules, or rely on the
  per-interface descriptor strings. Pick when Phase 4 lands.
- **Control-channel format.** JSON-lines or CBOR? JSON is more
  debuggable from a terminal; CBOR is more compact. Probably JSON
  for v1, with the option to bump later.
- **Graphics-mode framebuffer encoding.** Raw, RLE, or zstd? Depends
  on what the Stage 1 framebuffer-transfer side actually delivers.
  Defer until then.

## 8. Related documents

- [`design.md`](design.md) — top-level system design.
- [`instrumentation_surface.md`](instrumentation_surface.md) —
  source of the `events` interface's schema.
- [`two_plane_transport.md`](two_plane_transport.md) — PS/2 +
  IEEE 1284 plane abstraction the bulk + console interfaces
  ultimately bind to.
- [`firmware_crate_and_trait_design.md`](firmware_crate_and_trait_design.md)
  — trait surface the USB transport will plug into alongside the
  existing LPT transport.
