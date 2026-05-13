# AISHELL REAL + DPMI Detailed Design

**Status:** Future target architecture (not yet implemented)
**Date:** 2026-05-13
**Scope:** DOS-side Stage 2 runtime
**Relationship to current code:** Replaces the `dos/pico1284/` Stage 2 placeholder once the Phase 3-5 bootstrap chain is hardware-validated and the IEEE 1284 + PS/2 transports are stable.

This document captures the full future-state design of **AISHELL** — the AI-native DOS shell, transport executive, and human/AI session arbiter that runs after the bootstrap chain (Stages -1 / 0 / 1) brings up Stage 2. It is the conceptual target for `dos/pico1284/PICO1284.EXE`; the current 49-byte placeholder served by Stage 1 is a stand-in until this design lands.

The architecture is intentionally two-runtime so a single binary can serve everything from XT-class machines (where REAL mode is the only option) up to 486/Windows-9x-DOS-box hosts (where DPMI gives a real workstation).

---

## 1. Purpose

AISHELL is an AI-native DOS shell, transport executive, and human/AI session arbiter for vintage DOS machines. It exposes DOS as a structured, controllable environment to a modern AI host while remaining usable from the local DOS console.

AISHELL has two primary runtime modes:

1. **AISHELL REAL** — a small, deterministic, real-mode resident shell/runtime for plain DOS, non-V86 environments, recovery, and constrained machines.
2. **AISHELL DPMI** — a V86/protected-mode-aware runtime using a tiny real-mode Assembly shim plus a 32-bit Watcom C protected-mode core for richer AI tooling.

**Design principle:**

```
REAL mode is the lifeboat.
DPMI mode is the workstation.
```

AISHELL must always degrade safely from DPMI to REAL, from IEEE 1284 to PS/2 fallback, and finally to COMMAND.COM or an emergency shell if needed.

---

## 2. Goals

### 2.1 Functional goals

AISHELL shall provide:

- Local human shell operation.
- Remote-human operation through the modern host bridge.
- AI co-pilot observation and suggestions.
- Structured AI tool-call endpoint.
- PS/2 control/fallback transport.
- IEEE 1284 bulk data transport.
- File transfer.
- Console capture.
- Text-mode screen snapshot.
- Program execution with output/result capture.
- Minimal raw memory inspection primitives.
- Transport recovery and diagnostics.
- Safe fallback to COMMAND.COM.

### 2.2 AI co-pilot goals

The AI host shall be able to:

- Observe human-entered commands.
- Observe command output and screen state.
- Detect build errors and transport failures using host-side analysis.
- Suggest commands, patches, next steps, or explanations.
- Execute approved actions through structured tools.
- Defer to the human operator or configured remote-operator policy at all times.

### 2.3 Compatibility goals

AISHELL shall support these machine classes:

| Class | Mode | Notes |
|---|---|---|
| XT / 8088 | REAL tiny | No bidirectional PS/2; LPT-focused; keyboard is bootstrap/input only. |
| 286 AT | REAL | Bidirectional keyboard/i8042 possible; no AUX. |
| 386 plain DOS | REAL or DPMI | REAL for diagnostics, DPMI for rich tooling if available. |
| 386+ with EMM386/QEMM | DPMI/V86-aware | Real-mode shim plus protected-mode core. |
| 486+ | DPMI preferred | Better buffers, file transfer, co-pilot experience. |
| Windows 9x DOS box | DPMI-safe | Hardware may be virtualized; conservative probing required. |

### 2.4 Scope discipline principle

The DOS-side runtime shall stay as small and dumb as practical. Anything that can be done on the modern host without losing correctness should be done on the modern host.

Host-side responsibilities include:

- JSON-facing tool schema.
- AI reasoning.
- Source-code analysis.
- Patch generation.
- Build-error recognition.
- Rich memory decoding.
- Long-term logs.
- Artifact comparison and diffing.
- UI presentation.

DOS-side AISHELL responsibilities are limited to:

- Hardware capture and transport.
- Safe execution of approved actions.
- Raw data access.
- Minimal local arbitration.
- Recovery.

---

## 3. Non-goals

AISHELL does not initially attempt to:

- Run an AI model inside DOS.
- Implement preemptive multitasking.
- Fully clone COMMAND.COM batch semantics.
- Parse JSON on the DOS target.
- Perform rich source-code analysis locally.
- Depend on DPMI being present.
- Depend on IEEE 1284 always working.
- Depend on PS/2 AUX being present on XT/AT systems.

The modern host performs AI reasoning, JSON-facing tool translation, patch generation, larger diffing, source analysis, and long-term logs.

---

## 4. High-level architecture

```
Modern host / AI assistant
        |
        | AI tool calls, suggestions, policy
        v
Host bridge process
        |
        | PS/2 control plane + IEEE 1284 data plane
        v
+----------------------------------------------------+
| AISHELL                                            |
|                                                    |
|  REAL runtime                                      |
|    - Assembly resident core                        |
|    - IRQ handlers                                  |
|    - i8042 / PS/2                                  |
|    - LPT fallback helpers                          |
|    - emergency shell                               |
|                                                    |
|  DPMI runtime, when available                      |
|    - real-mode Assembly shim                       |
|    - 32-bit Watcom C AICORE                        |
|    - large buffers                                 |
|    - AI RPC dispatcher                             |
|    - file/exec/screen/memory services              |
|    - co-pilot event engine                         |
+----------------------------------------------------+
        |
        v
DOS filesystem / console / memory / programs
```

---

## 5. Runtime modes

### 5.1 AISHELL REAL

AISHELL REAL is the minimal, deterministic runtime.

It is used when:

- DPMI is unavailable.
- The CPU is XT/286-class.
- The machine is in plain DOS and direct hardware access is desired.
- DPMI or V86 behavior is unstable.
- Recovery mode is needed.
- The user explicitly requests `/REAL`, `/TSR`, `/TINY`, or `/SAFE`.

