; s0_xt.asm
; ---------------------------------------------------------------------------
; S0_XT.COM - XT-class Stage0 bootstrap for Pico1284
;
; Target:
;   - 8088/8086-compatible real-mode DOS .COM
;   - NASM syntax
;   - Does NOT rely on AT/PS2 keyboard bidirectional commands
;   - Uses keyboard only for Stage-1 injection via DEBUG
;   - Establishes first real bidirectional channel over LPT/SPP DB-25
;
; Build:
;   nasm -f bin s0_xt.asm -o S0_XT.COM
;
; High-level behavior:
;   1. Probe common LPT base addresses: 03BCh, 0378h, 0278h
;   2. Find Pico1284 endpoint with a tiny SPP nibble protocol
;   3. Request Stage1 metadata
;   4. Download Stage1 in 64-byte blocks
;   5. Verify CRC-16/CCITT-FALSE per block
;   6. Verify whole-image 16-bit additive checksum
;   7. Jump to Stage1 at CS:0800h
;
; Protocol assumptions:
;   DOS -> Pico:
;       data register carries command/data byte
;       INIT control line is toggled as host strobe
;
;   Pico -> DOS:
;       status bits 3..6 carry a nibble:
;           status bit 3 = nibble bit 0
;           status bit 4 = nibble bit 1
;           status bit 5 = nibble bit 2
;           status bit 6 = nibble bit 3
;
;       Pico toggles status bit 7 as an acknowledge phase bit after presenting
;       each output nibble. Stage0 treats the register bit as the logical phase;
;       Pico firmware must be written to match the PC-side register behavior.
;
; This protocol is intentionally conservative and slow. Stage1 will perform the
; real hardware inventory and negotiate faster IEEE 1284 modes.
;
; Revision notes:
;   - DELAY_COUNT is tunable and defaults to a conservative 200 loops.
;   - Nibble receive uses a second status read after a short delay to reduce
;     false phase changes on noisy or slow hardware.
; ---------------------------------------------------------------------------

bits 16
org 100h

; ---------------------------------------------------------------------------
; Configuration
; ---------------------------------------------------------------------------

STAGE1_LOAD_OFF   equ 0800h
MAX_STAGE1_SIZE   equ 50000
BLOCK_SIZE        equ 64

; Stage0 wire commands
CMD_PROBE0        equ 050h        ; 'P'
CMD_PROBE1        equ 031h        ; '1'
CMD_PROBE2        equ 032h        ; '2'
CMD_PROBE3        equ 038h        ; '8'

CMD_GET_META      equ 081h
CMD_GET_BLOCK     equ 082h
CMD_ACK           equ 006h
CMD_NAK           equ 015h

; Expected Pico response
RSP_OK0           equ 04Fh        ; 'O'
RSP_OK1           equ 04Bh        ; 'K'

; Retry/timeout tuning
PROBE_RETRIES     equ 3
BLOCK_RETRIES     equ 5

; Delay tuning.
; DELAY_COUNT=200 is intentionally conservative for 4.77 MHz XT-class
; machines and slow/marginal parallel-port glue logic. Stage1 can negotiate
; faster timing later. Consumed by tiny_delay in lpt_nibble.inc.
DELAY_COUNT       equ 200

; LPT register layout, control/status bits, nibble timeouts, and the
; init/send/recv/delay routines now live in the shared include. The
; include is brought in at the end of code, after start: (see below),
; so the entry point remains at offset 0x100.

; ---------------------------------------------------------------------------
; COM entry
; ---------------------------------------------------------------------------

start:
    cli
    cld
    push cs
    pop ds
    push cs
    pop es
    sti

    mov si, msg_banner
    call puts

    call find_pico_lpt
    jc  fail_no_pico

    mov si, msg_found
    call puts
    call print_base

    call get_stage1_meta
    jc  fail_meta

    call check_stage1_size
    jc  fail_size

    call download_stage1
    jc  fail_download

    call verify_image_checksum
    jc  fail_checksum

    mov si, msg_jump
    call puts

    ; Handoff to Stage1.
    ; Registers on entry:
    ;   AX = 'P1' marker
    ;   BX = selected LPT base
    ;   CX = Stage1 size
    ;   DX = channel-availability bitmap reflecting channels actually up:
    ;        bit 0 (0x01) = LPT channel up
    ;        bit 1 (0x02) = i8042 KBD private channel up (never on XT)
    ;        bit 2 (0x04) = i8042 AUX private channel up (never on XT)
    ;   On XT only bit 0 is achievable. If LPT probe fails we exit to DOS
    ;   before reaching this hand-off, so DX=0001h is always correct here.
    ;   DS = ES = CS
    mov ax, 3150h
    mov bx, [lpt_base]
    mov cx, [stage1_size]
    mov dx, 0001h
    push cs
    pop ds
    push cs
    pop es
    jmp STAGE1_LOAD_OFF

fail_no_pico:
    mov si, msg_no_pico
    call puts
    jmp exit_error

fail_meta:
    mov si, msg_meta_fail
    call puts
    jmp exit_error

fail_size:
    mov si, msg_size_fail
    call puts
    jmp exit_error

