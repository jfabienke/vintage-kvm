# Pico firmware design

Detailed design for the Pico-side firmware crate (`firmware/`). This expands [`design.md` §20](design.md) (firmware-architecture sketch) into an implementable specification covering all phases of the project.

This is the **peripheral-side dual** of every DOS-host artifact in the project. Every byte the DOS side sends, this firmware receives; every reply the DOS side expects, this firmware produces.

---

## 1. Purpose and scope

The Pico firmware is the single Rust crate that runs on the Adafruit Feather RP2350 HSTX board and bridges:

```
   DOS host  ⇆  LPT (IEEE 1284) + PS/2 KBD + PS/2 AUX  ⇆  Pico  ⇆  USB CDC  ⇆  modern host
```

It serves four distinct, time-overlapping responsibilities:

| Responsibility | Phase(s) | Description |
|---|---|---|
| **PS/2 keyboard emulation** | 1, 2, 11 | Type DEBUG bootstrap scripts; run private inbound byte pump after i8042 unlock. |
| **PS/2 AUX emulation** | 2 (opt.), 11 | Same as KBD on the AUX channel — used for higher-bandwidth bootstrap and dual-lane fallback. |
| **IEEE 1284 peripheral** | 3+ | Serve Stage 0, Stage 1, Stage 2 over LPT; carry the steady-state data plane. |
| **USB CDC bridge** | 6+ | Forward packet traffic to/from the modern host. |

Plus an always-on **instrumentation surface** that emits per-frame timing, error counters, mode transitions, and state changes via defmt-RTT (development) and a vendor-defined USB CDC channel (production).

### Scope of this document

Covers:
- Async task architecture (embassy)
- PIO/state-machine allocation across the project lifecycle
- Memory map (SRAM, PSRAM, flash)
- Each module's responsibilities and interfaces
- The end-to-end lifecycle (boot → DP_ACTIVE)
- Error handling and recovery
- Build system
- Testing strategy

Does *not* cover:
- DOS-side details ([`stage0_design.md`](stage0_design.md), [`stage1_design.md`](stage1_design.md))
- Wire-protocol byte-level specs that already live elsewhere ([`design.md` §9](design.md), [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md))
- Long-term TSR architecture ([`design.md` §21](design.md))

### Relationship to other docs

| Document | Relationship |
|---|---|
| [`design.md`](design.md) §20 | High-level firmware sketch; this doc is the detailed expansion. |
| [`hardware_reference.md`](hardware_reference.md) §3.3 | Pin allocation (authoritative). |
| [`pico_phase3_design.md`](pico_phase3_design.md) | Phase 3+ MVP — the immediate implementation slice. |
| [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) | LPT peripheral semantics this firmware mirrors. |
| [`ps2_eras_reference.md`](ps2_eras_reference.md) | PS/2 era differences this firmware auto-detects. |
| [`ps2_private_channel_design.md`](ps2_private_channel_design.md) | i8042 private-mode protocol this firmware serves. |
| [`two_plane_transport.md`](two_plane_transport.md) | Steady-state data plane this firmware carries. |
| Memory `ps2-oversampling-preference` | PS/2 architectural decision: PIO oversampling + instrumentation + auto-detect. |

---

## 2. Firmware lifecycle

Single trajectory from power-on to steady-state, observed by the **session state machine** (§7):

```
POWER_ON
    │
    ▼
BOOT                       embassy init, defmt-RTT, GPIO direction setup,
                           NeoPixel = solid red (boot in progress)
    │
    ▼
SELFTEST                   Pin direction, transceiver enable, PIO program
                           load, CRC engines, panic-handler test
                           NeoPixel = blinking red → solid yellow on pass
    │
    ▼
AWAIT_HOST_POWER           Wait for DOS host's KBD CLK line to rise (host
                           PSU on). Cold-start: no DEBUG-injection until
                           the host actually exists.
                           NeoPixel = solid yellow
    │
    ▼
DETECT_MACHINE_CLASS       Oversample PS/2 lines passively. Observe host
                           BAT (basic assurance test) traffic, command set,
                           timing. Classify host into MachineClass::
                           {Xt, At, Ps2}. (§5.PS/2 phy)
                           NeoPixel = blinking cyan
    │
    ▼
INJECT_DEBUG               Emit characters via PS/2 KBD as if typed by a
                           real keyboard. Drives a built-in DEBUG script
                           that creates STAGE0.COM on the host disk and
                           runs it. Script is class-specific.
                           NeoPixel = solid cyan
    │
    ▼
SERVE_STAGE0_DOWNLOAD      Stage 0 .COM is running on DOS. It will request
                           the rest of itself + Stage 1 over the chosen
                           bootstrap channel (LPT nibble or PS/2 private).
                           Pico serves embedded blobs.
                           NeoPixel = blinking blue
    │
    ▼
SERVE_STAGE1_HANDOFF       Stage 0 hands off to Stage 1. Pico answers
                           Stage 1's IEEE 1284 negotiation attempts
                           (or refuses them in Phase 3, forcing SPP fallback).
                           NeoPixel = blinking blue
    │
    ▼
SERVE_CAP_HANDSHAKE        CAP_REQ → CAP_RSP (size, CRC-32, version) → CAP_ACK
                           NeoPixel = solid blue
    │
    ▼
SERVE_STAGE2_DOWNLOAD      Block server: SEND_BLOCK → RECV_BLOCK with
                           BLOCK_ACK / BLOCK_NAK retries. Streams the
                           embedded PICO1284.EXE blob.
                           NeoPixel = blinking magenta
    │
    ▼
DP_READY                   Stage 1 EXECs PICO1284.EXE; child boots and
                           re-issues a CAP handshake on its own connection.
                           Pico transitions to the long-term protocol.
                           NeoPixel = solid magenta
    │
    ▼
DP_ACTIVE                  Steady state. Stage 2 driving the data plane.
                           Full command set: screen dump, file transfer,
                           console I/O, dual-lane fallback if needed.
                           NeoPixel = solid green
```

