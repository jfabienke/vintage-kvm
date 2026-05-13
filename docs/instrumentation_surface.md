# Instrumentation surface

End-to-end specification of how the Pico firmware surfaces instrumentation data and how operators consume it: console output formats, the TUI dashboard, the CDC telemetry protocol, and the signature database that powers fingerprinting.

Companion to [`pico_firmware_design.md` §5.8 USB CDC bridge](pico_firmware_design.md) and [`pio_state_machines_design.md` §12.4](pio_state_machines_design.md), which described the underlying capture; this document specifies the operator-facing surface.

---

## 1. Purpose and scope

The firmware captures rich instrumentation from the PS/2 oversampler (bit-period histograms, glitch counts, edge skew) and the LPT PIO pipeline (mode state, packet rate, CRC sniffer accumulator). This document specifies **how that data leaves the chip and reaches the operator**:

1. **defmt-RTT** — primary dev surface. Structured line-oriented output via `probe-rs run`. Used during bring-up and bench debugging.
2. **USB CDC telemetry channel** — production surface. Newline-delimited JSON events on the second CDC interface, suitable for live tools (TUI, log aggregators) and offline analysis.
3. **TUI dashboard** — host-side viewer (separate Rust crate, `tools/tui/`) that consumes the CDC telemetry stream and renders a live multi-panel terminal UI.

### Scope of this document

Covers:
- Console line formats (defmt-RTT and equivalent CDC text output) for every event type.
- TUI dashboard layout (all views, with full mocks).
- CDC telemetry JSON-line protocol (event taxonomy, schema, cadence, backpressure).
- Signature database format and match algorithm.
- Cross-references to which firmware module emits which event.

Does *not* cover:
- The PIO programs that capture the data ([`pio_state_machines_design.md`](pio_state_machines_design.md)).
- The CRC sniffer wiring ([`pio_state_machines_design.md` §11.5](pio_state_machines_design.md)).
- The classifier state machine ([`pio_state_machines_design.md` §8](pio_state_machines_design.md)).

### Relationship to other docs

| Document | Relationship |
|---|---|
| [`pico_firmware_design.md`](pico_firmware_design.md) | §5.8 (USB CDC) and §5.10 (Telemetry) — this doc is the detailed surface spec. |
| [`pio_state_machines_design.md`](pio_state_machines_design.md) | Captures what the oversampler / demodulator produce; this doc captures how it's rendered. |
| Memory `ps2-oversampling-preference` | Architectural decision behind the instrumentation; this doc is the realization. |

---

## 2. Two-tier output surface

```
                   Pico firmware
                        │
                        ├── defmt-RTT  ─────────┐
                        │   (compile-time text)  │
                        │                         │
                        └── CDC telemetry  ──┬───┤
                            (runtime JSON)   │   │
                                              │   │
                  ┌───────────────────────────┘   │
                  ▼                                ▼
            Tools / tui                    probe-rs serial monitor
            (parsed JSON,                  (raw text, dev-only)
             interactive TUI)
```

**defmt-RTT** is fast (the firmware writes interned format-string IDs, not bytes), zero-overhead when no probe is attached, and ideal for development. It's text-oriented and human-readable directly.

**CDC telemetry** is the production surface: it survives without a probe, can be piped to any host-side consumer, and uses a structured (JSON) format so tools can parse without text-scraping.

The two surfaces emit the same **content** but in different **encodings**. Where this document shows a console line, that line appears as-is via defmt-RTT and is equivalently emitted as a JSON event over CDC.

---

## 3. Console line formats

All lines below appear via defmt-RTT during dev. The CDC telemetry channel emits the same events as JSON; see §5 for the JSON schema.

### 3.1 Boot and lifecycle events

One line per significant state transition. Timestamps are seconds since boot, three-decimal precision.

```
[ 0.000] vintage-kvm firmware v0.3.0; phase=3
[ 0.001] PIO0 LPT (compat-in+nibble-out) ready
[ 0.002] PIO1 PS/2 RX cluster (kbd+aux oversample+demod) ready
[ 0.003] PIO2 TX cluster ready
[ 0.010] PS2 KBD: wire idle (CLK=HI, DATA=HI), waiting for host power
[ 0.012] PS2 AUX: wire idle, waiting for host power
[ 1.847] PS2 KBD: first transition observed — host PSU on
[ 2.078] classifier: Confirmed(At) — 3 consecutive matches
[ 2.079] selecting bootstrap: S0_AT.COM (AT-class Stage 0, 1635 B)
```

Format conventions:
- **`[s.mmm]`** — timestamp prefix, fixed width 9 characters including space and brackets.
- **First token** is the **channel** (`PS2 KBD`, `PS2 AUX`, `LPT`) or a system label (`classifier:`, `vintage-kvm firmware`).
- **Second segment** is the **event**, terse imperative or noun phrase.
- **Indented continuation** (two spaces, no timestamp) for related sub-data on the next line.

### 3.2 Per-frame data (PS/2)

Issued from the demodulator path. One line per accepted frame, plus one continuation line with timing summary.

```
[ 1.892] PS2 KBD: frame data=0xAA parity=OK 11-bit AT framing
[ 1.892]   bit periods (µs): 82 81 82 81 82 81 82 81 82 81 82
[ 1.892]   p50=81 p99=82 duty=52% skew=+0.3µs glitches=0
```

Per-frame logging is **disabled by default** in production builds (volume is too high — 13 lines/sec under heavy typing). Enabled via the CDC command `?trace=ps2_kbd` or by setting `DEFMT_LOG=trace` at build time.

### 3.3 Periodic summary (every 1 s)

One line per channel, fixed columnar layout. **The default at-a-glance status.**

```
[ 5.000] KBD: 12 fr/s, 0 err, 0 glt, p50=81µs p95=82µs p99=83µs dty=52% skw=+0.3µs
[ 5.000] AUX: 0 fr/s, idle
[ 5.000] LPT: mode=SPP-Nibble, 0 pkt/s, no traffic
```

Field positions are deliberately aligned across consecutive samples so the eye spots drifts. Field abbreviations:

| Abbrev | Field | Units |
|---|---|---|
| `fr/s` | frame rate | frames/sec |
| `err` | parity + framing errors in window | count |
| `glt` | glitches in window | count |
| `p50`, `p95`, `p99` | percentiles of bit period | µs |
| `dty` | CLK duty cycle (high time / total) | % |
| `skw` | CLK→DATA edge skew, signed | µs |
| `pkt/s` | packet rate (LPT) | packets/sec |
| `mode` | active LPT mode | enum |

### 3.4 Anomaly events (interjected)

Tagged with `▲` prefix for easy grepping. Two lines: the event, then context.

