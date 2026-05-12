# vintage-kvm Implementation Plan

**Status:** Living planning document  
**Last updated:** 2026-05-12  
**Companion documents:** [`design.md`](design.md), [`hardware_reference.md`](hardware_reference.md), [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md), [`ps2_eras_reference.md`](ps2_eras_reference.md), [`ps2_private_channel_design.md`](ps2_private_channel_design.md)

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
| 1 | `firmware/` | RP2350 Pico firmware | **Phase 0 working** | Phase 1: PS/2 keyboard endpoint PIO + state machine |
| 2 | `dos/stage0/` | DOS Stage 0 bootstrap | **1 of 3 variants working** | Add `s0_at.asm` + `s0_ps2.asm` |
| 3 | `dos/stage1/` | DOS Stage 1 loader | **Stub** | Real LPT detect + IEEE 1284 negotiation + Stage 2 hand-off |
| 4 | `dos/pico1284/` | DOS Stage 2 TSR/CLI | **Stub** | Install Open Watcom V2; set up wmake build alongside NASM |
| 5 | `dos/common/` | Shared NASM/C headers | **Not created (YAGNI)** | Create on first cross-stage constant |
| 6 | `host/` | Modern-host USB CDC client | **Not created, language undecided (Rust recommended)** | Decide language; scaffold workspace member |
| 7 | `tools/` | Dev fixtures + tooling | **Not created** | Defer until Phase 3 — first useful tool is libusb DeLock fixture |
| 8 | `hardware/` | KiCad schematic + PCB | **Not created** | Initial schematic-only project mirroring `hardware_reference.md` §3.3 |

### Dependency graph (what blocks what)