fail_download:
    mov si, msg_download_fail
    call puts
    jmp exit_error

fail_checksum:
    mov si, msg_checksum_fail
    call puts
    jmp exit_error

exit_error:
    mov ax, 4C01h
    int 21h

; ---------------------------------------------------------------------------
; Find Pico on common LPT ports
; ---------------------------------------------------------------------------

find_pico_lpt:
    mov si, lpt_candidates

.next_port:
    lodsw
    or ax, ax
    jz .not_found

    mov [lpt_base], ax
    call init_lpt_control

    mov cx, PROBE_RETRIES
.try_probe:
    push cx
    call probe_pico
    pop cx
    jnc .found
    loop .try_probe

    jmp .next_port

.found:
    clc
    ret

.not_found:
    stc
    ret

; init_lpt_control now lives in stage0/lpt_nibble.inc (included below).

probe_pico:
    mov al, CMD_PROBE0
    call lpt_send_byte
    mov al, CMD_PROBE1
    call lpt_send_byte
    mov al, CMD_PROBE2
    call lpt_send_byte
    mov al, CMD_PROBE3
    call lpt_send_byte

    call lpt_recv_byte
    jc .fail
    cmp al, RSP_OK0
    jne .fail

    call lpt_recv_byte
    jc .fail
    cmp al, RSP_OK1
    jne .fail

    clc
    ret

.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; Stage1 metadata
;
; Pico response to CMD_GET_META:
;   u16 stage1_size
;   u16 total_blocks
;   u16 image_sum16
;
; Little-endian byte order.
; ---------------------------------------------------------------------------

get_stage1_meta:
    mov al, CMD_GET_META
    call lpt_send_byte

    call recv_u16
    jc .fail
    mov [stage1_size], ax

    call recv_u16
    jc .fail
    mov [stage1_blocks], ax

    call recv_u16
    jc .fail
    mov [stage1_sum16], ax

    clc
    ret

.fail:
    stc
    ret

check_stage1_size:
    ; Validate stage1_size and cross-check stage1_blocks == ceil(size/BLOCK_SIZE).
    ; A bad/corrupted metadata response must not be allowed to drive
    ; download_stage1's block-indexed writes past the intended image region;
    ; without this check, a forged stage1_blocks would let block_no * 64 walk
    ; over the PSP or Stage 0's own code before checksum failure.
    mov ax, [stage1_size]
    or ax, ax
    jz .bad
    cmp ax, MAX_STAGE1_SIZE
    ja .bad

    ; expected_blocks = (stage1_size + BLOCK_SIZE - 1) / BLOCK_SIZE
    add ax, BLOCK_SIZE - 1
    jc  .bad                    ; overflow guard
    mov cx, 6                   ; log2(BLOCK_SIZE) = log2(64) = 6
.shr_loop:
    shr ax, 1
    loop .shr_loop

    cmp ax, [stage1_blocks]
    jne .bad

    clc
    ret

.bad:
    stc
    ret

; ---------------------------------------------------------------------------
; Download Stage1
;
; Block protocol:
;   DOS -> Pico:
;       CMD_GET_BLOCK
;       u16 block_no
;
;   Pico -> DOS:
;       u8  payload_len
;       payload[payload_len]
;       u16 crc16_ccitt over payload bytes
;
;   DOS -> Pico:
;       ACK or NAK
; ---------------------------------------------------------------------------

download_stage1:
    xor ax, ax
    mov [current_block], ax

.block_loop:
    mov ax, [current_block]
    cmp ax, [stage1_blocks]
    jae .done

    mov byte [retry_count], BLOCK_RETRIES

.retry_block:
    call request_current_block
    jc .block_failed

    call receive_current_block
    jc .send_nak

    call verify_block_crc
    jc .send_nak

    mov al, CMD_ACK
    call lpt_send_byte

    inc word [current_block]
    jmp .block_loop

.send_nak:
    mov al, CMD_NAK
    call lpt_send_byte

    dec byte [retry_count]
    jnz .retry_block

.block_failed:
    stc
    ret

.done:
    clc
    ret

request_current_block:
    mov al, CMD_GET_BLOCK
    call lpt_send_byte
    mov ax, [current_block]
    call send_u16
    clc
    ret

receive_current_block:
    call lpt_recv_byte
    jc .fail

    mov [block_len], al
    cmp al, BLOCK_SIZE
    ja .fail

    ; Bound check: block_offset + block_len must not exceed stage1_size.
    ; Without this, a forged final-block payload_len could let stosb write
    ; past the intended image region into PSP/code memory before the whole-
    ; image checksum has a chance to reject it.
    mov ax, [current_block]
    mov bx, BLOCK_SIZE
    mul bx
    or dx, dx
    jnz .fail                   ; offset >= 64 KiB: impossible inside .COM

    mov bx, ax                  ; bx = block_offset_in_image (bytes)
    xor ah, ah
    mov al, [block_len]
    add bx, ax                  ; bx = block_offset + block_len
    jc  .fail                   ; wrapped past 64 KiB
    cmp bx, [stage1_size]
    ja  .fail                   ; would write past end of image

    ; Destination = STAGE1_LOAD_OFF + current_block * BLOCK_SIZE
    mov ax, [current_block]
    mov bx, BLOCK_SIZE
    mul bx
    add ax, STAGE1_LOAD_OFF
    mov di, ax

    xor cx, cx
    mov cl, [block_len]
    jcxz .recv_crc

