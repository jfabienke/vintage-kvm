; s0_atps2_core.inc
; ---------------------------------------------------------------------------
; Shared AT/PS2 Stage0 implementation.
;
; Required defines before include:
;   ENABLE_AUX     0 for AT, 1 for PS/2/SuperIO
;   DELAY_COUNT    LPT pacing loop count
;   BANNER_TEXT    db operands for banner string
;   NO_CHANNEL_TEXT db operands for no-channel error
; ---------------------------------------------------------------------------

bits 16
org 100h

STAGE1_LOAD_OFF   equ 0800h
MAX_STAGE1_SIZE   equ 50000
BLOCK_SIZE        equ 64

CHAN_LPT          equ 01h
CHAN_KBD          equ 02h
CHAN_AUX          equ 04h

; LPT register layout, control/status bits, nibble timeouts, and the
; init/send/recv/delay routines now live in the shared include. The
; include is brought in at the end of code (see %include below) so the
; .COM entry point at offset 0x100 remains the first emitted byte.

KBC_DATA          equ 060h
KBC_STATUS        equ 064h
KBC_CMD           equ 064h
KBC_OBF           equ 001h
KBC_IBF           equ 002h
KBC_AUX_OBF       equ 020h

KBC_READ_CCB      equ 020h
KBC_WRITE_CCB     equ 060h
KBC_DISABLE_AUX   equ 0A7h
KBC_ENABLE_AUX    equ 0A8h
KBC_WRITE_AUX     equ 0D4h

KBD_CMD_SET_LEDS  equ 0EDh
KBD_ACK           equ 0FAh

MOUSE_SET_RATE    equ 0F3h

CMD_PROBE0        equ 050h
CMD_PROBE1        equ 031h
CMD_PROBE2        equ 032h
CMD_PROBE3        equ 038h

CMD_GET_META      equ 081h
CMD_GET_BLOCK     equ 082h
CMD_ACK           equ 006h
CMD_NAK           equ 015h

RSP_OK0           equ 04Fh
RSP_OK1           equ 04Bh

PROBE_RETRIES     equ 3
BLOCK_RETRIES     equ 5

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

    xor ax, ax
    mov [dx_bitmap], ax
    mov [lpt_base], ax
    mov [current_channel], al
    mov [i8042_active], al
    mov [ccb_saved], al
    mov [aux_enabled], al

    call try_lpt_channel

    call try_i8042_private

    mov ax, [dx_bitmap]
    or ax, ax
    jz fail_no_channel

    call choose_download_channel
    jc fail_no_channel

    call get_stage1_meta
    jc fail_meta

    call check_stage1_size
    jc fail_size

    call download_stage1
    jc fail_download

    call verify_image_checksum
    jc fail_checksum

    mov si, msg_jump
    call puts

    mov ax, 3150h
    mov bx, [lpt_base]
    mov cx, [stage1_size]
    mov dx, [dx_bitmap]
    push cs
    pop ds
    push cs
    pop es
    jmp STAGE1_LOAD_OFF

fail_no_channel:
    mov si, msg_no_channel
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
    call restore_i8042_and_pic
    mov ax, 4C01h
    int 21h

try_lpt_channel:
    call find_pico_lpt
    jc .done
    or word [dx_bitmap], CHAN_LPT
    mov si, msg_lpt_found
    call puts
    call print_base
.done:
    ret

choose_download_channel:
    test word [dx_bitmap], CHAN_LPT
    jz .try_kbd
    mov byte [current_channel], CHAN_LPT
    clc
    ret

.try_kbd:
    test word [dx_bitmap], CHAN_KBD
    jz .try_aux
    mov byte [current_channel], CHAN_KBD
    clc
    ret

.try_aux:
%if ENABLE_AUX
    test word [dx_bitmap], CHAN_AUX
    jz .fail
    mov byte [current_channel], CHAN_AUX
    clc
    ret
%endif

.fail:
    stc
    ret

; ---------------------------------------------------------------------------
; LPT channel
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
    call probe_pico_lpt
    pop cx
    jnc .found
    loop .try_probe

    jmp .next_port

.found:
    clc
    ret

.not_found:
    xor ax, ax
    mov [lpt_base], ax
    stc
    ret

; init_lpt_control / lpt_send_byte / lpt_recv_byte / lpt_recv_nibble now
; live in stage0/lpt_nibble.inc (brought in at end of code below).

probe_pico_lpt:
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
; i8042 keyboard/AUX private channels
; ---------------------------------------------------------------------------