```
                      ┌─────────────────────────────┐
                      │       firmware/             │
                      │  (Phase 0 done; Phase 1+    │
                      │   needs Feather boards)     │
                      └──────┬──────────────────────┘
                             │ tests against
                             ▼
              ┌──────────────────────────────────────┐
              │       dos/stage0/                    │
              │  s0_xt working; s0_at/s0_ps2 need    │
              │  firmware PS/2 endpoint to test      │
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
| 5: EPP/ECP modes | `firmware/ieee1284/` | `dos/stage1/` | After Phase 3; needs real DOS PC or USS-720 |
| 6: Raw file transfer | `dos/pico1284/`, `host/` | `firmware/packet/` | **Needs `host/` decision** |
| 7–9: Screen dump (text → RLE → VESA) | `dos/pico1284/`, `firmware/compression/`, `host/` | — | Needs Watcom + `host/` |
| 10: Full TSR/CLI | `dos/pico1284/` | — | Needs Watcom |
| 11: PS/2 dual-lane fallback | `firmware/ps2/`, `dos/stage0/s0_ps2.asm` | — | After Phases 1+2 |

---

## 1. `firmware/`

### Purpose

Single binary that runs on the Adafruit Feather RP2350 HSTX + 8 MB PSRAM, bridging the DOS PC's PS/2 keyboard/mouse ports and LPT to a modern host over USB CDC. Implements all the protocol layers in `design.md` §4 and §20.1.

### Directory layout (target state)

```
firmware/
├── .cargo/config.toml             target = thumbv8m.main-none-eabihf, probe-rs runner
├── Cargo.toml
├── build.rs                       copies memory.x to OUT_DIR
├── memory.x                       RP2350 + 8 MB PSRAM regions
├── src/
│   ├── main.rs                    Embassy `#[main]`, task spawn, init
│   ├── ps2/
│   │   ├── mod.rs
│   │   ├── pio.rs                 ps2_at_dev.pio bindings (and ps2_xt_dev.pio for XT mode)
│   │   ├── ps2_at_dev.pio         oversampled host-traffic decoder (ps2_private_channel_design.md)
│   │   ├── ps2_xt_dev.pio
│   │   ├── kbd.rs                 keyboard state machine (BAT, ACK, LED-unlock detector, typematic, Lane-0 control encoder)
│   │   ├── mouse.rs               mouse state machine (3-byte packets, IntelliMouse knock, Lane-1 data encoder)
│   │   ├── scancodes.rs           Set 1 + Set 2 tables
│   │   ├── private_mode.rs        post-unlock dual-lane framing per ps2_private_channel_design.md
│   │   ├── host_timing.rs         oversampled host-clock histogram + chipset fingerprinting
│   │   └── calibration.rs         adaptive timing negotiation (SAFE/STANDARD/FAST/EXPERIMENTAL modes)
│   ├── ieee1284/
│   │   ├── mod.rs
│   │   ├── pio.rs                 PIO bindings
│   │   ├── ieee1284_compat.pio    Compatibility/Nibble mode
│   │   ├── ieee1284_ecp.pio       ECP mode
│   │   ├── ieee1284_epp.pio       EPP mode
│   │   ├── negotiation.rs         IEEE 1284 negotiation watcher (design.md §8)
│   │   └── modes.rs               mode selection + transition state machine
│   ├── usb/
│   │   ├── mod.rs
│   │   └── cdc.rs                 USB CDC ACM endpoint to modern host
│   ├── packet/                    SHARED CRATE (see Cross-cutting / language decision)
│   │   ├── mod.rs
│   │   ├── framing.rs             SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX (design.md §9)
│   │   ├── crc.rs                 CRC-16-CCITT, CRC-32
│   │   └── commands.rs            command ID constants (design.md §9.1)
│   ├── session/
│   │   ├── mod.rs
│   │   ├── capability.rs          CAP_REQ/RSP/ACK handshake (design.md §10)
│   │   └── router.rs              packet dispatch
│   ├── compression/
│   │   ├── mod.rs
│   │   ├── rle.rs
│   │   ├── delta_rle.rs
│   │   └── lz4.rs
│   ├── screen/
│   │   ├── mod.rs
│   │   ├── text_diff.rs           cell-diff codec (design.md §13)
│   │   └── vbe_tile.rs            tile-diff codec for VESA (design.md §14)
│   └── status.rs                  NeoPixel GP21 status indicator (replaces GP7 blink)
```

### Build pipeline

Already scaffolded. `cargo fw-release` from repo root, or `cd firmware && cargo build --release`. Output: `target/thumbv8m.main-none-eabihf/release/vintage-kvm-firmware` (ELF). Flash via `cargo fw-run` (probe-rs), or switch runner in `firmware/.cargo/config.toml` to `elf2uf2-rs -d` for BOOTSEL mode.

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

### Module responsibilities (per phase)

| Module | Phase | Lines (rough) | Tested against |
|---|---|---:|---|
| `main.rs` + `status.rs` | 0 | 80 | Bench: GP7 LED visible blink |
| `ps2/pio.rs` + `ps2_at_dev.pio` | 1 | ~200 | DOS PC keyboard port accepts scan codes |
| `ps2/kbd.rs` + `scancodes.rs` | 1–2 | ~300 | DEBUG script types correctly; §7.4 unlock detected |
| `ps2/private_mode.rs` | 2 | ~150 | Bidirectional byte exchange with `s0_at.asm` |
| `ps2/mouse.rs` | 11 | ~200 | Optional AUX endpoint, IRQ12 reaches DOS |
| `ieee1284/negotiation.rs` + `pio.rs` | 3 | ~400 | DOS PC negotiates into Compat mode |
| `ieee1284/modes.rs` + ECP/EPP PIO | 5 | ~600 | DOS PC negotiates ECP/EPP successfully |
| `packet/` | 4 | ~250 | Round-trip packets across CDC, CRC checks pass |
| `session/capability.rs` + `router.rs` | 4 | ~200 | Capability intersection produces valid mode set |
| `usb/cdc.rs` | 3+ | ~150 | Modern host opens CDC port, see frames |
| `compression/{rle,delta_rle,lz4}.rs` | 7–9 | ~400 | Roundtrip compression tests |
| `screen/text_diff.rs` | 7 | ~300 | 80×25 text mode diff renders correctly on host |
| `screen/vbe_tile.rs` | 9 | ~600 | 640×480×8 VBE frame streams |

### Open decisions

- **PIO block allocation** (`design.md` §20.2): tentatively block 0 = IEEE 1284, block 1 = ECP/EPP, block 2 = PS/2 KBD + AUX. Confirm after PIO program sizing in Phase 1.
- **PSRAM integration:** `embassy-rp` doesn't have first-class PSRAM support yet. Plan: configure QSPI controller manually at boot, add `PSRAM : ORIGIN = 0x11000000, LENGTH = 8M` region to `memory.x`, tag screen buffers with `#[link_section = ".psram"]`. Not needed until Phase 7.
- **Mode selection between XT keyboard and AT/PS/2 keyboard:** config GPIO at boot? Persisted flash flag? Two separate firmware builds like `No0ne/ps2pico`? Lean toward config GPIO (no flash juggling, instant retest).

