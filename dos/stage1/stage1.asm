; stage1.asm
; ---------------------------------------------------------------------------
; S1 - DOS Stage 1 loader for Pico1284.
;
; Entered by Stage 0 (S0_XT/AT/PS2) via near jump at CS:0x0800 with the
; channel-availability bitmap in DX (see docs/stage0_design.md §Hand-off ABI).
; Stage 1's job, in order:
;
;   1. Validate the hand-off ABI register state.
;   2. Probe LPT chipset capabilities (ECR / EPP).
;   3. Negotiate IEEE 1284 mode (ECP -> EPP -> Byte -> SPP).
;   4. Minimal CAP_REQ / CAP_RSP handshake.
;   5. Download PICO1284.EXE with per-block CRC-16 + whole-image CRC-32.
;   6. Write PICO1284.EXE to disk; spawn via INT 21h AH=4Bh.
;
; Full design: docs/stage1_design.md
; Hand-off ABI: docs/stage0_design.md §Hand-off ABI
;
; This is the v1.0 scaffold:
;   - Hand-off validation:        implemented.
;   - Inherited-state printing:   implemented.
;   - Failure-path IRQ cleanup:   implemented.
;   - LPT chipset detection:      implemented (ECR + EPP probe).
;   - LPT SPP nibble pump:        implemented via shared stage0/lpt_nibble.inc.
;   - LPT Mode B re-probe:        TODO.
;   - CRC-16-CCITT + packet I/O:  implemented (buffer-based encode/decode
;                                 with round-trip self-test).
;   - Channel byte pump:          implemented for LPT SPP nibble; EPP/ECP/
;                                 KBD/AUX stubbed (return CF set).
;   - IEEE 1284 negotiation:      implemented (ECP -> EPP -> Byte -> SPP).
;                                 current_pump stays LPT_SPP until EPP/ECP
;                                 pumps land in a later iteration.
;   - Capability handshake:       implemented (CAP_REQ/RSP/ACK). REQ/ACK
;                                 are 0-byte payload; RSP parsed for version,
;                                 stage2_image_size, stage2_image_crc32,
;                                 active_parallel_mode. Skips cleanly until
;                                 Pico-side firmware responds.
;   - Pump stress test:           implemented. N=8 PING/PONG round-trips with
;                                 a 64-byte payload; counts errors, measures
;                                 elapsed centis. Reports only; ladder
;                                 fallback waits on EPP/ECP pumps.
;   - Stage 2 download:           implemented. Streams 64-byte blocks via
;                                 SEND_BLOCK / RECV_BLOCK / BLOCK_ACK, writes
;                                 directly to PICO1284.EXE on disk, runs a
;                                 CRC-32 over the received bytes and verifies
;                                 against stage2_image_crc32 at finish. On
;                                 any failure the partial file is deleted.
;   - Stage 2 EXEC + env:         implemented. Builds a fresh PSP environment
;                                 block at CS:ENV_BUF_OFF with PICO_BOOT=
;                                 LPT=XXXX MODE=YYY CHAN=N VER=X.Y, shrinks
;                                 our memory allocation via INT 21h AH=4Ah,
;                                 and EXECs PICO1284.EXE via INT 21h AH=4Bh.
;                                 Child exit code propagates to our caller.
; Returns to DOS with errorlevel 0 when the scaffold path completes cleanly,
; or errorlevel 1 on bad hand-off.
;
; Build:
;   nasm -f bin stage1.asm -o stage1.bin
; ---------------------------------------------------------------------------

bits 16
org 0x800

; ---------------------------------------------------------------------------
; Hand-off ABI constants (per docs/stage0_design.md)
; ---------------------------------------------------------------------------
HANDOFF_MARKER      equ 3150h           ; 'P1' = 0x3150 in AX at entry
MAX_STAGE1_SIZE     equ 50000           ; Stage 0's contract; defensive bound

CHAN_LPT            equ 01h
CHAN_KBD            equ 02h
CHAN_AUX            equ 04h
CHAN_ANY            equ CHAN_LPT | CHAN_KBD | CHAN_AUX
CHAN_I8042_ANY      equ CHAN_KBD | CHAN_AUX

; ---------------------------------------------------------------------------
; LPT chipset constants (docs/stage1_design.md §LPT chipset detection)
;
; LPT_DATA/STATUS/CONTROL and the SPP-nibble byte pump live in the shared
; stage0/lpt_nibble.inc, brought in at the end of code below.
; ---------------------------------------------------------------------------
LPT_EPP_ADDR        equ 3
LPT_EPP_DATA        equ 4
LPT_ECR             equ 402h        ; ECR at base + 0x402 (not base + small)

ECR_MODE_SPP        equ 000h        ; bits 7:5 = 000
ECR_MODE_PS2        equ 020h        ; bits 7:5 = 001
ECR_MODE_PPF        equ 040h        ; bits 7:5 = 010
ECR_MODE_ECP        equ 060h        ; bits 7:5 = 011
ECR_MODE_EPP        equ 080h        ; bits 7:5 = 100
ECR_MODE_MASK       equ 0E0h

ECR_TEST_PATTERN1   equ 035h        ; arbitrary; tests bits 0-5 of ECR
ECR_TEST_PATTERN2   equ 0CAh        ; complement of pattern1; tests inversion

CAPS_SPP            equ 01h
CAPS_EPP            equ 02h
CAPS_ECP            equ 04h
CAPS_ECP_DMA        equ 08h

; ---------------------------------------------------------------------------
; Packet framing (docs/design.md §9, docs/stage1_design.md §Packet I/O)
;
; Wire format: SOH | CMD | SEQ | LEN_HI | LEN_LO | PAYLOAD | CRC_HI | CRC_LO | ETX
; CRC-16-CCITT (poly 0x1021, init 0xFFFF) computed over CMD..end of payload.
; ---------------------------------------------------------------------------
SOH                 equ 01h
ETX                 equ 03h
PACKET_HEADER_LEN   equ 5           ; SOH + CMD + SEQ + LEN_HI + LEN_LO
PACKET_TRAILER_LEN  equ 3           ; CRC_HI + CRC_LO + ETX
PACKET_OVERHEAD     equ PACKET_HEADER_LEN + PACKET_TRAILER_LEN
PACKET_BUF_SIZE     equ 256         ; control-packet capacity; blocks stream

; Stage 1 packet command set (subset of design.md §9.1; see
; docs/stage1_design.md §Minimal packet I/O)
CMD_CAP_REQ         equ 000h
CMD_CAP_RSP         equ 00Fh
CMD_CAP_ACK         equ 00Eh
CMD_PING            equ 010h
CMD_PONG            equ 011h
CMD_ERROR           equ 013h
CMD_ACK             equ 015h
CMD_NAK             equ 016h
CMD_SEND_BLOCK      equ 020h
CMD_RECV_BLOCK      equ 021h
CMD_BLOCK_ACK       equ 022h
CMD_BLOCK_NAK       equ 023h

; TX/RX buffers live in segment scratch above stage1.bin but below the
; Stage 2 download region. NOT part of the binary image (no `db` emit).
TX_BUF_OFF          equ 2600h
RX_BUF_OFF          equ 2700h

; ---------------------------------------------------------------------------
; LPT SPP nibble protocol
;
; Constants (LPT_DATA/STATUS/CONTROL, CTRL_*, STAT_*, TIMEOUT_*), routines
; (init_lpt_control, lpt_send_byte, lpt_recv_byte, lpt_recv_nibble, tiny_delay,
; tiny_delay_short), and data (lpt_base, last_phase) live in the shared
; stage0/lpt_nibble.inc, which is brought in at the end of code below. Stage 1
; matches the wire protocol exactly so the same Pico-side firmware serves
; both Stage 0 and Stage 1.
; ---------------------------------------------------------------------------
DELAY_COUNT         equ 100         ; AT-class baseline; consumed by tiny_delay

; Channel byte-pump dispatch (docs/stage1_design.md §Channel handling)
PUMP_NONE           equ 0
PUMP_LPT_SPP        equ 1
PUMP_LPT_EPP        equ 2           ; TODO after IEEE 1284 negotiation
PUMP_LPT_ECP        equ 3           ; TODO
PUMP_KBD            equ 4           ; TODO (inherit unlocked KBD private)
PUMP_AUX            equ 5           ; TODO (inherit unlocked AUX private)

; ---------------------------------------------------------------------------
; IEEE 1284 negotiation (docs/design.md §8, docs/stage1_design.md
; §IEEE 1284 negotiation; reference: Linux drivers/parport/ieee1284.c)
;
; Extensibility bytes the host places on data lines during a negotiation
; request; the peripheral accepts (Select high) or rejects (Select low).
; ---------------------------------------------------------------------------
XFLAG_NIBBLE        equ 000h        ; reverse nibble (Compat/Nibble result)
XFLAG_BYTE          equ 001h        ; reverse byte mode
XFLAG_ECP           equ 014h        ; ECP request
XFLAG_EPP           equ 040h        ; EPP request

; Control-register bit composition for the negotiation events.
; (CTRL_BASE = 0x0C is the SPP idle state, defined in lpt_nibble.inc.)
;   NEG_REQ   = nSelectIn high (wire), nAutoFd low, nInit high, nStrobe high
;             = (bit 1=1) | (bit 2=1) = 0x06
;   NEG_STRB  = NEG_REQ + nStrobe low = 0x07
;   NEG_ACK   = nInit high alone (commit to target mode)
;             = (bit 2=1) = 0x04
CTRL_NEG_REQ        equ 06h
CTRL_NEG_STRB       equ 07h
CTRL_NEG_ACK        equ 04h

; Status-register bits the host reads from the peripheral
STAT_NACK_BIT       equ 40h         ; bit 6 = nAck; 0 = peripheral acknowledged
STAT_SELECT_BIT     equ 10h         ; bit 4 = Select; 1 = mode accepted

; Negotiation timeout (rough; ~1ms on AT-class via the inner read loop)
NEG_TIMEOUT_OUTER   equ 100h

; Post-negotiation result tracking
NEG_MODE_NONE       equ 0
NEG_MODE_SPP        equ 1
NEG_MODE_BYTE       equ 2
NEG_MODE_EPP        equ 3
NEG_MODE_ECP        equ 4

