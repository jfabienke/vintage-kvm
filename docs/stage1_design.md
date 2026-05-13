# Stage 1 Design

**Status:** Architectural design document
**Last updated:** 2026-05-12
**Companion documents:** [`design.md`](design.md) §7 (bootstrap ladder), §8 (IEEE 1284 negotiation), §9 (packet format), §10 (capability discovery), §22 Phases 3–5 (roadmap); [`stage0_design.md`](stage0_design.md) (Stage 0 design Stage 1 inherits from); [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) (controller-side IEEE 1284 reference); [`two_plane_transport.md`](two_plane_transport.md) (the session Stage 2 brings up); [`implementation_plan.md`](implementation_plan.md) §3 (per-file plan).

## What Stage 1 is

Stage 1 is the **DOS-side loader that bridges Stage 0's bring-up to Stage 2's TSR**. Stage 0 typed itself in via keyboard and brought up *one* working bidirectional channel; Stage 2 is the production TSR with file transfer, screen capture, and console I/O. Stage 1 fills the gap: it negotiates IEEE 1284 into a real high-throughput mode, downloads Stage 2, and hands off cleanly.

> **Stage 1 is a one-shot loader.** It is not resident, not interactive, and not the protocol endpoint for any data service. It exists to take a small inherited channel and turn it into a working LPT data plane that Stage 2 can use, then get out of the way.

What Stage 1 must do, in order:

1. Validate the hand-off ABI from Stage 0 (`AX = 0x3150` marker, sane `BX`/`CX`/`DX`).
2. Probe the LPT chipset Stage 0 already located (or re-probe if LPT didn't come up at Stage 0).
3. Drive the IEEE 1284 negotiation sequence and pick the highest mode the host + Pico both support.
4. Perform a minimal capability handshake (`CAP_REQ` / `CAP_RSP`) — enough to learn Stage 2 size, image CRC, and protocol version.
5. Download `PICO1284.EXE` to disk in the current directory, with per-block CRC-16 and whole-image CRC-32.
6. EXEC Stage 2 via `INT 21h AH=4Bh` so DOS handles relocation, PSP setup, segment allocation.

What Stage 1 must **not** do:

- Install a TSR, hook `INT 21h`, or persist anything other than the Stage 2 file on disk.
- Implement compression, screen capture, file transfer service, console I/O, or any other Stage 2 service.
- Enter the `DUAL_PLANE_ACTIVE` state of [`two_plane_transport.md`](two_plane_transport.md) — that's Stage 2's job. Stage 1 brings the data plane up to `DP_READY` and stops.
- Unlock i8042 private mode itself — Stage 0 already did that. Stage 1 inherits.
- Try to keep PS/2 private channel alive across the EXEC boundary — `INT 21h AH=4Bh` resets register state and may clobber i8042 ownership. Stage 2 re-establishes if needed.

## Why a separate stage

Stage 1 exists because the alternatives are worse:

| Alternative | Why it's worse |
|---|---|
| **Fold Stage 1 into Stage 0** | Stage 0 is typed character-by-character through `DEBUG`. Adding IEEE 1284 negotiation (~3–4 KB) would push injection time from ~75 s to ~5 min on XT ([§Stage –1 injection duration](stage0_design.md#stage-1-injection-duration)). Unacceptable. |
| **Fold Stage 1 into Stage 2** | Stage 2 needs to *receive* itself over LPT — chicken-and-egg. Something has to negotiate 1284 before Stage 2 arrives. |
| **Skip Stage 1; use SPP/nibble for Stage 2 download** | SPP nibble is ~50 KB/s. A 50 KB Stage 2 takes 1 second on EPP/ECP and 20+ seconds on nibble. Stage 1 closes that gap. |
| **Pre-install Stage 2 from media** | Defeats the no-media-required bootstrap goal. |

Stage 1 is the smallest piece of DOS-side code that can promote a barely-working bootstrap channel into a real EPP/ECP data plane.

## Bootstrap ladder context

```
Stage -1: PS/2 keyboard injection (Pico-side)
    Pico types DEBUG script.
        ↓
Stage 0: docs/stage0_design.md
    Brings up first bidirectional channel.
    Downloads Stage 1 over that channel.
    Hands off via near jump at CS:0x0800 with DX = channel bitmap.
        ↓
Stage 1: this document
    Validates hand-off.
    Negotiates IEEE 1284.
    Capability handshake (minimal).
    Downloads PICO1284.EXE to disk.
    EXECs Stage 2.
        ↓
Stage 2: pico1284 (docs/implementation_plan.md §4)
    TSR/CLI. Full data plane. Compression. Screen capture.
```

Stage 1 owns the **Stage 0 → Stage 2** edge. Everything before is bootstrap-bandwidth-constrained; everything after is steady-state.

## Hand-off ABI

### Inbound (Stage 0 → Stage 1)

Entered via near jump at `CS:0x0800` from Stage 0. Same physical segment, same PSP. Register state per [`stage0_design.md` §Hand-off ABI](stage0_design.md):

```
AX = 0x3150              'P1' marker
BX = LPT base port       0 if LPT not up; else 0x3BC / 0x378 / 0x278
CX = Stage 1 size        bytes loaded at CS:0800h
DX = Channel-availability bitmap:
        bit 0 (0x01) = LPT channel up
        bit 1 (0x02) = i8042 KBD private channel up
        bit 2 (0x04) = i8042 AUX private channel up
     DX != 0 invariant.
DS = ES = CS = host PSP segment
Direction flag: cleared
Interrupts: enabled
IRQ mask state: per Stage 0 variant (see stage0_design.md §IRQ mask at hand-off)
```

Stage 1's entry code validates `AX == 0x3150`, `CX > 0`, `CX <= MAX_STAGE1_SIZE`, `DX != 0`. Bad inbound state → print a single-line error and exit to DOS with errorlevel 1; Stage 0 cannot have produced a bad hand-off, so this is purely defensive.

### Outbound (Stage 1 → Stage 2)

Stage 1 does **not** define a direct register-level hand-off to Stage 2. Stage 2 is spawned via `INT 21h AH=4Bh` (Load and Execute), so DOS handles PSP/relocation/segment setup. The contract is at the *filesystem and environment* level:

```
PICO1284.EXE              in current working directory, just written by Stage 1
Environment var PICO_BOOT:
    LPT=XXXX              LPT base in hex, or "0" if no LPT
    MODE=ECP|EPP|BYTE|SPP active 1284 mode (or NEG_FAILED)
    CHAN=N                Stage 0 DX bitmap value (decimal)
    VER=X.Y               protocol version from CAP_RSP
```

Stage 2 reads the environment, knows the inherited state, doesn't need to re-probe. Failure to set the environment → Stage 2 re-probes from scratch (slower but correct).

**Why environment, not registers?** `INT 21h AH=4Bh` clobbers register state — DOS sets up a fresh PSP for the child, loads the program, jumps to the entry point with DOS-defined register conventions. Passing state through registers is impossible. The PSP environment block is the cleanest hand-off channel for a few KB of name/value pairs.

## Common architecture

```
start:
    cli / save DS, ES / cld / sti

    call validate_handoff           ; AX, BX, CX, DX sanity
    jc   fail_handoff

    call lpt_chipset_detect         ; ECR probe, EPP_ADDR probe, classify
    ; result in dp_caps struct

    call ieee1284_negotiate         ; drive extensibility-byte sequence
    jc   .nibble_fallback           ; negotiation failed; use SPP

    call minimal_cap_handshake      ; CAP_REQ → CAP_RSP, learn Stage 2 size + CRC
    jc   fail_caps

    call download_stage2            ; per-block CRC-16; whole-image CRC-32
    jc   fail_download

    call write_stage2_file          ; PICO1284.EXE in cwd
    jc   fail_write

    call set_environment            ; PICO_BOOT=...
    call exec_stage2                ; INT 21h AH=4Bh
    ; control does not return on success

fail_*:
    print one-line error
    INT 21h AH=4Ch AL=01h           ; exit, errorlevel 1
```

### Memory layout within the COM segment

```
0x0000 – 0x00FF   PSP                            (DOS-managed)
0x0100 – 0x07FF   Stage 0 .COM                   (still resident; we may overwrite)
0x0800 – 0x27FF   Stage 1 .bin                   (this code, up to 8 KB)
0x2800 – 0xCFFF   Stage 2 download buffer        (~42 KB working space)
0xD000 – 0xFFFE   stack + scratch                (DOS-set; respect SP at entry)
```

Stage 0's code at `0x100–0x7FF` is no longer needed once Stage 1 starts. Stage 1 may overwrite it as scratch (e.g., for an environment-block buffer) if size pressure demands, but the default plan is to leave Stage 0 intact since the segment has plenty of free space.

### Build pipeline

Already wired in [`dos/Makefile`](../dos/Makefile):

```make
$(BUILD)/stage1.bin: stage1/stage1.asm | $(BUILD)
	$(NASM) -f bin -o $@ $<
```

Output: `dos/build/stage1.bin`, flat binary, `org 0x800`. Gets embedded into the Pico firmware via `include_bytes!` and served to Stage 0 over the inherited channel during the bootstrap protocol.

When Stage 1 grows beyond a single file, split into `stage1/main.asm` + sub-includes (e.g., `stage1/lpt_negotiate.inc`, `stage1/packet.inc`). NASM `%include` is sufficient; no linker needed for a flat binary at fixed origin.

## Subsystems

### 1. Hand-off validation

~50 bytes. Checks `AX = 0x3150`, `CX != 0 && CX <= 50000`, `DX != 0`. On failure, prints `ERROR: bad handoff from Stage 0` and exits. This is paranoia — Stage 0 cannot produce a bad hand-off by construction — but cheap.

### 2. LPT chipset detection

Per [`design.md` §8.3](design.md) and [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md). Two-step:

```
1. If (DX & 0x01) — LPT bit set by Stage 0:
     Use [BX] as lpt_base. Done.
   Else:
     Re-probe [0x3BC, 0x378, 0x278] using the same nibble protocol as Stage 0.
     If still no response: skip ECP/EPP probe; fall through to PS/2-only path.

2. With lpt_base known:
     Probe ECR at base+0x402:
       write 0x35 → read back; if matches → ECP-capable
     Probe EPP_DATA at base+4:
       set ECR to PS/2 mode; toggle direction bit; read EPP_TIMEOUT bit behavior
       → EPP-capable
     Classify Super I/O via config-mode unlock (0x87 0x87 to 0x2E for Winbond,
     0x87 0x01 0x55 0x55 for ITE, etc.) — opportunistic; failure is fine.
```

Outputs a `dp_caps`-shaped local struct ([`two_plane_transport.md` §Capability discovery](two_plane_transport.md)) but kept minimal here — just enough for mode selection.

```c
struct stage1_dp_caps {
    u16 lpt_base;            // 0 if LPT not available
    u8  supports_spp     : 1;
    u8  supports_epp     : 1;
    u8  supports_ecp     : 1;
    u8  supports_ecp_dma : 1;
    u8  reserved         : 4;
    u8  irq;                 // 0xFF if unknown
    u8  dma_channel;         // 0xFF if unknown
};
```

DMA channel detection is *opportunistic in Stage 1*. If it works, the value flows through to Stage 2 via `PICO_BOOT`. If it fails, Stage 2 re-probes (Stage 2 owns the actual DMA pump per [`two_plane_transport.md` §Data-plane modes and optional ECP DMA](two_plane_transport.md)).

### 3. IEEE 1284 negotiation

Per [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md). The negotiation byte sequence is mirrored from Linux `drivers/parport/ieee1284.c`:

```
Host:                                                Pico (peripheral)
1. Drive data lines with extensibility byte           (observes nSelectIn high,
   (0x10 = nibble, 0x14 = ECP, 0x40 = EPP, etc.)       nAutoFd low, latches byte)
2. Pulse nSelectIn low (1 µs)                          (sees nSelectIn falling
                                                       edge → enter negotiation)
3. Pulse nStrobe low (1 µs)                            (drives nAck low when
4. Wait for nAck low (XFLAG, BUSY, PERROR set if      ready; sets XFLAG = 1 if
   peripheral accepts request)                         requested mode supported,
                                                       else XFLAG = 0)
5. Read XFLAG/PERROR/SELECT to confirm accepted        
6. Pulse nStrobe again to enter target mode
```

Try modes in order: **ECP → EPP → Byte → SPP/nibble**. Stop at first acceptance. Total budget: ~50 ms for the whole sequence on a 6 MHz 286.

On total negotiation failure (no peripheral acknowledgment), Stage 1 falls back to bidirectional SPP nibble (which Stage 0 already proved works) and downloads Stage 2 at the slow rate. Better slow than dead.

### 4. Mode selection / fallback ladder

The fallback ladder for Stage 1 is narrower than Stage 2's. Stage 1 doesn't need ECP DMA — Stage 2 owns that.

```
ECP PIO  →  EPP PIO  →  Byte mode  →  SPP nibble  →  fail
```

The selected mode is recorded for the `PICO_BOOT` environment variable. Stage 2 may renegotiate to a higher mode (e.g., promote ECP PIO to ECP DMA after validating the host's DMA channel), so Stage 1's choice is a *floor*, not a *ceiling*.

### 5. Minimal packet I/O

Stage 1 uses the [`design.md` §9](design.md) packet format with the **smallest possible command set**:

| Command | Direction | Stage 1 uses it for |
|---|---|---|
| `CAP_REQ` (0x00) | Host → Pico | Request capability blob |
| `CAP_RSP` (0x0F) | Pico → Host | Receive capabilities (version, Stage 2 size, image CRC32, preferred mode) |
| `CAP_ACK` (0x0E) | Host → Pico | Confirm intersection (Stage 1 sends a stripped-down acceptance) |
| `SEND_BLOCK` (0x20) | Host → Pico | Request next Stage 2 block |
| `RECV_BLOCK` (0x21) | Pico → Host | Receive Stage 2 block payload |
| `BLOCK_ACK` (0x22) | Host → Pico | ACK successful block |
| `BLOCK_NAK` (0x23) | Host → Pico | NAK on CRC-16 mismatch |
| `ERROR` (0x13) | Pico → Host | Receive Pico-side error |
| `PING`/`PONG` (0x10/0x11) | bidir | Liveness during long blocks |

Everything in 0x30+ (FILE_*, SCREEN_*, CONSOLE_*, MEM_*, EXEC_*, DICT_*, CODEC_*) is **out of scope for Stage 1**. Stage 2 owns them.

CRC-16-CCITT (poly 0x1021, init 0xFFFF) per packet; CRC-32 over the whole Stage 2 image, learned from `CAP_RSP`.

### 6. Capability handshake (minimal)

Stage 1 sends `CAP_REQ` and parses `CAP_RSP` per [`design.md` §10](design.md). Of the response fields, Stage 1 cares about:

| Field | Used for |
|---|---|
| `version_major` / `version_minor` | Sanity-check protocol compatibility; reject if `version_major != 1` |
| `stage2_image_size` (custom field — extend §10.2) | Allocate download buffer; validate against `MAX_STAGE2_SIZE` |
| `stage2_image_crc32` (custom field) | Validate downloaded image |
| `preferred_parallel_mode` | Cross-check with our negotiated mode |
| `active_parallel_mode` | Confirm Pico is in the same mode we negotiated |

All other capability fields (compression, traffic bitmaps, dictionaries, USB mode, etc.) are **ignored by Stage 1** and forwarded verbatim to Stage 2 via a binary blob in the environment or via re-request. Default plan: re-request — Stage 2 sends its own `CAP_REQ` once it's running, gets a fresh snapshot. This is simpler than parsing the capability blob twice.

Note that `stage2_image_size` and `stage2_image_crc32` are **new fields** that need to be added to the §10.2 capability response layout. They're carried in a new sub-block that doesn't disturb existing fields:

```
Suggested addition to §10.2:
    stage2_image_size      u32      bytes; Pico-firmware-embedded Stage 2 size
    stage2_image_crc32     u32      CRC-32 over the embedded image
```

These two fields are Stage-1-specific and don't affect Stage 2's later operations.

### 7. Stage 2 download + verify

Block-oriented, identical pattern to Stage 0's Stage 1 download:

```
For each block_no in 0..ceil(image_size / BLOCK_SIZE):
    send SEND_BLOCK with block_no (u32)
    receive RECV_BLOCK header (block_no, payload_len, CRC-16)
    receive payload_len bytes into download_buffer + block_no * BLOCK_SIZE
    if CRC-16 matches: send BLOCK_ACK; advance
    else: send BLOCK_NAK; retry up to N times

After all blocks received:
    compute CRC-32 over the whole image
    cmp against stage2_image_crc32 from CAP_RSP
    mismatch → fail; matches → proceed
```

**BLOCK_SIZE** is mode-dependent. For SPP nibble fallback: 64 bytes (matches Stage 1 download). For Byte mode: 256 bytes. For EPP/ECP: 1024 bytes. Larger blocks amortize the per-block CRC + ACK overhead but increase the cost of a single retransmit.

**Download-buffer bounds checking** mirrors `s0_xt.asm`'s defenses ([`stage0_design.md` §Metadata bounds checking](stage0_design.md)):

1. `0 < image_size <= MAX_STAGE2_SIZE` (proposed: 200 KB; Stage 2 .EXE is expected ~50–100 KB).
2. `block_offset + payload_len <= image_size` per block.
3. `(STAGE2_DOWNLOAD_OFF + block_offset + payload_len) <= 0xD000` to stay clear of stack.

Block-by-block CRC-16 catches *transmission* errors; whole-image CRC-32 catches *coherence* errors (e.g., a block written to the wrong offset, an off-by-one in block accounting). Both are required.

### 8. Stage 2 file write + EXEC

After the image is in memory and verified:

```
INT 21h AH=3Ch (create file)
    DS:DX = "PICO1284.EXE",0
    CX    = 0 (normal attribute)
    → AX = file handle on success, CF set on error

INT 21h AH=40h (write)
    BX    = file handle
    CX    = image_size
    DS:DX = download_buffer
    → AX = bytes written; verify == image_size

INT 21h AH=3Eh (close)
    BX    = file handle

Build environment block at scratch_area:
    "PICO_BOOT=LPT=03BC;MODE=ECP;CHAN=3;VER=1.0",0,0

INT 21h AH=4Bh AL=00h (load and execute)
    DS:DX = "PICO1284.EXE",0
    ES:BX = parameter block:
              .env_seg   = scratch_area_seg
              .cmdtail   = empty command line (just one byte 0x0D)
              .fcb1      = 0
              .fcb2      = 0
```

On successful EXEC, control returns to Stage 1 only after Stage 2 exits. Stage 1 then exits to DOS with Stage 2's errorlevel (`INT 21h AH=4Dh` to read child errorlevel, then `AH=4Ch AL=...` to propagate).

If `INT 21h AH=4Bh` fails (DOS out of memory, file not found, executable format error), Stage 1 prints `ERROR: Stage 2 EXEC failed (DOS code XX)` and exits with errorlevel 1.

**File location:** current working directory by default. Operator can pre-set `PICO1284_DIR` environment variable to override; Stage 1 reads it from the inherited environment at start.

## Channel handling

Stage 1 has two operating modes depending on Stage 0's inherited `DX`:

### Mode A — LPT available (`DX & 0x01`)

The expected fast path. Stage 1:

- Skips LPT re-probe (uses inherited `BX`).
- Drives 1284 negotiation.
- Downloads Stage 2 over LPT at EPP/ECP speed (~1 second for 50 KB).
- Sets `PICO_BOOT=LPT=XXXX;MODE=...`.

PS/2 private channels (KBD/AUX, if set in `DX`) are *not used by Stage 1* in this mode. They're left in whatever state Stage 0 left them, and Stage 2 inherits the same state via `PICO_BOOT=CHAN=N`. Stage 2 decides whether to use AUX as a fallback transport per [`two_plane_transport.md`](two_plane_transport.md).

### Mode B — LPT not available (`DX & 0x01 == 0`)

Falls back to downloading Stage 2 over the inherited PS/2 private channel. Slow (~30 seconds for 50 KB over KBD private at 1.5 KB/s; ~20 seconds over AUX private), but completes.

Choice of private channel: **prefer KBD over AUX**, matching Stage 0's `choose_download_channel` priority. AUX is reserved for Stage 2's data-plane fallback.

Stage 1 also does **one** LPT re-probe at start in this mode, since Stage 0's probe might have been confused by a marginal cable or a slow chipset. The re-probe uses identical nibble code; if it succeeds, Stage 1 promotes to Mode A. If it fails, stays in Mode B.

`PICO_BOOT` in Mode B: `LPT=0;MODE=PS2_NIBBLE;CHAN=N`. Stage 2 sees `LPT=0` and either retries LPT promotion itself (it has more time and code budget) or operates degraded.

## Failure handling

Every failure path:

1. Closes any open file handles.
2. Frees any allocated memory (Stage 1 only allocates the env block via DOS; no manual mem mgmt).
3. Prints a single-line error message identifying which step failed.
4. Exits via `INT 21h AH=4Ch AL=01h` so DOS errorlevel signals failure.

Error messages (matching Stage 0 precedent):

```
ERROR: bad handoff from Stage 0
ERROR: LPT chipset detection failed
ERROR: IEEE 1284 negotiation failed (no peripheral response)
ERROR: capability handshake failed
ERROR: Stage 2 size invalid
ERROR: Stage 2 download failed
ERROR: Stage 2 image CRC mismatch
ERROR: Cannot write PICO1284.EXE (DOS code XX)
ERROR: Stage 2 EXEC failed (DOS code XX)
```

Note: "IEEE 1284 negotiation failed" is not an absolute failure if SPP nibble works — Stage 1 silently falls back to nibble for the Stage 2 download. The error appears only if SPP also fails (effectively: the LPT cable is dead despite Stage 0 having probed it; should be rare).

**IRQ mask / i8042 state on failure exit.** Stage 1 *does not modify* IRQ masks or i8042 CCB — it inherits whatever Stage 0 left and does not touch them. So on Stage 1 failure exit, the state is whatever Stage 0 left at hand-off:

- If only LPT came up at Stage 0 (DX = 0x01): IRQs in BIOS state. Stage 1 failure exits cleanly to DOS.
- If KBD or AUX private also came up: IRQ1 (and IRQ12 on PS/2) are masked at the PIC. Stage 1 failure exit leaves them masked, which is a problem — user can't type to retry.

To handle this, **Stage 1's failure path explicitly unmasks IRQ1 and IRQ12** before exit if it detects the inherited state had them masked (via `DX & 0x06 != 0`). One in/out instruction per PIC, ~20 bytes total. Doesn't restore CCB bit 6 (scancode translation) — that's i8042-controller state which BIOS will re-init on next boot; safe to leave for the user to power-cycle if necessary.

## Size budget

Target: ≤ 8 KB blob (10× Stage 0's XT size). Well within Stage 0's `MAX_STAGE1_SIZE = 50000`.

Estimated byte breakdown:

| Subsystem | Estimated size |
|---|---:|
| Entry header + hand-off validation | ~80 B |
| LPT chipset detection (ECR + EPP probe) | ~400 B |
| LPT nibble re-probe (shared with Stage 0 pattern) | ~250 B |
| IEEE 1284 negotiation state machine | ~800 B |
| Packet framing (CRC-16, encode/decode) | ~500 B |
| Capability handshake | ~300 B |
| Stage 2 download loop + bounds check | ~600 B |
| Stage 2 file write (INT 21h calls) | ~150 B |
| Environment block setup | ~200 B |
| `INT 21h AH=4Bh` EXEC | ~150 B |
| Failure handling + error strings | ~400 B |
| Mode-specific byte pumps (SPP/EPP/ECP/PS/2) | ~1200 B |
| CRC-32 routine (whole-image verify) | ~200 B |
| Data: string literals, lookup tables | ~300 B |
| **Subtotal** | **~5530 B** |
| **Slack to 8 KB budget** | **~2660 B** |

The ~2.6 KB slack accommodates per-CPU-class optimizations (Stage 1 is *not* CPU-dispatched like Stage 2, since it runs once for a few seconds — a single 8086-compatible build is fine), Super I/O chipset-specific quirks, and unforeseen platform issues.

## Build pipeline

Currently a 42-byte stub:

```asm
; stage1.asm
bits 16
org 0x800
start:
    push cs
    pop ds
    mov dx, msg
    mov ah, 09h
    int 21h
    int 20h
msg db 'STAGE1 v0.0 scaffold reached',13,10,'$'
```

Production layout:

```
dos/stage1/
├── stage1.asm                ; entry + top-level dispatch (~500 B)
├── handoff.inc               ; hand-off validation
├── lpt_detect.inc            ; chipset detection (ECR / EPP probe)
├── lpt_nibble.inc            ; SPP nibble (shared with Stage 0 — see Open §1)
├── ieee1284_neg.inc          ; negotiation state machine
├── packet.inc                ; SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX framing
├── crc16.inc                 ; CRC-16-CCITT (shared with Stage 0)
├── crc32.inc                 ; CRC-32 over Stage 2 image
├── cap_handshake.inc         ; minimal CAP_REQ/CAP_RSP
├── pumps_lpt.inc             ; SPP/EPP/ECP byte pumps
├── pumps_ps2.inc             ; KBD/AUX private byte pumps
├── stage2_dl.inc             ; download loop + bounds check
├── stage2_exec.inc           ; file write + INT 21h AH=4Bh
└── messages.inc              ; error strings, banners
```

NASM `%include` is sufficient — flat-binary output, no linker. Build:

```make
$(BUILD)/stage1.bin: $(STAGE1_SRC) $(STAGE1_INCLUDES) | $(BUILD)
	$(NASM) -f bin -o $@ $<
```

Output `stage1.bin` gets embedded in the Pico firmware via `include_bytes!("../../dos/build/stage1.bin")` and served to Stage 0 over the bootstrap protocol.

CI should fail the build if `stage1.bin` exceeds 8 KB; this protects the embed-into-Pico-flash budget and the over-LPT delivery time.

## Testing strategy

| Test | What it validates | Where it runs |
|---|---|---|
| Stub round-trip from Stage 0 | Hand-off ABI is byte-correct; segment math works | CI (DOSBox-X) |
| LPT chipset detection on USS-720 fixture | ECR + EPP probe behave correctly against the Linux-kernel reference | host (libusb fixture) |
| IEEE 1284 negotiation against Pico peripheral | Byte sequence matches `drivers/parport/ieee1284.c` | bench, Phase 3+ |
| Mode-fallback ladder | ECP → EPP → Byte → SPP each individually under fault injection | bench |
| CAP_RSP parsing | All fields decoded; bad version rejected | CI (with mock Pico) |
| Stage 2 download (small + large + boundary) | CRC-16 per block, CRC-32 whole-image | CI (with mock Pico) |
| Block bounds checking | Forged metadata cannot overrun the download buffer | CI (fuzz) |
| `INT 21h AH=4Bh` EXEC | Stage 2 spawns, environment passes through | CI (DOSBox-X with stub Stage 2) |
| Mode B (LPT down at hand-off) | Stage 2 downloads over KBD private | bench |
| Failure paths | IRQ masks restored on exit when Stage 0 left them masked | bench |

CI substrate: DOSBox-X with a host-side mock Pico (replaces the LPT side with a libusb endpoint, replaces the PS/2 side with a virtual KBC controlled by the test harness). Tracked under [`implementation_plan.md` §6 tools](implementation_plan.md).

## Open decisions

1. **Shared LPT nibble code with Stage 0.** `s0_xt.asm`, `s0_atps2_core.inc`, and Stage 1 all need the same `find_pico_lpt` / `lpt_send_byte` / `lpt_recv_byte` / `lpt_recv_nibble` routines. [`stage0_design.md`](stage0_design.md) §Per-file plan lists a proposed `lpt_nibble.inc`. Stage 1 should consume that include once it exists; until then, duplicate (with a `TODO: dedupe` comment).
2. **Where does Stage 2's image live in Pico firmware?** Two options: (a) embedded in firmware via `include_bytes!`, identical to Stage 1; (b) flashed separately to a known offset, updated independently. Default proposal: (a) — single firmware artifact is simpler for v1; revisit if Stage 2 grows past ~200 KB or needs independent updates.
3. **Custom `CAP_RSP` fields (`stage2_image_size`, `stage2_image_crc32`).** Need to be added to [`design.md` §10.2](design.md). Should they go at the end of the response (backward-compatible) or in a versioned sub-block? Default proposal: end-of-response, gated on `version_minor >= 1`.
4. **Where does `PICO1284.EXE` live on disk?** Current directory by default; operator override via `PICO1284_DIR` env var. Open: should Stage 1 delete the file after Stage 2 exits, or leave it for re-runs? Default proposal: leave it. Stage 2 can be re-run without the bootstrap chain if it's already on disk.
5. **Stage 2 errorlevel propagation.** Stage 1 reads child errorlevel via `INT 21h AH=4Dh` and propagates via its own exit code. Open: should Stage 1 retry Stage 2 EXEC on transient failures, or fail fast? Default proposal: fail fast; Stage 2 is a TSR that succeeds or doesn't, no transient cases.
6. **Stage 1 image compression.** A 50 KB Stage 2 over PS/2 nibble fallback takes ~30 seconds. Compressing Stage 2 with RLE before serving from Pico could halve that. But it adds a decompressor in Stage 1 (~500 B). Default proposal: skip for v1; revisit if Mode B times become a real UX problem.
7. **Where does Stage 1 keep its DEBUG-mode banner string?** Stage 0 has `'Pico1284 endpoint found',13,10,'$'` and similar. Stage 1 could be silent on success or print `STAGE1: negotiated ECP at 0x378`. Default proposal: minimal banner + result string, ~30 bytes of text.
8. **Re-probe LPT in Mode B?** A single re-probe is cheap (~100 ms) and could rescue a marginal Stage 0 attempt. Default proposal: yes, one re-probe at start of Mode B before committing to PS/2 download.

## Related documents

- [`design.md`](design.md) §7 — bootstrap ladder
- [`design.md`](design.md) §8 — IEEE 1284 negotiation Stage 1 implements
- [`design.md`](design.md) §9 — packet format Stage 1 uses (subset)
- [`design.md`](design.md) §10 — capability discovery Stage 1 does minimally
- [`design.md`](design.md) §22 Phases 3–5 — Pico firmware milestones Stage 1 depends on
- [`stage0_design.md`](stage0_design.md) — predecessor stage; hand-off ABI source
- [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) — controller-side reference Stage 1 mirrors
- [`two_plane_transport.md`](two_plane_transport.md) — Stage 2's data plane, which Stage 1 brings to `DP_READY`
- [`implementation_plan.md`](implementation_plan.md) §3 — per-file plan summary (replaced/extended by this doc)
- [`stage1_implementation.md`](stage1_implementation.md) — as-built reference for `dos/stage1/stage1.asm` v1.0 (file:line citations, data section map, EXEC param block layout)