```
[14.873] KBD ▲ anomaly: glitch detected at bit 4 of frame data=0x2C
[14.873]   CLK pulse 2.1µs (threshold 4µs); inserted spurious 1 in DATA stream
[14.873]   recovered: parity check would fail, frame dropped
[14.874] KBD: 1 glitch/min (was 0/min last 5min); investigate cable / pull-ups

[28.412] KBD ▲ anomaly: p99 drift +4µs (was 83µs, now 87µs)
[28.412]   last 100 frames: p50=83µs p95=86µs p99=87µs
[28.412]   possible causes: thermal drift, marginal pull-up, BIOS keyboard polling change
```

Conventions:
- The `▲` symbol is the canonical anomaly marker. Operators grep for it (`▲` is U+25B2, distinctive vs ASCII art elsewhere).
- Anomaly lines include a **suggested investigation** when one is known. Helps an operator triage without consulting docs.

### 3.5 Fingerprint dump (on demand)

Triggered by the CDC command `?fingerprint` or automatically once classifier confidence ≥ 0.95. Rich, structured, copy-pasteable into an issue report.

```
================================================================================
                          PS/2 KEYBOARD FINGERPRINT
================================================================================

  Classification
    Machine class:           AT (Confirmed)
    Confidence:              0.97 (47 consistent frames, 0 conflicts)
    Decision basis:          11-bit framing + odd parity + valid stop

  Wire timing
    Effective bit rate:      12.3 kHz
    Bit period (µs):
        p50  =  81
        p95  =  82
        p99  =  83
        max  =  85
        min  =  79
        σ    =  0.8
    CLK duty cycle:          52% high / 48% low
    CLK rise time:           < 1.0 µs  (limited by oversample resolution)
    CLK fall time:           < 1.0 µs
    CLK ↔ DATA edge skew:   +0.3 µs  (DATA settles AFTER CLK falling)

  Bit-period histogram (last 1000 frames, 11000 bit samples)
        60 µs  │
        65 µs  │
        70 µs  │
        75 µs  │▏
        80 µs  │██████████████████████████████████████  ◄── p50 = 81 µs
        85 µs  │██████▏
        90 µs  │
        95 µs  │
       100 µs  │
       >100 µs │

  Electrical quality
    Glitches/sec:            0.00  (CLK transitions <4µs in last 60s: 0)
    Parity errors:           0     (in 472 device→host frames)
    Framing errors:          0     (stop-bit violations)
    Bus contention events:   0

  Direction balance
    Device→Host frames:      472   (99.8%)
    Host→Device frames:        1   (0.2%)
    Host inhibit (CLK LO):
        average duration:    142 µs
        observed range:      138 – 147 µs
    Device ACK delay:        18 µs

  Reset / BAT timing
    Host reset (0xFF):       observed at t = +0.043s
    Device BAT (0xAA):       observed at t = +1.847s
    BAT response latency:    1.804s  (within AT spec 0.5–2.0s)

  Closest match (signature database, 12 entries)
    1.  IBM Model M (1391401, 1986–1996)        Δ = 0.05
        p50 81µs   duty 52%   skew +0.3µs   inh 140µs
    2.  IBM Model F XT/AT (1981–1985)           Δ = 0.34
        p50 96µs   duty 50%   skew +0.5µs   inh 220µs
    3.  Northgate OmniKey 102 (~1991)           Δ = 0.41
        p50 84µs   duty 55%   skew +0.1µs   inh 155µs

  Fingerprint hash:          a7f3-9c12-8e44-2b91

================================================================================
                          HOST CONTROLLER FINGERPRINT
================================================================================

  Detected from:             host-initiated traffic, BAT sequence, inhibit pattern

  Inhibit profile
    Mean duration:           142 µs
    Style:                   short-inhibit (i8042-style, not XT-BIOS busy-wait)
    Cmd→ACK delay:           18 µs (Pico-measured, before our reply)
    Polling cadence:         none observed (interrupt-driven KBC)

  Reset behavior
    Issues keyboard reset:   YES  (0xFF on bootstrap)
    Awaits 0xAA reply:       YES  (≤ 2.5s timeout estimated)
    Issues SET_LEDS probe:   YES  (LED pattern observed at t = +2.4s)

  Closest match
    1.  Generic AT i8042 (early 90s clones)     Δ = 0.12
    2.  Intel SuperIO (mid-90s)                 Δ = 0.27
    3.  IBM PS/2 Model 50 KBC                   Δ = 0.55

================================================================================
```

Conventions:
- Total width 80 columns so the dump fits any standard terminal.
- Double-rule (`====`) section breaks; single-rule subsections not used (the indented `Classification`, `Wire timing` etc. labels are enough).
- Histograms use Unicode block characters (`█▏`); 8th-block resolution gives smooth ramps.
- **Δ (delta)** is the L2 distance over the normalized feature vector (§6.3).
- **Fingerprint hash** is a stable hash of the rounded feature vector. Two runs against the same hardware produce identical hashes; ideal as an issue-report identifier.

### 3.6 Stage 2 download progress

Issued by the block server during an active download. One line per phase; throughput line every 5 seconds while in progress.

```
[ 5.234] LPT: Stage 2 download begin (size=49 B, CRC-32=0x9A4F12C8 expected)
[ 5.234]   block 0/1 sent (49 B; last block short)
[ 5.247]   block 0 ACK received
[ 5.247]   running CRC-32 (sniffer): 0x9A4F12C8
[ 5.247] LPT: Stage 2 download OK — CRC match, image complete
```

For large images, intermediate progress at 5 s cadence:

```
[15.000] LPT: Stage 2 download progress 43/812 blocks (5.3%), 2.3 KB/s, ETA 5m 18s
[20.000] LPT: Stage 2 download progress 121/812 blocks (14.9%), 2.4 KB/s, ETA 4m 49s
```

### 3.7 Compact monitor mode

Single-line-per-channel display that refreshes in place via `\r`. Used when the operator runs `picotool serial-monitor --compact` and wants minimal scroll.

```
KBD  AT confirmed | 13 fr/s | p50 81µs ±0.8 | dty 52% | skw +0.3µs | err 0 | glt 0 | match: IBM M
AUX  idle (no traffic in 300s)
LPT  SPP-nibble  |  6 pkt/s | last CRC OK (12ms ago) | mode=SERVING_BLOCKS blk 4/N
```

The compact mode is **defmt-RTT only** — CDC telemetry doesn't carry in-place updates; the TUI takes over that role for structured live display.

---

## 4. TUI dashboard

Host-side viewer at `tools/tui/`. Built with `ratatui`; consumes the CDC telemetry stream (§5) and renders five views.

### 4.1 View navigation

| Key | View |
|---|---|
| `1` | Overview (default landing) |
| `2` | PS/2 KBD detail |
| `3` | PS/2 AUX detail |
| `4` | LPT detail |
| `5` | Events log |
| `6` | Hardware status |
| `7` | Fingerprint dump |
| `Esc` | Back to overview |
| `q` | Quit |
| `/` | Filter (events log only) |
| `?` | Help overlay |