; ---------------------------------------------------------------------------
; CAP_RSP payload layout (docs/design.md §10.2 + Stage 1 extension)
;
; Stage 1 reads only a subset of the response; the offsets below are the
; payload offsets (i.e., from RX_BUF + PACKET_HEADER_LEN). The new
; stage2_image_size / stage2_image_crc32 fields are positioned at fixed
; offsets before the variable-length device_string tail so Stage 1 can
; read them by absolute offset without iterating.
; ---------------------------------------------------------------------------
CAP_RSP_VER_MAJ_OFF  equ 0          ; u8  version_major (must be 1)
CAP_RSP_VER_MIN_OFF  equ 1          ; u8  version_minor
CAP_RSP_ACTIVE_OFF   equ 23         ; u8  active_parallel_mode (NEG_MODE_*)
CAP_RSP_STAGE2_SIZE  equ 28         ; u32 BE
CAP_RSP_STAGE2_CRC   equ 32         ; u32 BE
CAP_RSP_MIN_PAYLOAD  equ 36         ; minimum payload length for Stage 1

; ---------------------------------------------------------------------------
; Pump stress test (pre-Stage-2-download channel validation)
;
; Round-trips a fixed PING payload N times. Counts errors (timeout, CRC,
; payload mismatch, wrong cmd) and measures elapsed time. If the channel
; isn't clean here, downloading ~50 KB of Stage 2 over it would just fail
; mid-image — much better to catch it pre-flight and (eventually) downgrade.
; ---------------------------------------------------------------------------
STRESS_ITERATIONS   equ 8
STRESS_PAYLOAD_LEN  equ 64
STRESS_MAX_ERRORS   equ 0           ; require a perfect run to declare pass

; ---------------------------------------------------------------------------
; Memory layout within the COM segment (docs/stage1_design.md §Memory layout)
;
;   0x0000 - 0x00FF   PSP                                     (DOS-managed)
;   0x0100 - 0x07FF   Stage 0 .COM                            (leave intact)
;   0x0800 - 0x27FF   Stage 1 .bin                            (this code)
;   0x2800 - 0xCFFF   Stage 2 download buffer (~42 KB)        (TODO)
;   0xD000 - 0xFFFE   stack + scratch                         (DOS-set)
; ---------------------------------------------------------------------------
STAGE2_DOWNLOAD_OFF equ 2800h
MAX_STAGE2_SIZE     equ 200000          ; 200 KB cap; proposed in design doc

; ---------------------------------------------------------------------------
; Stage 2 download (docs/stage1_design.md §Stage 2 download)
;
; Stream blocks of DOWNLOAD_BLOCK_SIZE bytes from the Pico, write each block
; directly to PICO1284.EXE on disk. A running CRC-32/IEEE (reflected, init
; 0xFFFFFFFF, xor-out 0xFFFFFFFF) is updated over the payload bytes and
; compared against stage2_image_crc32 from CAP_RSP at finish.
;
; Per-block protocol (DOS = host, Pico = device):
;   DOS  -> Pico:  SEND_BLOCK    payload = u32 block_no (BE)
;   Pico -> DOS:   RECV_BLOCK    payload = u32 block_no (BE)
;                                       + u8  byte_count (1..64)
;                                       + byte_count data bytes
;   DOS  -> Pico:  BLOCK_ACK     payload = u32 block_no (BE)
;     (on receive error)
;   DOS  -> Pico:  BLOCK_NAK     payload = u32 block_no (BE)  -- retry
; ---------------------------------------------------------------------------
DOWNLOAD_BLOCK_SIZE equ 64
DL_BLOCK_RETRIES    equ 3
RECV_BLOCK_HDR_LEN  equ 5             ; u32 block_no + u8 byte_count
DL_PROGRESS_MASK    equ 003Fh         ; print a dot every 64 blocks (~4 KB)

; INT 21h DOS file primitives
DOS_CREATE          equ 3Ch
DOS_WRITE           equ 40h
DOS_CLOSE           equ 3Eh
DOS_DELETE          equ 41h

; ---------------------------------------------------------------------------
; Stage 2 EXEC (docs/stage1_design.md §Outbound hand-off)
;
; The PSP environment block must start on a paragraph boundary. We park it
; at offset ENV_BUF_OFF in our own segment (0x2800 is paragraph-aligned and
; sits above the Stage 1 binary). The env-block segment passed to the
; child = CS + (ENV_BUF_OFF >> 4).
;
; EXEC_RESIZE_PARAS shrinks our memory allocation so DOS can hand the child
; the rest. 0x300 paragraphs (= 12 KB) is enough to cover Stage 1's code +
; data and the env block + EXEC parameter block, with comfortable headroom.
; ---------------------------------------------------------------------------
ENV_BUF_OFF         equ 2800h
ENV_BUF_PARAS       equ 280h                    ; ENV_BUF_OFF >> 4
EXEC_RESIZE_PARAS   equ 300h                    ; ~12 KB

DOS_RESIZE          equ 4Ah
DOS_EXEC            equ 4Bh
DOS_GET_EXITCODE    equ 4Dh

; ---------------------------------------------------------------------------
; PIC ports for failure-path IRQ cleanup
; ---------------------------------------------------------------------------
PIC1_DATA           equ 21h
PIC2_DATA           equ 0A1h
IRQ1_MASK           equ 02h             ; bit 1 of PIC1 mask = IRQ1
IRQ12_MASK          equ 10h             ; bit 4 of PIC2 mask = IRQ12

; ---------------------------------------------------------------------------
; Entry
; ---------------------------------------------------------------------------

start:
    ; Snapshot the hand-off registers immediately. Stage 0 hands off with
    ; DS = ES = CS already set; bracket the saves with cli/sti so an ISR
    ; cannot clobber AX/BX/CX/DX before we record them.
    cli
    mov [saved_ax], ax
    mov [saved_bx], bx
    mov [saved_cx], cx
    mov [saved_dx], dx
    sti

    ; Re-establish DS=ES=CS defensively (Stage 0 already did this, but cheap).
    push cs
    pop ds
    push cs
    pop es

    mov si, msg_banner
    call puts

    call validate_handoff
    jc  fail_handoff

    call print_inherited_state

    ; --- Subsystems below: each will replace its TODO comment with a real
    ; --- call when implemented. See docs/stage1_design.md §Subsystems for
    ; --- the order and design intent.

    call lpt_chipset_detect             ; ECR + EPP probe; populates dp_caps_*
    jc  .no_lpt_yet                     ; CF set = Mode B (no LPT)
    call print_caps
    call init_lpt_control               ; SPP idle state + phase capture
    mov byte [current_pump], PUMP_LPT_SPP
    mov si, msg_pump_lpt_spp
    call puts
    jmp .next

.no_lpt_yet:
    mov si, msg_mode_b_todo
    call puts
    mov byte [current_pump], PUMP_NONE  ; explicit; defensive

.next:
    call packet_self_test
    jc  .packet_test_fail
    mov si, msg_packet_ok
    call puts
    jmp .after_packet_test
.packet_test_fail:
    mov si, msg_packet_fail
    call puts

.after_packet_test:
    call pump_dispatcher_self_test
    jc  .pump_test_fail
    mov si, msg_pump_ok
    call puts
    jmp .after_pump_test
.pump_test_fail:
    mov si, msg_pump_fail
    call puts

.after_pump_test:
    call ieee1284_negotiate_ladder      ; ECP -> EPP -> Byte -> SPP fallback

    call cap_handshake                  ; CAP_REQ / RSP / ACK
    ; CF is informational only at scaffold stage; let everything fall through
    ; until the Pico-side firmware lands and a real handshake completes.

    call pump_stress_test               ; N x PING/PONG; count errors + time
    ; CF is informational at this stage. Ladder fallback waits on EPP/ECP
    ; pumps; on SPP we're already at the bottom.

    call download_stage2                ; stream blocks -> PICO1284.EXE + CRC-32
    jc .no_stage2                       ; download failed / skipped; no EXEC

    call build_environment              ; PICO_BOOT=LPT=...;MODE=...;CHAN=...
    call exec_stage2                    ; INT 21h AH=4Bh; AL = child exit
                                        ; AH = error code on failure (CF set)
    jc .exec_failed

    ; AL = child exit code; exit ourselves with same errorlevel.
    mov ah, 4Ch
    int 21h

.no_stage2:
    mov si, msg_no_stage2
    call puts
    mov ax, 4C01h                       ; exit, errorlevel 1
    int 21h

.exec_failed:
    mov si, msg_err_exec
    call puts
    mov ax, 4C01h                       ; exit, errorlevel 1
    int 21h

; ---------------------------------------------------------------------------
; Failure dispatch
;
; All failure paths print a one-line error, restore IRQ masks if Stage 0
; left them masked, and exit with errorlevel 1.
; ---------------------------------------------------------------------------

fail_handoff:
    mov si, msg_err_handoff
    jmp exit_error

fail_lpt:
    mov si, msg_err_lpt
    jmp exit_error

fail_neg:
    mov si, msg_err_neg
    jmp exit_error

fail_caps:
    mov si, msg_err_caps
    jmp exit_error

fail_size:
    mov si, msg_err_size
    jmp exit_error

fail_download:
    mov si, msg_err_download
    jmp exit_error

fail_crc:
    mov si, msg_err_crc
    jmp exit_error

fail_write:
    mov si, msg_err_write
    jmp exit_error

fail_exec:
    mov si, msg_err_exec
    jmp exit_error

exit_error:
    call puts
    call restore_irq_state_if_masked
    mov ax, 4C01h                       ; exit, errorlevel 1
    int 21h

; ---------------------------------------------------------------------------
; Hand-off validation
;
; CF set on bad inbound state. Stage 0 cannot produce a bad hand-off by
; construction, so this is defensive paranoia; cheap.
; ---------------------------------------------------------------------------

validate_handoff:
    cmp word [saved_ax], HANDOFF_MARKER
    jne .bad

    mov ax, [saved_cx]
    or ax, ax
    jz .bad
    cmp ax, MAX_STAGE1_SIZE
    ja .bad

    mov ax, [saved_dx]
    or ax, ax
    jz .bad
    test al, CHAN_ANY
    jz .bad

    clc
    ret

.bad:
    stc
    ret

; ---------------------------------------------------------------------------
; Print inherited state from Stage 0
; ---------------------------------------------------------------------------

