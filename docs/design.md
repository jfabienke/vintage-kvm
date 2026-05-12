# Pico1284 + Dual-PS/2 Bootstrap Bridge Design

**Version:** 1.1  
**Date:** 2026-05-11  
**Status:** Merged design artifact  
**Primary platform:** Raspberry Pi Pico 2 / RP2350  
**Vintage host:** DOS PC with LPT and PS/2 keyboard/mouse ports  
**Modern host:** USB-C host PC  

---

## 1. Executive Summary

This design merges two complementary projects:

1. **Pico1284 High-Speed Parallel Port Bridge**  
   A high-speed IEEE 1284 bridge between a vintage DOS PC and a modern host PC, using the DOS PC parallel port as the main data plane and the RP2350 Pico 2 as an intelligent peripheral connected to the modern host over USB CDC.

2. **Dual-PS/2 Bootstrap + Control Bridge**  
   A PS/2 keyboard and optional AUX/mouse interface implemented in PIO/GPIO, allowing the Pico to boot as a normal PS/2 keyboard, inject a tiny DOS bootstrap through `DEBUG`, and later switch into a proprietary bidirectional protocol through the PC keyboard controller.

The merged architecture uses:

```text
Parallel IEEE 1284:
    primary high-speed data plane
    file transfer
    screen dumps
    memory access
    remote command execution
    bulk transport

PS/2 keyboard + AUX:
    zero-media bootstrap
    keyboard injection
    fallback/recovery channel
    low-speed control channel
    emergency loader if LPT mode negotiation fails
```

The key architectural decision is to **separate bootstrap/control from bulk transport**.

The PS/2 path is universal and useful even on a bare DOS prompt, but it is limited by the PC i8042-compatible controller. The IEEE 1284 path is much faster and should carry all serious traffic once the DOS-side loader/TSR is installed.

---

## 2. Primary Use Cases

### 2.1 High-Speed Use Cases over IEEE 1284

- Screen dumps.
- File transfers.
- Console I/O.
- Memory access.
- Remote command execution.
- Diagnostics.
- DOS TSR/CLI communication.
- Optional remote debugger features.
- Transfer of larger Stage 1 / Stage 2 DOS tools.

### 2.2 Bootstrap and Recovery Use Cases over PS/2

- Type a DOS `DEBUG` script without any disk, floppy, serial, or preinstalled software.
- Generate a tiny `STAGE0.COM`.
- Unlock the Pico into private keyboard-controller mode.
- Transfer a larger loader if the parallel path is not yet available.
- Recover from broken LPT negotiation or missing DOS driver.
- Provide a low-speed out-of-band control channel.
- Optionally use keyboard+AUX as a slow fallback data link.

---

## 3. Combined Hardware Architecture

### 3.1 Pico Side

Target board:

```text
Raspberry Pi Pico 2 / RP2350
```

Core hardware:

```text
RP2350 Pico 2
    + 74LVC161284 IEEE 1284 level shifter/transceiver
    + DB-25 male connector for LPT
    + PS/2 male keyboard connector wired to PIO/GPIO
    + optional PS/2 male mouse/AUX connector wired to PIO/GPIO
    + USB-C or micro-USB connection to modern host
```

RP2350 resources:

```text
PIO:
    IEEE 1284 negotiation and data handshaking
    PS/2 keyboard device endpoint
    optional PS/2 AUX/mouse device endpoint

DMA:
    Parallel port data movement
    USB buffering
    screen/file packet buffers

CPU:
    Embassy/Rust async tasks
    compression dispatch
    session state
    packet routing
    bootstrap state machine

HSTX:
    reserved for future high-speed extensions
```

### 3.2 DOS Side

Vintage PC interfaces:

```text
LPT1:
    base 0x378 by default
    SPP / PS/2 / EPP / ECP capability detection
    IEEE 1284 negotiation

PS/2 keyboard:
    used by Pico as a PS/2 keyboard device
    optional private keyboard-controller transport

PS/2 mouse/AUX:
    optional second PS/2 private lane
```

DOS software:

```text
STAGE0.COM:
    tiny bootstrap created via DEBUG or typed-in hex
    owns i8042 keyboard controller
    unlocks Pico
    receives larger loader if needed

STAGE1.COM:
    small loader
    probes LPT hardware
    performs IEEE 1284 negotiation
    starts reliable packet exchange

PICO1284.EXE or TSR:
    full DOS-side client
    NASM + Open Watcom C
    file transfer
    screen capture
    memory access
    command execution
    compression
```

### 3.3 Modern Host Side

Modern host connection:

```text
USB CDC ACM over USB full-speed
```

Host software can be written in:

```text
Python
Rust
C#
Go
native C/C++
```

Functions:

- Receive screen dumps.
- Send/receive files.
- Issue remote commands.
- Display terminal stream.
- Store logs.
- Decode compressed streams.
- Manage sessions and device updates.

---

## 4. Combined System Diagram