Transitions are one-way except for error states (§9) and the explicit re-bootstrap path (`RESET_REQUEST` cmd from the modern host).

---

## 3. Hardware context

Pin allocation is authoritative in [`hardware_reference.md` §3.3](hardware_reference.md). Brief recap of the budget:

- **9 GPIO** for IEEE 1284 (GP11, GP12-19 data bus, GP20, GP22-27, plus nInit routing TBD per [`pico_phase3_design.md` §LPT pin map](pico_phase3_design.md))
- **6 GPIO** for PS/2 KBD + AUX (GP2-6, GP9-10, GP28; bidirectional via 74LVC07A)
- **2 GPIO** for status indicators (GP7 red LED, GP21 NeoPixel)
- **2 GPIO** for debug UART (GP0/GP1)
- **1 GPIO** reserved (GP8 PSRAM CS — do not touch)
- **1 GPIO** spare (GP29)

**Resource budget:**

| Resource | Available | Phase 3+ usage | Steady-state usage |
|---|---|---|---|
| Cortex-M33 cores | 2 | 1 (executor-thread) | 2 (one per data plane) |
| PIO blocks | 3 | 1 (LPT) | 3 (LPT, PS/2, status) |
| State machines | 12 | 2-3 | 10-11 |
| DMA channels | 16 | 2 | ~8 |
| SRAM | 264 KB | <32 KB | ~200 KB |
| PSRAM | 8 MB | unused | screen buffers, history rings |
| Flash | 4 MB | ~256 KB | ~512 KB + embedded blobs |

---

## 4. Task and PIO architecture

### 4.1 Async task model

Built on **embassy 0.10** with the `executor-thread` (core 0) and `executor-interrupt` (core 1, for hard-realtime tasks).

```
core 0 (thread executor)
├── session_task          state machine driver (§7)
├── packet_decoder_task   drains LPT+PS/2 inboxes; runs CRC; dispatches
├── packet_encoder_task   drains tx outbox; encodes; pushes to phy
├── usb_cdc_task          USB CDC bridge to modern host
├── telemetry_task        periodic stats emit (defmt + CDC)
└── status_task           NeoPixel + red LED state-driven animation

core 1 (interrupt executor, hard-realtime)
├── lpt_phy_task          drives LPT byte pump (Phase 3: bit-bang;
│                         Phase 5+: PIO + DMA orchestration)
├── ps2_kbd_phy_task      oversample, frame-decode, classify;
│                         emit BAT, scancodes; receive DOS replies
└── ps2_aux_phy_task      same for AUX channel
```

**Inter-task channels** (all `embassy_sync::channel::Channel`, sized for ~16 messages):

```
lpt_rx_queue : Channel<NoopRawMutex, RawByte, 256>
lpt_tx_queue : Channel<NoopRawMutex, RawByte, 256>

ps2_kbd_rx_queue : Channel<NoopRawMutex, Ps2Frame, 32>
ps2_kbd_tx_queue : Channel<NoopRawMutex, Ps2Frame, 32>

ps2_aux_rx_queue : Channel<NoopRawMutex, Ps2Frame, 32>
ps2_aux_tx_queue : Channel<NoopRawMutex, Ps2Frame, 32>

packet_rx_queue : Channel<NoopRawMutex, IncomingPacket, 16>
packet_tx_queue : Channel<NoopRawMutex, OutgoingPacket, 16>

session_event_queue : Channel<NoopRawMutex, SessionEvent, 32>
telemetry_queue     : Channel<NoopRawMutex, TelemetryEvent, 64>

usb_cdc_rx_queue : Channel<NoopRawMutex, CdcChunk, 8>
usb_cdc_tx_queue : Channel<NoopRawMutex, CdcChunk, 8>
```

Mutex is `NoopRawMutex` because everything runs on the embassy executors (no preemptive context switches inside the same executor). Cross-core uses `CriticalSectionRawMutex`.

### 4.2 PIO and state-machine allocation

**Detailed program designs:** [`pio_state_machines_design.md`](pio_state_machines_design.md) — wire-protocol-level expansion of every entry in the table below, with PIO assembly sketches, CPU pre/post-processing, DMA configurations, and bring-up validation per program.

| PIO | SM | Phase | Program | Purpose |
|---|---|---|---|---|
| **0** | 0 | 3+ | `lpt_compat_in` | nInit-strobe-triggered byte capture (DOS → Pico, fwd mode). |
| | 1 | 3+ | `lpt_nibble_out` | Nibble presenter on status bits + phase toggle (Pico → DOS). |
| | 2 | 5+ | `lpt_epp_fwd` | EPP forward strobe handshake at line rate. |
| | 3 | 5+ | `lpt_ecp_rev_dma` | ECP reverse DMA-fed byte stream. |
| **1** | 0 | 1+ | `ps2_kbd_oversample` | 4-8× oversampler on KBD CLK/DATA → 32-bit frame to FIFO. |
| | 1 | 1+ | `ps2_kbd_tx` | Frame transmitter to host KBD (open-drain via 74LVC07A). |
| | 2 | 2+ | `ps2_aux_oversample` | Same for AUX channel. |
| | 3 | 2+ | `ps2_aux_tx` | Same for AUX TX. |
| **2** | 0 | 0+ | `ws2812_neopixel` | NeoPixel status indicator (GP21). |
| | 1 | TBD | reserved | Future: i8042 wire sniffer for diagnostics. |
| | 2 | TBD | reserved | Future: high-speed link experiments (HSTX). |
| | 3 | TBD | reserved | Spare. |