---

## 2. `dos/stage0/`

### Purpose

Tiny DOS `.COM` programs created via `DEBUG`-script injection (typed by the Pico over the PS/2 keyboard). Establish the first real bidirectional channel between DOS and the Pico. Three variants per `ps2_eras_reference.md`:

| File | Target | Channel |
|---|---|---|
| `s0_xt.asm` | XT, 8088/8086 | LPT (SPP nibble bidir) — keyboard input-only |
| `s0_at.asm` | AT, 286+ | LPT + i8042 keyboard port (§7.4 LED-pattern unlock) |
| `s0_ps2.asm` | PS/2 + SuperIO, 386+ | LPT + i8042 KBD + i8042 AUX (dual-lane per §17) |

### Directory layout

```
dos/stage0/
├── s0_xt.asm                       ✅ exists (1022 B .COM)
├── s0_at.asm                       NEW
└── s0_ps2.asm                      NEW
```

### Build pipeline

Already wired in `dos/Makefile`:

```make
$(BUILD)/S0_XT.COM: $(STAGE0_SRC) | $(BUILD)
    $(NASM) -f bin -o $@ $<
```

Add two more rules of the same shape for `s0_at` and `s0_ps2`. Outputs to `dos/build/S0_AT.COM` and `dos/build/S0_PS2.COM`.

### Plan per file

**`s0_xt.asm`** — done. Bootstrap protocol over LPT base 0x3BC/0x378/0x278 candidates; nibble receive on status bits 3–6, INIT line as host strobe. Receives Stage 1 in 64-byte blocks with CRC-16-CCITT, jumps to CS:0800h with `AX='P1'`, `BX=lpt_base`, `CX=stage1_size`, `DX=0x0003 (XT_LPT_BOOTSTRAP)`.

**`s0_at.asm`** — TODO. Differs from `s0_xt.asm`:
- Adds i8042 mastery (mask IRQ1, flush 0x60, send `0xED`-mask LED unlock sequence per `design.md` §7.4)
- After unlock, can use 0x60/0x64 as a second bidirectional channel
- Falls back to LPT path if i8042 unlock fails
- Hand-off: `DX=0x0005 (AT_DUAL_CHANNEL)`

**`s0_ps2.asm`** — TODO. Extends `s0_at.asm`:
- After AT-style unlock, additionally enables AUX port via `OUT 64h, 0xA8`
- Sends mouse unlock variant on AUX channel
- Hand-off: `DX=0x0007 (PS2_TRIPLE_CHANNEL)`

### Hand-off contract (proposed extension)

```
AX = 'P1' marker (0x3150)
BX = LPT base port chosen by Stage 0
CX = Stage 1 size in bytes
DX = Stage 0 kind code:
       0x0003 = XT_LPT_BOOTSTRAP        (LPT only)
       0x0005 = AT_DUAL_CHANNEL         (LPT + KBD private mode)
       0x0007 = PS2_TRIPLE_CHANNEL      (LPT + KBD private + AUX private)
DS = ES = CS = host PSP segment
```

Stage 1 reads `DX` to know which channels it can use.

### Open decisions

- **Detection logic in `s0_at.asm` / `s0_ps2.asm`:** probe `0x64` for status-bit response to discriminate XT vs AT; probe AUX via `OUT 64h, 0xA8` for AT vs PS/2. Need to pick a graceful-fallback order if detection ambiguous.
- **Does Stage 0 carry the *full* keyboard injector itself, or does the Pico inject DEBUG scripts for each Stage 0 variant?** Almost certainly the latter — Stage 0 is what `DEBUG` produces from the typed hex, so the Pico needs to know which Stage 0 to type. Implies the Pico's keyboard injector embeds three hex blobs (xt/at/ps2).

---

## 3. `dos/stage1/`

### Purpose