```text
                              ┌─────────────────────────────────┐
                              │      Modern Host PC (USB-C)      │
                              │ Python / Rust / C# / Native App  │
                              └────────────────┬────────────────┘
                                               │
                                               │ USB CDC ACM
                                               │ bulk-ish host stream
                                               ▼
╔══════════════════════════════════════════════════════════════════════════════╗
║                         RP2350 Pico 2 / Pico1284                           ║
╟──────────────────────────────────────────────────────────────────────────────╢
║ Application Layer                                                           ║
║   Console / File / Screen / Memory / Command / Diagnostics                  ║
║                                                                              ║
║ Compression Layer                                                            ║
║   LZ4 / LZSS / Delta+RLE / RLE / raw fallback                               ║
║                                                                              ║
║ Packet Layer                                                                 ║
║   [SOH CMD SEQ LEN PAYLOAD CRC-16 ETX]                                      ║
║                                                                              ║
║ Capability + Session Layer                                                   ║
║   capability request/response/ack, traffic profiles, dictionaries           ║
║                                                                              ║
║ Data Plane                                                                   ║
║   IEEE 1284 negotiation + ECP/EPP/SPP/Byte/Compatibility PIO + DMA          ║
║                                                                              ║
║ Bootstrap / Control Plane                                                    ║
║   PS/2 keyboard endpoint, optional AUX endpoint, DEBUG bootstrap, fallback  ║
╚══════════════════════════════════════════════════════════════════════════════╝
             │                                              │
             │ IEEE 1284 DB-25                              │ PS/2 keyboard/AUX
             │ high-speed data plane                        │ bootstrap/fallback
             ▼                                              ▼
╔══════════════════════════════════════════════════════════════════════════════╗
║                            Vintage DOS PC                                   ║
╟──────────────────────────────────────────────────────────────────────────────╢
║ DOS Application / TSR                                                        ║
║   screen dump, file transfer, console, CLI, memory access                    ║
║                                                                              ║
║ DOS Compression                                                              ║
║   RLE / Delta+RLE / static dictionary / raw                                  ║
║                                                                              ║
║ Packet Layer                                                                 ║
║   same SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX framing                               ║
║                                                                              ║
║ Capability + Session Layer                                                   ║
║   computes feature intersection and agreed settings                          ║
║                                                                              ║
║ IEEE 1284 Layer                                                              ║
║   port detection, ECP/EPP registers, negotiation, packet I/O                 ║
║                                                                              ║
║ PS/2 Bootstrap Layer                                                         ║
║   BIOS/DOS keyboard input, DEBUG script, STAGE0, i8042 private mode         ║
╚══════════════════════════════════════════════════════════════════════════════╝
```

---

## 5. Interface Roles

### 5.1 IEEE 1284 Parallel Interface

The parallel port is the **primary data plane**.

Responsibilities:

- Main bidirectional transport.
- High-speed data transfer.
- Reliable packet exchange.
- Screen dump streaming.
- File transfers.
- Memory read/write.
- Remote command execution.
- Stage 1 / Stage 2 loader delivery after basic bootstrap.

Expected speed regime:

```text
ECP/EPP on good hardware:
    2–8+ MB/s raw link potential

USB CDC full-speed:
    ~800–1000 kB/s sustained practical ceiling

Effective compressed screen throughput:
    10–30+ MB/s apparent for highly compressible/delta screen data,
    while the USB CDC output path may still limit host-visible throughput.
```

### 5.2 PS/2 Keyboard + AUX Interface

The PS/2 path is the **bootstrap, fallback, and recovery plane**.

Responsibilities:

- Act as a normal PS/2 keyboard at boot.
- Type commands into DOS.
- Create `STAGE0.COM` using `DEBUG`.
- Allow `STAGE0.COM` to communicate with the Pico through the i8042.
- Optionally activate keyboard+AUX dual-lane private transport.
- Recover the system if parallel negotiation fails.

Expected speed regime through i8042:

```text
single PS/2 lane:
    ~0.9–1.5 kB/s raw

keyboard + AUX aggregate:
    ~1.8–3.0 kB/s raw theoretical
    ~1.5–2.5 kB/s useful mixed traffic
```

The PS/2 path is not a substitute for IEEE 1284. It is a universal bootstrap and control mechanism.

---

## 6. Hardware Detail

### 6.1 IEEE 1284 Hardware

Pico side:

```text
RP2350 Pico 2
    ⇄ 74LVC161284 IEEE 1284 transceiver/level shifter
    ⇄ DB-25 male connector
    ⇄ IEEE 1284-compliant cable
    ⇄ DOS PC LPT port
```

The `74LVC161284` provides suitable IEEE 1284 line interface behavior and protects the Pico-side logic from raw parallel-port electrical characteristics.

DOS side:

```text
LPT1 base:
    commonly 0x378

Supported modes:
    Compatibility/SPP
    PS/2 bidirectional byte mode
    EPP
    ECP
```

### 6.2 PS/2 Hardware

The Pico is wired as a PS/2 device to the DOS PC.

Keyboard connector:

```text
DOS PC PS/2 keyboard port ⇄ Pico PIO/GPIO PS/2 keyboard endpoint
```

Optional AUX connector:

```text
DOS PC PS/2 mouse port ⇄ Pico PIO/GPIO PS/2 AUX endpoint
```