print_inherited_state:
    mov si, msg_lpt_label
    call puts
    mov ax, [saved_bx]
    call print_hex16
    mov si, msg_crlf
    call puts

    mov si, msg_chan_label
    call puts
    mov ax, [saved_dx]
    call print_hex16
    mov si, msg_crlf
    call puts

    mov si, msg_size_label
    call puts
    mov ax, [saved_cx]
    call print_hex16
    mov si, msg_crlf
    call puts

    ret

; ---------------------------------------------------------------------------
; LPT chipset detection (docs/stage1_design.md §LPT chipset detection)
;
; If Stage 0 found LPT (saved_bx != 0), probe the ECR register at base+0x402
; with two complementary patterns to confirm it's a real readable register
; (not aliased or stuck). If ECR exists, try an EPP-mode switch to confirm
; EPP-capability. SPP is implied by any LPT response.
;
; Outputs (in dp_caps_*):
;   lpt_base    = saved_bx (or 0)
;   flags       = bit 0 SPP, bit 1 EPP, bit 2 ECP, bit 3 ECP_DMA (DMA TODO)
;   irq         = 0xFF (unknown until BIOS/PnP probe — Stage 2 territory)
;   dma_channel = 0xFF (same)
;
; Returns:
;   CF clear = LPT base present; dp_caps populated.
;   CF set   = no LPT (Mode B); caller should fall back to PS/2.
; ---------------------------------------------------------------------------

lpt_chipset_detect:
    ; Defaults: IRQ + DMA unknown.
    mov byte [dp_caps_irq], 0FFh
    mov byte [dp_caps_dma], 0FFh
    mov byte [dp_caps_flags], 0

    mov ax, [saved_bx]
    mov [lpt_base], ax
    or  ax, ax
    jz  .no_lpt

    ; Stage 0 communicated over LPT, so SPP is by definition supported.
    mov byte [dp_caps_flags], CAPS_SPP

    ; Save current ECR so we can restore on every exit path.
    mov dx, [lpt_base]
    add dx, LPT_ECR
    in  al, dx
    mov [ecr_saved], al

    ; --- Probe ECR existence with two complementary patterns ---
    mov al, ECR_TEST_PATTERN1
    out dx, al
    in  al, dx
    cmp al, ECR_TEST_PATTERN1
    jne .restore_ecr                ; ECR doesn't exist / aliased

    mov al, ECR_TEST_PATTERN2
    out dx, al
    in  al, dx
    cmp al, ECR_TEST_PATTERN2
    jne .restore_ecr                ; suspicious; treat as no ECR

    or  byte [dp_caps_flags], CAPS_ECP

    ; --- ECR exists; try EPP mode switch ---
    mov al, ECR_MODE_EPP
    out dx, al
    in  al, dx
    and al, ECR_MODE_MASK
    cmp al, ECR_MODE_EPP
    jne .restore_ecr

    or  byte [dp_caps_flags], CAPS_EPP

.restore_ecr:
    mov dx, [lpt_base]
    add dx, LPT_ECR
    mov al, [ecr_saved]
    out dx, al

    clc
    ret

.no_lpt:
    ; Mode B (LPT not up). TODO: re-probe LPT bases once shared lpt_nibble.inc
    ; is available; until then, signal "no LPT" and let caller handle.
    xor ax, ax
    mov [lpt_base], ax
    mov byte [dp_caps_flags], 0
    stc
    ret

; ---------------------------------------------------------------------------
; Print detected capabilities
; ---------------------------------------------------------------------------

print_caps:
    mov si, msg_caps_label
    call puts

    xor ah, ah
    mov al, [dp_caps_flags]
    call print_hex8

    mov si, msg_caps_open
    call puts

    test byte [dp_caps_flags], CAPS_SPP
    jz .no_spp
    mov si, msg_caps_spp_str
    call puts
.no_spp:
    test byte [dp_caps_flags], CAPS_EPP
    jz .no_epp
    mov si, msg_caps_epp_str
    call puts
.no_epp:
    test byte [dp_caps_flags], CAPS_ECP
    jz .no_ecp
    mov si, msg_caps_ecp_str
    call puts
.no_ecp:
    mov si, msg_caps_close
    call puts
    ret

; ---------------------------------------------------------------------------
; Failure-path IRQ cleanup
;
; If Stage 0 left IRQ1 / IRQ12 masked (DX has KBD or AUX bit set), unmask
; them before returning to DOS so the user can type to retry. See
; docs/stage1_design.md §Failure handling.
;
; Conservative: unmask IRQ1 if KBD or AUX bit set; unmask IRQ12 only if
; AUX bit set. The PS/2-KBD-up-AUX-failed case leaves PIC2 in Stage 0's
; state -- recoverable on next boot, not damaging.
; ---------------------------------------------------------------------------

restore_irq_state_if_masked:
    mov ax, [saved_dx]
    test al, CHAN_I8042_ANY
    jz .done

    in al, PIC1_DATA
    and al, 0FDh                        ; clear bit 1 (IRQ1)
    out PIC1_DATA, al

    mov ax, [saved_dx]
    test al, CHAN_AUX
    jz .done
    in al, PIC2_DATA
    and al, 0EFh                        ; clear bit 4 (IRQ12)
    out PIC2_DATA, al

.done:
    ret

; ---------------------------------------------------------------------------
; Console helpers
; ---------------------------------------------------------------------------

puts:
    push ax
    push dx
    mov dx, si
    mov ah, 09h
    int 21h
    pop dx
    pop ax
    ret

print_hex16:
    push ax
    mov al, ah
    call print_hex8
    pop ax
    call print_hex8
    ret

print_hex8:
    push ax
    mov ah, al
    shr al, 4
    call print_hex_digit
    mov al, ah
    and al, 0Fh
    call print_hex_digit
    pop ax
    ret

print_hex_digit:
    cmp al, 10
    jb .digit
    add al, 'A' - 10
    jmp .emit
.digit:
    add al, '0'
.emit:
    push dx
    mov dl, al
    mov ah, 02h
    int 21h
    pop dx
    ret

; ---------------------------------------------------------------------------
; CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, no refl, no xor-out)
;
; Inputs:
;   DS:SI = buffer
;   CX    = length
; Output:
;   AX    = CRC
; Clobbers: BX, CX, DX, SI
;
; Ported from dos/stage0/s0_xt.asm crc16_ccitt_buf. Bit-by-bit; ~40 B.
; Stage 2 may replace with a table-driven 386-optimized version later.
; ---------------------------------------------------------------------------

crc16_ccitt:
    mov dx, 0FFFFh

.byte_loop:
    jcxz .done
    lodsb
    xor dh, al
    mov bl, 8

.bit_loop:
    test dx, 8000h
    jz .shift_only
    shl dx, 1
    xor dx, 1021h
    jmp .next_bit

.shift_only:
    shl dx, 1

.next_bit:
    dec bl
    jnz .bit_loop

    dec cx
    jmp .byte_loop

.done:
    mov ax, dx
    ret

; ---------------------------------------------------------------------------
; packet_encode
;
; Build a SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX packet in TX_BUF.
;
; Inputs:
;   AL    = cmd byte
;   DS:SI = payload
;   CX    = payload length (may be 0)
; Output:
;   CX    = total packet length on wire (8 + payload_len)
; Clobbers: AX, BX, DX, DI, SI; advances tx_seq.
; ---------------------------------------------------------------------------

packet_encode:
    push si                         ; we need a fresh SI later for CRC

    mov bx, cx                      ; bx = payload_len (preserved across)

    mov di, TX_BUF_OFF
    mov byte [di], SOH
    inc di
    mov [di], al                    ; CMD
    inc di
    mov al, [tx_seq]
    mov [di], al                    ; SEQ
    inc byte [tx_seq]
    inc di
    mov [di], bh                    ; LEN_HI (BE)
    inc di
    mov [di], bl                    ; LEN_LO
    inc di
                                    ; DI now at TX_BUF + 5 (payload start)

    jcxz .skip_payload
    ; ES:DI = TX_BUF position; DS:SI = caller payload. DS = ES = CS.
    rep movsb
.skip_payload:
                                    ; DI now at TX_BUF + 5 + payload_len

    ; CRC over CMD..end-of-payload = 4 + payload_len bytes at TX_BUF+1
    mov si, TX_BUF_OFF + 1
    mov cx, bx
    add cx, 4
    call crc16_ccitt
                                    ; AX = CRC; DI still at TX_BUF + 5 + payload_len

    mov [di], ah                    ; CRC_HI
    inc di
    mov [di], al                    ; CRC_LO
    inc di
    mov byte [di], ETX

    mov cx, bx
    add cx, PACKET_OVERHEAD          ; total = payload + 8

    pop si
    ret

; ---------------------------------------------------------------------------
; packet_validate
;
; Validate and parse a packet at DS:SI (typically RX_BUF after a channel
; fill).
;
; Inputs:
;   DS:SI = packet
;   CX    = received packet length
; Outputs (on success, CF clear):
;   AL    = cmd
;   AH    = seq
;   DS:SI = payload start
;   CX    = payload length
; On failure (CF set): registers indeterminate.
; Clobbers: AX, BX, CX, DX, SI (caller saves if needed)
; ---------------------------------------------------------------------------

packet_validate:
    cmp cx, PACKET_OVERHEAD
    jb .bad                          ; too short for even an empty-payload packet

    cmp byte [si], SOH
    jne .bad

    ; payload_len from BE bytes at [si+3] (hi) [si+4] (lo)
    mov bh, [si+3]
    mov bl, [si+4]

    ; total = 8 + payload_len; must equal CX
    mov ax, bx
    add ax, PACKET_OVERHEAD
    jc .bad                          ; overflow guard
    cmp ax, cx
    jne .bad

    ; ETX at offset 7 + payload_len = [bx + si + 7]
    cmp byte [bx+si+7], ETX
    jne .bad

    ; CRC over CMD..end-of-payload = 4 + payload_len bytes at SI+1
    push si
    push bx
    inc si
    mov cx, bx
    add cx, 4
    call crc16_ccitt
    pop bx
    pop si
                                     ; AX = computed CRC (AH high, AL low)

    cmp ah, [bx+si+5]                ; received CRC_HI
    jne .bad
    cmp al, [bx+si+6]                ; received CRC_LO
    jne .bad

    ; All checks passed; return outputs
    mov al, [si+1]                   ; cmd
    mov ah, [si+2]                   ; seq
    add si, PACKET_HEADER_LEN        ; payload start
    mov cx, bx                       ; payload_len

    clc
    ret

