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

; LPT register offsets
LPT_DATA          equ 0
LPT_STATUS        equ 1
LPT_CONTROL       equ 2

; Control register bits as written to base+2.
; INIT is used as the host strobe.
CTRL_INIT         equ 04h
CTRL_BASE         equ 0Ch

; Status bits
STAT_NIBBLE_MASK  equ 78h
STAT_PHASE        equ 80h

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
TIMEOUT_OUTER     equ 0FFFFh
TIMEOUT_INNER     equ 0010h

; Delay tuning.
; DELAY_COUNT=200 is intentionally conservative for 4.77 MHz XT-class
; machines and slow/marginal parallel-port glue logic. Stage1 can negotiate
; faster timing later.
DELAY_COUNT       equ 200

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
    ;   DX = Stage0 kind: 0003h = XT_LPT_BOOTSTRAP
    ;   DS = ES = CS
    mov ax, 3150h
    mov bx, [lpt_base]
    mov cx, [stage1_size]
    mov dx, 0003h
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

init_lpt_control:
    mov dx, [lpt_base]
    add dx, LPT_CONTROL
    mov al, CTRL_BASE
    out dx, al
    call tiny_delay
    ret

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
    mov ax, [stage1_size]
    or ax, ax
    jz .bad
    cmp ax, MAX_STAGE1_SIZE
    ja .bad
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

    ; Destination = STAGE1_LOAD_OFF + current_block * BLOCK_SIZE
    mov ax, [current_block]
    mov bx, BLOCK_SIZE
    mul bx
    or dx, dx
    jnz .fail

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
; LPT byte I/O
; ---------------------------------------------------------------------------

lpt_send_byte:
    ; AL = byte to send.
    ; Write byte to DATA, then toggle INIT as strobe.
    push ax
    push dx

    mov dx, [lpt_base]
    add dx, LPT_DATA
    out dx, al
    call tiny_delay

    mov dx, [lpt_base]
    add dx, LPT_CONTROL

    mov al, CTRL_BASE ^ CTRL_INIT
    out dx, al
    call tiny_delay

    mov al, CTRL_BASE
    out dx, al
    call tiny_delay

    pop dx
    pop ax
    ret

lpt_recv_byte:
    ; Return:
    ;   CF clear, AL = received byte
    ;   CF set on timeout
    push bx

    call lpt_recv_nibble
    jc .fail
    mov bl, al                  ; low nibble

    call lpt_recv_nibble
    jc .fail
    shl al, 4
    or al, bl

    clc
    pop bx
    ret

.fail:
    stc
    pop bx
    ret

lpt_recv_nibble:
    ; Wait for Pico phase bit to toggle, debounce/stabilize the status register,
    ; then read nibble from status bits 3..6.
    ;
    ; Pico-side expectation:
    ;   - Present nibble on logical status bits 3..6.
    ;   - Toggle logical status bit 7 after nibble is stable.
    ;   - Keep nibble stable until the DOS host consumes it.
    ;
    ; Returns AL = nibble 0..15.
    push bx
    push cx
    push dx

    mov dx, [lpt_base]
    add dx, LPT_STATUS

    in al, dx
    and al, STAT_PHASE
    mov bl, al                  ; previous phase

    mov cx, TIMEOUT_OUTER

.wait_outer:
    push cx
    mov cx, TIMEOUT_INNER

.wait_inner:
    in al, dx
    mov ah, al
    and ah, STAT_PHASE
    cmp ah, bl
    jne .candidate_phase
    loop .wait_inner

    pop cx
    loop .wait_outer

    stc
    jmp .done

.candidate_phase:
    ; Debounce/stability check:
    ; read the status register again after a very short delay. Accept only if
    ; the phase bit is still changed and the full status byte is stable enough
    ; for the nibble read.
    mov bh, al
    call tiny_delay_short
    in al, dx
    mov ah, al
    and ah, STAT_PHASE
    cmp ah, bl
    je .continue_waiting

    ; Prefer the second stable-ish sample for the nibble.
    pop cx                      ; balance pushed outer counter

    and al, STAT_NIBBLE_MASK
    shr al, 3
    and al, 0Fh

    clc
    jmp .done

.continue_waiting:
    ; Phase bounced or was not stable. Continue waiting within the same outer
    ; timeout budget.
    mov al, bh
    loop .wait_inner

    pop cx
    loop .wait_outer

    stc

.done:
    pop dx
    pop cx
    pop bx
    ret

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

tiny_delay:
    push cx
    mov cx, DELAY_COUNT

.loop:
    loop .loop
    pop cx
    ret

tiny_delay_short:
    push cx
    mov cx, 12

.loop:
    loop .loop
    pop cx
    ret

; ---------------------------------------------------------------------------
; Data
; ---------------------------------------------------------------------------

lpt_candidates:
    dw 03BCh
    dw 0378h
    dw 0278h
    dw 0000h

lpt_base:           dw 0000h
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