PS/2 signals:

```text
Pin 1 = DATA
Pin 3 = GND
Pin 4 = +5 V
Pin 5 = CLK
```

The Pico GPIOs are **not 5 V tolerant**. Use open-drain buffering or proper level shifting.

Recommended robust design:

```text
RP2350 GPIO/PIO
    ⇄ 3.3 V safe input/output side
    ⇄ open-drain buffer / level shifter
    ⇄ 5 V PS/2 CLK/DATA lines
```

Suitable options:

```text
BSS138 bidirectional MOSFET level shifter
74LVC07 / 74LVC06 open-drain buffer plus input sensing
discrete NMOS/NPN pull-down stage with protected input path
```

### 6.3 Suggested GPIO Layout

#### Compact prototype layout

```text
GP0  = UART0_TX debug
GP1  = UART0_RX debug

GP2  = PS2_KBD_CLK
GP3  = PS2_KBD_DATA
GP4  = PS2_AUX_CLK
GP5  = PS2_AUX_DATA

GP25 = status LED
```

This assumes bidirectional open-drain-safe level shifting.

#### Robust split input/output layout

For explicit pull-low control:

```text
Keyboard endpoint:
    GP2  = KBD_CLK_IN
    GP3  = KBD_CLK_PULLLOW
    GP4  = KBD_DATA_IN
    GP5  = KBD_DATA_PULLLOW

AUX endpoint:
    GP6  = AUX_CLK_IN
    GP7  = AUX_CLK_PULLLOW
    GP8  = AUX_DATA_IN
    GP9  = AUX_DATA_PULLLOW

Debug:
    GP0  = UART0_TX
    GP1  = UART0_RX
    GP25 = status LED
```

Each PS/2 line:

```text
PS/2 line ──────────────── level-safe Pico input
        └─────────────── open-drain pull-low transistor/buffer controlled by Pico
```

Logic:

```text
PULLLOW = 1  → pull PS/2 line low
PULLLOW = 0  → release line
IN reads actual line state
```

---

## 7. Bootstrapping Strategy

### 7.1 Bootstrap Ladder

```text
Stage -1: PS/2 keyboard injection
    Pico behaves as normal PS/2 keyboard.
    Pico types DEBUG script into DOS.

Stage 0: DEBUG-created STAGE0.COM
    Tiny program.
    Owns i8042.
    Unlocks Pico.
    Optionally receives Stage 1 over PS/2 if LPT path is unavailable.

Stage 1: Loader
    Detects LPT hardware.
    Negotiates IEEE 1284 mode.
    Starts packet protocol.
    Can load full TSR/CLI.

Stage 2: Full DOS TSR/CLI
    File transfer.
    Screen dump.
    Console I/O.
    Memory access.
    Remote command execution.
    Compression.
```

### 7.2 DEBUG Bootstrap

The Pico types a `DEBUG` script using normal PS/2 scan codes.

Preferred method: hex-entry mode rather than DEBUG assembly.

Example structure:

```dos
DEBUG STAGE0.COM
E 100 <hex bytes>
E 110 <hex bytes>
E 120 <hex bytes>
...
RCX
<program length in hex>
W
Q
STAGE0
```

Advantages:

- No disk required.
- No serial/parallel driver required.
- No existing file transfer required.
- More deterministic than assembly input.
- Small Stage 0 minimizes fragile keyboard-injection time.

### 7.3 Stage 0 Responsibilities

`STAGE0.COM` should be tiny.

Required:

- Disable interrupts or mask IRQ1/IRQ12.
- Flush i8042 output buffer.
- Send legal PS/2 unlock sequence to Pico.
- Verify Pico response/signature.
- Detect or invoke Stage 1 path.

Preferred behavior:

```text
If IEEE 1284 path works:
    load/launch Stage 1 via LPT

If IEEE 1284 path fails:
    receive Stage 1 over PS/2 private keyboard lane

If everything fails:
    return Pico to keyboard mode and exit
```

### 7.4 PS/2 Unlock Sequence

Use legal PS/2 keyboard commands.

Example LED-based unlock:

```text
0xED 0x00
0xED 0x07
0xED 0x00
0xED 0x05
0xED 0x02
```

The Pico ACKs as a normal keyboard until the full pattern is recognized, then enters private mode.

### 7.5 Recovery Behavior

The Pico should support:

```text
power-cycle → PS/2 keyboard mode
hardware button at boot → recovery keyboard mode
private command → return to PS/2 keyboard mode
timeout in private mode → optional return to keyboard mode
parallel negotiation failure → keep PS/2 fallback alive
```

---

## 8. IEEE 1284 Negotiation and Autodetection

### 8.1 Pico-Side Negotiation Watcher

A dedicated PIO state machine watches for the IEEE 1284 negotiation sequence:

```text
nSelectIn high
nAutoFd low
extensibility byte on data bus
```

The Pico responds with the correct status lines and pushes the request byte to a Rust task.

### 8.2 Dynamic PIO Program Selection

Rust session task selects mode:

```text
Try ECP first
then EPP
then Byte mode
then Compatibility/SPP fallback
```

The Pico can dynamically load or activate the matching PIO program:

