# Two-Plane Transport Architecture

**Status:** Architectural design document  
**Last updated:** 2026-05-12  
**Companion documents:** [`design.md`](design.md) (§9 packet format, §10 capability handshake, §17 PS/2 fallback — this doc refines and supersedes the supervisory framing in §17), [`ps2_private_channel_design.md`](ps2_private_channel_design.md), [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md)

## The load-bearing rule

> **The PS/2 channel is authoritative for session control.**
> **The IEEE 1284 channel is authoritative for bulk byte delivery.**
> **The IEEE 1284 channel may fail independently without killing the session.**
> **The PS/2 channel may command, pause, reset, or re-negotiate the IEEE 1284 channel at any time.**

The slow channel owns correctness. The fast channel owns throughput. Once IEEE 1284 is available, **do not treat PS/2 as a second equal data pipe** — treat it as the always-on supervisor that brings the data plane up, watches it, resets it, throttles it, and recovers from failure.

## Plane assignment

| Plane | Physical channel | Primary purpose |
|---|---|---|
| **Control plane** | PS/2 keyboard + AUX | session control, liveness, mode negotiation, flow-control hints, recovery, abort, attention, low-rate telemetry |
| **Data plane** | IEEE 1284 EPP/ECP | bulk payloads — screen data, files, logs, firmware chunks, trace dumps, compressed video/frame deltas |
| **Emergency plane** | PS/2 only | rescue path if LPT negotiation fails, LPT missing, or EPP/ECP becomes wedged |

This is consistent with the existing PS/2 channel design: keyboard as attention/bootstrap/control, AUX as packetized fallback, PS/2 useful for diagnostics/configuration/control but **not** for firmware uploads, logs, bus traces, or screen dumps. Those stay on IEEE 1284. See [`ps2_private_channel_design.md`](ps2_private_channel_design.md) bandwidth analysis.

## Session architecture

The Pico and DOS-side Stage 0 / pico1284 maintain **one logical vintage-kvm session** with two transports underneath:

```
                 Logical vintage-kvm session
                         |
        +----------------+----------------+
        |                                 |
  Control plane                      Data plane
  PS/2 KBD/AUX                       IEEE 1284 EPP/ECP
  low-rate, robust                   high-rate, bulk
  IRQ1 / IRQ12                       polled PIO, optional ECP DMA
```

**The control plane owns state. The data plane owns bytes.**

## Session state machine

```
BOOTSTRAP
  ↓
PS2_ONLY_SAFE
  ↓
DISCOVER_LPT
  ↓
NEGOTIATE_1284
  ↓
DUAL_PLANE_ACTIVE  ←──────────────────────────────┐
  ↓ 1284 failure                                  │
PS2_FALLBACK_ENTER                                │
  ↓                                               │
PS2_FALLBACK_CONTROL_ONLY ──── 1284 restored ─────┤
  ↓ if stable, recovery not imminent              │
PS2_FALLBACK_MUXED ────────── 1284 restored ──────┘
```

The two fallback sub-states are:

- **`PS2_FALLBACK_CONTROL_ONLY`** — entered immediately on IEEE 1284 failure. Only session control, heartbeat, 1284 recovery negotiation, and small diagnostics flow. **No data services.** Goal: assess whether the data plane can be restored before committing the PS/2 link to degraded data work.
- **`PS2_FALLBACK_MUXED`** — entered if `CONTROL_ONLY` stays stable and 1284 recovery is judged unlikely soon. Multiplexes control + small degraded data over the AUX lane. Explicitly reduced service set (see [§Fallback multiplexing](#fallback-multiplexing-when-ieee-1284-is-down) below).

On 1284 recovery from either sub-state, the session transitions back to `DUAL_PLANE_ACTIVE` and PS/2 returns to its control-only role.

### 1. BOOTSTRAP

Keyboard path types or unlocks the Stage 0 loader. Per [`ps2_eras_reference.md`](ps2_eras_reference.md): XT is LPT-only plus keyboard injection; AT adds bidirectional keyboard / i8042; PS/2 + Super I/O adds AUX via `0xD4` and IRQ12.

### 2. PS2_ONLY_SAFE

Before touching the parallel port aggressively, establish a minimal PS/2 management session:

```
HELLO
VERSION
CAPS?
HOST_CLASS         (XT / AT / PS/2 / SuperIO)
IRQ_MASKED?
AUX_PRESENT?
SAFE_PROFILE       (calibration outcome — see ps2_private_channel_design.md §calibration)
```

This gives a reliable "lifeline" before IEEE 1284 negotiation starts. If the LPT path is going to be hostile, the session is already up and can talk about it.

### 3. DISCOVER_LPT

Use PS/2 to coordinate LPT probing:

```
CP_CMD_LPT_PROBE_BEGIN
CP_CMD_LPT_BASES?        ; 0x378, 0x278, 0x3BC, detected BIOS entries
CP_CMD_LPT_MODE?         ; SPP / EPP / ECP / unknown
CP_EVT_LPT_PROBE_RESULT
```

**Key benefit:** if LPT probing wedges something, the Pico still has PS/2 as a recovery/diagnostic link.

### 4. NEGOTIATE_1284

Use PS/2 as the negotiation coordinator:

```
PS/2 control:
  CP_CMD_1284_NEGOTIATE(mode=EPP_PIO preferred, fallback=ECP_PIO/SPP_PIO,
                        ECP_DMA opt-in if supports_ecp_dma=1 in dp_caps)
  CP_EVT_1284_MODE(mode=EPP_PIO, width=8, crc=on)
  CP_EVT_1284_READY(data_channel_id=N)

IEEE 1284 data:
  starts carrying framed data only after control-plane commit
```

**The control plane is the authority on whether the 1284 plane is considered valid.** The data plane never self-promotes from "negotiating" to "ready" — it waits for the control plane to commit.

### 5. DUAL_PLANE_ACTIVE

Steady-state operation:

```
PS/2:
  heartbeat
  command queue
  ACK/NAK for session-control messages
  data-plane health reports
  pause/resume/reset
  credit/window adjustment
  error reporting

IEEE 1284:
  bulk frames
  stream chunks
  retransmission data
  screen/file/log payloads
```

Credit-based flow control as in `design.md` §17.3. Don't let either lane free-run into the controller.

### 6. DEGRADED_PS2_ONLY / RECOVER_1284

When the data plane stalls (no progress for T ms, CRC storm, EPP timeout bit), the control plane transitions the data plane to `DP_STALLED`, decides whether recovery is possible, and either:

- attempts re-negotiation (`DP_RESETTING` → `DP_READY` on success)
- falls back to PS/2-only (`DEGRADED`) and keeps the session alive

## Control-plane message set

A compact Type-Length-Value protocol over PS/2. Tiny, deterministic, mostly idempotent.

### Core message types

```c
enum cp_msg_type {
    CP_HELLO              = 0x01,
    CP_HELLO_ACK          = 0x02,
    CP_CAPS_REQ           = 0x03,
    CP_CAPS_RSP           = 0x04,

    CP_DP_OPEN            = 0x10,
    CP_DP_OPEN_ACK        = 0x11,
    CP_DP_CLOSE           = 0x12,
    CP_DP_RESET           = 0x13,
    CP_DP_PAUSE           = 0x14,
    CP_DP_RESUME          = 0x15,

    CP_DP_CREDIT_UPDATE   = 0x20,
    CP_DP_WATERMARK       = 0x21,
    CP_DP_HEALTH          = 0x22,
    CP_DP_ERROR           = 0x23,

    CP_JOB_START          = 0x30,
    CP_JOB_CANCEL         = 0x31,
    CP_JOB_DONE           = 0x32,
    CP_JOB_FAIL           = 0x33,

    CP_OOB_ATTENTION      = 0x40,
    CP_OOB_ABORT          = 0x41,
    CP_OOB_RESYNC         = 0x42,

    CP_TIME_SYNC          = 0x50,
    CP_TRACE_MARK         = 0x51,
    CP_DEBUG_TEXT         = 0x52,
};
```

### Control frame

```c
struct cp_frame {
    uint8_t  magic;        // 0xA5
    uint8_t  version;
    uint8_t  type;         // cp_msg_type
    uint8_t  seq;
    uint8_t  ack;
    uint8_t  flags;
    uint8_t  len;
    uint8_t  payload[len];
    uint16_t crc16;        // CRC-16-CCITT over magic..payload
};
```

### Transport encoding over PS/2

| Lane | Use |
|---|---|
| **Keyboard lane** (IRQ1) | Urgent / small control only: `CP_OOB_ATTENTION`, `CP_OOB_ABORT`, `CP_OOB_RESYNC`, heartbeat ping. Each encoded as `E0` + reserved scan-code symbol. The keyboard port is too semantically loaded (BIOS/INT 9 assumptions, scan-code translation) to carry structured control reliably. |
| **AUX lane** (IRQ12) | All structured `cp_frame` messages. Encoded via the spec-legal mouse-packet format in [`ps2_private_channel_design.md`](ps2_private_channel_design.md) §Frame format. |

### Relationship to `design.md` §9 packet format

`design.md` §9 defines a single packet format with command IDs 0x00–0x81 covering both control and data semantics. In the two-plane model, those command IDs split:

- **Control-plane (over PS/2):** `cp_msg_type` opcodes (new namespace, 0x01–0x52). Covers what §9 calls `CAP_REQ/RSP/ACK`, `PING/PONG/RESET_SESSION`, `ERROR`, `CREDIT`, `ACK/NAK`.
- **Data-plane (over IEEE 1284):** `dp_frame` per-stream (below). Covers what §9 calls `SEND_BLOCK`, `FILE_*`, `SCREEN_*`, `CONSOLE_*`, `MEM_*`, `EXEC_*`, `DICT_SELECT`, `CODEC_SELECT`.

The §9 numbering remains the authoritative source for *operation* IDs; this doc adds the *transport routing* on top.

## Data-plane frame format

```c
struct dp_frame {
    uint32_t magic;        // 'VKDP'
    uint8_t  version;
    uint8_t  stream_id;
    uint8_t  type;
    uint8_t  flags;
    uint16_t header_len;
    uint16_t payload_len;
    uint32_t seq;
    uint32_t ack_hint;
    uint32_t crc32;        // CRC-32 over header + payload
    uint8_t  payload[payload_len];
};
```

### Stream IDs

| ID | Stream | Purpose |
|---|---|---|
| 0 | control mirror / reserved | optional in-band echo of CP state for debug |
| 1 | screen / framebuffer | screen dump payload (text-mode diff, VESA tiles) |
| 2 | KBD / mouse injection | Pico → host-side TSR keyboard/mouse events |
| 3 | file transfer | FILE_DATA blocks |
| 4 | diagnostics / logs | structured log records, traces |
| 5 | firmware / update chunks | Stage 2+ DOS-side binary delivery |
| 6 | bus trace / capture | logic-analyzer-style capture frames |
| 7 | debug console | bidirectional text terminal |

Streams 8–255 reserved for future use. The control plane opens and closes streams; the data plane carries stream payloads.

## Supervision patterns

The PS/2 control plane's most valuable role is **out-of-band supervision of the data plane**.

### Heartbeat

The PS/2 heartbeat continues even when the parallel port is saturated:

```c
CP_DP_HEALTH {
    session_id,
    dp_state,
    last_dp_seq_rx,
    last_dp_seq_tx,
    rx_queue_depth,
    tx_queue_depth,
    error_count,
}
```

If IEEE 1284 wedges, PS/2 still reports:

```
DP_STALLED
last_good_seq = 0x0012A932
reason = timeout / bad_crc / ecp_fifo_stuck / epp_timeout
```

### Pause / resume

When DOS-side buffers are under pressure:

```
PS/2:        CP_DP_PAUSE(stream_id=screen)
IEEE 1284:   stop sending stream 1 frames
PS/2:        CP_DP_RESUME(stream_id=screen, credit=N)
```

More robust than relying only on in-band flow control inside the bulk stream — the pause command lands on a channel that isn't itself blocked.

### Reset without losing session

If IEEE 1284 gets wedged:

```
PS/2:        CP_DP_RESET(reason=EPP_TIMEOUT)
Pico:        tri-state / re-initialize 1284 transceiver
Host:        reprogram LPT/ECP/EPP registers
PS/2:        CP_DP_OPEN(mode=EPP)
IEEE 1284:   resume at seq=N+1 (or request retransmit)
```

**This is the main architectural win.** The high-speed channel doesn't have to recover itself over a broken pipe.

### Abort

Keyboard lane gives a hard out-of-band abort:

```
KBD lane:    E0 <ABORT_SCAN>
AUX control: CP_OOB_ABORT(job_id)
IEEE 1284:   stop current transfer immediately
```

Useful for screen-capture loops, large dumps, and firmware transfer attempts that need to stop now.

## Division of reliability responsibilities

Do **not** duplicate all reliability logic equally on both planes.

| Responsibility | PS/2 control plane | IEEE 1284 data plane |
|---|---|---|
| Session identity | **Authoritative** | Mirrors session ID |
| Capability negotiation | **Authoritative** | Reports measured mode |
| Flow-control policy | **Authoritative** | Executes credits / windows |
| Bulk sequence numbers | Tracks coarse / high-water | Tracks per-frame / per-stream |
| CRC | CRC-16 enough | CRC-32 per bulk frame |
| Retransmission decision | **Commands** retransmit / range | Carries retransmitted data |
| Liveness | **Authoritative** heartbeat | Optional in-band ping |
| Recovery | **Authoritative** reset / resync | Resettable subordinate |
| Emergency fallback | **Yes** | No |

## Runtime states for the data plane

```c
enum dp_state {
    DP_DOWN,
    DP_PROBING,
    DP_NEGOTIATING,
    DP_READY,
    DP_ACTIVE,
    DP_PAUSED,
    DP_STALLED,
    DP_RESETTING,
    DP_DEGRADED,
};
```

**The PS/2 control plane is the only side allowed to change `dp_state`.**

Transition examples:

```
DP_ACTIVE  → DP_STALLED
  Trigger: no data-plane progress for T ms, CRC storm, EPP timeout bit

DP_STALLED → DP_RESETTING
  Trigger: CP decides recovery is possible

DP_RESETTING → DP_READY
  Trigger: IEEE 1284 renegotiation succeeds

DP_RESETTING → DP_DEGRADED
  Trigger: renegotiation fails; continue PS/2-only
```

## Concrete examples

### Screen update transfer

```
1. PS/2 CP_JOB_START(type=SCREEN_DELTA, stream=1)
2. PS/2 CP_DP_CREDIT_UPDATE(stream=1, bytes=32768)
3. IEEE 1284 sends DP_FRAME stream=1 seq=100..N
4. PS/2 heartbeat reports:
     last_dp_seq_rx=N
     rx_queue_depth=...
     display_applied_seq=...
5. If host falls behind:
     PS/2 CP_DP_PAUSE(stream=1)
6. When ready:
     PS/2 CP_DP_RESUME(stream=1, credit=32768)
7. Completion:
     IEEE 1284 sends final DP_FRAME(type=END)
     PS/2 CP_JOB_DONE(job_id, final_seq=N)
```

For display/screen data, the PS/2 channel **never carries pixels** except maybe a tiny diagnostic "panic frame." It carries which stream, which frame range, which codec/compression mode, and whether to pause/resume.

### IEEE 1284 failure recovery

```
Normal:
  IEEE 1284 seq 2000, 2001, 2002...

Problem:
  Host sees EPP timeout / Pico sees bad handshake

PS/2:
  CP_DP_ERROR(code=EPP_TIMEOUT, last_good_seq=2001)
  CP_DP_PAUSE(all)
  CP_DP_RESET(mode=EPP)

Recovery:
  Host reinitializes LPT
  Pico resets 1284 state machine
  PS/2 CP_DP_OPEN(mode=EPP, resume_seq=2002)

IEEE 1284:
  retransmit from seq=2002
```

Much cleaner than trying to recover in-band over the same broken LPT path.

### Bootstrap acceleration

```
1. Keyboard PS/2 gets Stage 0 loaded.
2. Stage 0 claims i8042, masks IRQ1/IRQ12, probes AUX.
3. PS/2 negotiates a management session (PS2_ONLY_SAFE).
4. PS/2 asks Pico for the preferred IEEE 1284 mode.
5. IEEE 1284 loads the larger Stage 1 / TSR quickly.
6. PS/2 remains active as the watchdog / control channel.
```

This directly follows the era split in [`ps2_eras_reference.md`](ps2_eras_reference.md): XT remains special (no i8042/AUX); AT adds bidirectional keyboard / i8042; PS/2 + Super I/O adds AUX via `0xD4` and IRQ12.

## Buffering and priority

### On the Pico

```
control_rx_q       small, high priority
control_tx_q       small, high priority
dp_rx_ring         large, bulk
dp_tx_ring         large, bulk
emergency_q        tiny, highest priority
```

**Priority order:**

1. PS/2 emergency keyboard attention / abort
2. PS/2 AUX control frames
3. IEEE 1284 control mirror frames, if any
4. IEEE 1284 bulk data

### On DOS

See [§DOS-side concurrency model](#dos-side-concurrency-model) below for the full design. Summary:

```
IRQ1 handler:        emergency keyboard symbols only — tiny, set flags, return
IRQ12 handler:       AUX bytes → ring buffer with bounded drain — tiny, set flags, return
Foreground loop:     parse CP frames, pump IEEE 1284 in bounded slices,
                     check control flags between slices, respect dp_state
```

Keep ISRs tiny. The resident driver or polling loop processes frames outside interrupt context, and IEEE 1284 is **deliberately not interrupt-driven** on slow DOS hosts.

## DOS-side concurrency model

A slow DOS machine (8088/286/386/486 class) should not handle PS/2 and IEEE 1284 as two fully concurrent, preemptive high-level channels. Handle them as:

```
PS/2     = interrupt-driven, tiny, high-priority event/control path
IEEE 1284 = foreground or cooperative bulk pump
```

In other words: **interrupts capture events; the main loop does the work.** No full frame parsing, CRC, retransmission, decompression, screen processing, or file I/O ever happens inside the PS/2 interrupt handlers.

The bandwidth mismatch is actually helpful here. PS/2 is slow (200–2000 B/s — see [`ps2_private_channel_design.md`](ps2_private_channel_design.md)); IEEE 1284 EPP/ECP is much faster but can be pumped cooperatively from a DOS loop. The slow PS/2 ISRs only need to enqueue work and set flags; the foreground 1284 pump does the actual transport.

### Architecture overview

```
                +------------------------+
IRQ1 ---------> | kbd_isr                |
                | set emergency flags    |
                +------------+-----------+
                             |
IRQ12 --------> +------------v-----------+
                | aux_isr                |
                | drain 0x60, ring buffer|
                +------------+-----------+
                             |
                +------------v-----------+
                | foreground scheduler   |
                | parse CP frames        |
                | pump IEEE 1284         |
                | run jobs               |
                +------------------------+
```

### ISR rules

**IRQ1 (keyboard lane) — emergency control only:**

Used for out-of-band symbols: `ABORT`, `ATTENTION`, `RESYNC`, mode switch, bootstrap marker.

```asm
kbd_isr:
    in   al, 64h        ; status
    test al, 01h        ; OBF?
    jz   .ack
    in   al, 60h        ; read scan code
    ; classify tiny symbol against reserved E0-prefixed table
    ; set bit in kbd_event_flags
.ack:
    mov  al, 20h
    out  20h, al        ; EOI to PIC1
    iret
```

No dynamic allocation, no loops except draining one or two bytes, no `INT 21h`, no BIOS calls, no `printf`, no file I/O.

**IRQ12 (AUX lane) — bounded ring-buffer drain:**

```asm
aux_isr:
    push ax
    push bx
    push cx
    mov  cx, 16          ; bounded max reads per IRQ — prevents starving LPT
.drain:
    in   al, 64h
    test al, 21h         ; OBF + AUX bit (bit 5 = AUX data available)
    jz   .done
    test al, 20h         ; AUX bit
    jz   .done
    in   al, 60h         ; read AUX byte
    ; push al into aux_ring (lock-free: producer = ISR, consumer = foreground)
    loop .drain
    ; if ring near full, set FLOW_STOP flag for foreground to send CP_DP_PAUSE
.done:
    mov  al, 20h
    out  0A0h, al        ; EOI to PIC2 (slave)
    out  20h, al         ; EOI to PIC1 (master) — IRQ12 cascades through slave
    pop  cx
    pop  bx
    pop  ax
    iret
```

**Critical:** IRQ12 cascades through the slave PIC, so the handler must EOI both PIC2 (port `0xA0`) *and* PIC1 (port `0x20`). The bounded drain (16 bytes default) prevents AUX traffic from starving the LPT pump on a slow 286/386.

### Foreground scheduler

The TSR / Stage 1 / pico1284 main loop is cooperative:

```c
while (!done) {
    /* 1. Emergency first — drains keyboard-lane flags */
    if (kbd_flags & KBD_ABORT) {
        cancel_current_job();
        send_cp_abort_ack();
        continue;
    }
    if (kbd_flags & KBD_RESYNC) {
        ps2_resync();
        continue;
    }

    /* 2. Decode queued AUX control bytes */
    while (aux_ring_has_frame()) {
        struct cp_frame cp = aux_decode_frame();
        handle_control_frame(&cp);
    }

    /* 3. Apply control decisions */
    if (dp_state == DP_RESETTING) {
        reset_lpt_1284();
        continue;
    }
    if (dp_state == DP_PAUSED) {
        maybe_send_heartbeat();
        continue;
    }

    /* 4. Pump bulk data with bounded budget */
    if (dp_state == DP_ACTIVE) {
        pump_1284_budget(current_budget);
    }

    /* 5. Low-priority fallback data */
    if (dp_state == DP_DEGRADED) {
        pump_ps2_fallback_data_small_budget();
    }

    /* 6. Periodic control maintenance */
    maybe_send_credit_update();
    maybe_send_heartbeat();
}
```

The key call is `pump_1284_budget(N)` — not "pump forever." After each bounded slice the scheduler returns and re-checks PS/2 flags.

### Priority model (strict, bounded)

```
1. IRQ1 emergency flag                          ← preempts everything at slice boundary
2. AUX control frames
3. ACK/NAK / flow control
4. IEEE 1284 recovery commands
5. IEEE 1284 bulk pump                          ← bounded slices
6. PS/2 fallback data jobs
```

**During `DUAL_PLANE_ACTIVE`:** PS/2 control always preempts IEEE 1284 bulk at slice boundaries.  
**During `PS2_FALLBACK_MUXED`:** PS/2 control preempts PS/2 fallback data at frame boundaries.

### LPT slice budget per CPU class

The `pump_1284_budget(N)` slice size is machine-dependent:

| CPU class | Bytes per LPT slice | Rationale |
|---|---:|---|
| 8088 / XT | n/a (LPT-only, no concurrent PS/2 control plane) | — |
| 286 | 32–128 | Tight scheduling latency; small AUX rings |
| 386 | 128–512 | Comfortable dual-lane; ~1 MIPS spare |
| 486+ | 512–2048 | Plenty of CPU for slice + control servicing |

Adaptive sizing rules:

```
if aux_ring_fill > high_water:    reduce slice size
if aux_ring_fill == 0 for K cycles: increase slice size
if CP heartbeat overdue:           reduce bulk budget
```

### Buffer sizing per CPU class

```
kbd_event_flags:       1–2 bytes       (bit flags)
aux_rx_ring:           128–512 bytes   (smaller on 286)
aux_tx_ring:           64–256 bytes
cp_frame_queue:        4–8 frames
lpt_rx_ring:           2–16 KB         (2 KB on 286, 16 KB on 486+)
lpt_tx_ring:           2–16 KB
```

8088/286-class: prefer smaller rings + tighter flow control.  
386+: 8–16 KB LPT rings are reasonable.

### Flow control is mandatory

The slow DOS host advertises credits based on available buffer space:

```
CP_DP_CREDIT_UPDATE(bytes_available)
CP_DP_PAUSE(stream_id)
CP_DP_RESUME(stream_id, credit)
```

AUX fallback window (per [`§Fallback multiplexing`](#fallback-multiplexing-when-ieee-1284-is-down)):

```
4–8 frames outstanding
16–64 bytes per data chunk
```

IEEE 1284 window:

```
sized by lpt_rx_ring free space
larger frames allowed
pause/resume commanded over PS/2 (control plane is authoritative)
```

### Concurrency model per host class

| Host class | Concurrency |
|---|---|
| **XT (8088)** | No concurrent PS/2 control plane. LPT/nibble/SPP is the *only* data path. Keyboard lane bootstraps the loader; that's all the keyboard does after Stage 0 entry. (Matches XT row in [`ps2_eras_reference.md`](ps2_eras_reference.md).) |
| **AT (286+)** | KBD PS/2 = tiny control/unlock path only (no AUX). LPT = data path. Be conservative — DOS software on 286 may be timing-sensitive, and IRQ1 must stay very small. |
| **PS/2 / Super I/O (386+)** | First comfortable dual-lane setup: KBD IRQ1 = emergency/control; AUX IRQ12 = control + fallback fragments; LPT/EPP/ECP = data. Super I/O is protocol-identical to PS/2 from the peripheral side. |
| **486 / Pentium** | Same architecture as 386. Increase LPT slice + buffer sizes, **but do not make the ISR heavier just because the machine is faster.** ISR discipline is constant; only the foreground scheduler scales. |

### Timing strategy

Two simple rules:

1. **Never spend more than a small bounded time in an ISR.**
2. **Never spend more than one LPT slice without checking PS/2 control flags.**

Every 1284 slice:

```
transfer up to N bytes
check aux_ring
check kbd emergency flags
handle pause/reset/abort
return to scheduler
```

On slow hosts, `N` is adaptive based on AUX ring fill and CP heartbeat freshness (see [§LPT slice budget per CPU class](#lpt-slice-budget-per-cpu-class) above).

### DOS reentrancy traps to avoid

DOS is not reentrant. The ISRs must never:

- Call `INT 21h` (any DOS service)
- Write files
- Allocate memory
- Call BIOS keyboard/mouse services (we've replaced those)
- Call BIOS `INT 16h` / `INT 33h`
- Do anything that touches DOS internal state

ISR-allowed operations:

```
read port
write ring buffer
set flag
ACK PIC
return
```

Any DOS calls happen only in foreground context, never from interrupt context.

### IEEE 1284 stays deliberately non-interrupt-driven

On a slow DOS host, avoid making IEEE 1284 interrupt-driven even when the LPT chipset supports it (most do, via the IRQ in the control register). Reasons:

- Interrupt nesting on old PIC-based systems causes latency spikes.
- Two interrupt sources competing for the same slow CPU produces unpredictable timing.
- The foreground polling loop already runs at a cadence that matches the LPT data rate fine.

**Rule:** only the slow control path (PS/2) gets IRQ priority. The fast data path (IEEE 1284) is deliberately cooperative/polled. This rule applies to ECP DMA as well: DMA completion is **polled via terminal count / ECR status**, not signalled by an LPT IRQ. See [§Data-plane modes and optional ECP DMA](#data-plane-modes-and-optional-ecp-dma) below.

### Final design rule

> **PS/2 interrupts decide what must happen.**  
> **The foreground scheduler decides when to do it.**  
> **IEEE 1284 moves bytes only in bounded slices.**

This gives responsive abort/reset/control semantics without overwhelming 286/386-era machines or violating DOS reentrancy constraints.

## Data-plane modes and optional ECP DMA

The data plane is not a single transport — it is a small set of host-side LPT personalities with very different throughput, complexity, and reliability profiles. **PS/2 never uses DMA.** DMA is an **optional ECP acceleration mode**, not a baseline transport.

### Mode set

```c
enum dp_mode {
    DP_MODE_EPP_PIO   = 1,   // default fast path
    DP_MODE_ECP_PIO   = 2,   // fallback if EPP unavailable but ECP works
    DP_MODE_ECP_DMA   = 3,   // optional acceleration on validated hosts
    DP_MODE_SPP_PIO   = 4,   // last-resort byte-at-a-time (compat / nibble)
};
```

| Mode | DMA | Notes |
|---|---|---|
| `EPP_PIO` | No | Fast handshaked PIO. CPU-driven `in`/`out`/`outs` loops. Default. |
| `ECP_PIO` | No | ECP FIFO drained by CPU. Useful when EPP isn't available but ECP is. |
| `ECP_DMA` | Yes | ECP FIFO ↔ ISA DMA channel ↔ DOS bounce buffer. Opt-in, calibrated. |
| `SPP_PIO` | No | Byte-at-a-time SPP / nibble. Rescue path only. |

This list extends the ECR mode space from [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) (`ECR_SPP/PS2/PPF/ECP/EPP`) with an explicit PIO-vs-DMA dimension that is invisible to the peripheral side: from the Pico's wire view, `ECP_PIO` and `ECP_DMA` are both just ECP — only the host's transport implementation differs.

### Why PS/2 has no DMA path

PS/2 KBD/AUX bytes arrive through the i8042-compatible controller at port `0x60` with status at `0x64`, signalled by IRQ1 (KBD) and IRQ12 (AUX via `0xD4`). There is no PC DMA channel attached to the i8042 output buffer. PS/2 therefore stays IRQ-driven byte capture into a small ring buffer — which is fine, because PS/2 is the control/safety plane, not the bulk pipe. See [`ps2_eras_reference.md`](ps2_eras_reference.md) for the per-era resource table.

### Why DMA is only plausible for ECP

The parallel port has three broad personalities, and only one is DMA-friendly:

| Mode | DMA suitability | Reason |
|---|---|---|
| SPP / compatibility | Poor | Byte-at-a-time programmed I/O, no FIFO. |
| EPP | Usually poor | Fast PIO handshake; typically CPU-driven `in`/`out` loops. No standard DMA path. |
| ECP | **Good candidate** | Designed with FIFO and optional DMA/IRQ support; the only mode that ISA DMA naturally fits. |

So the useful DMA target is exactly:

```
ECP FIFO ↔ ISA DMA / chipset DMA ↔ DOS bounce buffer
```

### When DMA is worth it

DMA helps for **larger, linear payloads** and hurts for small messages. Sizing thresholds (tune empirically):

| Payload size | Mode |
|---|---|
| < 256 B | PIO (DMA setup dominates) |
| 256 B – 2 KiB | Probably PIO unless CPU is very slow |
| > 2–4 KiB | Consider ECP DMA |

DMA helps:

- Pico → host: screen deltas, file chunks, trace dumps, compressed logs, firmware/image chunks.
- Host → Pico: larger command payloads, file uploads, firmware/update chunks, configuration blobs.

DMA does **not** help (and adds risk) for: small commands, ACK/NAK, control messages, short register reads, PS/2 fallback traffic. Those stay PIO or PS/2.

### DOS DMA constraints

Classic PC DMA imposes constraints that the host driver must enforce — these are the dominant source of "works on my machine, breaks on clones" failures.

**1. 64 KiB page boundary.** ISA DMA cannot cross a 64 KiB physical DMA page boundary in a single transfer. The bounce buffer must satisfy:

```
(physical_start & 0xFFFF) + length <= 0x10000
```

Safest strategy: allocate a dedicated bounce buffer (16 KiB or 32 KiB) at install time, aligned so it never crosses a 64 KiB DMA boundary, in conventional memory. Do not DMA directly into arbitrary XMS/protected-mode buffers — use the bounce and `memcpy` from foreground context.

**2. DMA channel assignment.** ECP parallel ports historically use DMA 1 or DMA 3, but channel selection depends on BIOS/PnP/ECP configuration and clone-chipset behavior. Potential conflicts: DMA 1 with sound cards / 8-bit devices, DMA 3 sometimes claimed by other LPT/ECP or tape devices, DMA 5/6/7 are 16-bit and less relevant for classic 8-bit ECP FIFO paths. The driver must either read BIOS/PnP/ECP configuration, accept an operator-supplied channel at install, or probe conservatively — **never blindly assume a channel.**

**3. Cache coherency on 486+ / Pentium.** Chipset behavior with ISA DMA and CPU caches varies. Conventional low memory is usually safe; arbitrary XMS / protected-mode buffers are not. The bounce-buffer strategy above also resolves this concern.

**4. Transfer granularity.** DMA setup overhead is non-trivial on a 286/386. Honor the size thresholds in [§When DMA is worth it](#when-dma-is-worth-it) — do not DMA tiny frames.

### Capability discovery and mode negotiation

During data-plane setup, PS/2 control coordinates discovery and the Pico chooses the preferred mode after the host reports capability. Probe order:

1. PS/2 control plane: `CP_CMD_LPT_PROBE_BEGIN`.
2. Host probes LPT base candidates: `0x378`, `0x278`, `0x3BC`, plus BIOS Data Area entries.
3. Host detects EPP/ECP capability via standard ECR/CTR probes ([`ieee1284_controller_reference.md`](ieee1284_controller_reference.md)).
4. Host reports a `dp_caps` capability frame to the Pico.
5. Pico selects preferred mode (typically `ECP_DMA` if reported and validated, else `EPP_PIO`).
6. Host validates the chosen mode with a small calibration transfer.
7. PS/2 commits the data-plane mode via `CP_DP_OPEN`.

Capability frame:

```c
struct dp_caps {
    uint16_t lpt_base;            // 0x378 / 0x278 / 0x3BC / ...
    uint8_t  supports_spp     : 1;
    uint8_t  supports_epp     : 1;
    uint8_t  supports_ecp     : 1;
    uint8_t  supports_ecp_dma : 1;
    uint8_t  reserved         : 4;
    uint8_t  irq;                 // LPT IRQ, or 0xFF if none/unknown
    uint8_t  dma_channel;         // 0xFF if none/unknown
    uint8_t  fifo_depth;          // ECP FIFO depth, if known
    uint16_t max_dma_block;       // largest single DMA transfer the host will allow
};
```

This extends, rather than replaces, the §10 capability handshake in [`design.md`](design.md): `dp_caps` is a data-plane-specific extension of the global capability blob, carried during `CP_CAPS_RSP`.

### Control-plane integration of DMA state

PS/2 owns DMA lifecycle. The data plane never decides on its own to start, stop, or recover a DMA transfer.

```
CP_DP_OPEN(mode=ECP_DMA, dma_ch=3, buf=phys_addr, len=N, max_block=4096)
CP_DP_PAUSE
CP_DP_RESUME
CP_DP_RESET
CP_DP_ERROR(code=DMA_TC_TIMEOUT | DMA_BOUNDARY_WRAP | ECP_FIFO_STUCK | ...)
CP_DP_FALLBACK(mode=ECP_PIO | EPP_PIO | SPP_PIO)
```

Recovery semantics fall straight out of the existing rule that **only the PS/2 control plane changes `dp_state`** (see [§Runtime states for the data plane](#runtime-states-for-the-data-plane)). A DMA failure transitions `DP_ACTIVE → DP_STALLED` and PS/2 then either renegotiates the same mode, falls back to a lower mode, or degrades to PS/2-only.

### Concurrency: keep the model, change only the pump

ECP DMA does **not** change the [§DOS-side concurrency model](#dos-side-concurrency-model). It changes only the implementation of `pump_1284_budget(N)`:

```c
// PIO variant (EPP_PIO / ECP_PIO)
void pump_1284_budget_pio(unsigned budget) {
    while (budget > 0 && tx_has_bytes() && lpt_ready()) {
        outb(lpt_data_port, tx_next_byte());
        budget--;
    }
}

// DMA variant (ECP_DMA)
void pump_1284_budget_dma(unsigned budget) {
    if (!dma_in_flight) {
        size_t chunk = min3(budget, max_dma_block, tx_contig_bytes());
        if (chunk == 0) return;
        dma_program(chunk);     // program 8237, set ECR to ECP, kick FIFO
        dma_in_flight = chunk;
        return;                 // yield; foreground checks completion next slice
    }
    if (dma_terminal_count() || ecr_fifo_empty_after_drain()) {
        tx_commit(dma_in_flight);
        dma_in_flight = 0;
    } else if (dma_deadline_expired()) {
        signal_cp_dp_error(DMA_TC_TIMEOUT);
        dma_in_flight = 0;
    }
}
```

Two invariants hold across both variants:

1. **Bounded slices.** A DMA chunk is sized to the slice budget (and to `max_dma_block`), then the scheduler returns and re-checks PS/2 flags. A single transfer never monopolises the CPU.
2. **Polled completion.** Terminal count and ECP status are polled — the LPT IRQ is **not** wired to an ISR. This preserves the "only PS/2 gets IRQ priority" rule above.

Chunk sizing rules:

- 4 KiB chunks on cautious hosts.
- 8–16 KiB chunks on known-good 386/486+ (subject to `max_dma_block` from `dp_caps`).
- **Never cross a 64 KiB DMA page boundary** inside a single transfer — split at the boundary instead.
- Each chunk carries a `dp_frame` sequence number and CRC-32; PS/2 supervises chunk commit via `CP_DP_CHUNK_OK(seq)` or in-band ACK plus periodic PS/2 cross-check.

### DMA failure modes and graceful degradation

DMA adds failure modes that PIO does not have:

- DMA terminal count never asserts (wrong channel, wrong direction, masked).
- ECP FIFO underrun / overrun.
- 64 KiB boundary wrap (driver bug; must not happen if allocator is correct).
- Stale bounce-buffer contents.
- Chipset advertises ECP but implements it badly.
- Wrong DMA direction bit.

Degradation ladder (PS/2 drives the transition each step):

```
ECP_DMA  →  ECP_PIO  →  EPP_PIO  →  SPP_PIO / nibble  →  PS2_ONLY
```

Do not spend much code attempting to salvage a broken DMA mode on unknown clone hardware. Two failed retries on the same mode is a strong signal to step down the ladder.

### Architectural rule for DMA

> **PS/2 never uses DMA.**  
> **IEEE 1284 EPP defaults to bounded PIO.**  
> **IEEE 1284 ECP may use DMA for large chunks after capability detection and validation.**  
> **PS/2 remains the supervisor that can pause, reset, or downgrade the DMA data plane.**

### Phased rollout

DMA is not in the v1 transport. It comes in only after the PIO baseline is real and the control-plane recovery story is proven.

| Phase | Modes supported | Notes |
|---|---|---|
| v1 | `EPP_PIO` + PS/2 control + `SPP_PIO` rescue | First end-to-end transport. No ECP work yet. |
| v2 | adds `ECP_PIO` | FIFO-aware pump; validates ECP mode-transition contract from [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md). |
| v3 | adds `ECP_DMA` | Opt-in via `dp_caps`; calibrated per host. |

## Striping policy when IEEE 1284 is up

**Should PS/2 AUX carry data when IEEE 1284 is up?** Only in three cases:

1. Control-plane payloads too large for the keyboard lane.
2. Rescue data while IEEE 1284 is down (covered in [§Fallback multiplexing](#fallback-multiplexing-when-ieee-1284-is-down) below).
3. Redundant metadata for high-value transfers — e.g., final hash, last-good sequence, job result.

Otherwise, **do not stripe bulk data across PS/2 and IEEE 1284.** The rate mismatch is too large (PS/2 ~ 200–1500 B/s vs IEEE 1284 ~ 2–8 MB/s), ordering becomes messy, and PS/2's main value is independence from the data plane.

## Fallback multiplexing when IEEE 1284 is down

In `PS2_FALLBACK_MUXED`, PS/2 carries both control and degraded data. **Multiplex by frame, never by byte** — no untyped byte stream with escape codes. The PS/2 fallback transport is latency-bounded and recoverable, not high-throughput.

### Lane assignment in fallback

```
Keyboard lane / IRQ1:
  emergency control only
  attention, abort, resync, mode switch, bootstrap

AUX lane / IRQ12:
  multiplexed framed transport
  control frames + degraded data frames
```

The keyboard lane stays reserved for out-of-band emergency — it remains the only path that survives if AUX itself gets stuck.

### Frame classification

The existing AUX frame format from [`ps2_private_channel_design.md`](ps2_private_channel_design.md) carries a frame-type field in the IntelliMouse byte3 metadata. Extended for fallback multiplexing:

```c
enum ps2_frame_class {
    PS2_CLASS_CONTROL   = 0x0,
    PS2_CLASS_DATA      = 0x1,
    PS2_CLASS_ACK       = 0x2,
    PS2_CLASS_MGMT      = 0x3,
};

struct ps2_fallback_frame {
    uint8_t  magic;
    uint8_t  version;
    uint8_t  frame_class;   // ps2_frame_class
    uint8_t  stream_id;
    uint8_t  seq;
    uint8_t  ack;
    uint8_t  flags;
    uint8_t  len;
    uint8_t  payload[len];
    uint16_t crc16;
};

// flags byte
//   bit 0: urgent
//   bit 1: checkpoint
//   bit 2: retransmit
//   bit 3: end_of_job
```

These frames are then mouse-packet-encoded onto the AUX lane per the wire-format design.

### Scheduler priority

Strict priority with fairness caps:

1. Emergency keyboard-lane events (KBD IRQ1 wakes a flag)
2. AUX `PS2_CLASS_CONTROL` frames
3. AUX `PS2_CLASS_ACK` / `PS2_CLASS_MGMT` frames (flow control, heartbeat)
4. AUX `PS2_CLASS_DATA` (small interactive — debug console responses, command results)
5. AUX `PS2_CLASS_DATA` (bulk/degraded — job chunks)

**The crucial rule:** *a pending control frame must be able to preempt data between PS/2 frames.* Not mid-byte (we never interrupt a partial mouse packet), but at the next packet/frame boundary.

### Reserved scheduling capacity

Data credits must never consume all transport capacity. Reserve a minimum control budget per scheduling window:

```
8-packet scheduling window (default):
  4 packets — data
  2 packets — ACK / flow control
  1 packet  — control
  1 packet  — emergency / mgmt reserve

If no control/mgmt is pending in a slot, data may borrow it.
```

This guarantees heartbeat, abort, and `CP_DP_RESET` always have a transmission opportunity within ~one window even under data saturation.

### Stream IDs in fallback

Keep logical stream IDs so the muxed transport is compatible with the normal data-plane architecture:

```
stream 0 = control / session
stream 1 = emergency console / debug text
stream 2 = small command responses
stream 3 = file fragments — tiny only
stream 4 = diagnostic summaries
stream 5 = 1284 recovery negotiation
stream 6 = screen panic frame / text-mode status only
```

**Do not carry normal screen/video bulk over PS/2 fallback** except for a tiny diagnostic mode. Streams 0–4 in this table reuse semantics from the main data-plane stream IDs where possible; stream 5 is fallback-specific.

### Chunking and job migration

Data chunks in fallback are aggressively small (16–64 bytes before ACK/checkpoint) to keep retransmission cost low and control latency bounded.

Large operations become **job-based and resumable**:

```
JOB_START job_id, type, total_size
DATA_CHUNK job_id, seq=0, payload (≤64 B)
DATA_CHUNK job_id, seq=1, payload
…
JOB_CHECKPOINT job_id, range, hash
JOB_DONE job_id
```

When IEEE 1284 recovers, the same job can migrate back to the data plane mid-flight:

```
PS/2:        CP_DP_RECOVERED
             MIGRATE_JOB job_id, next_offset
IEEE 1284:   resumes bulk transfer from next_offset
```

This is the main architectural win of muxed fallback: degraded service continues, and recovery is non-disruptive.

### Semantic compression for diagnostics

Because PS/2 fallback is painfully slow, prefer semantic summaries over raw data wherever possible:

| Instead of | Send |
|---|---|
| full log dump | error code + last N events + counters + first bad seq + CRC mismatch details |
| full screen frame | screen-mode change notice + text-mode status row |
| full memory dump | hash + addresses of differing regions + first 64 B of each |
| firmware blob | refusal to fallback (firmware updates require IEEE 1284) |

For diagnostics, the summary is usually enough either to recover IEEE 1284 or to tell the user what failed.

### Scheduling example

```
Pending queues:
  control_q:   CP_DP_RESET_1284, CP_DP_HEALTH
  ack_q:       ACK seq=42
  data_q:      debug chunk 12, debug chunk 13

Transmit order:
  1. CONTROL: CP_DP_RESET_1284
  2. ACK:     ACK seq=42
  3. CONTROL: CP_DP_HEALTH (heartbeat)
  4. DATA:    debug chunk 12
  5. DATA:    debug chunk 13
```

If the keyboard lane emits `ABORT` mid-sequence:

```
KBD IRQ1: ABORT marker
AUX scheduler:
  finish current mouse-packet fragment (don't truncate mid-packet)
  inject CONTROL: ABORT_CONFIRM at next boundary
  drop / cancel lower-priority data job
```

### IEEE 1284 recovery is the primary "data" use of PS/2 fallback

The main reason PS/2 carries non-control bytes in fallback is to **repair the data plane**:

```
CONTROL frames carry:
  - 1284 failure class report
  - request to re-probe LPT
  - select EPP/ECP/SPP fallback mode
  - reset 1284 transceiver command
  - re-open data plane

DATA frames carry:
  - small diagnostic artifacts only, when recovery is not converging
```

Normal loop:

1. Enter `PS2_FALLBACK_CONTROL_ONLY`.
2. Attempt to recover IEEE 1284.
3. If recovery fails after N attempts, transition to `PS2_FALLBACK_MUXED`.
4. Permit only explicitly degraded services on the muxed link.
5. Continue periodic recovery attempts; transition back to `DUAL_PLANE_ACTIVE` if they succeed.

### What NOT to do in fallback

- Strip the same bulk stream across both KBD and AUX lanes.
- Let the keyboard lane carry ordinary data.
- Send full screen deltas, full logs, firmware blobs, or bus traces over PS/2.
- Let data frames block `HEARTBEAT`, `ABORT`, or `RESET`.
- Use one untyped byte stream with escape codes for everything.
- Try to make PS/2 fallback reliable by widening retransmit windows — keep chunks small instead.
- Treat `PS2_FALLBACK_MUXED` as a long-term operating mode; it's a bridge to recovery, not a destination.

### Architectural rule for fallback

> **In fallback mode, PS/2 carries both control and degraded data, but control frames are always prioritized and data services are explicitly reduced to small, resumable jobs.**

This gives a robust rescue channel without letting the fallback path become a slow, fragile imitation of IEEE 1284.

## Layered protocol stack

```
Layer 0: Physical
  PS/2 KBD, PS/2 AUX, IEEE 1284 (DB-25)

Layer 1: Link
  ps2_private_link: small frames, CRC-16, seq8 / seq16
  lpt_1284_link:    bulk frames,  CRC-32, seq32

Layer 2: Session
  session_id, capabilities, authentication / unlock, heartbeat

Layer 3: Control/Data split
  cp_* messages   over PS/2
  dp_* streams    over IEEE 1284

Layer 4: Services
  screen, file, debug console, firmware, trace, diagnostics
```

## Implementation mapping

| Layer / role | Pico firmware module | DOS-side module |
|---|---|---|
| L0 PS/2 PIO + open-drain timing | `firmware/src/ps2/{pio.rs, ps2_at_dev.pio}` | (hardware) |
| L0 IEEE 1284 PIO | `firmware/src/ieee1284/pio.rs` | (hardware) |
| L1 PS/2 private link (CRC-16, seq, framing) | `firmware/src/ps2/private_mode.rs` | `dos/pico1284/src/ps2_i8042.{asm,c}` |
| L1 IEEE 1284 link (CRC-32, seq32) | `firmware/src/ieee1284/modes.rs` + `firmware/src/packet/` | `dos/pico1284/src/ieee1284.{asm,c}` + `dos/pico1284/src/packet.c` |
| L2 Session | `firmware/src/session/capability.rs` | `dos/pico1284/src/session.c` (planned) |
| L3 Control plane (`cp_*` over PS/2) | `firmware/src/session/control_plane.rs` (new) | `dos/pico1284/src/control_plane.c` (new) |
| L3 Data plane (`dp_*` over IEEE 1284) | `firmware/src/session/data_plane.rs` (new) | `dos/pico1284/src/data_plane.c` (new) |
| L4 Services | `firmware/src/screen/`, future `console/`, `file/`, `exec/` modules | `dos/pico1284/src/{screen_text.c, screen_vesa.c, file_xfer.c, console.c, tsr.c}` |

Two new firmware modules emerge from this architecture and should be added to [`implementation_plan.md`](implementation_plan.md) §1:

- `firmware/src/session/control_plane.rs` — `cp_frame` framing, `cp_msg_type` dispatch, state machine
- `firmware/src/session/data_plane.rs` — `dp_frame` framing, stream multiplexer, credit/window enforcement

## Open decisions

1. **CRC choice for the control plane:** spec proposes CRC-16-CCITT. Confirm this aligns with the existing IEEE 1284 packet CRC choice in `design.md` §9.
2. **Heartbeat cadence:** how often does PS/2 send `CP_DP_HEALTH`? Default proposal: every 500 ms during `DUAL_PLANE_ACTIVE`, every 100 ms during `DP_STALLED` / `DP_RESETTING`.
3. **Keyboard-lane scan-code symbols for OOB attention:** which specific `E0`-prefixed codes are reserved? Pick codes that are unambiguously not produced by real keyboards (e.g., codes that no PC/AT keyboard has ever emitted).
4. **Stream-ID ownership:** is stream 2 (KBD/mouse injection) bidirectional, or strictly Pico → DOS? Probably bidirectional once `host/` exists so the modern host can also drive virtual keystrokes.
5. **CP_TIME_SYNC semantics:** what time domain — Pico monotonic, modern-host wall-clock, or something else? Affects how trace marks and log records align across the planes.
6. **Authoritative protocol-constants file:** since both `cp_*` and `dp_*` constants must match across `firmware/` (Rust), `host/` (Rust), and `dos/pico1284/` (C/asm), the codegen-vs-hand-maintained choice from [`implementation_plan.md`](implementation_plan.md) §5 becomes acute. Lean toward a single TOML/YAML spec with code-gen to all three languages.
7. **Fallback scheduling window size:** default proposal is 8 packets (4 data / 2 ACK / 1 control / 1 mgmt reserve). Tune based on observed control-frame latency under saturation.
8. **`CONTROL_ONLY` → `MUXED` transition trigger:** how many failed 1284 recovery attempts before opening muxed data services? Default proposal: 3 attempts over 5 seconds, or any user-initiated diagnostic command.
9. **`MUXED` service whitelist:** which stream IDs may be opened in fallback? Default proposal: 0/1/2/4/5/6 (control, debug console, command responses, diagnostic summaries, recovery, panic frame). Block 3 (file fragments) unless explicitly forced; block 7 (debug console — full bidirectional) unless reduced to summary mode.
10. **Frame-class encoding in mouse packets:** the `ps2_frame_class` field occupies bits 4–5 of the IntelliMouse byte3 metadata (currently allocated to "type" in [`ps2_private_channel_design.md`](ps2_private_channel_design.md) frame format). Confirm the four-class encoding fits before finalizing.
11. **AUX ISR bounded-drain count:** default 16 bytes per IRQ12 invocation. Tune per CPU class — 286 may need 8, 486+ may tolerate 32. Empirical only.
12. **Adaptive LPT slice sizing thresholds:** default high-water for AUX ring is 75% full and low-water is 0 fill for 5 cycles. Tune once real traffic is measured.
13. **Detection of CPU class at TSR install time:** `pico1284` needs to pick slice/buffer sizes per host. Options: BIOS CPU detect, timing loop calibration, or operator-supplied flag at install. Lean toward CPU detect with timing-loop calibration override.
14. **ECP DMA channel discovery:** read BIOS/PnP/ECP configuration, accept an operator-supplied flag at install, or conservative probe? Default proposal: read PnP/ECP config when present, fall back to operator flag, never auto-probe a channel. See [§DOS DMA constraints](#dos-dma-constraints).
15. **ECP DMA bounce-buffer sizing:** 16 KiB vs 32 KiB conventional-memory allocation at TSR install. Default proposal: 16 KiB on 286/386, 32 KiB on 486+, both aligned so no 64 KiB DMA page boundary is crossed.
16. **DMA-vs-PIO crossover threshold:** payload size at which `ECP_DMA` beats `ECP_PIO`. Default proposal: 2 KiB, but tune per host class once measured. Below crossover, even an `ECP_DMA`-capable session uses PIO for the chunk.
17. **DMA mode-downgrade hysteresis:** how many `ECP_DMA` failures before stepping to `ECP_PIO`? Default proposal: 2 consecutive failures on the same chunk, or any boundary-wrap or wrong-channel error (immediate downgrade).
18. **`max_dma_block` policy:** capped by `dp_caps.max_dma_block`, but should the Pico further cap it by available `dp_rx_ring` space? Default proposal: yes — never DMA more bytes than the consumer can drain in one slice budget cycle.

## Related documents

- [`design.md`](design.md) — architectural overview; this doc refines §17 substantially
- [`ps2_private_channel_design.md`](ps2_private_channel_design.md) — PS/2 wire-level design (lanes, calibration, frame format); the L0/L1 of the control plane
- [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) — IEEE 1284 controller-side reference; the L0/L1 of the data plane
- [`ps2_eras_reference.md`](ps2_eras_reference.md) — per-era PS/2 capabilities (XT keyboard-only, AT keyboard bidir, PS/2+SuperIO adds AUX)
- [`implementation_plan.md`](implementation_plan.md) — per-subrepo plan (firmware modules, DOS modules)
- [`stage0_design.md`](stage0_design.md) — Stage 0 brings the data plane up and hands off via fixed register ABI; this doc describes the session that runs after that hand-off
- [`stage1_design.md`](stage1_design.md) — Stage 1 takes Stage 0's hand-off, negotiates IEEE 1284, and downloads Stage 2; this doc is the protocol Stage 2 then enters
