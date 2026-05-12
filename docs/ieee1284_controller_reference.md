# IEEE 1284 Controller Reference

**Status:** Reference document  
**Last updated:** 2026-05-12  
**Companion documents:** [`ps2_eras_reference.md`](ps2_eras_reference.md), [`two_plane_transport.md`](two_plane_transport.md) (IEEE 1284 is the data plane in the two-plane stack — this doc covers the L0/L1 host-side reference)

When designing the Pico's IEEE 1284 **peripheral** state machine ([`design.md`](design.md) §22 Phases 3–5), the controller side it inter-operates with is the same on every PC LPT chipset — NS PC87332, Winbond W83627, ITE IT87, USS-720, etc. all expose the standard 7-register Extended Capabilities interface at I/O base+0..+6. The Pico's peripheral has to be the wire-side dual of these registers.

## Canonical reference files in the Linux kernel

| File | What it gives you | Best read during |
|---|---|---|
| `drivers/usb/misc/uss720.c` (~830 lines, Thomas Sailer, GPL-2.0+) | Clean controller-side implementation of every IEEE 1284 mode (Compat/Nibble/Byte/EPP/ECP). Shows register-access primitives, mode-transition rules, FIFO-drain handshake. | Phase 3–5 |
| `drivers/parport/ieee1284.c` + `ieee1284_ops.c` | The actual IEEE 1284 negotiation byte sequences — what bytes the controller writes to negotiate into each mode, and what status patterns it expects back from the peripheral. **The Pico must mirror these from the peripheral side.** | Phase 4 |
| `drivers/parport/parport_pc.c` | Real-PC-chipset controller implementation. Cross-reference for register-level behavior, timing assumptions, FIFO sizing. | Phase 5 |

## Standard PC LPT register layout (base+0 … base+6)

| Reg | Name | Pico-side observation |
|---|---|---|
| 0 | DATA | PD0–PD7 driven outward (forward) or read by controller (reverse). Pico drives these in reverse, reads them in forward. |
| 1 | STATUS | Pico drives bits [7:3]: Busy (7), nAck (6), PError (5), Select (4), nFault (3). |
| 2 | CONTROL | Pico observes bits [3:0]: nStrobe (0), nAutoFd (1), nInit (2), nSelectIn (3). Bit 4 = IRQ enable, bit 5 = direction (1 = reverse). |
| 3 | EPP_ADDR | EPP address-strobed cycle on nAutoFd |
| 4 | EPP_DATA | EPP data-strobed cycle on nStrobe |
| 5 | ECP_FIFO | ECP forward FIFO byte |
| 6 | **ECR** (Extended Control Register) | **Mode bits [7:5] tell the Pico which mode the controller is now in.** Bit 0 = FIFO empty. |

## ECR mode constants (use these names in the Pico firmware verbatim)

From `uss720.c` lines 248–252:

```
ECR_SPP = 0    standard parallel port (Compat mode, forward only)
ECR_PS2 = 1    PS/2 bidirectional byte mode (used as transition state)
ECR_PPF = 2    Parallel Port FIFO (fast Compat with FIFO)
ECR_ECP = 3    ECP
ECR_EPP = 4    EPP
```

Mode 5–7 are reserved/test on real chipsets. The Pico's mode tracker should default to SPP at reset and update on observing the controller's ECR writes (which appear on the wire as the controller transitioning through the negotiation handshake — the Pico won't see the ECR write directly, but will see the negotiation sequence that precedes it).

## Mode-transition rules the Pico must expect

From `uss720.c::change_mode()` (lines 257–301):

1. **No direct switching between high-tier modes (PPF/ECP/EPP).** The controller drops to PS/2 (mode 1) or SPP (mode 0) as an intermediate state, then negotiates into the target mode. The Pico's mode tracker must not treat PS/2 as the destination during a tier transition — it's a stepping stone.
2. **FIFO drain before switching out of ECP/PPF.** Controller polls ECR bit 0 (FIFO empty) before changing mode, with a deadline; if the Pico's ECP forward path still has unprocessed bytes, status must reflect "not empty" until they're consumed. Polling cadence in uss720 is 10 ms — plenty of headroom on Pico/PIO timescales.

## Per-mode bidirectional support in uss720 (for context if used as a Phase 3+ Linux dev fixture)

| Mode | Forward | Reverse | Notes |
|---|---|---|---|
| Compat | ✓ (`compat_write_data`, via bulk EP1) | n/a by spec | — |
| Nibble | n/a by spec | ✓ (generic `parport_ieee1284_read_nibble`) | — |
| Byte | n/a by spec | ✓ (generic `parport_ieee1284_read_byte`) | — |
| EPP | ✓ data + addr | ✓ data + addr | Fully bidirectional |
| ECP | ✓ data + addr | ✓ data only | **Missing `ecp_read_addr`, `ecp_write_block`, `ecp_read_block`** → byte-by-byte, no DMA fast path |

Interrupt handling is disabled in the driver (`"usb_request_irq crashes somewhere within ohci.c"` — line 322ish comment). Polling only.

## USB transport protocol (vendor-specific, in case we ever need to talk to a USS-720 adapter from libusb)

Two control requests + two bulk endpoints:

```
SET register:   bRequest=4, bmRequestType=0x40, wValue=(reg<<8)|val
READ all regs:  bRequest=3, bmRequestType=0xC0, wLength=7    → priv->reg[0..6]
Bulk OUT EP1:   ECP/EPP/PPF block writes
Bulk IN  EP2:   ECP block reads
```

Atomic 7-register snapshot read is the chip's main efficiency win over per-register access.

## How to apply

- **Borrow ECR mode constants and the 7-register naming verbatim** when writing the Pico's PIO programs and Rust state structs. Free interop with every parport reference in the world.
- **Build the Pico's peripheral state machine as the wire-side dual of uss720.c.** Every controller-side `set_1284_register(pp, N, val)` corresponds to "wire shows X, Pico observes and responds."
- **Honor the FIFO-drain-before-mode-switch contract** in the Pico's ECP forward path.
- **Read `drivers/parport/ieee1284.c` next** when Phase 4 starts — that's where the actual negotiation byte sequences live, which the Pico must respond to from the peripheral side.

## Related documents

- [`design.md`](design.md) — full merged design, especially §8 (IEEE 1284 negotiation and autodetection), §9 (packet format), §22 Phases 3–5 (LPT-side roadmap)
- [`hardware_reference.md`](hardware_reference.md) — 74LVC161284 transceiver choice, GPIO map (8-bit data bus on HSTX connector GP12–GP19, control/status on GP11/GP20/GP22–GP27)
- [`ps2_eras_reference.md`](ps2_eras_reference.md) — sibling reference for the PS/2 side; same pattern of "host-side code is the dual of what the Pico's device-side firmware must mirror"