try_i8042_private:
    call i8042_mastery
    jc .done

    mov byte [i8042_active], 1

    call kbd_unlock_private
    jc .try_aux
    or word [dx_bitmap], CHAN_KBD
    mov si, msg_kbd_found
    call puts

.try_aux:
%if ENABLE_AUX
    call aux_unlock_private
    jc .restore_if_none
    or word [dx_bitmap], CHAN_AUX
    mov si, msg_aux_found
    call puts
%endif

.restore_if_none:
    test word [dx_bitmap], CHAN_KBD | CHAN_AUX
    jnz .done
    call restore_i8042_and_pic

.done:
    ret

i8042_mastery:
    in al, 21h
    mov [pic1_mask_orig], al
    or al, 02h
    out 21h, al

%if ENABLE_AUX
    in al, 0A1h
    mov [pic2_mask_orig], al
    or al, 10h
    out 0A1h, al
%endif

    call i8042_flush

    call i8042_read_ccb
    jc .fail
    mov [ccb_orig], al
    mov byte [ccb_saved], 1
    and al, 0BFh
    call i8042_write_ccb
    jc .fail

    clc
    ret

.fail:
    call restore_i8042_and_pic
    stc
    ret

restore_i8042_and_pic:
%if ENABLE_AUX
    ; Undo Stage 0's AUX enable (best-effort). Only fires on failure-exit
    ; paths; successful hand-off keeps AUX hardware up for Stage 1.
    cmp byte [aux_enabled], 0
    je .ccb
    call i8042_wait_ibe
    jc .aux_done
    mov al, KBC_DISABLE_AUX
    out KBC_CMD, al
.aux_done:
    mov byte [aux_enabled], 0
%endif

.ccb:
    cmp byte [ccb_saved], 0
    je .restore_pic
    mov al, [ccb_orig]
    call i8042_write_ccb
    mov byte [ccb_saved], 0

.restore_pic:
    mov al, [pic1_mask_orig]
    out 21h, al
%if ENABLE_AUX
    mov al, [pic2_mask_orig]
    out 0A1h, al
%endif
    mov byte [i8042_active], 0
    ret

i8042_flush:
    mov cx, 256
.loop:
    in al, KBC_STATUS
    test al, KBC_OBF
    jz .done
    in al, KBC_DATA
    loop .loop
.done:
    ret

i8042_read_ccb:
    call i8042_wait_ibe
    jc .fail
    mov al, KBC_READ_CCB
    out KBC_CMD, al
    call i8042_recv_kbd_byte
    ret
.fail:
    stc
    ret

i8042_write_ccb:
    push ax
    call i8042_wait_ibe
    jc .fail_pop
    mov al, KBC_WRITE_CCB
    out KBC_CMD, al
    call i8042_wait_ibe
    jc .fail_pop
    pop ax
    out KBC_DATA, al
    clc
    ret

.fail_pop:
    pop ax
    stc
    ret

kbd_unlock_private:
    ; First attempt; on failure, wait ~100 ms and retry once. Some slow
    ; Super I/O parts need a settling window after a partial sequence.
    call kbd_unlock_attempt
    jnc .ok
    call delay_100ms
    call kbd_unlock_attempt
.ok:
    ret

kbd_unlock_attempt:
    mov si, kbd_led_pattern
    mov cx, 5

.loop:
    mov al, KBD_CMD_SET_LEDS
    call kbd_send_expect_ack
    jc .fail
    call delay_1ms

    lodsb
    call kbd_send_expect_ack
    jc .fail
    call delay_1ms

    loop .loop

    call kbd_recv_private_byte
    jc .fail
    cmp al, 0AAh
    jne .fail

    call kbd_recv_private_byte
    jc .fail
    cmp al, 055h
    jne .fail

    clc
    ret

.fail:
    stc
    ret

aux_unlock_private:
%if ENABLE_AUX
    call i8042_wait_ibe
    jc .fail
    mov al, KBC_ENABLE_AUX
    out KBC_CMD, al
    mov byte [aux_enabled], 1
    call delay_5ms                  ; let AUX hardware come up before knock

    mov si, aux_rate_pattern
    mov cx, 3

.rate_loop:
    mov al, MOUSE_SET_RATE
    call aux_send_expect_ack
    jc .fail

    lodsb
    call aux_send_expect_ack
    jc .fail

    call delay_5ms                  ; IntelliMouse-spec inter-rate spacing

    loop .rate_loop

    call aux_recv_private_byte
    jc .fail
    cmp al, 0AAh
    jne .fail

    call aux_recv_private_byte
    jc .fail
    cmp al, 055h
    jne .fail

    clc
    ret

