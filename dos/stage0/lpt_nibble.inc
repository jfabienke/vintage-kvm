; lpt_nibble.inc
; ---------------------------------------------------------------------------
; Shared LPT SPP nibble byte pump for vintage-kvm DOS stages.
;
; Consumed by:
;   dos/stage0/s0_xt.asm           (XT bootstrap;             DELAY_COUNT=200)
;   dos/stage0/s0_atps2_core.inc   (AT/PS2 bootstrap;         100 / 50)
;   dos/stage1/stage1.asm          (Stage 1 loader;           100)
;
; Wire protocol:
;   DOS  -> Pico: write byte to LPT_DATA, toggle nInit as host strobe.
;   Pico -> DOS:  present nibble on status bits 3..6, toggle bit 7 (phase)
;                 when stable. Persistent `last_phase` tracks the consumed
;                 phase across calls -- see docs/stage0_design.md §LPT
;                 nibble protocol §Persistent phase invariant.
;
; Parent file requirements before %include:
;   - `DELAY_COUNT equ <N>` or `%define DELAY_COUNT <N>` -- the inner-loop
;     count for tiny_delay (LPT pacing); typical values noted above.
;   - The parent must populate `lpt_base` before calling any pump routine
;     (find_pico_lpt does this on Stage 0; Stage 1 copies it from Stage 0's
;     hand-off BX, or from its own re-probe).
;
; Symbols exported:
;   Constants : LPT_DATA, LPT_STATUS, LPT_CONTROL, CTRL_INIT, CTRL_BASE,
;               STAT_NIBBLE_MASK, STAT_PHASE, TIMEOUT_OUTER, TIMEOUT_INNER
;   Routines  : init_lpt_control, lpt_send_byte, lpt_recv_byte,
;               lpt_recv_nibble, tiny_delay, tiny_delay_short
;   Data      : lpt_base (word), last_phase (byte)
; ---------------------------------------------------------------------------

%ifndef LPT_NIBBLE_INC
%define LPT_NIBBLE_INC

; LPT register offsets (parallel-port base + N)
LPT_DATA            equ 0
LPT_STATUS          equ 1
LPT_CONTROL         equ 2

; Control register bits as written to base+2.
; nInit is used as the host strobe.
CTRL_INIT           equ 04h
CTRL_BASE           equ 0Ch

; Status register bits used by the nibble protocol
STAT_NIBBLE_MASK    equ 78h         ; bits 3..6 carry the nibble payload
STAT_PHASE          equ 80h         ; bit 7 toggles when nibble is stable

; Receiver wait budgets (inner * outer = total iterations before timeout)
TIMEOUT_OUTER       equ 0FFFFh
TIMEOUT_INNER       equ 0010h

; ---------------------------------------------------------------------------
; init_lpt_control
;
; Set the SPP idle state on the control register and capture the current
; status-phase bit. After this, lpt_recv_nibble waits for the Pico to
; toggle the phase bit rather than racing whatever phase was on the wire.
; Caller must have populated `lpt_base` first.
; ---------------------------------------------------------------------------
init_lpt_control:
    mov dx, [lpt_base]
    add dx, LPT_CONTROL
    mov al, CTRL_BASE
    out dx, al
    call tiny_delay

    mov dx, [lpt_base]
    add dx, LPT_STATUS
    in  al, dx
    and al, STAT_PHASE
    mov [last_phase], al
    ret

; ---------------------------------------------------------------------------
; lpt_send_byte
;
; AL = byte to send. Preserves AX, DX (and all other registers).
; Writes the byte to LPT_DATA then pulses nInit as the host strobe.
; ---------------------------------------------------------------------------
lpt_send_byte:
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

; ---------------------------------------------------------------------------
; lpt_recv_byte
;
; Receive one byte from the Pico (two nibbles, low first).
; CF clear and AL = byte on success.
; CF set on outer-timer timeout.
; ---------------------------------------------------------------------------
lpt_recv_byte:
    push bx

    call lpt_recv_nibble
    jc .fail
    mov bl, al                          ; low nibble

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

; ---------------------------------------------------------------------------
; lpt_recv_nibble
;
; Wait for the Pico's phase bit to differ from [last_phase], debounce by
; re-reading after a short delay, then sample the nibble from status bits
; 3..6. Commits the new phase to [last_phase] before returning so the next
; call waits for the next Pico toggle, not this one.
;
; AL = nibble (0..15) on success, CF set on timeout.
; ---------------------------------------------------------------------------
lpt_recv_nibble:
    push bx
    push cx
    push dx

    mov dx, [lpt_base]
    add dx, LPT_STATUS

    mov bl, [last_phase]
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
    mov bh, al
    call tiny_delay_short
    in al, dx
    mov ah, al
    and ah, STAT_PHASE
    cmp ah, bl
    je .continue_waiting

    mov [last_phase], ah
    pop cx

    and al, STAT_NIBBLE_MASK
    shr al, 3
    and al, 0Fh

    clc
    jmp .done

.continue_waiting:
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

; ---------------------------------------------------------------------------
; tiny_delay / tiny_delay_short
;
; Bounded `loop` spin used for LPT pacing (tiny_delay) and nibble debounce
; (tiny_delay_short). tiny_delay's count is parent-tunable via DELAY_COUNT;
; tiny_delay_short is fixed at ~12 iterations.
; ---------------------------------------------------------------------------
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
; State
; ---------------------------------------------------------------------------
lpt_base:           dw 0000h        ; parent populates before pump use
last_phase:         db 00h          ; init_lpt_control seeds; receiver maintains

%endif ; LPT_NIBBLE_INC
