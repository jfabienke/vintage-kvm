# PIO state machines design

Detailed design of the RP2350 PIO programs that drive the PS/2 KBD + AUX wires and all IEEE 1284 modes (Compat / Nibble / Byte / EPP / ECP). Companion to [`pico_firmware_design.md`](pico_firmware_design.md); this is the wire-protocol-level expansion of §4.2 (PIO and state-machine allocation).

Phase 3+ MVP was bit-bang only. This document specifies the PIO programs that replace it across the project lifecycle, plus the PIO-only PS/2 phy that the bit-bang version was never going to provide.

---

## 1. Goals and constraints

### Goals

- **PS/2 RX:** raw oversampling of both CLK and DATA at ≥4× nominal bit rate, with continuous instrumentation (per-bit timing, glitch detection, edge skew). Used for both passive sniffing and active reception.
- **PS/2 TX:** standard PS/2 frame transmission (device-side) for keyboard emulation, DEBUG-injection bootstrap, and i8042 private-mode byte pumping.
- **LPT all-mode coverage:** every IEEE 1284 mode the project uses, with sufficient throughput to run Stage 2 over EPP/ECP at line rate, and DMA-backed forward + reverse ECP for the steady-state data plane.
- **Auto-detection:** the PS/2 RX path produces enough metadata for the firmware to classify the host as XT, AT, or PS/2 from observed traffic without operator input.

### Constraints

- **PIO budget:** 3 PIO blocks × 4 SMs = 12 SMs; 32 instruction slots per block (shared across SMs in the block).
- **Pin layout already fixed** for current board ([`hardware_reference.md`](hardware_reference.md) §3.3). Some PS/2 and LPT signals are on non-consecutive GPIOs, which constrains PIO `in pins, N` / `out pins, N` width and forces some pin-masking in CPU. §4 details the impact.
- **74LVC07A** for PS/2 is non-inverting open-drain: `PULL pin = 0` drives the wire LOW; `PULL pin = 1` releases (pulled HIGH externally).
- **74LVC161284** for LPT has explicit direction control; the data bus direction flips per IEEE 1284 phase. A direction GPIO (TBD per board revision) is required for Phase 4+ modes that drive the data bus from the Pico.
- **System clock:** 150 MHz default (RP2350 boot). All dividers below assume this.

### Non-goals