.fail:
%endif
    stc
    ret

kbd_send_expect_ack:
    call kbd_private_send_byte
    jc .fail
    call kbd_recv_private_byte
    jc .fail
    cmp al, KBD_ACK
    jne .fail
    clc
    ret
.fail:
    stc
    ret

aux_send_expect_ack:
%if ENABLE_AUX
    call aux_private_send_byte
    jc .fail
    call aux_recv_private_byte
    jc .fail
    cmp al, KBD_ACK
    jne .fail
    clc
    ret
.fail:
%endif
    stc
    ret

kbd_private_send_byte:
    push ax
    call i8042_wait_ibe
    jc .fail_pop
    pop ax
    out KBC_DATA, al
    clc
    ret
.fail_pop:
    pop ax
    stc
    ret

kbd_recv_private_byte:
    call i8042_recv_kbd_byte
    ret

aux_private_send_byte:
%if ENABLE_AUX
    push ax
    call i8042_wait_ibe
    jc .fail_pop
    mov al, KBC_WRITE_AUX
    out KBC_CMD, al
    call i8042_wait_ibe
    jc .fail_pop
    pop ax
    out KBC_DATA, al
    clc
    ret
.fail_pop:
    pop ax
%endif
    stc
    ret

aux_recv_private_byte:
%if ENABLE_AUX
    call i8042_recv_aux_byte
    ret
%else
    stc
    ret
%endif

i8042_wait_ibe:
    push cx
    push dx
    mov cx, TIMEOUT_OUTER

.outer:
    push cx
    mov cx, TIMEOUT_INNER

.inner:
    in al, KBC_STATUS
    test al, KBC_IBF
    jz .ok
    loop .inner

    pop cx
    loop .outer

    stc
    jmp .done

.ok:
    pop cx
    clc

.done:
    pop dx
    pop cx
    ret

i8042_recv_kbd_byte:
    push cx
    push dx
    mov cx, TIMEOUT_OUTER

.outer:
    push cx
    mov cx, TIMEOUT_INNER

.inner:
    in al, KBC_STATUS
    test al, KBC_OBF
    jz .next
%if ENABLE_AUX
    test al, KBC_AUX_OBF
    jnz .drain_or_wait
%endif
    in al, KBC_DATA
    pop cx
    clc
    jmp .done

%if ENABLE_AUX
.drain_or_wait:
    in al, KBC_DATA
    loop .inner
    pop cx
    loop .outer
    stc
    jmp .done
%endif

.next:
    loop .inner
    pop cx
    loop .outer

    stc

.done:
    pop dx
    pop cx
    ret

i8042_recv_aux_byte:
%if ENABLE_AUX
    push cx
    push dx
    mov cx, TIMEOUT_OUTER

.outer:
    push cx
    mov cx, TIMEOUT_INNER

.inner:
    in al, KBC_STATUS
    test al, KBC_OBF
    jz .next
    test al, KBC_AUX_OBF
    jz .drain_or_wait
    in al, KBC_DATA
    pop cx
    clc
    jmp .done

.drain_or_wait:
    in al, KBC_DATA
    loop .inner
    pop cx
    loop .outer
    stc
    jmp .done

.next:
    loop .inner
    pop cx
    loop .outer

    stc

.done:
    pop dx
    pop cx
    ret
%else
    stc
    ret
%endif

; ---------------------------------------------------------------------------
; Transport dispatcher + Stage1 download
; ---------------------------------------------------------------------------

send_byte:
    cmp byte [current_channel], CHAN_LPT
    jne .try_kbd
    call lpt_send_byte
    clc
    ret

.try_kbd:
    cmp byte [current_channel], CHAN_KBD
    jne .try_aux
    call kbd_private_send_byte
    ret

.try_aux:
%if ENABLE_AUX
    cmp byte [current_channel], CHAN_AUX
    jne .fail
    call aux_private_send_byte
    ret
%endif

.fail:
    stc
    ret

recv_byte:
    cmp byte [current_channel], CHAN_LPT
    jne .try_kbd
    call lpt_recv_byte
    ret

.try_kbd:
    cmp byte [current_channel], CHAN_KBD
    jne .try_aux
    call kbd_recv_private_byte
    ret

.try_aux:
%if ENABLE_AUX
    cmp byte [current_channel], CHAN_AUX
    jne .fail
    call aux_recv_private_byte
    ret
%endif

.fail:
    stc
    ret