```text
ECP PIO program
EPP PIO program
Byte-mode PIO program
Compatibility-mode PIO program
```

### 8.3 DOS-Side Detection

The DOS side:

1. Detects likely LPT base ports.
2. Defaults to `0x378` if appropriate.
3. Probes port registers.
4. Checks for ECP/EPP support if available.
5. Initiates IEEE 1284 negotiation.
6. Falls back to lower modes if needed.

Default order:

```text
ECP → EPP → Byte/PS/2 → Compatibility/SPP
```

---

## 9. Packet Format

Use the same logical packet framing on both the IEEE 1284 and PS/2 private transport, where practical.

Base packet:

```text
[SOH 0x01]
[CMD 1 byte]
[SEQ 1 byte]
[LEN 2 bytes BE]
[PAYLOAD LEN bytes]
[CRC-16 2 bytes]
[ETX 0x03]
```

CRC:

```text
CRC-16/CCITT
polynomial 0x1021
```

The packet layer is independent of the physical transport. IEEE 1284 carries packets directly. PS/2 carries packets fragmented into much smaller lane frames.

### 9.1 Core Commands

Suggested command ranges:

```text
0x00       CAP_REQ
0x0F       CAP_RSP
0x0E       CAP_ACK

0x10       PING
0x11       PONG
0x12       RESET_SESSION
0x13       ERROR
0x14       CREDIT
0x15       ACK
0x16       NAK

0x20       SEND_BLOCK
0x21       RECV_BLOCK
0x22       BLOCK_ACK
0x23       BLOCK_NAK

0x30       FILE_OPEN
0x31       FILE_DATA
0x32       FILE_CLOSE
0x33       FILE_ACK
0x34       FILE_ERROR

0x40       SCREEN_DUMP_START
0x41       SCREEN_DUMP_DATA
0x42       SCREEN_DUMP_END
0x43       SCREEN_DUMP_ACK
0x44       SCREEN_MODE_INFO
0x45       SCREEN_PALETTE
0x46       SCREEN_REGION

0x50       CONSOLE_DATA
0x51       CONSOLE_CONTROL

0x60       MEM_READ
0x61       MEM_WRITE
0x62       MEM_DATA

0x70       EXEC_CMD
0x71       EXEC_RESULT

0x80       DICT_SELECT
0x81       CODEC_SELECT
```

---

## 10. Capability Discovery

### 10.1 Handshake Sequence

After physical negotiation:

```text
DOS → Pico: CAPABILITY_REQUEST
Pico → DOS: CAPABILITY_RESPONSE
DOS computes intersection
DOS → Pico: CAPABILITY_ACK
```

### 10.2 Pico Capability Response Fields

Suggested byte layout:

```text
version_major              u8
version_minor              u8
max_packet_size            u16
max_fragment_size          u16
supported_traffic          u32
features                   u32
dos_compr_supported        u32
pico_compr_supported       u32
preferred_parallel_mode    u8
active_parallel_mode       u8
ps2_fallback_supported     u8
buffer_kb                  u16
usb_mode                   u8
device_string_len          u8
device_string              bytes
```

Traffic bitfield examples:

```text
bit 0  Console
bit 1  File transfer
bit 2  Screen dump
bit 3  Memory access
bit 4  Remote command execution
bit 5  Diagnostics/logging
bit 6  PS/2 fallback transport
bit 7  VESA graphics
```

Feature bitfield examples:

```text
bit 0  CRC-16
bit 1  CRC-32
bit 2  Sliding window
bit 3  Credit-based flow control
bit 4  LZ4
bit 5  LZSS
bit 6  RLE
bit 7  Delta+RLE
bit 8  Static dictionaries
bit 9  Text screen diff
bit 10 VESA tile diff
bit 11 Palette delta
bit 12 USB CDC forwarding
```

Compression IDs:

```text
0x00 RAW
0x01 RLE
0x02 DELTA_RLE
0x03 STATIC_DICT
0x04 LZSS
0x05 LZ4_FAST
0x06 LZ4_HC optional
```

---

## 11. Compression Strategy

### 11.1 Asymmetric Design

DOS PC:

```text
Target CPU example:
    486DX2-50 MHz

Use:
    raw
    RLE
    Delta+RLE
    static dictionary for console
    simple tile/diff encoders

Avoid initially:
    heavy LZ search
    Huffman coding
    complex dynamic compressors
```

Pico / RP2350:

```text
Use:
    Delta+RLE
    LZ4 fast
    optional LZSS/LZ4-HC-like heavier parsing
    previous-frame buffer
    dictionary transforms
    recompression for USB host stream
```

### 11.2 DOS Compression Performance Targets

For a 486DX2-50 MHz-class system:

```text
RLE:
    2–4 MB/s
    roughly 40–75 ms for a 150 KB VGA-ish screen

Delta+RLE:
    1.2–2.8 MB/s
    roughly 55–125 ms for a 150 KB screen
    excellent ratios on sparse screen changes
```

### 11.3 Pico Compression Performance Targets

RP2350-class expectations:

