; stage1.asm
; ---------------------------------------------------------------------------
; Stage 1 stub for vintage-kvm bootstrap chain.
;
; Loaded by Stage 0 (S0_XT.COM and successors) into the host program segment
; at offset 0x800 and entered via near jump. On entry from Stage 0:
;
;     AX = 0x3150 ('P1' marker)
;     BX = LPT base port chosen by Stage 0
;     CX = Stage 1 size in bytes
;     DX = Stage 0 kind code (e.g. 0x0003 = XT_LPT_BOOTSTRAP)
;     DS = ES = CS = host PSP segment
;
; Production responsibilities (docs/design.md §7.1, §8, §22 Phase 3+):
;   - LPT hardware detection and IEEE 1284 negotiation
;   - Capability handshake (CAP_REQ/RSP/ACK)
;   - SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX framed packet exchange
;   - Hand off to or load Stage 2 (PICO1284 TSR/CLI)
;
; This stub prints a banner and returns to DOS so the bootstrap path can be
; exercised end-to-end before real logic lands.
;
; Build:
;   nasm -f bin stage1.asm -o stage1.bin
; ---------------------------------------------------------------------------

bits 16
org 0x800

start:
    push cs
    pop ds

    mov dx, msg
    mov ah, 09h
    int 21h

    int 20h                 ; return to DOS via PSP at CS:0000

msg     db 'STAGE1 v0.0 scaffold reached',13,10,'$'