get_stage1_meta:
    mov al, CMD_GET_META
    call send_byte
    jc .fail

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

    add ax, BLOCK_SIZE - 1
    jc .bad
    mov cx, 6
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
    call send_byte
    jc .block_failed

    inc word [current_block]
    jmp .block_loop

.send_nak:
    mov al, CMD_NAK
    call send_byte

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
    call send_byte
    jc .fail
    mov ax, [current_block]
    call send_u16
    ret
.fail:
    stc
    ret

receive_current_block:
    call recv_byte
    jc .fail

    mov [block_len], al
    cmp al, BLOCK_SIZE
    ja .fail

    mov ax, [current_block]
    mov bx, BLOCK_SIZE
    mul bx
    or dx, dx
    jnz .fail

    mov bx, ax
    xor ah, ah
    mov al, [block_len]
    add bx, ax
    jc .fail
    cmp bx, [stage1_size]
    ja .fail

    mov ax, [current_block]
    mov bx, BLOCK_SIZE
    mul bx
    add ax, STAGE1_LOAD_OFF
    mov di, ax

    xor cx, cx
    mov cl, [block_len]
    jcxz .recv_crc

.recv_loop:
    call recv_byte
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

send_u16:
    push ax
    call send_byte
    jc .fail_pop
    pop ax
    mov al, ah
    call send_byte
    ret

.fail_pop:
    pop ax
    stc
    ret

recv_u16:
    call recv_byte
    jc .fail
    mov ah, al
    call recv_byte
    jc .fail
    xchg al, ah
    clc
    ret

.fail:
    stc
    ret

crc16_ccitt_buf:
    mov dx, 0FFFFh

.crc_byte_loop:
    jcxz .crc_done

    lodsb
    xor dh, al

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
; Console and delay helpers
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

; tiny_delay / tiny_delay_short now live in stage0/lpt_nibble.inc.

; Approximate ~1 ms spacing for LED-unlock inter-command pauses. Sized for
; 6 MHz 286 worst case (~17 cycles/loop iteration); shorter on faster CPUs,
; which is fine — the upper bound is what matters for Super I/O settling.
delay_1ms:
    push cx
    mov cx, 1500

.loop:
    loop .loop
    pop cx
    ret

; Approximate ~5 ms spacing between AUX 200/100/80 sample-rate writes per
; IntelliMouse spec timing. Same sizing rationale as delay_1ms.
delay_5ms:
    push cx
    mov cx, 7500

.loop:
    loop .loop
    pop cx
    ret

; Approximate ~100 ms wait between unlock attempts on initial failure.
; Outer × inner = ~75k iterations: ~210 ms on 6 MHz 286, ~40 ms on 16 MHz
; 386, single-digit ms on 486+. Range is wide but always within the
; "noticeable settling window" intent of the design.
delay_100ms:
    push bx
    push cx
    mov bx, 50

.outer:
    mov cx, 1500
.inner:
    loop .inner
    dec bx
    jnz .outer
    pop cx
    pop bx
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

kbd_led_pattern:
    db 00h, 07h, 00h, 05h, 02h

aux_rate_pattern:
    db 200, 100, 80

stage1_size:        dw 0000h
stage1_blocks:      dw 0000h
stage1_sum16:       dw 0000h
current_block:      dw 0000h
block_crc_recv:     dw 0000h
dx_bitmap:          dw 0000h

block_len:          db 00h
retry_count:        db 00h
current_channel:    db 00h
i8042_active:       db 00h
ccb_saved:          db 00h
ccb_orig:           db 00h
pic1_mask_orig:     db 00h
pic2_mask_orig:     db 00h
aux_enabled:        db 00h          ; 1 if Stage 0 issued OUT 64h, 0xA8

msg_banner:         db BANNER_TEXT
msg_lpt_found:      db 'LPT channel up',13,10,'$'
msg_kbd_found:      db 'KBD private channel up',13,10,'$'
msg_aux_found:      db 'AUX private channel up',13,10,'$'
msg_base_prefix:    db 'LPT base: 0x','$'
msg_crlf:           db 13,10,'$'
msg_no_channel:     db NO_CHANNEL_TEXT
msg_meta_fail:      db 'ERROR: Stage1 metadata failed',13,10,'$'
msg_size_fail:      db 'ERROR: Stage1 size invalid',13,10,'$'
msg_download_fail:  db 'ERROR: Stage1 download failed',13,10,'$'
msg_checksum_fail:  db 'ERROR: Stage1 checksum failed',13,10,'$'
msg_jump:           db 'Jumping to Stage1',13,10,'$'
