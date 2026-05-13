# Stage 1 implementation (v1.0, as-built)

Companion to [`stage1_design.md`](stage1_design.md). The design doc captures *intent*; this document captures the *as-built state* of `dos/stage1/stage1.asm` so a reader can navigate the source without re-deriving the structure.

All file:line citations are against `dos/stage1/stage1.asm` unless noted.

---

## Synopsis

Stage 1 is a flat NASM binary (`org 0x800`) loaded by Stage 0 at `CS:0x0800` and entered via near jump. It:

1. Validates the Stage 0 hand-off (`AX = 0x3150`, `CX`/`DX` non-zero, `CX ≤ 50000`).
2. Detects LPT chipset capabilities (SPP / EPP / ECP).
3. Walks the IEEE 1284 negotiation ladder (ECP → EPP → Byte → SPP).
4. Runs a capability handshake (CAP_REQ / RSP / ACK) to learn Stage 2's size and CRC-32.
5. Stress-tests the byte pump (8× PING/PONG with timing).
6. Streams Stage 2 to `PICO1284.EXE` on disk with per-block CRC-16 retries and a whole-image CRC-32 verify.
7. Builds a `PICO_BOOT=...` environment block and EXECs `PICO1284.EXE` via `INT 21h AH=4Bh`.

Failure at any stage prints a one-line error, restores PIC mask state if necessary, and exits with errorlevel 1. The child's errorlevel propagates through Stage 1 on success.

---

## Build artifacts

| Artifact | Size | Source |
|---|---|---|
| `dos/build/stage1.bin` | 4821 B | `dos/stage1/stage1.asm` + `dos/stage0/lpt_nibble.inc` |

**Budget**: 8 KB (Stage 0 contract `MAX_STAGE1_SIZE = 50000` is the absolute cap, but the segment-layout target is ≤ 8 KB). Currently 59% used.

Build:

```sh
make -C dos stage1
```

The Makefile (`dos/Makefile:57-58`) wires the `lpt_nibble.inc` dependency, so editing the shared include rebuilds Stage 1.

---

## Top-level execution flow

`start:` at `stage1.asm:291` runs the pipeline straight-line, with `jc` branches at each step:

```
start (291)
├── snapshot AX/BX/CX/DX           (cli/sti bracket)
├── validate_handoff (445)         → jc fail_handoff
├── print_inherited_state (472)
├── lpt_chipset_detect (515)       → jc .no_lpt_yet  (Mode B re-probe TODO)
├── init_lpt_control               (in lpt_nibble.inc)
├── packet_self_test (873)         → reports OK/FAIL, no abort
├── pump_dispatcher_self_test (974)→ reports OK/FAIL, no abort
├── ieee1284_negotiate_ladder (1117)
├── cap_handshake (1198)           → CF informational; download skips if no size
├── pump_stress_test (1512)        → CF informational
├── download_stage2 (1767)         → jc .no_stage2 (errorlevel 1)
├── build_environment (2141)
├── exec_stage2 (2302)             → jc .exec_failed (errorlevel 1)
│
└── INT 21h AH=4Ch with child's errorlevel
```

**Failure dispatch** (`stage1.asm:389-441`): named entry points (`fail_handoff`, `fail_lpt`, `fail_neg`, `fail_caps`, `fail_size`, `fail_download`, `fail_crc`, `fail_write`, `fail_exec`) all funnel into `exit_error:` (`stage1.asm:432`), which prints a one-line error, calls `restore_irq_state_if_masked`, and exits with errorlevel 1.

---

## Subsystem map

