# Pico1284 Hardware Reference

**Version:** 1.1  
**Date:** 2026-05-12  
**Target Board:** Adafruit Feather RP2350 HSTX with 8 MB PSRAM (Adafruit P/N 6130)  
**Also supports:** Raspberry Pi Pico 2 (RP2350) with pin-map adjustments — see §3.3 / §6

---

## 1. Core Microcontroller

**Chip:** Raspberry Pi RP2350A (on Adafruit Feather RP2350 HSTX)

**Key Specifications (relevant to Pico1284):**

| Feature                  | Specification                          | Relevance to Project |
|--------------------------|----------------------------------------|----------------------|
| CPU                      | Dual Cortex-M33 @ 150 MHz (or Hazard3 RISC-V) | High performance for compression + packet handling |
| SRAM                     | 520 KB (10 banks)                      | Working set, packet rings, hot code paths |
| External PSRAM           | 8 MB QSPI (AP6404L on Feather, GP8 = PCS) | Multi-frame screen buffers, dictionaries, file staging |
| External Flash           | 8 MB QSPI                              | Firmware + embedded Stage 1/2 blobs served to DOS |
| PIO                      | 3 blocks / 12 state machines           | **Critical** – one block for IEEE 1284, one for dual PS/2 |
| HSTX                     | High Speed Serial Transmit (up to 8 pins @ 150 MHz DDR) on GP12–GP19 | Reused as IEEE 1284 data bus today; future high-speed link |
| USB                      | USB 1.1 Full Speed (device + host capable) | Primary link to modern host (CDC ACM) over USB-C |
| GPIO                     | 29 usable 3.3 V GPIOs on Feather (GP8 reserved for PSRAM CS) | Requires external level shifting for 5 V interfaces |
| 5 V Tolerance            | None (external shifting required)      | Must use 74LVC161284 + level shifters |
| Debug                    | JST SH 3-pin SWD port (Pico Probe spec) | Plug-and-play SWD with `probe-rs` |

