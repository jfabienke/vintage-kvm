# hardware/

KiCad schematics and (later) PCB for the vintage-kvm carrier board: Feather RP2350 HSTX + 74LVC161284 (IEEE 1284 transceiver) + 74LVC07A (PS/2 open-drain buffer) + DB-25 + 2× mini-DIN + USB-C.

**Status:** Not started.

Sequencing:
1. Schematic-only KiCad project mirroring [`../docs/hardware_reference.md`](../docs/hardware_reference.md) §3.3 + §6 pinout.
2. Protoboard layout (through-hole + minimum SMT) for hand-assembly.
3. Integrated SMT carrier board (optional / future).

Detailed plan: [`../docs/implementation_plan.md`](../docs/implementation_plan.md) §8.
