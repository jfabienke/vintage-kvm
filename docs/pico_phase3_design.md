# Pico firmware — Phase 3+ design

Covers what the Pico needs to do to talk to DOS Stage 1 v1.0 over the IEEE 1284 / LPT bootstrap channel. Companion to [`design.md`](design.md) §22 Phases 3-5 and [`stage1_implementation.md`](stage1_implementation.md). This is the *implementation slice* for Phase 3+; the comprehensive Pico-side architecture lives in [`pico_firmware_design.md`](pico_firmware_design.md).

This is the **peripheral-side dual** of Stage 1's wire protocol — every byte Stage 1 sends, the Pico must receive; every reply Stage 1 expects, the Pico must produce.

---

## Scope

**In scope (Phase 3+ MVP):**

1. LPT SPP nibble byte pump (matches `dos/stage0/lpt_nibble.inc` wire protocol exactly).
2. CRC-16-CCITT packet framing (matches `dos/stage1/stage1.asm:97-122`).
3. Protocol dispatcher:
   - `CAP_REQ → CAP_RSP` (hard-coded version + embedded Stage 2 metadata)
   - `CAP_ACK` (acknowledge)
   - `PING → PONG` (echo payload)
   - `SEND_BLOCK → RECV_BLOCK` (serve embedded Stage 2 image)
   - `BLOCK_ACK` (advance) / `BLOCK_NAK` (re-serve)
4. Embedded Stage 2 placeholder blob + computed CRC-32.
5. Defmt-RTT logging for bench-side debugging.

**Out of scope (later phases):**

- Real IEEE 1284 negotiation handshake — Stage 1 attempts ECP→EPP→Byte→SPP and falls back to SPP on timeout, so the Pico can ignore the negotiation request and Stage 1 will land on SPP automatically. Phase 4-5 will add real responses.
- PIO-based EPP/ECP byte pumps — bit-bang SPP nibble is fast enough for Stage 2 download (~50 KB/s gives a ~4 s download for 200 KB; livable).
- PS/2 work (Phases 1, 2, 11) — none required for Stage 1 bench testing. When implemented, will use PIO oversampling with instrumentation (see "Forward-looking" below).
- USB CDC transport — defmt-RTT covers debug logging needs for now.

---

## LPT wire protocol (peripheral side)

### Pin map (from [`hardware_reference.md` §3.3](hardware_reference.md))

| GPIO | Signal | DOS dir | Pico dir | Notes |
|---|---|---|---|---|
| GP11 | nStrobe / HostClk | OUT | **IN** | Falling edge = DOS asserts byte (compat mode forward). |
| GP12-19 | Data 0-7 | OUT (fwd) / IN (rev) | **IN** (fwd) / **OUT** (rev) | Direction flips per IEEE 1284 phase. |
| GP20 | nAutoFd / HostAck | OUT | **IN** | Drives negotiation events; held high in SPP idle. |
| GP22 | nSelectIn / 1284Active | OUT | **IN** | Negotiation request when LO; idle HI. |
| GP23 | nAck / PeriphClk | IN | **OUT** | Pico pulses LO to acknowledge a received byte. |
| GP24 | Busy / PeriphAck | IN | **OUT** | Drive HI to stall DOS during processing. |
| GP25 | PError / nAckReverse | IN | **OUT** | Used in reverse-mode handshakes. |
| GP26 | Select / Xflag | IN | **OUT** | Set HI to accept a 1284 negotiation request. |
| GP27 | nFault / nPeriphRequest | IN | **OUT** | Held HI in normal operation; LO requests host attention. |
| GP21 | NeoPixel status | OUT | OUT | Runtime LED — green = idle, blue = packet I/O, red = error. |

Also on the LPT control register, DOS uses **nInit** (bit 2 of base+2) as the **host strobe** in our nibble protocol — *not* nStrobe. nInit maps to a DOS-output bit on the control register that doesn't have its own dedicated wire in the standard Centronics pinout — it's pin 16 (peripheral reset, host-driven). On the 74LVC161284 this comes in as an additional control-bus input the Pico must observe.