11 SMs allocated, 1 spare. All three PIO blocks utilized. Per [`design.md` §20.2](design.md), this isolates LPT, PS/2, and status into separate PIO blocks so a glitch in one can't stall the others.

### 4.3 DMA usage

| DMA ch | Owner | Direction | Notes |
|---|---|---|---|
| 0 | `lpt_compat_in` | PIO0 RX → SRAM ring | One byte per nInit strobe. |
| 1 | `lpt_nibble_out` | SRAM → PIO0 TX | Pre-staged nibbles + phase. |
| 2 | `lpt_epp_fwd` | SRAM → PIO0 TX | Phase 5+. Bulk-mode forward. |
| 3 | `lpt_ecp_rev_dma` | PIO0 RX → SRAM | Phase 5+. Reverse-mode bulk. |
| 4 | `ps2_kbd_oversample` | PIO1 RX → SRAM ring | 4-byte-per-sample bursts. |
| 5 | `ps2_kbd_tx` | SRAM → PIO1 TX | Per-frame. |
| 6 | `ps2_aux_oversample` | PIO1 RX → SRAM ring | Same. |
| 7 | `ps2_aux_tx` | SRAM → PIO1 TX | Same. |
| 8-15 | reserved | — | USB, future, spare. |

---

## 5. Module designs

Modules live as files under `firmware/src/`. Each module exports a public API used by the task layer above it.

```
firmware/src/
├── main.rs                    embassy entry; task spawning; init order
├── lifecycle.rs               session state machine (SessionState enum, transitions)
├── lpt/
│   ├── mod.rs                 trait LptPhy { send_byte, recv_byte, ... }
│   ├── compat.rs              SPP nibble bit-bang impl (Phase 3)
│   ├── epp.rs                 EPP PIO impl (Phase 5)
│   ├── ecp.rs                 ECP PIO impl (Phase 5)
│   └── negotiation.rs         IEEE 1284 negotiation watcher (Phase 4)
├── ps2/
│   ├── mod.rs                 trait Ps2Phy + MachineClass enum
│   ├── oversampler.rs         PIO oversampler + frame extractor
│   ├── framer.rs              bit-stream → Ps2Frame { data, parity, errors, timing }
│   ├── tx.rs                  outbound frame transmitter
│   ├── classifier.rs          MachineClass auto-detection from frame stats
│   └── instrumentation.rs     timing histograms, glitch counts, edge skew
├── i8042/
│   ├── mod.rs                 private-channel client; LED-pattern unlock,
│   │                          AUX 200/100/80 knock; sees from peripheral side
│   ├── kbd_private.rs         kbd-private byte pump (post-unlock)
│   └── aux_private.rs         aux-private byte pump (post-unlock)
├── packet/
│   ├── mod.rs                 SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX encode + decode
│   ├── crc16.rs               CRC-16/CCITT-FALSE (matches stage1.asm:705)
│   ├── crc32.rs               CRC-32/IEEE reflected (matches stage1.asm:1692)
│   └── commands.rs            CMD_* IDs (matches stage1.asm:106-117)
├── protocol/
│   ├── mod.rs                 command dispatcher; routes to handlers
│   ├── cap.rs                 CAP_REQ → CAP_RSP responder
│   ├── ping.rs                PING → PONG echo
│   ├── block_server.rs        SEND_BLOCK / RECV_BLOCK / ACK / NAK state
│   └── stage_blobs.rs         embedded Stage 0 / 1 / 2 blob accessors
├── debug_inject/
│   ├── mod.rs                 DEBUG script generator + driver
│   ├── script_xt.rs           XT-class injection script
│   ├── script_at.rs           AT-class injection script
│   └── script_ps2.rs          PS/2-class injection script
├── transport/
│   ├── mod.rs                 plane abstraction; routes packets to LPT or PS/2 channels
│   ├── single_plane.rs        Phase 3+: LPT only OR PS/2 only
│   └── dual_plane.rs          Phase 11+: LPT data + PS/2 control concurrently
├── usb_cdc/
│   ├── mod.rs                 USB CDC stack; modern-host bridge
│   ├── packet_channel.rs      framed packet I/O over CDC
│   └── telemetry_channel.rs   second CDC interface for stats stream
├── status/
│   ├── mod.rs                 NeoPixel + red LED driver
│   ├── neopixel.rs            PIO WS2812 driver
│   └── animations.rs          per-state color/blink patterns
├── telemetry/
│   ├── mod.rs                 stats aggregation + emit
│   ├── ring.rs                SRAM ring buffer for events
│   └── psram_log.rs           PSRAM long-history log (optional)
├── crc.rs                     shared CRC tables/helpers (referenced by packet/*)
└── util/
    ├── pin.rs                 GPIO direction helpers
    ├── ringbuf.rs             SPSC ring for cross-task data
    └── time.rs                low-jitter delay helpers
```

The full tree is built lazily — Phase 3+ only needs `main`, `lifecycle`, `lpt/{mod,compat,negotiation}`, `packet/*`, `protocol/{mod,cap,ping,block_server,stage_blobs}`, `status/*`, `telemetry/{mod,ring}`. Everything else can be stub modules with `unimplemented!()` bodies until its phase arrives.

### 5.1 LPT phy

#### `trait LptPhy`

```rust
pub trait LptPhy {
    async fn send_byte(&mut self, b: u8) -> Result<(), LptError>;
    async fn recv_byte(&mut self) -> Result<u8, LptError>;
    fn current_mode(&self) -> LptMode;
}

pub enum LptMode { Spp, Byte, Epp, Ecp, EcpDma }
```