```text
Delta+RLE:
    60–120 MB/s

LZ4 fast compression:
    35–70 MB/s

LZ4 decompression:
    300–600+ MB/s class
```

These numbers comfortably exceed the IEEE 1284 and USB CDC throughput ceilings, so the Pico can spend cycles on compression and format conversion.

### 11.4 Directional Policy

DOS → Pico:

```text
screen dumps:
    RLE or Delta+RLE

console traffic:
    static dictionary + RLE

file upload:
    raw initially
    optional RLE/static dictionary where useful

memory dumps:
    Delta+RLE if previous baseline exists
```

Pico → DOS:

```text
file download:
    LZ4/LZSS/Delta+RLE where useful

commands/control:
    raw or static dictionary

screen/control metadata:
    raw or small RLE

Stage transfer:
    compressed if DOS loader supports decompression
```

Pico → Modern Host:

```text
forward as:
    raw
    RLE/Delta+RLE
    LZ4-compressed blob
    host-selected format
```

---

## 12. Screen Dump Protocol

### 12.1 Commands

```text
0x40 SCREEN_DUMP_START
0x41 SCREEN_DUMP_DATA
0x42 SCREEN_DUMP_END
0x43 SCREEN_DUMP_ACK
0x44 SCREEN_MODE_INFO
0x45 SCREEN_PALETTE
0x46 SCREEN_REGION
```

### 12.2 SCREEN_DUMP_START Payload

Original 14-byte baseline can be extended.

Baseline:

```text
mode                 u16
width                u16
height               u16
depth                u8
dos_compression      u8
frame_number         u32
uncompressed_size    u32
```

Recommended extended format:

```text
mode                 u16
width                u16
height               u16
depth                u8
pixel_format         u8
dos_compression      u8
flags                u8
frame_number         u32
uncompressed_size    u32
encoded_size_hint    u32
pitch_bytes          u16
tile_w               u8
tile_h               u8
palette_id           u16
prev_frame_crc32     u32
```

### 12.3 SCREEN_DUMP_DATA Payload

```text
frame_number         u32
offset               u32
chunk_len            u16
chunk_flags          u8
payload              bytes
```

For tile mode, payload may contain tile opcodes rather than raw framebuffer bytes.

### 12.4 SCREEN_DUMP_END Payload

```text
frame_number         u32
uncompressed_crc32   u32
encoded_crc32        u32 optional
total_chunks         u16
```

### 12.5 SCREEN_DUMP_ACK Payload

```text
frame_number         u32
status               u8
missing_offset/count optional
```

Status examples:

```text
0 = OK
1 = CRC_FAILED
2 = MISSING_CHUNK
3 = UNSUPPORTED_MODE
4 = RESYNC_REQUIRED
```

---

## 13. Text-Mode Screen Diffing

Text mode should use a dedicated cell diff codec, not generic framebuffer compression.

### 13.1 Basic Text Buffer

Standard color text mode:

```text
80 × 25 × 2 = 4000 bytes
```

Each cell:

```text
char byte
attribute byte
```

### 13.2 Supported Text Modes

Start:

```text
80×25 color at B800:0000
```

Then add:

```text
80×25 mono at B000:0000
40×25
80×43
80×50
alternate display pages
```

### 13.3 Text Diff Opcodes

Maintain previous and current cell arrays.

Opcodes:

```text
FULL_SCREEN_BEGIN mode,width,height,cursor
SKIP n
LITERAL n, cells...
FILL n, char, attr
ATTR_RUN n, attr, chars...
END crc
```

Frame integrity:

```text
frame_no
prev_crc32
new_crc32
```

Receiver behavior:

```text
if local_crc != prev_crc:
    request full frame
else:
    apply diff
    verify new_crc
```

### 13.4 Expected Encoded Sizes

| Update | Typical encoded size |
|---|---:|
| One character typed | ~5–15 B |
| One command line edited | ~20–120 B |
| One new output line | ~20–200 B |
| Several lines output | ~100–800 B |
| Full directory page | ~500–2000 B |
| Random text+attributes | ~4000 B, use raw/full fallback |

---

## 14. VESA Graphics Support

### 14.1 Design Principle

Support all VESA modes through a mode-descriptor-driven capture engine.

Do not hardcode only common resolutions.

Use:

```text
VBE mode query
framebuffer descriptor
tile grid
tile hash/diff
tile-specific codecs
palette handling
ROI/progressive refresh
```

### 14.2 VBE Mode Metadata

Required fields:

```c
typedef struct {
    uint16_t mode;
    uint16_t width;
    uint16_t height;
    uint16_t pitch_bytes;
    uint8_t  bpp;
    uint8_t  memory_model;

    uint8_t  red_mask_size;
    uint8_t  red_field_pos;
    uint8_t  green_mask_size;
    uint8_t  green_field_pos;
    uint8_t  blue_mask_size;
    uint8_t  blue_field_pos;
    uint8_t  rsvd_mask_size;
    uint8_t  rsvd_field_pos;

    uint32_t framebuffer_phys;
    uint8_t  has_lfb;
    uint8_t  banked;
} video_mode_info_t;
```

VBE calls:

```text
4F03h = get current mode
4F01h = get mode information
4F05h = bank switching
```