**TODO during bring-up:** confirm which Pico GPIO carries nInit. If the hardware doesn't expose it directly, we may need to multiplex onto nAutoFd or treat nStrobe as the strobe instead. Stage 0's `lpt_nibble.inc:42-43` documents `CTRL_INIT equ 04h` as the strobe.

### SPP nibble wire protocol (matching `lpt_nibble.inc`)

**DOS → Pico (host strobes a byte):**
1. DOS writes byte to LPT_DATA (data bus drives D0-D7 HI/LO).
2. DOS pulses nInit (host-strobe) LO then HI.
3. Pico observes nInit falling edge, latches D0-D7, queues byte.

**Pico → DOS (peripheral sends nibble, host polls):**
1. Pico presents low 4 bits of byte on status bits [3..6] (mapped to GP25..GP27 + nAck or similar — see implementation).
2. Pico toggles status bit 7 (phase) to signal "nibble stable".
3. DOS reads status, sees phase changed, consumes the nibble.
4. Pico presents high 4 bits, toggles phase again.
5. DOS consumes high nibble; byte complete.

**Persistent phase invariant** (critical, per `lpt_nibble.inc:13-15`): both sides track `last_phase`. The Pico's phase bit only flips on a *new* nibble; the receiver waits for `phase != last_phase`, then commits `last_phase = phase`. This prevents false reads if the Pico is slow to update the next nibble.

### IEEE 1284 negotiation (Phase 3: stub)

Stage 1 drives ECP → EPP → Byte → SPP. The Pico can simply:
- **Not respond** to nAck assertion within Stage 1's timeout (`NEG_TIMEOUT_OUTER = 0x100` ≈ 1 ms on AT-class).
- All four negotiation attempts time out.
- Stage 1's `ieee1284_negotiate_ladder` falls through to `.fall_spp` and prints `STAGE1: IEEE 1284 negotiation declined; staying in SPP/Nibble`.

This is the cleanest Phase 3 path — no negotiation state machine needed. Phase 4-5 will add real responses.

---

## Packet framing (peripheral side)

Matches `dos/stage1/stage1.asm:97-122` exactly.

```
SOH | CMD | SEQ | LEN_HI | LEN_LO | PAYLOAD | CRC_HI | CRC_LO | ETX
```

- `SOH = 0x01`, `ETX = 0x03`.
- `LEN` is big-endian u16 payload length.
- `SEQ` auto-increments per direction (Pico keeps `rx_seq_expected` and `tx_seq`).
- `CRC-16-CCITT` (poly `0x1021`, init `0xFFFF`, no refl, no xor-out) computed over `CMD..end-of-payload` (i.e., `4 + payload_len` bytes), appended big-endian.

Pico-side responsibilities:
- **Decode:** validate SOH/CRC/ETX, parse CMD/SEQ/LEN, expose payload slice.
- **Encode:** build outgoing packet with auto-incrementing SEQ and computed CRC.
- **Reject:** bad SOH/CRC/ETX → drop silently; Stage 1 will re-send via `BLOCK_NAK` retries or stress-test error counters.

---

## Protocol dispatch (peripheral side)

### State machine

```
IDLE
  ├─ recv CAP_REQ  → send CAP_RSP, state = AWAIT_CAP_ACK
  ├─ recv PING     → send PONG (echo), stay IDLE
  └─ (other)       → ignore

AWAIT_CAP_ACK
  ├─ recv CAP_ACK  → state = SERVING_BLOCKS, current_block = 0
  ├─ recv PING     → send PONG, stay AWAIT_CAP_ACK
  └─ timeout (1 s) → state = IDLE (Stage 1 will retry handshake)

SERVING_BLOCKS
  ├─ recv SEND_BLOCK(N)
  │   if N == expected_block  → send RECV_BLOCK(N, byte_count, data)
  │   if N != expected_block  → send RECV_BLOCK(N, ...) anyway   (DOS re-sync)
  ├─ recv BLOCK_ACK(N)
  │   if N == expected_block  → current_block += 1
  │   (idempotent on repeats)
  ├─ recv BLOCK_NAK(N)        → no state change; DOS will SEND_BLOCK again
  ├─ recv PING                → send PONG
  └─ (all blocks served)      → state = SERVED (terminal)
```