#### Phase 3: `compat.rs` (SPP nibble bit-bang)

Mirrors the wire protocol of `dos/stage0/lpt_nibble.inc`:

- **recv_byte**: wait for nInit falling edge (interrupt or polling), read GP12-19, return byte.
- **send_byte**: present low 4 bits on status bits [3..6], toggle phase bit [7], wait for the host to consume (looped read of nInit), present high 4 bits, toggle phase again.
- Maintains the **persistent phase invariant**: `last_phase` tracks the most-recently-emitted phase so the host's `lpt_recv_nibble` (with its own `last_phase`) sees a clean transition.

Bit-bang is fine for Phase 3 because SPP nibble tops out at ~50 KB/s — well below what bit-bang can drive. PIO becomes mandatory at EPP/ECP rates.

#### Phase 4: `negotiation.rs`

Watches the IEEE 1284 negotiation sequence per [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md). State machine:

```
IDLE
  ├─ observe nSelectIn HI + nAutoFd LO → enter NEG_PENDING
  └─ otherwise stay idle

NEG_PENDING
  ├─ latch xflag byte from data bus (GP12-19)
  ├─ pulse nAck LO (250 ns) to acknowledge
  └─ wait for next nStrobe pulse

  on nStrobe pulse:
    ├─ if xflag ∈ {XFLAG_BYTE, XFLAG_ECP, XFLAG_EPP} and supported:
    │     set Select HI, set Xflag bit reflection, enter target mode
    └─ else:
          set Select LO (reject)
          enter SPP idle
```

#### Phase 5: `epp.rs` and `ecp.rs`

PIO programs handle the strobe handshakes at full line rate (~500 kB/s for EPP, ~2 MB/s for ECP with DMA). Detailed sequencing: TBD when this phase is reached.

### 5.2 PS/2 phy

Per memory `ps2-oversampling-preference`, all PS/2 receive paths use **PIO oversampling** rather than CLK-edge interrupts.

#### `trait Ps2Phy`

```rust
pub trait Ps2Phy {
    async fn send_frame(&mut self, frame: Ps2Frame) -> Result<(), Ps2Error>;
    async fn recv_frame(&mut self) -> Ps2Frame;
    fn machine_class(&self) -> Option<MachineClass>;
    fn instrumentation_snapshot(&self) -> Ps2Stats;
}

pub enum MachineClass { Xt, At, Ps2 }

pub struct Ps2Frame {
    pub data: u8,
    pub parity_ok: bool,
    pub framing_ok: bool,
    pub timing: FrameTiming,
}

pub struct FrameTiming {
    pub bit_periods_ns: [u32; 11],   // measured bit-by-bit period
    pub clk_data_skew_ns: i32,       // signed skew at start bit
    pub glitch_count: u8,            // sub-bit-period CLK pulses observed
}

pub struct Ps2Stats {
    pub frames_total: u64,
    pub frames_errored: u64,
    pub bit_period_histogram: [u32; 16],  // log-spaced buckets
    pub edge_skew_histogram: [u32; 16],
}
```

#### `oversampler.rs` (PIO)

PIO program oversamples both CLK and DATA at **~250 kHz minimum** (≈4× nominal AT clock rate). Each sample writes a 2-bit value (CLK, DATA) into a 32-bit shift register; when the register fills, push to FIFO via DMA.

#### `framer.rs` (CPU)

Consumes oversampled stream, extracts bit transitions:

1. Detect start bit (CLK falling edge with DATA low).
2. For each of 11 expected bit slots: find CLK falling edge ± half-period window, sample DATA.
3. Validate parity (odd for AT/PS/2; absent for XT).
4. Validate stop bit (high; absent for XT).
5. Emit `Ps2Frame` with timing metadata.

Glitch detection: count CLK transitions that are shorter than `MIN_BIT_PERIOD_NS / 4`.

#### `classifier.rs` (auto-detect XT/AT/PS/2)

Observes the first ~10 frames during `DETECT_MACHINE_CLASS`. Classification rules:

| Observation | Conclusion |
|---|---|
| Frame length = 9 bits, no parity, no stop, ~5-10 kHz clock | `Xt` |
| Frame length = 11 bits with odd parity + stop, ~10-16.7 kHz | `At` |
| `At` framing + AUX channel produces traffic | `Ps2` |
| Host responds to `0xFF` reset command | `At` or `Ps2` (XT keyboards don't respond) |

The classifier emits `MachineClassDetected(class)` to `session_event_queue` once confident (3 consecutive matching frames).

#### `instrumentation.rs`

Aggregates `FrameTiming` into `Ps2Stats`:
- Updates bit-period histogram (16 log-spaced buckets from 30 µs to 200 µs).
- Updates CLK/DATA skew histogram (16 linear buckets from -10 µs to +10 µs).
- Emits `TelemetryEvent::Ps2Frame { class, frame, stats_delta }` to `telemetry_queue`.

Defmt-RTT log line per second: `PS2 KBD: 487 frames, 0 errors, p99 clk=83µs, skew=+0.2µs`.

### 5.3 i8042 private channel

#### `i8042/mod.rs`

The Pico-side dual of [`ps2_private_channel_design.md`](ps2_private_channel_design.md). Observes the LED-pattern unlock sequence and the AUX 200/100/80 knock sent by Stage 0. Once unlocked, the KBD wire becomes a generic byte pump (`kbd_private.rs`); the AUX wire becomes another byte pump (`aux_private.rs`).

State machine:

```
LOCKED
  └─ observe LED pattern matching unlock_byte_trace:
       sequence of SET_LEDS 0xED followed by N data bytes (the unlock key)
       ACK each with 0xFA back
     → enter UNLOCK_PENDING

UNLOCK_PENDING
  └─ observe AUX SET_SAMPLE_RATE 200, 100, 80 in order
     → enter UNLOCKED, both wires are now generic byte pumps

UNLOCKED
  └─ normal byte-pump behavior; bytes from DOS go straight into
     packet_rx_queue prefixed with channel tag (KBD or AUX)
```

The Pico must accept *any* byte stream while UNLOCKED — Stage 0's protocol on the wire is not PS/2 framing anymore, it's our packet protocol. The PS/2 phy is reused for bit transport (oversampler still extracts bytes correctly), but framing semantics shift from PS/2 keyboard scancodes to our SOH/CMD/SEQ/... packets.

### 5.4 Packet framing

Wire format identical to `dos/stage1/stage1.asm:97-122`:

```
SOH | CMD | SEQ | LEN_HI | LEN_LO | PAYLOAD | CRC_HI | CRC_LO | ETX
```

#### `packet/mod.rs`

```rust
pub struct IncomingPacket {
    pub cmd: u8,
    pub seq: u8,
    pub payload: heapless::Vec<u8, 256>,  // matches PACKET_BUF_SIZE
    pub source: PacketSource,             // Lpt | Ps2Kbd | Ps2Aux | UsbCdc
}

pub struct OutgoingPacket {
    pub cmd: u8,
    pub payload: heapless::Vec<u8, 256>,
    pub dest: PacketSource,
}

pub fn encode(p: &OutgoingPacket, seq: u8, buf: &mut [u8]) -> usize;
pub fn decode(buf: &[u8]) -> Result<IncomingPacket, DecodeError>;
```

CRC-16 + CRC-32 are bit-by-bit (~30 B equivalent in Rust). Optimizable later with `crc` crate's table-driven impls; deferred until profiling says it matters.

#### Sequence number tracking

Pico keeps:
- `rx_seq_expected: u8` — incremented on each valid packet receipt.
- `tx_seq: u8` — incremented on each packet send.

Mismatched RX seq → `TelemetryEvent::SeqGap { expected, got }`. Doesn't drop the packet (Stage 1's retry logic handles re-sync via block_no), just logs.

### 5.5 Protocol dispatcher

#### `protocol/mod.rs`

```rust
pub async fn dispatch(p: IncomingPacket, state: &mut SessionState) {
    match p.cmd {
        CMD_CAP_REQ      => cap::handle_req(p, state).await,
        CMD_CAP_ACK      => cap::handle_ack(p, state).await,
        CMD_PING         => ping::handle(p, state).await,
        CMD_SEND_BLOCK   => block_server::handle_send(p, state).await,
        CMD_BLOCK_ACK    => block_server::handle_ack(p, state).await,
        CMD_BLOCK_NAK    => block_server::handle_nak(p, state).await,
        _ => telemetry::report(UnknownCmd { cmd: p.cmd }),
    }
}
```

Each handler is a small async function. State (session-scope) is passed by mutable reference; cross-task state is held in `embassy_sync::Mutex<CriticalSectionRawMutex, _>` if shared.

### 5.6 Stage-blob server

#### `stage_blobs.rs`

Three embedded blobs:

```rust
pub static STAGE0_XT:  &[u8] = include_bytes!("../assets/S0_XT.COM");
pub static STAGE0_AT:  &[u8] = include_bytes!("../assets/S0_AT.COM");
pub static STAGE0_PS2: &[u8] = include_bytes!("../assets/S0_PS2.COM");
pub static STAGE1:     &[u8] = include_bytes!("../assets/stage1.bin");
pub static STAGE2:     &[u8] = include_bytes!("../assets/PICO1284.EXE");

pub const STAGE2_CRC32: u32 = stage2_crc32_compile_time();
```

`build.rs` computes `STAGE2_CRC32` at build time so the firmware can drop it into CAP_RSP without runtime computation.

Stage 1 is served as a single download (small enough); Stage 2 is block-served with the protocol above.

### 5.7 Debug injection

#### `debug_inject/mod.rs`

Generates and types a DEBUG script via the PS/2 KBD phy. The script:

1. Creates `STAGE0.COM` via DEBUG's E (enter) command — one hex byte at a time.
2. Writes the file via `R` + `W` commands.
3. Runs it via `G` (go).

Script length depends on Stage 0 size. For an XT-class S0 of ~1 KB, the DEBUG script is ~3 KB of typed characters. Injection takes ~75 seconds on XT, ~20 seconds on AT, ~10 seconds on PS/2 (per [`stage0_design.md` §Stage –1 injection duration](stage0_design.md)).

The class-specific script files differ in:
- Which Stage 0 binary is embedded (S0_XT.COM / S0_AT.COM / S0_PS2.COM)
- Inter-character pacing (XT needs more delay due to slow BIOS keyboard handling)
- Specific DEBUG commands used (some early DEBUG versions lack the `N` rename command)

### 5.8 USB CDC bridge

**Telemetry wire-protocol details:** [`instrumentation_surface.md` §5](instrumentation_surface.md) specifies the JSON-line schema, event taxonomy, command set, and backpressure policy on the telemetry channel.

#### `usb_cdc/mod.rs`

Two USB CDC interfaces:

| Interface | Purpose | Direction |
|---|---|---|
| **CDC 0 (data)** | Packet stream to modern host | Bidirectional |
| **CDC 1 (telemetry)** | Stats / events log | Pico → host only |

Packet stream is framed identically to LPT — same SOH/CMD/SEQ/LEN/CRC/ETX format, so the modern host can use the same decoder library DOS uses (if we ever build one).

