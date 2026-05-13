# Firmware crate and trait model

Design for splitting the firmware into a small Cargo workspace and the trait taxonomy that ties the layers together. Companion to [`pico_firmware_design.md`](pico_firmware_design.md) (architecture) and [`instrumentation_surface.md`](instrumentation_surface.md) (surfaces); this document specifies the **module and contract structure** rather than what each module does internally.

The goal is to make the firmware:

1. **Testable on host** for everything that doesn't need hardware (packet codec, command dispatcher, classifier, signature DB).
2. **Reusable** — types emitted by the firmware (telemetry events, signature features) are consumed by `tools/tui/` and any future host-side tooling without duplication.
3. **Swappable** — bit-bang vs PIO LPT phy, software vs DMA-sniffer CRC, defmt vs CDC vs noop telemetry sink. Each layer takes the layer below by trait, not by concrete type.
4. **Layered** — clear contracts between phy, transport, session, and surface, so a change at one layer doesn't ripple.

---

## 1. Crate workspace structure

### 1.1 Proposed layout

```
vintage-kvm/
├── Cargo.toml                          workspace root
├── crates/
│   ├── protocol/                       no_std no_alloc; host + device target
│   │   ├── Cargo.toml                  package = "vintage-kvm-protocol"
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── packet/                 SOH/CMD/SEQ/LEN/CRC/ETX framing
│   │       │   ├── mod.rs              encode / decode
│   │       │   ├── commands.rs         CMD_* constants
│   │       │   ├── crc16.rs            CRC-16-CCITT
│   │       │   └── crc32.rs            CRC-32/IEEE reflected
│   │       ├── cap.rs                  CAP_RSP payload builder
│   │       ├── block_server.rs         block-server logic (pure)
│   │       ├── handoff.rs              Stage 0 → Stage 1 ABI constants
│   │       └── stage1_constants.rs     shared with DOS (offsets / magic)
│   ├── telemetry-schema/               no_std; serde types for CDC events
│   │   ├── Cargo.toml                  package = "vintage-kvm-telemetry-schema"
│   │   └── src/
│   │       └── lib.rs                  Event enum, Stats, Histogram, Fingerprint
│   ├── signatures/                     no_std; keyboard/chipset DB + match
│   │   ├── Cargo.toml                  package = "vintage-kvm-signatures"
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── features.rs             KeyboardFeatures struct
│   │       ├── delta.rs                Δ-distance algorithm
│   │       ├── hash.rs                 stable fingerprint hash
│   │       └── known.rs                seed entries (IBM Model M, etc.)
│   └── firmware/                       cortex-m bin
│       ├── Cargo.toml                  package = "vintage-kvm-firmware"
│       ├── build.rs
│       ├── memory.x
│       ├── .cargo/config.toml
│       └── src/                        ←  current firmware/src/ moves here
│           └── …                       (all embedded-specific code)
├── tools/
│   └── tui/                            host-side dashboard crate
│       ├── Cargo.toml                  deps: vintage-kvm-telemetry-schema
│       │                                       vintage-kvm-signatures
│       └── src/
│           └── …
└── docs/
    └── …
```

### 1.2 Why this split

| Crate | Targets | Why split |
|---|---|---|
| `protocol` | host + cortex-m | Pure-logic packet/CRC/command code is the **same** on both sides. Host-side tooling (TUI, fixtures, integration tests) needs to encode/decode the same wire format the firmware uses. Single source of truth eliminates schema drift. |
| `telemetry-schema` | host + cortex-m | The TUI parses what the firmware emits. Sharing the serde-derived types means an `Event::Frame { … }` variant added in the firmware automatically deserializes correctly in the TUI on `cargo update`. No JSON-schema drift bugs. |
| `signatures` | host + cortex-m | Keyboard/chipset signature database. Lives on the device for live classification; lives on the host for offline signature DB curation (add entries from logged captures, regenerate). |
| `firmware` | cortex-m only | embassy-rp, PIO, USB CDC, defmt-RTT. Embedded-only. |

The three shared crates are **all `no_std` + `no_alloc`**, so they're trivially device-compatible. Host code that needs `std` adds `serde` / `serde_json` etc. via the host-side build profile.

### 1.3 Feature flags

