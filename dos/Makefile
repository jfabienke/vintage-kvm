# vintage-kvm DOS-side executables.
#
# All targets build with plain NASM and produce flat binaries. Open Watcom C
# is reserved for the production PICO1284 TSR/CLI per docs/design.md §21.1
# and is not required by this Makefile.
#
# Outputs land in dos/build/:
#   S0_XT.COM    XT-class Stage 0 bootstrap (real DOS .COM, run by user)
#   stage1.bin   Raw Stage 1 blob (Pico serves this over LPT to Stage 0;
#                loaded into Stage 0's segment at offset 0x800 and entered
#                via near jump)
#   PICO1284.COM DOS-side client (stub; production build will mix NASM +
#                Open Watcom C objects per docs/design.md §21.1)

NASM    ?= nasm
BUILD   := build

STAGE0_SRC  := stage0/s0_xt.asm
STAGE1_SRC  := stage1/stage1.asm
PICO_SRC    := pico1284/pico1284.asm

STAGE0_OUT  := $(BUILD)/S0_XT.COM
STAGE1_OUT  := $(BUILD)/stage1.bin
PICO_OUT    := $(BUILD)/PICO1284.COM

OUTPUTS := $(STAGE0_OUT) $(STAGE1_OUT) $(PICO_OUT)

.PHONY: all clean stage0 stage1 pico1284 sizes

all: $(OUTPUTS)

stage0:   $(STAGE0_OUT)
stage1:   $(STAGE1_OUT)
pico1284: $(PICO_OUT)

$(BUILD):
	@mkdir -p $@

$(STAGE0_OUT): $(STAGE0_SRC) | $(BUILD)
	$(NASM) -f bin -o $@ $<

$(STAGE1_OUT): $(STAGE1_SRC) | $(BUILD)
	$(NASM) -f bin -o $@ $<

$(PICO_OUT): $(PICO_SRC) | $(BUILD)
	$(NASM) -f bin -o $@ $<

sizes: $(OUTPUTS)
	@wc -c $(OUTPUTS)

clean:
	rm -rf $(BUILD)
