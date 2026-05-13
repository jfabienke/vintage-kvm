# Stage 0 Design

**Status:** Architectural design document
**Last updated:** 2026-05-12
**Companion documents:** [`design.md`](design.md) §7 (Bootstrap ladder, DEBUG, unlock, recovery) and §22 Phases 1–2 (Stage 0 roadmap), [`ps2_eras_reference.md`](ps2_eras_reference.md) (per-era Stage 0 changes), [`ps2_private_channel_design.md`](ps2_private_channel_design.md) (i8042 private mode, AUX framing), [`two_plane_transport.md`](two_plane_transport.md) (DUAL_PLANE_ACTIVE / fallback states this hands off into), [`implementation_plan.md`](implementation_plan.md) §2 (per-file plan, hand-off contract)

## What Stage 0 is

Stage 0 is the **smallest piece of DOS-side code that vintage-kvm types into the target machine through the keyboard, by `DEBUG` script, with zero pre-installed software.** Its only job is to bring up the first real bidirectional channel to the Pico and hand off to Stage 1.

> **Stage 0 is not a driver. It is a beachhead.** Every byte counts: each one is typed character-by-character by the Pico's PS/2 keyboard emulation through `DEBUG`'s hex-entry mode. Smaller is faster to inject and less likely to fail mid-typing.

What Stage 0 must do, in order:

1. Survive being run as a `.COM` from `DEBUG` with no DOS environment assumptions beyond a working `INT 21h`.
2. Establish *one* working bidirectional channel to the Pico.
3. Download Stage 1 over that channel into memory at `CS:0800h`.
4. Verify Stage 1 (per-block CRC-16 + whole-image checksum).
5. Hand off via near jump with a fixed register ABI.
6. On any failure: restore the system enough that the user can power-cycle and try again.

What Stage 0 must **not** do:

- Load drivers, hook `INT 21h`, install TSRs, allocate memory beyond its PSP segment.
- Implement IEEE 1284 negotiation (Stage 1's job).
- Implement EPP/ECP modes (Stage 1's job).
- Implement compression, file transfer, screen capture, or anything in [`two_plane_transport.md`](two_plane_transport.md) §Services.
- Persist anything.

## Why three variants

Stage 0 exists in three flavours because the **DOS-side hardware available for the bootstrap channel is fundamentally different across PC eras**, as documented in [`ps2_eras_reference.md`](ps2_eras_reference.md). The PS/2 connector hides three protocol discontinuities, and Stage 0 sits exactly where those discontinuities matter most.

| Variant | Target era | DOS CPU | Bootstrap channel | i8042? | AUX? |
|---|---|---|---|:---:|:---:|
| `s0_xt.asm` | XT (1981–1984) | 8088 / 8086 | LPT SPP nibble | No | No |
| `s0_at.asm` | AT (1984–1987) | 286+ | LPT SPP nibble **+** i8042 KBD private mode | Yes | No |
| `s0_ps2.asm` | PS/2 + SuperIO (1987+) | 386+ | LPT + i8042 KBD + i8042 AUX private mode | Yes | Yes |

The split is not "one Stage 0 with runtime probes." It is three deliberately small binaries because:

- **XT really is different.** No i8042. Keyboard line is hardware-unidirectional (9-bit, kbd→PC only). The Pico cannot send anything back through the keyboard. The bidirectional channel must be LPT-only. There is no way to fold this into the AT variant without paying for code that an XT can never execute.
- **AT vs PS/2 could theoretically merge** by runtime-probing AUX, but every extra byte in Stage 0 is typed by the Pico through the keyboard, character-by-character. An AT-only machine paying for AUX code that will never run wastes injection time. The split is a deliberate size/inject-time trade.
- **SuperIO does not get its own variant.** Wire-protocol-identical to PS/2. Quirks (port-swap, A20 via `0xD1`, vendor unlock at `0x2E`/`0x4E`) are runtime probes inside `s0_ps2.asm`, not a fourth binary. See [`ps2_eras_reference.md`](ps2_eras_reference.md) §SuperIO.

**Which variant does the Pico type?** The Pico's keyboard injector embeds three `DEBUG` hex blobs and selects one based on host-class detection (config GPIO, boot flag, or BIOS POST string sniffed during Stage –1 — see [§Variant selection](#variant-selection-pico-side) below). The Stage 0 binary itself never has to decide which variant it is — it only has to be correct for the era it was injected into.

## Bootstrap ladder context

```
Stage -1: PS/2 keyboard injection (Pico-side)
    Pico behaves as normal PS/2 keyboard.
    Pico types DEBUG script into DOS via legal scan codes.
    DEBUG produces STAGE0.COM via hex-entry mode (E 100 ..., RCX, W, Q).
        ↓
Stage 0: this document
    The .COM created by Stage -1.
    Brings up the first real bidirectional channel.
    Downloads + verifies Stage 1.
    Hands off via near jump.
        ↓
Stage 1: docs/stage1_design.md
    Larger DOS-side loader, served by the Pico over LPT (or inherited PS/2).
    Detects LPT chipset.
    Performs IEEE 1284 mode negotiation (Compat → Nibble → Byte → EPP → ECP).
    Minimal CAP_REQ/CAP_RSP handshake.
    Downloads PICO1284.EXE + EXEC.
        ↓
Stage 2: pico1284
    Full TSR/CLI. File transfer, screen, console, etc.
```

Stage 0 owns the **Stage –1 → Stage 1** edge. Everything before is keyboard typing; everything after is real protocol.

## Common architecture

All three variants share a skeleton. Differences are localized to *which* channel is brought up, not *how* the rest of the program works.

```
start:
    cli / set DS=ES=CS / sti
    print banner

    call bring_up_channel       ; <-- variant-specific
    jc   fail_no_channel

    call get_stage1_meta        ; size, blocks, image_sum16
    jc   fail_meta

    call check_stage1_size      ; 0 < size <= MAX_STAGE1_SIZE
    jc   fail_size

    call download_stage1        ; per-block CRC-16 + ACK/NAK
    jc   fail_download

    call verify_image_checksum  ; whole-image 16-bit additive
    jc   fail_checksum

    setup_handoff_registers     ; AX/BX/CX/DX/DS/ES
    jmp  STAGE1_LOAD_OFF        ; near jump to 0x0800
```

The download loop, CRC-16/CCITT-FALSE bitwise implementation, 16-bit additive image checksum, hex-printing helpers, and the `STAGE1_LOAD_OFF = 0x0800`, `MAX_STAGE1_SIZE = 50000`, `BLOCK_SIZE = 64` constants are **byte-for-byte identical** across all three variants. Only the channel layer differs. `s0_xt.asm` is the reference implementation for the shared parts; see [`dos/stage0/s0_xt.asm`](../dos/stage0/s0_xt.asm).

### Shared block protocol

```
DOS -> Pico:   CMD_GET_BLOCK, u16 block_no
Pico -> DOS:   u8 payload_len, payload[payload_len], u16 crc16_ccitt
DOS -> Pico:   ACK (0x06) or NAK (0x15)
```

CRC is CRC-16/CCITT-FALSE (poly `0x1021`, init `0xFFFF`, no reflection, no xor-out). The whole-image checksum is a 16-bit additive sum of all `stage1_size` bytes, sent in the metadata block before download begins.

### Shared metadata block

```
Pico -> DOS, in response to CMD_GET_META:
    u16 stage1_size       ; bytes, 0 < size <= 50000
    u16 stage1_blocks     ; ceil(size / 64)
    u16 image_sum16       ; 16-bit additive checksum of entire image
```

### Shared retry / timeout policy

```
PROBE_RETRIES   = 3       per LPT base
BLOCK_RETRIES   = 5       per block (NAK and re-request)
TIMEOUT_OUTER   = 0xFFFF  outer wait loop iterations
TIMEOUT_INNER   = 0x0010  inner wait loop iterations
DELAY_COUNT     = 200     conservative for 4.77 MHz XT; tuned by host class
```

These are deliberately the same across variants so behaviour on a wedged channel is uniform. The DELAY_COUNT value is the only one likely to be tuned per variant; see [§Per-variant timing](#per-variant-timing).

## Hand-off ABI

The boundary between Stage 0 and Stage 1 is a **fixed register-only contract**, identical across all three variants. Stage 1 reads `DX` to know which channels Stage 0 brought up.

```
On entry to Stage 1 at CS:0x0800:
    AX = 0x3150              'P1' marker (literal bytes 'P','1')
    BX = LPT base port       chosen by Stage 0 (0, 0x3BC, 0x378, or 0x278)
                             0 = no LPT channel up
    CX = Stage 1 size        bytes loaded at CS:0800h
    DX = Channel-availability bitmap (computed at hand-off time):
            bit 0 (0x01) = LPT channel up
            bit 1 (0x02) = i8042 KBD private channel up
            bit 2 (0x04) = i8042 AUX private channel up
          DX != 0 is invariant — if no channel came up, Stage 0 fails to DOS
          rather than handing off.
    DS = ES = CS = host PSP segment
    Direction flag: cleared
    Interrupts: enabled
    IRQ mask state: see "IRQ mask at hand-off" below
```

**`DX` reports what actually works, not what the variant theoretically supports.** Stage 0 tries every channel its variant has hardware for, then hands off with the bitmap of those that came up. If LPT probe fails on AT but KBD private unlock succeeds, `DX = 0x0002` (not `0x0003`); if the reverse, `DX = 0x0001`. Stage 1 reads `DX` to know what it can use *right now* without re-probing.

```asm
test dx, 1    ; LPT actually usable?
test dx, 2    ; KBD private actually unlocked?
test dx, 4    ; AUX private actually unlocked?
```

Per-variant achievable `DX` values:

| Variant | Achievable `DX` | Failure → no hand-off |
|---|---|---|
| `s0_xt.asm` | `0x0001` only | LPT probe fail (the only channel) |
| `s0_at.asm` | `0x0001`, `0x0002`, `0x0003` | both LPT probe and KBD unlock fail |
| `s0_ps2.asm` | any of `0x0001`–`0x0007` | LPT + KBD + AUX all fail |

**Why a bitmap instead of an opaque kind enum?** Earlier revisions of this doc used `0x0003`/`0x0005`/`0x0007` as static "variant identity" codes (XT_LPT_BOOTSTRAP / AT_DUAL_CHANNEL / PS2_TRIPLE_CHANNEL). That was wrong on two counts: (1) `0x0005` had bit 2 set with no AUX, breaking `test dx, 4`; (2) the static-per-variant value lied to Stage 1 whenever a probe failed (e.g., AT with LPT down still reported `bit 0 = LPT up`). The current dynamic bitmap fixes both. New variants extend the bitmap by assigning new bits, never by re-using or re-defining existing ones.

**Why no separate "variant identity" register?** Stage 1 only cares about channel state, not era. If it needs era for some reason (it shouldn't), it can read CMOS or BIOS data area. Adding a variant byte to the hand-off ABI would be dead weight.

### IRQ mask at hand-off

| Variant | IRQ1 (KBD) | IRQ12 (AUX) | LPT IRQ |
|---|---|---|---|
| `s0_xt.asm` | as BIOS left it | n/a (no AUX) | as BIOS left it |
| `s0_at.asm` | masked if KBD private is up; restored to BIOS state on LPT-only hand-off | n/a (no AUX) | as BIOS left it |
| `s0_ps2.asm` | masked if any private i8042 lane is up; restored to BIOS state on LPT-only hand-off | masked if AUX/private i8042 path is up; restored to BIOS state on LPT-only hand-off | as BIOS left it |

Stage 1 is responsible for re-enabling or re-hooking IRQs as it transitions to `DUAL_PLANE_ACTIVE`. Stage 0 leaves IRQs masked only when it successfully owns an i8042 private lane; if private unlock fails and the hand-off is LPT-only, Stage 0 restores the original PIC masks before continuing.

## Variant: `s0_xt.asm`

**Status:** ✅ Exists, 1082 bytes, see [`dos/stage0/s0_xt.asm`](../dos/stage0/s0_xt.asm).

**Channel:** LPT SPP nibble bidirectional. Keyboard is input-only (Pico → DOS during DEBUG injection); after Stage 0 starts, keyboard is unused.

**Pico-side mode:** XT keyboard emulation (`ps2_xt_dev.pio`, 9-bit unidirectional, Set 1 scancodes) for the bootstrap phase. After Stage 0 starts, Pico switches to LPT-only attention via the nibble protocol described below.

### LPT nibble protocol

```
DOS -> Pico:
    Write byte to DATA register (base+0).
    Toggle INIT line (control bit 2) as host strobe.

Pico -> DOS, two nibble reads per byte:
    Pico presents low nibble on status bits 3..6.
    Pico toggles status bit 7 (phase) when nibble stable.
    DOS waits for phase to differ from its persistent last_phase,
        commits the new phase, then samples the nibble.
    Same for high nibble.
    Combined: byte = (high << 4) | low.
```

This matches the `lpt_recv_nibble` / `lpt_recv_byte` implementation in `s0_xt.asm`. Conservative on purpose — `DELAY_COUNT = 200` is sized for 4.77 MHz XT clones with marginal LPT glue. Stage 1 negotiates faster modes (Byte, EPP, ECP) later.

**Invariant: persistent phase tracker.** The phase bit is *level*, not edge. The DOS receiver must maintain `last_phase` across `lpt_recv_nibble` calls and wait for the wire phase to differ from it — *not* re-sample the current phase as "previous" at every call. If the Pico has already toggled before the DOS side enters the next call, a non-persistent receiver treats the already-toggled phase as the new baseline and stalls waiting for a redundant second toggle, silently dropping or hanging on a byte. This invariant applies to every Stage 0 variant and every Stage 1 LPT-nibble receiver. Seed `last_phase` from the status register at probe time so the first nibble waits for the next Pico-driven toggle.

### Metadata bounds checking

Stage 0 trusts no Pico-supplied integer without cross-checking it. Two checks are mandatory after `CMD_GET_META`:

1. `0 < stage1_size <= MAX_STAGE1_SIZE`.
2. `stage1_blocks == ceil(stage1_size / BLOCK_SIZE)`. The Pico is free to report a different value, but the DOS side must reject it. Without this check, a forged `stage1_blocks` lets `download_stage1`'s block-indexed `stosb` walk over the PSP or Stage 0's own code before the whole-image checksum has a chance to reject it.

And one check per block:

3. `block_offset + payload_len <= stage1_size`, where `block_offset = block_no * BLOCK_SIZE`. Without this, the final block's `payload_len` can exceed the byte budget for that block and overflow past `stage1_size` into adjacent memory.

These three checks together guarantee that every byte Stage 0 writes during download is inside the `[STAGE1_LOAD_OFF, STAGE1_LOAD_OFF + stage1_size)` window, before any CRC or checksum gets a chance to reject the image. The whole-image checksum is a *correctness* check; the bounds checks are a *safety* check. Both are required.

### Probe sequence

```
For each base in [0x3BC, 0x378, 0x278]:
    init_lpt_control  (write 0x0C to base+2)
    for attempt in 1..PROBE_RETRIES:
        send 'P', '1', '2', '8'   ; CMD_PROBE0..3 = 'P128'
        recv 'O', 'K'             ; RSP_OK0..1
        if both match: lpt_base := base; success
    next base
fail if none responded
```

The four-byte probe word `'P128'` (`0x50 0x31 0x32 0x38`) is the Stage 0 → Pico magic. It's recognizable in a logic-analyzer trace and unlikely to appear in real printer traffic.

### Hand-off

```
AX = 0x3150
BX = [lpt_base]
CX = [stage1_size]
DX = 0x0001        ; LPT channel up — the only achievable value for XT
DS = ES = CS
jmp 0x0800
```

### Size budget

Current: 1082 B (after persistent-phase + bounds-check fixes). Target: stay under 1.5 KB. Going over 2 KB makes DEBUG-injection noticeably slower on XT (no buffered keyboard input — every byte waits for the previous ACK). See [§Stage –1 injection duration](#stage-1-injection-duration) for the per-byte cost.

## Variant: `s0_at.asm`

**Status:** Implemented in [`dos/stage0/s0_at.asm`](../dos/stage0/s0_at.asm), sharing the AT/PS/2 core in [`dos/stage0/s0_atps2_core.inc`](../dos/stage0/s0_atps2_core.inc).

**Channel:** LPT SPP nibble **plus** i8042 keyboard port as a bidirectional private channel after LED-pattern unlock. The keyboard channel is the fallback if LPT probe fails.

**Pico-side mode:** AT/PS/2 keyboard emulation (`ps2_at_dev.pio`, 11-bit bidirectional, Set 2 with optional 8042 translation to Set 1). The Pico's LED-pattern unlock detector (in `firmware/src/ps2/kbd.rs`, per [`ps2_private_channel_design.md`](ps2_private_channel_design.md) §implementation map) watches for the unlock sequence from Stage 0 and enters private mode on match.

### Differences from `s0_xt.asm`

1. **i8042 mastery.** Stage 0 probes LPT first, then attempts i8042 private channels so the final `DX` bitmap reflects every channel that came up. Before sending private-mode traffic it:
   - Mask IRQ1 at PIC1 (`in al, 21h ; or al, 02h ; out 21h, al`).
   - Flush i8042 output buffer (drain `0x60` while OBF in `0x64` is set).
   - Disable 8042 scancode translation (Controller Command Byte bit 6) so private-mode bytes survive cleanly.
2. **LED-pattern unlock** to put the Pico into private mode (see [§LED-pattern unlock](#led-pattern-unlock-shared-with-s0_ps2asm) below).
3. **Dual-channel probe order:** try LPT first (faster, ~50 KB/s), then try i8042 private mode (~200–1500 B/s per [`ps2_private_channel_design.md`](ps2_private_channel_design.md)) regardless of whether LPT succeeded.
4. **Wider hand-off bitmap range:** `DX ∈ {0x0001, 0x0002, 0x0003}` depending on which probes succeeded. Best-case `0x0003` = LPT + KBD private; minimum `0x0001` or `0x0002` if only one channel came up.

### i8042 mastery pattern

```asm
; --- Mask IRQ1 at PIC ---------------------------------------------------
in   al, 21h
or   al, 02h
out  21h, al

; --- Flush output buffer ------------------------------------------------
.flush:
in   al, 64h          ; status
test al, 01h          ; OBF?
jz   .flushed
in   al, 60h          ; consume and discard
jmp  .flush
.flushed:

; --- Wait input buffer empty before each command ------------------------
.wait_ibe:
in   al, 64h
test al, 02h          ; IBF?
jnz  .wait_ibe

; --- Disable scancode translation (CCB bit 6 = 0) -----------------------
mov  al, 20h          ; read CCB
out  64h, al
.wait_obf:
in   al, 64h
test al, 01h
jz   .wait_obf
in   al, 60h          ; AL = CCB
and  al, 0BFh         ; clear bit 6
mov  bl, al
.wait_ibe2:
in   al, 64h
test al, 02h
jnz  .wait_ibe2
mov  al, 60h          ; write CCB
out  64h, al
.wait_ibe3:
in   al, 64h
test al, 02h
jnz  .wait_ibe3
mov  al, bl
out  60h, al
```

This pattern is shared with `s0_ps2.asm` through `dos/stage0/s0_atps2_core.inc`.

### Channel selection

Stage 0 tries **both** channels and records which came up. It does not stop at the first success.

```
dx_bitmap = 0

1. try_lpt_channel:
     probe [0x3BC, 0x378, 0x278] as in s0_xt.asm
     if found: lpt_base = base; dx_bitmap |= 0x01.
2. try_kbd_private:
     i8042 mastery (above)
     LED-pattern unlock (§LED-pattern unlock)
     verify Pico signature byte via 0x60 read
     if signature matches: dx_bitmap |= 0x02.
3. if dx_bitmap == 0: print error, exit to DOS with errorlevel 1.
4. Download Stage 1 over LPT if (dx_bitmap & 0x01), else KBD private.
```

Stage 1 is downloaded over the fastest available channel — LPT if up, otherwise KBD private. The download protocol is the same (CMD_GET_META, CMD_GET_BLOCK, ACK/NAK); only the transport byte-pump changes (LPT nibble vs i8042 `0x60`/`0x64`). Both channels are left in working state at hand-off so Stage 1 can choose either independently.

### Hand-off

```
AX = 0x3150
BX = [lpt_base]    ; 0 if LPT failed and KBD-only path was used
CX = [stage1_size]
DX = [dx_bitmap]   ; one of 0x0001, 0x0002, 0x0003 — reflects channels up
```

### Size budget

Target: ≤2 KB. Adds ~500 B over `s0_xt.asm` for i8042 mastery + LED-pattern unlock + private-mode byte pump.

Measured: 1635 B.

## Variant: `s0_ps2.asm`

**Status:** Implemented in [`dos/stage0/s0_ps2.asm`](../dos/stage0/s0_ps2.asm), sharing the AT/PS/2 core in [`dos/stage0/s0_atps2_core.inc`](../dos/stage0/s0_atps2_core.inc).

**Channel:** LPT + i8042 KBD private mode + i8042 AUX private mode. The AUX channel is what enables the two-plane fallback transport ([`two_plane_transport.md`](two_plane_transport.md) §`PS2_FALLBACK_MUXED`).

**Pico-side mode:** Same as AT (`ps2_at_dev.pio` for KBD) plus a second PIO state machine running the same program for AUX. Both endpoints enter private mode on their respective unlock sequences.

### Differences from `s0_at.asm`

1. **Enables AUX port** via `OUT 64h, 0xA8` after KBD private mode is established.
2. **Sends AUX unlock** via the `0xD4` prefix (route next `OUT 60h` byte to AUX). The AUX unlock pattern is a mouse-side variant of the LED-pattern unlock — sample-rate sequence `200 / 100 / 80` (the IntelliMouse 4-byte-mode knock from [`ps2_private_channel_design.md`](ps2_private_channel_design.md) §AUX).
3. **Masks IRQ12** for AUX traffic during Stage 0, leaving IRQ1/IRQ12 masked at hand-off so Stage 1 can install its own handlers before unmasking.
4. **Wider hand-off bitmap range:** `DX ∈ {0x0001, …, 0x0007}` — any non-empty subset of `{LPT, KBD private, AUX private}`. Best-case `0x0007` = all three up.
5. **SuperIO port-swap tolerance:** the Super I/O may have auto-swap enabled, so the physical mini-DIN labelled "keyboard" may actually be wired to the AUX block (and vice versa). The current Stage 0 does not keep separate physical-port labels; it independently attempts KBD and AUX private unlock and records whichever logical i8042 lanes respond.

### Channel selection

Stage 0 tries **all three** channels and accumulates the bitmap. None of the steps are early-exit.

```
dx_bitmap = 0

1. try_lpt_channel:           same as s0_at.asm
     if probe succeeds:        dx_bitmap |= 0x01.
2. try_kbd_private:            same as s0_at.asm
     if signature matches:     dx_bitmap |= 0x02.
3. try_aux_private:
     enable AUX (OUT 64h, 0xA8)
     send AUX unlock (200/100/80 knock via 0xD4 prefix)
     verify Pico AUX signature
     if signature matches:     dx_bitmap |= 0x04.
4. if dx_bitmap == 0: print error, exit to DOS with errorlevel 1.
5. Download Stage 1 over the fastest available channel:
     LPT if (dx_bitmap & 0x01), else KBD private if (dx_bitmap & 0x02),
     else AUX private.
```

AUX is never the first choice for Stage 1 download — LPT is faster, and KBD private is roughly equivalent to AUX. AUX is brought up so it's *available* to Stage 1, not because Stage 0 itself uses it. The reason to enable AUX in Stage 0 (rather than letting Stage 1 do it) is that AUX enabling requires the keyboard lane to be fully owned, which is most cleanly done before any handler is installed — and Stage 1's first action otherwise would be to immediately re-master the i8042, wasteful.

### Hand-off

```
AX = 0x3150
BX = [lpt_base]    ; 0 if LPT failed
CX = [stage1_size]
DX = [dx_bitmap]   ; one of 0x0001..0x0007 — reflects channels up
```

### Size budget

Target: ≤2.5 KB. Adds AUX enable + AUX unlock over `s0_at.asm`.

Measured: 1880 B.

## LED-pattern unlock (shared with `s0_ps2.asm`)

The unlock sequence puts the Pico into **private mode**, where bytes sent via `0x60` are not interpreted as scancodes but as private-channel data. This is the protocol-legal way to multiplex a hidden bidirectional channel onto the keyboard wire — see [`ps2_private_channel_design.md`](ps2_private_channel_design.md) §discriminator.

### Sequence

The unlock is **ten host-to-device bytes** (five `0xED CMD / 0xXX DATA` pairs), each ACKed normally with `0xFA`, **followed by an additive two-byte private-mode magic** emitted by the Pico after the last ACK. The unlock never replaces any ACK — it appends two bytes after the sequence completes successfully.

Full wire trace, in order, with direction:

```
host → Pico:   0xED     (LED-set command)
Pico → host:   0xFA     (ACK)
host → Pico:   0x00     (data: all LEDs off)
Pico → host:   0xFA     (ACK)

host → Pico:   0xED
Pico → host:   0xFA
host → Pico:   0x07     (all three LEDs on)
Pico → host:   0xFA

host → Pico:   0xED
Pico → host:   0xFA
host → Pico:   0x00     (all off)
Pico → host:   0xFA

host → Pico:   0xED
Pico → host:   0xFA
host → Pico:   0x05     (ScrollLock + CapsLock)
Pico → host:   0xFA

host → Pico:   0xED
Pico → host:   0xFA
host → Pico:   0x02     (NumLock only)
Pico → host:   0xFA     ← final ACK of the LED sequence proper
Pico → host:   0xAA     ← magic byte 1, appended after final ACK
Pico → host:   0x55     ← magic byte 2, appended after final ACK
```

**Three things to note:**

1. **Every host byte gets `0xFA`.** Both `0xED` command bytes and their `0xXX` data bytes are ACKed normally, including the final `0xED` and the final `0x02`. The Pico behaves identically to a real keyboard for the entire 10-byte sequence.
2. **`0xAA 0x55` is additive.** It comes *after* the final `0xFA`, not in place of it. Stage 0 reads the 10 ACKs first, then waits for the two magic bytes. This keeps the byte stream layered: any observer that sees only the LED commands and their ACKs sees a perfectly normal LED sequence; the private-mode signal lives strictly after.
3. **Recognition is full-sequence.** The Pico's keyboard state machine only emits the magic if *all five pairs* arrived in order with valid data bytes (`0x00, 0x07, 0x00, 0x05, 0x02`). A partial or mis-ordered sequence — e.g., a real OS driver issuing one stray `0xED 0x00` — gets normal ACKs and no magic, leaving the Pico in keyboard mode.

After Stage 0 reads `0xAA 0x55`, the Pico is in private mode: subsequent bytes via `0x60` are private-channel data per [`ps2_private_channel_design.md`](ps2_private_channel_design.md), not scancodes.

### Why this specific pattern

- Every byte is a **valid keyboard command**. If anything above Stage 0 sees these bytes, it interprets them as ordinary LED-set traffic. No system damage.
- The pattern is **stateful** — recognition requires the exact five-pair sequence. Random keyboard traffic cannot accidentally trigger it.
- The LED bits walk through values (0, 7, 0, 5, 2) that are unlikely to appear consecutively in normal OS LED management.
- See [`design.md`](design.md) §7.4 for the canonical sequence; this doc just enumerates it.

### Failure handling

If the Pico does not respond with `0xAA 0x55` after the full pattern, `kbd_unlock_private` does:

1. Wait `~100 ms` (`delay_100ms`) to let slow Super I/O parts settle.
2. Retry the pattern once via `kbd_unlock_attempt`.
3. If still no magic: return CF set. The caller (`try_i8042_private`) records no `CHAN_KBD` bit and falls through to `try_aux` / `.restore_if_none`. If neither i8042 lane unlocks, the i8042 state (CCB + PIC masks + AUX enable) is fully restored before hand-off (or before `exit_error` returns to DOS) so the user can press Ctrl-Alt-Del and retry.

The "fail clean" path is critical because a stuck i8042 means the user can't type to retry. Power-cycle becomes mandatory, and that's bad UX.

## Variant selection (Pico-side)

The Pico decides which Stage 0 to type during Stage –1. This is technically a Pico-firmware concern, not a Stage 0 concern, but it determines which Stage 0 binary ever runs on a given host.

Decision sources, in priority order:

1. **Explicit operator override:** config GPIO or boot button at Pico power-on selects XT / AT / PS2 mode.
2. **Persisted flash flag:** previous successful boot's host-class is cached. Useful when the same KVM cable is plugged into the same machine repeatedly.
3. **Passive wire observation:** see [§What's actually detectable](#whats-actually-detectable) below — useful as a hint, never as the sole input.
4. **Default:** AT. AT-class machines are the most common vintage target, and `s0_at.asm` degrades gracefully to LPT-only if i8042 mastery fails.

The Pico's keyboard injector embeds **three `DEBUG` hex blobs** — one per Stage 0 variant — and selects which to type from the decision above. There is no auto-detection inside Stage 0 itself; by the time Stage 0 runs, the choice has been made.

### What's actually detectable

Partial autodetection is possible, but **not reliable enough to be the sole input**. The cost of typing the wrong Stage 0 (system hang, mandatory power-cycle) is much higher than the cost of asking the operator once — so detection is always a hint that informs the default, never an authoritative answer.

**What the Pico can observe passively during Stage –1:**

- **XT vs AT/PS/2 keyboard framing.** XT is 9-bit unidirectional, AT/PS/2 is 11-bit bidirectional with parity. If the host ever drives the clock low (host inhibit) **and follows it with a host-to-device byte**, it's AT/PS/2 — XT hosts never send. But a host that never sends a command looks the same as XT for an unbounded window.
- **Clock direction during host inhibit.** Both XT and AT/PS/2 hosts pull clock low to inhibit; only AT/PS/2 follows that with a host-to-device byte. The discrimination is not "clock pulled low" but "byte transmitted afterward."
- **POST-time scancode-set / LED requests.** Some AT/PS/2 BIOSes issue `0xF0` (Set Scancode Set) or `0xED` (Set LEDs) during POST. XT BIOSes never issue host-to-device bytes. **Any** host-to-device byte = AT or newer.

**What the Pico cannot distinguish from the keyboard wire alone:**

- **AT vs PS/2.** Wire-protocol-identical on the keyboard line. The only difference is whether an AUX port exists, and AUX lives on a separate physical connector. From the keyboard wire alone, AT and PS/2 are indistinguishable.
- **PS/2 vs SuperIO.** Wire-identical to PS/2 (documented in [`ps2_eras_reference.md`](ps2_eras_reference.md) — same `ps2_at_dev.pio` program handles both). No distinguishing observation is necessary because `s0_ps2.asm` covers both eras.

### Practical detection chain

```
1. AUX-connector electrical presence
   If the Pico's AUX cable is plugged in AND the AUX line shows
   activity (clock pulses, BAT request on power-up) → PS/2 or SuperIO.
   If AUX cable is plugged in but the line is dead       → AT (no AUX)
                                                           or PS/2 with
                                                           AUX disabled.
   This is the cleanest discriminator, but requires the AUX cable.

2. Passive POST observation (≤2 s window after power-up)
   Any host-to-device keyboard byte → AT or newer.
   Silence                          → likely XT (but ambiguous —
                                       some AT BIOSes never send).

3. Fallback default
   AT. s0_at.asm degrades to LPT-only if i8042 mastery fails, so:
     - AT binary on a PS/2 host  → works, loses AUX only.
     - AT binary on an XT host   → i8042 mastery fails, falls through
                                   to LPT — also recoverable.
   The asymmetric failure modes make AT the safest blind choice.
```

**Summary:** yes for XT vs AT+, no for AT vs PS/2 on the keyboard wire alone, yes for PS/2 if AUX is electrically present. Operator override and persisted flash flag remain the authoritative inputs; wire observation refines the default when those are absent.

## Stage –1 injection duration

How long does the user wait between "Pico starts typing" and "Stage 0 banner appears"? It's a function of Stage 0 size, DEBUG hex-entry overhead, and CPU-bound BIOS/DEBUG processing per character. This section is the model — re-run the math when variant sizes change.

### Character cost model

The Pico types this script verbatim into DOS's `DEBUG`:

```
DEBUG STAGE0.COM<CR>                            17 chars
E XXXX BB BB BB BB BB BB BB BB<CR>              32 chars per 8-byte line
... ⌈N / 8⌉ such lines ...
RCX<CR>NNNN<CR>                                  9 chars
W<CR>Q<CR>                                       4 chars
STAGE0<CR>                                       7 chars  (run Stage 0)
```

So **`chars_typed = 37 + 32 × ⌈N / 8⌉`** where `N` is the Stage 0 binary size in bytes.

DEBUG is case-insensitive for commands and hex digits, so the script uses all-lowercase ASCII — no Shift-modifier sequences, every character is one Set-2 make byte + a two-byte break (`0xF0 + scancode`), three wire bytes per character.

### Per-character timing per CPU class

At a Pico-driven PS/2 clock of ~12 kHz, each 11-bit frame is ~917 µs plus a ~100 µs inter-byte gap ≈ **1 ms/byte** ≈ **3 ms/char** wire time. On top of that:

| CPU class | Wire | BIOS INT 9 | DEBUG read + echo | Total / char | Best-case rate | Safe rate (30 % margin) |
|---|---:|---:|---:|---:|---:|---:|
| XT (4.77 MHz 8088) | 3 ms | ~60 µs | ~6–8 ms (slow CGA/MDA INT 10h) | ~10–12 ms | ~85 ch/s | **~60 ch/s** |
| AT (6–25 MHz 286) | 3 ms | ~20 µs | ~2–3 ms | ~5–6 ms | ~170 ch/s | **~120 ch/s** |
| PS/2 / SuperIO (16+ MHz 386+) | 3 ms | ~5 µs | ~0.5–1 ms | ~4 ms | ~250 ch/s | **~180 ch/s** |

The "safe rate" column applies 30 % headroom against BIOS keyboard-buffer overrun (16 chars on XT; DEBUG drains via `INT 21h AH=01` one char at a time). XT can't be pushed as hard because the keyboard wire is unidirectional — BIOS cannot inhibit the Pico when the buffer fills, so the Pico must self-pace. On AT+ the host pulls Clock low to inhibit; a feedback-paced injector can run closer to the best-case ceiling.

### Resulting injection time

`t_inject = chars_typed / rate`

| Variant | `N` (B) | Lines | Chars typed | Safe-rate time | Best-case time |
|---|---:|---:|---:|---:|---:|
| `S0_XT.COM` | 1082 | 136 | 4 389 | **~73 s** | ~52 s |
| `S0_AT.COM` | 1635 | 205 | 6 597 | **~55 s** | ~39 s |
| `S0_PS2.COM` | 1880 | 235 | 7 557 | **~42 s** | ~30 s |

### Total elapsed time (Pico starts typing → Stage 0 running)

Add fixed overheads:

| Overhead | XT | AT | PS/2+ |
|---|---:|---:|---:|
| `DEBUG.COM` load from disk | 1–3 s | 0.5–1 s | <0.5 s |
| DOS prompt redraw after `Q` | ~0.3 s | <0.1 s | <0.1 s |
| Stage 0 self-init + LPT probe | <0.1 s | <0.1 s | <0.1 s |

**Headline numbers** (safe-rate typing + DEBUG load):

| Variant | Total Stage –1 → Stage 0 entry |
|---|---:|
| XT | **~75–90 s** (1 min 15 s – 1 min 30 s) |
| AT | **~55–60 s** (≈ 1 min) |
| PS/2 / SuperIO | **~42–45 s** |

### Observations and sensitivities

- **XT is the slowest variant despite the smallest binary.** A 4.77 MHz 8088 spends so much time per character in INT 10h echo that the throughput penalty (~3×) more than wipes out the size advantage (~1.7×). Pathological worst case (slow CGA scroll + cautious pacing): ~2 minutes.
- **The DEBUG line-length lever is small.** Going from 8 bytes/line (4 chars/byte) to 16 or 24 bytes/line (~3.3 chars/byte) saves ~15 %. Not worth complicating Stage –1's error recovery on long lines.
- **AT+ inhibit-aware pacing can hit best-case.** If the Pico's injector reads the Clock line and adapts when BIOS inhibits, AT can approach 39 s and PS/2 ≈ 30 s. Without inhibit awareness, the safe-rate numbers apply.
- **Per-byte injection ACK (open decision §2 below) is a 2–4× cost.** If Stage –1 verifies each typed byte (e.g., via LPT loopback or screen-memory readback) before typing the next, XT slips to 3–5 minutes. The default proposal is to trust DEBUG and rely on Stage 0's banner / Stage 1 CRC for end-to-end verification — preserves the numbers above.
- **Size budgets are tighter than the binaries suggest.** Every 256 B added to a Stage 0 variant costs ~17 s on XT, ~9 s on AT, ~6 s on PS/2 at safe rate. The `≤1.5 / 2 / 2.5 KB` budgets in [§Per-file plan summary](#per-file-plan-summary) directly defend the injection-time budget.

### Rule of thumb

> **Plan for ~1 minute of unattended Pico typing on AT, ~40 seconds on PS/2, and up to ~1.5 minutes on XT.** This is a one-time cost per host until the operator (or persisted flash flag) caches the variant choice — closer to a slow floppy boot than to an interactive interaction.

## Per-variant timing

```
DELAY_COUNT (LPT nibble pacing):
    s0_xt    200   conservative for 4.77 MHz XT
    s0_at    100   286/386 can drive LPT faster
    s0_ps2    50   386+ comfortably handles tighter timing

i8042 IBE/OBF poll timeout:
    all      ~10ms equivalent in loop iterations
             (PS/2 spec allows up to 20ms for ACK)

LED-pattern inter-command spacing:
    all      ~1ms after each ACK (delay_1ms helper, sized for 6 MHz 286
             worst case; shorter on faster CPUs)

LED-pattern unlock retry:
    all      single retry after ~100ms wait if first attempt fails
             (delay_100ms; gives slow Super I/O parts time to settle)

AUX unlock (200/100/80 knock) inter-command spacing:
    s0_ps2   ~5ms after AUX enable and between sample-rate writes
             (delay_5ms; matches IntelliMouse-spec timing)
```

These are tuned per variant because the CPU class is known statically (XT = 4.77 MHz baseline, AT = 6–25 MHz, PS/2+ = 16+ MHz). Stage 1 measures real performance and adjusts; Stage 0 just needs to be safe.

## Failure handling

Every Stage 0 path that can fail must leave the system in a **typeable** state — i.e., the user can press Ctrl-Alt-Del and retry. This means:

1. **Restore scancode translation** if Stage 0 changed it (AT/PS2 variants).
2. **Restore the original IRQ1 mask** at PIC1 before exit on failure paths.
3. **Restore the original IRQ12 mask** at PIC2 before exit on failure paths (PS/2 variant only).
4. **Disable AUX if Stage 0 enabled it** but the hand-off doesn't reach Stage 1 (PS/2 variant). `restore_i8042_and_pic` sends `OUT 64h, 0xA7` when the `aux_enabled` flag is set, so the controller returns to BIOS in the same AUX state DOS expected. Successful hand-off with AUX private up leaves AUX enabled for Stage 1.
5. **Print a clear single-line error** identifying which step failed.
6. **Exit with `INT 21h AH=4Ch AL=01h`** so DOS errorlevel signals failure to any wrapping batch file.

Error messages should be short (`s0_xt.asm` precedent):

```
ERROR: Pico1284 DB-25 endpoint not found
ERROR: Stage1 metadata failed
ERROR: Stage1 size invalid
ERROR: Stage1 download failed
ERROR: Stage1 checksum failed
```

AT/PS2 private-lane probe failures are currently folded into the generic endpoint failure: Stage 0 can still hand off over LPT if LPT is up, so failed private unlock is diagnostic unless every channel fails.

### Recovery from `DEBUG`-injection-induced damage

If Stage –1 mistyped (e.g., a flaky keyboard wire dropped a scan code mid-injection), the resulting `.COM` is corrupt. Common failure modes:

- Garbage entry point → DOS reports "Bad command or file name" or crashes.
- Partial hex line → `DEBUG` rejects the `W` command.
- Wrong `RCX` size → `.COM` is truncated and crashes mid-execution.

Stage 0 cannot defend against these — by the time it would run, it doesn't exist. The Pico must verify the typed bytes during Stage –1 by snapshotting them through some side channel (LPT loopback during typing, if wired, or by re-reading what `DEBUG` echoes to the screen via the same approach pico1284 will eventually use for screen capture).

This is a **Stage –1 / Pico-firmware concern**, listed here only so the failure path is documented.

## Per-file plan summary

| File | Status | Size budget | Channels |
|---|---|---|---|
| [`dos/stage0/s0_xt.asm`](../dos/stage0/s0_xt.asm) | ✅ Exists (1082 B) | ≤1.5 KB | LPT |
| [`dos/stage0/s0_at.asm`](../dos/stage0/s0_at.asm) | ✅ Exists (1635 B) | ≤2 KB | LPT + KBD private |
| [`dos/stage0/s0_ps2.asm`](../dos/stage0/s0_ps2.asm) | ✅ Exists (1880 B) | ≤2.5 KB | LPT + KBD private + AUX private |
| [`dos/stage0/s0_atps2_core.inc`](../dos/stage0/s0_atps2_core.inc) | ✅ Exists | n/a (include) | shared by AT + PS/2 |
| `dos/stage0/lpt_nibble.inc` *(proposed)* | TODO | n/a (include) | shared by all three |
| `dos/stage0/crc16.inc` *(proposed)* | TODO | n/a (include) | shared by all three |
| `dos/stage0/handoff.inc` *(proposed)* | TODO | n/a (include) | shared by all three |

The AT and PS/2 variants share `s0_atps2_core.inc`; `s0_xt.asm` remains self-contained to keep the XT path simple and byte-stable. The proposed smaller includes are still an optional cleanup if `s0_xt.asm` and the shared AT/PS/2 LPT code begin to drift.

## Build

`dos/Makefile` builds all three Stage 0 variants:

```make
$(BUILD)/S0_XT.COM: stage0/s0_xt.asm | $(BUILD)
	$(NASM) -f bin -o $@ $<

$(BUILD)/S0_AT.COM: stage0/s0_at.asm stage0/s0_atps2_core.inc | $(BUILD)
	$(NASM) -f bin -o $@ $<

$(BUILD)/S0_PS2.COM: stage0/s0_ps2.asm stage0/s0_atps2_core.inc | $(BUILD)
	$(NASM) -f bin -o $@ $<
```

Output sizes verified by `make sizes`. CI should fail the build if any Stage 0 binary exceeds its size budget; this is what protects the DEBUG-injection time budget over the long term.

## Testing strategy

| Test | What it validates | Where it runs |
|---|---|---|
| `s0_xt.asm` on real XT clone | LPT nibble timing on 4.77 MHz hardware | bench |
| `s0_xt.asm` under DOSBox-X | DEBUG-injection round-trip + hand-off ABI | CI |
| `s0_at.asm` on real 286/386 | i8042 mastery + LED-pattern unlock | bench |
| `s0_at.asm` LPT fallback | LPT path used when KBD private unlock fails | bench (force-fail unlock) |
| `s0_ps2.asm` on real PS/2 | AUX enable + AUX unlock + IRQ12 masking | bench |
| `s0_ps2.asm` SuperIO port-swap | logical KBD/AUX lane probes do not rely on physical connector labels | bench |
| All variants under fault injection | Failure paths restore i8042 + PIC state cleanly | bench |

Bench testing is gated on Phase 0 hardware ([`design.md`](design.md) §22). Until Feather + transceiver boards arrive, DOSBox-X is the only available substrate, and it covers only `s0_xt.asm` and the shared bits.

A `delock-fixture` Linux-side test harness (see [`implementation_plan.md`](implementation_plan.md) §6 tools) can claim a USS-720 USB-to-parallel adapter and pretend to be a DOS host, exercising the Pico's LPT nibble peripheral side without needing real DOS hardware. This is the cheapest path to validating `s0_xt.asm` end-to-end before Phase 0 hardware lands.

## Open decisions

1. **Should the three Stage 0 binaries actually share include files**, or stay self-contained? Lean toward shared `.inc` files once the second variant exists; the duplication risk grows fast otherwise. Tracked in [§Per-file plan summary](#per-file-plan-summary).
2. **Per-byte injection ACK during Stage –1.** Does the Pico verify each typed byte made it into `DEBUG`'s hex buffer before typing the next? If yes, Stage –1 takes longer (2–4× per [§Stage –1 injection duration](#stage-1-injection-duration)) but Stage 0 is guaranteed intact. If no, Stage 0 must self-verify aggressively (which it already does via per-block CRC + image checksum on Stage 1, but Stage 0 itself has no checksum). Default proposal: trust DEBUG, rely on Stage 0 startup banner to confirm successful run.
3. **AUX hand-off mode in `s0_ps2.asm`:** does Stage 0 leave AUX in private mode (Pico continues to emit private-mode bytes), or revert AUX to normal mouse mode and let Stage 1 re-unlock? Default proposal: leave in private mode. Saves Stage 1 the re-unlock step and matches the LPT/KBD pattern.
4. **Stage 0 watchdog:** should Stage 0 hard-fail after N seconds if Stage 1 download stalls? Currently it relies on per-byte timeouts. A wall-clock watchdog would catch pathological cases where every byte just barely succeeds but throughput is unusable. Default proposal: yes, ~30 s wall-clock via BIOS tick count `INT 1Ah`.
5. **Pico variant-selection heuristics:** the priority order in [§Variant selection](#variant-selection-pico-side) is a proposal; [§What's actually detectable](#whats-actually-detectable) resolves the BIOS-POST-sniff question (it is feasible but only as a hint, never sole input). Open: how long is the passive-observation window before falling back to the default? Proposal: 2 s; tune once real hardware traces exist.
6. **Disable scancode translation: required or optional?** AT 8042 translation (CCB bit 6) converts Set 2 from the keyboard into Set 1 for the OS. Private-mode bytes are not scan codes, so translation arguably doesn't matter — but some real 8042s have surprising edge cases. Default proposal: disable in `s0_at.asm` and `s0_ps2.asm` to be safe.
7. **Recovery byte from Pico after failed unlock:** if the Pico is genuinely in keyboard-only mode and the unlock sequence completes by accident on a real keyboard (extremely unlikely but not impossible), what does the Pico return? Default proposal: never emit the magic unless the full 10-byte sequence (5 × `0xED 0xXX`) arrives within 200 ms, and the Pico's normal LED state didn't already match. Stale-pattern resistance.

## Related documents

- [`design.md`](design.md) §7 — canonical bootstrap ladder, DEBUG bootstrap, unlock sequence, recovery
- [`design.md`](design.md) §22 Phases 1–2 — Pico-firmware and Stage 0 roadmap
- [`ps2_eras_reference.md`](ps2_eras_reference.md) — per-era hardware reasoning behind the three-variant split
- [`ps2_private_channel_design.md`](ps2_private_channel_design.md) — private-mode framing, AUX IntelliMouse knock, i8042 ownership gotchas
- [`stage1_design.md`](stage1_design.md) — Stage 1 design; consumes Stage 0's `DX` channel bitmap and downloads Stage 2
- [`two_plane_transport.md`](two_plane_transport.md) — the session Stage 2 brings up after Stage 1 hands off
- [`implementation_plan.md`](implementation_plan.md) §2 — per-file plan and the hand-off contract