### 14.3 Mode Classes

| Class | Examples | Strategy |
|---|---|---|
| Legacy text | 80×25, 80×50 | Cell diff |
| Planar VGA/EGA | 16-color planar | Plane-aware capture |
| Packed 8 bpp | 320×200×8, 640×480×8 | Tile diff + palette |
| 15/16 bpp RGB | RGB555/RGB565 | Native tile diff |
| 24 bpp RGB | RGB888 | Tile diff, expensive raw |
| 32 bpp RGB | XRGB8888 | Tile diff, optional alpha/drop |
| Banked VBE | older VBE | Banked window capture |
| LFB VBE | VBE 2.0+ | Preferred capture path |

### 14.4 Tile Protocol

Suggested tile sizes:

| Mode | Tile size |
|---|---:|
| 8 bpp | 16×16 or 16×8 |
| 15/16 bpp | 8×8 or 16×8 |
| 24/32 bpp | 8×8 |
| high-res | 8×8 or 16×8 |

Tile opcodes:

```text
SKIP_TILES n
TILE_SOLID color
TILE_2COLOR color0,color1,bitmap
TILE_4COLOR optional
TILE_RLE payload
TILE_XOR_RLE payload
TILE_LZ payload
TILE_RAW payload
```

### 14.5 Palette Handling

For 8 bpp modes:

```text
PALETTE_FULL 256 × RGB
PALETTE_DELTA changed entries
palette_id
```

If the palette changes, unchanged pixel indexes may render differently. A significant palette change should trigger a full visual refresh or receiver-side re-render.

### 14.6 Raw Size Reality

| Mode | Raw frame size |
|---|---:|
| 640×480×8 | 300 KiB |
| 640×480×16 | 600 KiB |
| 800×600×8 | 469 KiB |
| 800×600×16 | 938 KiB |
| 1024×768×8 | 768 KiB |
| 1024×768×16 | 1.5 MiB |
| 1024×768×32 | 3 MiB |

Over IEEE 1284 this is feasible. Over PS/2 fallback it is only feasible with aggressive ROI/tile/delta and patience.

### 14.7 Region of Interest and Progressive Refresh

Support:

```text
CAPTURE_RECT x,y,w,h
FOLLOW_CURSOR
MAX_BYTES_PER_FRAME
MAX_TILES_PER_UPDATE
FULL_SCREEN_PROGRESSIVE
```

This keeps the UI responsive and avoids blocking for large screens.

---

## 15. File Transfer

### 15.1 Packetized File Transfer

Commands:

```text
FILE_OPEN
FILE_DATA
FILE_CLOSE
FILE_ACK
FILE_ERROR
```

Suggested file data block:

```c
struct file_data_block {
    uint16_t file_id;
    uint32_t block_no;
    uint32_t file_offset;
    uint16_t raw_len;
    uint8_t  enc_mode;
    uint16_t enc_len;
    uint32_t crc32;
    uint8_t  payload[];
};
```

### 15.2 Recommended Block Sizes

IEEE 1284 path:

```text
4 KiB to 64 KiB logical blocks
packet size negotiated based on buffers
```

PS/2 fallback path:

```text
256 B to 512 B blocks
small fragments
CRC and retransmission
```

### 15.3 Compression Policy

DOS → Pico:

```text
raw first
RLE/Delta+RLE where beneficial
static dictionary for text/config/log files
```

Pico → DOS:

```text
LZ4/LZSS/Delta+RLE where beneficial
raw fallback for already-compressed data
```

---

## 16. Console I/O and Remote Commands

### 16.1 Console Traffic

Console traffic should support:

```text
raw character stream
static dictionary compression
RLE for spaces/repeated chars
control events
cursor updates
```

Dictionary examples:

```text
"
"
"C:\>"
"A:\>"
"DIR"
"CD "
"COPY "
"TYPE "
"DEL "
"REN "
"MD "
"RD "
"EDIT "
"DEBUG "
"CONFIG.SYS"
"AUTOEXEC.BAT"
"PATH="
"SET "
"MODE "
"File not found"
"Bad command or file name"
"Abort, Retry, Fail?"
```

### 16.2 Remote Command Execution

Command flow:

```text
Modern host → Pico → DOS TSR:
    EXEC_CMD

DOS TSR:
    runs command or internal action

DOS TSR → Pico → modern host:
    EXEC_RESULT
    stdout/stderr/exit status
```

Remote execution should support both:

```text
shell command execution
internal commands
```

Internal commands can include:

```text
capture_screen
send_file
recv_file
read_mem
write_mem
probe_lpt
probe_video
status
```

---

## 17. PS/2 Private Fallback Transport

Although IEEE 1284 is the main data plane, the PS/2 private protocol remains useful.

### 17.1 Lane Model

```text
Lane 0 = keyboard PS/2
Lane 1 = AUX/mouse PS/2
```

Recommended use:

```text
keyboard lane:
    control/reliable bidirectional lane

AUX lane:
    secondary upstream lane, mostly Pico → DOS
```

### 17.2 PS/2 Fragment

