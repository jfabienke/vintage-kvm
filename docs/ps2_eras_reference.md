# PS/2 Eras Reference: XT → AT → PS/2 → SuperIO

**Status:** Reference document  
**Last updated:** 2026-05-12  
**Companion document:** [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md)

vintage-kvm must talk to DOS PCs spanning ~1981 to mid-1990s+. The "PS/2 connector" looks continuous in retrospect but masks three protocol discontinuities. Both the Pico's PS/2 emulation and the DOS-side Stage 0 must handle all four eras.

## Era comparison

| Era | Years | Host chip | Connector | Direction | Frame | Clock | Scancode set | Mouse/AUX |
|---|---|---|---|---|---|---|---|---|
| **XT** | 1981–84 | Intel 8255 PPI + 8048 in keyboard | 5-pin DIN | **Unidirectional** (kbd→PC only) | 9-bit: start(1) + 8 data LSB-first, no parity, no stop | ~8 kHz | Set 1 | None |
| **AT** | 1984–87 | Intel 8042 | 5-pin DIN | **Bidirectional** | 11-bit: start(0) + 8 data + odd parity + stop(1) | 10–16.7 kHz | Set 2 default; 8042 can translate to Set 1 | None |
| **PS/2** | 1987–95 | Extended 8042 with AUX port | 6-pin mini-DIN | Bidirectional | 11-bit (same as AT) | 10–16.7 kHz | Set 2 | Yes — IRQ12, host cmd `0xD4` prefixes AUX-bound byte |
| **SuperIO** | 1995+ | Winbond / ITE / NS / SMSC i8042-compat in LPC ASIC | 6-pin mini-DIN | Bidirectional | 11-bit | 10–16.7 kHz | Set 2 | Yes |

**Major discontinuities:**
- **XT → AT**: unidirectional 9-bit becomes bidirectional 11-bit; no commands → full command set; LED-set unlock (§7.4) becomes available.
- **AT → PS/2**: keyboard wire protocol unchanged; AUX/mouse port added.
- **PS/2 → SuperIO**: electrically and protocol-identical from peripheral side. SuperIO just lives in a larger ASIC with FDC/UART/LPT and tolerates faster timing.

## Per-era Pico changes

Two PIO programs cover all eras (already foreshadowed by `No0ne/ps2pico` shipping `ps2pico.uf2` for AT/PS/2 and `ps2pico-XT.uf2` for XT as separate builds):

| Pico mode | PIO program | Frame | Command handling | Notes |
|---|---|---|---|---|
| XT keyboard | `ps2_xt_dev.pio` (new — adapt from `No0ne/ps2pico` XT variant) | 9-bit unidir | None | When host pulls clock low (XT inhibit), pause and resume on release. No LED-set unlock possible. Only role: type the DEBUG script into XT BIOS. |
| AT/PS/2 keyboard | `ps2_at_dev.pio` (adapt `No0ne/ps2x2pico/src/ps2out.pio`) | 11-bit bidir | Full Set 1/Set 2 command set; §7.4 LED-pattern unlock detector | Same program serves AT, PS/2, and SuperIO hosts. Wire timing is identical. |
| PS/2 mouse (AUX) | Same `ps2_at_dev.pio` on a second SM | 11-bit bidir | Mouse command set + IntelliMouse knock sequence 200/100/80 | Only enabled on PS/2-class and SuperIO hosts. |

**Scancode emitter** needs both Set 1 (for XT) and Set 2 (for AT+). On AT+ the host may enable 8042 translation (Controller Command Byte bit 6) which converts our Set 2 stream back to Set 1 anyway — but don't assume; emit Set 2 and let the 8042 translate.

**Mode selection** at the Pico: pragmatic answer is a config GPIO/button/boot flag picks the host class. Auto-detection on the wire during DEBUG-injection is fragile (no out-of-band channel exists on XT). `No0ne/ps2pico` ships separate UF2s for the two modes — vintage-kvm should do something similar: same firmware, host-class flag in boot config or persisted in flash.