Per-view action keys appear in the bottom status bar.

### 4.2 View 1 — Overview

The Overview is built around the **two-plane transport** model from [`two_plane_transport.md`](two_plane_transport.md): a top `PLANES` band shows the two logical channels (CONTROL, DATA) with current health and throughput; binding arrows point down to the `PORTS` band, which shows the three physical interfaces (PS/2 KBD, PS/2 AUX, LPT) with per-port detail. The arrow style indicates binding state: `│` solid = active binding, `┊` dashed = planned binding, no arrow = port unbound from any plane.

This visualization makes the same dashboard render the project's full lifecycle — Phase 3 bootstrap, steady-state DP_ACTIVE, PS/2 dual-lane fallback, and XT single-plane — without changing layout.

#### State A — Phase 3 bootstrap (control plane not yet active)

```
┌─ vintage-kvm 0.3.0 ──────────────────────── uptime 00:14:23 ────── [F1] help  [q] quit ─┐
│ Session: ServingBlocks ▸ block 43/49                                                     │
│ Host: AT-class ✓   Keyboard: IBM Model M (Δ=0.05)   Chipset: i8042 SIO (Δ=0.12)         │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│                                                                                          │
│   PLANES     CONTROL ○ idle                          DATA  ● 2.3 KB/s                    │
│              (awaiting Stage 2)                      (Stage 2 download)                  │
│                    ┊ planned                                │ active                     │
│                    ▼                                        ▼                            │
│   PORTS  ┌─ PS/2 KBD ●─────┐  ┌─ PS/2 AUX ○─────┐  ┌─ LPT IEEE 1284 ●───────────────┐  │
│          │ 13 fr/s, AT      │  │ idle             │  │ mode: SPP-Nibble               │  │
│          │ p99 83 µs        │  │                  │  │ ████████████████░░░░ 87 %      │  │
│          │ dty 52% skw +0.3 │  │                  │  │ block 43 / 49                  │  │
│          │ err 0  glt 0 ▲ 1 │  │ err 0  glt 0     │  │ throughput 2.3 KB/s             │  │
│          │ IBM Model M      │  │                  │  │ CRC accum 0x4B8AE9CF            │  │
│          └──────────────────┘  └──────────────────┘  └────────────────────────────────┘  │
│                                                                                          │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ Events (live, last 7)                                                                    │
│  14:22:12  LPT  blk 42 ACK; CRC accumulator OK                                           │
│  14:22:13  LPT  blk 43 SENT                                                              │
│  14:22:18  LPT  blk 43 ACK                                                               │
│  14:22:18  LPT  blk 44 SENT                                                              │
│  14:22:23 ▲ KBD  glitch at bit 4 of frame 0x2C  (CLK pulse 2.1 µs; frame dropped)        │
│  14:22:23  LPT  blk 44 ACK                                                               │
│  14:22:24  LPT  blk 45 SENT                                                              │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ [1]Overview  [2]KBD  [3]AUX  [4]LPT  [5]Events  [6]Hardware  [7]Fingerprint  [/]filter   │
│ NeoPixel ● magenta (SERVE_STAGE2_DOWNLOAD)             CDC: bridged → /dev/ttyACM0       │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- CONTROL is `○ idle` because no Stage 2 is running yet — the i8042 private channel hasn't been established. Dashed arrow (`┊`) points at PS/2 KBD because that's where the control plane *will* land once Stage 2 takes over.
- DATA is `● 2.3 KB/s` active, carrying the Stage 2 download over LPT SPP-Nibble. Solid arrow (`│`) shows the live binding.

#### State B — DP_ACTIVE (steady state, both planes live)

Once Stage 2 is running, the i8042 private channel comes up on PS/2 KBD and LPT promotes to its negotiated high-speed mode.

```
│   PLANES     CONTROL ● 12 cmd/s active               DATA  ● 1.4 MB/s active             │
│              (PS/2 KBD private)                      (ECP-DMA reverse)                   │
│                    │                                        │                            │
│                    ▼                                        ▼                            │
│   PORTS  ┌─ PS/2 KBD ●─────┐  ┌─ PS/2 AUX ○─────┐  ┌─ LPT IEEE 1284 ●───────────────┐  │
│          │ 12 cmd/s         │  │ idle             │  │ mode: ECP-DMA reverse          │  │
│          │ private byte pump│  │                  │  │ 1.4 MB/s    ETA 2.3 s          │  │
│          │ latency 18 ms    │  │                  │  │ stream 1: screen frame         │  │
│          │ err 0  drops 0   │  │ err 0  glt 0     │  │ DMA ch 7 ACTIVE                │  │
│          │ IBM Model M      │  │                  │  │ 0 CRC err                      │  │
│          └──────────────────┘  └──────────────────┘  └────────────────────────────────┘  │
```

Both arrows are solid; control on PS/2 KBD, data on LPT. AUX still idle. This is the "happy path" the project targets.

#### State C — PS/2 dual-lane fallback (LPT down, both planes on PS/2)

If LPT negotiation fails or Stage 0 came up Mode B (PS/2-only handoff), both planes degrade onto PS/2 wires.

```
│   PLANES     CONTROL ● 3 cmd/s degraded              DATA  ● 9 KB/s fallback             │
│              (PS/2 KBD private)                      (PS/2 AUX private)                  │
│                    │                                        │                            │
│                    ▼                                        ▼                            │
│   PORTS  ┌─ PS/2 KBD ●─────┐  ┌─ PS/2 AUX ●─────┐  ┌─ LPT IEEE 1284 ✗───────────────┐  │
│          │ 3 cmd/s          │  │ 9 KB/s           │  │ unavailable                    │  │
│          │ private byte pump│  │ private byte pump│  │ Mode B: no LPT base at         │  │
│          │ latency 95 ms    │  │ ETA 8 min for    │  │ handoff (Stage 0 brought up    │  │
│          │                  │  │ 80×25 text frame │  │ PS/2 only)                     │  │
│          │ err 0            │  │ err 0            │  │ probe attempts: 0              │  │
│          └──────────────────┘  └──────────────────┘  └────────────────────────────────┘  │
```

The `✗` indicator on the LPT box replaces the `●`/`○` to mark it as not-available rather than merely idle. Useful to distinguish "no traffic right now" from "this port can't carry traffic."

#### State D — XT single-plane (no concurrent PS/2 control)

Per [`two_plane_transport.md`](two_plane_transport.md), XT-class machines have no concurrent PS/2 control plane — the keyboard wire is bootstrap-only, and LPT carries everything afterward.

```
│   PLANES     CONTROL  -  (XT class; no concurrent     DATA  ● 8 KB/s                     │
│              PS/2 control plane available)            (LPT SPP, multiplexed)             │
│                                                              │                           │
│                                                              ▼                           │
│   PORTS  ┌─ PS/2 KBD ●─────┐  ┌─ PS/2 AUX ○─────┐  ┌─ LPT IEEE 1284 ●───────────────┐  │
│          │ Stage -1 only    │  │ idle             │  │ mode: SPP                      │  │
│          │ (DEBUG inject)   │  │                  │  │ 8 KB/s                         │  │
│          │ post-bootstrap:  │  │                  │  │ control + data multiplexed     │  │
│          │ silent           │  │                  │  │ 0 CRC err                      │  │
│          │ IBM Model F XT   │  │                  │  │                                │  │
│          └──────────────────┘  └──────────────────┘  └────────────────────────────────┘  │
```

The CONTROL plane displays a literal `-` rather than `○`/`●` to indicate "not applicable in this configuration." No arrow from CONTROL.

#### Topology rendering rules (codified)

| Plane indicator | Meaning |
|---|---|
| `● <rate>` + label | Plane active; rate and short status follow. |
| `○ idle` | Plane defined for this configuration but not currently active. |
| `-` (literal dash) | Plane not applicable for this host class (e.g. XT). |

| Arrow style | Meaning |
|---|---|
| `│ active` (or just `│`) | Plane currently bound to this port; traffic flowing. |
| `┊ planned` | Plane will bind here once a prerequisite completes (e.g. Stage 2 boot). |
| (no arrow) | Plane not bound to any port at the moment. |

| Port indicator | Meaning |
|---|---|
| `●` | Port up; traffic observed. |
| `○` | Port up; idle. |
| `✗` | Port unavailable (hardware not present, negotiation failed, etc.). |

These rules combine to give a one-glance read of "what's healthy, what's planned, what's broken" without consulting any other view.

### 4.3 View 2 — KBD detail

```
┌─ vintage-kvm › KBD ────────────────────────────────────────────── [Esc] back  [q] quit ┐
│ ● AT  Confirmed since 2026-05-13 14:08:17 (14 min ago)  Confidence 0.97                 │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ ╭─ Live frames ─────────────────────────╮ ╭─ Per-frame bit timing (latest) ─────────────╮│
│ │ 14:22:23.412  0x2C  parity OK         │ │   start  ┃        ┃          ◀ falling edge ││
│ │ 14:22:23.398  0x2C  ▲ glitch  drop    │ │   bit 0  ┃        ┃     81 µs               ││
│ │ 14:22:23.305  0x4E  parity OK         │ │   bit 1  ┃        ┃     82 µs               ││
│ │ 14:22:23.221  0x4E  parity OK         │ │   bit 2  ┃        ┃     81 µs               ││
│ │ 14:22:23.139  0xAA  parity OK         │ │   bit 3  ┃        ┃     81 µs               ││
│ │ 14:22:22.998  0x4E  parity OK         │ │   bit 4  ┃        ┃     82 µs               ││
│ │ 14:22:22.815  0x4E  parity OK         │ │   bit 5  ┃        ┃     81 µs               ││
│ │ 14:22:22.633  0xAA  parity OK         │ │   bit 6  ┃        ┃     81 µs               ││
│ │ ↓ scroll  470 more in ring             │ │   bit 7  ┃        ┃     82 µs               ││
│ ╰────────────────────────────────────────╯ │   par    ┃        ┃     81 µs   parity=0   ││
│                                            │   stop   ┃        ┃     82 µs               ││
│ ╭─ Rolling stats (60 s window) ────────╮  │                                              ││
│ │ frames           472                   │  │   total frame: 813 µs    skew +0.3 µs       ││
│ │ frames/s         13.1                  │  ╰──────────────────────────────────────────────╯│
│ │ parity errors    0                     │                                                  │
│ │ framing errors   0                     │  ╭─ Bit-period histogram (last 1000 frames) ──╮  │
│ │ glitches         1                     │  │  75 µs  ▏                                   │  │
│ │ p50 / p95 / p99  81 / 82 / 83 µs       │  │  80 µs  ████████████████████████▏ p50 81 µs │  │
│ │ σ (std-dev)      0.8 µs                │  │  85 µs  █████▏                               │  │
│ │ duty (CLK HI)    52 %                  │  │  90 µs  ▏                                   │  │
│ │ skew CLK→DATA    +0.3 µs               │  │  95 µs                                       │  │
│ │ inhibit avg      142 µs (host→dev)     │  │ ≥100 µs                                      │  │
│ ╰────────────────────────────────────────╯  ╰──────────────────────────────────────────────╯│
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ [Esc] back  [s] save snapshot  [r] reset window  [d] dump JSON                           │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