Each shared crate uses small, well-defined features rather than one big `std` flag:

```toml
# crates/telemetry-schema/Cargo.toml
[features]
default = []
serde = ["dep:serde"]                    # JSON serialization (host-side + CDC)
defmt = ["dep:defmt"]                    # Embedded structured logging
```

- Device build: `features = ["defmt", "serde"]` (both — CDC needs `serde`, RTT logging needs `defmt`).
- Host build: `features = ["serde"]`.
- Host build with size-stripped data only: `features = []` (rare; useful for embedded host fixtures).

### 1.4 Migration plan from the current single-crate

Three small commits:

1. **Add workspace crates without moving anything.**
   - Create `crates/protocol/`, `crates/telemetry-schema/`, `crates/signatures/` with their `Cargo.toml` and empty `lib.rs`.
   - Add them to workspace `members`.
   - Verify `cargo ci` still passes (no-op change for the firmware).
2. **Extract `packet/` into `crates/protocol/`.**
   - Move `firmware/src/packet/*` to `crates/protocol/src/packet/`.
   - Re-export from firmware via `use vintage_kvm_protocol::packet::*;` to avoid touching every call site.
   - `protocol/src/cap.rs`, `block_server.rs` follow.
   - Verify `cargo ci` and (eventually) `cargo test -p vintage-kvm-protocol` on host.
