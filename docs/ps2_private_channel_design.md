# PS/2 Private Channel Design

**Status:** Detailed design document for [`design.md`](design.md) §17 (PS/2 Private Fallback Transport)  
**Last updated:** 2026-05-12  
**Companion documents:** [`design.md`](design.md), [`ps2_eras_reference.md`](ps2_eras_reference.md), [`two_plane_transport.md`](two_plane_transport.md) (this doc covers the L0/L1 wire layer; `two_plane_transport.md` covers L2/L3 session and control-plane semantics)  
**Maps to roadmap:** Phase 11 in `design.md` §22

This document is the implementation design for the PS/2 private fallback transport — the low-speed bidirectional channel that runs through the Pico's emulated PS/2 keyboard and AUX/mouse ports between Stage 0+ DOS code and the Pico firmware. It exists because the IEEE 1284 path may not always be available (broken negotiation, missing LPT, hostile chipset), and a PS/2-only rescue/control path is required.

## Why this works at all

A PS/2 keyboard/mouse port is **not receive-only**. The host can transmit bytes to the device by driving Clock/Data in the defined host-to-device sequence; the device acknowledges. The device transmits back to the host using its own clocking. A mini-DIN-connected peripheral is therefore an active communication peer, not just a passive input source.

At the software side, the classic 8042 interface exposes the KBC through I/O ports 0x60/0x64. The AUX/mouse port is reached via controller command `0xD4` followed by the data byte. Once the Pico's firmware and the DOS-side Stage 0 both **own the i8042** (Stage 0 has masked IRQ1/IRQ12 and replaced normal handlers), the two endpoints can exchange arbitrary bytes inside the existing PS/2 frame protocol.

