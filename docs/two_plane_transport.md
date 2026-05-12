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
  IRQ1 / IRQ12                       LPT IRQ/poll/DMA-ish loops
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
  CP_CMD_1284_NEGOTIATE(mode=EPP preferred, fallback=ECP/SPP)
  CP_EVT_1284_MODE(mode=EPP, width=8, crc=on)
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

```
IRQ1 handler:
  decode urgent keyboard-lane symbols only
  set flags; do minimal work

IRQ12 handler:
  collect AUX control fragments
  push complete CP frames to control queue

LPT/EPP/ECP loop:
  move bulk data as fast as possible
  periodically yield / check control flags
```

Keep IRQ handlers tiny. The resident driver or polling loop processes frames outside interrupt context.

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

## Related documents

- [`design.md`](design.md) — architectural overview; this doc refines §17 substantially
- [`ps2_private_channel_design.md`](ps2_private_channel_design.md) — PS/2 wire-level design (lanes, calibration, frame format); the L0/L1 of the control plane
- [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) — IEEE 1284 controller-side reference; the L0/L1 of the data plane
- [`ps2_eras_reference.md`](ps2_eras_reference.md) — per-era PS/2 capabilities (XT keyboard-only, AT keyboard bidir, PS/2+SuperIO adds AUX)
- [`implementation_plan.md`](implementation_plan.md) — per-subrepo plan (firmware modules, DOS modules)