#### 5.1.1 REAL implementation language

All real-mode code should be Assembly.

Recommended toolchain:

```
NASM for real-mode Assembly
WLINK or compatible linker for DOS EXE/COM output
```

If NASM/OMF integration becomes painful, selected modules may be built with JWASM/WASM while keeping the real-mode layer conceptually all Assembly.

#### 5.1.2 REAL responsibilities

AISHELL REAL owns:

- DOS startup / PSP handling.
- Resident installation.
- IRQ vector save/restore.
- IRQ1 keyboard handler.
- IRQ12 AUX handler, when available.
- Optional IRQ7/LPT handler, if enabled.
- PIC mask/save/restore/EOI.
- i8042 access through ports 0x60 and 0x64.
- PS/2 byte rings.
- Emergency keyboard hotkeys.
- Shared conventional-memory block.
- INT 2Fh or private interrupt API.
- Panic/status text output.
- DPMI core launcher.
- Safe fallback to COMMAND.COM.

Panic / status output assumes a text-mode video state (color: `B800:0000`; mono: `B000:0000` based on BIOS data area at `0040:0049`). If a direct VRAM write isn't appropriate — for example, if the host was last in a graphics mode — REAL falls back to `INT 10h AH=0Eh` teletype output. REAL does not switch video modes on its own; that's an operator-or-AICORE concern.

#### 5.1.3 REAL should not own

AISHELL REAL should avoid:

- Rich shell parsing.
- JSON parsing.
- Heavy TLV parsing.
- Complex file transfer state.
- Directory walking beyond basic tools.
- Compression/decompression.
- Source patching.
- Rich policy engine.
- Co-pilot reasoning.
- Full-screen windowing.

Those belong in AICORE where DPMI is available, or on the modern host.

#### 5.1.4 REAL shell profile

AISHELL REAL exposes a minimal emergency and fallback shell:

```
AISHELL SAFE>
  STATUS
  HOST STATUS
  TRANSPORT STATUS
  TRANSPORT RESET
  AI PAUSE
  AI RESUME
  COMMAND
  LOADPM
  RESTORE
  REBOOT
```

REAL may also expose a compact normal shell profile:

```
DIR
CD
TYPE
COPY
DEL
REN
RUN
MEM
SCREEN
AI
HOST
TRANSPORT
COMMAND
EXIT
```

Unknown commands may be passed to:

```
COMMAND.COM /C <line>
```

### 5.2 AISHELL DPMI

AISHELL DPMI is the rich runtime.

It is used when:

- CPU is 386+.
- DPMI host is available and stable.
- Port I/O and interrupt reflection behavior are acceptable.
- The user requests `/DPMI`, `/PM`, or `/AUTO` selects it.

#### 5.2.1 DPMI implementation language

Recommended toolchain:

```
Open Watcom C for 32-bit protected-mode DOS core
NASM for selected 32-bit hot loops and low-level helper routines
```

#### 5.2.2 DPMI responsibilities

AICORE, the protected-mode core, owns:

- AI RPC dispatcher.
- Binary tool-call protocol parsing.
- Human shell frontend.
- Command history.
- Policy engine.
- Job scheduler.
- Human/AI arbitration.
- File services.
- Program execution wrapper.
- Console capture.
- Text-mode and later graphics screen snapshot.
- Memory explorer tools.
- IEEE 1284 data pump, if port I/O is permitted.
- Co-pilot event queue.
- Suggestion display/approval flow.
- Larger buffers and transfer windows.
- CRC/hash/compression helpers.

#### 5.2.3 DPMI depends on REAL shim

DPMI mode still keeps the real-mode Assembly shim installed.

```
IRQ1/IRQ12 -> REAL ASM ISR -> shared rings -> AICORE drains rings
```

Avoid initially:

```
IRQ -> real-mode ISR -> DPMI callback -> protected-mode C ISR
```

That approach is clever but fragile across DPMI hosts. Prefer polling shared rings from AICORE's foreground loop.

---

## 6. Mode selection

### 6.1 Startup probes

AISHELL performs these probes when no runtime is explicitly forced:

1. CPU class.
2. V86 flag.
3. DPMI presence.
4. XMS/EMS/memory-manager presence.
5. i8042 accessibility.
6. AUX presence.
7. LPT base addresses.
8. IEEE 1284 mode capability.
9. Port I/O behavior.
10. IRQ hook feasibility.
11. COMMAND.COM availability.

#### 6.1.1 CPU and mode decision

| Detection | Preferred mode |
|---|---|
| 8088/8086 | REAL tiny |
| 286 | REAL |
| 386+ plain DOS | REAL or DPMI if requested/available |
| 386+ with DPMI | DPMI |
| V86 without stable DPMI | REAL-safe |
| Unknown hardware behavior | REAL-safe |

#### 6.1.2 User-facing syntax

Common case should be simple:

```
AISHELL
AISHELL /SAFE
AISHELL /ASSISTED
AISHELL /AUTO
```

Runtime selection is auto-detected by default. Runtime override is for diagnostics and development:

```
AISHELL /RUNTIME=REAL
AISHELL /RUNTIME=DPMI
AISHELL /RUNTIME=AUTO
```

Policy mode is operator-facing:

```
AISHELL /POLICY=SAFE
AISHELL /POLICY=MANUAL
AISHELL /POLICY=OBSERVE
AISHELL /POLICY=SUGGEST
AISHELL /POLICY=ASSISTED
AISHELL /POLICY=AUTO
```

Profiles control footprint and debug behavior:

```
AISHELL /PROFILE=TINY
AISHELL /PROFILE=NORMAL
AISHELL /PROFILE=FULL
AISHELL /PROFILE=DEBUG
```

Avoid `/AUTOAI`; use `/POLICY=AUTO` instead.

---

## 7. Transport architecture

AISHELL uses two physical transports when available:

```
PS/2 KBD/AUX  = control plane, fallback plane, safety plane
IEEE 1284     = data plane, bulk transfer plane
```

### 7.1 Normal dual-plane mode

```
PS/2:
  session control
  heartbeat
  recovery
  flow-control hints
  AI suggestion/control envelope
  emergency abort
IEEE 1284:
  file payloads
  screen payloads
  logs
  large command results
  memory dumps
  trace dumps
```

### 7.2 PS/2-only fallback mode

If IEEE 1284 fails:

```
Keyboard lane / IRQ1:
  emergency attention, abort, bootstrap, mode switch
AUX lane / IRQ12:
  multiplexed control + degraded small data
```

Control frames always outrank data frames.

```
priority:
  1. emergency keyboard events
  2. AUX control
  3. ACK/NAK and flow control
  4. management/heartbeat
  5. small degraded data
```

### 7.3 XT and AT limitations

- XT keyboard is unidirectional and has no i8042 control path.
- AT has bidirectional keyboard/i8042 but no AUX lane.
- PS/2 and Super I/O hosts provide AUX via IRQ12 and 0xD4 routing.

Therefore:

```
XT:
  LPT is transport; keyboard is bootstrap/input only.
AT:
  keyboard/i8042 may provide limited control; LPT remains data path.
PS/2/Super I/O:
  full KBD+AUX control/fallback plus IEEE 1284 data-plane design.
```

---

## 8. Shared REAL/DPMI memory block

A conventional-memory shared block is the boundary between the real-mode Assembly shim and the protected-mode DPMI core.

### 8.1 Shared block layout

The shared block must be small in TINY/NORMAL builds and extensible in DEBUG builds.

```c
#define AIS_RM_MAGIC 0xA15E
#define AIS_RM_VERSION 0x0001
struct ais_rm_shared {
    volatile uint16_t magic;
    volatile uint16_t version;
    volatile uint16_t size;
    volatile uint16_t flags;
    volatile uint16_t error_flags;
    volatile uint16_t runtime_state;
    volatile uint16_t transport_state;
    volatile uint16_t policy_state;
    volatile uint16_t kbd_head;
    volatile uint16_t kbd_tail;
    uint8_t kbd_ring[AIS_KBD_RING_SIZE];
    volatile uint16_t aux_head;
    volatile uint16_t aux_tail;
    uint8_t aux_ring[AIS_AUX_RING_SIZE];
    volatile uint16_t tx_head;
    volatile uint16_t tx_tail;
    uint8_t tx_ring[AIS_TX_RING_SIZE];
    volatile uint16_t elog_head;
    volatile uint16_t elog_count;
    struct ais_event_log_entry event_log[AIS_EVENT_LOG_SIZE];
    volatile uint32_t bios_ticks_seen;
    volatile uint32_t irq1_count;
    volatile uint32_t irq12_count;
    volatile uint16_t last_status_64;
    volatile uint16_t last_data_60;
    volatile uint16_t last_error;
    volatile uint16_t command_flags;
    volatile uint16_t kick_flags;
};
struct ais_event_log_entry {
    uint16_t event_id;
    uint16_t arg0;
    uint16_t arg1;
    uint32_t timestamp;
};
```

The event log is intentionally tiny. Its purpose is crash forensics: after AICORE fails, the REAL shim can report the last N significant events.

Canonical event-log IDs (distinct from the AI co-pilot events in §13.2 — those flow on the wire, these stay in the shared block):

```c
enum ais_el_id {
    EL_BOOT             = 0x0001,
    EL_RUNTIME_PROMOTED = 0x0002,   /* REAL -> DPMI launch */
    EL_RUNTIME_DROPPED  = 0x0003,   /* DPMI -> REAL (AICORE exit) */
    EL_POLICY_CHANGED   = 0x0004,
    EL_TRANSPORT_RESET  = 0x0010,
    EL_TRANSPORT_DOWNGRADE = 0x0011,
    EL_RING_OVERFLOW    = 0x0020,   /* arg0 = channel, arg1 = bytes dropped */
    EL_I8042_STUCK      = 0x0021,
    EL_PIC_MASK_DRIFT   = 0x0022,
    EL_LPT_STROBE_STALL = 0x0023,
    EL_FOREIGN_IRQ      = 0x0024,
    EL_REAL_FAULT       = 0x002F,   /* generic; arg0 = flag, arg1 = reason */
    EL_EXEC_STARTED     = 0x0040,
    EL_EXEC_FINISHED    = 0x0041,
    EL_PANIC            = 0x00FF
};
```

The `AIS_KBD_RING_SIZE`, `AIS_AUX_RING_SIZE`, `AIS_TX_RING_SIZE`, and `AIS_EVENT_LOG_SIZE` macros are compiled in at build time from the active profile (§8.2). A single build chooses one profile; multi-profile builds ship as separate `AISHELL.EXE` binaries rather than runtime-switchable.

### 8.2 Ring rules

- Rings are power-of-two sized.
- ISR only writes head and data.
- Foreground core writes tail.
- Overflow sets an error flag and drops the oldest or newest byte depending on profile.
- REAL tiny profile uses smaller rings.

Recommended profiles:

| Profile | KBD ring | AUX ring | TX ring | Event log |
|---|---:|---:|---:|---:|
| TINY | 32 | 64 | 64 | 8 entries |
| NORMAL | 64 | 256 | 256 | 16 entries |
| DEBUG | 256 | 2048 | 2048 | 64 entries |

---

## 9. REAL interrupt handlers

### 9.1 ISR design rules

Interrupt handlers must:

- Save only required registers.
- Load AISHELL data segment explicitly.
- Read hardware status/data.
- Push bytes/events into rings.
- Set flags.
- Send PIC EOI.
- Return quickly.

They must not:

- Call DOS.
- Allocate memory.
- Write files.
- Parse full frames.
- Print except in panic paths.
- Invoke AI/DPMI logic directly.