**Important Notes:**
- No native 5 V tolerant GPIOs.
- ADC pins (GPIO26–29) have reverse diodes — avoid exposing to 5 V when unpowered.
- Normal GPIOs (0–25) are safer when the chip is unpowered.
- **GP8 is the PSRAM chip-select on the Feather PSRAM variant — do not use as a regular GPIO.** Repurposing it will corrupt PSRAM access.
- **RP2350 A2 silicon E9 erratum** (the Feather ships with A2): high-impedance inputs and internal pull-downs can read high spuriously. If a design depends on a pull-down, use an external resistor ≤ 8.2 kΩ. PS/2 is unaffected (pull-ups sourced from the DOS PC's 5 V rail); IEEE 1284 status inputs that rely on weak pull-downs need explicit external resistors.

---

## 2. IEEE 1284 Parallel Port Interface

### 2.1 Transceiver Chip

**Recommended Part:** Texas Instruments **SN74LVC161284**

**Why this chip?**
- Specifically designed for IEEE 1284 (Centronics / parallel port) applications
- 8 bidirectional data lines with automatic direction control
- Proper handling of control and status lines
- 3.3 V logic on Pico side ↔ 5 V on parallel port side
- Built-in pull-up / termination support for legacy printers
- Proven in multiple Pico parallel port projects (e.g., Tom Verbeure's Fake Parallel Printer)

**Typical External Components (per datasheet + community designs):**
- 0.1 µF decoupling capacitor close to the chip
- Optional series resistors (22–33 Ω) on data lines for ringing suppression
- Pull-up resistors on certain status lines if needed by Pico firmware

### 2.2 Connector & Cabling

- **Connector:** DB-25 male (plugs directly into DOS LPT port)
- **Cable:** Standard IEEE 1284-compliant parallel cable (shortest possible recommended for high-speed ECP/EPP)
- **Pinout:** Standard Centronics / IEEE 1284-A

### 2.3 GPIO Allocation (Suggested)

| Function          | GPIOs Used | Notes |
|-------------------|------------|-------|
| Data 0–7          | 8 pins     | Bidirectional via 74LVC161284 |
| nStrobe / HostClk | 1 pin      | Output |
| nAutoFd / HostAck | 1 pin      | Output |
| nSelectIn / 1284Active | 1 pin | Output |
| nAck / PeriphClk  | 1 pin      | Input (with phase detection) |
| Busy / PeriphAck  | 1 pin      | Input |
| PError / nAckReverse | 1 pin   | Input |
| Select / Xflag    | 1 pin      | Input |
| nFault / nPeriphRequest | 1 pin | Input |
| **Total**         | **~14–16 pins** | Leaves plenty of headroom |

---

## 3. PS/2 Keyboard + AUX Interface (Revised)

### 3.1 Decision: Use 74LVC07A (Hex Open-Drain Buffer)

**We will use one single SN74LVC07A** for the entire PS/2 subsystem.

**Rationale**:
- PS/2 uses open-drain signaling (Clock + Data are bidirectional open-drain).
- The 74LVC07A provides 6 open-drain buffers in one cheap 14-pin package.
- One chip easily handles **both Keyboard and optional AUX/Mouse** (4 channels used, 2 spare).

### 3.2 Revised Wiring Diagram – Full System Overview

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                        Raspberry Pi Pico 2 (RP2350)                         │
│                                                                             │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │                        IEEE 1284 Section                             │   │
│  │                                                                      │   │
│  │   Pico GPIOs (3.3V)          74LVC161284          DB-25 Male         │   │
│  │   ─────────────────────┐     (IEEE 1284)        (to DOS LPT)         │   │
│  │   Data[0..7]           │────▶ A1–A8   B1–B8 ◀────▶ Data[0..7]        │   │
│  │   nStrobe/HostClk      │────▶ DIR/Control        │                   │   │
│  │   nAutoFd/HostAck      │                         │                   │   │
│  │   nSelectIn/1284Active │                         │                   │   │
│  │   nAck/PeriphClk       │◀──── Status bits        │                   │   │
│  │   Busy/PeriphAck       │                         │                   │   │
│  │   PError/nAckReverse   │                         │                   │   │
│  │   Select/Xflag         │                         │                   │   │
│  │   nFault/nPeriphReq    │                         │                   │   │
│  │                        │                         │                   │   │
│  │   VCC (3.3V) ──────────┼────▶ VCC                │                   │   │
│  │   GND                  │────▶ GND                │                   │   │
│  │   VCCABLE (5V)         │────▶ VCCABLE            │                   │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │                        PS/2 Section (74LVC07A)                       │   │
│  │                                                                      │   │
│  │   Pico GPIOs              74LVC07A (Hex Open-Drain)   PS/2 Connectors│   │
│  │   ─────────────────────┐                                             │   │
│  │   GP2  = KBD_CLK_IN    │◀──── Input 1              │            │    │   │
│  │   GP3  = KBD_CLK_PULL  │────▶ Output 1 (OD) ───────┼── 10kΩ ───▶│ KBD│   │
│  │   GP4  = KBD_DATA_IN   │◀──── Input 2              │   Pull-up  │    │   │
│  │   GP5  = KBD_DATA_PULL │────▶ Output 2 (OD) ───────┼───────────▶│    │   │
│  │                        │                           │            │    │   │
│  │   GP6  = AUX_CLK_IN    │◀──── Input 3              │            │ AUX│   │
│  │   GP28 = AUX_CLK_PULL  │────▶ Output 3 (OD) ───────┼── 10kΩ ───▶│    │   │
│  │   GP9  = AUX_DATA_IN   │◀──── Input 4              │   Pull-up  │    │   │
│  │   GP10 = AUX_DATA_PULL │────▶ Output 4 (OD) ───────┼───────────▶│    │   │
│  │                        │                           │            │    │   │
│  │   (Spare: Inputs 5 & 6 on 74LVC07A)                │            │    │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Power:                                                                     │
│    USB-C (VBUS 5V) ──▶ Pico VSYS + 74LVC161284 VCCABLE                      │
│    3V3 (Pico SMPS) ──▶ Pico logic + 74LVC07A VCC + pull-ups (optional)      │
│                                                                             │
│  Debug: GP0 = UART0_TX, GP1 = UART0_RX                                      │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 3.3 Recommended GPIO Allocation (Final)

| GPIO    | Function                  | Direction     | Connected To           | Notes |
|---------|---------------------------|---------------|------------------------|-------|
| GP0     | UART0_TX (Debug)          | Output        | USB-Serial adapter     | Development only; Feather "TX" label |
| GP1     | UART0_RX (Debug)          | Input         | USB-Serial adapter     | Feather "RX" label |
| GP2     | PS2_KBD_CLK_IN            | Input         | 74LVC07A Pin 2         | Read actual line state |
| GP3     | PS2_KBD_CLK_PULL          | Output        | 74LVC07A Pin 1         | Active-low pull-down |
| GP4     | PS2_KBD_DATA_IN           | Input         | 74LVC07A Pin 4         | — |
| GP5     | PS2_KBD_DATA_PULL         | Output        | 74LVC07A Pin 3         | — |
| GP6     | PS2_AUX_CLK_IN            | Input         | 74LVC07A Pin 6         | Optional |
| **GP7** | **Red LED (status)**      | Output        | On-board `#7` red LED  | Phase 0 smoke test; runtime status |
| GP8     | **RESERVED — PSRAM CS**   | —             | AP6404L PSRAM chip-select | **Do not use as GPIO** |
| GP9     | PS2_AUX_DATA_IN           | Input         | 74LVC07A Pin 8         | Optional |
| GP10    | PS2_AUX_DATA_PULL         | Output        | 74LVC07A Pin 7         | Optional |
| GP11    | nStrobe / HostClk         | Output        | 74LVC161284            | Feather "D11" |
| GP12–19 | Parallel Data 0–7         | Bidirectional | 74LVC161284 A-port     | **HSTX connector block** — also future high-speed link |
| GP20    | nAutoFd / HostAck         | Output        | 74LVC161284            | Feather "MI" |
| GP21    | NeoPixel (status)         | Output (WS2812) | On-board NeoPixel    | Multi-color runtime status indicator |
| GP22    | nSelectIn / 1284Active    | Output        | 74LVC161284            | Feather "SCK" |
| GP23    | nAck / PeriphClk          | Input         | 74LVC161284            | Phase detection; Feather "MO" |
| GP24    | Busy / PeriphAck          | Input         | 74LVC161284            | Feather "D24" |
| GP25    | PError / nAckReverse      | Input         | 74LVC161284            | Feather "D25" |
| GP26    | Select / Xflag            | Input         | 74LVC161284            | Feather "A0" (ADC capable, used digital) |
| GP27    | nFault / nPeriphRequest   | Input         | 74LVC161284            | Feather "A1" |
| GP28    | PS2_AUX_CLK_PULL          | Output        | 74LVC07A Pin 5         | Feather "A2"; AUX block split from GP6 here |
| GP29    | Spare                     | —             | —                      | Future ADC / diagnostic |

**Total GPIOs allocated:** 26 functional + 2 status LEDs (GP7 red, GP21 NeoPixel) + 1 reserved (GP8 PSRAM CS) + 1 spare (GP29). 29 of the Feather's 29 user-accessible GPIOs covered.

**On bare Pico 2:** the same logical functions can be mapped to GP0–GP25 if no PSRAM is present and the on-board LED on GP25 is sacrificed. The Feather pinout above is the reference; deviations should be documented per-build.

### 3.3 External Components (PS/2 Side)

Per PS/2 line:
- 10 kΩ pull-up resistor to **5 V** (sourced from DOS PC)
- Optional 100–220 Ω series resistor between 74LVC07A output and connector for protection

---

## 11. Recommended Package / Footprint Choices (2026)

### 11.1 74LVC161284 (IEEE 1284 Transceiver)

| Package       | Size              | Pitch   | Recommendation |
|---------------|-------------------|---------|----------------|
| **TSSOP-48**  | 12.5 × 6.1 mm     | 0.5 mm  | **Best choice** – smallest widely available, proven in Pico parallel projects |
| SSOP-48       | 15.9 × 7.5 mm     | 0.635 mm| Easier to hand-solder but larger |

**Note:** No QFN or BGA version exists for this part. The 0.5 mm TSSOP requires good soldering technique (fine tip + flux or hot air).

### 11.2 74LVC07A (Hex Open-Drain Buffer for PS/2)

| Package        | Size                    | Pitch   | Recommendation |
|----------------|-------------------------|---------|----------------|
| **DHVQFN14**   | 2.0 × 2.0 × 0.48 mm     | 0.4 mm  | Smallest, but requires reflow |
| **VQFN14**     | 3.5 × 3.5 mm            | 0.5 mm  | Very small, good compromise |
| **TSSOP14**    | 5.0 × 4.4 mm            | 0.65 mm | **Recommended for prototypes** – easiest to hand-solder |

**Recommendation for v1.0:**
- Use **TSSOP14** for the 74LVC07A on the first prototype.
- Use **TSSOP-48** for the 74LVC161284.

---

**This hardware reference complements the merged design document and provides the concrete electrical and pinout foundation needed to build the first prototype.** 

Ready for schematic capture or PCB layout when you are. Would you like me to generate a starter KiCad schematic or a more detailed pin assignment table?

**This revised wiring diagram is now clean, complete, and ready for schematic capture.** 

The 74LVC07A choice keeps the BOM minimal while giving full dual-PS/2 capability from day one. 

Would you like me to generate a **text-based netlist** or a **KiCad-friendly connection list** next? Or update the full hardware reference document with this new diagram?

### 3.3 GPIO Allocation (Robust Split Layout)

**Keyboard Endpoint (consecutive on Feather main header):**
- GP2 = KBD_CLK_IN (input, read actual line state)
- GP3 = KBD_CLK_PULL (output, active-low pull-down)
- GP4 = KBD_DATA_IN
- GP5 = KBD_DATA_PULL

**Optional AUX/Mouse Endpoint (split — avoids GP8 PSRAM CS):**
- GP6  = AUX_CLK_IN
- GP28 = AUX_CLK_PULL  *(moved from GP7 because GP7 is the red status LED, and from GP8 because GP8 is reserved for PSRAM CS on the Feather PSRAM variant)*
- GP9  = AUX_DATA_IN
- GP10 = AUX_DATA_PULL

**Total:** 8 GPIOs for full dual-PS/2 support. PIO state machines do not require the four AUX pins to be physically consecutive — the IN base and OUT base can be configured independently.

### 3.4 Connector

- Standard Mini-DIN 6-pin PS/2 female (on Pico side) or male pigtail that plugs into DOS PC keyboard port.
- Pinout (standard):
  - 1 = Data
  - 3 = GND
  - 4 = +5 V (from DOS PC — do **not** back-power the Pico from this if possible)
  - 5 = Clock

---

## 4. USB Connection to Modern Host

- **Interface:** USB 1.1 Full Speed (CDC ACM class)
- **Connector:** USB-C (on Pico 2) or micro-USB
- **Power:** Pico can be powered from this port (VBUS)
- **Speed:** ~800–1000 kB/s practical sustained throughput (bottleneck for large screen dumps)

**Future Option:** Use RP2350 **HSTX** (High Speed Serial Transmit) for a faster custom link if USB CDC becomes limiting.

---

## 5. Power Architecture

| Rail       | Source                  | Notes |
|------------|-------------------------|-------|
| VBUS       | USB-C from modern host  | 5 V input, powers Pico SMPS |
| VSYS       | VBUS or external 5 V    | Can be OR'd with Schottky / ideal diode |
| 3V3        | On-board RT6150 SMPS    | Main logic rail (~300 mA available) |
| 5 V (PS/2) | Sourced from DOS PC     | Only for PS/2 pull-ups; do not back-power Pico |

**Important:** When the DOS PC is powered on, the parallel port and PS/2 ports are live at 5 V. The 74LVC161284 and level shifters provide isolation so the Pico can be powered from USB only.

---

## 6. Recommended GPIO Layout (Feather RP2350 HSTX + 8 MB PSRAM)

```
GP0  = UART0_TX (debug)              [Feather: TX]
GP1  = UART0_RX (debug)              [Feather: RX]

GP2  = PS2_KBD_CLK_IN                [Feather: SDA]
GP3  = PS2_KBD_CLK_PULL              [Feather: SCL]
GP4  = PS2_KBD_DATA_IN               [Feather: D4]
GP5  = PS2_KBD_DATA_PULL             [Feather: D5]

GP6  = PS2_AUX_CLK_IN                [Feather: D6]
GP7  = RED LED (status)              [Feather: D13 / on-board red LED]
GP8  = RESERVED (PSRAM chip-select)  [Feather: PCS — DO NOT USE]
GP9  = PS2_AUX_DATA_IN               [Feather: D9]
GP10 = PS2_AUX_DATA_PULL             [Feather: D10]
GP11 = nStrobe / HostClk             [Feather: D11]

GP12 = Parallel Data 0               [Feather: HSTX D2P]
GP13 = Parallel Data 1               [Feather: HSTX D2N]
GP14 = Parallel Data 2               [Feather: HSTX CKP]
GP15 = Parallel Data 3               [Feather: HSTX CKN]
GP16 = Parallel Data 4               [Feather: HSTX D1P]
GP17 = Parallel Data 5               [Feather: HSTX D1N]
GP18 = Parallel Data 6               [Feather: HSTX D0P]
GP19 = Parallel Data 7               [Feather: HSTX D0N]

GP20 = nAutoFd / HostAck             [Feather: MI]
GP21 = NEOPIXEL (status)             [Feather: on-board NeoPixel]
GP22 = nSelectIn / 1284Active        [Feather: SCK]
GP23 = nAck / PeriphClk              [Feather: MO]
GP24 = Busy / PeriphAck              [Feather: D24]
GP25 = PError / nAckReverse          [Feather: D25]
GP26 = Select / Xflag                [Feather: A0  (ADC0, used digital)]
GP27 = nFault / nPeriphRequest       [Feather: A1  (ADC1, used digital)]
GP28 = PS2_AUX_CLK_PULL              [Feather: A2  (ADC2, used digital)]
GP29 = Spare                         [Feather: A3  (ADC3, future use)]
```

The IEEE 1284 8-bit data bus lives on the Feather's HSTX connector (GP12–GP19), giving a clean physical grouping for ribbon-cable routing today and preserving the path to a future high-speed HSTX-driven link per §10 without re-wiring.

This layout uses all 29 user-accessible GPIOs on the Feather: 26 functional pins, 2 status LEDs (red + NeoPixel), and 1 spare (GP29). GP8 is reserved by the PSRAM chip-select.

---

## 7. Level Shifting & Protection Summary

| Interface       | Level Shifter              | Protection Notes |
|-----------------|----------------------------|------------------|
| IEEE 1284       | 74LVC161284 (mandatory)    | Handles direction, termination, 5 V tolerance |
| PS/2 Keyboard   | BSS138 or 74LVC07 + input  | Open-drain pull-low + 5 V safe input |
| PS/2 AUX        | Same as above              | Optional but recommended |
| Debug UART      | Direct 3.3 V (or level shift if needed) | For development only |

---

## 8. Bill of Materials (Core)

| Part                        | Qty | Notes |
|-----------------------------|-----|-------|
| Adafruit Feather RP2350 HSTX + 8 MB PSRAM (P/N 6130) | 1 | RP2350A board with 8 MB flash and 8 MB QSPI PSRAM populated |
| Adafruit FPC Breakout for Raspberry Pi 5 DSI or RP2350 HSTX, 22-pin 0.5 mm | 1 | Breadboard-friendly adapter for the Feather's HSTX FPC port; carries Parallel D0–D7 (GP12–GP19) and optionally nStrobe (GP11) |
| 22-pin 0.5 mm-pitch FPC flex cable (~100 mm) | 1 | Feather HSTX port ↔ FPC breakout |
| 74LVC161284 (TSSOP-48)      | 1   | IEEE 1284 transceiver — see §11.1 |
| 74LVC07A (TSSOP-14)         | 1   | Hex open-drain buffer for PS/2 — see §11.2 |
| DB-25 male connector        | 1   | Right-angle or straight |
| Mini-DIN 6-pin (PS/2)       | 1–2 | Keyboard + optional AUX |
| 10 kΩ pull-up resistors     | 4–8 | For PS/2 lines (pulled to DOS PC's 5 V rail) |
| 0.1 µF ceramic capacitors   | 4+  | Decoupling (near chips) |
| 22–33 Ω series resistors    | 8   | Optional on parallel data lines |
| Raspberry Pi Debug Probe (or equivalent) | 1 | Plugs into Feather's JST SH 3-pin SWD port |
| USB-C connector / cable     | 1   | For modern host link |

---

## 9. Electrical & Safety Notes

- **Do not back-power the Pico** from the DOS PC's parallel or PS/2 ports when the Pico is USB-powered.
- The 74LVC161284 provides good isolation on the parallel side.
- For PS/2, the level shifters + series resistors provide basic protection.
- On very old XT machines, parallel port timing can be extremely slow — the conservative nibble protocol in `s0_xt.asm` is appropriate.

---

## 10. Future Hardware Extensions

- Add **HSTX** connector for >10 MB/s custom link to modern host (bypassing USB CDC limit)
- Add **SD card** or **PSRAM** for large screen/frame buffering
- Add **status LEDs** + **user button** for mode selection / recovery
- Create a **carrier board** that combines everything into one clean PCB with proper connectors

---

**This hardware reference complements the merged design document and provides the concrete electrical and pinout foundation needed to build the first prototype.** 

Ready for schematic capture or PCB layout when you are. Would you like me to generate a starter KiCad schematic or a more detailed pin assignment table?
