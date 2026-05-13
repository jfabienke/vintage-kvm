# vintage-kvm DOS-side executables.
#
# All targets build with plain NASM and produce flat binaries. Open Watcom C
# is reserved for the production PICO1284 TSR/CLI per docs/design.md §21.1
# and is not required by this Makefile.
#
# Outputs land in dos/build/:
#   S0_XT.COM    XT-class Stage 0 bootstrap (real DOS .COM, run by user)
#   S0_AT.COM    AT-class Stage 0 bootstrap (LPT + i8042 KBD private)
#   S0_PS2.COM   PS/2/SuperIO Stage 0 bootstrap (LPT + KBD + AUX private)
#   stage1.bin   Raw Stage 1 blob (Pico serves this over LPT to Stage 0;
#                loaded into Stage 0's segment at offset 0x800 and entered
#                via near jump)
#   PICO1284.COM DOS-side client (stub; production build will mix NASM +
#                Open Watcom C objects per docs/design.md §21.1)

NASM    ?= nasm
BUILD   := build

STAGE0_XT_SRC   := stage0/s0_xt.asm
STAGE0_AT_SRC   := stage0/s0_at.asm
STAGE0_PS2_SRC  := stage0/s0_ps2.asm
STAGE0_CORE     := stage0/s0_atps2_core.inc
STAGE0_NIBBLE   := stage0/lpt_nibble.inc
STAGE1_SRC  := stage1/stage1.asm
PICO_SRC    := pico1284/pico1284.asm

STAGE0_XT_OUT   := $(BUILD)/S0_XT.COM
STAGE0_AT_OUT   := $(BUILD)/S0_AT.COM
STAGE0_PS2_OUT  := $(BUILD)/S0_PS2.COM
STAGE1_OUT  := $(BUILD)/stage1.bin
PICO_OUT    := $(BUILD)/PICO1284.COM

STAGE0_OUTPUTS := $(STAGE0_XT_OUT) $(STAGE0_AT_OUT) $(STAGE0_PS2_OUT)
OUTPUTS := $(STAGE0_OUTPUTS) $(STAGE1_OUT) $(PICO_OUT)

.PHONY: all clean stage0 stage1 pico1284 sizes

all: $(OUTPUTS)

stage0:   $(STAGE0_OUTPUTS)
stage1:   $(STAGE1_OUT)
pico1284: $(PICO_OUT)

$(BUILD):
	@mkdir -p $@

$(STAGE0_XT_OUT): $(STAGE0_XT_SRC) $(STAGE0_NIBBLE) | $(BUILD)
	$(NASM) -f bin -o $@ $<

$(STAGE0_AT_OUT): $(STAGE0_AT_SRC) $(STAGE0_CORE) $(STAGE0_NIBBLE) | $(BUILD)
	$(NASM) -f bin -o $@ $<

$(STAGE0_PS2_OUT): $(STAGE0_PS2_SRC) $(STAGE0_CORE) $(STAGE0_NIBBLE) | $(BUILD)
	$(NASM) -f bin -o $@ $<

$(STAGE1_OUT): $(STAGE1_SRC) $(STAGE0_NIBBLE) | $(BUILD)
	$(NASM) -f bin -o $@ $<

$(PICO_OUT): $(PICO_SRC) | $(BUILD)
	$(NASM) -f bin -o $@ $<

sizes: $(OUTPUTS)
	@wc -c $(OUTPUTS)

clean:
	rm -rf $(BUILD)