### 9.2 IRQ1 keyboard ISR

Purpose:

- Capture emergency keyboard lane events.
- Capture private keyboard control symbols.
- Detect human takeover hotkeys.
- Optionally chain to old handler when AISHELL does not own the keyboard.

Pseudo-flow:

```
IRQ1:
  read 64h status
  if OBF:
    read 60h data
    if AISHELL owns keyboard:
      push to kbd_ring
    else if emergency hotkey:
      set emergency flag
    else:
      chain old IRQ1 handler or emulate delivery
  EOI master PIC
  IRET
```

### 9.3 IRQ12 AUX ISR

Purpose:

- Capture AUX bytes for PS/2 control/fallback transport.

Pseudo-flow:

```
IRQ12:
  bounded loop:
    read 64h status
    if OBF and AUX byte:
      read 60h data
      push to aux_ring
    else break
  EOI slave PIC
  EOI master PIC
  IRET
```

Use a bounded drain loop, e.g. 8 or 16 bytes, to avoid starving foreground work.

---

## 10. INT 2Fh / private API

AISHELL REAL exposes an installation and service API.

Preferred DOS-friendly API: INT 2Fh multiplex.

### 10.1 Functions

```
AX = A100h  installation check
AX = A101h  get shared block pointer
AX = A102h  install IRQ hooks
AX = A103h  restore IRQ hooks
AX = A104h  send byte to keyboard device path
AX = A105h  send byte to AUX path
AX = A106h  flush i8042
AX = A107h  get runtime status
AX = A108h  set runtime flags
AX = A109h  kick transmitter
AX = A10Ah  panic/status print
```

### 10.2 Installation check response

```
Input:
  AX = A100h
Output if installed:
  AX = 4153h   ; 'AS'
  BX = version
  ES:DI = shared block
```

---

## 11. Protected-mode AICORE

### 11.1 AICORE main loop

```c
for (;;) {
    drain_rm_ps2_rings();
    service_control_frames();
    service_transport_state();
    if (emergency_abort_pending())
        enter_recovery_mode();
    service_human_console_slice();
    service_ai_rpc_slice();
    schedule_jobs_slice();
    pump_ieee1284_slice();
    run_active_job_slice();
    send_heartbeat_if_due();
}
```

### 11.2 AICORE subsystems

```
aicore_main        startup, event loop, shutdown
aicore_rpc         binary tool-call protocol
aicore_transport   PS/2 + IEEE 1284 supervisor
aicore_shell       human shell frontend
aicore_jobs        job queue, locks, console ownership
aicore_policy      safe/manual/observe/suggest/assisted/auto modes
aicore_fs          file services
aicore_exec        child process launch/capture
aicore_console     console input/output capture
aicore_screen      text/graphics snapshot
aicore_mem         memory explorer
aicore_copilot     event emission, suggestion intake
aicore_compat      COMMAND.COM bridge
aicore_lpt         EPP/ECP/SPP data-plane routines
```

---

## 12. AI tool-call protocol

The modern host exposes JSON-like tools to the AI, but AISHELL speaks compact binary TLV.

### 12.1 Frame header

```c
struct ais_frame {
    uint16_t magic;      /* 0xA15E */
    uint8_t  version;
    uint8_t  type;
    uint8_t  flags;
    uint8_t  seq;
    uint16_t len;
    uint16_t crc16;
    uint8_t  payload[];
};
```

### 12.2 Frame types

```
FRAME_HELLO
FRAME_HELLO_ACK
FRAME_TOOL_CALL
FRAME_TOOL_RESULT
FRAME_TOOL_CANCEL
FRAME_EVENT
FRAME_EVENT_BATCH
FRAME_SUGGESTION
FRAME_ACK
FRAME_NAK
FRAME_HEARTBEAT
FRAME_ERROR
FRAME_STREAM_OPEN
FRAME_STREAM_CLOSE
```

`FRAME_TOOL_CANCEL` cancels an in-flight tool call by `call_id`. Cancellation is cooperative first. If the call owns a child process or blocking transport operation, AISHELL escalates according to the timeout/cancellation policy in §14.6.

### 12.3 HELLO capability exchange

Capability negotiation is mandatory. Both sides must tolerate unknown feature bits and TLVs.

`FRAME_HELLO` payload:

```c
struct ais_hello {
    uint16_t protocol_version;
    uint16_t min_protocol_version;
    uint16_t build_id;
    uint16_t runtime_mode;       /* REAL, DPMI, SAFE */
    uint16_t cpu_class;          /* 8088, 286, 386, 486, ... */
    uint16_t feature_words;      /* number of following uint32_t words */
    uint32_t feature_bits[];
};
```

Initial feature bits:

```
bit 0   PS2_KBD_LANE
bit 1   PS2_AUX_LANE
bit 2   IEEE1284_SPP
bit 3   IEEE1284_EPP
bit 4   IEEE1284_ECP
bit 5   IEEE1284_ECP_DMA
bit 6   DPMI_CORE
bit 7   REAL_SAFE_CORE
bit 8   FS_BASIC
bit 9   FS_TRANSFER
bit 10  EXEC_RUN
bit 11  CONSOLE_TEXT
bit 12  SCREEN_TEXT
bit 13  SCREEN_GRAPHICS
bit 14  MEM_READ
bit 15  MEM_MAP_RAW
bit 16  COPILOT_EVENTS
bit 17  COPILOT_SUGGESTIONS
bit 18  EVENT_BATCHING
bit 19  TIMEOUTS
bit 20  RATE_LIMITS
bit 21  REMOTE_OPERATOR_MODE
bit 22  EVENT_LOG
```

Feature-bit allocation policy:

```
bits 0-31 live in feature_bits[0]
bits 32-63 live in feature_bits[1]
future bits extend feature_bits[] without changing the frame layout
producers may add bits without bumping protocol_version
consumers must ignore unknown bits
protocol_version changes only for incompatible wire-format changes
```