### 4.4 View 4 — LPT detail

```
┌─ vintage-kvm › LPT ────────────────────────────────────────────── [Esc] back  [q] quit ┐
│ ● SPP-Nibble  (negotiated 14 min ago; ECP/EPP probes declined → fallback)                │
│ Base 0x378   IRQ ? (TBD)   DMA channel ? (TBD)                                           │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ ╭─ PIO state ─────────────────────────────╮ ╭─ DMA + sniffer ────────────────────────────╮│
│ │ PIO 0 SM 0  lpt_compat_in    RUN  pc=4   │ │ ch 2  PIO0_RX → SRAM staging   ACTIVE      ││
│ │ PIO 0 SM 1  lpt_nibble_out   RUN  pc=2   │ │ ch 3  SRAM → PIO0_TX           IDLE        ││
│ │ PIO 0 SM 2  lpt_byte_rev     STALL       │ │ ch 8  Stage 2 CRC-32 sniffer   ACCUM       ││
│ │ PIO 0 SM 3  lpt_epp          STALL       │ │ ch 8.SNIFF_DATA  0x4B8AE9CF                ││
│ │ PIO 2 SM 0  lpt_ecp_fwd_dma  STALL       │ │ ch 8.bytes_sniffed  2752                   ││
│ │ PIO 2 SM 1  lpt_ecp_rev_dma  STALL       │ │                                            ││
│ ╰──────────────────────────────────────────╯ ╰────────────────────────────────────────────╯│
│ ╭─ Mode ladder (last attempt) ────────────╮ ╭─ Pin levels (snapshot 14:22:23.500) ───────╮│
│ │ ECP   declined  (xflag 0x14 rejected)    │ │ nInit/HostStrobe   GP11  H ─┐              ││
│ │ EPP   declined  (xflag 0x40 rejected)    │ │ D0-D7              GP12-19  -- 0x00        ││
│ │ Byte  declined  (xflag 0x01 rejected)    │ │ nAutoFd            GP20  H                 ││
│ │ SPP   accepted  (current mode)           │ │ nSelectIn          GP22  H                 ││
│ │                                          │ │ nAck      (out)    GP23  L                 ││
│ │ Caps detected:                           │ │ Busy/phase (out)   GP24  L  ← persistent   ││
│ │   SPP  ✓   EPP  ✗   ECP  ✗   ECP-DMA ✗  │ │ PError    (out)    GP25  L                 ││
│ │ (Pico-side; will accept all in Phase 4)  │ │ Select    (out)    GP26  L                 ││
│ │                                          │ │ nFault    (out)    GP27  L                 ││
│ ╰──────────────────────────────────────────╯ ╰────────────────────────────────────────────╯│
│ ╭─ Stage 2 download progress ──────────────────────────────────────────────────────────╮  │
│ │                                                                                       │  │
│ │  ███████████████████████████████████████████████████████░░░░░░░░░░░░░░  87 %         │  │
│ │  block 43 / 49      throughput 2.3 KB/s      ETA 8 sec     retries this run: 0       │  │
│ │                                                                                       │  │
│ │  Running CRC-32 (sniffer): 0x4B8AE9CF                                                │  │
│ │  Expected (from CAP_RSP):  0x9A4F12C8                                                │  │
│ │  Match-on-complete? ──── TBD until last block                                        │  │
│ ╰───────────────────────────────────────────────────────────────────────────────────────╯  │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ [Esc] back   [p] pause download   [r] re-negotiate   [d] dump packet log                 │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

### 4.5 View 7 — Fingerprint dump

```
┌─ vintage-kvm › Fingerprint ────────────────────────────────────── [Esc] back  [q] quit ┐
│ Saved snapshot at 14:22:23.000     hash: a7f3-9c12-8e44-2b91                             │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│  ╭─ Keyboard signature ──────────────────────────╮ ╭─ Database match ───────────────────╮ │
│  │ Class                    AT (Confirmed)         │ │ 1.  IBM Model M (1391401)         │ │
│  │ Confidence               0.97 / 1.00            │ │     1986-1996       Δ = 0.05      │ │
│  │ Frames observed          472                    │ │     p50 81µs  dty 52%  skw +0.3   │ │
│  │ Errors                   0 parity, 0 framing    │ │                                    │ │
│  │ Wire bit rate            12.3 kHz               │ │ 2.  IBM Model F XT/AT             │ │
│  │ Bit period      p50 81 µs   p99 83 µs   σ 0.8  │ │     1981-1985       Δ = 0.34      │ │
│  │ CLK duty                52 % high               │ │     p50 96µs  dty 50%  skw +0.5   │ │
│  │ CLK ↔ DATA skew         +0.3 µs (DATA after)   │ │                                    │ │
│  │ CLK rise / fall         < 1 µs / < 1 µs         │ │ 3.  Northgate OmniKey 102         │ │
│  │ Glitches / minute       0.2                     │ │     ~1991           Δ = 0.41      │ │
│  ╰────────────────────────────────────────────────╯ ╰────────────────────────────────────╯ │
│                                                                                            │
│  ╭─ Host controller signature ────────────────────╮ ╭─ Reset sequence ───────────────────╮ │
│  │ Inhibit duration         142 µs avg             │ │ t=  0.000  power on               │ │
│  │ Inhibit style            short  (i8042-like)    │ │ t= +0.043  host issues 0xFF       │ │
│  │ CMD → ACK delay          18 µs                  │ │ t= +1.847  device BAT 0xAA        │ │
│  │ Polling cadence          none (interrupt-drv)   │ │ t= +2.412  host LED probe         │ │
│  │ Issues 0xFF reset        yes                    │ │ t= +2.451  device 0xFA ACK        │ │
│  │ Awaits 0xAA              yes                    │ │            BAT latency 1.804s ✓    │ │
│  ╰────────────────────────────────────────────────╯ ╰────────────────────────────────────╯ │
│                                                                                            │
│  ╭─ Closest chipset match ────────────────────────────────────────────────────────────╮   │
│  │  1.  Generic AT i8042  (early-90s clones)     Δ = 0.12                              │   │
│  │  2.  Intel SuperIO     (mid-90s)              Δ = 0.27                              │   │
│  │  3.  IBM PS/2 Model 50 KBC                    Δ = 0.55                              │   │
│  ╰─────────────────────────────────────────────────────────────────────────────────────╯   │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ [Esc] back  [s] save .json  [c] copy to clipboard  [u] upload to project DB              │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