.bad:
    stc
    ret

; ---------------------------------------------------------------------------
; packet_self_test
;
; Round-trip an encoded packet through TX_BUF -> RX_BUF -> validate, confirm
; cmd and payload survive intact. Used in the v0.3 scaffold path to flag
; CRC / framing bugs before any real Pico traffic.
;
; Returns CF clear on success, CF set on any mismatch.
; ---------------------------------------------------------------------------

packet_self_test:
    ; Build packet: cmd=CMD_PING, payload="HELLO"
    mov al, CMD_PING
    mov si, test_payload_hello
    mov cx, 5
    call packet_encode
                                     ; CX = 13 (total length)

    ; Copy TX_BUF[0..total) to RX_BUF
    push cx
    mov si, TX_BUF_OFF
    mov di, RX_BUF_OFF
    rep movsb                        ; ES = DS = CS already
    pop cx

    ; Validate RX_BUF
    mov si, RX_BUF_OFF
    call packet_validate
    jc .fail

    cmp al, CMD_PING
    jne .fail

    cmp cx, 5
    jne .fail

    ; Compare payload bytes against expected "HELLO"
    mov di, si                       ; rx payload (SI from packet_validate)
    mov si, test_payload_hello
    repe cmpsb
    jne .fail

    clc
    ret

.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; Shared LPT SPP nibble byte pump (constants, routines, lpt_base, last_phase)
; ---------------------------------------------------------------------------

%include "stage0/lpt_nibble.inc"

; ---------------------------------------------------------------------------
; Channel byte-pump dispatcher
;
; tx_drain(DS:SI=buf, CX=len)        -> CF set on failure
; rx_fill(ES:DI=buf,  CX=expected)   -> CF set on timeout/error
;
; Dispatches on [current_pump]. For v0.4 only PUMP_LPT_SPP is implemented;
; other modes return CF set ("unimplemented"). Streaming Stage 2 download
; will use these directly with the download buffer as dest.
; ---------------------------------------------------------------------------

tx_drain:
    cmp byte [current_pump], PUMP_LPT_SPP
    je .lpt_spp
    stc                                 ; pump not yet implemented
    ret

.lpt_spp:
    jcxz .done
.loop:
    lodsb
    call lpt_send_byte
    loop .loop
.done:
    clc
    ret

rx_fill:
    cmp byte [current_pump], PUMP_LPT_SPP
    je .lpt_spp
    stc
    ret

.lpt_spp:
    jcxz .done
.loop:
    call lpt_recv_byte
    jc .timeout
    stosb
    loop .loop
.done:
    clc
    ret
.timeout:
    stc
    ret

; ---------------------------------------------------------------------------
; pump_dispatcher_self_test
;
; Verifies the dispatcher correctly routes to PUMP_LPT_SPP when set and
; correctly rejects (CF set) when PUMP_NONE. Does NOT exercise the actual
; LPT byte pump (which would require a Pico to respond) -- that's
; bench-validation territory.
; ---------------------------------------------------------------------------

pump_dispatcher_self_test:
    ; Save real pump state. Use AX as a 16-bit container around the byte
    ; (8086 has no byte push); AH is don't-care.
    mov al, [current_pump]
    push ax

    ; --- Case 1: PUMP_NONE should reject ---
    mov byte [current_pump], PUMP_NONE
    mov si, TX_BUF_OFF
    xor cx, cx
    call tx_drain
    jnc .fail                           ; should have failed

    mov di, RX_BUF_OFF
    xor cx, cx
    call rx_fill
    jnc .fail

    ; --- Case 2: PUMP_LPT_SPP with len=0 should succeed trivially ---
    mov byte [current_pump], PUMP_LPT_SPP
    mov si, TX_BUF_OFF
    xor cx, cx                          ; zero length = no I/O performed
    call tx_drain
    jc .fail

    mov di, RX_BUF_OFF
    xor cx, cx
    call rx_fill
    jc .fail

    ; Restore real pump state
    pop ax
    mov [current_pump], al
    clc
    ret

.fail:
    pop ax
    mov [current_pump], al
    stc
    ret

; ---------------------------------------------------------------------------
; ieee1284_negotiate
;
; Drive one IEEE 1284 negotiation cycle for the requested extensibility byte.
; Wire-level sequence per drivers/parport/ieee1284.c:
;
;   1. Place xflag on data lines.
;   2. Drive control to NEG_REQ (nSelectIn high, nAutoFd low) -- request.
;   3. Wait for peripheral to assert nAck low (status bit 6 = 0).
;   4. Pulse nStrobe low (NEG_STRB) then high (NEG_REQ).
;   5. Read status: Select (bit 4) high = mode accepted; low = rejected.
;   6. On accept, drive control to NEG_ACK (nAutoFd high) -- commit to mode.
;   7. On reject, restore SPP idle (CTRL_BASE).
;
; Inputs:
;   AL = extensibility byte (XFLAG_*)
; Outputs:
;   CF clear = accepted (control in NEG_ACK state)
;   CF set   = rejected or timed out (control restored to SPP idle)
; Clobbers: AX, CX, DX.
; ---------------------------------------------------------------------------

ieee1284_negotiate:
    ; Event 0: drive extensibility byte on data lines
    push ax                             ; preserve xflag for fault path
    mov dx, [lpt_base]
    add dx, LPT_DATA
    out dx, al
    call tiny_delay

    ; Event 1: negotiation request -- nSelectIn high, nAutoFd low (wire)
    mov dx, [lpt_base]
    add dx, LPT_CONTROL
    mov al, CTRL_NEG_REQ
    out dx, al
    call tiny_delay

    ; Event 4: wait for peripheral to assert nAck low (status bit 6 = 0)
    mov dx, [lpt_base]
    add dx, LPT_STATUS
    mov cx, NEG_TIMEOUT_OUTER
.wait_ack:
    in al, dx
    test al, STAT_NACK_BIT
    jz .ack_received
    loop .wait_ack
    jmp .reject

.ack_received:
    ; Event 6: strobe pulse -- nStrobe low then high
    mov dx, [lpt_base]
    add dx, LPT_CONTROL
    mov al, CTRL_NEG_STRB
    out dx, al
    call tiny_delay
    mov al, CTRL_NEG_REQ
    out dx, al
    call tiny_delay

    ; Read Select bit (bit 4): 1 = accepted, 0 = rejected
    mov dx, [lpt_base]
    add dx, LPT_STATUS
    in al, dx
    test al, STAT_SELECT_BIT
    jz .reject

    ; Event 9: commit to target mode -- nAutoFd high (wire)
    mov dx, [lpt_base]
    add dx, LPT_CONTROL
    mov al, CTRL_NEG_ACK
    out dx, al
    call tiny_delay

    pop ax
    clc
    ret

.reject:
    ; Restore SPP idle so the LPT pump remains usable.
    mov dx, [lpt_base]
    add dx, LPT_CONTROL
    mov al, CTRL_BASE
    out dx, al
    call tiny_delay
    pop ax
    stc
    ret

; ---------------------------------------------------------------------------
; ieee1284_negotiate_ladder
;
; Walk the negotiation ladder ECP -> EPP -> Byte -> SPP fallback, gated by
; what dp_caps reports the chipset can do. Records the achieved mode in
; [negotiated_mode] and prints a one-line result.
;
; current_pump is NOT updated here. The PUMP_LPT_EPP / PUMP_LPT_ECP byte
; pumps are not yet implemented, so packets continue to flow over the SPP
; nibble pump until those land in a later iteration. The negotiated mode
; is preserved for that handoff.
; ---------------------------------------------------------------------------

ieee1284_negotiate_ladder:
    ; If Stage 0 didn't bring up LPT, skip entirely.
    cmp word [lpt_base], 0
    je .no_lpt

    ; Try ECP first (highest bandwidth) if dp_caps reports it
    test byte [dp_caps_flags], CAPS_ECP
    jz .try_epp
    mov al, XFLAG_ECP
    call ieee1284_negotiate
    jc .try_epp
    mov byte [negotiated_mode], NEG_MODE_ECP
    mov si, msg_neg_ecp
    call puts
    ret

.try_epp:
    test byte [dp_caps_flags], CAPS_EPP
    jz .try_byte
    mov al, XFLAG_EPP
    call ieee1284_negotiate
    jc .try_byte
    mov byte [negotiated_mode], NEG_MODE_EPP
    mov si, msg_neg_epp
    call puts
    ret

.try_byte:
    mov al, XFLAG_BYTE
    call ieee1284_negotiate
    jc .fall_spp
    mov byte [negotiated_mode], NEG_MODE_BYTE
    mov si, msg_neg_byte
    call puts
    ret

.fall_spp:
    ; Stage 0 already left the port in SPP idle; nothing to reset.
    mov byte [negotiated_mode], NEG_MODE_SPP
    mov si, msg_neg_spp
    call puts
    ret

.no_lpt:
    mov byte [negotiated_mode], NEG_MODE_NONE
    mov si, msg_neg_skipped
    call puts
    ret

; ---------------------------------------------------------------------------
; print_hex32
;
; DX = high word, AX = low word. Prints 8 hex digits big-endian.
; ---------------------------------------------------------------------------

print_hex32:
    push ax
    mov ax, dx
    call print_hex16
    pop ax
    call print_hex16
    ret

; ---------------------------------------------------------------------------
; Capability handshake (docs/design.md §10, docs/stage1_design.md §Capability
; handshake)
;
; Wire flow:
;   DOS  -> Pico: CAP_REQ  (0-byte payload)
;   Pico -> DOS:  CAP_RSP  (payload per design.md §10.2 + Stage 1 extension)
;   DOS  -> Pico: CAP_ACK  (0-byte payload; "I accept, start Stage 2 download")
;
; Stage 1 reads:
;   version_major / minor       -- must be 1.x
;   active_parallel_mode        -- cross-check with our negotiated_mode
;   stage2_image_size           -- u32, bounded by MAX_STAGE2_SIZE
;   stage2_image_crc32          -- u32, verified after download
;
; CF clear on full success; CF set on any failure (with diagnostic printed).
; ---------------------------------------------------------------------------

cap_handshake:
    cmp byte [negotiated_mode], NEG_MODE_NONE
    je .skip_no_lpt
    cmp byte [negotiated_mode], NEG_MODE_SPP
    je .proceed

    ; Negotiated to a higher mode but those byte pumps aren't implemented
    ; yet. Skip the handshake cleanly; Stage 1 can't drive the wire in the
    ; negotiated mode.
    mov si, msg_cap_skipped_pump
    call puts
    stc
    ret

