# tools/

Dev fixtures and protocol tooling for vintage-kvm. Not part of the production build chain — each tool is independently buildable and justified individually.

Planned tools:

- **`delock-fixture/`** — libusb client claiming PL2305 (DeLock USB 1.1 adapter) as a Mac/Linux-side fake LPT host for testing Pico Stage 0 firmware without a DOS PC.
- **`packet-dissector/`** — Wireshark Lua dissector for the SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX framing.
- **`capture-replay/`** — Saleae-compatible PS/2 line capture/replay for protocol debugging.
- **`bring-up/`** — per-Feather smoke tests beyond the Phase 0 LED blink.

**Status:** Not started. First useful tool is `delock-fixture` for Phase 3 dev work.

Detailed plan: [`../docs/implementation_plan.md`](../docs/implementation_plan.md) §7.