`FRAME_HELLO_ACK` returns the intersection of supported features plus negotiated limits:

```c
struct ais_hello_ack {
    uint16_t protocol_version;
    uint16_t session_id;
    uint16_t runtime_mode;
    uint16_t policy_mode;
    uint16_t max_control_frame;
    uint16_t max_data_frame;
    uint16_t max_event_batch;
    uint16_t max_tool_timeout_s;
    uint32_t negotiated_features[];
};
```

### 12.4 Tool IDs

```c
enum ais_tool_id {
    T_SYSTEM_INFO       = 0x0001,
    T_TRANSPORT_STATUS  = 0x0010,
    T_SESSION_STATUS    = 0x0020,
    T_EVENT_LOG_READ    = 0x0030,
    T_FS_LIST           = 0x0100,
    T_FS_STAT           = 0x0101,
    T_FS_READ           = 0x0102,
    T_FS_WRITE          = 0x0103,
    T_FS_HASH           = 0x0104,
    T_FS_DELETE         = 0x0105,
    T_EXEC_RUN          = 0x0200,
    T_EXEC_COMMAND      = 0x0201,
    T_CONSOLE_READ      = 0x0300,
    T_CONSOLE_KEYS      = 0x0301,
    T_SCREEN_TEXT       = 0x0310,
    T_SCREEN_SNAPSHOT   = 0x0311,
    T_MEM_READ          = 0x0400,
    T_MEM_MAP_RAW       = 0x0401,
    T_POLICY_STATUS     = 0x0500,
    T_POLICY_SET_MODE   = 0x0501,
    T_COPILOT_EVENT     = 0x0600,
    T_COPILOT_SUGGEST   = 0x0601,
    T_COPILOT_FEEDBACK  = 0x0602
};
```

### 12.5 Tool-call envelope

Every tool call includes common control fields:

```c
struct ais_tool_call_header {
    uint16_t tool_id;
    uint16_t call_id;
    uint16_t side_effect_class;
    uint16_t flags;
    uint32_t timeout_ms;
    uint32_t byte_budget;
    uint32_t rate_budget_bytes_per_min;
};
```

`timeout_ms` is mandatory for mutating and executing calls. A zero timeout means "use policy default," not "infinite."

`call_id` is the cancellation and correlation handle. `FRAME_TOOL_CANCEL` references the same `call_id` and may be sent by the host bridge when the remote operator or AI policy wants to abort a long-running operation without tearing down the session.

### 12.6 Timestamp policy

AISHELL uses a tiered timestamp model:

| Source | Availability | Resolution | Use |
|---|---|---|---|
| BIOS tick counter | universal DOS | ~55 ms | baseline event ordering and fallback timestamps |
| PIT-derived sub-tick counter | AT+ when enabled | implementation-defined, higher resolution | latency measurement, batching cadence, diagnostics |
| Host bridge timestamp | modern host | high resolution | durable logs and AI-facing timelines |

REAL shared-block timestamps use a 32-bit monotonic tick value. In TINY/REAL-safe mode, this is BIOS ticks. In DPMI/NORMAL mode, AICORE may combine BIOS ticks with a PIT-derived sub-tick counter for finer local timing. The host bridge should treat DOS timestamps as monotonic ordering hints and apply host-side wall-clock timestamps for durable logs.

---

## 13. Co-pilot event model

AISHELL monitors human interaction and emits semantic events to the modern AI host.

### 13.1 Event boundaries

AISHELL should emit events for:

- Command entered.
- Command completed.
- Error summary detected by local boundary heuristics.
- Screen changed substantially.
- File-change batch.
- Program executed.
- Transport degraded/recovered.
- Human requested help.
- Human accepted/rejected suggestion.

AISHELL should not stream every raw keystroke by default.

AISHELL should also avoid per-line output events on slow machines. Raw output should be captured into a buffer or data-plane stream, while the control plane receives batches or summaries.

### 13.2 Event IDs

```c
enum ais_event_id {
    E_COMMAND_ENTERED       = 0x0001,
    E_COMMAND_FINISHED      = 0x0002,
    E_OUTPUT_BATCH          = 0x0003,
    E_ERROR_SUMMARY         = 0x0004,
    E_SCREEN_CHANGED        = 0x0005,
    E_SCREEN_TEXT_SNAPSHOT  = 0x0006,
    E_FILE_CHANGED_BATCH    = 0x0007,
    E_EXEC_STARTED          = 0x0008,
    E_EXEC_FINISHED         = 0x0009,
    E_TRANSPORT_DEGRADED    = 0x0010,
    E_TRANSPORT_RECOVERED   = 0x0011,
    E_HELP_REQUESTED        = 0x0020,
    E_SUGGESTION_ACCEPTED   = 0x0030,
    E_SUGGESTION_REJECTED   = 0x0031
};
```

#### 13.2.1 Event batching

Use `FRAME_EVENT_BATCH` for frequent events.

Output batching policy:

```
Emit control-plane event after:
  - N milliseconds,
  - M lines,
  - buffer high-water mark,
  - command completion,
  - explicit error boundary.
```

Default suggested values:

```
N = 250-500 ms on 386+
N = 500-1000 ms on 286/slow hosts
M = 16-64 lines depending on profile
```

The batch event carries metadata:

```c
struct ais_output_batch_event {
    uint16_t line_count;
    uint16_t error_count;
    uint16_t flags;
    uint32_t data_ref;       /* optional IEEE 1284 stream ref */
    uint32_t byte_count;
};
```

File-change batching policy:

```
Aggregate changed paths until:
  - command exits,
  - batch reaches max entries,
  - timeout expires.
```

### 13.3 Suggestion lifecycle

```
proposed -> displayed -> accepted / edited / rejected -> executed -> reported
```

### 13.4 Human UI

At shell prompt:

```
AI: Turbo C cannot find STDIO.H. Set INCLUDE=C:\TC\INCLUDE?
[F9 accept] [F8 edit] [F7 explain] [Esc dismiss]
```

Hotkeys:

```
Ctrl-Alt-A      AI panel
Ctrl-Alt-H      ask AI about current screen
Ctrl-Alt-V      human takeover / pause AI
Ctrl-Alt-Break  abort active AI job
F9              accept suggestion
F8              inspect/edit suggestion
Esc             dismiss suggestion
```

---

## 14. Human/AI arbitration

AISHELL serializes DOS execution while allowing concurrent human and AI intent.

### 14.1 Console ownership

```c
enum console_owner {
    OWNER_HUMAN,
    OWNER_REMOTE_HUMAN,
    OWNER_AI,
    OWNER_CHILD_PROGRAM,
    OWNER_SHARED_MONITOR,
    OWNER_RECOVERY
};
```

`OWNER_REMOTE_HUMAN` covers the common workflow where the operator is not at the DOS keyboard but is using the modern host UI while watching screen snapshots and approving AI actions remotely.

For v1, AISHELL treats the host bridge as trusted. It does not authenticate the remote human directly. Authentication, user identity, and remote UI authorization belong on the modern host side. AISHELL only sees policy decisions and approved tool calls from the trusted bridge.

### 14.2 Job classes

```c
enum job_class {
    JOB_OBSERVE,
    JOB_TRANSFER,
    JOB_EXEC,
    JOB_MUTATE,
    JOB_DEBUG,
    JOB_RECOVERY
};
```

### 14.3 AI policy modes

```
SAFE:
  AI mutations disabled; transport receive allowed.
MANUAL:
  AI disabled or only explicitly invoked.
OBSERVE:
  AI can observe and explain; no action suggestions.
SUGGEST:
  AI can propose commands or patches.
ASSISTED:
  AI can prepare actions; human approves locally or remotely.
AUTO:
  AI can execute policy-approved actions within scope.
```

### 14.4 Side-effect classes

Each tool declares a side-effect class:

```
READ_ONLY
WRITE_FILE
EXECUTE
MEMORY_WRITE
DEBUG_HOOK
SYSTEM_CONTROL
TRANSPORT_CONTROL
DESTRUCTIVE
```

AISHELL policy gates tool execution by side-effect class.

### 14.5 Budgets and anti-DoS policy

Side-effect class is not sufficient. Every policy grant also has resource budgets.

Budget dimensions:

```
timeout_ms
max_bytes_per_call
max_bytes_per_minute
max_files_per_call
max_events_per_minute
max_runtime_seconds
max_transfer_window
```

`timeout_ms` is the per-tool-call execution deadline. `max_runtime_seconds` is a broader policy budget for a job/session scope, which may contain multiple tool calls.

Examples:

```
fs.read is READ_ONLY, but may still be denied if file size > budget.
console.capture is READ_ONLY, but may be rate-limited.
exec.run requires timeout and console ownership.
mem.read may be size-limited even when allowed.
```

Policy enforcement occurs before dispatch and during long-running operations.

### 14.6 Time-bounded operations

Every mutating, executing, transport-reset, or potentially blocking tool call must have a timeout. On timeout:

1. AISHELL attempts cooperative cancel.
2. If a child program is active, AISHELL attempts Ctrl-Break or configured abort sequence.
3. AISHELL reports timeout result to host.
4. AISHELL may enter recovery mode if the foreground state is unknown.

---

## 15. IEEE 1284 data plane

### 15.1 Modes

```
SPP/Nibble       baseline/fallback
EPP_PIO          default fast path
ECP_PIO          optional
ECP_DMA          optional, validated hosts only
```

### 15.2 DPMI interaction

DPMI mode may perform port I/O directly if permitted.

Backends:

```
LPT_BACKEND_PM_IO
LPT_BACKEND_RM_BLOCK_THUNK
LPT_BACKEND_BIOS_DIAGNOSTIC
```

Do not thunk per byte. Thunk only block operations.

### 15.3 DMA policy

ECP DMA is optional only.

If used:

- Allocate conventional-memory DMA bounce buffer.
- Avoid 64 KiB DMA boundary crossing.
- Use small chunks first, e.g. 4 KiB.
- Control and recovery remain PS/2-supervised.
- Fall back quickly on failure.

Fallback order:

```
ECP_DMA -> ECP_PIO -> EPP_PIO -> SPP/Nibble -> PS2_ONLY
```

---

## 16. File, exec, console, screen, and memory services

### 16.1 File services

V1 tools:

```
fs.list
fs.stat
fs.read
fs.write
fs.hash
fs.delete
fs.transfer_get
fs.transfer_put
```

Large payloads use IEEE 1284 when available; PS/2 fallback uses small chunks only.

File operations are budgeted by size, rate, path scope, and policy mode.

### 16.2 Execution services

```
exec.run
exec.run_command
```

Execution model:

1. Acquire console/job lock.
2. Require timeout.
3. Prepare capture hooks.
4. Execute child through DOS.
5. Regain control on exit or timeout.
6. Capture exit code, screen, changed-file batch, output batch metadata.
7. Emit event/result to AI host.

### 16.3 Console capture

Capture layers:

1. INT 21h output where possible.
2. INT 10h BIOS output where feasible.
3. Text VRAM snapshot.
4. Keyboard injection for interactive programs.

Raw captured output should be buffered or streamed over the data plane; control plane should receive summaries/batches.

### 16.4 Screen tools

V1:

```
screen.text_snapshot
```

Later:

```
screen.graphics_snapshot
screen.palette
screen.mode
screen.delta
```

### 16.5 Memory tools

V1 DOS-side memory tool should be intentionally dumb:

```
mem.read
mem.map_raw
```

AISHELL returns raw bytes or raw MCB/IVT/BDA regions. Rich decoding happens on the modern host.

Address-space types:

```
REAL segment:offset
PHYS physical address
PM selector:offset
DPMI linear address
```