## Per-era DOS Stage 0 changes

> **Full Stage 0 design:** [`stage0_design.md`](stage0_design.md). This table is the era-aware summary; the design doc covers per-variant size budgets, hand-off ABI, LED-pattern unlock, AUX enable, and failure handling.

| Stage 0 file | Target era | Channels available | What's new |
|---|---|---|---|
| `dos/stage0/s0_xt.asm` *(exists)* | XT, 8088/8086 | LPT (nibble bidir), keyboard (input-only typing of DEBUG script) | — (already correct; XT has no i8042 to master) |
| `dos/stage0/s0_at.asm` *(exists)* | AT, 286+ | LPT + **i8042 keyboard port** (bidirectional via §7.4 LED-pattern unlock) | i8042 mastery: mask IRQ1 at PIC, flush 0x60, send `0xED`+mask sequence, verify Pico private-mode response, then bidirectional byte exchange via 0x60/0x64 |
| `dos/stage0/s0_ps2.asm` *(exists)* | PS/2 + SuperIO, 386+ | LPT + i8042 keyboard + **i8042 AUX (mouse)** for dual-lane fallback transport | Adds AUX channel via `0xD4` command prefix and IRQ12 masking — this is what enables `docs/design.md` §17 dual-lane fallback |

**System detection at boot:**
- BIOS data area `0040:0010` (equipment word) and `INT 11h` indicate mouse/PS/2 presence.
- XT vs AT detection: write a known byte to port `0x64` and read status. XT systems get garbage (port 0x64 isn't decoded).
- AT vs PS/2 detection: probe i8042 AUX port (`OUT 64h, 0A8h`; check for errors).
- SuperIO indistinguishable from PS/2 at the i8042 level — no need to discriminate further. A20 gate handling via `0x64` cmd `0xD1` is the only SuperIO-vs-vanilla-PS/2 wrinkle, and only matters if real-mode addressing wraps at 1 MB.

**i8042 mastery pattern (used by `s0_at.asm` and `s0_ps2.asm`):**

```asm
; Mask IRQ1 at PIC so BIOS INT 9 stops firing while we own the controller
in   al, 21h          ; read PIC1 mask
or   al, 02h          ; set bit 1 (IRQ1)
out  21h, al

; Flush output buffer
.flush:
in   al, 64h          ; status
test al, 01h          ; OBF? (output buffer full from controller)
jz   .flushed
in   al, 60h          ; consume and discard
jmp  .flush
.flushed:

; Wait for input buffer empty before issuing command
.wait_ibe:
in   al, 64h
test al, 02h          ; IBF? (input buffer full to controller)
jnz  .wait_ibe

; Send keyboard command byte (e.g. 0xED for LED set)
mov  al, 0EDh
out  60h, al
; (then wait_ibe again, send 00h mask, wait OBF, read 0xFA ACK, repeat for next ED+mask)
```

For AUX (`s0_ps2.asm` only), prefix any keyboard-side command byte with `OUT 64h, 0D4h` so the next `OUT 60h, val` routes to the mouse port.

## Reference code per era

### XT (1981–1984)

| Reference | What it gives | License |
|---|---|---|
| [`tmk_keyboard` wiki: IBM PC XT Keyboard Protocol](https://github.com/tmk/tmk_keyboard/wiki/IBM-PC-XT-Keyboard-Protocol) | Definitive protocol writeup — 9-bit framing, clock timing, host inhibit behavior, Set 1 scancodes | Doc |
| [`No0ne/ps2pico`](https://github.com/No0ne/ps2pico) (built as `ps2pico-XT.uf2`) | Pico-RP2040 XT keyboard emulator, working firmware. Note: protected by NPN transistor + zener level shifter (schematic in README) | MIT |
| [`AndersBNielsen/pcxtkbd`](https://github.com/AndersBNielsen/pcxtkbd) | Arduino XT keyboard implementation; readable reference | MIT |
| [`skiselev/ps2-xt`](https://github.com/skiselev/ps2-xt) | AVR PS/2-to-XT converter; protocol code portable | GPL |
| [`asig/ps2-to-xt-adapter`](https://github.com/asig/ps2-to-xt-adapter) | Pico adapter for IBM 5150/5160 specifically | MIT |
| [`cr1901/AT2XT`](https://github.com/cr1901/AT2XT) | AT-to-XT protocol converter firmware (note: moved to Codeberg) | — |
| [minuszerodegrees.net](https://minuszerodegrees.net/) | Ray Knight's vintage IBM hardware archive — the canonical XT reference site, also has [a ps2pico writeup](https://minuszerodegrees.net/keyboard/ps2pico.htm) | Doc |

### AT (1984–1987)

| Reference | What it gives | License |
|---|---|---|
| [OSDev wiki: "8042" PS/2 Controller](https://wiki.osdev.org/%228042%22_PS/2_Controller) | Host-side controller programming, port map, status bits, command set, init sequence | Doc |
| [OSDev wiki: PS/2 Keyboard](https://wiki.osdev.org/PS/2_Keyboard) | Device-side protocol (works for AT and PS/2 — wire-identical) | Doc |
| [Microsoft scancode `translate.pdf`](https://download.microsoft.com/download/1/6/1/161ba512-40e2-4cc9-843a-923143f3456c/translate.pdf) | Set 2 → Set 1 8042 translation table | Doc (MS) |
| [Linux `drivers/input/serio/i8042.c`](https://github.com/torvalds/linux/blob/master/drivers/input/serio/i8042.c) | Modern host-side reference for the 8042 (companion to `[[ieee1284-controller-reference]]`'s uss720.c) | GPL-2.0 |
| [Linux `drivers/input/keyboard/atkbd.c`](https://github.com/torvalds/linux/blob/master/drivers/input/keyboard/atkbd.c) | AT keyboard layer above i8042; useful for what commands the host actually issues in practice | GPL-2.0 |

### PS/2 (1987–1995)

| Reference | What it gives | License |
|---|---|---|
| [OSDev wiki: PS/2 Mouse](https://wiki.osdev.org/PS/2_Mouse) | Device-side mouse protocol, IntelliMouse knock (200/100/80), 3/4/5-byte packet formats | Doc |
| [`No0ne/ps2x2pico`](https://github.com/No0ne/ps2x2pico) | Pico keyboard + mouse (both endpoints) emulator. **Closest functional match to vintage-kvm's PS/2 side.** Files: `src/ps2out.pio` (device-side PIO, MIT), `src/ps2in.pio` (host-side PIO), `src/ps2kb.c` + `src/ps2ms.c` (C-level command state machines) | MIT |
| [QEMU `hw/input/ps2.c`](https://github.com/qemu/qemu/blob/master/hw/input/ps2.c) | Device-side keyboard + mouse emulation; complete command state machine (BAT, ACK, all command bytes) | GPL-2.0 |
| [`PaulW/rp2040-keyboard-converter`](https://github.com/PaulW/rp2040-keyboard-converter) | Multi-protocol RP2040 converter (Model F PC/AT + more), actively maintained | MIT |
| [`Harvie/ps2dev`](https://github.com/Harvie/ps2dev) | The C++ PS/2 device library most Arduino projects link in | GPL-3.0 |
| [Linux `drivers/input/mouse/psmouse-base.c`](https://github.com/torvalds/linux/blob/master/drivers/input/mouse/psmouse-base.c) | Host-side mouse driver; quirks per mouse model | GPL-2.0 |

### SuperIO (1995+)

Wire-protocol-identical to PS/2 — the device-side references above all apply. The KBC block inside a Super I/O is fundamentally an **8042-compatible controller with PS/2 signalling**; vendor extensions are board-management features (wake, port routing, ACPI), not richer mouse protocols. **No mainstream Super I/O exposes a high-level pointing-device engine, multi-drop AUX, IntelliMouse decode in the KBC itself, or any hardware "sideband mailbox over PS/2."**

From the Pico's perspective every Super I/O looks like an AT/PS/2 host — the same `ps2_at_dev.pio` program drives them all. The relevance to vintage-kvm is on the **DOS side** (Stage 0 / pico1284 may probe chip identity for diagnostics, or trip over board-level quirks like KB/MS port auto-swap).

#### Catalog of common Super I/O parts (host-side features)

| Vendor / family | KBC capability | Vendor-specific extras beyond plain PS/2 |
|---|---|---|
| **Winbond / Nuvoton W83627HF/F/HG/G** | 8042-compatible KBC, PS/2 mouse, port 92, interrupt/polling modes, fast Gate A20, hardware keyboard reset, selectable 6/8/12/16 MHz KBC clock | **Strongest extension set:** programmable keyboard wake (up to 5-key password sequence via indexed registers), mouse wake (left/right button selection, single/double-click), CIR wake, ACPI S1–S5 wake-source status, **KB/MS port swap bit**, optional AMIKEY/customer KBC firmware in internal ROM/RAM |
| **Nuvoton NCT6776D** | 8042 KBC with two data registers + status register, PS/2 mouse, port 92, interrupt/polling, fast Gate A20, 12 MHz | Conventional from public docs; inherits Winbond-style integration but detailed wake/password registers not in public listing |
| **ITE IT8718F** | 8042-compatible KBC + PS/2 mouse, hardware KBC, Gate A20, keyboard reset | Sequential/simultaneous-key power-on events (any key / 2–5 sequential / 1–3 simultaneous keys), mouse double-click and/or mouse-move power-on, **keyboard/mouse interface hardware auto-swap** |
| **ITE IT8786E-I** | LPC/eSPI Super I/O, legacy I/O + hardware monitor + fan controller | Modern industrial direction: eSPI/LPC bridging + environmental control; public docs don't expose detailed KBC extensions |
| **SMSC/Microchip LPC47M192** | 8042-based KBC/mouse in PC99/PC2001 LPC Super I/O | ACPI 1.0/2.0, low-power modes, KBD/mouse wake events |
| **Microchip SCH322x** | LPC Super I/O family, 8042 KBC, PS/2 keyboard/mouse options | Industrial/embedded packaging; security features, power control, reset generation, hardware monitor, long lifecycle. PS/2 is a configurable family option rather than a protocol enhancement |
| **Fintek F71889 / F71889A** | KBC, 8042-compatibility, PS/2 mouse, interrupt/polling, Gate A20, keyboard reset | System-level extras: ACPI/DPM, 5VDUAL switching, deep S5 behavior, CIR on some variants, GPIO/system-volume functions, port 0x80 debug. **KBC itself conventional.** |
| **SMSC/Microchip FDC37C669** | **PS/2 ports: 0** | Useful counterexample — not every "Super I/O" has KBC capability. Don't assume PS/2 just because it's a Super I/O |

#### What "beyond standard PS/2" typically means

The common Super I/O extensions, none of which change the Pico's device-side protocol:

1. **Wake-on-keyboard / wake-on-mouse** from ACPI sleep/soft-off (Winbond OnNow S1–S5)
2. **Keyboard password or hotkey wake** (Winbond indexed 5-key sequence; ITE sequential/simultaneous-key events)
3. **Mouse wake semantics** — left/right button, single/double-click, mouse-move
4. **KB/MS port swap** (Winbond bit, ITE auto-swap) — compensates for swapped mini-DIN routing at the board level. **Implication for vintage-kvm:** the Pico can't unambiguously tell from the wire which physical port it's plugged into if auto-swap is enabled
5. **Gate A20, reset, port 92 integration** — classic PC platform hooks; not the Pico's concern
6. **Firmware/custom-code KBC** — Winbond optional AMIKEY/customer firmware with internal RAM/ROM. Generally serves 8042 compatibility, not a new peripheral model

#### What does NOT exist on mainstream Super I/O parts

- Arbitrary UART-like mode on mini-DIN pins
- Direct raw Clock/Data bitstream capture exposed to host software
- Multi-drop AUX addressing
- High-speed PS/2 variants
- Hardware packet FIFOs beyond normal KBC buffering
- Vendor-documented "sideband mailbox over PS/2"

The KBC path is always an 8042 compatibility abstraction. Even when the Super I/O has many GPIOs and rich power-management registers, the PS/2 block is not exposed as a general serial engine.

#### Implications for vintage-kvm

- **Pico side:** zero changes per Super I/O variant. The same `ps2_at_dev.pio` and Rust state machines handle all of them.
- **DOS side (Stage 0 / pico1284):** worth probing Super I/O identity (port `0x2E`/`0x4E` config-mode unlock sequence varies by vendor) only for diagnostics, not behavior. **Avoid relying on which mini-DIN port is "keyboard" vs "mouse" by physical position** — port-swap can be set in BIOS or by Super I/O firmware. Probe identity via `INT 11h` equivalent and i8042 commands rather than connector position.
- **Phase 11 wake-event leverage (potential):** if vintage-kvm ever wants the Pico to wake a sleeping DOS PC, mouse-move wake or keyboard hotkey wake are documented Super I/O features — the Pico can emit a movement packet or a configured hotkey sequence and the Super I/O brings the system out of S1–S5. Not in scope today; worth noting for the future.

#### ACPI / PnP identification

coreboot's [`src/superio/acpi/pnp_kbc.asl`](https://github.com/coreboot/coreboot/blob/master/src/superio/acpi/pnp_kbc.asl) is the parameterized template every Super I/O chip family instantiates for its KBC + PS/2 mouse ACPI block. Three things every Super I/O agrees on at the ACPI layer:

**PnP hardware IDs (universal across all Super I/O variants):**

| Device | PnP ID | What it represents |
|---|---|---|
| Keyboard controller | `PNP0303` | IBM Enhanced (101/102-key, PS/2) keyboard |
| PS/2 mouse / AUX | `PNP0F13` | PS/2 Mouse |

These are stable across every coreboot-supported Super I/O and every modern PnP BIOS. **Implication for vintage-kvm:** Stage 0 / pico1284 on PnP-capable systems can enumerate the KBC and AUX via these IDs rather than probing chipset registers. On pre-PnP DOS, fall back to port `0x64` probe + `OUT 64h, 0xA8` AUX-enable probe.

**Universal resource template (also hard-coded in the ASL):**

| Resource | Value |
|---|---|
| KBC data port | `0x60` (single-byte I/O) |
| KBC status/command port | `0x64` (single-byte I/O) |
| KBC IRQ | `1` |
| PS/2 mouse / AUX IRQ | `12` |

The `_PRS` (possible resource settings) is a single fixed dependent function with no alternatives — coreboot codifies that **no Super I/O is free to put the KBC anywhere else.** Stage 0 can hard-code `0x60`/`0x64`/IRQ1/IRQ12 with confidence.

**Two AUX architectures** (the ASL is mutually exclusive about these):

- **`SUPERIO_KBC_PS2M`** — mouse shares the same Logical Device Number (LDN) as the keyboard. Two IRQs come out of one LDN. Common pattern on older Winbond parts.
- **`SUPERIO_KBC_PS2LDN`** — mouse has its own independently-configurable LDN. Enables clean enable/disable of AUX without touching KBD. Used by ITE IT87xx and similar.

**Implication for vintage-kvm:** the §17 dual-lane fallback doesn't care which AUX architecture the host uses — both lanes are active by default and Stage 0 masks IRQ1 + IRQ12 while it owns the controller, then Stage 1 installs any handlers it needs before unmasking. The distinction matters only if Stage 0 wants to **disable** one lane without affecting the other (e.g., bring AUX down for diagnostics while keyboard stays live). On PS2M chips that requires extra care because disabling the LDN takes both lanes.

#### References

| Reference | What it gives |
|---|---|
| [coreboot `src/superio/acpi/pnp_kbc.asl`](https://github.com/coreboot/coreboot/blob/master/src/superio/acpi/pnp_kbc.asl) | Parameterized ACPI template — PnP IDs, universal `0x60`/`0x64`/IRQ1/IRQ12 resources, the two AUX architectures (PS2M vs PS2LDN), enable/disable + Notify pattern |
| [coreboot per-chip SuperIO drivers](https://github.com/coreboot/coreboot/tree/master/src/superio) | Per-chip ASL specializations (Winbond, ITE, Fintek, SMSC) supplying `ENTER_CONFIG_MODE`/`EXIT_CONFIG_MODE` macros, LDN numbers, vendor-specific register layouts |
| Datasheets: Winbond/Nuvoton W83627HF, NCT6776D; ITE IT8718F, IT8786E-I; SMSC LPC47M192, SCH322x; Fintek F71889 | Per-chip config registers, KBC firmware quirks, wake/password registers, A20/port-92 specifics |
| Per-vendor config-mode unlock sequences (typically `0x87 0x87` to port 0x2E for Winbond; `0x87 0x01 0x55 0x55` for ITE; etc.) | Stage 0 / pico1284 chipset identification when PnP enumeration unavailable |

Only universal SuperIO-specific concern for vintage-kvm Stage 0 is **A20 gate** via `0x64` cmd `0xD1` if real-mode addressing wraps at 1 MB matters; otherwise SuperIO == PS/2 from the Pico's perspective.

## How to apply

- **Pico firmware structure:** two PIO programs (`ps2_xt_dev.pio` for XT-class hosts, `ps2_at_dev.pio` for AT/PS/2/SuperIO). Rust state machines layered on top: `ps2_kbd_state.rs` (BAT, ACK, LED-set unlock detector, typematic), `ps2_mouse_state.rs` (3-byte packets + IntelliMouse 4-byte mode unlock). Host class chosen by config GPIO or persisted flash flag, mirroring `No0ne/ps2pico`'s separate-UF2 model.
- **DOS Stage 0 variants:** keep `s0_xt.asm` as-is (LPT-only, correct for XT). `s0_at.asm` adds i8042 mastery via the pattern above. `s0_ps2.asm` extends the AT path with AUX channel support for `docs/design.md` §17 dual-lane fallback. SuperIO uses `s0_ps2.asm` unchanged.
- **§7.4 LED-pattern unlock works on AT+, never on XT** — XT keyboard line is hardware-unidirectional. On XT, the bidirectional channel is exclusively LPT (which is what `s0_xt.asm` already implements).
- **Scancode set choice:** emit Set 2 from AT/PS/2/SuperIO; 8042's translate mode (CCB bit 6) handles software that expects Set 1. Emit Set 1 from XT (no choice — XT keyboards only speak Set 1).
- **System detection at boot:** BIOS data area `0040:0010` + `INT 11h` for mouse presence; probe port `0x64` for XT-vs-AT discrimination; probe AUX (`OUT 64h, 0A8h`) for AT-vs-PS/2.
- **For implementation, port `ps2x2pico`'s C state machines (`ps2kb.c`, `ps2ms.c`) to Rust/embassy-rp.** The PIO programs (`ps2out.pio`, `ps2in.pio`) can be near-verbatim ports — RP2350 PIO is a superset of RP2040 PIO.

## Related documents

- [`design.md`](design.md) — full merged design (packet format, capability handshake, compression, VESA, roadmap phases 0–11)
- [`stage0_design.md`](stage0_design.md) — detailed design for the three Stage 0 variants referenced in this table
- [`hardware_reference.md`](hardware_reference.md) — Feather pinout, 74LVC07A PS/2 buffer, 74LVC161284 IEEE 1284 transceiver, BOM
- [`ieee1284_controller_reference.md`](ieee1284_controller_reference.md) — sibling reference for the parallel-port side (Linux `uss720.c` + `parport/ieee1284.c`); same pattern of "controller-side code is the dual of what the Pico's peripheral must mirror"