### 4.6 View 6 — Hardware status

```
┌─ vintage-kvm › Hardware ───────────────────────────────────────── [Esc] back  [q] quit ┐
├──────────────────────────────────────────────────────────────────────────────────────────┤
│  ╭─ PIO blocks ──────────────────────────────────────────────────────────────────────╮  │
│  │                                                                                    │  │
│  │  PIO 0  (LPT, 4/4 SMs, 30/32 instr slots)                                          │  │
│  │    SM 0  lpt_compat_in   ▮▮▮▯  RUN   div 1.00     RX FIFO ▮▮▮▯  DMA ch 2          │  │
│  │    SM 1  lpt_nibble_out  ▮▮▮▯  RUN   div 150.00   TX FIFO ▯▯▯▯  DMA ch 3          │  │
│  │    SM 2  lpt_byte_rev    ▯▯▯▯  STALL div 3.00     (Phase 4)                       │  │
│  │    SM 3  lpt_epp         ▯▯▯▯  STALL div 5.00     (Phase 5)                       │  │
│  │                                                                                    │  │
│  │  PIO 1  (PS/2 RX cluster, 4/4 SMs, 14/32 instr slots)                              │  │
│  │    SM 0  kbd_oversample  ▮▮▮▮  RUN   div 150.00   RX FIFO ▮▮▮▮  DMA ch 0  ping    │  │
│  │    SM 1  kbd_demod       ▮▮▮▯  RUN   div 150.00   RX FIFO ▮▯▯▯  DMA ch 1          │  │
│  │    SM 2  aux_oversample  ▮▯▯▯  RUN   div 150.00   RX FIFO ▯▯▯▯  DMA ch 4  ping    │  │
│  │    SM 3  aux_demod       ▮▯▯▯  RUN   div 150.00   RX FIFO ▯▯▯▯  DMA ch 5          │  │
│  │                                                                                    │  │
│  │  PIO 2  (TX + status, 3/4 SMs, 26/32 instr slots)                                  │  │
│  │    SM 0  kbd_tx          ▯▯▯▯  IDLE  div 1500.00  TX FIFO ▯▯▯▯                    │  │
│  │    SM 1  aux_tx          ▯▯▯▯  IDLE  div 1500.00  TX FIFO ▯▯▯▯                    │  │
│  │    SM 2  ws2812          ▮▮▮▮  RUN   div 18.67    TX FIFO ▮▯▯▯                    │  │
│  │    SM 3  -               (free)                                                    │  │
│  ╰────────────────────────────────────────────────────────────────────────────────────╯  │
│                                                                                          │
│  ╭─ DMA channels ────────────────────────────╮ ╭─ Sniffer / CRC ────────────────────╮   │
│  │ ch 0  kbd_oversample  ⟳ ring  ACTIVE       │ │ Bound:   ch 8                       │   │
│  │ ch 1  kbd_demod       byte queue  ACTIVE   │ │ Mode:    CRC-32 reflected           │   │
│  │ ch 2  lpt_compat_in   per-packet  IDLE     │ │ Init:    carried from prev block    │   │
│  │ ch 3  lpt_nibble_out  per-packet  IDLE     │ │ Now:     0x4B8AE9CF (after 2752 B)  │   │
│  │ ch 4  aux_oversample  ⟳ ring  ACTIVE       │ │ Target:  0x9A4F12C8                 │   │
│  │ ch 5  aux_demod       byte queue  ACTIVE   │ │                                     │   │
│  │ ch 8  CRC sniffer     ACCUM                │ │                                     │   │
│  │ ch 9-15  unallocated                       │ │                                     │   │
│  ╰────────────────────────────────────────────╯ ╰─────────────────────────────────────╯   │
│                                                                                          │
│  ╭─ Resources ────────────────────────────────────────────────────────────────────────╮ │
│  │ SRAM       ████░░░░░░░░░░░░░░░░░░░░░░░░░░░░  18 / 264 KB                            │ │
│  │ PSRAM      ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   0 / 8192 KB                          │ │
│  │ Flash      █▏░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  16 / 4096 KB                          │ │
│  │ CPU core 0 ██░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   6 %                                  │ │
│  │ CPU core 1 ▏░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   1 %                                  │ │
│  ╰────────────────────────────────────────────────────────────────────────────────────╯ │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ [Esc] back   [r] reset counters   [p] pause sampling                                     │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

### 4.7 Visual conventions

Codified for consistency across views:

| Symbol | Meaning |
|---|---|
| `●` (filled circle) | Active / connected / OK state |
| `○` (hollow circle) | Inactive / idle / no traffic |
| `✗` | Port unavailable (hardware missing, negotiation failed) |
| `-` (literal dash, as plane indicator) | Plane not applicable to this host class (e.g. XT has no CONTROL plane) |
| `│` (vertical bar, in topology view) | Plane→port binding active (traffic flowing) |
| `┊` (dashed vertical, in topology view) | Plane→port binding planned but not yet active |
| `▮` (filled bar) | FIFO depth occupied, resource gauge filled |
| `▯` (hollow bar) | FIFO depth empty, gauge unfilled |
| `█▏` (block + thin) | Histogram bar with sub-cell precision |
| `▲` | Anomaly marker (live event log) |
| `✓` / `✗` | Capability supported / not supported |
| `─┐ ─┘` (corner pieces) | Pin level transitions in the LPT view |
| `╭─ … ─╮` (rounded) | Nested panels (inside the main outer box) |
| `┌─ … ─┐` (square) | Outer view frame |

Color is **not used** as a primary indicator. Some terminals (defmt-RTT, screen recordings) flatten color; symbol shape carries the meaning. ANSI color may be applied opportunistically in `ratatui` for emphasis (green dots, red anomaly markers) but the layout is correct without it.

Width target: **100 columns** for the dashboard. Compact views (§3.7) target 80 columns.

---

## 5. CDC telemetry protocol

The second USB CDC interface on the Pico (CDC 1; see [`pico_firmware_design.md` §5.8](pico_firmware_design.md)) carries a newline-delimited JSON stream from the firmware to any consumer.

### 5.1 Stream format

- **One JSON object per line.** UTF-8, no BOM.
- **No backpressure.** The Pico drops oldest events on CDC TX overflow (see §5.5).
- **Versioning** via top-level `"v"` field. Current version: `1`.

```json
{"v":1,"t":1234.567,"ch":"kbd","kind":"frame","data":174,"parity_ok":true,"timing":{"p50":81,"p99":83,"skew":0.3}}
```

### 5.2 Event taxonomy

Each event has a `"kind"` discriminator. Categories:

#### Lifecycle (low-frequency, < 1/s steady-state)

```
{"v":1,"t":0.000,"kind":"boot","fw_version":"0.3.0","phase":3}
{"v":1,"t":2.078,"kind":"classifier_state","channel":"kbd","new":"Confirmed(At)","confidence":0.97,"consecutive":3}
{"v":1,"t":5.234,"kind":"download_begin","total_blocks":49,"expected_crc32":"0x9A4F12C8","size_bytes":49}
{"v":1,"t":5.247,"kind":"download_complete","crc_match":true,"final_crc32":"0x9A4F12C8","duration_ms":13}
```

#### Per-frame (high-frequency; up to ~30/s under heavy typing)

```
{"v":1,"t":1.892,"kind":"frame","ch":"kbd","data":170,"parity_ok":true,"framing_ok":true,"bit_periods_us":[82,81,82,81,82,81,82,81,82,81,82],"duty_pct":52,"skew_us":0.3,"glitches":0}
```

Per-frame events are **suppressed by default** at the CDC interface (volume) but always emitted via defmt-RTT. Operator enables them with the `?subscribe=frames` command (§5.4).

#### Per-channel periodic stats (1/s)

```
{"v":1,"t":5.000,"kind":"stats","ch":"kbd","window_s":1,"frames":12,"errors":0,"glitches":0,"p50_us":81,"p95_us":82,"p99_us":83,"duty_pct":52,"skew_us":0.3}
{"v":1,"t":5.000,"kind":"stats","ch":"aux","window_s":1,"frames":0,"errors":0,"glitches":0}
{"v":1,"t":5.000,"kind":"stats","ch":"lpt","window_s":1,"mode":"SPP-Nibble","packets":0,"crc_errors":0}
```

#### Plane state (event-driven on transition; also emitted periodically at 1/s)

Drives the Overview's PLANES band — see §4.2. Each event names a plane (`"control"` / `"data"`), the port it's bound to (`"kbd"` / `"aux"` / `"lpt"` / `null`), the binding state, and current throughput.

```
{"v":1,"t":5.000,"kind":"plane","plane":"control","state":"idle_planned","bound_to":"kbd","throughput":null,"detail":"awaiting Stage 2"}
{"v":1,"t":5.000,"kind":"plane","plane":"data","state":"active","bound_to":"lpt","throughput":{"bytes_per_s":2354},"detail":"Stage 2 download"}
```

After Stage 2 lands and `DP_ACTIVE` is reached:

```
{"v":1,"t":62.100,"kind":"plane","plane":"control","state":"active","bound_to":"kbd","throughput":{"cmds_per_s":12},"detail":"PS/2 KBD private"}
{"v":1,"t":62.100,"kind":"plane","plane":"data","state":"active","bound_to":"lpt","throughput":{"bytes_per_s":1468006},"detail":"ECP-DMA reverse"}
```

PS/2 dual-lane fallback (LPT unavailable):

```
{"v":1,"t":62.100,"kind":"plane","plane":"control","state":"degraded","bound_to":"kbd","throughput":{"cmds_per_s":3},"detail":"PS/2 KBD private"}
{"v":1,"t":62.100,"kind":"plane","plane":"data","state":"fallback","bound_to":"aux","throughput":{"bytes_per_s":9216},"detail":"PS/2 AUX private"}
```

XT single-plane (no control plane):

```
{"v":1,"t":62.100,"kind":"plane","plane":"control","state":"not_applicable","bound_to":null,"throughput":null,"detail":"XT class"}
{"v":1,"t":62.100,"kind":"plane","plane":"data","state":"active","bound_to":"lpt","throughput":{"bytes_per_s":8192},"detail":"SPP multiplexed"}
```

`state` values:

| Value | Meaning |
|---|---|
| `active` | Bound to a port and carrying traffic. |
| `idle` | Bound but no traffic in the last window. |
| `idle_planned` | Not yet bound, but binding is planned (`bound_to` names the planned port). |
| `degraded` | Active but operating below normal capability (e.g. degraded latency). |
| `fallback` | Bound to a non-primary port due to primary unavailability. |
| `not_applicable` | This plane does not exist in this host class. |

`bound_to` ∈ `{"kbd", "aux", "lpt", null}`.

`throughput` is plane-specific: `bytes_per_s` for the data plane, `cmds_per_s` for the control plane. Either can be omitted (or `null`) when no traffic is flowing.

#### Anomalies (event-driven; rare)

```
{"v":1,"t":14.873,"kind":"anomaly","ch":"kbd","subtype":"glitch","bit_index":4,"frame_data":44,"pulse_us":2.1,"action":"frame_dropped"}
{"v":1,"t":28.412,"kind":"anomaly","ch":"kbd","subtype":"p99_drift","p99_was_us":83,"p99_now_us":87,"hint":"thermal_drift_or_marginal_pullup"}
```

#### Block download progress (per ACK, during active download)

```
{"v":1,"t":5.247,"kind":"block_ack","block_no":0,"total_blocks":49,"throughput_bps":3692,"running_crc32":"0x9A4F12C8"}
```

#### Fingerprint snapshot (on `?fingerprint` or auto-trigger)

```
{"v":1,"t":14.500,"kind":"fingerprint","ch":"kbd","class":"AT","confidence":0.97,
 "bit_rate_hz":12345,"bit_period_p50_us":81,"bit_period_p99_us":83,"bit_period_sigma_us":0.8,
 "duty_pct":52,"skew_us":0.3,"glitches_per_min":0.2,
 "matches":[
   {"name":"IBM Model M (1391401)","era":"1986-1996","delta":0.05},
   {"name":"IBM Model F XT/AT","era":"1981-1985","delta":0.34},
   {"name":"Northgate OmniKey 102","era":"~1991","delta":0.41}
 ],
 "hash":"a7f3-9c12-8e44-2b91"}