### Command IDs (`stage1.asm:106-117`)

| CMD | Value | Direction |
|---|---|---|
| `CMD_CAP_REQ` | `0x00` | DOS → Pico |
| `CMD_CAP_RSP` | `0x0F` | Pico → DOS |
| `CMD_CAP_ACK` | `0x0E` | DOS → Pico |
| `CMD_PING` | `0x10` | DOS → Pico |
| `CMD_PONG` | `0x11` | Pico → DOS |
| `CMD_SEND_BLOCK` | `0x20` | DOS → Pico |
| `CMD_RECV_BLOCK` | `0x21` | Pico → DOS |
| `CMD_BLOCK_ACK` | `0x22` | DOS → Pico |
| `CMD_BLOCK_NAK` | `0x23` | DOS → Pico |

### CAP_RSP payload layout (`stage1.asm:191-205`)

| Offset | Size | Field | Value Phase 3 |
|---|---|---|---|
| 0 | u8 | `version_major` | `1` |
| 1 | u8 | `version_minor` | `0` |
| 2-22 | 21 B | (reserved / unused by Stage 1) | zeros |
| 23 | u8 | `active_parallel_mode` | `1` (NEG_MODE_SPP) |
| 24-27 | 4 B | (reserved) | zeros |
| 28 | u32 BE | `stage2_image_size` | embedded blob length |
| 32 | u32 BE | `stage2_image_crc32` | computed at build time |

Total CAP_RSP payload length: 36 bytes (matches `CAP_RSP_MIN_PAYLOAD = 36`).

### Block server

- Block size: 64 bytes (`DOWNLOAD_BLOCK_SIZE` in `stage1.asm:248`).
- Last block may be short (`byte_count = stage2_image_size mod 64` if non-zero).
- `RECV_BLOCK` payload layout: `u32 block_no (BE)` + `u8 byte_count` + `byte_count × data bytes`.
- Total payload length: `5 + byte_count`.

---

## Embedded Stage 2 placeholder

Phase 3 doesn't have a real `PICO1284.EXE` yet (that's the Stage 2 binary, far future work). For bench testing, we embed a small placeholder DOS .COM-style blob:

```rust
const STAGE2_PLACEHOLDER: &[u8] = include_bytes!("../assets/stage2_placeholder.bin");
const STAGE2_CRC32: u32 = /* computed by build.rs */;
```

The placeholder should be a real DOS program that:
1. Prints "PICO1284 Stage 2 placeholder vN.M" to stdout.
2. Inspects the inherited `PICO_BOOT` environment variable and prints its value.
3. Returns errorlevel 0.

A 100-200 byte NASM-built `.COM` is plenty for this. `build.rs` computes the CRC-32 at build time and exposes it as a constant.

Detail: Stage 1 EXECs `PICO1284.EXE`, but `INT 21h AH=4Bh` requires .EXE format. We have options:
- Build the placeholder as a real .EXE (NASM with `format mz`).
- Or, since Stage 1 just calls `AH=4Bh AL=00` and DOS handles both .COM and .EXE based on file content, write a `.COM` blob with the `.EXE` extension. DOS examines the first two bytes — `"MZ"` means .EXE, anything else means .COM.

Going with the .COM-with-.EXE-extension approach for Phase 3 — simpler to build, and DOS handles it.

---

## Module layout

```
firmware/
├── Cargo.toml
├── build.rs                       (compute STAGE2_CRC32 from embedded blob)
├── src/
│   ├── main.rs                    (task spawning, init)
│   ├── lpt.rs                     (SPP nibble bit-bang phy)
│   ├── packet.rs                  (CRC-16-CCITT + encode/decode)
│   ├── protocol.rs                (command dispatcher, state machine)
│   ├── stage2_blob.rs             (embedded placeholder + metadata)
│   └── crc.rs                     (shared CRC-16 + CRC-32 helpers)
└── assets/
    └── stage2_placeholder.bin     (NASM-built; see below)
```