3. **Add `telemetry-schema` and `signatures` from scratch** once those firmware modules are implemented (they don't exist yet — Phase 3 MVP has neither).

`firmware/` itself stays at the repo root for v1 to minimize churn. Migrating to `crates/firmware/` can happen later (or never; it's cosmetic).

---

## 2. Trait taxonomy

Five layers, each consuming the layer below by trait:

```
   ┌─────────────────────────────────────────────────────────┐
   │  Layer 5: Telemetry                                      │
   │     trait TelemetryEmit                                  │
   └─────────────────────────────────────────────────────────┘
                                ▲ observes
   ┌─────────────────────────────────────────────────────────┐
   │  Layer 4: Session                                        │
   │     trait SessionSupervisor                              │
   └─────────────────────────────────────────────────────────┘
                                │ owns
                                ▼
   ┌─────────────────────────────────────────────────────────┐
   │  Layer 3: Transport                                      │
   │     trait Transport, PacketSink, PacketSource            │
   └─────────────────────────────────────────────────────────┘
                                │ uses
                                ▼
   ┌─────────────────────────────────────────────────────────┐
   │  Layer 2: Protocol (pure logic, in crates/protocol/)     │
   │     packet::{encode, decode}                             │
   │     trait Crc16Engine, Crc32Engine                       │
   │     trait BlockSource                                    │
   │     trait CommandHandler                                 │
   └─────────────────────────────────────────────────────────┘
                                │ uses
                                ▼
   ┌─────────────────────────────────────────────────────────┐
   │  Layer 1: Phy                                            │
   │     trait LptPhy, Ps2Phy                                 │
   └─────────────────────────────────────────────────────────┘
                                │ uses
                                ▼
   ┌─────────────────────────────────────────────────────────┐
   │  Layer 0: HAL (embassy-rp, pac, pio)                     │
   └─────────────────────────────────────────────────────────┘
```

Telemetry plugs in at every layer as an observer, not a consumer in the data path.

---

## 3. Layer 1 — Phy traits

### 3.1 `LptPhy`

Already prototyped in `firmware/src/lpt/mod.rs`; refine.

```rust
pub trait LptPhy {
    /// Wait for one inbound byte on the LPT bus. Cancellable.
    async fn recv_byte(&mut self) -> Result<u8, LptError>;

    /// Send one outbound byte. Cancellable.
    async fn send_byte(&mut self, b: u8) -> Result<(), LptError>;

    /// Currently active IEEE 1284 mode.
    fn current_mode(&self) -> LptMode;

    /// Attempt to switch mode. Returns the mode that's actually active after
    /// the call (the request may be denied if the host hasn't negotiated it).
    async fn set_mode(&mut self, target: LptMode) -> LptMode;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum LptMode { Spp, SppNibble, Byte, Epp, Ecp, EcpDma }

#[derive(Debug, Clone, Copy, defmt::Format)]
pub enum LptError { Timeout, ModeMismatch, Hardware }
```

**Concrete impls planned:**

| Impl | Lives in | Phase | Modes supported |
|---|---|---|---|
| `SppNibblePhyBitBang` | `firmware/src/lpt/compat.rs` | 3 (built) | SppNibble only |
| `SppNibblePhyPio` | `firmware/src/lpt/compat_pio.rs` | 4 | SppNibble |
| `BytePhyPio` | `firmware/src/lpt/byte.rs` | 4 | Byte |
| `EppPhyPio` | `firmware/src/lpt/epp.rs` | 5 | Epp |
| `EcpPhyDma` | `firmware/src/lpt/ecp.rs` | 5 | Ecp, EcpDma |
| `LptPhyMux` | `firmware/src/lpt/mod.rs` | 4 | All — dispatches on `current_mode` to the right concrete impl |

The `LptPhyMux` is the long-term face the rest of the firmware sees. Phase 3 just uses `SppNibblePhyBitBang` directly.

### 3.2 `Ps2Phy`

The two-pipeline architecture (oversampler + demodulator) means `Ps2Phy` isn't one trait — it's two, plus a TX trait.

```rust
/// Production byte stream: clean frames from the PIO demodulator.
pub trait Ps2Receiver {
    async fn recv_frame(&mut self) -> Ps2Frame;
    fn machine_class(&self) -> Option<MachineClass>;
}

/// Instrumentation stream: raw oversampled timing data for the classifier
/// and signature extractor.
pub trait Ps2Sampler {
    /// Returns a borrowed view of the most recent samples; the underlying
    /// DMA ring is shared, so the caller must not retain the slice past the
    /// next call.
    fn snapshot(&self) -> SamplesView<'_>;

    fn stats(&self, window: TimeWindow) -> Ps2Stats;
    fn histogram(&self, metric: HistogramMetric) -> Histogram;
}

/// Outbound frames (keyboard emulation).
pub trait Ps2Transmitter {
    async fn send_frame(&mut self, frame: Ps2Frame) -> Result<(), Ps2Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum MachineClass { Xt, At, Ps2 }

#[derive(Debug, Clone, Copy, defmt::Format)]
pub struct Ps2Frame {
    pub data: u8,
    pub parity_ok: bool,
    pub framing_ok: bool,
    pub start_timestamp_us: u64,
    pub timing: FrameTiming,
}
```

**Concrete impls:**

| Impl | Implements | Notes |
|---|---|---|
| `KbdDemodulator` | `Ps2Receiver` | PIO1 SM1 fed; CPU adds parity check. |
| `AuxDemodulator` | `Ps2Receiver` | PIO1 SM3 fed; same shape. |
| `KbdOversampler` | `Ps2Sampler` | PIO1 SM0; DMA ring; instrumentation only. |
| `AuxOversampler` | `Ps2Sampler` | PIO1 SM2. |
| `KbdTransmitter` | `Ps2Transmitter` | PIO2 SM0; CPU pre-packs 11-bit frames. |
| `AuxTransmitter` | `Ps2Transmitter` | PIO2 SM1. |
| `Classifier` | (consumes `Ps2Sampler`) | Always-running task; emits `MachineClassDetected` on convergence. |

The split lets the demodulator be cheap and lean (just bytes) while the oversampler does the instrumentation/fingerprinting work. They can be enabled or disabled independently.

---

## 4. Layer 2 — Protocol

Lives in `crates/protocol/`. Pure logic; no async; no I/O.

### 4.1 Packet codec

Already implemented as functions, not traits. Reasonable: it's pure data transformation. Stays as:

```rust
// crates/protocol/src/packet/mod.rs
pub fn encode(cmd: u8, seq: u8, payload: &[u8], out: &mut [u8]) -> Result<usize, EncodeError>;
pub fn decode(buf: &[u8]) -> Result<IncomingPacket, DecodeError>;
```

No trait needed — there's only one encoding, and it's the wire format both sides commit to.

### 4.2 CRC engines (trait)

Two CRCs in our wire protocol; both have hardware-accelerated variants on RP2350.

```rust
// crates/protocol/src/crc.rs

pub trait Crc16Engine {
    fn reset(&mut self);
    fn update(&mut self, data: &[u8]);
    fn finalize(&self) -> u16;
}

pub trait Crc32Engine {
    fn reset(&mut self);
    fn update(&mut self, data: &[u8]);
    fn finalize(&self) -> u32;
}
```

**Concrete impls:**

| Impl | Lives in | Use case |
|---|---|---|
| `SoftwareCrc16Ccitt` | `protocol::crc::software` | Always available; small packets where the trip through the DMA sniffer would cost more code than it saves. |
| `SoftwareCrc32Reflected` | `protocol::crc::software` | Reference impl; testable on host. |
| `SnifferCrc16Ccitt` | `firmware::crc_sniffer` | Binds the DMA sniffer to a specific channel. **Not portable** — RP2350-only. |
| `SnifferCrc32Reflected` | `firmware::crc_sniffer` | The big win for Stage 2 image CRC. |

The trait gives us:

- Host-side unit tests use the software impl exclusively.
- Stage 2 download wires up the sniffer impl on the firmware; the same code path otherwise.
- Future targets without a CRC accelerator just plug in the software impl.

### 4.3 `BlockSource` trait

The block server reads from "something that has the Stage 2 image". On the device, that's the embedded blob. In tests, it can be a `Vec<u8>` or a slice.

```rust
// crates/protocol/src/block_server.rs

pub trait BlockSource {
    fn total_size(&self) -> usize;
    fn crc32(&self) -> u32;

    /// Return the bytes for `block_no` and the actual byte count (last block
    /// may be short). `None` if `block_no` is past end-of-image.
    fn block(&self, block_no: u16, block_size: usize) -> Option<(&[u8], u8)>;
}
```

**Concrete impls:**

| Impl | Lives in | Use case |
|---|---|---|
| `EmbeddedBlob` | `firmware::protocol::stage_blobs` | `include_bytes!`-backed blob with CRC computed at boot. |
| `SliceBlob` | `protocol::block_server::slice` | `&[u8]` + CRC field; used by host tests and the eventual TUI replay tool. |
| `FlashBlob` | `firmware::flash` | Phase 7+: blob stored in a flash partition rather than `.rodata`. |

### 4.4 `CommandHandler` trait

For the dispatcher. Each handler is a small async function consuming a packet and producing a response.

```rust
// crates/protocol/src/dispatch.rs

pub trait CommandHandler<S> {
    /// Process one incoming packet. State is supplied by the dispatcher; the
    /// handler is stateless across calls.
    fn handle(&self, p: &IncomingPacket, state: &mut S) -> DispatchOutcome;
}

pub enum DispatchOutcome {
    Reply(heapless::Vec<u8, MAX_PACKET>),
    Silent,
    Ignored,
}
```

**Concrete impls:** small zero-sized structs, one per command, registered in a const table:

```rust
const HANDLERS: &[(u8, &dyn CommandHandler<SessionState>)] = &[
    (CMD_CAP_REQ,    &CapReqHandler),
    (CMD_CAP_ACK,    &CapAckHandler),
    (CMD_PING,       &PingHandler),
    (CMD_SEND_BLOCK, &SendBlockHandler),
    (CMD_BLOCK_ACK,  &BlockAckHandler),
    (CMD_BLOCK_NAK,  &BlockNakHandler),
];
```

Trait-object dispatch isn't free (vtable indirection), but the cost is dwarfed by the wire-level cycle time. Easier than a giant `match` and adds well: new commands are just new struct types.

---

## 5. Layer 3 — Transport

A `Transport` wraps a `LptPhy` (or PS/2 phy) with packet-level semantics.

```rust
// firmware/src/transport/mod.rs

pub trait Transport {
    async fn send_packet(&mut self, p: &OutgoingPacket) -> Result<(), TransportError>;
    async fn recv_packet(&mut self) -> Result<IncomingPacket, TransportError>;
    fn plane(&self) -> Plane;
    fn port(&self) -> Port;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum Plane { Control, Data }

#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum Port { Ps2Kbd, Ps2Aux, Lpt }
```

**Concrete impls:**

| Impl | Plane×Port | Phase |
|---|---|---|
| `LptTransport` | Data×Lpt | 3 (built); wraps any `LptPhy` |
| `Ps2ControlTransport` | Control×Ps2Kbd | 2 (i8042 private channel byte pump) |
| `Ps2DataTransport` | Data×Ps2Aux | 11 (PS/2 dual-lane fallback) |
| `MultiplexedTransport` | Control+Data on one phy | XT mode, PS/2 fallback degraded mode |

### 5.1 PacketSink / PacketSource split

For testability and decoupling, expose receiver and sender separately:

```rust
pub trait PacketSink {
    async fn send(&mut self, p: &OutgoingPacket) -> Result<(), TransportError>;
}

pub trait PacketSource {
    async fn recv(&mut self) -> Result<IncomingPacket, TransportError>;
}

// Any Transport is both:
impl<T: Transport> PacketSink for T { ... }
impl<T: Transport> PacketSource for T { ... }
```

The command dispatcher takes `&mut dyn PacketSink` for its reply path. Tests can plug in a `MockSink: PacketSink` that captures replies into a Vec.

---

## 6. Layer 4 — Session

```rust
// firmware/src/session/mod.rs

pub trait SessionSupervisor {
    fn on_event(&mut self, event: SessionEvent);
    fn current_state(&self) -> SessionState;
    fn plane_bindings(&self) -> PlaneBindings;
}

pub struct PlaneBindings {
    pub control: Option<Port>,
    pub data: Option<Port>,
}

pub enum SessionState { Boot, AwaitHostPower, DetectMachineClass, /* ... */ DpActive, Error(ErrorKind) }
pub enum SessionEvent { HostPowerDetected, MachineClassDetected(MachineClass), /* ... */ }
```

**Concrete impl:** `LifecycleSupervisor` — owns the state machine, drives transitions, dispatches `SessionEvent`s that other tasks emit. One global instance, held inside the `session_task`.

This is where plane binding decisions live. When `Phase 3 download complete` and `Stage 2 exec ack` events fire, the supervisor binds CONTROL to PS/2 KBD and DATA to LPT (in normal flow) or to PS/2 AUX (in fallback). The supervisor emits the `plane` telemetry event (see [`instrumentation_surface.md` §5.2](instrumentation_surface.md)) on every binding change.

---

## 7. Layer 5 — Telemetry

```rust
// crates/telemetry-schema/src/lib.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Event {
    Boot { v: u8, t: f64, fw_version: String, phase: u8 },
    Stats { v: u8, t: f64, ch: Channel, /* ... */ },
    Plane { v: u8, t: f64, plane: Plane, state: PlaneState, /* ... */ },
    Frame { v: u8, t: f64, ch: Channel, data: u8, /* ... */ },
    Anomaly { v: u8, t: f64, ch: Channel, /* ... */ },
    Fingerprint { v: u8, t: f64, ch: Channel, class: MachineClass, /* ... */ },
    // ...
}
```

Each event is a struct variant of the `Event` enum; serde does the JSON encoding. Same types deserialize in the host TUI.

```rust
// firmware/src/telemetry/emit.rs

pub trait TelemetryEmit {
    fn emit(&self, event: Event);
}
```

**Concrete impls:**

| Impl | Lives in | Behavior |
|---|---|---|
| `DefmtEmit` | `firmware::telemetry::defmt_emit` | Formats per [`instrumentation_surface.md` §3](instrumentation_surface.md) and writes via `defmt::info!`. |
| `CdcEmit` | `firmware::telemetry::cdc_emit` | Serializes JSON and writes to CDC 1 (with backpressure handling per §5.5). |
| `MultiEmit<A, B>` | `firmware::telemetry::multi` | Forwards to both. The default device emitter. |
| `NoopEmit` | `firmware::telemetry::noop` | Used in tests. |
| `CapturingEmit` | (test-only) | Captures events into a Vec for assertions. |

The `Multi` impl is generic over its children:

```rust
pub struct MultiEmit<A: TelemetryEmit, B: TelemetryEmit>(pub A, pub B);

impl<A: TelemetryEmit, B: TelemetryEmit> TelemetryEmit for MultiEmit<A, B> {
    fn emit(&self, event: Event) {
        self.0.emit(event.clone());
        self.1.emit(event);
    }
}
```

Single static instance held by the `telemetry_task`; every module gets it via `&dyn TelemetryEmit` injected at construction time.

### 7.1 Observation hooks

Each layer takes a `&dyn TelemetryEmit` and calls `.emit()` at specific points:

| Layer | Hook |
|---|---|
| `LptPhy` impls | `LptModeChanged`, on first byte after idle, on errors |
| `Ps2Receiver` impls | `Frame`, on parity error, on framing error |
| `Ps2Sampler` impls | `Stats` (1/s), `Anomaly` on glitches, `Fingerprint` periodic |
| `Transport` impls | `Packet{Rx,Tx}`, `CrcError`, `Timeout` |
| `SessionSupervisor` impls | `StateChange`, `Plane` |

Telemetry calls are **fire-and-forget** — never block, never return errors visible to the caller. Backpressure is handled inside the `CdcEmit` impl (drop-oldest policy).

---

## 8. Cross-cutting: error model

A single error enum per layer keeps signatures clean.

```rust
// Layer 1 - phy
pub enum LptError { Timeout, ModeMismatch, Hardware }
pub enum Ps2Error { Parity, Framing, BusContention }

// Layer 3 - transport
pub enum TransportError {
    Phy(LptError),                // or Ps2Error variant
    Decode(DecodeError),
    Encode(EncodeError),
}

// Layer 4 - session
pub enum SessionError {
    Transport(TransportError),
    SupervisorTimeout,
    Bug(&'static str),
}
```

Layers wrap; no `From` magic at first (keep conversions explicit so the wrap site is searchable). Adopt `From` per-pair only if a pattern recurs three or more times.

---

## 9. Test strategy

### 9.1 Host-side unit tests (per shared crate)

Each crate in `crates/` runs `cargo test -p <crate>` on the host triple. Pure-logic coverage:

- `protocol::packet::{encode, decode}` — round-trip and adversarial corruption tests.
- `protocol::crc16` / `crc32` — known-vector tests.
- `protocol::cap::build_cap_rsp_payload` — offset / size cross-check.
- `protocol::block_server` — block sizing, offset bounds, last-block-short.
- `signatures::delta` — δ-distance monotonicity, hash stability.
- `telemetry_schema` — serialize → deserialize round-trip on every Event variant.

Configured in workspace `Cargo.toml`:

```toml
[alias]
ci          = "test --workspace --exclude vintage-kvm-firmware"
fw-check    = "check -p vintage-kvm-firmware --target thumbv8m.main-none-eabihf"
test-all    = "test --workspace --exclude vintage-kvm-firmware && cargo fw-check"
```

`ci` now does real host-side testing; `fw-check` validates the embedded build. Both run in CI.

### 9.2 Firmware integration tests

Embedded testing is hard; `embedded-test` or `defmt-test` crates run on real hardware. Phase 3+: defer until a bench rig exists. Until then, the host-side tests on the shared crates plus the firmware's `cargo fw-check` are the only automated checks.

### 9.3 Mock impls for trait-driven tests

Each trait has a corresponding mock under `#[cfg(test)]`:

```rust
pub struct MockLptPhy {
    pub recv_queue: heapless::Vec<u8, 256>,
    pub sent: heapless::Vec<u8, 256>,
}
impl LptPhy for MockLptPhy { ... }
```

The session task can be driven through a full protocol flow against `MockLptPhy + MockTelemetryEmit` without any hardware. Asserts on the captured `sent` buffer and the captured telemetry events validate behavior end-to-end.

---

## 10. Concrete impl summary

| Trait | Concrete impl(s) | Crate | Phase |
|---|---|---|---|
| `Crc16Engine` | `SoftwareCrc16Ccitt`, `SnifferCrc16Ccitt` | protocol, firmware | 3, 5 |
| `Crc32Engine` | `SoftwareCrc32Reflected`, `SnifferCrc32Reflected` | protocol, firmware | 3, 5 |
| `BlockSource` | `EmbeddedBlob`, `SliceBlob`, `FlashBlob` | firmware, protocol, firmware | 3, test, 7+ |
| `CommandHandler<S>` | one zero-sized struct per command | protocol | 3 |
| `LptPhy` | `SppNibblePhyBitBang`, `SppNibblePhyPio`, `BytePhyPio`, `EppPhyPio`, `EcpPhyDma`, `LptPhyMux` | firmware | 3, 4, 4, 5, 5, 4 |
| `Ps2Receiver` | `KbdDemodulator`, `AuxDemodulator` | firmware | 1, 2 |
| `Ps2Sampler` | `KbdOversampler`, `AuxOversampler` | firmware | 1, 2 |
| `Ps2Transmitter` | `KbdTransmitter`, `AuxTransmitter` | firmware | 1, 2 |
| `Transport` | `LptTransport`, `Ps2ControlTransport`, `Ps2DataTransport`, `MultiplexedTransport` | firmware | 3, 2, 11, XT |
| `PacketSink` / `PacketSource` | (blanket impl for `Transport`) | firmware | 3 |
| `SessionSupervisor` | `LifecycleSupervisor` | firmware | 3+ |
| `TelemetryEmit` | `DefmtEmit`, `CdcEmit`, `MultiEmit`, `NoopEmit`, `CapturingEmit` | firmware (tests in protocol) | 1+ |

---

## 11. What stays a function (no trait)

Not everything needs a trait. Trait overhead pays back only when there's more than one impl, or when testability needs a mock. Functions stay functions for:

- `packet::encode` / `packet::decode` — single canonical encoding.
- `crc16::compute` / `crc32::compute` — convenience wrappers around the `*Engine` traits; pure functions for one-shot use.
- `cap::build_cap_rsp_payload` — single byte layout.
- `fingerprint::hash` — single hash algorithm.
- `signatures::delta::compute` — single distance metric.
- Format-string helpers, byte-packing helpers, etc.

If a second impl ever appears, promote to a trait at that point.

---

## 12. Open decisions

1. **Move `firmware/` into `crates/`?** Cosmetic but cleaner. Defer for v1; revisit when the workspace gets >3 members.
2. **`async fn` in traits vs explicit `Future` associated types?** Rust 1.75+ supports `async fn` in traits with limitations (no auto-trait inference). Lean toward `async fn` for the simplicity; switch to `Future` types if generic-over-multiple-impls hits friction.
3. **Should `Crc{16,32}Engine` impls be `Send`?** Probably yes — embassy may schedule across cores in Phase 5+ multi-core split. Tag explicitly.
4. **One workspace `Cargo.lock` vs per-crate?** Workspace-wide lockfile is the Rust default; keep it. Locks all dependencies pin-tight across the project.
5. **Re-export protocol types from firmware vs `use vintage_kvm_protocol::*;` everywhere?** Re-export `pub use vintage_kvm_protocol::packet::*;` from `firmware/src/packet/mod.rs` so existing call sites don't churn during migration. Drop the re-export later if it causes confusion.

---

## 13. Phasing

| Phase | Crate work | Trait work |
|---|---|---|
| Now | Create empty `crates/{protocol,telemetry-schema,signatures}` | Extract `LptPhy` trait; promote `SppNibblePhy` to impl it |
| Phase 3.5 (now) | Migrate `packet/` → `crates/protocol/` | Add `Crc16Engine` + `Crc32Engine` traits |
| Phase 4 | — | Add `LptPhyMux`; introduce `Transport` trait |
| Phase 5 | Add `Sniffer{Crc16,Crc32}` impls | — |
| Phase 1 | Populate `telemetry-schema` from PS/2 work | Add `Ps2Receiver`, `Ps2Sampler`, `Ps2Transmitter` |
| Phase 6 | `tools/tui/` consumes `telemetry-schema`+`signatures` | `SessionSupervisor` solidified for steady-state DP_ACTIVE |

---

## 14. Related documents

- [`pico_firmware_design.md`](pico_firmware_design.md) — module-level architecture (this doc specifies the trait contracts between those modules)
- [`pio_state_machines_design.md`](pio_state_machines_design.md) — PIO programs each phy impl will use
- [`instrumentation_surface.md`](instrumentation_surface.md) — telemetry event schema (`crates/telemetry-schema/`)
- [`stage1_implementation.md`](stage1_implementation.md) — DOS side that the firmware peers with
- [`implementation_plan.md`](implementation_plan.md) §1 firmware/ — overall status
