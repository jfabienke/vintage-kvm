# vintage-kvm Implementation Plan

**Status:** Living planning document  
**Last updated:** 2026-05-13  
**Companion documents:**

- Architecture: [`design.md`](design.md), [`two_plane_transport.md`](two_plane_transport.md)
- Hardware: [`hardware_reference.md`](hardware_reference.md), [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md), [`ps2_eras_reference.md`](ps2_eras_reference.md), [`ps2_private_channel_design.md`](ps2_private_channel_design.md)
- DOS side: [`stage0_design.md`](stage0_design.md), [`stage1_design.md`](stage1_design.md), [`stage1_implementation.md`](stage1_implementation.md)
- Pico side: [`pico_firmware_design.md`](pico_firmware_design.md), [`pico_phase3_design.md`](pico_phase3_design.md), [`pio_state_machines_design.md`](pio_state_machines_design.md), [`instrumentation_surface.md`](instrumentation_surface.md)

This document is the per-subrepo execution plan that complements the architecture in [`design.md`](design.md). For each subrepo (existing or planned), it specifies directory layout, build pipeline, module responsibilities, phase mapping, and open decisions.

## Contents

- [Overview](#overview) — repo layout, subrepo status table, dependency graph
- [Cross-cutting concerns](#cross-cutting-concerns) — workspace, toolchains, host-language decision, phase sequencing
- [1. firmware/](#1-firmware) — RP2350 Pico firmware (Rust + embassy-rp)
- [2. dos/stage0/](#2-dosstage0) — DOS Stage 0 bootstrap (XT/AT/PS/2 variants)
- [3. dos/stage1/](#3-dosstage1) — DOS Stage 1 loader blob
- [4. dos/pico1284/](#4-dospico1284) — DOS Stage 2 TSR / CLI
- [5. dos/common/](#5-doscommon) — shared NASM `.inc` + Watcom C headers
- [6. host/](#6-host) — modern-host USB CDC client
- [7. tools/](#7-tools) — dev fixtures and protocol tooling
- [8. hardware/](#8-hardware) — KiCad schematics + PCB
- [Sequencing & critical path](#sequencing--critical-path)
- [Open decisions summary](#open-decisions-summary)

---

## Overview

### Top-level repo layout (target state)

```
vintage-kvm/
├── .cargo/config.toml             workspace cargo aliases
├── Cargo.toml                     workspace root
├── Cargo.lock
├── docs/                          design + planning docs (4 files today + this one)
├── firmware/                      RP2350 Pico firmware crate
├── dos/
│   ├── Makefile                   top-level NASM + Watcom build
│   ├── stage0/                    s0_xt.asm + s0_at.asm + s0_ps2.asm
│   ├── stage1/                    stage1.asm (raw blob served by Pico over LPT)
│   ├── pico1284/                  TSR/CLI: NASM modules + Watcom C modules
│   └── common/                    shared .inc and .h
├── host/                          modern-host USB CDC client (Rust workspace member)
├── tools/                         dev fixtures and protocol tooling
└── hardware/                      KiCad schematic + PCB
```

### Status table

| # | Path | Purpose | Status | Next step |
|---|---|---|---|---|
| 1 | `firmware/` | RP2350 Pico firmware | **Phase 3 MVP (~13 KB)** — bit-bang LPT SPP-nibble + CRC-16 packet protocol + CAP/PING/block-server dispatch. PIO designs ready: [`pio_state_machines_design.md`](pio_state_machines_design.md), instrumentation surface: [`instrumentation_surface.md`](instrumentation_surface.md). | Replace bit-bang with PIO programs; add PS/2 oversampler+demod for Phase 1 |
| 2 | `dos/stage0/` | DOS Stage 0 bootstrap | **3 of 3 variants build** | Hardware-validate AT/PS2 private channels |
| 3 | `dos/stage1/` | DOS Stage 1 loader | **v1.0 scaffold (4821 B)** — builds `PICO_BOOT` env block, shrinks via `AH=4Ah`, EXECs `PICO1284.EXE` via `AH=4Bh`; child errorlevel propagates | Per [`stage1_design.md`](stage1_design.md): auto-downgrade ladder once EPP/ECP byte pumps land |
| 4 | `dos/pico1284/` | DOS Stage 2 TSR/CLI | **Stub** | Install Open Watcom V2; set up wmake build alongside NASM |
| 5 | `dos/common/` | Shared NASM/C headers | **Not created (YAGNI)** | Create on first cross-stage constant |
| 6 | `host/` | Modern-host USB CDC client | **Not created, language undecided (Rust recommended)** | Decide language; scaffold workspace member |
| 7 | `tools/` | Dev fixtures + tooling | **Not created** | First tool: `tools/tui/` — ratatui dashboard consuming the Pico's CDC telemetry stream ([`instrumentation_surface.md` §4](instrumentation_surface.md)); ships in Phase 6 |
| 8 | `hardware/` | KiCad schematic + PCB | **Not created** | Initial schematic-only project mirroring `hardware_reference.md` §3.3 |

### Dependency graph (what blocks what)

```
                      ┌─────────────────────────────┐
                      │       firmware/             │
                      │  Phase 3 MVP built; Phase   │
                      │  1/2/4/5 designed, awaiting │
                      │  Feather boards for bench   │
                      └──────┬──────────────────────┘
                             │ tests against
                             ▼
              ┌──────────────────────────────────────┐
              │       dos/stage0/                    │
              │  s0_xt/s0_at/s0_ps2 build; AT/PS2    │
              │  need firmware PS/2 endpoint tests   │
              └──────┬───────────────────────────────┘
                     │ extends
                     ▼
              ┌──────────────────────────────────────┐
              │       dos/stage1/                    │
              │  needs firmware LPT data plane       │
              │  (Phase 3+) to receive               │
              └──────┬───────────────────────────────┘
                     │ delivers
                     ▼
              ┌──────────────────────────────────────┐
              │       dos/pico1284/                  │
              │  needs Watcom; modules per §21.2     │
              └──────┬───────────────────────────────┘
                     │ communicates with
                     ▼
              ┌──────────────────────────────────────┐
              │       host/                          │
              │  language not yet chosen             │
              │  (every phase ≥ 6 needs this)        │
              └──────────────────────────────────────┘

hardware/ ─ pure-additive, depends on nothing, blocks nothing critical
tools/    ─ pure-additive
dos/common/ ─ created on demand; not blocking
```

---

## Cross-cutting concerns

### Workspace structure

Current `Cargo.toml` (root) is a workspace with one member: `firmware`. Plan to add `host` as a second member, keeping `firmware/` and `host/` in one `cargo` workspace so a single `cargo` invocation handles both Rust sides.

`dos/` is **not** a Cargo workspace member — DOS-side code is NASM + Open Watcom C, built via `make`. The DOS pieces produce flat binaries (`.COM`, `.bin`) into `dos/build/`.

Final root `Cargo.toml` (target state):

```toml
[workspace]
resolver = "3"
members = ["firmware", "host"]

[workspace.package]
edition = "2024"
license = "MIT OR Apache-2.0"
# ...profiles unchanged
```

### Toolchain matrix

| Subrepo | Toolchain | Install state |
|---|---|---|
| `firmware/` | Rust stable (1.95+), `thumbv8m.main-none-eabihf` target, `probe-rs`, `elf2uf2-rs` or `picotool` | **All installed** |
| `dos/stage0/`, `dos/stage1/` | NASM 3.x | **Installed (3.01)** |
| `dos/pico1284/` | NASM + Open Watcom V2 (`wcc`, `wcl`, `wlink`, `wmake`) | **Watcom not installed** |
| `host/` | TBD (depends on language decision) | — |
| `tools/` | libusb (Mac/Linux: brew/apt) for any PL2305/USS-720 fixtures | brew has `libusb` |
| `hardware/` | KiCad 9 (current) | TBD |

### Language decision for `host/`

Recommendation: **Rust**, for four reasons:

1. **Already in the workspace.** No new toolchain; `cargo build` from the root builds both sides.
2. **`serialport` + `tokio-serial` are mature.** USB CDC ACM appears as a regular serial port; the Rust ecosystem handles it natively on macOS, Linux, and Windows without OS-specific code.
3. **Shared protocol types between firmware and host.** With both in Rust, the `packet/` module can be a `no_std` crate consumed by both `firmware` and `host` — no duplicate definitions of command IDs, packet layout, CRC functions, etc.
4. **Single binary, no runtime.** Distributing to other vintage-machine owners is just a static executable.

Alternatives considered: Python (fastest to prototype but requires runtime + dep wrangling); Go (decent serial libs, but no easy shared-types story with firmware); C# (Windows-leaning); C/C++ (more friction than benefit).

**Decision point:** confirm Rust, then `host/` and a new `packet/` workspace member can be scaffolded.

### Phase sequencing (mapped to subrepos)

| Phase (`design.md` §22) | Primary subrepo | Secondary | Status |
|---|---|---|---|
| 0: Hardware bring-up | `firmware/` | — | ✅ done (GP7 blink) |
| 1: PS/2 keyboard + DEBUG bootstrap | `firmware/ps2/` | `dos/stage0/s0_at.asm` *(eventually)* | Unblocked once Feather boards land |
| 2: Stage 0 i8042 control | `dos/stage0/` | `firmware/ps2/` private mode | After Phase 1 |
| 3: IEEE 1284 Compatibility mode | `firmware/ieee1284/` | `dos/stage1/` | After Phase 1; can dev-fixture with DeLock |
| 4: Capability handshake | `firmware/session/`, `firmware/packet/` | `dos/stage1/`, `host/` | After Phase 3 transport |
| 5: EPP/ECP modes | `firmware/ieee1284/` | `dos/stage1/` | After Phase 3; needs real DOS PC or USS-720. v1 ships `EPP_PIO` + `SPP_PIO` rescue; v2 adds `ECP_PIO`; v3 adds opt-in `ECP_DMA` (see [`two_plane_transport.md` §Data-plane modes and optional ECP DMA](two_plane_transport.md#data-plane-modes-and-optional-ecp-dma)). |
| 6: Raw file transfer | `dos/pico1284/`, `host/` | `firmware/packet/` | **Needs `host/` decision** |
| 7–9: Screen dump (text → RLE → VESA) | `dos/pico1284/`, `firmware/compression/`, `host/` | — | Needs Watcom + `host/` |
| 10: Full TSR/CLI | `dos/pico1284/` | — | Needs Watcom |
| 11: PS/2 dual-lane fallback | `firmware/ps2/`, `dos/stage0/s0_ps2.asm` | — | After Phases 1+2 |

---

## 1. `firmware/`

### Purpose

Single binary that runs on the Adafruit Feather RP2350 HSTX + 8 MB PSRAM, bridging the DOS PC's PS/2 keyboard/mouse ports and LPT to a modern host over USB CDC. Implements all the protocol layers in `design.md` §4 and §20.1.

**Comprehensive design:** [`pico_firmware_design.md`](pico_firmware_design.md) (task model, PIO/SM allocation, memory map, per-module designs, lifecycle, error recovery, build system).

**PIO programs:** [`pio_state_machines_design.md`](pio_state_machines_design.md) (every PS/2 and LPT mode with PIO assembly sketches, CPU pre/post-processing, DMA configurations).

**Phase 3+ MVP slice (currently built):** [`pico_phase3_design.md`](pico_phase3_design.md).

**Operator-facing instrumentation surface:** [`instrumentation_surface.md`](instrumentation_surface.md) (console formats, TUI dashboard, CDC telemetry JSON protocol, signature DB).

### Directory layout (target state)

```
firmware/
├── .cargo/config.toml             target = thumbv8m.main-none-eabihf, probe-rs runner
├── Cargo.toml
├── build.rs                       copies memory.x to OUT_DIR
├── memory.x                       RP2350 + 8 MB PSRAM regions
├── src/
│   ├── main.rs                    Embassy `#[main]`, task spawn, init
│   ├── lifecycle.rs               SessionState machine (BOOT → … → DP_ACTIVE)
│   ├── lpt/
│   │   ├── mod.rs                 trait LptPhy { send_byte, recv_byte, current_mode }
│   │   ├── compat.rs              SPP-nibble bit-bang (Phase 3 MVP; built)
│   │   ├── compat_pio.rs          SPP-nibble PIO + sniffer-DMA (Phase 4; drop-in replacement)
│   │   ├── byte.rs                Byte-mode PIO (Phase 4)
│   │   ├── epp.rs                 EPP fwd/rev PIO (Phase 5)
│   │   ├── ecp.rs                 ECP fwd/rev DMA + sniffer (Phase 5)
│   │   ├── negotiation.rs         IEEE 1284 negotiation peripheral (Phase 4)
│   │   └── pio/                   .pio source files compiled via pio-proc!
│   ├── ps2/
│   │   ├── mod.rs                 trait Ps2Phy + MachineClass enum
│   │   ├── oversampler.rs         PIO oversampler (1 MS/s; instrumentation stream)
│   │   ├── demodulator.rs         PIO frame demodulator (production byte stream)
│   │   ├── framer.rs              bit-stream → Ps2Frame extraction (CPU)
│   │   ├── tx.rs                  outbound frame transmitter (PIO + CPU pre-pack)
│   │   ├── classifier.rs          XT/AT/PS2 auto-detection from frame stats
│   │   ├── instrumentation.rs     timing histograms, glitch counts, edge skew
│   │   ├── parity.rs              odd-parity validate (CPU, 3 instructions)
│   │   └── pio/                   .pio source for oversample, demod, tx
│   ├── i8042/
│   │   ├── mod.rs                 private-channel client; LED-unlock + AUX knock watcher
│   │   ├── kbd_private.rs         post-unlock generic byte pump on KBD wire
│   │   └── aux_private.rs         post-unlock generic byte pump on AUX wire
│   ├── packet/
│   │   ├── mod.rs                 encode/decode SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX (built)
│   │   ├── commands.rs            CMD_* IDs matching stage1.asm:106 (built)
│   │   ├── crc16.rs               CRC-16-CCITT (built; CPU; small packets)
│   │   └── crc32.rs               CRC-32/IEEE reflected (built; CPU baseline)
│   ├── packet_stream.rs           byte-stream → packet reassembler with resync (built)
│   ├── protocol/
│   │   ├── mod.rs                 dispatcher: CAP/PING/SEND_BLOCK/ACK/NAK (built)
│   │   ├── cap.rs                 CAP_RSP payload builder (built)
│   │   ├── ping.rs                PING → PONG echo
│   │   ├── block_server.rs        block download server (built)
│   │   └── stage_blobs.rs         embedded Stage 0/1/2 blobs (Stage 2 placeholder built)
│   ├── debug_inject/
│   │   ├── mod.rs                 DEBUG script generator + driver (Phase 1)
│   │   ├── script_xt.rs           XT-class injection script
│   │   ├── script_at.rs           AT-class
│   │   └── script_ps2.rs          PS/2-class
│   ├── transport/
│   │   ├── mod.rs                 plane abstraction (control + data binding)
│   │   ├── single_plane.rs        Phase 3+: LPT only OR PS/2 only
│   │   └── dual_plane.rs          Phase 11+: LPT data + PS/2 control concurrently
│   ├── crc_sniffer.rs             DMA-sniffer wrapper (CRC-32 over Stage 2 image; Phase 5)
│   ├── usb_cdc/
│   │   ├── mod.rs                 USB CDC dual interface
│   │   ├── packet_channel.rs      CDC 0: framed packet I/O (modern host bridge)
│   │   └── telemetry_channel.rs   CDC 1: JSON-line events (per instrumentation_surface.md §5)
│   ├── status/
│   │   ├── mod.rs                 NeoPixel + red LED driver
│   │   ├── neopixel.rs            PIO WS2812 driver
│   │   └── animations.rs          per-SessionState color/pattern table
│   ├── telemetry/
│   │   ├── mod.rs                 event aggregation
│   │   ├── ring.rs                lock-free SPSC event ring
│   │   ├── signature_db.rs        embedded keyboard/chipset signature DB
│   │   └── fingerprint.rs         Δ-distance match + stable hash
│   └── util/
│       ├── pin.rs                 GPIO direction helpers
│       └── time.rs                low-jitter delay helpers
```

### Build pipeline

Already scaffolded. `cargo fw-release` from repo root, or `cd firmware && cargo build --release`. Output: `target/thumbv8m.main-none-eabihf/release/vintage-kvm-firmware` (ELF). Flash via `cargo fw-run` (probe-rs), or switch runner in `firmware/.cargo/config.toml` to `elf2uf2-rs -d` for BOOTSEL mode.

### Canonical from-root verification

`cargo test --workspace` currently fails because the only workspace member is the cortex-m-only firmware crate (no host-buildable tests exist yet). Use the aliases in `.cargo/config.toml` instead:

| Command | What it runs | Purpose |
|---|---|---|
| `cargo ci` | `cargo fw-check` | Canonical CI / pre-push check |
| `cargo test-all` | `cargo fw-check` | Placeholder until host-side test crates exist |
| `cargo fw-check` | `check -p vintage-kvm-firmware --target thumbv8m.main-none-eabihf` | Firmware-only type/borrow check |
| `cargo fw-release` | release build | Produces flashable ELF |
| `cargo fmt --check` | rustfmt | Style |
| `make -C dos all sizes` | NASM | DOS binaries + size budgets |

When host-targetable crates land (`host/`, `tools/`), expand `ci` and `test-all` to include them.

### Dependencies (current + planned)

| Dep | Version | Why |
|---|---|---|
| `embassy-rp` | 0.10 (features: `rp235xa`, `defmt`, `unstable-pac`, `time-driver`, `critical-section-impl`, `binary-info`) | ✅ in Cargo.toml |
| `embassy-executor` | 0.10 | ✅ |
| `embassy-time` | 0.5.1 | ✅ |
| `embassy-sync` | 0.8 | ✅ |
| `embassy-usb`, `embassy-usb-driver` | TBD (planned for Phase 3) | USB CDC for modern host |
| `defmt`, `defmt-rtt`, `panic-probe` | 1.0 | ✅ |
| `cortex-m`, `cortex-m-rt` | 0.7 | ✅ |
| `pio` (proc-macro for inline PIO) | latest | When Phase 1 PIO assembly begins |
| `lz4_flex` (or `lz-fear`) | latest no_std variant | Phase 8+ compression |
| `smart-leds`, `ws2812-pio` | latest | NeoPixel status (replaces simple GP7 blink) |

### Status (Phase 3 MVP, ~13 KB text + 2.5 KB BSS)

| Module / subsystem | State |
|---|---|
| `main.rs` task wiring | ✅ implemented |
| `lifecycle.rs` Phase 3 state enum | ✅ scaffold |
| `lpt/compat.rs` SPP-nibble bit-bang phy | ✅ implemented |
| `packet/{mod,commands,crc16,crc32}.rs` | ✅ implemented |
| `packet_stream.rs` byte→packet reassembler | ✅ implemented |
| `protocol/{mod,cap,block_server,stage_blobs}.rs` | ✅ implemented |
| Stage 2 placeholder blob (49 B DOS .COM) | ✅ embedded |
| GP7 heartbeat LED task | ✅ implemented |
| `lpt/compat_pio.rs` PIO-native SPP-nibble | 🟡 designed ([`pio_state_machines_design.md` §10.1-10.2](pio_state_machines_design.md)) |
| `lpt/byte.rs` Byte-mode PIO | 🟡 designed (§10.3) |
| `lpt/epp.rs` EPP PIO (fwd/rev combined) | 🟡 designed (§10.4) |
| `lpt/ecp.rs` ECP DMA + sniffer | 🟡 designed (§10.5) |
| `lpt/negotiation.rs` 1284 peripheral | 🟡 designed (§10) |
| `ps2/oversampler.rs` PIO 1 MS/s + DMA ring | 🟡 designed (§6) |
| `ps2/demodulator.rs` PIO edge-triggered byte stream | 🟡 designed (§9 two-pipeline architecture) |
| `ps2/framer.rs` + `instrumentation.rs` | 🟡 designed (§7) |
| `ps2/classifier.rs` XT/AT/PS2 auto-detect | 🟡 designed (§8) |
| `ps2/tx.rs` PIO PS/2 transmitter | 🟡 designed (§9) |
| `i8042/` private-channel client | 🟡 designed (Phase 2) |
| `debug_inject/` DEBUG script generator | 🟡 designed (Phase 1) |
| `crc_sniffer.rs` DMA sniffer wrapper | 🟡 designed (Phase 5 use; HW available now) |
| `usb_cdc/telemetry_channel.rs` | 🟡 designed ([`instrumentation_surface.md` §5](instrumentation_surface.md)) |
| `status/neopixel.rs` PIO WS2812 | 🟡 designed |
| `telemetry/signature_db.rs` keyboard fingerprint DB | 🟡 designed ([`instrumentation_surface.md` §6](instrumentation_surface.md)) |
| Compression, screen capture, VBE | ⏳ Phase 7+ |

### Plan (per phase)

Driven by [`design.md` §22 Phases 0-11](design.md); each phase's firmware deliverable is enumerated in [`pico_firmware_design.md` §13](pico_firmware_design.md). The Phase 3 MVP currently built proves the protocol end-to-end with a bit-bang phy; Phase 4-5 swap in the designed PIO programs for line-rate throughput.

| Phase | New firmware modules | Phys live | Acceptance |
|---|---|---|---|
| 0 ✅ | `main.rs` blink | — | GP7 red LED blinks |
| **3 ✅** | `lpt/compat.rs`, `packet/*`, `packet_stream.rs`, `protocol/*`, `lifecycle.rs` | bit-bang LPT | Stage 1 downloads + EXECs placeholder Stage 2 |
| 1 | `ps2/{oversampler,demodulator,framer,tx,classifier,instrumentation}`, `debug_inject/*` | PS/2 KBD (PIO) | XT/AT/PS/2 auto-detect on bench; DEBUG injects S0_*.COM |
| 2 | `i8042/*`, `ps2/{oversampler,demodulator}` for AUX | PS/2 KBD+AUX | Stage 0 unlocks i8042 private mode; bidirectional bytes |
| 4 | `lpt/{compat_pio,byte,negotiation}` | LPT PIO (compat+byte) | Stage 1's 1284 ladder lands on Byte mode |
| 5 | `lpt/{epp,ecp}`, `crc_sniffer.rs` | LPT PIO (all modes) | Stage 1 stress-test passes at ECP rates; CRC sniffer accumulates Stage 2 CRC-32 |
| 6 | `transport/dual_plane.rs`, `usb_cdc/telemetry_channel.rs` finalized | both planes | File transfer over single + dual plane; TUI consumes CDC stream |
| 7+ | screen capture, compression, VESA, full TSR | — | per `design.md` §22 |

### Open decisions

- **`nInit` GPIO routing** (Phase 3 blocker): which Pico GPIO sees nInit through the 74LVC161284? Stage 0/1 pulse nInit as the host strobe in nibble mode. Phase 3 MVP assumes GP11; needs logic-analyzer confirmation on real hardware. Flagged in [`pico_phase3_design.md` §4.4](pico_phase3_design.md) and [`pio_state_machines_design.md` §4.4](pio_state_machines_design.md).
- **LPT data-bus direction GPIO** (Phase 4+ blocker): 74LVC161284 needs a direction pin for Byte/EPP/ECP reverse modes where the Pico drives the data bus. Not enumerated in the current hardware reference; needs board verification or addition.
- **LPT status-bit GPIO mapping** (cleanup): nibble + phase output pins are non-consecutive (GP23, GP25, GP26, GP27 + GP24 in middle). Current design works via CPU pre-shuffling (~20 ns/byte cost); a v2 board could lay them consecutive for cleaner `out pins, 5`.
- **PSRAM integration:** `embassy-rp` doesn't have first-class PSRAM support yet. Plan: configure QSPI controller manually at boot, add `PSRAM : ORIGIN = 0x11000000, LENGTH = 8M` region to `memory.x`, tag screen buffers with `#[link_section = ".psram"]`. Not needed until Phase 7.
- **PSRAM cache coherency for ECP DMA** (Phase 5+): RP2350 PSRAM-as-XIP requires explicit cache flush before DMA reads. Pick single-block vs full-cache flush primitive once Phase 5 lands.
- **Multi-core split:** phys designed to run on core 1 (interrupt executor) so the protocol task on core 0 can't stall them. Validated in Phase 5 profiling.

---

## 2. `dos/stage0/`

### Purpose

Tiny DOS `.COM` programs created via `DEBUG`-script injection (typed by the Pico over the PS/2 keyboard). Establish the first real bidirectional channel between DOS and the Pico. Three variants per `ps2_eras_reference.md`. **Full design:** [`stage0_design.md`](stage0_design.md).

| File | Target | Channel |
|---|---|---|
| `s0_xt.asm` | XT, 8088/8086 | LPT (SPP nibble bidir) — keyboard input-only |
| `s0_at.asm` | AT, 286+ | LPT + i8042 keyboard port (§7.4 LED-pattern unlock) |
| `s0_ps2.asm` | PS/2 + SuperIO, 386+ | LPT + i8042 KBD + i8042 AUX (dual-lane per §17) |

### Directory layout

```
dos/stage0/
├── s0_xt.asm                       ✅ exists (1082 B .COM)
├── s0_at.asm                       ✅ exists (1635 B .COM)
├── s0_ps2.asm                      ✅ exists (1880 B .COM)
└── s0_atps2_core.inc               ✅ shared AT/PS2 core
```

### Build pipeline

Wired in `dos/Makefile`:

```make
$(BUILD)/S0_XT.COM: $(STAGE0_XT_SRC) | $(BUILD)
    $(NASM) -f bin -o $@ $<

$(BUILD)/S0_AT.COM: $(STAGE0_AT_SRC) $(STAGE0_CORE) | $(BUILD)
    $(NASM) -f bin -o $@ $<

$(BUILD)/S0_PS2.COM: $(STAGE0_PS2_SRC) $(STAGE0_CORE) | $(BUILD)
    $(NASM) -f bin -o $@ $<
```

### Plan per file

**`s0_xt.asm`** — done. Bootstrap protocol over LPT base 0x3BC/0x378/0x278 candidates; nibble receive on status bits 3–6, INIT line as host strobe. Receives Stage 1 in 64-byte blocks with CRC-16-CCITT, jumps to CS:0800h with `AX='P1'`, `BX=lpt_base`, `CX=stage1_size`, `DX=0x0001` (LPT channel up).

**`s0_at.asm`** — builds. Differs from `s0_xt.asm`:
- Adds i8042 mastery (mask IRQ1, flush 0x60, send `0xED`-mask LED unlock sequence per `design.md` §7.4)
- After unlock, can use 0x60/0x64 as a second bidirectional channel
- Tries both LPT and KBD private; either or both can succeed
- Hand-off: `DX ∈ {0x0001, 0x0002, 0x0003}` reflecting channels up

**`s0_ps2.asm`** — builds. Extends `s0_at.asm`:
- After AT-style unlock, additionally enables AUX port via `OUT 64h, 0xA8`
- Sends mouse unlock variant on AUX channel
- Hand-off: `DX ∈ {0x0001…0x0007}` reflecting channels up

### Hand-off contract (proposed extension)

```
AX = 'P1' marker (0x3150)
BX = LPT base port chosen by Stage 0 (0 if LPT not up)
CX = Stage 1 size in bytes
DX = Channel-availability bitmap, computed at hand-off time:
       bit 0 (0x01) = LPT channel up
       bit 1 (0x02) = i8042 KBD private channel up
       bit 2 (0x04) = i8042 AUX private channel up
     DX != 0 invariant. Stage 0 fails to DOS rather than handing off if
     no channel came up. Per-variant achievable ranges:
       s0_xt.asm   = {0x0001}
       s0_at.asm   ⊆ {0x0001, 0x0002, 0x0003}
       s0_ps2.asm  ⊆ {0x0001 .. 0x0007}
DS = ES = CS = host PSP segment
```

Stage 1 reads `DX` to know which channels it can use.

### Open decisions

- **Detection logic in `s0_at.asm` / `s0_ps2.asm`:** probe `0x64` for status-bit response to discriminate XT vs AT; probe AUX via `OUT 64h, 0xA8` for AT vs PS/2. Need to pick a graceful-fallback order if detection ambiguous.
- **Does Stage 0 carry the *full* keyboard injector itself, or does the Pico inject DEBUG scripts for each Stage 0 variant?** Almost certainly the latter — Stage 0 is what `DEBUG` produces from the typed hex, so the Pico needs to know which Stage 0 to type. Implies the Pico's keyboard injector embeds three hex blobs (xt/at/ps2).

---

## 3. `dos/stage1/`

### Purpose

Larger DOS-side loader, served by the Pico over LPT (or PS/2 fallback) and run by Stage 0. Detects LPT chipset, performs IEEE 1284 mode negotiation, starts SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX packet exchange (`design.md` §9), and loads Stage 2 (`pico1284`). **Full design:** [`stage1_design.md`](stage1_design.md). **As-built reference (v1.0):** [`stage1_implementation.md`](stage1_implementation.md). **Pico-side dual:** [`pico_firmware_design.md`](pico_firmware_design.md) (comprehensive) + [`pico_phase3_design.md`](pico_phase3_design.md) (Phase 3+ MVP).

### Directory layout

```
dos/stage1/
├── stage1.asm                      v0.1 scaffold (780 B); single-file initially
└── (split per stage1_design.md §Build pipeline once size grows past ~3 KB)
```

### Build pipeline

NASM `org 0x800`, flat-binary output. Already in `dos/Makefile`:

```make
$(BUILD)/stage1.bin: $(STAGE1_SRC) | $(BUILD)
    $(NASM) -f bin -o $@ $<
```

The output `stage1.bin` gets embedded into the Pico firmware via `include_bytes!` and served to Stage 0 over the bootstrap protocol.

### Status (v1.0 scaffold)

| Subsystem | State |
|---|---|
| Hand-off ABI validation | ✅ implemented |
| Inherited-state printing | ✅ implemented |
| Failure-path IRQ unmask | ✅ implemented |
| Console + hex-print + decimal helpers | ✅ implemented |
| LPT chipset detection (ECR + EPP probe) | ✅ implemented |
| CRC-16-CCITT | ✅ implemented |
| Packet encode / decode (buffer-based) | ✅ implemented |
| Packet round-trip self-test | ✅ implemented |
| LPT SPP nibble byte pump | ✅ implemented via shared `stage0/lpt_nibble.inc` |
| `tx_drain` / `rx_fill` dispatcher | ✅ implemented |
| Pump dispatcher self-test | ✅ implemented |
| IEEE 1284 negotiation (ECP/EPP/Byte/SPP ladder) | ✅ implemented |
| Capability handshake (`CAP_REQ` / `RSP` / `ACK`) | ✅ implemented |
| Pump stress test (PING/PONG round-trips + timing) | ✅ implemented |
| CRC-32/IEEE (reflected, bit-by-bit) | ✅ implemented |
| Stage 2 download (SEND/RECV/ACK/NAK + retries) | ✅ implemented (streams to PICO1284.EXE; image CRC-32 verified) |
| `PICO_BOOT` env block (LPT/MODE/CHAN/VER) | ✅ implemented |
| Stage 2 EXEC (`INT 21h AH=4Ah` resize + `AH=4Bh`) | ✅ implemented; child errorlevel propagates |
| Auto-downgrade on stress-test failure | 🟡 needs EPP/ECP pumps to fall back from |
| LPT EPP / ECP byte pumps | 🟡 stubs |
| LPT Mode B re-probe | 🟡 TODO |
| KBD / AUX private byte pumps | 🟡 TODO |

**Size:** 4821 B / 8 KB budget. ~59 % used.

### Plan (replace remaining TODOs)

In order, following [`stage1_design.md`](stage1_design.md) §Subsystems:

1. ✅ **Entry header + hand-off validation** — Stage 0 ABI accepted.
2. ✅ **LPT chipset detection** — ECR existence probe (0x35/0xCA pattern at `base+0x402`) + EPP mode-switch test. Populates `stage1_dp_caps` (lpt_base, flags, irq=0xFF, dma=0xFF). Super I/O classification and IRQ/DMA discovery deferred to Stage 2.
3. ✅ **Packet I/O (foundation)** — buffer-based `packet_encode` / `packet_validate` with CRC-16-CCITT, command IDs per §9.1, 256 B TX/RX buffers at `CS:0x2600/0x2700` (outside the binary image). Round-trip self-test catches CRC/framing bugs before any Pico traffic.
4. ✅ **Channel byte pump (LPT SPP)** — `tx_drain` / `rx_fill` dispatcher on `current_pump`; LPT SPP nibble byte loop ported from `s0_xt.asm`. EPP / ECP / KBD / AUX pumps stubbed (CF-set return) and wired after their respective negotiation / private-mode dependencies land. Dispatcher self-test verifies routing and zero-length behaviour.
5. ✅ **IEEE 1284 negotiation** — extensibility-byte handshake per `drivers/parport/ieee1284.c`. `ieee1284_negotiate` drives one cycle for a given xflag; `ieee1284_negotiate_ladder` walks ECP → EPP → Byte → SPP gated by `dp_caps`. `current_pump` stays `PUMP_LPT_SPP` until the EPP/ECP byte pumps land; `negotiated_mode` records the result for that handoff.
6. ✅ **Capability handshake** — `CAP_REQ` (0-byte) → two-stage `recv_cap_rsp` (header + LEN-driven payload) → `parse_cap_rsp_fields` (version, stage2_image_size BE→LE, stage2_image_crc32 BE→LE, active_parallel_mode) → `CAP_ACK` (0-byte). Sanity checks: `version_major == 1`, `0 < size ≤ MAX_STAGE2_SIZE`. Skipped cleanly when `negotiated_mode == NEG_MODE_NONE` (no LPT) or `> NEG_MODE_SPP` (pump not ready). Adds `pico_version_*`, `pico_active_mode`, `stage2_image_size`, `stage2_image_crc32` data slots and `print_hex32` helper.
7. ✅ **Pump stress test** — runs 8 iterations of `CMD_PING` (64-byte payload) → `CMD_PONG` (echo) before committing to Stage 2 download. Counts errors (timeout, length mismatch, CRC fail, payload mismatch, wrong cmd). Measures elapsed time via `INT 21h AH=2Ch` with minute-wrap handling. Prints `STAGE1: stress: 8 iter, N errors, NNN centis`. Returns CF set if errors > `STRESS_MAX_ERRORS` (currently 0). Skips cleanly when `current_pump == PUMP_NONE`. Ladder fallback waits on EPP/ECP byte pumps (no pump lower than SPP/Nibble exists today).
8. ✅ **Stage 2 download** — streams 64-byte blocks via `CMD_SEND_BLOCK` (u32 block_no BE) → `CMD_RECV_BLOCK` (block_no echo + u8 byte_count + data) → `CMD_BLOCK_ACK` / `CMD_BLOCK_NAK` (with up to `DL_BLOCK_RETRIES = 3` retries per block). Per-block packet integrity is the existing CRC-16-CCITT in `packet_validate`; a running CRC-32/IEEE (reflected, bit-by-bit, init 0xFFFFFFFF, xor-out 0xFFFFFFFF) is folded over the payload bytes and verified against `stage2_image_crc32` from CAP_RSP at finish. Last block may be short (`byte_count = size mod 64`). Streams directly to `PICO1284.EXE` on disk (`INT 21h AH=3Ch/40h/3Eh`); on any failure the partial file is deleted (`AH=41h`) and CF is returned set. Progress dot every 64 blocks (~4 KB). Skips cleanly when no pump is up or no CAP size has been received.
9. ✅ **Stage 2 EXEC + `PICO_BOOT` env** — `build_environment` writes a fresh PSP env block at `CS:ENV_BUF_OFF` (paragraph-aligned at 0x2800): `PICO_BOOT=LPT=XXXX MODE=YYY CHAN=N VER=X.Y\0`, double-NUL terminator, DOS 3.0+ count word + ASCIIZ program path. `exec_stage2` shrinks our memory allocation via `INT 21h AH=4Ah` (to `EXEC_RESIZE_PARAS = 0x300` paragraphs ≈ 12 KB), patches the EXEC parameter block segments to CS-relative values, then `INT 21h AH=4Bh AL=00` with the param block at `ES:BX` and the filename at `DS:DX`. Child exit code is queried via `INT 21h AH=4Dh` and propagated as Stage 1's own errorlevel via `INT 21h AH=4Ch`.

Target size: ≤8 KB blob (well within Stage 0's `MAX_STAGE1_SIZE = 50000`).

### Open decisions

- **Should Stage 1 also handle the PS/2 dual-lane case (§17), or is that exclusively Stage 0 + Stage 2 territory?** Probably Stage 1 inherits whatever channels Stage 0 negotiated (via `DX` kind code), but only really *uses* LPT — the PS/2 lane lives quiet until Stage 2 wants the fallback.

---

## 4. `dos/pico1284/`

### Purpose

Production DOS client per `design.md` §21.1. Either a `.COM`, `.EXE`, or TSR depending on use case. Per `design.md` §21.2, it has modules:

```
port_detect.asm/c      detect LPT base, ECP/EPP capability, ECP DMA channel + IRQ
                       (populates dp_caps; see two_plane_transport.md §Capability discovery)
ieee1284.asm/c         negotiation and mode setup; PIO pump + optional ECP DMA pump
                       with bounded slices and polled terminal-count completion
dma_bounce.asm/c       conventional-memory bounce buffer for ECP DMA, 64 KiB-page-safe
packet.c               SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX framing
cpu_detect.asm         install-time 8086/286/386+ probe; ~30 B (design.md §21.3)
crc.asm/c              CRC-16 + CRC-32  ──  CRC-32 split into _8086 + _386 variants
ps2_i8042.asm/c        Stage 0 and fallback control
screen_text.c          text-mode capture and diff  ──  diff split into _8086 + _386
screen_vesa.c          VBE mode query and graphics capture  ──  tile-diff split
                       into _8086 (rep movsw) + _386 (rep movsd) variants
compress_rle.asm/c     RLE and Delta+RLE  ──  inner loop split into _8086 + _386
file_xfer.c            file transfer protocol
console.c              console stream and dictionary compression
tsr.c                  resident command handler if used; wires hot_dispatch table
                       at install time per cpu_detect result
```

**Hot-loop CPU dispatch.** Four modules (`crc`, `screen_text`, `screen_vesa`, `compress_rle`) ship dual-CPU hot loops. See [`design.md` §21.3](design.md) for rationale and build pattern. The 286 takes the 8086 dispatch path — small 286-only gains aren't worth a third code path.

### Directory layout (target state)

```
dos/pico1284/
├── pico1284.asm                    current 62-byte .COM stub
├── wmakefile                       Open Watcom build
├── src/
│   ├── port_detect.{asm,c}
│   ├── ieee1284.{asm,c}
│   ├── packet.c
│   ├── cpu_detect.asm              install-time CPU-class probe
│   ├── crc_8086.asm                CRC-32 8086 baseline
│   ├── crc_386.asm                 CRC-32 386 32-bit accumulator
│   ├── crc.c                       CRC-16 + dispatch glue
│   ├── ps2_i8042.{asm,c}
│   ├── screen_text_8086.c          text-diff 8086 baseline
│   ├── screen_text_386.c           text-diff 386 (movzx + 32-bit loop)
│   ├── screen_text.c               capture orchestration + dispatch
│   ├── screen_vesa_8086.c          tile-diff 8086 baseline
│   ├── screen_vesa_386.c           tile-diff 386 (rep movsd)
│   ├── screen_vesa.c               VBE query + capture orchestration
│   ├── compress_rle_8086.asm       RLE/Delta+RLE 8086 baseline
│   ├── compress_rle_386.asm        RLE/Delta+RLE 386 inner loop
│   ├── compress_rle.c              compression orchestration + dispatch
│   ├── file_xfer.c
│   ├── console.c
│   └── tsr.c                       wires hot_dispatch at install
├── include/                        local headers
└── (output: dos/build/PICO1284.EXE)
```

### Build pipeline (when Watcom is installed)

NASM produces `.obj`; `wcc` produces `.obj`; `wlink` links into MZ `.EXE` (or `.COM` for small-model). Top-level `dos/Makefile` invokes `wmake` in `dos/pico1284/`. Hot-loop `_8086` sources compile with `wcc -0` / NASM 8086 baseline; `_386` sources compile with `wcc -3` / NASM with 32-bit operand-size overrides. All variants link into the same `PICO1284.EXE`; dispatch is wired at install time (see [`design.md` §21.3 CPU-Class Dispatch in Stage 2](design.md)).

Skeleton `dos/Makefile` rule:

```make
$(BUILD)/PICO1284.EXE: $(PICO1284_SRC) | $(BUILD)
    $(MAKE) -C pico1284 build
    cp pico1284/build/pico1284.exe $@
```

### Sequencing

This is the biggest single subrepo and the lowest-priority initially. Phase 10 in the roadmap. Order of module work, roughly:

1. `port_detect`, `crc`, `packet` — Phase 4
2. `ieee1284` — Phase 5
3. `file_xfer`, `console` — Phase 6
4. `screen_text`, `compress_rle` — Phase 7–8
5. `screen_vesa` — Phase 9
6. `ps2_i8042`, `tsr` — Phase 10–11

### Open decisions

- **Watcom V2 installation on macOS:** build from source (Codeberg), grab a prebuilt binary, or run inside a Linux VM? Decide before Phase 6.
- **TSR vs CLI vs both:** design doc says "TSR/CLI". Most flexibility is a single binary that detects mode (resident? no? installer?) at startup.
- **`.COM` vs `.EXE`:** stage0 is `.COM` (fits in 64 KB single segment). Production pico1284 will exceed 64 KB; needs `.EXE` (MZ format, multi-segment small/compact/large model). Choose small-model first, expand if needed.

---

## 5. `dos/common/`

### Purpose

Shared declarations across `stage0/`, `stage1/`, and `pico1284/`:

- Packet command IDs (matches `firmware/packet/commands.rs`)
- IEEE 1284 mode constants (matches `firmware/ieee1284/modes.rs`)
- CRC-16-CCITT polynomial, init vector, etc.
- Bootstrap protocol constants (already inlined in `s0_xt.asm` — would move here)
- Stage 0 hand-off contract constants

### Directory layout (target state)

```
dos/common/
├── protocol.inc                    NASM include — packet IDs, IEEE 1284 modes, CRC consts
├── protocol.h                      Watcom C equivalent
├── handoff.inc                     Stage 0 → Stage 1 hand-off contract
└── handoff.h
```

### Build pipeline

Pure headers — no compilation. NASM `%include "../common/protocol.inc"` from each `.asm` file; Watcom `#include "../common/protocol.h"` from each `.c` file.

### Sequencing

**Not needed yet.** Today `s0_xt.asm` is the only DOS source and inlines its constants. Create `dos/common/protocol.inc` the day `stage1.asm` first needs to share `CMD_*` IDs with `s0_xt.asm`. Likely Phase 4.

### Open decisions

- **Single source of truth for protocol constants:** since the Rust side also has `firmware/src/packet/commands.rs` and `host/src/packet/commands.rs`, there's a maintenance question. Options: (a) hand-maintain `dos/common/protocol.inc` to match the Rust side, with a CI check; (b) code-gen `.inc`/`.h` from a single TOML/YAML spec; (c) just accept periodic drift, version-tag protocol changes.

---

## 6. `host/`

### Purpose

Modern-host USB CDC client. Connects to the Pico over USB-C, receives the framed packet stream from `firmware/`, exposes:

- File transfer (send/recv files to/from the DOS PC)
- Screen viewer (text-mode + VBE)
- Console terminal
- Remote command execution
- Memory inspection
- Diagnostics + logs

### Directory layout (proposed — Rust)

```
host/
├── Cargo.toml                      workspace member
├── src/
│   ├── main.rs                     CLI dispatch (clap)
│   ├── lib.rs                      library entry (so subcommands can be reused)
│   ├── transport/
│   │   ├── mod.rs
│   │   └── cdc.rs                  serialport / tokio-serial USB CDC
│   ├── packet/                     either: shared no_std workspace member with firmware
│   │   └── …
│   ├── session.rs                  capability handshake mirroring firmware/session/
│   ├── file.rs                     file_xfer client side
│   ├── screen/
│   │   ├── text.rs                 80×25 text diff render
│   │   └── vbe.rs                  VBE tile reassembly
│   ├── console.rs                  remote terminal
│   └── exec.rs                     remote command execution
└── examples/
    └── …
```

### Build pipeline

`cargo build -p vintage-kvm-host` from repo root. Or scope an alias in `.cargo/config.toml`:

```toml
[alias]
host       = "build -p vintage-kvm-host"
host-run   = "run   -p vintage-kvm-host"
```

Single static binary per platform — no runtime install required.

### Dependencies (Rust, planned)

| Dep | Why |
|---|---|
| `serialport` (sync) or `tokio-serial` (async) | USB CDC ACM transport |
| `clap` (with `derive`) | CLI subcommands |
| `tokio` | async runtime if going async |
| `anyhow` + `thiserror` | error handling |
| `tracing` + `tracing-subscriber` | structured logging |
| Shared `packet` crate from workspace | protocol types |
| `image` (or `ratatui` for TUI screen viewer) | screen render |

### Module sequencing (matched to phases)

| Phase | Module | What it does |
|---|---|---|
| 3 | `transport/cdc.rs` + `main.rs` minimal | Open CDC port, dump bytes |
| 4 | `packet/` + `session.rs` | Cap handshake; verify protocol round-trip |
| 6 | `file.rs` | File send/recv subcommands |
| 7 | `screen/text.rs` | 80×25 text viewer |
| 8 | (compression integration — no new module, decode in `packet/`) | RLE/Delta+RLE decode |
| 9 | `screen/vbe.rs` | VBE viewer |
| 10 | `console.rs`, `exec.rs` | terminal + remote shell |

### Open decisions

- **Language confirm: Rust?** (recommendation above) — needs explicit go-ahead before scaffolding.
- **Sync vs async:** `tokio-serial` is fine, but for a single-port CLI tool, synchronous `serialport` is simpler. Decide once concurrency demands appear (Phase 7 screen streaming probably wants async).
- **UI: pure CLI, TUI (`ratatui`), or GUI?** CLI for v0; TUI for screen viewer is plausible at Phase 7+. GUI is out of scope until someone asks.

---

## 7. `tools/`

### Purpose

Dev fixtures, protocol tooling, and one-off scripts that don't belong in production. None blocking; each one is justified individually.

### Directory layout (proposed, build incrementally)

```
tools/
├── README.md
├── tui/                            ratatui dashboard over CDC telemetry
│   ├── Cargo.toml                  ratatui + tokio + serde_json + clap
│   └── src/
│       ├── main.rs                 CDC attach, arg parsing
│       ├── stream.rs               tokio task reading JSON-line events
│       ├── state.rs                arc-swap'd shared state per view
│       ├── views/                  overview, kbd, aux, lpt, events, hardware, fingerprint
│       └── widgets/                histogram, progress, status_dot, plane_topology
├── delock-fixture/                 libusb client for PL2305-based DeLock adapter
│   ├── Cargo.toml
│   └── src/main.rs
├── packet-dissector/               Wireshark Lua dissector for the SOH-framed protocol
│   └── vintage_kvm.lua
├── capture-replay/                 PS/2 line capture/replay for debugging
│   └── …
└── bring-up/                       hardware bring-up scripts (probe board, check pin continuity)
    └── …
```

### Build pipeline

Each tool is independently buildable. The `tui/` crate ships as a workspace member once it lands (shares `serde` types with the firmware's telemetry emitter via a small shared crate, TBD). Other tools stay outside the workspace unless they grow enough to deserve membership.

### Plan per tool

| Tool | Purpose | Priority |
|---|---|---|
| `tui` | Ratatui dashboard rendering the Pico's CDC telemetry stream: plane topology + per-port stats + histograms + fingerprint dump. Five views (Overview, KBD, AUX, LPT, Events, Hardware, Fingerprint) per [`instrumentation_surface.md` §4](instrumentation_surface.md). Also supports `--replay <file.jsonl>` for offline analysis. | Phase 6 (CDC stream available from Phase 3 onward via `jq`, so TUI is convenience over necessity) |
| `delock-fixture` | Mac/Linux libusb client claiming PL2305 interface A0/A1 to act as a fake DOS LPT host for testing Pico Stage 0 firmware in Compat/Nibble modes | Phase 3 — first hardware fixture worth building |
| `packet-dissector` | Wireshark Lua dissector for the SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX format so packet captures over USB CDC are readable | Phase 4+ |
| `capture-replay` | Logic-analyzer-style capture of PS/2 CLK/DATA lines for protocol debugging (Saleae-compatible format) | Phase 1–2 if PS/2 timing issues appear; partially obsoleted by the TUI's oversampler ring view |
| `bring-up` | Per-Feather smoke tests beyond the LED blink (probe every GPIO, check pull-ups, validate PSRAM accessible) | Once first Feather arrives |

### Open decisions

- **Tool languages:** mix is fine. Rust for the libusb fixture (shares packet types with firmware/host), Lua for Wireshark, Python for one-off bring-up scripts. Don't enforce uniformity.

---

## 8. `hardware/`

### Purpose

KiCad project for the Feather + 74LVC161284 + 74LVC07A + connectors. Produces a schematic for review, then a PCB carrier board that consolidates the breadboard prototype into a manufacturable artifact.

### Directory layout (proposed)

```
hardware/
├── README.md
├── vintage-kvm.kicad_pro
├── vintage-kvm.kicad_sch           top schematic
├── ieee1284.kicad_sch              sub-sheet: Feather HSTX → 74LVC161284 → DB-25
├── ps2.kicad_sch                   sub-sheet: Feather → 74LVC07A → 2× mini-DIN
├── power.kicad_sch                 sub-sheet: USB-C VBUS, 3V3 SMPS, decoupling
├── vintage-kvm.kicad_pcb           PCB layout (later — schematic first)
├── symbols/                        custom symbols (Feather, etc.)
├── footprints/                     custom footprints
├── 3dmodels/                       
└── bom/
    └── vintage-kvm.csv             CSV BOM exportable from KiCad
```

### Build pipeline

KiCad 9 (or newer). No CI integration initially; manual export of schematic PDF, BOM, gerbers.

Optional later: `kicad-cli` for headless PDF/BOM export in CI.

### Sequencing

1. **First artifact: schematic-only**, mirroring `hardware_reference.md` §3.3 + §6 exactly. No PCB yet. Goal is reviewable single-PDF deliverable that captures the BOM, net names, and pin assignments.
2. **Second artifact: protoboard layout** — through-hole + minimum SMT for hand-assembly. Optimize for debug-ability over size.
3. **Third artifact: integrated carrier board** — proper SMT layout, includes connectors, possibly LCD or button I/O for status. Optional / future.

### Open decisions

- **KiCad version:** assume 9 (current as of 2026). Pin if 10 ships during the project.
- **Schematic-only vs full PCB scope:** schematic-only is a much smaller commitment. Defer layout until firmware is working on breadboarded version.
- **Carrier board manufacturer:** OSH Park, JLCPCB, PCBWay — all fine for low volume; decide when layout exists.

---

## Sequencing & critical path

### Immediate (when Feather boards arrive)

1. **Phase 0 verification** on the actual board: flash, observe GP7 blink, validate JST SH SWD probe works → `firmware/` checkpoint.
2. **Phase 3 MVP bench validation** (`firmware/lpt/compat.rs` bit-bang + `protocol/*`). Confirm `nInit` GPIO routing on real hardware; run Stage 1's `pump_stress_test` + Stage 2 download against the placeholder blob. Validates the full DOS→Pico protocol over the slowest path.
3. **Phase 1 PS/2 keyboard endpoint** (`firmware/ps2/{oversampler,demodulator,framer,tx,classifier}`). Build the PIO programs from [`pio_state_machines_design.md` §6-9](pio_state_machines_design.md); validate XT/AT/PS2 auto-detection against a real DOS PC.
4. **Phase 1 DEBUG injection** — Pico types a fixed hex sequence that produces a tiny test `.COM` on the DOS PC; verify the `.COM` runs.

That's a 4-step demonstrable milestone covering both planes end-to-end with no `host/` or Watcom dependency. Phase 3 MVP code is already on `master`; only hardware verification of `nInit` routing is required to declare Phase 3 done.

### Parallel-track work that doesn't need Feather boards

- **`hardware/` schematic** — produce a KiCad schematic PDF mirroring `hardware_reference.md`. Reconcile with the open hardware questions from `pio_state_machines_design.md` §13 (nInit routing, LPT data-direction GPIO, status-pin consecutive layout).
- **`tools/tui/` skeleton** — `ratatui` + tokio + serde scaffold that consumes the CDC JSON-line stream. Can be developed against a fixture file (`--replay`) before any firmware emits real telemetry. Spec: [`instrumentation_surface.md` §4 + §5](instrumentation_surface.md).
- **`host/` language decision** — confirm Rust, scaffold workspace member with a stub `main.rs` that opens CDC 0 (packet stream). The TUI consumes CDC 1 (telemetry) separately.
- **`tools/delock-fixture`** — if the DeLock adapter is in hand, prototype the libusb claim sequence. Useful for Phase 3-4 testing.
- **`dos/common/`** — design (but don't yet create) the protocol constants header. Worth specifying ahead of Stage 1 work.

### Critical path (longest dependency chain)

```
Feather boards arrive
   │
   ▼
firmware Phase 0 verified                                ✓ code exists, awaits bench
   │
   ├─── confirm nInit GPIO routing ──┐
   │                                  │
   ▼                                  ▼
firmware Phase 3 MVP bench validated  dos/stage1 bench validated
   │  (bit-bang LPT + protocol stack already on master)
   │
   ▼
firmware Phase 1 PS/2 PIO programs                       ✓ designed (pio_state_machines_design.md)
   │  (oversampler + demodulator + framer + tx + classifier)
   │
   ▼  ┌──────────────────────────────────┐
       │ Phase 1: DEBUG injection         │  ✓ designed (debug_inject/*)
       │ Phase 2: i8042 private mode      │  ✓ stage0 side done, firmware designed
       └──────────┬───────────────────────┘
                  │
                  ▼  ┌──────────────────────────────────┐
                     │ firmware Phase 4-5: PIO LPT      │  ✓ designed
                     │ Byte / EPP / ECP-DMA + sniffer   │
                     └──────────┬───────────────────────┘
                                │
                                ▼  ┌──────────────────────────────────┐
                                   │ tools/tui/ ships                  │  ✓ designed (instrumentation_surface.md)
                                   │ host/ file-transfer demo          │
                                   └──────────┬───────────────────────┘
                                              │
                                              ▼  (Watcom installed)
                                   ┌──────────────────────────────────┐
                                   │ dos/pico1284/ initial modules    │
                                   └──────────┬───────────────────────┘
                                              │
                                              ▼
                                   ┌──────────────────────────────────┐
                                   │ Screen dump (Phases 7–9)         │
                                   └──────────────────────────────────┘
```

Time-estimate-honest: this is a 6–12 month project at hobby cadence. The first demoable end-to-end win — Stage 1 downloads Stage 2 over LPT bit-bang — is reachable in a couple of bench sessions after the boards land (Phase 3 MVP code is already on master). After that, Phase 1 PS/2 PIO + DEBUG injection lands the truly autonomous bootstrap; Phase 4-5 promotes throughput to ECP-DMA rates.

---

## Open decisions summary

| # | Decision | Owner | Needed by | Status |
|---|---|---|---|---|
| 1 | `host/` language — confirm Rust? | User | Phase 6 (file transfer); ideally now to scaffold | Open |
| 2 | PS/2 mode selection: ~~config GPIO vs persisted flash flag vs two firmware builds~~ | Implementation | Phase 1 | **Resolved** — auto-detect from oversampled traffic (XT/AT/PS2 classifier per [`pio_state_machines_design.md` §8](pio_state_machines_design.md)); no operator selection needed |
| 3 | Open Watcom V2 installation method on macOS (source build / prebuilt / Linux VM) | User | Phase 6 | Open |
| 4 | Single source of truth for protocol constants (hand-maintained / code-gen / drift-tolerant) | Implementation | Phase 4 | Open; current state is hand-maintained (firmware/src/packet/commands.rs ↔ dos/stage1/stage1.asm) |
| 5 | `dos/pico1284/` packaging: `.COM` small-model vs `.EXE` small-model vs `.EXE` compact/large | Implementation | Phase 10 | Open |
| 6 | ~~UI for `host/`: pure CLI, TUI (`ratatui`), or GUI~~ | User | Phase 7+ | **Resolved** — `tools/tui/` ratatui dashboard for instrumentation/observability (spec'd in [`instrumentation_surface.md`](instrumentation_surface.md)); `host/` itself is a CLI for actual file/screen ops. Separate concerns. |
| 7 | KiCad project scope: schematic-only or schematic + carrier-board PCB | User | Hardware track, any time | Open |
| 8 | Stage 0 hand-off contract: extend `DX` kind code, or use additional registers? | Implementation | Phase 2 | Open; current Phase 3 path doesn't depend on this |
| 9 | **`nInit` GPIO routing** through the 74LVC161284 | Hardware | Phase 3 bench validation | Open — required for Phase 3 MVP hardware validation |
| 10 | **LPT data-bus direction GPIO** (for Pico-driven reverse modes) | Hardware | Phase 4 | Open — Byte/EPP/ECP rev modes blocked |
| 11 | **LPT status-bit GPIO consecutive layout** (v2 board cleanup) | Hardware | Optional v2 board | Open — current layout works via CPU pre-shuffle, ~20 ns/byte cost |
| 12 | **PSRAM cache flush policy** for ECP DMA reads | Implementation | Phase 5+ | Open — pick single-block vs full-cache flush primitive |