Telemetry stream is line-oriented text: `[ts_ms] PS2 KBD: 487 frames, 0 errors, p99=83µs, skew=+0.2µs`.

### 5.9 Status indicators

#### `status/neopixel.rs`

PIO WS2812 driver (well-trodden territory; many open-source impls). PIO0 SM0... no, PIO2 SM0 per the allocation table. Drives GP21.

#### `status/animations.rs`

Maps `SessionState` → color/pattern:

| State | Color | Pattern |
|---|---|---|
| BOOT | red | solid |
| SELFTEST | red | blink 4 Hz |
| SELFTEST passed | yellow | solid |
| AWAIT_HOST_POWER | yellow | solid |
| DETECT_MACHINE_CLASS | cyan | blink 2 Hz |
| INJECT_DEBUG | cyan | solid |
| SERVE_STAGE0_DOWNLOAD | blue | blink 2 Hz |
| SERVE_STAGE1_HANDOFF | blue | blink 4 Hz |
| SERVE_CAP_HANDSHAKE | blue | solid |
| SERVE_STAGE2_DOWNLOAD | magenta | blink 2 Hz |
| DP_READY | magenta | solid |
| DP_ACTIVE | green | solid |
| ERROR (recoverable) | orange | blink 4 Hz |
| FAULT (unrecoverable) | red | blink 8 Hz |

Red LED on GP7 stays unused after `SELFTEST` — purely a Phase 0 indicator. The NeoPixel covers all states.

### 5.10 Telemetry

**Operator-facing surface specification:** [`instrumentation_surface.md`](instrumentation_surface.md) defines the full console output formats, TUI dashboard views, CDC telemetry JSON protocol, and signature database design.

#### `telemetry/ring.rs`

Lock-free SPSC ring of `TelemetryEvent`. Size: 256 events (8 KB at 32 B/event). Single producer = whoever emits the event (any task); single consumer = `telemetry_task`.

#### `telemetry_task`

Drains the ring, emits to:
1. `defmt::info!` for development builds (formatted per [`instrumentation_surface.md` §3](instrumentation_surface.md)).
2. CDC 1 interface as JSON-line events (per [`instrumentation_surface.md` §5](instrumentation_surface.md)).

Per-state aggregations are computed in the consumer, not the producer, to keep emit hot paths cheap.

---

## 6. Memory map

### 6.1 SRAM (264 KB on RP2350)

```
0x2000_0000  ┌──────────────────────────────────┐
             │ .text in SRAM (cache-resident    │  ~32 KB
             │ critical paths only; rest in     │
             │ flash XIP)                       │
             ├──────────────────────────────────┤
             │ .rodata (small constants)        │  ~4 KB
             ├──────────────────────────────────┤
             │ .data + .bss                     │  ~16 KB
             │   - task queues                  │
             │   - session state                │
             │   - PS/2 ring buffers            │
             │   - LPT ring buffers             │
             ├──────────────────────────────────┤
             │ DMA rings (oversample / nibble)  │  ~16 KB
             ├──────────────────────────────────┤
             │ Stack (core 0 thread executor)   │   8 KB
             │ Stack (core 1 interrupt exec)    │   8 KB
             ├──────────────────────────────────┤
             │ unused / future                  │  ~180 KB
             └──────────────────────────────────┘
0x2004_2000
```

Phase 3+ uses well under 80 KB. PSRAM unused until Phase 7+ (screen buffers).

### 6.2 PSRAM (8 MB on Feather)

Reserved for:
- Screen capture frame ring (Phase 7+)
- File staging buffers (Phase 6+)
- Telemetry long-history log (optional)
- LZ4/LZSS dictionaries (Phase 8+)

Access via `embassy_rp::psram` (or a thin wrapper). Cache-coherency: RP2350 PSRAM is accessed via XIP cache by default; explicit cache flushes needed before DMA reads from PSRAM.

### 6.3 Flash (4 MB minimum)

```
0x1000_0000  ┌──────────────────────────────────┐
             │ Bootloader / picotool header     │   4 KB
             ├──────────────────────────────────┤
             │ .text + .rodata (firmware code)  │  ~256 KB
             ├──────────────────────────────────┤
             │ Embedded Stage 0 blobs (×3)      │  ~6 KB
             │   S0_XT.COM:   ~1 KB             │
             │   S0_AT.COM:   ~2 KB             │
             │   S0_PS2.COM:  ~2 KB             │
             ├──────────────────────────────────┤
             │ Embedded Stage 1 blob            │  ~5 KB
             │   stage1.bin v1.0: 4821 B        │
             ├──────────────────────────────────┤
             │ Embedded Stage 2 blob            │  TBD: 50-200 KB
             │   PICO1284.EXE                   │
             ├──────────────────────────────────┤
             │ unused / future                  │  ~3.5 MB
             └──────────────────────────────────┘
```

Stage blobs are positioned via linker section attributes so a `picotool save` can extract them individually for verification.

---

## 7. Session state machine

Lives in `lifecycle.rs`. Single `SessionState` enum, single global `session_task` driving transitions in response to `SessionEvent`s.

```rust
pub enum SessionState {
    Boot,
    Selftest,
    AwaitHostPower,
    DetectMachineClass,
    InjectDebug { class: MachineClass, progress: u8 },
    ServeStage0Download,
    ServeStage1Handoff,
    ServeCapHandshake,
    ServeStage2Download { current_block: u16, total_blocks: u16 },
    DpReady,
    DpActive,
    Error { kind: ErrorKind, recoverable: bool },
}

pub enum SessionEvent {
    SelftestPassed,
    HostPowerDetected,
    MachineClassDetected(MachineClass),
    DebugInjectionComplete,
    Stage0DownloadComplete,
    CapHandshakeComplete,
    BlockDownloaded { block_no: u16 },
    Stage2DownloadComplete,
    Stage2ExecAcknowledged,
    Error(ErrorKind),
    Reset,
}
```