```c
struct ps2_fragment {
    uint8_t  lane_id;
    uint8_t  seq;
    uint8_t  type;
    uint8_t  len;
    uint8_t  payload[0..32];
    uint16_t crc16;
};
```

### 17.3 Flow Control

Credit-based:

```text
CREDIT_KBD n
CREDIT_AUX n
```

Do not allow free-running from both lanes into the controller.

### 17.4 Practical Speeds

```text
Pico → DOS aggregate:
    ~1.6–2.2 kB/s robust
    ~2.5–2.8 kB/s optimistic

DOS → Pico aggregate:
    ~0.8–1.4 kB/s robust
    ~1.8–2.0 kB/s optimistic
```

Use PS/2 fallback for:

```text
Stage 1 transfer if LPT fails
emergency command channel
diagnostics
small files
control
```

Do not use it as the main screen/file data plane once IEEE 1284 works.

---

## 18. Error Handling and Reliability

### 18.1 Baseline Reliability

Use CRC + retransmission.

Per layer:

| Layer | Check |
|---|---|
| IEEE 1284 packet | CRC-16 |
| PS/2 fragment | CRC-16 |
| File block | CRC-32 |
| Screen frame | CRC-32 |
| Final file | CRC-32 |
| Control message | CRC-16 + seq |

ECC is not part of the initial design. Add it only if measured error rates justify it.

### 18.2 Session Resync

Each transport should support:

```text
RESET_SESSION
PING/PONG
CAP_REQ restart
packet sequence reset
mode renegotiation
transport fallback
```

If IEEE 1284 fails:

```text
fall back to PS/2 control
renegotiate LPT
reload Stage 1
return to keyboard mode if necessary
```

---

## 19. Performance Expectations

### 19.1 IEEE 1284

Expected:

```text
Parallel link:
    2–8+ MB/s on good ECP/EPP hardware

USB CDC:
    ~800–1000 kB/s sustained

Effective compressed screen throughput:
    10–30+ MB/s apparent for delta-friendly screens,
    though USB CDC may cap what the modern host receives.
```

### 19.2 PS/2 Fallback

Expected:

```text
keyboard only:
    ~1 kB/s class

keyboard + AUX:
    ~2 kB/s useful aggregate typical
```

### 19.3 Screen Dump Expectations

Text mode:

```text
80×25 text diff:
    near-interactive

full 80×25 text:
    usually sub-second over parallel
    feasible over PS/2 fallback
```

VESA graphics over parallel:

```text
640×480×8:
    raw 300 KiB
    practical with ECP/EPP and compression

800×600×16:
    raw 938 KiB
    feasible but benefits strongly from delta/tile

1024×768×32:
    raw 3 MiB
    feasible for screenshot/progressive capture,
    not for live streaming unless deltas are tiny
```

VESA graphics over PS/2 fallback:

```text
only ROI/tile/progressive inspection is realistic
```

---

## 20. Firmware Architecture on Pico 2

### 20.1 Rust/Embassy Task Layout

Suggested tasks:

```text
usb_cdc_task
    handles modern host USB stream

parallel_negotiation_task
    watches IEEE 1284 negotiation
    selects mode

parallel_data_task
    runs ECP/EPP/SPP PIO data movement
    DMA integration

ps2_keyboard_task
    normal keyboard emulation
    DEBUG bootstrap injection
    private keyboard lane

ps2_aux_task
    optional mouse emulation
    private AUX lane

session_task
    capability handshake
    transport selection
    packet routing

compression_task
    RLE/Delta+RLE/LZ4/LZSS

screen_task
    frame buffering
    tile state
    palette state

storage_or_buffer_task
    optional buffering, staging, flash/PSRAM if present
```

### 20.2 PIO Allocation

RP2350 provides more PIO resources than RP2040. Use that to keep protocols isolated.

Possible allocation:

```text
PIO block 0:
    IEEE 1284 negotiation watcher
    IEEE 1284 compatibility/byte mode

PIO block 1:
    EPP/ECP data handshaking

PIO block 2:
    PS/2 keyboard endpoint
    PS/2 AUX endpoint
```

Exact allocation should be revised after PIO program sizing.

---

## 21. DOS Software Architecture

### 21.1 Languages

```text
NASM:
    low-level port I/O
    interrupt handling
    hot loops
    RLE/Delta+RLE inner loops if needed

Open Watcom C:
    CLI/TSR structure
    file I/O
    screen capture orchestration
    packet/session logic
    command dispatch
```

### 21.2 Modules

```text
port_detect.asm/c
    detect LPT base, ECP/EPP capability

ieee1284.asm/c
    negotiation and mode setup

packet.c
    SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX framing

crc.asm/c
    CRC-16 and CRC-32

ps2_i8042.asm/c
    Stage 0 and fallback control

screen_text.c
    text-mode capture and diff

screen_vesa.c
    VBE mode query and graphics capture

compress_rle.asm/c
    RLE and Delta+RLE

file_xfer.c
    file transfer protocol

console.c
    console stream and dictionary compression

tsr.c
    resident command handler if used
```

---

## 22. Implementation Roadmap

### Phase 0: Hardware Bring-Up