.recv_loop:
    call lpt_recv_byte
    jc .fail
    stosb
    loop .recv_loop

.recv_crc:
    call recv_u16
    jc .fail
    mov [block_crc_recv], ax

    clc
    ret

.fail:
    stc
    ret

verify_block_crc:
    ; Recompute CRC over block payload at destination.
    mov ax, [current_block]
    mov bx, BLOCK_SIZE
    mul bx
    add ax, STAGE1_LOAD_OFF
    mov si, ax

    xor cx, cx
    mov cl, [block_len]

    call crc16_ccitt_buf
    cmp ax, [block_crc_recv]
    jne .bad

    clc
    ret

.bad:
    stc
    ret

verify_image_checksum:
    ; 16-bit additive checksum over exactly stage1_size bytes.
    mov si, STAGE1_LOAD_OFF
    mov cx, [stage1_size]
    xor bx, bx

.sum_loop:
    jcxz .done
    lodsb
    xor ah, ah
    add bx, ax
    dec cx
    jmp .sum_loop

.done:
    cmp bx, [stage1_sum16]
    jne .bad
    clc
    ret

.bad:
    stc
    ret

; ---------------------------------------------------------------------------
; LPT byte I/O moved to stage0/lpt_nibble.inc; the %include below brings in
; init_lpt_control, lpt_send_byte, lpt_recv_byte, lpt_recv_nibble, tiny_delay,
; and tiny_delay_short.
; ---------------------------------------------------------------------------

send_u16:
    ; AX little-endian
    push ax
    call lpt_send_byte
    pop ax
    mov al, ah
    call lpt_send_byte
    ret

recv_u16:
    call lpt_recv_byte
    jc .fail
    mov ah, al                  ; save low byte in AH
    call lpt_recv_byte
    jc .fail
    xchg al, ah                 ; AX = high:low
    clc
    ret

.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; CRC-16/CCITT-FALSE bitwise
;
; Polynomial: 0x1021
; Initial:    0xFFFF
; Reflected:  no
; XOR out:    0x0000
;
; Input:
;   DS:SI = buffer
;   CX    = length
; Output:
;   AX    = CRC
; Clobbers:
;   BX, CX, DX, SI
; ---------------------------------------------------------------------------

crc16_ccitt_buf:
    mov dx, 0FFFFh              ; DX = CRC

.crc_byte_loop:
    jcxz .crc_done

    lodsb                       ; AL = data byte
    xor dh, al                  ; crc ^= byte << 8

    mov bl, 8

.crc_bit_loop:
    test dx, 8000h
    jz .crc_shift_only
    shl dx, 1
    xor dx, 1021h
    jmp .crc_next_bit

.crc_shift_only:
    shl dx, 1

.crc_next_bit:
    dec bl
    jnz .crc_bit_loop

    dec cx
    jmp .crc_byte_loop

.crc_done:
    mov ax, dx
    ret

; ---------------------------------------------------------------------------
; Console output helpers
; ---------------------------------------------------------------------------

puts:
    ; DS:SI -> '$'-terminated string
    push ax
    push dx
    mov dx, si
    mov ah, 09h
    int 21h
    pop dx
    pop ax
    ret

print_base:
    mov si, msg_base_prefix
    call puts
    mov ax, [lpt_base]
    call print_hex16
    mov si, msg_crlf
    call puts
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
; Shared LPT SPP nibble byte pump (constants, routines, lpt_base, last_phase)
; ---------------------------------------------------------------------------

%include "stage0/lpt_nibble.inc"

; ---------------------------------------------------------------------------
; Data
; ---------------------------------------------------------------------------

lpt_candidates:
    dw 03BCh
    dw 0378h
    dw 0278h
    dw 0000h

stage1_size:        dw 0000h
stage1_blocks:      dw 0000h
stage1_sum16:       dw 0000h
current_block:      dw 0000h
block_crc_recv:     dw 0000h
block_len:          db 00h
retry_count:        db 00h

msg_banner:         db 'S0_XT Pico1284 bootstrap',13,10,'$'
msg_found:          db 'Pico1284 endpoint found',13,10,'$'
msg_base_prefix:    db 'LPT base: 0x','$'
msg_crlf:           db 13,10,'$'
msg_no_pico:        db 'ERROR: Pico1284 DB-25 endpoint not found',13,10,'$'
msg_meta_fail:      db 'ERROR: Stage1 metadata failed',13,10,'$'
msg_size_fail:      db 'ERROR: Stage1 size invalid',13,10,'$'
msg_download_fail:  db 'ERROR: Stage1 download failed',13,10,'$'
msg_checksum_fail:  db 'ERROR: Stage1 checksum failed',13,10,'$'
msg_jump:           db 'Jumping to Stage1',13,10,'$'