Transitions are pure functions of `(state, event) → state`. Side effects (NeoPixel update, telemetry emit) happen on transition via `on_enter(state)`.

---

## 8. Bootstrap injection (Phase 1)

Detailed sequence after `MachineClassDetected`:

1. Pick Stage 0 blob: `S0_XT.COM` / `S0_AT.COM` / `S0_PS2.COM`.
2. Generate DEBUG script:
   ```
   N STAGE0.COM   (name file)
   E 0100         (enter at offset 0x100)
   <hex bytes>    (one chunk per DEBUG line; ~32 B per line)
   ...
   RBX 0          (zero BX high)
   RCX <size>     (set size in CX)
   W              (write)
   Q              (quit DEBUG)
   STAGE0         (run)
   ```
3. Type characters via `ps2_kbd_phy_task.send_frame` at class-specific pacing.
4. Watch for failure indicators on the wire:
   - DEBUG echoes characters back — if we observe no echoes, something's wrong.
   - DEBUG prompts `-` for each new command — wait for the prompt before sending next line.
5. Transition to `ServeStage0Download` when the final `STAGE0` Enter is sent.

Robustness:
- If DEBUG prompt times out (>2 s), retry the last line up to 3 times.
- If multiple retries fail, transition to `Error(ErrorKind::DebugInjectionFailed)`.

---

## 9. Error handling and recovery

### 9.1 Error taxonomy

```rust
pub enum ErrorKind {
    SelftestFailed,
    HostPowerLost,
    MachineClassAmbiguous,         // can't classify after N frames
    DebugInjectionFailed,
    Stage0DownloadTimeout,
    CapHandshakeTimeout,
    Stage2DownloadCrcMismatch,
    LptPhyFault { details: LptError },
    Ps2PhyFault { channel: Channel, details: Ps2Error },
    UsbCdcDisconnected,
    Bug(&'static str),             // last resort; panic-equivalent
}
```

### 9.2 Recovery policy

| ErrorKind | Recovery |
|---|---|
| SelftestFailed | None. NeoPixel red blink fast. Wait for reset. |
| HostPowerLost | Return to `AwaitHostPower`; resume from scratch when host returns. |
| MachineClassAmbiguous | Continue oversampling; emit telemetry for human triage. |
| DebugInjectionFailed | Retry up to 3 times, then fault. |
| Stage0/Stage2 timeout/CRC | Return to `AwaitHostPower` (user must reboot DOS). |
| LptPhyFault | Reset PIO program; if persistent, fault. |
| Ps2PhyFault | Reset PIO program; if KBD wire is fatal, fault. AUX faults are non-fatal in single-plane mode. |
| UsbCdcDisconnected | Continue without modern-host bridge; resume CDC tasks when reconnected. |
| Bug | Panic-probe; defmt-RTT dumps backtrace; NeoPixel red blink fast. |

### 9.3 Recovery primitives

- **PIO reset**: `pio.sm_restart()` + reload program. ~10 µs.
- **DMA channel reset**: `dma.ch_reset()`. ~1 µs.
- **Session reset** (`SessionEvent::Reset`): hard fall back to `AwaitHostPower`.
- **Crash recovery**: panic-probe hooks into defmt; on panic, dump trace + restart core.

---

## 10. Build system

### 10.1 Build pipeline

```
dos/build/{S0_XT,S0_AT,S0_PS2}.COM         (NASM, built by `make -C dos stage0`)
dos/build/stage1.bin                       (NASM, built by `make -C dos stage1`)
dos/build/PICO1284.EXE                     (Open Watcom + NASM, future)
                │
                ▼ (cp or symlink)
firmware/assets/{...}
                │
                ▼ (include_bytes!)
firmware/target/.../vintage-kvm-firmware (.elf, .uf2)
                │
                ▼ (probe-rs / picotool)
RP2350 flash
```

Wire-up: `firmware/build.rs` checks `assets/` exists, computes `STAGE2_CRC32` at build time:

```rust
let stage2 = include_bytes!("assets/PICO1284.EXE");
let crc = crc32_compute(stage2);
writeln!(f, "pub const STAGE2_CRC32: u32 = 0x{:08X};", crc)?;
```

### 10.2 Cargo aliases

`.cargo/config.toml`:

```toml
[alias]
fw-check    = "check --manifest-path firmware/Cargo.toml --target thumbv8m.main-none-eabihf"
fw-build    = "build --manifest-path firmware/Cargo.toml --release"
fw-run      = "run   --manifest-path firmware/Cargo.toml --release"
ci          = "fw-check"
test-all    = "fw-check"
```