The catch: the Super I/O / KBC still frames the world as keyboard and mouse devices, so the data has to flow as legal PS/2 command/response traffic or as carefully chosen scan-code / mouse-packet encodings. There is **no chipset-level "raw mode."** ([`ps2_eras_reference.md`](ps2_eras_reference.md#what-does-not-exist-on-mainstream-super-io-parts) lists what mainstream Super I/Os do *not* expose.)

## Channel options

### Option A — Custom PS/2 mouse tunnel (primary data path)

The Pico impersonates a PS/2 mouse on the AUX port; payload travels as mouse packets or as responses to host commands.

```
Host → Pico (via AUX):
  out 0x64, 0xD4         ; next data byte to AUX
  out 0x60, <command>    ; e.g. payload-frame-request command
  ...Pico responds with 0xFA ACK + optional response byte(s)

Pico → Host (via AUX):
  emit legal mouse packet:
    byte0: status/sign bits/button bits   (acts as frame header/type)
    byte1: dx payload
    byte2: dy payload
    byte3 (IntelliMouse mode): wheel/extra payload
  host reads 0x60 when OBF + AUX bits set in status; routes to vintage-kvm receiver
```

**Pros:**
- Works through normal AUX path with no chipset-level tricks.
- Electrical and protocol behavior remains legal — the Super I/O sees a normal-looking mouse.
- Easy to implement on the Pico side; reuses the same `ps2_at_dev.pio` and command state machine as production mouse emulation.
- Inbound traffic is interrupt-driven via IRQ12 — Stage 0 just hooks the IRQ.

**Cons:**
- Low bandwidth (see [Bandwidth expectations](#bandwidth-expectations) below).
- Stage 0 must disable or replace the OS mouse driver (i.e., this works on bare-metal DOS, not while a graphical Windows session is running).
- Mouse packet values must avoid pathological cursor behavior if any layer above Stage 0 sees them. Negotiating IntelliMouse 4-byte mode (200/100/80 sample-rate knock) gives an extra payload byte per packet and is usually safe.

### Option B — Keyboard scan-code tunnel (control/bootstrap only, not for bulk data)

The Pico impersonates a keyboard and sends carefully chosen scan-code sequences as data.

**Pros:**
- Works on very old systems where no AUX port exists (XT, plain AT).
- IRQ1 delivery is simple; BIOS/boot-time visibility is excellent.
- This is how Stage –1 / Stage 0 bootstrap already works (Pico types `DEBUG` scripts via legal scan codes).

**Cons:**
- Dangerous if anything above Stage 0 interprets bytes as keystrokes.
- Harder to send arbitrary binary cleanly — modifier state, scan-code Set 2 → Set 1 8042 translation, typematic state, BIOS hotkey events all corrupt semantics.
- Cannot reliably send all 256 byte values without escapes.

**Use:** bootstrap and attention only. Never as the primary data lane on AT+.

### Option C — Dual-port bridge (control plane + data plane)

Combine A and B: AUX is the data plane, keyboard is the control/interrupt/bootstrap plane.

```
Keyboard port (IRQ1):
  E0-prefixed scan-code sequence = "attention / frame ready / mode change"
  bootstrap codepath (Stage –1, Stage 0 loader)
  out-of-band "abort" signal

AUX port (IRQ12):
  mouse-packet payload stream
  bulk data transfer
  acknowledgements
```

This is the **recommended architecture for vintage-kvm's §17 fallback** — it matches `design.md` §17.1's "Lane Model" (Lane 0 = keyboard, Lane 1 = AUX) exactly, with the refinement that the keyboard lane is reserved for control/bootstrap and the AUX lane carries data.

### Option D — Wake events as out-of-band attention (optional, low-rate)

On Winbond/Nuvoton W83627-class and ITE IT87xx-class Super I/Os (see [`ps2_eras_reference.md`](ps2_eras_reference.md#superio-1995) catalog), mouse-move, mouse-button, keyboard hotkey/password, and ACPI wake events can be detected/configured.

```
Mouse-move wake     → Pico requests service from a sleeping host
Mouse double-click  → high-priority alert
Keyboard hotkey     → configured management event
```

This is **not a data channel** — it's a wake/attention mechanism. Could be leveraged for a future "Pico can wake a sleeping DOS PC" feature. Not in scope for the initial Phase 11 implementation.

## What does NOT exist (capabilities to NOT assume)

Per the Super I/O catalog in [`ps2_eras_reference.md`](ps2_eras_reference.md#what-does-not-exist-on-mainstream-super-io-parts):

- Arbitrary UART-like mode on mini-DIN pins.
- Direct raw Clock/Data bitstream capture exposed to host software.
- Multi-drop AUX addressing.
- High-speed PS/2 variants — the wire protocol is fixed at 10–16.7 kHz clock.
- Hardware packet FIFOs beyond normal KBC buffering.
- Vendor-documented "sideband mailbox over PS/2."

The KBC path is always an 8042 compatibility abstraction. No tricks below the protocol layer are available.

## RP2350-side timing advantages

The Pico's PIO can **oversample host-to-device traffic at much higher rates than the PS/2 wire requires.** PS/2 host-to-device frames are clocked at 10–16.7 kHz; an RP2350 PIO state machine clocked at, say, 10 MHz samples each line transition with ~100 ns granularity. This buys:

| Use | Benefit |
|---|---|
| **Robust decoding** | Reject metastable transitions and ringing; vote on each bit window |
| **Clock-stretch detection** | Hosts pull clock low to inhibit the device; oversampling cleanly distinguishes inhibit from glitches |
| **Host-timing measurement** | Record actual host-driven clock period and duty cycle; build a per-host timing histogram |
| **Chipset fingerprinting** | Different KBC implementations (real 8042, Super I/O Winbond/Nuvoton/ITE/Fintek, virtualized KVM, USB legacy emulation) have measurably different timing signatures. Could distinguish a real DOS PC from DOSBox, or one Super I/O family from another |
| **Adaptive timing** | Tighten or relax the Pico's response timing based on observed host behavior; tolerate slow hosts without slowing everyone down |
| **Marginal-host detection** | Spot hosts whose timing drifts out of spec; surface as a diagnostic to the user |
| **Collision avoidance** | When the device wants to send but the host is about to inhibit, the oversampling state machine sees the falling edge of clock earlier and aborts the send cleanly |

What this does **not** buy: **higher host-to-device payload rate**. The PS/2 device supplies the shift clock for both byte directions (host-to-device too — the host signals "request to send" by manipulating Clock/Data, then the device clocks each data bit while the host drives the Data line); but the host controls *when* a host-to-device transfer begins, and the Super I/O controls what bit-cell timing it accepts as valid. The Pico can tune bit timing, but it cannot make the host issue command bytes faster, and it cannot bypass the i8042 receive path. The 10–16.7 kHz envelope is fixed by host-side tolerance, not by what the Pico can drive.

Implication: oversampling goes into the Pico's PS/2 PIO programs and Rust state machine as a quality and observability feature. **It improves reliability and observability; it does not raise throughput.**

## What the RP2350 can and cannot control

The PS/2 device supplies the shift clock for both byte directions. For device-to-host, the device clocks bits the device puts on the Data line. For host-to-device, the host first signals "request to send" (pulls Clock low briefly, then releases and pulls Data low), at which point the device begins clocking each bit while the host drives the Data line; the device samples Data on its own clock edges and ACKs at the end. So the RP2350 owns the bit-cell timing in both directions, **but the host owns when host-to-device transfers begin, and the Super I/O owns what timing it accepts as valid**. The Pico can drive cleaner and faster signalling than the spec requires; the host may or may not accept it, and it cannot force higher host command throughput.

**Controllable from RP2350 side:**

- Exact PS/2 clock period (any divisor of the PIO clock; 2.5 µs resolution at 320× divider)
- Intentional timing variation to probe host tolerance
- Open-drain behavior via `pindirs` (drive low = output, release = input — see [`hardware_reference.md`](hardware_reference.md) §3 for the 74LVC07A buffer)
- Deterministic frame generation via PIO state machines
- Oversampled host-to-device decoding (see §RP2350-side timing advantages above)

**Not controllable:**

- Super I/O internal PS/2 receiver timing, digital filters, FIFO depth
- IRQ delivery behavior
- Parity error handling
- Host's timeout assumptions
- USB legacy SMM emulation paths (if enabled in BIOS)

The hard limit: the Super I/O will always interpret the lines as PS/2 Clock/Data and deliver bytes through its KBC/AUX machinery. No reliable way to turn the mini-DIN into SPI, UART, USB-like signalling, high-speed custom links, raw GPIO, or any multi-Mbit sideband. **The i8042 abstraction is unbypassable from the device end.**

## Timing and bandwidth analysis

### Raw line rate envelope

PS/2 framing is 11 bits per byte (start + 8 data + parity + stop). At spec-compliant clocks:

```
10.0 kHz / 11 bits ≈   909 bytes/s raw
12.0 kHz / 11 bits ≈ 1091 bytes/s raw     ← common default
16.7 kHz / 11 bits ≈ 1518 bytes/s raw     ← spec maximum
```

Useful payload is lower once framing, escaping, CRC, ACK/NAK, command latency, and host interrupt overhead are subtracted.

### Overclocking the PS/2 clock (experimental, not portable)

The RP2350 can drive the clock above 16.7 kHz. Standard-compatible hosts (Cypress/Infineon device reference cites 10–14.6 kHz; broader PS/2 references cite 10–16.7 kHz) may accept faster rates **or may not**. Probing data:

| Device clock | Outcome on typical Super I/O |
|---|---|
| 20 kHz | Often accepted (~1.8 KB/s raw) |
| 25 kHz | Often accepted on tolerant chipsets (~2.3 KB/s raw) |
| 33 kHz | Sometimes accepted; host-specific |
| 50 kHz | Frequently fails — parity errors, RESEND storms, dropped bytes, controller stalls |

Failure modes when the host can't keep up: timing-violation rejection, silent byte drop, mis-sampling, parity errors, controller stalls, RESEND requests, **divergent behavior between keyboard and AUX ports on the same chip**, and divergent behavior between motherboards using the same Super I/O part. **Do not commit to overclocking as a portable strategy** — make it opt-in per host after calibration.

### Per-encoding payload table

Combining frame encoding with achievable report rates:

| Encoding | Report rate | Sustained payload |
|---|---:|---:|
| 3-byte mouse packet, 2 payload B/packet | 100 reports/s | 200 B/s |
| 3-byte mouse packet, 2 payload B/packet | 200 reports/s | 400 B/s |
| 3-byte mouse packet, 2 payload B/packet | 500 reports/s *(if host tolerates)* | 1000 B/s |
| IntelliMouse 4-byte packet, 3 payload B/packet | 200 reports/s | 600 B/s |
| IntelliMouse 4-byte packet, 3 payload B/packet | 500 reports/s *(tolerant hosts)* | 1500 B/s |
| Keyboard scan-code tunnel (Option B) | n/a | tens–hundreds B/s |
| Host-to-device (any encoding) | command-paced | **asymmetric, much lower** — i8042 sequencing + ACK round-trips per byte |
| Wake-event signalling (Option D) | event-only | not a payload channel |

**Target tiers for vintage-kvm:**

| Tier | Configuration | Sustained payload |
|---|---|---|
| **Portable** (any DOS PC, no calibration) | 3-byte packets, 100 reports/s, spec-compliant 12 kHz clock | 200 B/s |
| **Good host** (calibrated, IntelliMouse mode negotiated) | 4-byte packets, 200 reports/s, 12–16 kHz clock | ~600 B/s |
| **Experimental** (per-host calibrated, overclocked) | 4-byte packets, 500 reports/s, 20–25 kHz clock | 1–2 KB/s |

**Host-to-device direction is much worse** — each command involves i8042 sequencing and a device ACK, so practical bandwidth toward the Pico is sufficient only for command opcodes, flow control, ACK/NAK, mode switching, register reads, small configuration payloads. **Don't expect symmetric throughput.**

### Useful for / not useful for

**Useful for:** diagnostics, configuration, identity, bootstrap, trace summaries, control-plane messaging, small file transfer (a few KB), compressed diagnostic dumps.

**Not useful for:** firmware upload, full logs, bus traces, screen dumps — those stay on the IEEE 1284 path. PS/2 is rescue/control only.

## Calibration mode (adaptive timing negotiation)

Because host tolerance is the unknown, the Pico runs a calibration handshake before settling into the data-plane mode.

### Protocol

```
1. Pico boots into SAFE mode (12 kHz clock, 100 reports/s).
2. Stage 0 / pico1284 sends CALIBRATE_BEGIN command via AUX (host → device).
3. Pico ramps clock stepwise: 12 → 16 → 20 → 25 → 33 kHz.
4. At each step, Pico sends N known test frames with sequence numbers + CRC16.
5. Host-side driver validates:
   - frames-received vs frames-sent ratio
   - sequence-number gaps
   - parity errors visible in i8042 status
   - RESEND events
   - i8042 output-buffer overrun behavior
6. Pico falls back to the last clock rate at which loss rate < threshold.
7. Same procedure repeats for report rate (100 → 200 → 500 reports/s) at the chosen clock.
8. Pico stores chosen rates and reports SESSION_PROFILE = { clock_hz, report_rate, packet_size } via AUX.
```

### Test frame format

```
SYNC byte (reserved status pattern — see Frame format proposal below)
sequence_number    (1 byte, wraps mod 256)
payload_length     (1 byte)
payload            (payload_length bytes; pseudo-random known content)
CRC16-CCITT        (2 bytes)
```

### Session modes (the calibration outcome)

| Mode | Conditions to choose | Payload target |
|---|---|---|
| `SAFE` | Loss rate > 1% even at 12 kHz / 100 reports/s | 200 B/s |
| `STANDARD` | Loss rate < 0.1% at 16 kHz / 200 reports/s | ~600 B/s |
| `FAST` | Loss rate < 0.1% at 20 kHz / 500 reports/s | ~1 KB/s |
| `EXPERIMENTAL` | User-confirmed 25–33 kHz acceptable | 1–2 KB/s |

The Pico defaults to `SAFE` on every reset; the DOS-side driver can re-trigger calibration on demand (e.g., after a chipset change or if loss rate climbs during a session).

### Persistence

Optionally persist the calibration result to RP2350 flash keyed by a host fingerprint (assembled from the oversampling histograms in [§RP2350-side timing advantages](#rp2350-side-timing-advantages)) so the next boot can skip the ramp on a known host. Not required for v1.

## Recommended design for vintage-kvm

**Architecture:**

```
Pico (firmware):
  - PS/2 keyboard personality on KBD port (GP2–GP5)
  - PS/2 mouse personality on AUX port (GP6, GP9, GP10, GP28 — see hardware_reference.md §3.3)
  - Both use ps2_at_dev.pio with oversampled host-traffic decoder

DOS-side (s0_at.asm / s0_ps2.asm + pico1284):
  - claim i8042 directly
  - mask IRQ1 and IRQ12 at PIC
  - disable normal keyboard/mouse drivers (or run in bare-metal/diagnostic mode)
  - install custom IRQ1 handler for keyboard-lane attention events
  - install custom IRQ12 handler for AUX-lane data frames
  - use 0xD4 + AUX writes for outbound commands to Pico
```

**Lane assignment** (matches `design.md` §17.1):

| Lane | Direction | Purpose | Frame format |
|---|---|---|---|
| Keyboard (Lane 0) | host↔Pico (mostly host→Pico for control) | Attention, mode change, bootstrap, small reliable control messages | Scan-code-encoded; each control byte is `E0` + a reserved scan code |
| AUX (Lane 1) | Pico→host primary, host→Pico via `0xD4` | Bulk data frames, payloads | Mouse-packet-encoded; payload in dx/dy/wheel; frame header in button/sign bits |

**Frame format over AUX mouse packets** (proposal, refines `design.md` §17.2):

The default approach keeps every emitted byte **looking like a legal PS/2 mouse packet**, so unexpected paths (BIOS, SMM, ACPI mouse drivers running in parallel, virtual-machine input layers) treat the bytes as benign mouse data rather than malformed packets. Stage 0 owns IRQ12 and routes vintage-kvm traffic off to the receiver before any of those paths see it, but **survivability through "I didn't expect to see this packet" code paths is higher when the bytes themselves are spec-legal.**

```
PS/2 fragment (carried inside IntelliMouse 4-byte mouse packets):

  Status byte template (byte0) — always spec-legal:
    bit 0: L button   = 0
    bit 1: R button   = 0
    bit 2: M button   = 0
    bit 3: always-1   = 1     ← required by PS/2 spec
    bit 4: X-sign     = 0
    bit 5: Y-sign     = 0
    bit 6: X-overflow = 0
    bit 7: Y-overflow = 0
    = 0x08

  Header packet (start of each frame):
    byte0: 0x08                ← legal quiescent status
    byte1: 0x7F                ← escape sentinel A (legal as +127 dx with no sign)
    byte2: 0x81                ← escape sentinel B (legal as -127 dy with no sign)
    byte3: lane_id<<6 | type<<4 | seq    ← IntelliMouse 4th byte; frame metadata
              bits 7..6: lane_id (00 = AUX, 01 = KBD echo, 10/11 reserved)
              bits 5..4: type (00 = data, 01 = ack, 10 = nak, 11 = control)
              bits 3..0: sequence number (mod 16)

  Length packet (immediately follows header):
    byte0: 0x08
    byte1: payload_len_lo
    byte2: payload_len_hi
    byte3: reserved / first payload byte

  Payload packets:
    byte0: 0x08
    byte1..byte3: 3 payload bytes per packet

  Trailer:
    byte0: 0x08
    byte1: crc16_lo
    byte2: crc16_hi
    byte3: 0x00

Ack/Nak via host command byte (host → Pico via 0xD4 + cmd):
  0x55: ACK seq N
  0xAA: NAK seq N, request resend
```

The receiver discriminates vintage-kvm frames from real mouse activity by looking for the `0x7F 0x81` pair in byte1/byte2 of a packet with byte0 = 0x08. A real mouse could plausibly emit (status=0x08, dx=+127, dy=-127, byte3=0) during a fast diagonal stroke — collision probability is small but nonzero. To reduce it further, the receiver requires two consecutive packets matching the header pattern (escape + valid metadata) before committing to "frame in progress." Once committed, payload packets need only byte0=0x08 to be parsed.

**Alternative discriminator** (more aggressive, less portable): use byte0 with X-overflow + Y-overflow bits set (`0xC8`). Real mice rarely set both overflow bits — any sane driver discards such packets, so collision is near-zero. **But:** if any intermediate path (BIOS legacy mouse handler, virtual machine input, fallback driver) sees a 0xC8 packet, it may flag it as protocol error or reset the mouse. Use only when Stage 0 has confirmed exclusive IRQ12 ownership and no other consumer exists.

The host driver's mouse handler is bypassed entirely because vintage-kvm owns IRQ12 — but the legal-byte default protects against the cases where that bypass leaks.

**Flow control** (matches `design.md` §17.3): credit-based, mirroring the IEEE 1284 packet layer. Don't let the AUX lane free-run into the controller.

## Important caveats

- **OS ownership of i8042 is a real issue.** BIOS, SMM, ACPI firmware, and the OS input stack may all believe they own the KBC. Without complete control, you get race conditions, swallowed bytes, unexpected resets, or injected mouse/keyboard input. On a bare-metal DOS machine with Stage 0+ resident, this is manageable. On a Windows 9x session, it is brittle to impossible.
- **USB legacy support emulates PS/2 at the chipset/SMM level on modern machines.** If USB legacy is enabled in BIOS, SMM intercepts every `OUT 60h` / `IN 60h` and routes it through the USB stack. To get clean i8042 ownership, disable USB legacy support in BIOS setup and ensure no USB HID kernel drivers are running on ports 0x60/0x64.
- **The Super I/O may auto-swap KBD/MS ports** (Winbond bit, ITE IT8718F auto-swap). The Pico cannot tell from the wire which physical mini-DIN it's plugged into. Probe orientation via Stage 0 sending a keyboard-specific command and observing which Pico-side port responds.
- **Scan-code translation (Set 2 → Set 1) is on by default** on most BIOSes via 8042 CCB bit 6. Disable translation in Stage 0 if the keyboard lane is being used for raw control bytes; otherwise restrict to scan codes that survive translation.
- **Avoid pathological mouse-packet sequences** in case anything above Stage 0 momentarily sees them — e.g., never emit a packet that would request the cursor to wrap or move >1000 units in a frame.
- **BAT and reset:** the Pico must still emit `0xAA` (BAT pass) on power-up or when the host issues `0xFF` reset, and the DOS-side Stage 0 must handle this transparently. Reset is the Pico's only opportunity to re-sync if framing gets desynced.

## Implementation mapping

| Subrepo / module | Role |
|---|---|
| `firmware/src/ps2/ps2_at_dev.pio` | Oversampled host-traffic decoder; same PIO program serves keyboard and AUX endpoints |
| `firmware/src/ps2/kbd.rs` | Keyboard command state machine + LED-pattern unlock detector + control-byte encoder for Lane 0 |
| `firmware/src/ps2/mouse.rs` | Mouse command state machine + IntelliMouse 4-byte mode + data-frame encoder for Lane 1 |
| `firmware/src/ps2/private_mode.rs` | Lane-0/Lane-1 framing, credit-based flow, CRC, ACK/NAK, sequence numbers |
| `dos/stage0/s0_ps2.asm` | DOS-side counterpart: i8042 mastery, IRQ1+IRQ12 handlers, lane framing |
| `dos/pico1284/ps2_i8042.{asm,c}` | Production-grade dual-lane transport for the TSR/CLI |

## Open decisions

1. **Mouse-packet frame discriminator:** the proposal above uses spec-legal `byte0=0x08, byte1=0x7F, byte2=0x81` as the frame-start escape (every byte looks like a normal mouse packet). The more aggressive alternative is `byte0=0xC8` (overflow bits set) — near-zero collision but rejected by sane drivers if any intermediate path sees it. Choose the legal-byte default unless Stage 0 has confirmed total IRQ12 ownership with no other consumer.
2. **Lane 0 (keyboard) encoding for control bytes:** `E0`-prefix + reserved scan code, or use of an unused break/make code, or a vendor-defined hotkey? Decide before Phase 11 implementation.
3. **Sample-rate negotiation tuning:** PS/2 mouse sample rates are 10, 20, 40, 60, 80, 100, 200 reports/s. Default is 100. For data-plane use, set the highest the host accepts to maximize throughput, but watch for hosts that fail above 80.
4. **Credit-window size:** `design.md` §17.3 mentions credit-based flow but doesn't specify window. Initial proposal: 8 packets each direction, expandable based on measured RTT.
5. **Fingerprinting database:** if the Pico oversampling collects per-host timing histograms, where does it store/report the fingerprint? `defmt` over RTT for now, persisted to flash for diagnostics later?

## Related documents

- [`design.md`](design.md) §17 — overview of PS/2 private fallback transport (this doc is the detailed implementation design)
- [`ps2_eras_reference.md`](ps2_eras_reference.md) — per-era protocol differences, Super I/O catalog
- [`hardware_reference.md`](hardware_reference.md) §3 — PS/2 hardware (74LVC07A buffer, GPIO map)
- [`implementation_plan.md`](implementation_plan.md) §1 — firmware module breakdown including `ps2/private_mode.rs`