| # | Subsystem | Routine(s) | Lines |
|---|---|---|---|
| 1 | Hand-off validation | `validate_handoff` | 445-468 |
| 2 | Inherited-state printing | `print_inherited_state` | 472-494 |
| 3 | LPT chipset detection | `lpt_chipset_detect`, `print_caps` | 515-580 |
| 4 | Failure-path IRQ unmask | `restore_irq_state_if_masked` | 624-641 |
| 5 | Console helpers | `puts`, `print_hex16/8/_digit`, `print_hex32`, `print_dec16` | 647-689, 1172-1180, 1650-1678 |
| 6 | CRC-16-CCITT | `crc16_ccitt` | 705-733 |
| 7 | Packet I/O | `packet_encode`, `packet_validate`, `packet_self_test` | 749-871 |
| 8 | Channel byte pump | `tx_drain`, `rx_fill`, `pump_dispatcher_self_test` | 929-1036, plus `lpt_nibble.inc` |
| 9 | IEEE 1284 negotiation | `ieee1284_negotiate`, `ieee1284_negotiate_ladder` | 1038-1170 |
| 10 | Capability handshake | `cap_handshake`, `send_cap_req`, `send_cap_ack`, `recv_cap_rsp`, `parse_cap_rsp_fields`, `print_cap_summary` | 1198-1463 |
| 11 | Time helpers | `read_centisec_clock`, `compute_elapsed_centis` | 1465-1500 |
| 12 | Pump stress test | `pump_stress_test`, `stress_one_iter`, `print_stress_result` | 1512-1648 |
| 13 | CRC-32 / IEEE | `crc32_init`, `crc32_update_byte`, `crc32_buf`, `crc32_finalize` | 1692-1745 |
| 14 | Stage 2 download | `download_stage2`, `abort_download`, `request_one_block`, `commit_one_block`, `send_block_nak` | 1767-2138 |
| 15 | EXEC env block | `build_environment`, `emit_str/hex16/hex8/hex_digit/dec16/mode_name` | 2141-2300 |
| 16 | Stage 2 EXEC | `exec_stage2` | 2302-2353 |

---

## Memory layout within the COM segment

```
0x0000 – 0x00FF   PSP                            (DOS-managed)
0x0100 – 0x07FF   Stage 0 .COM                   (still resident; may overwrite)
0x0800 – 0x1AD4   Stage 1 .bin                   (this code, 4821 B)
0x2600 – 0x26FF   TX_BUF                         (packet encode scratch)
0x2700 – 0x27FF   RX_BUF                         (packet decode scratch)
0x2800 – 0x2FFF   PICO_BOOT env block            (paragraph-aligned, ENV_BUF_OFF)
0x3000 – ...      released to child via AH=4Ah   (EXEC_RESIZE_PARAS = 0x300)
```

Buffer offsets are constants (`stage1.asm:121-122`), not `db` reservations, so they don't count against `stage1.bin` size.

---

## Wire protocol

### Packet framing (`stage1.asm:97-122`)

```
SOH | CMD | SEQ | LEN_HI | LEN_LO | PAYLOAD | CRC_HI | CRC_LO | ETX
```

- `SOH = 0x01`, `ETX = 0x03`.
- `LEN` is **big-endian** u16 payload length.
- `SEQ` auto-increments via `[tx_seq]` in `packet_encode`.
- `CRC-16-CCITT` (poly `0x1021`, init `0xFFFF`, no refl, no xor-out) computed over `CMD..end-of-payload` (i.e. 4 + payload_len bytes), bytes appended big-endian.

### Stage 1 command set (`stage1.asm:106-117`)

| CMD | Value | Direction | Payload |
|---|---|---|---|
| `CMD_CAP_REQ` | `0x00` | DOS → Pico | empty |
| `CMD_CAP_RSP` | `0x0F` | Pico → DOS | version, size, CRC-32, mode (see CAP_RSP layout) |
| `CMD_CAP_ACK` | `0x0E` | DOS → Pico | empty |
| `CMD_PING` | `0x10` | DOS → Pico | 64 B fixed pattern (stress test) |
| `CMD_PONG` | `0x11` | Pico → DOS | 64 B echo |
| `CMD_SEND_BLOCK` | `0x20` | DOS → Pico | u32 block_no BE |
| `CMD_RECV_BLOCK` | `0x21` | Pico → DOS | u32 block_no BE + u8 byte_count + data |
| `CMD_BLOCK_ACK` | `0x22` | DOS → Pico | u32 block_no BE |
| `CMD_BLOCK_NAK` | `0x23` | DOS → Pico | u32 block_no BE |
| `CMD_ERROR` | `0x13` | (reserved) | — |
| `CMD_ACK` | `0x15` | (reserved) | — |
| `CMD_NAK` | `0x16` | (reserved) | — |

