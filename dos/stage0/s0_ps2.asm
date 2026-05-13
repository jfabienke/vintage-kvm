; s0_ps2.asm
; ---------------------------------------------------------------------------
; S0_PS2.COM - PS/2/SuperIO Stage0 bootstrap for Pico1284
;
; Builds a DOS .COM that probes LPT plus i8042 keyboard and AUX private
; channels. Stage 1 is downloaded over the fastest available channel, with all
; working channels left up for Stage 1.
; ---------------------------------------------------------------------------

%define ENABLE_AUX 1
%define DELAY_COUNT 50
%define BANNER_TEXT 'S0_PS2 Pico1284 bootstrap',13,10,'$'
%define NO_CHANNEL_TEXT 'ERROR: Pico1284 endpoint not found',13,10,'$'

%include "stage0/s0_atps2_core.inc"