.skip_no_lpt:
    mov si, msg_cap_skipped_no_lpt
    call puts
    stc
    ret

.proceed:
    call send_cap_req
    jc .fail_req

    call recv_cap_rsp
    jc .fail_rsp

    call parse_cap_rsp_fields
    jc .fail_parse

    call send_cap_ack
    jc .fail_ack

    mov si, msg_cap_ok
    call puts
    call print_cap_summary
    clc
    ret

.fail_req:
    mov si, msg_cap_fail_req
    call puts
    stc
    ret
.fail_rsp:
    mov si, msg_cap_fail_rsp
    call puts
    stc
    ret
.fail_parse:
    mov si, msg_cap_fail_parse
    call puts
    stc
    ret
.fail_ack:
    mov si, msg_cap_fail_ack
    call puts
    stc
    ret

; ---------------------------------------------------------------------------
; send_cap_req
;
; Encode an empty-payload CAP_REQ into TX_BUF and drain via the current
; pump. CF set on tx_drain failure.
; ---------------------------------------------------------------------------

send_cap_req:
    mov al, CMD_CAP_REQ
    xor cx, cx                          ; 0-byte payload
    mov si, TX_BUF_OFF                  ; SI ignored when CX=0 but defensive
    call packet_encode
                                        ; CX = 8 (header + trailer only)
    mov si, TX_BUF_OFF
    call tx_drain
    ret

; ---------------------------------------------------------------------------
; send_cap_ack
;
; Empty-payload CAP_ACK. Tells the Pico Stage 1 is ready for Stage 2 image.
; ---------------------------------------------------------------------------

send_cap_ack:
    mov al, CMD_CAP_ACK
    xor cx, cx
    mov si, TX_BUF_OFF
    call packet_encode

    mov si, TX_BUF_OFF
    call tx_drain
    ret

; ---------------------------------------------------------------------------
; recv_cap_rsp
;
; Two-stage receive: pull the 5-byte header first to learn the payload
; length, then pull the rest. Validate the whole packet and confirm cmd
; is CAP_RSP.
;
; CF clear on success; bytes available at RX_BUF for parse_cap_rsp_fields.
; CF set on rx_fill timeout, length-bound failure, or validation failure.
; ---------------------------------------------------------------------------

recv_cap_rsp:
    ; Step 1: 5-byte header
    mov di, RX_BUF_OFF
    mov cx, PACKET_HEADER_LEN
    call rx_fill
    jc .fail

    ; payload_len from LEN_HI / LEN_LO at offsets 3 / 4 (big-endian)
    mov bh, [RX_BUF_OFF + 3]
    mov bl, [RX_BUF_OFF + 4]
                                        ; BX = payload_len

    ; Sanity: total = 8 + payload_len must fit RX_BUF (256 B)
    mov ax, bx
    add ax, PACKET_OVERHEAD
    jc .fail
    cmp ax, PACKET_BUF_SIZE
    ja .fail

    ; Step 2: payload + CRC + ETX = payload_len + 3 bytes
    mov di, RX_BUF_OFF + PACKET_HEADER_LEN
    mov cx, bx
    add cx, PACKET_TRAILER_LEN
    call rx_fill
    jc .fail

    ; Validate the whole packet
    mov si, RX_BUF_OFF
    mov cx, bx
    add cx, PACKET_OVERHEAD
    call packet_validate
    jc .fail

    ; cmd must be CAP_RSP
    cmp al, CMD_CAP_RSP
    jne .fail

    clc
    ret

.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; parse_cap_rsp_fields
;
; Read fixed-offset fields from the CAP_RSP payload at RX_BUF + 5.
; Stores into:
;   pico_version_major / pico_version_minor
;   pico_active_mode
;   stage2_image_size           (4 bytes, little-endian in memory)
;   stage2_image_crc32          (4 bytes, little-endian in memory)
;
; CF set if:
;   payload_len < CAP_RSP_MIN_PAYLOAD,
;   version_major != 1,
;   stage2_image_size == 0 or > MAX_STAGE2_SIZE.
; ---------------------------------------------------------------------------

parse_cap_rsp_fields:
    ; Re-read payload_len from header to bound-check
    mov bh, [RX_BUF_OFF + 3]
    mov bl, [RX_BUF_OFF + 4]
    cmp bx, CAP_RSP_MIN_PAYLOAD
    jb .fail

    ; version_major / version_minor
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_VER_MAJ_OFF]
    mov [pico_version_major], al
    cmp al, 1
    jne .fail
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_VER_MIN_OFF]
    mov [pico_version_minor], al

    ; active_parallel_mode
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_ACTIVE_OFF]
    mov [pico_active_mode], al

    ; stage2_image_size: 4 wire bytes BE -> 4 memory bytes LE
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_STAGE2_SIZE + 0]
    mov [stage2_image_size + 3], al     ; wire MSB -> memory MSB
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_STAGE2_SIZE + 1]
    mov [stage2_image_size + 2], al
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_STAGE2_SIZE + 2]
    mov [stage2_image_size + 1], al
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_STAGE2_SIZE + 3]
    mov [stage2_image_size + 0], al

    ; stage2_image_crc32: same BE -> LE conversion
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_STAGE2_CRC + 0]
    mov [stage2_image_crc32 + 3], al
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_STAGE2_CRC + 1]
    mov [stage2_image_crc32 + 2], al
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_STAGE2_CRC + 2]
    mov [stage2_image_crc32 + 1], al
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + CAP_RSP_STAGE2_CRC + 3]
    mov [stage2_image_crc32 + 0], al

    ; Sanity-check stage2_image_size: 0 < size <= MAX_STAGE2_SIZE (200000 = 0x30D40)
    mov ax, [stage2_image_size]
    mov dx, [stage2_image_size + 2]
    mov bx, ax
    or  bx, dx
    jz .fail                            ; size == 0

    ; DX:AX <= 0x0003:0x0D40 ?
    cmp dx, 3
    ja  .fail
    jb  .ok
    cmp ax, 0D40h
    ja  .fail
.ok:
    clc
    ret

.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; print_cap_summary
;
; Prints version, Stage 2 size, Stage 2 CRC, active mode.
; ---------------------------------------------------------------------------

print_cap_summary:
    mov si, msg_cap_pico
    call puts
    xor ah, ah
    mov al, [pico_version_major]
    call print_hex8
    mov si, msg_dot
    call puts
    mov al, [pico_version_minor]
    call print_hex8
    mov si, msg_cap_size_label
    call puts
    mov ax, [stage2_image_size]
    mov dx, [stage2_image_size + 2]
    call print_hex32
    mov si, msg_cap_crc_label
    call puts
    mov ax, [stage2_image_crc32]
    mov dx, [stage2_image_crc32 + 2]
    call print_hex32
    mov si, msg_cap_mode_label
    call puts
    mov al, [pico_active_mode]
    call print_hex8
    mov si, msg_crlf
    call puts
    ret

; ---------------------------------------------------------------------------
; read_centisec_clock
;
; Returns AX = (seconds * 100 + centiseconds) within the current minute.
; INT 21h AH=2Ch returns CH=hour CL=min DH=sec DL=centisec. We only need
; seconds + centiseconds for short tests (<60 s); minute-wrap is handled
; in compute_elapsed_centis.
; ---------------------------------------------------------------------------

read_centisec_clock:
    push bx
    push cx
    push dx
    mov ah, 2Ch
    int 21h
    xor ah, ah
    mov al, dh                          ; AL = seconds (DH)
    mov bl, 100
    mul bl                              ; AX = seconds * 100
    xor dh, dh                          ; DX = centiseconds (DL was DL)
    add ax, dx                          ; AX = seconds*100 + centis
    pop dx
    pop cx
    pop bx
    ret

; ---------------------------------------------------------------------------
; compute_elapsed_centis
;
; Inputs:
;   AX = end centis-of-minute
;   BX = start centis-of-minute
; Output:
;   AX = elapsed centiseconds, accounting for one minute wrap.
;
; Tests >60 s would need a real DX:AX clock; out of scope.
; ---------------------------------------------------------------------------

compute_elapsed_centis:
    sub ax, bx
    jnc .done
    add ax, 6000                        ; wrapped through 00:00 minute boundary
.done:
    ret

; ---------------------------------------------------------------------------
; pump_stress_test
;
; Runs STRESS_ITERATIONS round-trips of PING(payload) -> PONG(echo), counts
; errors, measures elapsed time, prints a summary line.
;
; Output:
;   CF clear = error count <= STRESS_MAX_ERRORS (channel viable)
;   CF set   = too many errors / pump not ready (caller decides downgrade)
; ---------------------------------------------------------------------------

pump_stress_test:
    cmp byte [current_pump], PUMP_NONE
    jne .have_pump
    mov si, msg_stress_skipped
    call puts
    stc
    ret

.have_pump:
    ; Reset counters
    xor ax, ax
    mov [stress_errors], ax

    ; Capture start time
    call read_centisec_clock
    mov [stress_start_centis], ax

    ; N iterations
    mov cx, STRESS_ITERATIONS
.iter_loop:
    push cx
    call stress_one_iter
    pop cx
    loop .iter_loop

    ; Capture end time; compute elapsed
    call read_centisec_clock
    mov bx, [stress_start_centis]
    call compute_elapsed_centis
    mov [stress_elapsed_centis], ax

    call print_stress_result

    cmp word [stress_errors], STRESS_MAX_ERRORS
    ja .fail
    clc
    ret
.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; stress_one_iter
;
; One PING/PONG round-trip. Increments [stress_errors] on any failure.
; Always returns (success or failure) so the caller can run the next iter.
; ---------------------------------------------------------------------------