- PIO-based USB CDC (uses the RP2350's built-in USB controller; not PIO).
- PIO-based status LED — NeoPixel uses a well-trodden WS2812 PIO program; allocated but not in scope here.
- Compression / decompression — pure CPU.

---

## 2. PIO recap

Single-paragraph refresher for readers unfamiliar with RP2350 PIO. Skip if you've written PIO programs before.

Each PIO block has 4 state machines, each with: 32-bit OSR (output shift register, feeds `out` and `mov pins`), 32-bit ISR (input shift register, fed by `in pins`), 32-bit X and Y scratch registers, programmable shift direction and auto-push/pull thresholds, 4-deep TX FIFO (can be joined to 8-deep), 4-deep RX FIFO (likewise), 1-5 side-set bits driven on every cycle, and a 16-bit fractional clock divider. The instruction set is 9 instructions (`jmp`, `wait`, `in`, `out`, `push`, `pull`, `mov`, `irq`, `set`), each 1 cycle. All four SMs in a block share the 32-instruction code RAM.

Programs are written in PIO assembly, compiled to 16-bit instructions by `pioasm`, loaded at runtime via the embassy-rp HAL.

---

## 3. PIO and SM allocation

Adjusted from the high-level allocation in [`pico_firmware_design.md` §4.2](pico_firmware_design.md) to reflect the full mode-set design below.

| PIO | SM | Phase | Program | LOC est. | FIFO/DMA |
|---|---|---|---|---|---|
| **0** | 0 | 3 | `lpt_compat_in` | 4 | RX FIFO → DMA |
| | 1 | 3 | `lpt_nibble_out` | 6 | TX FIFO ← DMA |
| | 2 | 5 | `lpt_byte_rev` *(shares slots with EPP)* | 8 | — |
| | 3 | 5 | `lpt_epp` (fwd/rev select via Y reg) | 12 | TX+RX FIFO ↔ DMA |
| **1** | 0 | 1+ | `ps2_kbd_oversample` | 4 | RX FIFO → DMA |
| | 1 | 1+ | `ps2_kbd_tx` | 12 | TX FIFO ← CPU |
| | 2 | 2+ | `ps2_aux_oversample` | 4 | RX FIFO → DMA |
| | 3 | 2+ | `ps2_aux_tx` | 12 | TX FIFO ← CPU |
| **2** | 0 | 5 | `lpt_ecp_fwd_dma` | 10 | TX FIFO ← DMA (peripheral side) |
| | 1 | 5 | `lpt_ecp_rev_dma` | 10 | RX FIFO → DMA |
| | 2 | 0+ | `ws2812_neopixel` | 4 | TX FIFO ← CPU |
| | 3 | TBD | spare | — | — |

**11 SMs allocated, 1 spare.** Instruction-slot usage per PIO block:

- PIO 0: 30 / 32 (tight — see §10.5 for what gets shared).
- PIO 1: 32 / 32 (full; PS/2 KBD + AUX programs occupy the block exclusively).
- PIO 2: 24 / 32 (room for future expansion).

**Pre-Phase-5 use:** only PIO 0 SM 0-1 and PIO 1 SM 0-3 are loaded. PIO 0 SM 2-3 and PIO 2 SM 0-1 are loaded on mode promotion.

---

## 4. Pin assignments and constraints

### 4.1 Current pin layout

Per [`hardware_reference.md`](hardware_reference.md) §3.3:

```
PS/2 KBD:    GP2=CLK_IN  GP3=CLK_PULL  GP4=DATA_IN  GP5=DATA_PULL
PS/2 AUX:    GP6=CLK_IN  GP28=CLK_PULL  GP9=DATA_IN  GP10=DATA_PULL
LPT data:    GP12-19 = D0-D7 (consecutive ✓)
LPT ctrl:    GP11=nStrobe  GP20=nAutoFd  GP22=nSelectIn  (nInit: TBD)
LPT status:  GP23=nAck  GP24=Busy  GP25=PError  GP26=Select  GP27=nFault
```

### 4.2 Consecutive-pin requirements for PIO `out pins, N` / `set pins, N`

PIO `in pins, N` reads N consecutive pins starting at `IN_BASE`. `out pins, N` and `set pins, N` write N consecutive pins starting at `OUT_BASE` or `SET_BASE`. The bases are configurable per SM, but width is fixed and pins must be consecutive.

This constrains the design:

| Bus | Required width | Pins | Consecutive? |
|---|---|---|---|
| LPT data bus (D0-D7) | 8 | GP12-19 | ✓ |
| LPT status outputs (nibble + phase) | 5 | GP23, GP25, GP26, GP27 (nibble) + GP24 (phase) | ✗ (GP24 in middle) |
| PS/2 KBD inputs (CLK + DATA) | 2 | GP2, GP4 | ✗ (GP3 between) |
| PS/2 AUX inputs (CLK + DATA) | 2 | GP6, GP9 | ✗ (GP7, GP8 between) |
| PS/2 KBD outputs (CLK_PULL + DATA_PULL) | 2 | GP3, GP5 | ✗ (GP4 between) |
| PS/2 AUX outputs (CLK_PULL + DATA_PULL) | 2 | GP28, GP10 | ✗ (far apart) |

The non-consecutive cases work in PIO but require **wider reads/writes with bit masking**, costing FIFO bandwidth and CPU cycles. Specifically:

- **LPT status:** read GP23-27 as a contiguous 5-bit field with the **nibble mapping reorganized** so the natural bit positions match. Concretely: nibble bit 0 → GP23 (status bit 6 inverted), bit 1 → GP25, bit 2 → GP26, bit 3 → GP27; phase → GP24. The PIO `out pins, 5` with `OUT_BASE = GP23` writes (bit0,bit1,bit2,bit3,phase) but the wire mapping has nibble[3..0] on status[6..3]. CPU pre-reorders the bits before pushing to the TX FIFO. See §9.2 for the reordering routine.

- **PS/2 CLK + DATA:** read 3 pins for KBD (`in pins, 3` with IN_BASE=GP2 → bits 0,1,2 = CLK,CLK_PULL,DATA). Discard middle bit in CPU. AUX needs `in pins, 4` (GP6-9, GP7=LED + GP8=PSRAM_CS as don't-cares).

- **PS/2 PULL outputs:** use two separate `set pins, 1` instructions targeting different SET_BASE values. Or accept a wider `out pins` write that touches inert pins. We adopt **per-pin `set`** for the TX program because it's only 2 instructions and gives explicit control over CLK vs DATA edges.

### 4.3 Hardware prerequisites for full PIO use

For the design to be **fully PIO-native** without CPU bit-reshuffling per byte, a board revision should place:

- **PS/2 CLK_IN and DATA_IN on adjacent pins** for each channel (so `in pins, 2` reads exactly CLK and DATA with no padding).
- **PS/2 CLK_PULL and DATA_PULL on adjacent pins** for each channel (so `set pins, 2` writes exactly the two pulls).
- **LPT status outputs on consecutive pins** in the order needed by the nibble protocol: nibble[3..0] then phase, or phase then nibble[3..0]. Order doesn't matter as long as they're contiguous.

This is **not blocking** for current implementation — the CPU-side bit-reorder costs ~20 ns per byte at 150 MHz, well below the bottleneck (LPT wire rate or PS/2 frame rate). Flagged here so a v2 board can drop the reordering for cleaner PIO programs.

### 4.4 nInit routing (still open)

DOS Stage 0/1 use **nInit** (LPT control register bit 2) as the host strobe in nibble mode. The hardware ref doesn't enumerate which Pico GPIO sees nInit through the 74LVC161284. Confirming this is the **first bring-up step** for Phase 3+ regardless of bit-bang vs PIO. Until then, the design assumes GP11 (currently labeled "nStrobe") and notes the assumption.

---

## 5. System clock and PIO dividers

All dividers below assume **sys_clk = 150 MHz** (default RP2350 boot speed). If sys_clk is raised to 200+ MHz later, dividers scale proportionally and table below should be regenerated.

| Program | PIO clock | Divider (int / frac) | Rationale |
|---|---|---|---|
| `ps2_kbd_oversample` | 1.0 MHz | 150 / 0 | 60× nominal AT clock (16.7 kHz); 1 µs sample resolution. |
| `ps2_aux_oversample` | 1.0 MHz | 150 / 0 | Same. |
| `ps2_kbd_tx` | 100 kHz | 1500 / 0 | 6.25 × nominal frame rate; comfortable bit-time spacing (~80 µs PS/2 nominal → 10 PIO cycles per bit). |
| `ps2_aux_tx` | 100 kHz | 1500 / 0 | Same. |
| `lpt_compat_in` | 150 MHz | 1 / 0 | Wire-rate (`wait pin` is instruction-paced). |
| `lpt_nibble_out` | 1 MHz | 150 / 0 | 100 µs settle delays = 100 PIO cycles. |
| `lpt_byte_rev` | 50 MHz | 3 / 0 | ~10 cycles per bit-time at AT bus speed. |
| `lpt_epp` | 30 MHz | 5 / 0 | ~33 ns/cycle → EPP strobe handshakes within IEEE 1284 timing budget. |
| `lpt_ecp_fwd_dma` | 30 MHz | 5 / 0 | Same as EPP. |
| `lpt_ecp_rev_dma` | 30 MHz | 5 / 0 | Same. |
| `ws2812_neopixel` | 8 MHz | 18 / 12 | Standard WS2812 timing (125 ns / bit-third). |

Higher PS/2 oversample rates are technically free (we could go to 4 MHz easily, giving 250 ns resolution), but 1 MHz is a sweet spot:
- Enough to see CLK edges with ±0.5 µs uncertainty (PS/2 CLK transitions are ≥30 µs long).
- DMA bandwidth = 1 MS/s × 32 bit/sample = 4 MB/s per channel × 2 channels = 8 MB/s. RP2350's SRAM-XIP bus handles this in its sleep.
- SRAM ring per channel: 4 KB → 4 ms history → ~50 frames of context for the classifier.

---

## 6. PS/2 oversampler (RX path)

### 6.1 Program

```
.program ps2_kbd_oversample
.wrap_target
    in pins, 3       ; sample CLK_IN, CLK_PULL, DATA_IN (GP2,GP3,GP4)
.wrap
```

That's it. Two instructions (`in` + implicit `jmp .wrap_target`). With `autopush threshold = 30`, the ISR fills with 10 samples (10 × 3 bits = 30 bits) before auto-pushing to the RX FIFO. Each RX-FIFO word thus contains 10 timed samples.

**Why not `in pins, 2`?** PS/2 KBD CLK is on GP2 and DATA on GP4; GP3 (CLK_PULL) sits between. Reading width 2 would only capture GP2-3 (CLK + our own pull-down register, useless). We read 3 bits and discard the middle bit in the frame extractor.

**Why not `in pins, 32`?** Bandwidth waste — 16× more SRAM traffic. We don't need the other 29 GPIOs.

**Auto-push threshold = 30, not 32:** because 32 is not a multiple of 3 bits/sample. 30 bits = exactly 10 samples. CPU-side decode is cleaner.

### 6.2 AUX variant

```
.program ps2_aux_oversample
.wrap_target
    in pins, 4       ; GP6=CLK_IN, GP7=LED, GP8=PSRAM_CS, GP9=DATA_IN
.wrap
```

Reads 4 bits per sample because of the GP7/GP8 gap. Auto-push threshold = 28 (7 samples × 4 bits). CPU extracts bit 0 (CLK) and bit 3 (DATA), masks the rest.

Reading GP8 (PSRAM CS) does not disturb the PSRAM interface — only the GPIO **output mux** is contested, and PIO is only reading the pad input.

### 6.3 DMA configuration

Per channel:
- DMA channel chained for double-buffering.
- Read addr: PIO RX FIFO (fixed).
- Write addr: incrementing into a 4 KB SRAM ring split into two 2 KB halves.
- Transfer size: 32-bit words.
- Trigger: PIO RX FIFO not empty.
- Chain: each half-buffer triggers an interrupt to the CPU and starts the other half.

CPU side: when a half-buffer-full IRQ fires, the frame extractor processes the just-completed half while DMA fills the other.

### 6.4 Bandwidth budget

- 1 MS/s × 4 B/sample = 4 MB/s per channel (oversampling 32-bit words; pre-pack saves bandwidth).
- Pre-pack: 30 bits → push as 32-bit (FIFO entry is u32 regardless of how many bits we actually use). So 1 MS/s / 10 samples-per-word × 4 B/word = 400 kB/s per channel. KBD + AUX = 800 kB/s.
- Plenty of margin on the SRAM bus.

---

## 7. PS/2 frame extractor (CPU)

### 7.1 Stream model

The frame extractor sees a continuous u32 stream where each word contains 10 samples × 3 bits (KBD) or 7 samples × 4 bits (AUX). Each sample's offset within the word implies a timestamp (PIO clock 1 MHz → 1 µs/sample).

```rust
pub struct OversampleStream<'a> {
    ring: &'a [u32],
    head: usize,           // most recent word DMA wrote
    tail: usize,           // oldest word the extractor still cares about
    sample_offset: u8,     // sub-word position (0..NUM_SAMPLES_PER_WORD)
    samples_consumed: u64, // monotonic; gives absolute µs timestamp
}
```

### 7.2 Extraction algorithm

For each newly-arrived word:

1. Unpack into a `[Sample; N]` where `Sample = (clk: bool, data: bool)`.
2. Walk the samples looking for state transitions on CLK.
3. On CLK **falling edge**:
   - If in `Idle` state: this is the **start bit**. Record absolute timestamp.
   - If in `Receiving { bits_so_far }`: this is the **next bit clock**. Sample DATA at this exact offset (PS/2 spec: read DATA on falling CLK edge). Shift into the assembling byte.
4. Track time since last CLK edge → compute bit period.
5. After enough bits (9 for XT, 11 for AT/PS/2), emit a `Ps2Frame` to the dispatcher.

### 7.3 Ps2Frame data model

```rust
pub struct Ps2Frame {
    pub data: u8,
    pub parity_ok: bool,
    pub framing_ok: bool,
    pub start_timestamp_us: u64,
    pub timing: FrameTiming,
}

pub struct FrameTiming {
    pub bit_periods_us: [u16; 11],   // measured CLK→CLK interval for each bit
    pub clk_data_skew_us: i8,        // signed; positive = DATA changed after CLK
    pub glitch_count: u8,             // CLK transitions shorter than 4 samples
}
```

### 7.4 XT vs AT detection within extractor

Mid-frame, the extractor counts CLK edges:

- After **8 data bits**, if the next CLK edge brings DATA HIGH and STOPS (no further edges within ~200 µs): **XT**. Frame complete.
- After 8 data bits, if a 9th and 10th CLK edge follow: **AT or PS/2**. Bit 9 is parity, bit 10 is stop. Validate parity, frame complete.
- Timeout / framing error: emit `framing_ok = false`, continue trying.

The classifier (§8) aggregates these across multiple frames before committing to a `MachineClass`.

### 7.5 Glitch detection

Each oversampled CLK transition that lasts **< 4 samples** (< 4 µs) is counted as a glitch. PS/2 spec says CLK transitions are ≥ 25 µs; glitches at ≤ 4 µs are nearly always electrical noise or contact bounce.

Glitches are surfaced in `FrameTiming::glitch_count` and aggregated into the per-channel `Ps2Stats` histogram for diagnostic surfacing via defmt-RTT / CDC.

---

## 8. Machine-class classifier

### 8.1 State machine

```
Unknown
  ├─ observe frame with 9-bit length (no parity, no stop):
  │     -> XtCandidate (1 frame)
  │
  ├─ observe frame with 11-bit length, valid parity:
  │     -> AtCandidate (1 frame)
  │
  └─ observe host-to-keyboard traffic (CLK held LO ≥ 100 µs by host):
        -> AtCandidate (1 frame)

XtCandidate (n frames)
  ├─ another 9-bit frame -> XtCandidate (n+1)
  ├─ 3 consecutive matches -> Confirmed(Xt)
  └─ AT-style frame observed -> Unknown (reset)

AtCandidate (n frames)
  ├─ another 11-bit frame -> AtCandidate (n+1)
  ├─ 3 consecutive matches -> Confirmed(At)
  └─ XT-style frame observed -> Unknown

Confirmed(At) + AUX channel produces traffic
  -> Confirmed(Ps2)
```

Three consecutive matching frames before commit avoids spurious classification from a single corrupt frame.

### 8.2 Confidence thresholds

- **3 frames** of matching pattern → tentative classification (emit `MachineClassDetected` event).
- Pattern reverses → reset to `Unknown` and re-classify.
- Classifier is **always running** in the background; if the host changes (BIOS warm-reset can switch keyboard mode), reclassification follows automatically.

### 8.3 Stage 0 variant selection

Once `Confirmed(class)` is reached, the firmware:

1. Looks up the corresponding Stage 0 blob (`S0_XT.COM`, `S0_AT.COM`, `S0_PS2.COM`).
2. Generates the class-appropriate DEBUG injection script.
3. Begins keying the script via `ps2_kbd_tx`.

---

## 9. PS/2 transmitter (TX path)

### 9.1 Wire protocol (device → host frame)

PS/2 device-to-host transmission, one 11-bit frame for AT/PS/2:

```
Bit time = ~80 µs (12.5 kHz nominal)

   ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐
CLK│ │_│ │_│ │_│ │_│ │_│ │_│ │_│ │_│ │_│ │_│ │_│
   └─┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─┘
DATA: 0  D0  D1  D2  D3  D4  D5  D6  D7  P   1
      start ──── data bits ────────── parity stop
```

Device drives both CLK and DATA. Host samples DATA on each CLK **falling** edge.

### 9.2 TX program

```
.program ps2_kbd_tx
; Inputs:
;   - SET_BASE on the CLK_PULL pin (GP3)
;   - OUT_BASE on the DATA_PULL pin (GP5)
;   - Side-set: 0 bits used (or used for diagnostic LED)
;   - PIO clock: 100 kHz → 10 µs per cycle
;   - Bit time: 8 cycles = 80 µs
;
; CPU pre-packs a 16-bit word:
;   bit 0     = start (0)
;   bits 1-8  = data LSB first
;   bit 9     = odd parity
;   bit 10    = stop (1)
;   bits 11-15= padding (1s)

    pull block                  ; wait for byte (CPU pre-packs frame)

bit_loop:
    out y, 1                    ; pull next bit into Y
    set pins, 0 ; CLK_PULL=0    ; drive CLK low (start of bit)
    nop [3]                     ;   hold CLK low for half bit time
    mov pins, y                 ; drive DATA = bit value (low half)

    set pins, 1 ; CLK_PULL=1    ; release CLK (let pull-up bring it high)
    nop [3]                     ;   hold CLK high for half bit time

    jmp x-- bit_loop            ; X = 10; loop 11 times (one full frame)

.wrap
```

**Issues with this sketch:**

- `set pins, 0` uses SET_BASE = CLK_PULL pin. `mov pins, y` uses OUT_BASE. Need both base registers configured for the same SM. Allowed.
- "drive DATA = bit value" needs special handling: `mov pins, y` writes Y[0] to OUT_BASE = DATA_PULL. Y[0]=0 → drive low; Y[0]=1 → release. With 74LVC07A non-inverting open-drain, this is the right polarity for a "1" bit on the wire.

Wait — that's inverted. We want: send a "1" bit → DATA wire is HIGH. With open-drain, "DATA wire HIGH" = "DATA_PULL released" = "GP5 = 1". So `mov pins, 1` releases DATA, giving a HIGH wire. ✓

For a "0" bit: DATA wire is LOW = DATA_PULL drives low = GP5 = 0. So `mov pins, 0`. ✓

Logic is consistent: the bit value drives the GPIO output directly through the non-inverting open-drain.

**Refined program (loop count corrected):**

```
.program ps2_kbd_tx
    set y, 10                   ; 11 bits to send (loop from 10 down to 0)
    pull block

bit_loop:
    out x, 1                    ; X = next bit
    set pins, 0 [3]             ; CLK low, 4-cycle hold
    mov pins, x                 ; DATA = bit
    set pins, 1 [3]             ; CLK high, 4-cycle hold
    jmp y-- bit_loop

idle:
    set pins, 1                 ; idle: CLK released
.wrap
```

10 instructions. Fits comfortably.

### 9.3 Frame pre-packing (CPU)

```rust
pub fn pack_frame(byte: u8) -> u16 {
    let mut word: u16 = 0;
    word |= 0;                          // start bit (bit 0)
    word |= (byte as u16) << 1;          // 8 data bits (bits 1..8)
    let parity = !(byte.count_ones() as u8 & 1) & 1;  // odd parity
    word |= (parity as u16) << 9;
    word |= 1 << 10;                     // stop bit
    word |= 0xF800;                      // pad upper bits with 1s
    word
}
```

CPU pushes `pack_frame(byte) as u32` to TX FIFO; PIO shifts LSB-first via `out`.

### 9.4 XT vs AT/PS2 framing

XT keyboards send 9 bits: start + 8 data, no parity, no stop. The XT TX program omits the parity bit:

```rust
pub fn pack_frame_xt(byte: u8) -> u16 {
    let mut word: u16 = 0;
    word |= 0;
    word |= (byte as u16) << 1;
    word |= 1 << 9;                     // "stop" — actually idle-high
    word |= 0xFC00;                     // padding
    word
}
```

The TX PIO program is identical (it just sends N bits where N is encoded in the initial `set y, N-1`). The CPU configures Y based on `MachineClass`.

### 9.5 Host-to-device direction (host commands the device)

When the host wants to send a byte to the keyboard, it pulls CLK low for ≥100 µs (inhibit), drives DATA low (start bit), releases CLK, then the device clocks in 11 bits. After the stop bit, the device pulls DATA low for one cycle (ACK).

This direction is **observed** by the oversampler RX path — the same SM that captures device→host frames also captures host→device frames (it doesn't care which side is driving the wires; it just samples). The frame extractor distinguishes by examining who's pulling CLK during the start:

- Device→host: device pulls CLK, device drives DATA.
- Host→device: host pulls CLK first (extended hold), then host drives DATA low while device sees CLK still held by host until it begins clocking.

For Phase 2+ when we serve i8042 private-mode byte traffic, the firmware needs to respond to host-initiated frames with an ACK. The TX SM does this on demand.

---

## 10. LPT PIO programs

### 10.1 `lpt_compat_in` — SPP / nibble-mode forward byte capture

Already sketched in the Phase 3 bit-bang. PIO version:

```
.program lpt_compat_in
; Pin map:
;   IN_BASE = host-strobe pin (assumed GP11; see §4.4)
;   IN_BASE+1..IN_BASE+8 = D0..D7 (GP12..GP19, consecutive ✓)
; PIO clock: 150 MHz (no divider; instruction-paced)

.wrap_target
    wait 0 pin 0             ; wait for strobe LO (falling edge)
    in pins, 9               ; sample strobe + 8 data bits into ISR
    push                     ; push 9-bit word to RX FIFO
    wait 1 pin 0             ; wait for strobe HI (idle)
.wrap
```

4 instructions. CPU side decodes:

```rust
let word = pio_rx.read();   // u32 from FIFO
let byte = ((word >> 1) & 0xFF) as u8;   // shift off strobe bit
```

**DMA option:** when receiving long bursts (Stage 1's Stage 2-download SEND_BLOCK acks), the CPU bypasses per-byte read and lets DMA fill an SRAM buffer. DMA transfer count is set to the expected packet size; CPU waits on completion IRQ. Saves ~50 ns/byte CPU time.

### 10.2 `lpt_nibble_out` — SPP nibble-mode reverse byte send

Sends one byte as two nibbles + phase toggles. Critical: maintain persistent phase across byte boundaries.

**Pin layout dependency:** the 4 nibble bits + phase need to be consecutive for clean `out pins, 5`. Current layout has GP24 (Busy/phase) in the middle of GP23-27. Two options:

**Option A (CPU pre-shuffles):** keep current pin layout, CPU pre-orders the 5-bit output value so PIO `out pins, 5` with OUT_BASE=GP23 produces:

```
bit 0 → GP23 = nAck     (status bit 6 = nibble bit 3)
bit 1 → GP24 = Busy     (status bit 7 = phase)
bit 2 → GP25 = PError   (status bit 5 = nibble bit 2)
bit 3 → GP26 = Select   (status bit 4 = nibble bit 1)
bit 4 → GP27 = nFault   (status bit 3 = nibble bit 0)
```

CPU prepares each byte as two 5-bit fields (one per nibble), packs into a 32-bit word. Padding bits stay 0 (or 1; they're paired with discarded outputs).

```rust
fn pack_nibble_pair(byte: u8, last_phase: bool) -> u32 {
    let lo = byte & 0x0F;
    let hi = (byte >> 4) & 0x0F;
    let phase_after_lo = !last_phase;
    let phase_after_hi = last_phase;  // flipped twice = original
    let w_lo = nibble_to_pin_order(lo, phase_after_lo);
    let w_hi = nibble_to_pin_order(hi, phase_after_hi);
    (w_lo as u32) | ((w_hi as u32) << 8)
}

fn nibble_to_pin_order(nibble: u8, phase: bool) -> u8 {
    //  pin order: nAck(b6=nib3) Busy(b7=phase) PError(b5=nib2) Select(b4=nib1) nFault(b3=nib0)
    //  → bit positions 0..4 in OUT word:
    ((nibble >> 3) & 1) << 0           // nib3 → pin 0
    | (phase as u8) << 1               // phase → pin 1
    | ((nibble >> 2) & 1) << 2         // nib2 → pin 2
    | ((nibble >> 1) & 1) << 3         // nib1 → pin 3
    | (nibble & 1) << 4                // nib0 → pin 4
}
```

PIO program:

```
.program lpt_nibble_out
; OUT_BASE = GP23, width 5
; PIO clock: 1 MHz (1 µs/cycle)
; SETTLE = 100 cycles = 100 µs

.wrap_target
    pull block                ; CPU sends pre-packed 16-bit pair (each nibble = 5 bits + padding)
    out pins, 5               ; drive low nibble + new phase
    nop [99]                  ; settle 100 µs
    out null, 3               ; discard padding to byte boundary
    out pins, 5               ; drive high nibble + new phase
    nop [99]                  ; settle 100 µs
    out null, 3               ; discard padding
.wrap
```

6 instructions in the loop + 1 pull = 7 PIO slots. Plus the `out null, 3` is awkward — we'd be cleaner with a 10-bit pre-pack (5+5) and `out pins, 5` twice.

Refined:

```rust
fn pack_nibble_pair_10bit(byte: u8, last_phase: bool) -> u32 {
    let w_lo = nibble_to_pin_order(byte & 0x0F, !last_phase);
    let w_hi = nibble_to_pin_order((byte >> 4) & 0x0F, last_phase);
    (w_lo as u32) | ((w_hi as u32) << 5)
}
```

```
.program lpt_nibble_out
.wrap_target
    pull block
    out pins, 5
    nop [99]
    out pins, 5
    nop [99]
.wrap
```

5 instructions. Clean.

**Option B (rewire to consecutive pins):** v2 board places nibble+phase on consecutive GPIOs. PIO program becomes the same but the CPU-side `nibble_to_pin_order` collapses to `((nibble << 1) | phase)` (or a fixed permutation). Half the CPU cycles per byte. Worth doing if we hit a CPU bottleneck — unlikely at SPP-nibble rates.

### 10.3 `lpt_byte_rev` — IEEE 1284 byte-mode reverse

Byte mode is bidirectional 8-bit transfer. In reverse (Pico → DOS), the wire choreography is:

```
Host:  drives nAutoFd LO     (request next byte)
Pico:  drives data lines     (8 bits at once)
Pico:  pulses nAck LO        (data ready)
Host:  reads data, releases nAutoFd
Pico:  releases data (Hi-Z), waits for next nAutoFd
```

```
.program lpt_byte_rev
; OUT_BASE = D0 (GP12), width 8
; SIDE_BASE = nAck (GP23)
; IN_BASE = nAutoFd (GP20)
; Direction: Pico drives data bus → requires 74LVC161284 set to reverse via
;   the data_dir GPIO (CPU controls this before invoking the SM).
; PIO clock: 50 MHz (20 ns/cycle)

.side_set 1                       ; nAck drives via side-set

.wrap_target
    pull block          side 1    ; get byte, nAck idle HI
    wait 0 pin 0        side 1    ; wait for nAutoFd LO (host request)
    out pins, 8         side 1    ; drive data bus, nAck still HI
    nop [4]             side 0    ; nAck LO pulse (5 cycles = 100 ns)
    wait 1 pin 0        side 1    ; wait for host to release nAutoFd
.wrap
```

5 instructions + side-set. Tight.

**Direction switching:** CPU must set the 74LVC161284 direction GPIO HIGH (or LOW per board wiring) **before** loading data into the TX FIFO. After the last byte, CPU flips direction back. This is out-of-band of the SM.

### 10.4 `lpt_epp` — EPP forward / reverse (combined)

EPP is the simplest high-speed mode: each cycle is a single CPU `OUT` or `IN` instruction on the host. The peripheral responds with a strobe handshake per cycle.

**Forward (host → peripheral, data cycle):**

```
Host: drives data lines + Address/Data Select
Host: drives nDataStb LO          (begin cycle)
Pico: drives nWait LO              (acknowledge, latch byte)
Host: releases nDataStb
Pico: releases nWait
```

**Reverse (peripheral → host, data cycle):**

```
Host: drives nDataStb LO          (request byte)
Pico: drives data + nWait LO       (data ready, hold)
Host: reads data, releases nDataStb
Pico: releases data and nWait
```

Both directions can fit in a single SM with mode-selection via X register set by the CPU.

```
.program lpt_epp
; Mode flag in X: 0 = forward (DOS writes), 1 = reverse (DOS reads)
; OUT_BASE = D0, width 8
; SIDE_BASE = nWait (GP24 or similar; PHASE pin reused)
; IN_BASE = nDataStb (TBD; one of the control inputs)

.side_set 1

start:
    pull block       side 1       ; get next byte or direction-change cmd
    out x, 1         side 1       ; X = direction bit
    out null, 23     side 1       ; discard padding
    jmp !x forward   side 1

reverse:
    out pins, 8      side 1       ; drive data bus
    wait 0 pin 0     side 1       ; wait for host strobe LO
    nop [4]          side 0       ; nWait LO (handshake)
    wait 1 pin 0     side 1       ; wait for host to release
    jmp start        side 1

forward:
    wait 0 pin 0     side 1       ; wait for host strobe LO
    in pins, 8       side 0       ; sample data bus, nWait LO
    push             side 0
    wait 1 pin 0     side 0
    nop              side 1       ; release nWait
    jmp start
```

~12 instructions. The forward path pushes received bytes to RX FIFO; the reverse path pulls from TX FIFO. DMA can drive both directions for back-to-back transfers.

**Throughput estimate:** at 30 MHz PIO clock, each cycle is 33 ns. The strobe handshake takes ~5 cycles = 165 ns = ~6 MB/s peak. Bus reality (host LPT chip + cable) caps at ~500 kB/s for EPP; we have ample headroom.

### 10.5 `lpt_ecp_fwd_dma` / `lpt_ecp_rev_dma` — ECP with DMA

ECP adds:
- A **command/data** flag (encoded in the host's HostAck signal).
- A **forward FIFO** in the host's controller.
- Optional **DMA** at the host end for bulk transfer.

The peripheral-side wire choreography is similar to EPP but with the command/data bit needing to be tracked.

**ECP forward (host → Pico):**

```
Host: drives data + HostAck (=command flag)
Host: drives HostClk LO        (clock byte)
Pico: drives PeriphAck LO       (acknowledge)
Host: releases HostClk
Pico: releases PeriphAck
```

The Pico needs to capture **both the byte and the command/data bit** per cycle.

```
.program lpt_ecp_fwd_dma
; IN_BASE = HostClk (GP11)
; IN_BASE+1..+8 = D0..D7 (GP12..GP19)
; HostAck on a separate input (e.g. GP26); read via `jmp pin` or separate `in`.

.side_set 1 ; PeriphAck on side-set

.wrap_target
    wait 0 pin 0      side 1    ; host strobe LO
    in pins, 9        side 1    ; read strobe + 8 data
    nop [2]           side 0    ; PeriphAck LO pulse
    push              side 0
    ; HostAck is captured as part of a separate stream OR
    ; CPU reads it explicitly when consuming the FIFO entry.
    wait 1 pin 0      side 1    ; host releases strobe
.wrap
```

For the command/data flag, options:

- **A. CPU reads HostAck via the `lpt_compat_in` SM** when needed (rare).
- **B. Encode HostAck into ISR**: add `in pins, 1` reading the HostAck pin separately, giving 10-bit words to the FIFO.

Option B is cleaner for full ECP fidelity. Costs 1 more bit per FIFO entry and 1 more PIO instruction.

**ECP reverse:**

```
.program lpt_ecp_rev_dma
; OUT_BASE = D0, width 8
; Side-set = PeriphClk
; Wait for HostAck to know when host is ready

.side_set 1

.wrap_target
    pull block        side 1    ; next byte
    out pins, 8       side 1    ; drive data
    wait 0 pin 0      side 1    ; wait for HostAck LO (host ready)
    nop [2]           side 0    ; PeriphClk LO pulse
    wait 1 pin 0      side 1    ; host releases HostAck
.wrap
```

5 instructions. DMA can feed bytes from a PSRAM-resident send buffer at line rate.

**Throughput:** ECP wire can hit ~2 MB/s. At 30 MHz PIO clock with 5-instruction inner loop = 167 ns/cycle = 6 MB/s peak. DMA from PSRAM via XIP cache delivers ~10 MB/s, so PSRAM is not the bottleneck.

### 10.6 PIO program lifecycle across IEEE 1284 modes

The four 1284 modes use different programs on the forward and reverse SMs. PIO0 has 32 instructions of program memory shared across all of its SMs. Pre-loading every program at boot would overflow that budget:

```
Program memory cost per program (built / planned)
─────────────────────────────────────────────────────────────────
                              instr   role
  lpt_compat_in    (built)        3   forward strobe-edge sampler
                                      (reused by SPP, Byte)
  lpt_nibble_out   (built)        5   reverse 5-bit nibble pair
  lpt_byte_rev     (built)        5   reverse 8-bit + nAck handshake
  lpt_epp          (built)       12   combined fwd/rev on one SM
                                      (dir bit prepended to TX word)
  lpt_dir_follower (built)        1   EPP-only `mov pins, pins`
                                      mirror of nWrite → DIR (SM2)
  lpt_ecp_fwd      (built)        5   host→Pico burst, PeriphAck
  lpt_ecp_rev      (built)        5   Pico→host burst, PeriphClk
                              ─────
  Sum of all programs            36   exceeds 32 — can't pre-load
```

That sum is misleading though. The 1284 negotiator picks one mode per session and stays there; only the **pair** of programs for that mode needs to be loaded simultaneously:

```
Per-mode coexistence (what actually has to fit at once)
─────────────────────────────────────────────────────────────────────
Mode             SM0 program        SM1 program        SM2 program           Total  Free
─────────────────────────────────────────────────────────────────────
SPP              lpt_compat_in (3)  —                  —                       3    29
Nibble (boot)    lpt_compat_in (3)  lpt_nibble_out (5) —                       8    24
Byte             lpt_compat_in (3)  lpt_byte_rev (5)   —                       8    24
EPP              lpt_epp (12)       —                  lpt_dir_follower (1)   13    19
ECP              lpt_ecp_fwd (5)    lpt_ecp_rev (5)    —                      10    22
─────────────────────────────────────────────────────────────────────
Worst single mode footprint (EPP)                                            13 / 32
```

EPP is the only mode that uses SM2: the [`lpt_dir_follower`](../firmware/src/lpt/pio_dir_follower.rs) one-instruction mirror loop drives the 74LVC161284's DIR pin (GP29) from the host's nWrite (GP11) so per-cycle direction flips happen in ~20 ns of input-sync latency instead of CPU-poll time. See `docs/hardware_reference.md` §11.3 for the chip-side reasoning.

So PIO0's instruction memory is over-provisioned by roughly 2.5× for the busiest single mode. Even doubling every estimate, the worst case still fits.

SM and DMA budgets stay roomy too:

```
PIO0 utilization, worst single 1284 mode (EPP)
─────────────────────────────────────────────────────────────────
  SMs           3 / 4    (SM0 combined fwd/rev, SM2 DIR follower;
                          SM1 idle but held by EppPhy; SM3 spare)
  Instr mem    13 / 32   (reload on mode transition, not pre-load)
  IRQ flags     0 / 4
  DMA chans     2 of 12  (the firmware overall uses 6 of 12)
  RX/TX FIFOs   4 words each, joinable to 8 if any mode wants it
```

**Transition strategy: reload, not pre-load.** Mode changes happen at the 1284 negotiation boundary — by spec this is a quiescent handshake with no bulk data in flight. The transition path is therefore allowed to take microseconds:

1. Drain both SMs' FIFOs (CPU-pull until empty, with timeout).
2. `set_enable(false)` on both SMs; stop both DMA channels.
3. Drop the old `LoadedProgram` handles (frees their instruction-memory slots in embassy's allocator).
4. `common.load_program(...)` for the new pair of programs.
5. Build a fresh `Config` for each SM (program offset, clock divider, pin maps, side-set).
6. Restart DMA + `set_enable(true)`.

Pre-loading all programs and only `set_config`-swapping between them would be cheaper at transition time, but doesn't fit the budget at 32 instructions. Reload is cheap enough at the cadence we actually use it — typically once or twice per session, never per-packet.

**Where this gets tight.** Two scenarios would force a redesign:

- **A mode wanting more than 2 SMs.** We have SM2/SM3 spare, so a mode with three concurrent SMs (e.g., separate read/write/arbiter) still fits. Beyond that we'd have to choose between modes or move state to another PIO block.
- **A single program over ~24 instructions.** ECP with PIO-side RLE decode would be borderline. The plan keeps PIO programs as raw byte movers — RLE/compression handling stays CPU-side — which keeps per-program cost well under 10 instructions.

Neither shows up in the 1284 spec as currently planned, so the budget holds.

---

## 11. DMA architecture

| Direction | PIO SM | DMA channel | Source / dest |
|---|---|---|---|
| PS/2 KBD oversample → SRAM ring | PIO1 SM0 | DMA 0 | RX FIFO → SRAM ring (4 KB, chained) |
| PS/2 AUX oversample → SRAM ring | PIO1 SM2 | DMA 1 | RX FIFO → SRAM ring (4 KB, chained) |
| PS/2 KBD TX byte stream | PIO1 SM1 | (none; CPU-paced) | — |
| PS/2 AUX TX byte stream | PIO1 SM3 | (none) | — |
| LPT compat-in burst | PIO0 SM0 | DMA 2 | RX FIFO → SRAM staging (per-packet, transfer count set by CPU) |
| LPT nibble-out burst | PIO0 SM1 | DMA 3 | SRAM staging → TX FIFO |
| LPT byte-rev burst | PIO0 SM2 | DMA 4 | SRAM staging → TX FIFO |
| LPT EPP fwd | PIO0 SM3 | DMA 5 | RX FIFO → SRAM |
| LPT EPP rev | PIO0 SM3 | DMA 6 | SRAM → TX FIFO |
| LPT ECP fwd | PIO2 SM0 | DMA 7 | RX FIFO → PSRAM ring (large) |
| LPT ECP rev | PIO2 SM1 | DMA 8 | PSRAM ring → TX FIFO |

9 DMA channels used at peak; 16 available. Comfortable margin.

**Chaining for PS/2 oversample**: each DMA channel transfers 1 KB then chains to a second channel that transfers the next 1 KB, then chains back. Half-buffer IRQ to CPU when one side is full. Classic ping-pong.

---

## 12. Validation strategy

### 12.1 Loopback tests (no DOS host required)

- Hook GP3 (KBD_CLK_PULL) to GP2 (KBD_CLK_IN) externally; same for DATA. PIO TX program emits frames; PIO RX program captures. CPU diffs the result. Verifies framing + parity computation.
- Hook two LPT pins (e.g. nibble output to forward data input) in a wrap-around adapter. PIO programs round-trip bytes.

### 12.2 Logic-analyzer validation

- For each PIO program, capture the GPIO waveform on a 2-channel scope or logic analyzer.
- Assert: edge timing matches spec ± 5%, glitch-free transitions, no overlap during direction changes.

### 12.3 Bench validation with DOS

- **PS/2:** insert Pico between an AT keyboard and an AT-class DOS PC. Sniff a few seconds of typing; classifier should report `MachineClass::At`. Replay sniffed bytes via TX into a USB CDC terminal to confirm decode.
- **LPT:** run Stage 1's `pump_stress_test` (8 PING/PONG round-trips). Each PIO mode promotion runs the same test and reports 0 errors before advancing.

### 12.4 Instrumentation surface

**Full specification:** [`instrumentation_surface.md`](instrumentation_surface.md) — console line formats for every event type, TUI dashboard mocks for all five views, the CDC telemetry JSON-line protocol, and the signature-database design that powers keyboard/chipset fingerprinting.

Brief summary: per-channel `Ps2Stats` and `LptStats` are emitted to defmt-RTT at 1 Hz during normal operation. Sample:

```
[12.345] KBD: 12 fr/s, 0 err, 0 glt, p50=81µs p95=82µs p99=83µs dty=52% skw=+0.3µs
[12.345] AUX: 0 fr/s, idle
[12.345] LPT: mode=SPP-Nibble, 7 pkt/s, 0 CRC err, avg=42ms/pkt
```

These lines are searchable in the user's terminal — the first sign that anything is wrong on a target machine will be a delta in p99 or a non-zero error count. The CDC telemetry channel emits the same data as structured JSON for consumption by the TUI dashboard (`tools/tui/`) or any JSON-aware tool.

---

## 13. Open issues and hardware prerequisites

### Required for full PIO operation

1. **Confirm nInit GPIO routing.** Stage 0/1 use LPT control bit 2 (nInit) as the host strobe in nibble mode. Hardware ref doesn't enumerate which Pico GPIO sees nInit through the 74LVC161284. Bring-up step 1.

2. **Add LPT data-direction GPIO.** For Phase 4+ modes that drive the data bus from the Pico (Byte-rev, EPP rev, ECP rev), a GPIO controlling the 74LVC161284 direction pin must exist. May already be wired but not documented; needs verification or addition.

3. **Confirm LPT status-bit GPIO assignments.** §10.2's CPU bit-reordering depends on the exact GP↔status mapping through the transceiver. Verify with a logic analyzer.

### Recommended for v2 board

4. **PS/2 CLK_IN / DATA_IN on adjacent GPIOs** per channel. Currently has the PULL pin in the middle; we work around with `in pins, 3` reading three pins and discarding the middle. Functional but ugly.

5. **PS/2 CLK_PULL / DATA_PULL on adjacent GPIOs** per channel. Same reasoning.

6. **LPT nibble + phase pins consecutive.** Currently has phase (GP24) between nibble bits. We pre-reorder in CPU (cheap) but a clean layout would let `out pins, 5` write the values directly without permutation.

### Future-phase decisions

7. **PSRAM coherency for ECP DMA.** Phase 5+ ECP modes feed DMA from PSRAM (large send buffers can't fit in SRAM). PSRAM-DMA path requires either uncached access or explicit cache flushes; pick a policy.

8. **PIO clock dividers when sys_clk rises.** All dividers above assume sys_clk = 150 MHz. If we boost to 200/250 MHz for VESA capture work, regenerate.

9. **Multi-core split.** Phys are designed to run on core 1 (interrupt executor) so the protocol task on core 0 can't stall them. Validated when Phase 5 lands.

---

## 14. Phasing summary

| Phase | New PIO programs | Open hardware |
|---|---|---|
| 0 | `ws2812_neopixel` only (status indicator) | — |
| 1 | `ps2_kbd_oversample`, `ps2_kbd_tx` | PS/2 pin layout (workable as-is) |
| 2 | `ps2_aux_oversample`, `ps2_aux_tx` | Same |
| 3 | `lpt_compat_in`, `lpt_nibble_out` | nInit GPIO routing (open), LPT status mapping (open) |
| 4 | `lpt_byte_rev` | Data-direction GPIO |
| 5 | `lpt_epp`, `lpt_ecp_fwd_dma`, `lpt_ecp_rev_dma` | Same + PSRAM DMA policy |

Phase 3 is dual-tracked: the bit-bang phy already exists (`firmware/src/lpt/compat.rs`) and works against the same wire protocol. The PIO version is a drop-in replacement once `nInit` routing is confirmed; the `LptPhy` trait abstracts both impls.

---

## 15. Related documents

- [`pico_firmware_design.md`](pico_firmware_design.md) — overall firmware architecture
- [`pico_phase3_design.md`](pico_phase3_design.md) — Phase 3+ MVP implementation slice
- [`hardware_reference.md`](hardware_reference.md) §3.3 — pin allocation
- [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) — controller-side wire semantics
- [`ps2_eras_reference.md`](ps2_eras_reference.md) — PS/2 framing variations across XT/AT/PS2
- [`ps2_private_channel_design.md`](ps2_private_channel_design.md) — i8042 unlock protocol the firmware serves
- Memory `ps2-oversampling-preference` — architectural decision motivating §6 and §8
- RP2350 datasheet §11 (PIO) — instruction set, FIFO behavior, side-set, IRQ flags