### CAP_RSP payload layout (`stage1.asm:191-205`)

Offsets are payload-relative (i.e. from `RX_BUF + PACKET_HEADER_LEN`):

| Offset | Size | Field |
|---|---|---|
| 0 | u8 | `version_major` (must be 1) |
| 1 | u8 | `version_minor` |
| 23 | u8 | `active_parallel_mode` (`NEG_MODE_*`) |
| 28 | u32 BE | `stage2_image_size` |
| 32 | u32 BE | `stage2_image_crc32` |
| 36+ | — | variable device-string tail (not parsed) |

Minimum payload length: `CAP_RSP_MIN_PAYLOAD = 36`. Stage 1 reads fields by absolute offset rather than parsing forward.

### IEEE 1284 negotiation (`stage1.asm:155-189`)

Mirrors `drivers/parport/ieee1284.c`. The host:

1. Drives the extensibility byte (`XFLAG_*`) on data lines.
2. Drives control to `CTRL_NEG_REQ = 0x06` (nSelectIn high wire, nAutoFd low).
3. Waits for peripheral to assert nAck low (status bit 6 = 0), with `NEG_TIMEOUT_OUTER = 0x100` iterations.
4. Pulses nStrobe low (`CTRL_NEG_STRB = 0x07`) then high.
5. Reads Select (status bit 4): high = accepted, low = rejected.
6. On accept: drives control to `CTRL_NEG_ACK = 0x04` (commit). On reject: restores SPP idle (`CTRL_BASE = 0x0C`).

Extensibility bytes: `XFLAG_NIBBLE = 0x00`, `XFLAG_BYTE = 0x01`, `XFLAG_ECP = 0x14`, `XFLAG_EPP = 0x40`.

Ladder order (`ieee1284_negotiate_ladder`, `stage1.asm:1117`): ECP → EPP → Byte → SPP, gated by `[dp_caps_flags]`. Result stored in `[negotiated_mode]` as a `NEG_MODE_*` enum.

---

## Stage 2 download protocol

### Per-block flow (`stage1.asm:231-251`, `download_stage2:1767`)

```
DOS  → Pico:  SEND_BLOCK    payload = u32 block_no (BE)
Pico → DOS:   RECV_BLOCK    payload = u32 block_no (BE)
                                    + u8  byte_count (1..64)
                                    + byte_count data bytes
DOS  → Pico:  BLOCK_ACK     payload = u32 block_no (BE)    (success)
              BLOCK_NAK     payload = u32 block_no (BE)    (retry)
```

### Constants (`stage1.asm:248-252`)

- `DOWNLOAD_BLOCK_SIZE = 64`
- `DL_BLOCK_RETRIES = 3`
- `RECV_BLOCK_HDR_LEN = 5` (u32 block_no + u8 byte_count)
- `DL_PROGRESS_MASK = 0x003F` (a `.` printed every 64 blocks ≈ 4 KB)

### Validation per block (`request_one_block`, `stage1.asm:1977`)

1. `rx_fill` timeout / partial header → fail.
2. Payload length must equal `RECV_BLOCK_HDR_LEN + expected_byte_count`.
3. Total packet must fit `PACKET_BUF_SIZE = 256` (overflow guard).
4. `packet_validate` confirms CRC-16-CCITT and ETX.
5. cmd must be `CMD_RECV_BLOCK`.
6. Echoed `block_no` must match `[current_block]` (u16 with high two bytes zero).
7. `byte_count` must equal `[expected_byte_count]`.

### Commit per block (`commit_one_block`, `stage1.asm:2075`)

1. Fold data bytes into running CRC-32 via `crc32_buf`.
2. `INT 21h AH=40h` write to `[stage2_file_handle]`; reject short writes.
3. Send `BLOCK_ACK` with same u32 BE payload.

### Block sizing

`expected_byte_count = min(64, stage2_image_size − current_block * 64)`. The last block may be short. Computed inline in `download_stage2:.block_loop` (`stage1.asm:1839-1872`) via 32-bit subtract.

### Disk handling