stress_one_iter:
    ; Encode PING with stress_payload
    mov al, CMD_PING
    mov si, stress_payload
    mov cx, STRESS_PAYLOAD_LEN
    call packet_encode
                                        ; CX = total length

    mov si, TX_BUF_OFF
    call tx_drain
    jc .err

    ; Receive 5-byte header
    mov di, RX_BUF_OFF
    mov cx, PACKET_HEADER_LEN
    call rx_fill
    jc .err

    ; Length sanity (must match what we sent)
    mov bh, [RX_BUF_OFF + 3]
    mov bl, [RX_BUF_OFF + 4]
    cmp bx, STRESS_PAYLOAD_LEN
    jne .err

    ; Receive remaining (payload + CRC + ETX)
    mov di, RX_BUF_OFF + PACKET_HEADER_LEN
    mov cx, STRESS_PAYLOAD_LEN + PACKET_TRAILER_LEN
    call rx_fill
    jc .err

    ; Validate full packet
    mov si, RX_BUF_OFF
    mov cx, STRESS_PAYLOAD_LEN + PACKET_OVERHEAD
    call packet_validate
    jc .err

    ; Must be PONG
    cmp al, CMD_PONG
    jne .err

    ; Compare payload byte-by-byte. SI points at received payload after
    ; packet_validate; compare against our stress_payload.
    mov di, si
    mov si, stress_payload
    mov cx, STRESS_PAYLOAD_LEN
    repe cmpsb
    jne .err

    ret

.err:
    inc word [stress_errors]
    ret

; ---------------------------------------------------------------------------
; print_stress_result
;
; "STAGE1: stress: 8 iter, N errors, NNNN centis"
; ---------------------------------------------------------------------------

print_stress_result:
    mov si, msg_stress_prefix
    call puts

    mov ax, STRESS_ITERATIONS
    call print_dec16

    mov si, msg_stress_iter_sep
    call puts

    mov ax, [stress_errors]
    call print_dec16

    mov si, msg_stress_err_sep
    call puts

    mov ax, [stress_elapsed_centis]
    call print_dec16

    mov si, msg_stress_unit
    call puts
    ret

; ---------------------------------------------------------------------------
; print_dec16
;
; AX = unsigned 16-bit value. Prints as decimal, no leading zeros (except 0).
; Compact ~30 B implementation; suitable for stress-test output only.
; ---------------------------------------------------------------------------

print_dec16:
    push ax
    push bx
    push cx
    push dx
    mov bx, 10
    xor cx, cx                          ; CX = digit count
.divloop:
    xor dx, dx
    div bx                              ; AX /= 10; DX = remainder
    push dx
    inc cx
    or  ax, ax
    jnz .divloop
.printloop:
    pop dx
    mov al, dl
    add al, '0'
    mov dl, al
    push dx
    mov ah, 02h
    int 21h
    pop dx
    loop .printloop
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; ---------------------------------------------------------------------------
; CRC-32 / IEEE (zlib / PNG variant): poly 0xEDB88320 reflected,
; init 0xFFFFFFFF, xor-out 0xFFFFFFFF.
;
; State held in DX:AX during the inner loop. Persisted across blocks in
; [crc32_state] (4 bytes LE: AX low word, DX high word).
;
; Bit-by-bit; ~30 B. Fast enough on AT-class for the 200 KB worst case
; given the SPP nibble pump is the dominant cost (~minutes); Stage 2 may
; replace with table-driven if needed.
; ---------------------------------------------------------------------------

crc32_init:
    mov word [crc32_state + 0], 0FFFFh
    mov word [crc32_state + 2], 0FFFFh
    ret

; crc32_update_byte
;   AL = byte to fold in. Clobbers BX, CX, DX, and AX.
crc32_update_byte:
    push si
    mov bl, al
    mov ax, [crc32_state + 0]
    mov dx, [crc32_state + 2]
    xor al, bl                          ; state ^= byte (LSB only)
    mov cx, 8
.bit:
    shr dx, 1
    rcr ax, 1
    jnc .no_xor
    xor ax, 8320h                       ; low half of 0xEDB88320
    xor dx, 0EDB8h                      ; high half
.no_xor:
    loop .bit
    mov [crc32_state + 0], ax
    mov [crc32_state + 2], dx
    pop si
    ret

; crc32_buf
;   DS:SI = buffer, CX = length. Folds bytes into [crc32_state].
;   Clobbers AX, BX, CX, DX, SI.
crc32_buf:
    jcxz .done
.next:
    lodsb
    push cx
    call crc32_update_byte
    pop cx
    loop .next
.done:
    ret

; crc32_finalize
;   Output: DX:AX = final CRC-32 (high:low). Does not modify [crc32_state]
;   beyond the xor-out so the caller can inspect both.
crc32_finalize:
    mov ax, [crc32_state + 0]
    mov dx, [crc32_state + 2]
    xor ax, 0FFFFh
    xor dx, 0FFFFh
    ret

; ---------------------------------------------------------------------------
; download_stage2
;
; Top-level Stage 2 download driver.
;
; Preconditions:
;   - cap_handshake succeeded (stage2_image_size and stage2_image_crc32
;     populated from CAP_RSP).
;   - current_pump is a usable byte pump (PUMP_LPT_SPP today).
;
; Flow:
;   1. Skip cleanly if no pump, no size, or size > MAX_STAGE2_SIZE.
;   2. INT 21h AH=3Ch  -> create/truncate PICO1284.EXE; store handle.
;   3. CRC-32 init; current_block = 0; total_blocks = ceil(size / 64).
;   4. For each block:
;        request_one_block (SEND_BLOCK + retries)
;        commit_one_block (CRC update + INT 21h AH=40h write + BLOCK_ACK)
;   5. INT 21h AH=3Eh  -> close.
;   6. Finalize CRC-32; compare against stage2_image_crc32.
;   7. On any failure: close (if open) + delete PICO1284.EXE; print error.
;
; CF clear on full success, CF set otherwise.
; ---------------------------------------------------------------------------

download_stage2:
    ; --- Preflight checks ---
    cmp byte [current_pump], PUMP_NONE
    jne .have_pump
    mov si, msg_dl_skipped_pump
    call puts
    stc
    ret

.have_pump:
    ; Skip if cap_handshake never populated a size (still 0 from .bss).
    mov ax, [stage2_image_size + 0]
    or  ax, [stage2_image_size + 2]
    jnz .have_size
    mov si, msg_dl_skipped_nosize
    call puts
    stc
    ret

.have_size:
    ; total_blocks = ceil(size / 64) = (size + 63) >> 6, computed in DX:AX.
    mov ax, [stage2_image_size + 0]
    mov dx, [stage2_image_size + 2]
    add ax, DOWNLOAD_BLOCK_SIZE - 1
    adc dx, 0
    ; 32-bit shift right by 6
    mov cx, 6
.shr_loop:
    shr dx, 1
    rcr ax, 1
    loop .shr_loop
    ; total_blocks must fit a u16 (200000 / 64 ~ 3125 -- comfortable)
    or  dx, dx
    jnz .fail_overflow
    mov [total_blocks], ax

    mov si, msg_dl_start
    call puts

    ; Create / truncate PICO1284.EXE
    mov ah, DOS_CREATE
    xor cx, cx                          ; normal attributes
    mov dx, stage2_filename
    int 21h
    jc .fail_create
    mov [stage2_file_handle], ax

    ; Init running CRC-32 and block counter
    call crc32_init
    mov word [current_block], 0
    mov word [blocks_received], 0

.block_loop:
    mov ax, [current_block]
    cmp ax, [total_blocks]
    jae .blocks_done

    ; Compute byte_count for this block: last block may be short.
    ; expected = min(64, size - block*64). Use DX:AX = size - block*64,
    ; but since block*64 <= size (loop invariant) the high word is 0 once
    ; we subtract.
    mov ax, [current_block]
    mov cx, 6
    xor dx, dx
.shl_loop:
    shl ax, 1
    rcl dx, 1
    loop .shl_loop
    ; DX:AX = current_block * 64 (block_offset).
    ; remaining = size - block_offset = DX:AX of size - DX:AX of offset.
    mov bx, [stage2_image_size + 0]
    mov cx, [stage2_image_size + 2]
    sub bx, ax
    sbb cx, dx
    ; remaining in CX:BX. If CX > 0, more than 64K left, clamp to 64.
    or  cx, cx
    jnz .full_block
    cmp bx, DOWNLOAD_BLOCK_SIZE
    jbe .partial_block
.full_block:
    mov bx, DOWNLOAD_BLOCK_SIZE
.partial_block:
    mov [expected_byte_count], bl

    ; Try DL_BLOCK_RETRIES times
    mov byte [retry_count], DL_BLOCK_RETRIES
.retry:
    call request_one_block
    jnc .got_block
    dec byte [retry_count]
    jz .fail_download
    ; Send BLOCK_NAK so Pico knows to re-send (best-effort; ignore tx error).
    call send_block_nak
    jmp .retry

.got_block:
    call commit_one_block
    jc .fail_download

    inc word [current_block]
    inc word [blocks_received]

    ; Progress indicator: print a dot every DL_PROGRESS_MASK+1 blocks.
    mov ax, [current_block]
    and ax, DL_PROGRESS_MASK
    jnz .block_loop
    mov si, msg_dl_dot
    call puts
    jmp .block_loop

.blocks_done:
    ; Close file
    mov ah, DOS_CLOSE
    mov bx, [stage2_file_handle]
    int 21h
    mov word [stage2_file_handle], 0
    jc .fail_close

    ; Verify CRC-32
    call crc32_finalize                 ; DX:AX = computed
    cmp ax, [stage2_image_crc32 + 0]
    jne .fail_crc
    cmp dx, [stage2_image_crc32 + 2]
    jne .fail_crc

    mov si, msg_dl_ok
    call puts
    clc
    ret

.fail_overflow:
    mov si, msg_dl_fail_size
    call puts
    stc
    ret

.fail_create:
    mov si, msg_dl_fail_create
    call puts
    stc
    ret

.fail_download:
    mov si, msg_dl_fail_block
    call puts
    call abort_download
    stc
    ret

.fail_close:
    mov si, msg_dl_fail_close
    call puts
    call abort_download
    stc
    ret

.fail_crc:
    mov si, msg_dl_fail_crc
    call puts
    ; File is already closed; just delete it.
    mov ah, DOS_DELETE
    mov dx, stage2_filename
    int 21h
    stc
    ret

; ---------------------------------------------------------------------------
; abort_download
;
; Close PICO1284.EXE if still open, then delete it. Best-effort: errors
; from INT 21h are ignored -- we're already on the failure path.
; ---------------------------------------------------------------------------

abort_download:
    push ax
    push bx
    push dx
    cmp word [stage2_file_handle], 0
    je .skip_close
    mov ah, DOS_CLOSE
    mov bx, [stage2_file_handle]
    int 21h
    mov word [stage2_file_handle], 0