Larger DOS-side loader, served by the Pico over LPT (or PS/2 fallback) and run by Stage 0. Detects LPT chipset, performs IEEE 1284 mode negotiation, starts SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX packet exchange (`design.md` §9), and loads Stage 2 (`pico1284`).

### Directory layout

```
dos/stage1/
├── stage1.asm                      currently a 42-byte stub
└── (probably no submodules — single-file blob ≤8 KB)
```

### Build pipeline

NASM `org 0x800`, flat-binary output. Already in `dos/Makefile`:

```make
$(BUILD)/stage1.bin: $(STAGE1_SRC) | $(BUILD)
    $(NASM) -f bin -o $@ $<
```

The output `stage1.bin` gets embedded into the Pico firmware via `include_bytes!` and served to Stage 0 over the bootstrap protocol.

### Plan (replace stub)

Sections in order:

1. **Entry header** — at offset 0x800, accept Stage 0 hand-off contract (AX/BX/CX/DX)
2. **Port detection** — `design.md` §8.3: probe ECR register, EPP_ADDR register; classify SuperIO chipset
3. **IEEE 1284 negotiation** — issue extensibility byte sequence; check Pico's response
4. **Mode selection** — ECP → EPP → Byte → Compat fallback
5. **Packet I/O** — minimal framing for the next stage of bootstrapping
6. **Stage 2 loader** — receive Stage 2 (`PICO1284.COM` or `PICO1284.EXE`) into memory, hand off