- Open: `INT 21h AH=3Ch` (create/truncate) on `PICO1284.EXE` in cwd.
- Write: `INT 21h AH=40h` per block (no buffering — DOS handles).
- Close: `INT 21h AH=3Eh` after all blocks.
- Delete: `INT 21h AH=41h` on any failure (block retries exhausted, close error, CRC mismatch).

### CRC-32 (zlib / IEEE)

Reflected, poly `0xEDB88320`, init `0xFFFFFFFF`, xor-out `0xFFFFFFFF`. Bit-by-bit, ~30 B. State held in `DX:AX` during the inner loop, persisted in `[crc32_state]` (LE, 4 bytes).

Implementation pattern (`crc32_update_byte`, `stage1.asm:1699`):

```nasm
xor al, byte_in            ; state low ^= byte (LSB)
mov cx, 8
.bit:
    shr dx, 1
    rcr ax, 1              ; CF = original bit 0 of 32-bit state
    jnc .no_xor
    xor ax, 8320h          ; low half of poly
    xor dx, 0EDB8h         ; high half
.no_xor:
    loop .bit
```

Finalize XORs with `0xFFFFFFFF` and returns `DX:AX`.

---

## EXEC + environment

### PSP environment block (`build_environment`, `stage1.asm:2141`)

Built at `CS:ENV_BUF_OFF = 0x2800` (paragraph-aligned). Format (DOS 3.0+):

```
"PICO_BOOT=LPT=XXXX MODE=YYY CHAN=N VER=X.Y\0"
"\0"                                          ; double-NUL terminates list
WORD 1                                        ; count of strings that follow
"PICO1284.EXE\0"                              ; child program path
```

Field details:

- `LPT=XXXX` — 4 uppercase hex digits of `[saved_bx]` (no `0x` prefix). `0000` if no LPT.
- `MODE=` — one of `ECP`, `EPP`, `BYTE`, `SPP`, `NEG_FAILED` (from `[negotiated_mode]`).
- `CHAN=N` — decimal of `[saved_dx]` (Stage 0's channel-availability bitmap, 1..7).
- `VER=X.Y` — decimal `[pico_version_major].[pico_version_minor]` from CAP_RSP.

The env-block segment passed to the child is `CS + ENV_BUF_PARAS` (`ENV_BUF_PARAS = ENV_BUF_OFF >> 4 = 0x280`).

### Buffer-emitting helpers (`stage1.asm:2197-2300`)

Variants of the console `print_*` helpers that target `ES:DI` instead of `INT 21h AH=02h`:

- `emit_str` — copies ASCIIZ source (DS:SI) to ES:DI, excluding the terminator.
- `emit_hex16` / `emit_hex8` / `emit_hex_digit` — uppercase, no prefix.
- `emit_dec16` — decimal, no leading zeros.
- `emit_mode_name` — dispatches AL = `NEG_MODE_*` to the right string.

### EXEC sequence (`exec_stage2`, `stage1.asm:2302`)

1. **Resize** our memory allocation via `INT 21h AH=4Ah` with `BX = EXEC_RESIZE_PARAS = 0x300` paragraphs (~12 KB) and `ES = CS`. This releases the rest of conventional memory so DOS can give it to the child.
2. **Patch parameter block** segments (`exec_param_block`, `stage1.asm:2411`) to CS-relative values at runtime:
   - `+0 WORD env_seg = CS + ENV_BUF_PARAS`
   - `+2 DWORD cmdtail = exec_cmd_tail : CS`
   - `+6 DWORD fcb1 = exec_blank_fcb : CS`
   - `+10 DWORD fcb2 = exec_blank_fcb : CS`
3. **Load and execute**: `INT 21h AH=4Bh AL=00` with `ES:BX = param block`, `DS:DX = "PICO1284.EXE"`. DS and ES are pushed/popped around the call (DOS clobbers them).
4. **Query exit code**: on return, `INT 21h AH=4Dh` retrieves the child's errorlevel in AL.
5. Stage 1 exits via `INT 21h AH=4Ch` with the child's AL as its own errorlevel.

`exec_cmd_tail` is `db 0, 13` (LEN=0, CR — empty command line). `exec_blank_fcb` is 16 zero bytes, shared by both FCB1 and FCB2.

---

## Failure handling

### Single-line error pattern

Each failure path is a tiny stub that loads `SI` with a message and jumps to `exit_error`:

```
fail_handoff:   mov si, msg_err_handoff   ; "ERROR: bad handoff from Stage 0"
                jmp exit_error
...
exit_error:     call puts
                call restore_irq_state_if_masked
                mov ax, 4C01h
                int 21h
```

### IRQ unmask (`restore_irq_state_if_masked`, `stage1.asm:624`)

Stage 0 may have masked IRQ1 (KBD) and / or IRQ12 (AUX) to acquire i8042 private mode. On a Stage 1 failure exit, we unmask them so the user can type at the prompt. Conservative — only touches IRQ12 if the AUX bit was set in `[saved_dx]`.

### Download failure (`download_stage2:.fail_*`, `stage1.asm:1916-1938`)

- Closes the file handle (`AH=3Eh`) if still open.
- Deletes the partial file (`AH=41h`).
- Prints a specific diagnostic (`msg_dl_fail_{size,create,block,close,crc}`).
- Returns with CF set; `start:` then exits with errorlevel 1.

### Skip-cleanly conditions

`download_stage2` and `cap_handshake` skip cleanly (CF set with a `msg_*_skipped_*`) rather than failing when:

- No pump is up (`current_pump == PUMP_NONE`).
- No CAP size was received (Pico firmware not responding yet).
- Negotiated mode requires a byte pump that isn't implemented (EPP/ECP path; only SPP works in v1.0).

This lets the scaffold path complete even before the Pico-side firmware lands.

---

## Data section (`stage1.asm:2356-2452`)

| Symbol | Type | Purpose |
|---|---|---|
| `saved_ax/bx/cx/dx` | `dw` × 4 | Hand-off register snapshot taken under `cli`. |
| `ecr_saved` | `db` | ECR value at probe start; restored after detection. |
| `dp_caps_flags/irq/dma` | `db` × 3 | LPT chipset capability struct. |
| `tx_seq` | `db` | Auto-increment SEQ field for outbound packets. |
| `test_payload_hello` | `db 'HELLO'` | Packet self-test payload. |
| `current_pump` | `db` | `PUMP_*` enum: dispatches `tx_drain` / `rx_fill`. |
| `negotiated_mode` | `db` | `NEG_MODE_*` enum after `ieee1284_negotiate_ladder`. |
| `pico_version_major/minor/active_mode` | `db` × 3 | Parsed from CAP_RSP. |
| `stage2_image_size/crc32` | `dd` × 2 | Parsed from CAP_RSP (LE in memory). |
| `stress_errors/start_centis/elapsed_centis` | `dw` × 3 | Pump stress test counters / timing. |
| `stress_payload` | `db` × 64 | Incrementing 0..63 fixed pattern. |
| `stage2_file_handle` | `dw` | DOS file handle from `AH=3Ch`. |
| `current_block/total_blocks/blocks_received` | `dw` × 3 | Download progress. |
| `expected_byte_count/retry_count` | `db` × 2 | Per-block state. |
| `crc32_state` | `dd` | Running CRC-32. |
| `send_block_payload` | `db` × 4 | u32 BE block_no, reused per request. |
| `stage2_filename` | `db 'PICO1284.EXE',0` | ASCIIZ for `AH=3Ch/41h/4Bh`. |
| `exec_param_block` | structure | EXEC parameter block (env_seg + 3 DWORDs). |
| `exec_cmd_tail` | `db 0, 13` | LEN=0 + CR (empty command line). |
| `exec_blank_fcb` | `times 16 db 0` | Shared blank FCB. |
| `env_*_str` | ASCIIZ × 4 | "PICO_BOOT=LPT=", " MODE=", " CHAN=", " VER=". |
| `str_mode_*`, `str_neg_failed` | ASCIIZ × 5 | Mode names for `emit_mode_name`. |
| `msg_*` | `$`-terminated | All console strings. |

`lpt_base` and `last_phase` live in `dos/stage0/lpt_nibble.inc`, not here — they're shared with Stage 0.

---

## Shared include: `dos/stage0/lpt_nibble.inc`

Brought in at `stage1.asm:946` (`%include "stage0/lpt_nibble.inc"`). The parent file must:

1. Define `DELAY_COUNT` before the `%include`. Stage 1 uses 100 (AT-class baseline, line 134).
2. Populate `[lpt_base]` before calling any pump routine. Stage 1 does this in `lpt_chipset_detect` via `mov ax, [saved_bx]; mov [lpt_base], ax`.

Exports used by Stage 1:

- Constants: `LPT_DATA/STATUS/CONTROL`, `CTRL_INIT`, `CTRL_BASE`, `STAT_NIBBLE_MASK`, `STAT_PHASE`, `TIMEOUT_OUTER`, `TIMEOUT_INNER`.
- Routines: `init_lpt_control`, `lpt_send_byte`, `lpt_recv_byte`, `lpt_recv_nibble`, `tiny_delay`, `tiny_delay_short`.
- Data: `lpt_base` (word), `last_phase` (byte).

The persistent `last_phase` invariant is critical: `lpt_recv_nibble` waits for the Pico's phase bit to **differ** from `[last_phase]` rather than sampling fresh each call. See `lpt_nibble.inc:13-15` and `lpt_nibble.inc:143-204`.

---

## Self-tests run during the scaffold path

| Test | Routine | Failure handling |
|---|---|---|
| Packet round-trip | `packet_self_test` (873) | Reports OK/FAIL; does not abort. |
| Pump dispatcher routing | `pump_dispatcher_self_test` (974) | Reports OK/FAIL; does not abort. |
| Channel stress | `pump_stress_test` (1512) | Reports `STAGE1: stress: N iter, M errors, K centis`; CF informational in v1.0. |

These run unconditionally on every Stage 1 invocation. They cost a few hundred bytes total and catch CRC/framing bugs before any real Pico traffic.

---

## Not yet implemented

Tracked in [`implementation_plan.md` §3.Status](implementation_plan.md):

| Subsystem | Blocker / reason |
|---|---|
| LPT EPP byte pump | Pico-side firmware doesn't speak EPP yet; `current_pump` stays `PUMP_LPT_SPP`. |
| LPT ECP byte pump | Same; ECP DMA is Stage 2 territory regardless. |
| Auto-downgrade ladder from stress-test failure | Needs EPP/ECP pumps to fall back from (SPP is already the bottom). |
| LPT Mode B re-probe | Stage 0 brought up only PS/2; Stage 1 should retry LPT bases. Skeleton present in `lpt_chipset_detect:.no_lpt`. |
| KBD private byte pump | For Mode B path. Stage 1 would inherit Stage 0's unlocked private channel. |
| AUX private byte pump | Same. |

All five are gated on either Pico-side firmware Phase 3+ or on Mode B becoming the active path. Stage 1's structure (the `current_pump` dispatcher, the `dp_caps` struct, the `negotiated_mode` enum) is ready for them — each is a new case in `tx_drain` / `rx_fill` plus a probe in `lpt_chipset_detect`.

---

## Testing

No unit-test harness exists for Stage 1; validation today is the built-in self-tests + bench validation against the Pico-side firmware (once it lands).

To smoke-test the build:

```sh
make -C dos stage1
ls -la dos/build/stage1.bin       # size sanity check
```

To inspect the binary layout:

```sh
nasm -f bin -l /tmp/stage1.lst dos/stage1/stage1.asm -o /tmp/stage1.bin
less /tmp/stage1.lst
```

End-to-end validation requires the Pico-side firmware responding to `CAP_REQ` and the block protocol. Until then, Stage 1 prints `STAGE1: CAP handshake skipped (...)` and exits cleanly with errorlevel 1 via the `.no_stage2` path.

---

## Related documents

- [`stage1_design.md`](stage1_design.md) — design intent (the "why" and the "should")
- [`stage0_design.md`](stage0_design.md) — Stage 0 design, including the hand-off ABI
- [`design.md`](design.md) §9 (packet framing), §10 (capability handshake)
- [`two_plane_transport.md`](two_plane_transport.md) — overall transport model
- [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) — IEEE 1284 wire reference
- [`implementation_plan.md`](implementation_plan.md) §3 — overall status table and roadmap