.skip_close:
    mov ah, DOS_DELETE
    mov dx, stage2_filename
    int 21h
    pop dx
    pop bx
    pop ax
    ret

; ---------------------------------------------------------------------------
; request_one_block
;
; Send SEND_BLOCK with current_block as the u32 BE payload, then receive the
; RECV_BLOCK packet into RX_BUF. On success, payload starts at RX_BUF + 5
; and contains: u32 block_no BE | u8 byte_count | data[byte_count].
;
; Validates:
;   - rx_fill timeouts
;   - packet_validate (CRC, framing)
;   - cmd == CMD_RECV_BLOCK
;   - echoed block_no == current_block
;   - byte_count == expected_byte_count
;
; CF clear on success; CF set on any of the above.
; Clobbers: AX, BX, CX, DX, SI, DI.
; ---------------------------------------------------------------------------

request_one_block:
    ; Pack u32 block_no BE into send_block_payload (current_block fits a
    ; u16, so high two bytes are 0).
    mov byte [send_block_payload + 0], 0
    mov byte [send_block_payload + 1], 0
    mov ax, [current_block]
    mov [send_block_payload + 2], ah    ; BE: hi-byte first within low word
    mov [send_block_payload + 3], al

    mov al, CMD_SEND_BLOCK
    mov si, send_block_payload
    mov cx, 4
    call packet_encode
                                        ; CX = total length

    mov si, TX_BUF_OFF
    call tx_drain
    jc .fail

    ; Receive 5-byte header
    mov di, RX_BUF_OFF
    mov cx, PACKET_HEADER_LEN
    call rx_fill
    jc .fail

    ; payload_len from header (BE)
    mov bh, [RX_BUF_OFF + 3]
    mov bl, [RX_BUF_OFF + 4]

    ; Must be RECV_BLOCK_HDR_LEN (5) + expected_byte_count
    mov al, [expected_byte_count]
    xor ah, ah
    add ax, RECV_BLOCK_HDR_LEN
    cmp bx, ax
    jne .fail

    ; Sanity: total = 8 + payload_len must fit RX_BUF
    mov ax, bx
    add ax, PACKET_OVERHEAD
    jc .fail
    cmp ax, PACKET_BUF_SIZE
    ja .fail

    ; Receive remaining (payload + CRC + ETX)
    mov di, RX_BUF_OFF + PACKET_HEADER_LEN
    mov cx, bx
    add cx, PACKET_TRAILER_LEN
    call rx_fill
    jc .fail

    ; Validate whole packet
    mov si, RX_BUF_OFF
    mov cx, bx
    add cx, PACKET_OVERHEAD
    call packet_validate
    jc .fail

    ; cmd must be RECV_BLOCK
    cmp al, CMD_RECV_BLOCK
    jne .fail

    ; Echoed block_no must match current_block (u16 in low 16 bits BE)
    cmp byte [RX_BUF_OFF + PACKET_HEADER_LEN + 0], 0
    jne .fail
    cmp byte [RX_BUF_OFF + PACKET_HEADER_LEN + 1], 0
    jne .fail
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + 2]
    cmp al, byte [current_block + 1]
    jne .fail
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + 3]
    cmp al, byte [current_block + 0]
    jne .fail

    ; byte_count must match expected_byte_count
    mov al, [RX_BUF_OFF + PACKET_HEADER_LEN + 4]
    cmp al, [expected_byte_count]
    jne .fail

    clc
    ret
.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; commit_one_block
;
; The RECV_BLOCK packet has been validated. Data starts at
; RX_BUF + PACKET_HEADER_LEN + RECV_BLOCK_HDR_LEN, length = expected_byte_count.
;
; Steps:
;   1. Fold the data bytes into [crc32_state].
;   2. INT 21h AH=40h write to stage2_file_handle.
;   3. Send BLOCK_ACK with current_block payload.
;
; CF set on write short / write error / BLOCK_ACK tx error.
; ---------------------------------------------------------------------------

commit_one_block:
    ; CRC-32 over the data bytes
    mov si, RX_BUF_OFF + PACKET_HEADER_LEN + RECV_BLOCK_HDR_LEN
    mov cl, [expected_byte_count]
    xor ch, ch
    call crc32_buf

    ; Write to file
    mov ah, DOS_WRITE
    mov bx, [stage2_file_handle]
    mov cl, [expected_byte_count]
    xor ch, ch
    mov dx, RX_BUF_OFF + PACKET_HEADER_LEN + RECV_BLOCK_HDR_LEN
    int 21h
    jc .fail
    ; AX = bytes written; must equal byte_count
    mov bl, [expected_byte_count]
    xor bh, bh
    cmp ax, bx
    jne .fail

    ; Send BLOCK_ACK (same u32 BE payload as SEND_BLOCK)
    mov al, CMD_BLOCK_ACK
    mov si, send_block_payload
    mov cx, 4
    call packet_encode
    mov si, TX_BUF_OFF
    call tx_drain
    ret
.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; send_block_nak
;
; Send BLOCK_NAK with current_block as u32 BE payload. Best-effort; the
; retry loop calls request_one_block again regardless of tx outcome.
; ---------------------------------------------------------------------------

send_block_nak:
    ; send_block_payload still holds current_block's u32 BE encoding from
    ; the last request_one_block; it's safe to reuse.
    mov al, CMD_BLOCK_NAK
    mov si, send_block_payload
    mov cx, 4
    call packet_encode
    mov si, TX_BUF_OFF
    call tx_drain
    ret

; ---------------------------------------------------------------------------
; build_environment
;
; Build the PSP environment block the child PICO1284.EXE will inherit.
; Layout per DOS 3.0+ convention:
;
;   "PICO_BOOT=LPT=XXXX MODE=YYY CHAN=N VER=X.Y\0"
;   "\0"                          ; double-null terminates string list
;   WORD = 1                     ; count of strings that follow
;   "PICO1284.EXE\0"             ; child's program path
;
; Built at CS:ENV_BUF_OFF (paragraph-aligned). The env-segment value passed
; to the child = CS + ENV_BUF_PARAS.
; ---------------------------------------------------------------------------

build_environment:
    push es
    push cs
    pop es                              ; ES = CS so STOSB writes our segment
    cld

    mov di, ENV_BUF_OFF

    mov si, env_prefix_str              ; "PICO_BOOT=LPT="
    call emit_str

    mov ax, [saved_bx]                  ; lpt_base
    call emit_hex16

    mov si, env_mode_str                ; " MODE="
    call emit_str
    mov al, [negotiated_mode]
    call emit_mode_name

    mov si, env_chan_str                ; " CHAN="
    call emit_str
    mov ax, [saved_dx]
    call emit_dec16

    mov si, env_ver_str                 ; " VER="
    call emit_str
    xor ah, ah
    mov al, [pico_version_major]
    call emit_dec16
    mov al, '.'
    stosb
    xor ah, ah
    mov al, [pico_version_minor]
    call emit_dec16

    xor al, al
    stosb                               ; NUL terminator for PICO_BOOT= var
    stosb                               ; second NUL terminates the env list

    ; DOS 3.0+ extension: word count + ASCIIZ program path
    mov ax, 1
    stosw

    mov si, stage2_filename             ; "PICO1284.EXE",0
    call emit_str
    xor al, al
    stosb                               ; the trailing NUL of the path

    pop es
    ret

; ---------------------------------------------------------------------------
; emit_str
;   DS:SI = source ASCIIZ. Copies bytes (excluding the terminator) to ES:DI.
;   Returns DI advanced. Preserves SI past the terminator.
; ---------------------------------------------------------------------------
emit_str:
.loop:
    lodsb
    or al, al
    jz .done
    stosb
    jmp .loop
.done:
    ret

; emit_hex16  AX -> ES:DI as 4 hex digits, uppercase, no prefix.
emit_hex16:
    push ax
    mov al, ah
    call emit_hex8
    pop ax
    call emit_hex8
    ret

emit_hex8:
    push ax
    mov ah, al
    shr al, 4
    call emit_hex_digit
    mov al, ah
    and al, 0Fh
    call emit_hex_digit
    pop ax
    ret

emit_hex_digit:
    and al, 0Fh
    cmp al, 10
    jb .digit
    add al, 'A' - 10
    jmp .out
.digit:
    add al, '0'
.out:
    stosb
    ret

; emit_dec16  AX -> ES:DI as decimal, no leading zeros (except for 0).
emit_dec16:
    push ax
    push bx
    push cx
    push dx
    mov bx, 10
    xor cx, cx
.divloop:
    xor dx, dx
    div bx
    push dx
    inc cx
    or  ax, ax
    jnz .divloop
.emitloop:
    pop ax
    add al, '0'
    stosb
    loop .emitloop
    pop dx
    pop cx
    pop bx
    pop ax
    ret

; emit_mode_name  AL = NEG_MODE_* -> ES:DI as ECP/EPP/BYTE/SPP/NEG_FAILED
emit_mode_name:
    cmp al, NEG_MODE_ECP
    je .ecp
    cmp al, NEG_MODE_EPP
    je .epp
    cmp al, NEG_MODE_BYTE
    je .byte
    cmp al, NEG_MODE_SPP
    je .spp
    mov si, str_neg_failed
    jmp emit_str
.ecp:
    mov si, str_mode_ecp
    jmp emit_str
.epp:
    mov si, str_mode_epp
    jmp emit_str
.byte:
    mov si, str_mode_byte
    jmp emit_str
.spp:
    mov si, str_mode_spp
    jmp emit_str

; ---------------------------------------------------------------------------
; exec_stage2
;
; 1. Shrink our memory allocation via INT 21h AH=4Ah so DOS can give the
;    child the rest of conventional memory.
; 2. Patch the EXEC parameter block with CS-relative segments.
; 3. INT 21h AH=4Bh AL=00 (load and execute).
;
; On success: AL = child errorlevel (queried via AH=4Dh).
; On failure: CF set; AX = DOS error code (from INT 21h AH=4Bh / 4Ah).
; ---------------------------------------------------------------------------