Target size: ≤8 KB blob (well within Stage 0's `MAX_STAGE1_SIZE = 50000`).

### Open decisions

- **Should Stage 1 also handle the PS/2 dual-lane case (§17), or is that exclusively Stage 0 + Stage 2 territory?** Probably Stage 1 inherits whatever channels Stage 0 negotiated (via `DX` kind code), but only really *uses* LPT — the PS/2 lane lives quiet until Stage 2 wants the fallback.

---

## 4. `dos/pico1284/`

### Purpose

Production DOS client per `design.md` §21.1. Either a `.COM`, `.EXE`, or TSR depending on use case. Per `design.md` §21.2, it has modules:

```
port_detect.asm/c      detect LPT base, ECP/EPP capability
ieee1284.asm/c         negotiation and mode setup
packet.c               SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX framing
crc.asm/c              CRC-16 and CRC-32
ps2_i8042.asm/c        Stage 0 and fallback control
screen_text.c          text-mode capture and diff
screen_vesa.c          VBE mode query and graphics capture
compress_rle.asm/c     RLE and Delta+RLE
file_xfer.c            file transfer protocol
console.c              console stream and dictionary compression
tsr.c                  resident command handler if used
```

### Directory layout (target state)

```
dos/pico1284/
├── pico1284.asm                    current 62-byte .COM stub
├── wmakefile                       Open Watcom build
├── src/
│   ├── port_detect.{asm,c}
│   ├── ieee1284.{asm,c}
│   ├── packet.c
│   ├── crc.{asm,c}
│   ├── ps2_i8042.{asm,c}
│   ├── screen_text.c
│   ├── screen_vesa.c
│   ├── compress_rle.{asm,c}
│   ├── file_xfer.c
│   ├── console.c
│   └── tsr.c
├── include/                        local headers
└── (output: dos/build/PICO1284.EXE)
```

### Build pipeline (when Watcom is installed)

NASM produces `.obj`; `wcc` produces `.obj`; `wlink` links into MZ `.EXE` (or `.COM` for small-model). Top-level `dos/Makefile` invokes `wmake` in `dos/pico1284/`.

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
├── delock-fixture/                 libusb client for PL2305-based DeLock adapter
│   ├── Cargo.toml                  (Rust, or maybe Python — decide per tool)
│   └── src/main.rs
├── packet-dissector/               Wireshark Lua dissector for the SOH-framed protocol
│   └── vintage_kvm.lua
├── capture-replay/                 PS/2 line capture/replay for debugging
│   └── …
└── bring-up/                       hardware bring-up scripts (probe board, check pin continuity)
    └── …
```

### Build pipeline

Each tool is independently buildable. Don't add tools/ to the Cargo workspace unless a tool grows enough to deserve it.

### Plan per tool

| Tool | Purpose | Priority |
|---|---|---|
| `delock-fixture` | Mac/Linux libusb client claiming PL2305 interface A0/A1 to act as a fake DOS LPT host for testing Pico Stage 0 firmware in Compat/Nibble modes | Phase 3 — first one worth building |
| `packet-dissector` | Wireshark Lua dissector for the SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX format so packet captures over USB CDC are readable | Phase 4+ |
| `capture-replay` | Logic-analyzer-style capture of PS/2 CLK/DATA lines for protocol debugging (Saleae-compatible format) | Phase 1–2 if PS/2 timing issues appear |
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
2. **Phase 1 PS/2 keyboard endpoint** (`firmware/ps2/`). Write `ps2_at_dev.pio`, port `No0ne/ps2x2pico`'s state machine to Rust. Test against a real DOS PC's keyboard port.
3. **Phase 1 DEBUG injection** — Pico types a fixed hex sequence that produces a tiny test `.COM` on the DOS PC; verify the `.COM` runs.

That's a 3-step demonstrable milestone with no `host/` or Watcom dependency.

### Parallel-track work that doesn't need Feather boards

- **`hardware/` schematic** — produce a KiCad schematic PDF mirroring `hardware_reference.md`. No physical hardware needed.
- **`host/` language decision** — confirm Rust, scaffold workspace member with a stub `main.rs` that opens a CDC port. No firmware-side dependency.
- **`tools/delock-fixture`** — if the DeLock adapter is in hand, prototype the libusb claim sequence. Useful for Phase 3 testing.
- **`dos/common/`** — design (but don't yet create) the protocol constants header. Worth specifying ahead of Stage 1 work.

### Critical path (longest dependency chain)

```
Feather boards arrive
   │
   ▼
firmware Phase 0 verified
   │
   ▼
firmware Phase 1 PS/2 keyboard endpoint
   │
   ▼  ┌──────────────────────────────────┐
       │ dos/stage0/s0_at.asm + private  │
       │ mode handshake                   │
       └──────────┬───────────────────────┘
                  │
                  ▼  ┌──────────────────────────────────┐
                     │ firmware Phase 3 IEEE 1284       │
                     │ Compatibility mode               │
                     └──────────┬───────────────────────┘
                                │
                                ▼  ┌──────────────────────────────────┐
                                   │ dos/stage1/ real loader          │
                                   │ + firmware/packet/ + session/    │
                                   └──────────┬───────────────────────┘
                                              │
                                              ▼
                                   ┌──────────────────────────────────┐
                                   │ host/ scaffolded, file xfer demo │
                                   └──────────────────────────────────┘
                                              │
                                              ▼  (Watcom installed)
                                   ┌──────────────────────────────────┐
                                   │ dos/pico1284/ initial modules    │
                                   └──────────────────────────────────┘
                                              │
                                              ▼
                                   ┌──────────────────────────────────┐
                                   │ Screen dump (Phases 7–9)         │
                                   └──────────────────────────────────┘
```

Time-estimate-honest: this is a 6–12 month project at hobby cadence. The first demoable end-to-end win (Pico types DEBUG, DOS runs `.COM`) is reachable in a few weekends after the boards land.

---

## Open decisions summary

| # | Decision | Owner | Needed by |
|---|---|---|---|
| 1 | `host/` language — confirm Rust? | User | Phase 6 (file transfer); ideally now to scaffold |
| 2 | PS/2 mode selection: config GPIO vs persisted flash flag vs two firmware builds | Implementation | Phase 1 |
| 3 | Open Watcom V2 installation method on macOS (source build / prebuilt / Linux VM) | User | Phase 6 |
| 4 | Single source of truth for protocol constants (hand-maintained / code-gen / drift-tolerant) | Implementation | Phase 4 |
| 5 | `dos/pico1284/` packaging: `.COM` small-model vs `.EXE` small-model vs `.EXE` compact/large | Implementation | Phase 10 |
| 6 | UI for `host/`: pure CLI, TUI (`ratatui`), or GUI | User | Phase 7+ |
| 7 | KiCad project scope: schematic-only or schematic + carrier-board PCB | User | Hardware track, any time |
| 8 | Stage 0 hand-off contract: extend `DX` kind code, or use additional registers? | Implementation | Phase 2 |
