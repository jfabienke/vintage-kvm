//! Persistent LPT hardware ownership for the mode-swap path.
//!
//! Per `docs/pio_state_machines_design.md` §10.6 a mode transition is
//! "drain → disable → unload → load → reconfigure → enable", not
//! "drop and rebuild from raw peripherals". The Pico peripherals
//! (Common, state machines, DMA channels, GPIOs) only get consumed
//! once at boot — they have to live somewhere that outlasts any one
//! active phy.
//!
//! [`LptHardware`] is that home. It holds:
//!
//! - The PIO0 `Common`, used by every phy to load programs and bind
//!   pins.
//! - 14 `Pin<'static, PIO0>` handles — strobe + D0..D7 (forward) and
//!   the 5 reverse status pins. `make_pio_pin` is one-way (consumes
//!   `Peri<PIN_*>`, returns a `Pin` that lives for the program's
//!   duration), so we do all 14 once at boot.
//! - Parked SM0/SM1 + the nibble-out DMA channel and a Peri token for
//!   the compat-in ring DMA. Phys `take()` these on construction and
//!   `dismantle()` returns them to the parking lot.
//!
//! Phys hold whatever `LoadedProgram` handles they need; on dismantle
//! they pass `LoadedProgram::used_memory` back through
//! `unsafe { common.free_instr(...) }` so the instruction-memory
//! slots are recycled and the next mode's programs fit.

use embassy_rp::Peri;
use embassy_rp::dma;
use embassy_rp::peripherals::{
    DMA_CH3, DMA_CH4, PIN_11, PIN_12, PIN_13, PIN_14, PIN_15, PIN_16, PIN_17, PIN_18, PIN_19,
    PIN_20, PIN_22, PIN_23, PIN_24, PIN_25, PIN_26, PIN_27, PIO0,
};
use embassy_rp::pio::{Common, Pin, StateMachine};

use crate::irqs::DmaIrqs;

/// The 16 PIO0-mapped pins the LPT side uses across all 1284 modes.
/// Made once at boot so mode swaps don't have to deal with un-mapping
/// or re-mapping pins (embassy-rp has no `release_pio_pin` — pins
/// return to GPIO only when the owning `Common` itself drops).
///
/// Field names follow `docs/hardware_reference.md` §3.3 and are
/// signal-multiplexed across modes:
/// - `strobe`  GP11 = nStrobe / HostClk
/// - `d0..d7`  GP12..19 = Parallel Data 0..7 (bidirectional)
/// - `auto_fd` GP20 = nAutoFd / HostAck / nDataStb
/// - `slctin`  GP22 = nSelectIn / 1284Active
/// - `nack`    GP23 = nAck / PeriphClk / nAckReverse
/// - `busy`    GP24 = Busy / PeriphAck / nWait
/// - `perror`  GP25 = PError / nAckReverse
/// - `select`  GP26 = Select / Xflag
/// - `nfault`  GP27 = nFault / nPeriphRequest
pub struct LptPins {
    pub strobe: Pin<'static, PIO0>,
    pub d0: Pin<'static, PIO0>,
    pub d1: Pin<'static, PIO0>,
    pub d2: Pin<'static, PIO0>,
    pub d3: Pin<'static, PIO0>,
    pub d4: Pin<'static, PIO0>,
    pub d5: Pin<'static, PIO0>,
    pub d6: Pin<'static, PIO0>,
    pub d7: Pin<'static, PIO0>,
    pub auto_fd: Pin<'static, PIO0>,
    // GP22 = nSelectIn / 1284Active / EPP nAddrStb. Reserved for
    // future EPP address-cycle support and for asserting 1284Active
    // during negotiation; no current consumer.
    #[allow(dead_code)]
    pub slctin: Pin<'static, PIO0>,
    pub nack: Pin<'static, PIO0>,
    pub busy: Pin<'static, PIO0>,
    pub perror: Pin<'static, PIO0>,
    pub select: Pin<'static, PIO0>,
    pub nfault: Pin<'static, PIO0>,
}

pub struct LptHardware {
    pub common: Common<'static, PIO0>,
    pub pins: LptPins,
    pub parked_sm0: Option<StateMachine<'static, PIO0, 0>>,
    pub parked_sm1: Option<StateMachine<'static, PIO0, 1>>,
    /// `dma::Channel` for the nibble-out path. Phys borrow ownership
    /// of it for their lifetime and hand it back via `dismantle`.
    pub parked_dma_ch4: Option<dma::Channel<'static>>,
    /// Compat-in ring DMA token. Operated entirely through PAC writes
    /// in `super::ring_dma`; held here for compile-time exclusivity.
    /// `Option` only so initial construction can move it in.
    _dma_ch3: Option<Peri<'static, DMA_CH3>>,
}

impl LptHardware {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut common: Common<'static, PIO0>,
        sm0: StateMachine<'static, PIO0, 0>,
        sm1: StateMachine<'static, PIO0, 1>,
        dma_ch3: Peri<'static, DMA_CH3>,
        dma_ch4: Peri<'static, DMA_CH4>,
        strobe: Peri<'static, PIN_11>,
        d0: Peri<'static, PIN_12>,
        d1: Peri<'static, PIN_13>,
        d2: Peri<'static, PIN_14>,
        d3: Peri<'static, PIN_15>,
        d4: Peri<'static, PIN_16>,
        d5: Peri<'static, PIN_17>,
        d6: Peri<'static, PIN_18>,
        d7: Peri<'static, PIN_19>,
        auto_fd: Peri<'static, PIN_20>,
        slctin: Peri<'static, PIN_22>,
        nack: Peri<'static, PIN_23>,
        busy: Peri<'static, PIN_24>,
        perror: Peri<'static, PIN_25>,
        select: Peri<'static, PIN_26>,
        nfault: Peri<'static, PIN_27>,
    ) -> Self {
        let pins = LptPins {
            strobe: common.make_pio_pin(strobe),
            d0: common.make_pio_pin(d0),
            d1: common.make_pio_pin(d1),
            d2: common.make_pio_pin(d2),
            d3: common.make_pio_pin(d3),
            d4: common.make_pio_pin(d4),
            d5: common.make_pio_pin(d5),
            d6: common.make_pio_pin(d6),
            d7: common.make_pio_pin(d7),
            auto_fd: common.make_pio_pin(auto_fd),
            slctin: common.make_pio_pin(slctin),
            nack: common.make_pio_pin(nack),
            busy: common.make_pio_pin(busy),
            perror: common.make_pio_pin(perror),
            select: common.make_pio_pin(select),
            nfault: common.make_pio_pin(nfault),
        };

        let dma_ch4_channel = dma::Channel::new(dma_ch4, DmaIrqs);

        Self {
            common,
            pins,
            parked_sm0: Some(sm0),
            parked_sm1: Some(sm1),
            parked_dma_ch4: Some(dma_ch4_channel),
            _dma_ch3: Some(dma_ch3),
        }
    }
}