`assets/stage2_placeholder.bin` is built once during firmware build (or pre-built and checked in). For Phase 3 MVP, check it in to avoid pulling NASM into the firmware build path; revisit later if it becomes a problem.

---

## Bring-up phases (in order)

| # | What | Validation |
|---|---|---|
| 3.1 | Pin-direction setup + idle states (status outputs HI/LO per IEEE 1284 idle) | Multimeter on each pin; logic analyzer sanity check. |
| 3.2 | `lpt_recv_byte` (DOS → Pico): wait for nInit edge, latch D0-D7 | Stage 0 test send; defmt-log received bytes. |
| 3.3 | `lpt_send_byte` (Pico → DOS): present low nibble, toggle phase, repeat for high | Stage 0 round-trip self-test (already passes locally; should pass over the wire). |
| 3.4 | Packet decode / encode + CRC-16 | Round-trip via loopback test in firmware; then Stage 1's `packet_self_test` over the wire. |
| 3.5 | CAP_REQ → CAP_RSP responder | Stage 1's `cap_handshake` succeeds; `STAGE1: CAP handshake OK`. |
| 3.6 | PING → PONG echo | Stage 1's `pump_stress_test` reports `0 errors`. |
| 3.7 | Block server with embedded placeholder | Stage 1 downloads, verifies CRC-32, EXECs the placeholder, which prints "Stage 2 placeholder vN.M". |

---

## Forward-looking (out of Phase 3+ scope)

### PS/2 architecture (Phases 1, 2, 11)

When the PS/2-side firmware lands, it **will use PIO oversampling** with continuous instrumentation rather than edge-triggered interrupts. Rationale and details: user preference; see memory `ps2-oversampling-preference`.

**Primary use case** the oversampling enables: auto-detect machine class (XT / AT / PS/2) from observed PS/2 frame timing and structure, eliminating the need for the operator to pre-select a Stage 0 variant. The firmware classifies the host into `MachineClass::{Xt, At, Ps2}` from the first few observed frames, then picks the right Stage 0 .COM (S0_XT/AT/PS2) to inject via DEBUG.

**Instrumentation metadata** emitted per frame: min/max/mean bit period, glitch counts, CLK/DATA edge skew, frame errors (missing start bit, bad parity, missing stop bit). Surfaced via defmt-RTT in dev builds, USB CDC in production.

This decision affects PIO-block budgeting: PS/2 KBD + AUX oversamplers consume 2 SMs (one per channel) plus shared FIFO/DMA infrastructure. Plenty of headroom on the RP2350's 12 SMs across 3 PIO blocks.

### IEEE 1284 real negotiation (Phase 4)

The Pico will respond to negotiation requests by:
1. Latching the xflag byte on the data bus when nSelectIn LO + nAutoFd LO observed.
2. Pulsing nAck LO to acknowledge.
3. Setting Select HI on the next nStrobe pulse iff the requested mode is supported.
4. Setting xflag (status bit 3) to mirror the requested mode for confirmation.

Phase 4 also needs the FIFO-drain-before-mode-switch contract (`ieee1284_controller_reference.md` §"Mode-transition rules").

### EPP/ECP byte pumps (Phase 5)

PIO programs replace bit-bang. One SM per mode (EPP forward, EPP reverse, ECP forward, ECP reverse) — 4 SMs total, leaves 4 SMs free for PS/2 work after Phase 5 lands.

---

## Related documents

- [`design.md`](design.md) §22 Phases 3-5 — overall roadmap
- [`pico_firmware_design.md`](pico_firmware_design.md) — comprehensive Pico-side architecture (this doc is a Phase 3+ implementation slice)
- [`stage1_implementation.md`](stage1_implementation.md) — as-built DOS side that this firmware talks to
- [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) — controller-side dual
- [`hardware_reference.md`](hardware_reference.md) §3.3 — pin allocation
- [`two_plane_transport.md`](two_plane_transport.md) — overall transport architecture
- Memory `ps2-oversampling-preference` — PS/2 architectural decision
