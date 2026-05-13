; s0_at.asm
; ---------------------------------------------------------------------------
; S0_AT.COM - AT-class Stage0 bootstrap for Pico1284
;
; Builds a DOS .COM that probes LPT plus the AT i8042 keyboard port. Stage 1
; is downloaded over LPT when available, otherwise over the keyboard private
; channel unlocked via the LED-pattern sequence.
; ---------------------------------------------------------------------------

%define ENABLE_AUX 0
%define DELAY_COUNT 100
%define BANNER_TEXT 'S0_AT Pico1284 bootstrap',13,10,'$'
%define NO_CHANNEL_TEXT 'ERROR: Pico1284 endpoint not found',13,10,'$'

%include "stage0/s0_atps2_core.inc"
