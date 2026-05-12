; pico1284.asm
; ---------------------------------------------------------------------------
; DOS-side client stub for the vintage-kvm bridge.
;
; The production build will be a TSR/CLI written in NASM + Open Watcom C as
; described in docs/design.md §21.1, providing:
;
;   - LPT port detection and IEEE 1284 mode negotiation
;   - SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX packet exchange
;   - File transfer, screen dump, console I/O, memory access, remote exec
;   - RLE / Delta+RLE compression for screen and console traffic
;   - Optional PS/2 private-lane fallback when LPT negotiation fails
;
; This stub reserves the PICO1284.COM binary name and demonstrates the build
; target. Replace with the real implementation when Phase 10 (full TSR/CLI)
; lands per docs/design.md §22.
;
; Build:
;   nasm -f bin pico1284.asm -o PICO1284.COM
; ---------------------------------------------------------------------------

bits 16
org 0x100

start:
    mov ah, 09h
    mov dx, msg
    int 21h

    mov ax, 4C00h
    int 21h

msg     db 'PICO1284 v0.0 scaffold (no IEEE 1284 logic yet)',13,10,'$'