Host-side bridge may expose richer tools to the AI, such as `mem.decode_bda`, but those are host-derived views over `mem.read` results.

---

## 17. Error handling and recovery

### 17.1 Downward fallback ladder

```
DPMI full mode
  -> DPMI safe mode
  -> REAL mode
  -> COMMAND.COM
  -> emergency AISHELL SAFE prompt
```

### 17.2 Transport fallback ladder

```
IEEE 1284 active
  -> IEEE 1284 reset/recover
  -> lower 1284 mode
  -> PS/2 fallback muxed mode
  -> PS/2 control-only mode
  -> local-only AISHELL
```

### 17.3 AICORE crash behavior

If AICORE exits or crashes:

```
AISHELL REAL remains resident.
Vectors remain restorable.
Transport enters SAFE.
Human sees recovery prompt.
```

Recovery prompt:

```
AICORE stopped.
[R]etry  [S]afe mode  [C]OMMAND.COM  [U]ninstall  [B]oot/reboot
```

Policy persistence across an AICORE restart:

- On `[R]etry`, AICORE restarts in `/POLICY=SAFE` regardless of the prior policy, unless `AISHELL.CFG` declares a `RECOVERY_POLICY=` override. SAFE is the default because the operator has not yet reaffirmed consent.
- `AISHELL.CFG`'s baseline `POLICY=` value is used when AICORE is launched fresh (cold boot or `[R]etry` with `RECOVERY_POLICY=BASELINE`).
- The shared block's `policy_state` field is updated only by AICORE; REAL never promotes policy on its own.

### 17.4 REAL → AICORE error signaling

The REAL shim signals upward through the shared block's `error_flags`, `last_error`, and `kick_flags` fields. AICORE polls these on each loop iteration alongside ring drains.

Conditions REAL flags upward:

- Ring overflow (per-channel bit in `error_flags`).
- Sustained glitch / framing-error rate on the PS/2 side.
- i8042 stuck-status (status register fails to clear over a bounded wait).
- PIC mask drift (REAL observes a mask AISHELL didn't write).
- LPT host-strobe stall (no strobe edge within expected window).
- Foreign IRQ owner — another resident installed an IRQ1/IRQ12 hook after AISHELL.

On any flag set, REAL:

1. Writes the flag bit + a short reason code into `last_error`.
2. Records a `EL_REAL_FAULT` event log entry with `event_id`, `arg0` = flag, `arg1` = sub-reason, and current `bios_ticks_seen` as timestamp.
3. Sets `kick_flags` to wake AICORE on the next slice (no callback — AICORE will see it polling).

AICORE response is policy-driven:

- Soft errors (ring overflow, glitch rate) → emit `E_TRANSPORT_DEGRADED`, narrow budgets, continue.
- Hard errors (i8042 stuck, mask drift, foreign IRQ owner) → transition to recovery mode, prompt the human.

REAL never tears down on its own. The shim's contract is "stay alive, keep flagging" so AICORE or the operator decides next steps.

---

## 18. Build system

### 18.1 Toolchain

```
NASM       real-mode Assembly and selected PM Assembly
Open Watcom C/C++  32-bit DPMI core
WLINK      linking
```

### 18.2 Source layout

```
src/
  rm/
    rm_start.asm
    rm_resident.asm
    rm_isr_kbd.asm
    rm_isr_aux.asm
    rm_i8042.asm
    rm_pic.asm
    rm_lpt.asm
    rm_api_int2f.asm
    rm_ring.inc
    rm_video.asm
    rm_launch.asm
    rm_shared.inc
  pm/
    main.c
    rpc.c
    shell.c
    sched.c
    transport.c
    fs.c
    exec.c
    console.c
    screen.c
    mem.c
    policy.c
    copilot.c
    compat.c
    lpt_pm.asm
  common/
    protocol.h
    shared.h
    crc16.c
    crc32.c
    tlv.c
    errors.h
```

### 18.3 Build artifacts

Preferred split:

```
AISHELL.EXE   real-mode shell/loader/resident core
AICORE.EXE    32-bit DPMI protected-mode core
AISHELL.CFG   policy/config
```

No overlay system is part of v1. Features must either fit in `AICORE.EXE` or move to the modern host. `AISHELL.OVL` is reserved only for a future version if overlays are deliberately designed up front, including segment layout, swap discipline, and state ownership.

User-facing command remains `AISHELL`.

---

## 19. Size targets

### 19.1 REAL

| Profile | Resident target |
|---|---|
| TINY | 4–16 KB |
| NORMAL | 16–32 KB |
| DEBUG | 32–64 KB |

### 19.2 DPMI

| Component | Target |
|---|---|
| Real-mode shim | 4–24 KB resident |
| AICORE base | 64–256 KB on disk initially |
| Protected-mode heap/buffers | configurable |

---

## 20. Roadmap

The roadmap is deliberately divided into a shippable v1.0 and later polish. Scope discipline matters more than feature completeness.

### Phase 0 — REAL skeleton

- NASM startup.
- Panic/status text output.
- INT 2Fh installation check.
- Vector save/restore.
- COMMAND.COM chain.

### Phase 1 — PS/2 REAL core

- IRQ1 handler.
- IRQ12 handler.
- i8042 wait/flush helpers.
- Shared rings.
- Emergency hotkeys.

### Phase 2 — REAL shell minimum

- STATUS.
- TRANSPORT STATUS.
- AI PAUSE/RESUME.
- COMMAND bridge.
- SAFE prompt.

### Phase 3 — IEEE 1284 minimum

- LPT base detection.
- SPP/nibble or EPP PIO basic transfer.
- Transport status and reset.

### Phase 4 — DPMI launch

- Detect DPMI.
- Launch AICORE.
- Map/access shared REAL block.
- Drain PS/2 rings from PM.

### Phase 5 — AICORE RPC v1

- HELLO capability negotiation.
- Binary frame parser.
- `system.info`.
- `transport.status`.
- `fs.list/stat/read/write`.
- `console.read`.
- `screen.text`.
- mandatory timeouts/budgets in tool-call envelope.

### Phase 6 — Exec and co-pilot v1

- `exec.run_command` with timeout.
- command-entered events.
- batched output/error events.
- suggestion display.
- F9/F8/Esc handling.
- basic `console_owner` and policy gates.

### Phase 7 — IEEE 1284 data-plane integration

- Bulk streams.
- `fs.transfer_get/put`.
- CRC32.
- PS/2-supervised reset/recovery.

### v1.0 ship target

Ship v1.0 at the end of Phase 7, including the minimal parts of Phase 8 required for safety:

- `console_owner`.
- policy mode gates.
- side-effect classes.
- timeouts and byte budgets.
- human takeover hotkey.
- COMMAND.COM fallback.

This is enough for a useful AI co-pilot:

- inspect files,
- upload/download files,
- run commands,
- capture output/screen,
- receive suggestions,
- recover transports.

### Phase 8 — Human/AI arbitration polish, v1.1

- fuller job queue.
- richer approval flow.
- remote-human mode polish.
- command locks.
- better policy persistence.

### Phase 9 — Memory and diagnostics, v1.2

- `mem.read` improvements.
- raw MCB/IVT/BDA capture.
- host-side decoders.
- transport diagnostics.
- tiny event-log query UI.

### Phase 10 — Optional enhancements, v1.3+

- Graphics snapshots.
- ECP PIO.
- ECP DMA.
- Compression.
- `fs.patch`, if still useful after host-side patching.
- richer build-error recognizers on host side.
- DPMI safe-mode refinements.

---

## 21. Design rules

1. REAL mode must remain small, deterministic, and recoverable.
2. All real-mode code is Assembly.
3. Protected-mode complexity belongs in Watcom C AICORE.
4. DOS-side protocol is binary TLV, not JSON.
5. The modern host translates between AI-facing JSON tools and AISHELL binary frames.
6. Interrupt handlers never call DOS.
7. PS/2 is the control/fallback/safety plane.
8. IEEE 1284 is the bulk data plane.
9. In PS/2 fallback, control frames always preempt data frames.
10. Human intent outranks AI autonomy.
11. AISHELL serializes DOS execution while allowing concurrent human and AI intent.
12. DPMI mode must always fall back to REAL mode.
13. COMMAND.COM remains available as a compatibility and recovery path.

---

## 22. Summary

AISHELL should be implemented as a two-runtime system:

```
AISHELL REAL:
  pure Assembly, tiny, resident, deterministic, hardware-facing, always recoverable.
AISHELL DPMI:
  real-mode Assembly shim plus 32-bit Watcom C AICORE, rich AI tool server and co-pilot shell.
```

This architecture gives the project both vintage robustness and modern AI-facing capability. The real-mode layer owns hardware immediacy and recovery. The DPMI layer owns structured tools, co-pilot behavior, file/exec/screen/memory services, and the pleasant development experience.

---

## 23. Relationship to current project state

AISHELL is the **target replacement** for the placeholder served by `dos/stage1/stage1.asm` as `PICO1284.EXE`. The current bootstrap chain stops at "Stage 1 EXECs Stage 2 with a `PICO_BOOT` env block"; AISHELL is what Stage 2 grows into.

The bootstrap chain remains unchanged:

```
Stage -1 (Pico types DEBUG)
   │
   ▼
Stage 0 (S0_XT/AT/PS2 — bring up first bidirectional channel)
   │
   ▼
Stage 1 (load Stage 2 over IEEE 1284, write PICO1284.EXE to disk, EXEC)
   │
   ▼
Stage 2 = AISHELL  ◀── this document
```

AISHELL inherits from Stage 0:
- i8042 private channel (when on AT/PS2 class machines)
- PIC mask state
- IRQ ownership of 1 (KBD) and 12 (AUX, when present)

AISHELL inherits from Stage 1:
- `PICO_BOOT` env block (`LPT=XXXX MODE=YYY CHAN=N VER=X.Y`) — gives initial transport state without re-probing
- Negotiated IEEE 1284 mode (or fallback)
- Knowledge that the Pico is responsive on the chosen channel

The two-plane model in [`two_plane_transport.md`](two_plane_transport.md) describes the contract AISHELL fulfills from the DOS side. The Pico-side firmware design ([`pico_firmware_design.md`](pico_firmware_design.md), [`pio_state_machines_design.md`](pio_state_machines_design.md)) is symmetric: the Pico's data-plane and control-plane responsibilities mirror AISHELL's.

---

## 24. Related documents

- [`design.md`](design.md) §21 — original DOS-software-architecture sketch this document expands
- [`stage0_design.md`](stage0_design.md) — Stage 0 design that hands i8042 mastery to AISHELL
- [`stage1_design.md`](stage1_design.md), [`stage1_implementation.md`](stage1_implementation.md) — Stage 1 design + as-built; defines the `PICO_BOOT` env contract AISHELL reads
- [`two_plane_transport.md`](two_plane_transport.md) — control plane / data plane contract AISHELL implements DOS-side
- [`ps2_private_channel_design.md`](ps2_private_channel_design.md) — i8042 unlock protocol AISHELL inherits
- [`ps2_eras_reference.md`](ps2_eras_reference.md) — XT/AT/PS2 framing differences AISHELL adapts to
- [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) — IEEE 1284 reference AISHELL uses for the data plane
- [`pico_firmware_design.md`](pico_firmware_design.md), [`pio_state_machines_design.md`](pio_state_machines_design.md), [`instrumentation_surface.md`](instrumentation_surface.md) — Pico-side peer design AISHELL talks to
- [`implementation_plan.md`](implementation_plan.md) §4 `dos/pico1284/` — current placeholder state; AISHELL is the eventual target