- Pico 2 board powers cleanly.
- USB CDC works.
- IEEE 1284 level shifter wired and tested.
- PS/2 keyboard GPIO/PIO endpoint electrically safe.
- Optional AUX endpoint wired and tested.

### Phase 1: PS/2 Keyboard Emulation and DEBUG Bootstrap

- Pico emits normal PS/2 keyboard scan codes.
- DOS receives typed commands.
- Pico types a minimal `DEBUG` script.
- `STAGE0.COM` is generated and runs.
- Stage 0 exits cleanly.

### Phase 2: Stage 0 i8042 Control

- Stage 0 masks/hooks IRQ1.
- Flushes controller.
- Sends unlock sequence.
- Pico enters private keyboard mode.
- Stage 0 receives a small signed/test block from Pico.
- Recovery path verified.

### Phase 3: IEEE 1284 Compatibility Mode

- Pico PIO negotiation watcher.
- DOS port detection.
- Compatibility/SPP packet I/O.
- USB CDC forwarding.
- PING/PONG over full path.

### Phase 4: Capability Handshake

- CAP_REQ/CAP_RSP/CAP_ACK.
- Feature intersection.
- Packet size negotiation.
- Compression profile negotiation.

### Phase 5: EPP/ECP Modes

- EPP PIO implementation.
- ECP PIO implementation.
- Dynamic fallback.
- Throughput measurement.

### Phase 6: Raw File Transfer

- FILE_OPEN/DATA/CLOSE.
- CRC-32.
- ACK/NAK.
- Host-side file receiver/sender.

### Phase 7: Screen Dump Text Mode

- 80×25 raw dump.
- Text diff.
- CRC resync.
- Host-side rendering.

### Phase 8: DOS RLE/Delta+RLE

- Screen compression on DOS.
- Pico decompression/recompression.
- USB forwarding.
- Performance measurement on target CPUs.

### Phase 9: VESA Capture

- VBE mode query.
- 8 bpp capture.
- Palette support.
- Tile diff.
- 16/24/32 bpp support.
- ROI/progressive refresh.

### Phase 10: Full TSR/CLI

- Resident mode.
- Remote commands.
- Console I/O.
- Memory read/write.
- File transfer UX.
- Robust error recovery.

### Phase 11: PS/2 Dual-Lane Fallback

- Optional AUX private lane.
- Credit-based flow.
- Stage 1 transfer over PS/2 if LPT unavailable.
- Diagnostics mode.

---

## 23. Key Design Decisions

1. **Parallel port is primary data plane.**  
   IEEE 1284 ECP/EPP is orders of magnitude faster than PS/2.

2. **PS/2 is bootstrap and fallback.**  
   It enables zero-media startup and recovery.

3. **Use legal PS/2 commands for private-mode unlock.**  
   This keeps BIOS/controller behavior sane.

4. **Use `DEBUG` for the smallest bootstrap path.**  
   Hex-entry `.COM` creation avoids needing a file transfer mechanism.

5. **Use CRC + retransmission, not ECC.**  
   The link is controlled and wired.

6. **Keep DOS compression light.**  
   RLE and Delta+RLE are good fits for 486-class machines.

7. **Let Pico spend CPU.**  
   RP2350 can compress, reframe, and buffer much more aggressively.

8. **Use text diffing for text modes.**  
   It is simpler and better than generic compression.

9. **Use tile diffing for VESA modes.**  
   Full raw frames are large even over parallel.

10. **Negotiate capabilities.**  
    Old hardware varies. The session must adapt.

---

## 24. Open Questions

- Which minimum DOS CPU should be treated as the baseline: 286, 386SX, 386DX, or 486DX2?
- Should Stage 0 directly attempt LPT negotiation, or only unlock Pico and load Stage 1?
- Should `STAGE0.COM` be generated entirely by `DEBUG E` commands, or should DEBUG create a tiny hex decoder?
- Which parallel modes must be supported in version 1: Compatibility only, EPP, ECP, or all?
- How much buffering is available on the Pico 2 board variant?
- Should the modern host protocol expose raw Pico packets or a higher-level RPC API?
- Should final file integrity use CRC-32 only, or optionally SHA-1/SHA-256 on the modern host side?
- Should VESA LFB support require DPMI/DOS extender, or should version 1 use banked VBE only?
- Should the PS/2 AUX lane be enabled in v1, or reserved for v1.1 fallback?

---

## 25. Concise Merged Architecture

The merged design uses the Pico 2 as a multi-interface bridge:

```text
PS/2 keyboard/AUX:
    bootstraps the DOS machine without media
    creates STAGE0.COM using DEBUG
    unlocks private mode
    provides emergency low-speed control/fallback

IEEE 1284 parallel:
    becomes the primary high-speed link
    negotiates ECP/EPP/Byte/SPP
    transports framed packets with CRC
    carries files, screens, console, memory, and command traffic

USB CDC:
    connects Pico to the modern host
    forwards packets or higher-level streams
```

The result is a robust vintage-PC bridge that can start from a bare DOS prompt, install its own DOS-side loader, negotiate the fastest available parallel mode, and then provide high-speed bidirectional services while retaining a PS/2-based rescue path.