exec_stage2:
    ; --- Step 1: shrink memory ---
    push es
    push cs
    pop es                              ; ES = our PSP/segment for AH=4Ah
    mov ah, DOS_RESIZE
    mov bx, EXEC_RESIZE_PARAS
    int 21h
    pop es
    jc .fail

    ; --- Step 2: patch parameter block with CS-relative segments ---
    mov ax, cs
    add ax, ENV_BUF_PARAS               ; env block segment
    mov [exec_param_env_seg], ax

    mov ax, cs                          ; cmdtail / FCB1 / FCB2 segments
    mov [exec_param_cmdtail + 2], ax
    mov [exec_param_fcb1 + 2], ax
    mov [exec_param_fcb2 + 2], ax

    ; --- Step 3: INT 21h AH=4Bh ---
    push ds
    push es

    push cs
    pop es                              ; ES:BX = param block
    mov bx, exec_param_block

    push cs
    pop ds                              ; DS:DX = ASCIIZ filename
    mov dx, stage2_filename

    mov ax, 4B00h
    int 21h

    pop es
    pop ds
    jc .fail

    ; --- Step 4: query child's exit code ---
    mov ah, DOS_GET_EXITCODE
    int 21h
                                        ; AL = exit code, AH = termination type
    clc
    ret

.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; Data
; ---------------------------------------------------------------------------

saved_ax:           dw 0
saved_bx:           dw 0
saved_cx:           dw 0
saved_dx:           dw 0

ecr_saved:          db 0

; stage1_dp_caps struct (docs/stage1_design.md §LPT chipset detection).
; `lpt_base` itself lives in stage0/lpt_nibble.inc; the remaining caps
; fields stay here. Logical struct; not contiguous.
dp_caps_flags:      db 0            ; bit 0 SPP, 1 EPP, 2 ECP, 3 ECP_DMA
dp_caps_irq:        db 0FFh
dp_caps_dma:        db 0FFh

; Packet framing
tx_seq:             db 0            ; auto-incremented by packet_encode
test_payload_hello: db 'HELLO'

; LPT pump state (`last_phase` lives in stage0/lpt_nibble.inc)
current_pump:       db 0            ; PUMP_NONE until lpt_pump_init sets it

; IEEE 1284 negotiation result
negotiated_mode:    db 0            ; NEG_MODE_* enum after negotiate_ladder

; CAP_RSP parse outputs
pico_version_major: db 0
pico_version_minor: db 0
pico_active_mode:   db 0
stage2_image_size:  dd 0            ; u32 LE in memory
stage2_image_crc32: dd 0            ; u32 LE in memory

; Stress-test state
stress_errors:          dw 0
stress_start_centis:    dw 0
stress_elapsed_centis:  dw 0

; Stage 2 download state
stage2_file_handle:     dw 0
current_block:          dw 0
total_blocks:           dw 0
blocks_received:        dw 0
expected_byte_count:    db 0
retry_count:            db 0
crc32_state:            dd 0
send_block_payload:     db 0, 0, 0, 0   ; u32 block_no BE; rebuilt per block
stage2_filename:        db 'PICO1284.EXE',0

; EXEC parameter block (INT 21h AH=4Bh).
; Layout:
;   +0  WORD  env-block segment (0 = inherit parent's)
;   +2  DWORD pointer (offset:segment) to command tail (LEN byte, string, CR)
;   +6  DWORD pointer to FCB1
;   +10 DWORD pointer to FCB2
; All segments are patched to CS at runtime; the offsets are baked in.
exec_param_block:
exec_param_env_seg:     dw 0                    ; patched: CS + ENV_BUF_PARAS
exec_param_cmdtail:     dw exec_cmd_tail, 0     ; offset, segment (patched)
exec_param_fcb1:        dw exec_blank_fcb, 0    ; offset, segment (patched)
exec_param_fcb2:        dw exec_blank_fcb, 0    ; offset, segment (patched)

exec_cmd_tail:          db 0, 13                ; LEN=0, CR
exec_blank_fcb:         times 16 db 0

; Strings emitted into the env block by build_environment
env_prefix_str:         db 'PICO_BOOT=LPT=',0
env_mode_str:           db ' MODE=',0
env_chan_str:           db ' CHAN=',0
env_ver_str:            db ' VER=',0

str_mode_ecp:           db 'ECP',0
str_mode_epp:           db 'EPP',0
str_mode_byte:          db 'BYTE',0
str_mode_spp:           db 'SPP',0
str_neg_failed:         db 'NEG_FAILED',0

; Stress-test payload (incrementing 0..63; covers a range of bit patterns,
; including all-zero leading byte, walking-bit-style values, ASCII range)
stress_payload:
    db 000h, 001h, 002h, 003h, 004h, 005h, 006h, 007h
    db 008h, 009h, 00Ah, 00Bh, 00Ch, 00Dh, 00Eh, 00Fh
    db 010h, 011h, 012h, 013h, 014h, 015h, 016h, 017h
    db 018h, 019h, 01Ah, 01Bh, 01Ch, 01Dh, 01Eh, 01Fh
    db 020h, 021h, 022h, 023h, 024h, 025h, 026h, 027h
    db 028h, 029h, 02Ah, 02Bh, 02Ch, 02Dh, 02Eh, 02Fh
    db 030h, 031h, 032h, 033h, 034h, 035h, 036h, 037h
    db 038h, 039h, 03Ah, 03Bh, 03Ch, 03Dh, 03Eh, 03Fh

msg_banner:         db 'STAGE1 v1.0 scaffold',13,10,'$'
msg_scaffold_exit:  db 'STAGE1: subsystems not yet implemented; exiting clean',13,10,'$'
msg_no_stage2:      db 'STAGE1: Stage 2 unavailable; aborting',13,10,'$'
msg_lpt_label:      db 'STAGE1: inherited LPT base   = 0x','$'
msg_chan_label:     db 'STAGE1: inherited channels   = 0x','$'
msg_size_label:     db 'STAGE1: stage1 size          = 0x','$'
msg_caps_label:     db 'STAGE1: dp_caps.flags        = 0x','$'
msg_caps_open:      db ' [','$'
msg_caps_close:     db ']',13,10,'$'
msg_caps_spp_str:   db 'SPP ','$'
msg_caps_epp_str:   db 'EPP ','$'
msg_caps_ecp_str:   db 'ECP ','$'
msg_mode_b_todo:    db 'STAGE1: no LPT available; Mode B re-probe TODO',13,10,'$'
msg_packet_ok:      db 'STAGE1: packet self-test OK',13,10,'$'
msg_packet_fail:    db 'STAGE1: packet self-test FAILED',13,10,'$'
msg_pump_lpt_spp:   db 'STAGE1: pump = LPT SPP nibble',13,10,'$'
msg_pump_ok:        db 'STAGE1: pump dispatcher self-test OK',13,10,'$'
msg_pump_fail:      db 'STAGE1: pump dispatcher self-test FAILED',13,10,'$'
msg_neg_ecp:        db 'STAGE1: IEEE 1284 negotiated = ECP',13,10,'$'
msg_neg_epp:        db 'STAGE1: IEEE 1284 negotiated = EPP',13,10,'$'
msg_neg_byte:       db 'STAGE1: IEEE 1284 negotiated = Byte',13,10,'$'
msg_neg_spp:        db 'STAGE1: IEEE 1284 negotiation declined; staying in SPP/Nibble',13,10,'$'
msg_neg_skipped:    db 'STAGE1: IEEE 1284 negotiation skipped (no LPT)',13,10,'$'

msg_cap_skipped_no_lpt: db 'STAGE1: CAP handshake skipped (no LPT)',13,10,'$'
msg_cap_skipped_pump:   db 'STAGE1: CAP handshake skipped (EPP/ECP pump not ready)',13,10,'$'
msg_cap_fail_req:       db 'STAGE1: CAP handshake failed (REQ tx)',13,10,'$'
msg_cap_fail_rsp:       db 'STAGE1: CAP handshake failed (RSP rx)',13,10,'$'
msg_cap_fail_parse:     db 'STAGE1: CAP handshake failed (version/size sanity)',13,10,'$'
msg_cap_fail_ack:       db 'STAGE1: CAP handshake failed (ACK tx)',13,10,'$'
msg_cap_ok:             db 'STAGE1: CAP handshake OK',13,10,'$'
msg_cap_pico:           db 'STAGE1: Pico=','$'
msg_dot:                db '.','$'
msg_cap_size_label:     db ' size=0x','$'
msg_cap_crc_label:      db ' crc=0x','$'
msg_cap_mode_label:     db ' mode=0x','$'

msg_stress_skipped:     db 'STAGE1: stress test skipped (no pump)',13,10,'$'
msg_stress_prefix:      db 'STAGE1: stress: ','$'
msg_stress_iter_sep:    db ' iter, ','$'
msg_stress_err_sep:     db ' errors, ','$'
msg_stress_unit:        db ' centis',13,10,'$'

msg_dl_skipped_pump:    db 'STAGE1: stage2 download skipped (no pump)',13,10,'$'
msg_dl_skipped_nosize:  db 'STAGE1: stage2 download skipped (no CAP size)',13,10,'$'
msg_dl_start:           db 'STAGE1: stage2 download: ','$'
msg_dl_dot:             db '.','$'
msg_dl_ok:              db ' OK',13,10,'$'
msg_dl_fail_size:       db ' FAIL (size too large)',13,10,'$'
msg_dl_fail_create:     db ' FAIL (cannot create PICO1284.EXE)',13,10,'$'
msg_dl_fail_block:      db ' FAIL (block retries exhausted)',13,10,'$'
msg_dl_fail_close:      db ' FAIL (file close error)',13,10,'$'
msg_dl_fail_crc:        db ' FAIL (image CRC-32 mismatch)',13,10,'$'

msg_crlf:           db 13,10,'$'

msg_err_handoff:    db 'ERROR: bad handoff from Stage 0',13,10,'$'
msg_err_lpt:        db 'ERROR: LPT chipset detection failed',13,10,'$'
msg_err_neg:        db 'ERROR: IEEE 1284 negotiation failed',13,10,'$'
msg_err_caps:       db 'ERROR: capability handshake failed',13,10,'$'
msg_err_size:       db 'ERROR: Stage 2 size invalid',13,10,'$'
msg_err_download:   db 'ERROR: Stage 2 download failed',13,10,'$'
msg_err_crc:        db 'ERROR: Stage 2 image CRC mismatch',13,10,'$'
msg_err_write:      db 'ERROR: cannot write PICO1284.EXE',13,10,'$'
msg_err_exec:       db 'ERROR: Stage 2 EXEC failed',13,10,'$'