```

### 5.3 Histogram payload

For the bit-period histogram, the Pico emits a compact representation: bucket edges + counts.

```json
{"v":1,"t":15.000,"kind":"histogram","ch":"kbd","metric":"bit_period_us",
 "buckets_us":[60,65,70,75,80,85,90,95,100],
 "counts":[0,0,0,1,387,52,3,0,0],
 "overflow":0}
```

16 log-spaced buckets by default. Buckets count samples that fall into `[bucket_us[i], bucket_us[i+1])`. The TUI renders the inline `█▏` histogram from this payload.

### 5.4 Commands (host → Pico)

The CDC 1 channel is technically bidirectional. The Pico accepts a small command set on the receive side:

| Command | Effect |
|---|---|
| `?fingerprint` | Emit a fingerprint snapshot for both channels immediately. |
| `?subscribe=frames` | Begin emitting per-frame events on CDC (otherwise suppressed). |
| `?unsubscribe=frames` | Stop emitting per-frame events. |
| `?trace=ps2_kbd` | Increase per-channel verbosity. |
| `?reset_window` | Reset rolling-window stats. |
| `?dump_pio` | Emit a one-shot snapshot of all PIO SM states. |
| `?ping` | Pico echoes `{"v":1,"kind":"pong","t":...}`. |

Commands are sent as a single line ending in `\n`. Responses are returned as normal events on the stream.

### 5.5 Backpressure and overflow

CDC TX has a finite USB-side queue. If the modern host is slow to read:

1. **First defense:** the firmware drops the **oldest** per-frame events (highest volume, lowest individual value).
2. **Second defense:** drops the oldest periodic stats and histograms.
3. **Never dropped:** lifecycle events (boot, classifier transitions, downloads) and anomalies.

The number of drops per second is itself an event:

```json
{"v":1,"t":12.500,"kind":"telemetry_overflow","dropped_last_s":47,"category":"frame"}
```

If `dropped_last_s > 0` persists for >5 s, the firmware reduces emission rate (stops per-frame events entirely until the host catches up).

### 5.6 Schema versioning

The top-level `"v"` is incremented on **breaking** schema changes. New event types and new fields can be added without bumping `"v"`. Consumers should:

- Ignore unknown event types.
- Ignore unknown fields within known events.
- Reject any line with a higher `"v"` than they understand, with a clear error.

---

## 6. Signature database

Fingerprint matching compares the live snapshot against a small compiled-in database of known keyboard/chipset signatures. The match algorithm is L2-distance over a normalized feature vector.

### 6.1 Database format

Embedded in firmware as a static array; ~50 entries fits in <4 KB flash. Definition:

```rust
pub struct KeyboardSignature {
    pub name: &'static str,            // "IBM Model M (1391401)"
    pub era: &'static str,             // "1986-1996"
    pub class: MachineClass,
    pub features: KeyboardFeatures,
}