`cargo test --workspace` currently has no real tests (the firmware is `#![no_std]` and can't run host-side unit tests trivially); the `ci`/`test-all` aliases redirect to `fw-check` for now.

### 10.3 Profiles

```toml
[profile.dev]
opt-level = 1                  # required: PIO timing is unusable at -O0
debug = 2

[profile.release]
opt-level = "z"                # size matters for flash budget
debug = 1                      # keep for defmt frames
lto = "fat"
codegen-units = 1
panic = "abort"
```

---

## 11. Testing strategy

### 11.1 Unit-testable layers

Most embedded Rust code can't run on the host, but pure-logic modules can with feature gating:

- `packet/crc16.rs`, `packet/crc32.rs`: pure functions; `#[cfg(test)]` host tests verify byte-for-byte match with Stage 1's outputs.
- `packet/mod.rs` encode/decode: round-trip tests on known wire fragments.
- `protocol/cap.rs`: handler is `async fn(IncomingPacket, &mut State) -> OutgoingPacket`; testable with a mock state.
- `ps2/classifier.rs`: feed canned `Ps2Frame` streams, assert `MachineClass` output.

Test command: `cargo test --manifest-path firmware/Cargo.toml --features test-on-host --target <host-triple>`.

### 11.2 Bench validation

Real bench validation requires actual hardware:
- Logic analyzer hooked to GP11/GP12-19 for LPT.
- Logic analyzer hooked to GP2-5 for PS/2 KBD.
- Vintage AT-class machine with DEBUG.COM.

Bench protocol (per phase):
- Phase 3.1-3.3: oscilloscope-level pin direction + edge timing.
- Phase 3.4-3.7: full DOS-side stack (Stage 0 → Stage 1) running against the firmware.
- Phase 1-2: vintage machine with the firmware emulating a keyboard.

### 11.3 Telemetry as a test surface

Every test failure produces a `TelemetryEvent` with enough context to reproduce. The defmt-RTT log alone is sufficient for most debugging; the CDC 1 stream captures everything for offline analysis.

---

## 12. Open decisions

1. **`nInit` GPIO routing**: Stage 0 uses LPT control bit 2 (nInit) as the host strobe. Need to confirm which Pico GPIO sees this through the 74LVC161284. May require board revision.

2. **Status-bit mapping for nibble output**: The four nibble bits map to status[3..6] on the wire. Need to confirm the GPIO↔status-bit mapping through the transceiver. Currently assumed: GP23 (nAck) = bit 6, GP25 (PError) = bit 5, GP26 (Select) = bit 4, GP27 (nFault) = bit 3.

3. **PS/2 oversample rate**: 4× nominal (≈250 kHz for AT) is the proposed minimum. Higher = better instrumentation but more PIO/DMA bandwidth. Decision deferred to Phase 1 implementation.

4. **PSRAM cache flush strategy**: Phase 7+ concern. RP2350 supports PSRAM-as-XIP and DMA, but coherency is software-managed. Need to pick a flush primitive (single-block vs full-cache).

5. **Multi-core split**: Current plan puts phys on core 1. May reverse if PIO+DMA push enough work to make core 0's session/dispatcher the bottleneck. Phase 5+ profiling decides.

6. **Embedded Stage 2 size**: `PICO1284.EXE` doesn't exist yet. Bench testing Phase 3 uses a placeholder. Real Stage 2 may exceed 50 KB; flash budget has room (3.5 MB free) but boot time matters.

7. **USB CDC backpressure**: If the modern host stalls, CDC TX queue fills, and the protocol dispatcher stalls. Solution: drop telemetry events first, then drop oldest packet data. Detailed policy TBD.

---

## 13. Phasing summary

How the modules light up as phases land:

| Phase | New modules | Modules touched | Acceptance |
|---|---|---|---|
| 0 | `main`, `status/neopixel` (stub) | — | Red LED blinks on GP7. |
| 1 | `ps2/{oversampler, framer, tx, classifier, instrumentation}`, `debug_inject/*` | `lifecycle` | XT/AT/PS/2 auto-detect on bench; DEBUG script types correctly. |
| 2 | `i8042/*` | `ps2/*`, `lifecycle` | Stage 0 loads and runs from injected DEBUG. |
| **3** | `lpt/{mod, compat}`, `packet/*`, `protocol/{mod, cap, ping, block_server, stage_blobs}`, `status/animations` | `lifecycle` | Stage 1 v1.0 downloads + EXECs placeholder Stage 2. |
| 4 | `lpt/negotiation` | `lpt/mod`, `protocol/cap` | Stage 1's negotiation ladder lands on ECP/EPP/Byte. |
| 5 | `lpt/{epp, ecp}` | `lpt/mod`, `transport/*` | EPP/ECP byte pumps work; throughput measured. |
| 6 | `transport/dual_plane` | All transport-touching modules | File transfer over single + dual plane. |
| 7+ | screen capture, compression, VESA, full TSR | — | Future. |

Phase 3 is the immediate scope. Phase 4-5 unblock Stage 1's auto-downgrade ladder. Phase 1-2 unblock end-to-end bootstrap (currently we rely on operator manually copying Stage 0 to a floppy).

---

## 14. Related documents

- [`design.md`](design.md) §20 — high-level firmware sketch (this doc expands it)
- [`design.md`](design.md) §22 — phase roadmap
- [`pico_phase3_design.md`](pico_phase3_design.md) — Phase 3+ MVP implementation slice
- [`pio_state_machines_design.md`](pio_state_machines_design.md) — PIO program designs for every PS/2 and LPT mode
- [`instrumentation_surface.md`](instrumentation_surface.md) — console formats, TUI dashboard, CDC telemetry protocol, signature DB
- [`firmware_crate_and_trait_design.md`](firmware_crate_and_trait_design.md) — workspace crate split + trait taxonomy across phy/protocol/transport/session/telemetry layers
- [`hardware_reference.md`](hardware_reference.md) — pin allocation
- [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) — controller-side dual
- [`ps2_eras_reference.md`](ps2_eras_reference.md) — PS/2 era / framing differences
- [`ps2_private_channel_design.md`](ps2_private_channel_design.md) — i8042 unlock protocol
- [`two_plane_transport.md`](two_plane_transport.md) — steady-state architecture
- [`stage0_design.md`](stage0_design.md) — DOS-side dual for bootstrap
- [`stage1_design.md`](stage1_design.md), [`stage1_implementation.md`](stage1_implementation.md) — DOS-side dual for IEEE 1284 stage
- Memory `ps2-oversampling-preference` — PS/2 architectural decision
