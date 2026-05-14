# tools/

Dev fixtures and protocol tooling for vintage-kvm. Not part of the production build chain — each tool is independently buildable and justified individually.

Tools landed:

- **`events-consumer/`** — host-side reader for the firmware's `events` CDC ACM stream. Auto-detects the Pico by VID/PID, decodes postcard + COBS frames into `Event` values, prints one line per event with a wall-clock timestamp. Run with `cargo run -p vintage-kvm-events-consumer` (auto-detect) or pass `/dev/cu.usbmodemNNN` explicitly.
- **`control-client/`** — host-side one-shot CLI for the firmware's `control` CDC ACM RPC channel. Sends one verb invocation per run; verb grammar lives in the firmware so additions don't need a client bump. Auto-detects the second CDC (events is the first, control is the second). Passing `--json` switches stdout to a single structured JSON object (per-verb parsed `data`, raw reply lines, error string), which scripts and LLM callers can parse without scraping text. Example: `cargo run -p vintage-kvm-control-client -- --json stats`.

Planned tools:

- **`delock-fixture/`** — libusb client claiming PL2305 (DeLock USB 1.1 adapter) as a Mac/Linux-side fake LPT host for testing Pico Stage 0 firmware without a DOS PC.
- **`packet-dissector/`** — Wireshark Lua dissector for the SOH/CMD/SEQ/LEN/PAYLOAD/CRC/ETX framing.
- **`capture-replay/`** — Saleae-compatible PS/2 line capture/replay for protocol debugging.
- **`bring-up/`** — per-Feather smoke tests beyond the Phase 0 LED blink.

Detailed plan: [`../docs/implementation_plan.md`](../docs/implementation_plan.md) §7.