pub struct KeyboardFeatures {
    pub bit_period_p50_us: u16,        // 81
    pub bit_period_p99_us: u16,        // 83
    pub duty_pct: u8,                  // 52
    pub skew_us: i8,                   // tenths of µs * 10, signed
    pub inhibit_avg_us: u16,           // 142
}
```

Each field is **normalized** (divided by a canonical range) before computing distance, so different units don't dominate the metric.

### 6.2 Match algorithm

```rust
fn delta(observed: &KeyboardFeatures, candidate: &KeyboardFeatures) -> f32 {
    let normalize_period = |v: u16| (v as f32 - 60.0) / 60.0;       // 60-120 µs → 0..1
    let normalize_duty   = |v: u8|  (v as f32 - 40.0) / 30.0;       // 40-70 % → 0..1
    let normalize_skew   = |v: i8|  (v as f32 + 50.0) / 100.0;      // -5..+5 µs → 0..1
    let normalize_inhibit= |v: u16| (v as f32 - 100.0) / 200.0;     // 100-300 µs → 0..1

    let dp50 = normalize_period(observed.bit_period_p50_us) - normalize_period(candidate.bit_period_p50_us);
    let dp99 = normalize_period(observed.bit_period_p99_us) - normalize_period(candidate.bit_period_p99_us);
    let ddty = normalize_duty(observed.duty_pct) - normalize_duty(candidate.duty_pct);
    let dskw = normalize_skew(observed.skew_us) - normalize_skew(candidate.skew_us);
    let dinh = normalize_inhibit(observed.inhibit_avg_us) - normalize_inhibit(candidate.inhibit_avg_us);

    (dp50.powi(2) + dp99.powi(2) + ddty.powi(2) + dskw.powi(2) + dinh.powi(2)).sqrt()
}
```

The top 3 lowest-Δ matches are surfaced in the fingerprint dump.

### 6.3 Stable hash

```rust
fn fingerprint_hash(features: &KeyboardFeatures) -> u64 {
    // Round to canonical buckets so jitter doesn't change the hash.
    let p50  = features.bit_period_p50_us / 2 * 2;          // round to even µs
    let p99  = features.bit_period_p99_us / 2 * 2;
    let duty = (features.duty_pct / 5) * 5;                 // round to nearest 5%
    let skew = features.skew_us / 1 * 1;                    // already coarse
    let inh  = (features.inhibit_avg_us / 10) * 10;         // round to 10µs

    let mut h = FxHasher::default();
    h.write_u16(p50);
    h.write_u16(p99);
    h.write_u8(duty);
    h.write_i8(skew);
    h.write_u16(inh);
    h.finish()
}
```

Rendered as four hex groups separated by `-`: `a7f3-9c12-8e44-2b91`.

### 6.4 Initial known entries

Stub list for v1 of the database; expand as the project sees more hardware in the wild:

| Name | Era | Class | p50 µs | duty % | skew µs | inh µs |
|---|---|---|---|---|---|---|
| IBM Model M (1391401) | 1986-1996 | AT | 81 | 52 | +0.3 | 140 |
| IBM Model F XT | 1981-1985 | XT | 96 | 50 | +0.5 | n/a |
| IBM Model F AT | 1984-1986 | AT | 96 | 50 | +0.5 | 220 |
| Northgate OmniKey 102 | ~1991 | AT | 84 | 55 | +0.1 | 155 |
| Cherry G80-1800 | ~1993 | AT | 79 | 51 | +0.2 | 138 |
| Compaq Enhanced III | ~1989 | AT | 82 | 53 | +0.3 | 144 |
| Generic AT clone | 1990s | AT | 80 | 50 | 0 | 150 |
| Generic PS/2 clone | 1995+ | PS/2 | 78 | 50 | 0 | 148 |
| USB-to-PS/2 adapter (modern) | 2010+ | PS/2 | 77 | 50 | 0 | 145 |

Entries are speculative until verified against real hardware; the project's bench-test phase populates them with actual measurements.

---

## 7. Implementation

### 7.1 Pico-side emit

| Module | Emit |
|---|---|
| `firmware/src/lifecycle.rs` | Boot, state transitions |
| `firmware/src/ps2/instrumentation.rs` | Frame, stats, histograms, anomalies |
| `firmware/src/ps2/classifier.rs` | Classifier state changes |
| `firmware/src/lpt/*.rs` | Mode transitions, packet rate |
| `firmware/src/protocol/block_server.rs` | Download begin/progress/complete |
| `firmware/src/usb_cdc/telemetry_channel.rs` | Stream sink (CDC 1) |
| `firmware/src/telemetry/ring.rs` | Internal SPSC ring; consumer drains to defmt + CDC |

JSON serialization uses `serde-json-core` (no_std, no_alloc). Each event type has its own `Serialize` impl; the ring stores enum variants and the consumer serializes on the way out.

### 7.2 Host-side TUI crate

```
tools/tui/
├── Cargo.toml             ratatui + tokio + serde + serde_json + clap
├── src/
│   ├── main.rs            argument parsing, CDC stream attach
│   ├── stream.rs          tokio task: reads CDC, parses JSON, pushes to state
│   ├── state.rs           shared state (arc-swap'd snapshot per view)
│   ├── views/
│   │   ├── mod.rs         View enum + dispatch
│   │   ├── overview.rs    View 1
│   │   ├── kbd.rs         View 2
│   │   ├── aux.rs         View 3
│   │   ├── lpt.rs         View 4
│   │   ├── events.rs      View 5
│   │   ├── hardware.rs    View 6
│   │   └── fingerprint.rs View 7
│   └── widgets/
│       ├── histogram.rs   Inline █▏ histogram widget
│       ├── progress.rs    Fixed-width progress bar
│       └── status_dot.rs  ● / ○ indicators
```

Invocation:

```sh
# Live attach to /dev/ttyACM1 (the second CDC interface)
cargo run --bin vintage-kvm-tui -- --port /dev/ttyACM1

# Replay a saved capture file
cargo run --bin vintage-kvm-tui -- --replay capture.jsonl

# Headless dump (CI / scripts)
cargo run --bin vintage-kvm-tui -- --port /dev/ttyACM1 --headless --json > capture.jsonl
```

### 7.3 Capture file format

Capture files are the raw CDC telemetry stream, one JSON event per line. Loss-less round-trip with the live stream. Useful for:

- Bug reports — attach a capture, the maintainer can replay it in the TUI.
- CI baselines — record a known-good run, diff future runs against it.
- Offline analysis — Python / R / Excel via standard JSON-lines tooling.

---

## 8. Phasing

| Phase | Surface available |
|---|---|
| 0 | defmt-RTT only; no telemetry events yet |
| 1 | PS/2 frame, stats, histogram, anomaly events; classifier transitions |
| 2 | + i8042 lifecycle, AUX events |
| **3** | + LPT mode, packets, CRC sniffer state, download progress |
| 4 | + IEEE 1284 negotiation events |
| 5 | + EPP/ECP mode, throughput metrics |
| 6 | TUI dashboard host-side crate ships (`tools/tui/`) |
| 7+ | Capture replay tooling, baseline diff |

The console-line surface (defmt-RTT) is **always available** from Phase 1 onward. The TUI is a deferred convenience — Phase 6 is when it crystallizes into a polished tool, but the underlying CDC stream is consumable from Phase 3 onward by any JSON-aware tool (`jq`, Python, etc.).

---

## 9. Related documents

- [`pico_firmware_design.md`](pico_firmware_design.md) §5.8 (CDC bridge), §5.10 (Telemetry)
- [`pio_state_machines_design.md`](pio_state_machines_design.md) §6-9 (PS/2 capture), §12 (validation/instrumentation)
- [`pico_phase3_design.md`](pico_phase3_design.md) — Phase 3+ MVP that the LPT-side instrumentation builds on
- [`stage1_implementation.md`](stage1_implementation.md) — DOS-side dual for the download path being instrumented
- Memory `ps2-oversampling-preference` — architectural decision behind §6 and §8
