//! vintage-kvm firmware — Phase 3+ MVP.
//!
//! Serves DOS Stage 1 v1.0 over the IEEE 1284 SPP-nibble bootstrap channel:
//! `CAP_REQ` → `CAP_RSP`, `PING` → `PONG`, and `SEND_BLOCK` → `RECV_BLOCK`
//! for the embedded Stage 2 placeholder.
//!
//! Module layering follows `docs/firmware_crate_and_trait_design.md`:
//!
//! ```text
//! main         (this file: spawn tasks, wire layers)
//!   │
//!   ├── status::heartbeat            GP7 red-LED life indicator
//!   │
//!   └── serve_loop                   Phase-3 dispatcher loop
//!         │
//!         ├── transport::LptTransport<SppNibblePhy, DefmtEmit>
//!         │     ├── lpt::compat::SppNibblePhy      (LptPhy impl)
//!         │     └── transport::packet_stream::PacketReassembler
//!         │
//!         ├── protocol::SessionState
//!         │     └── protocol::stage_blobs::EmbeddedStage2 (BlockSource impl)
//!         │
//!         └── telemetry::DefmtEmit                 (TelemetryEmit impl)
//! ```

#![no_std]
#![no_main]

use defmt::info;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::pio::Pio;
use {defmt_rtt as _, panic_probe as _};

mod lifecycle;
mod lpt;
mod protocol;
mod ps2;
mod status;
mod telemetry;
mod transport;
mod util;

use lifecycle::SupervisorState;
use lpt::compat::SppNibblePhy;
use lpt::pio_compat_in::PioCompatIn;
use lpt::pio_nibble_out::PioNibbleOut;
use protocol::{handle_packet, DispatchOutcome, SessionState};
use telemetry::{DefmtEmit, Event, TelemetryEmit};
use transport::{LptTransport, Transport};

#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 4] = [
    embassy_rp::binary_info::rp_program_name!(c"vintage-kvm firmware"),
    embassy_rp::binary_info::rp_program_description!(
        c"Pico1284 RP2350 bridge — Phase 3 MVP (SPP-nibble + CAP/PING/block server)"
    ),
    embassy_rp::binary_info::rp_cargo_version!(),
    embassy_rp::binary_info::rp_program_build_attribute!(),
];

const FW_VERSION: &str = env!("CARGO_PKG_VERSION");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // Telemetry sink lives for the whole program. DefmtEmit is zero-sized so
    // we copy it freely into tasks that need to emit.
    let telemetry = DefmtEmit;
    telemetry.emit(Event::Boot {
        fw_version: FW_VERSION,
        phase: 3,
    });
    info!(
        "stage2 placeholder: {} bytes, CRC-32 = 0x{:08X}",
        protocol::stage_blobs::STAGE2_SIZE,
        protocol::stage_blobs::stage2_crc32()
    );

    // GP7 = on-board red LED (status heartbeat).
    let led = Output::new(p.PIN_7, Level::Low);
    spawner.spawn(status::heartbeat::run(led).expect("spawn heartbeat"));

    // GP21 = NeoPixel WS2812 (visible supervisor lifecycle indicator).
    // Owns PIO2 SM0 + DMA_CH0 per docs/pico_firmware_design.md §4.2.
    spawner
        .spawn(status::neopixel::run(p.PIO2, p.PIN_21, p.DMA_CH0).expect("spawn neopixel"));

    // PIO1 hosts both PS/2 KBD state machines:
    //   SM0 = ps2_kbd_oversample  (1 MS/s wire instrumentation)
    //   SM1 = ps2_kbd_tx          (device→host frame emitter)
    // GP3 (CLK_PULL) is read by SM0 for instrumentation AND driven by SM1
    // when emitting a frame — that's intentional per
    // docs/pio_state_machines_design.md §6.1.
    let Pio {
        common: mut pio1_common,
        sm0: pio1_sm0,
        sm1: pio1_sm1,
        ..
    } = Pio::new(p.PIO1, ps2::Pio1Irqs);
    let kbd_clk_in = pio1_common.make_pio_pin(p.PIN_2);
    let kbd_clk_pull = pio1_common.make_pio_pin(p.PIN_3);
    let kbd_data_in = pio1_common.make_pio_pin(p.PIN_4);
    let kbd_data_pull = pio1_common.make_pio_pin(p.PIN_5);

    let oversampler = ps2::oversampler::KbdOversampler::new(
        &mut pio1_common,
        pio1_sm0,
        &kbd_clk_in,
        &kbd_clk_pull,
        &kbd_data_in,
    );
    let _kbd_tx = ps2::tx_kbd::KbdTx::new(
        &mut pio1_common,
        pio1_sm1,
        &kbd_clk_pull,
        &kbd_data_pull,
    );

    spawner.spawn(ps2::oversampler::run(oversampler).expect("spawn ps2 kbd oversampler"));

    // PIO0 hosts both LPT SPP-nibble state machines:
    //   SM0 = lpt_compat_in   (forward: host → Pico, 9-bit capture)
    //   SM1 = lpt_nibble_out  (reverse: Pico → host, 2 × 5-bit nibble)
    // Pin allocation per `docs/hardware_reference.md` §3.3 and
    // `docs/pio_state_machines_design.md` §10. Host strobe is currently
    // assumed to land on GP11 (nInit routing TBD; see pio_state_machines
    // _design.md §4.4).
    let Pio {
        mut common,
        sm0,
        sm1,
        ..
    } = Pio::new(p.PIO0, lpt::Pio0Irqs);

    let compat_in = PioCompatIn::new(
        &mut common,
        sm0,
        p.PIN_11,
        p.PIN_12,
        p.PIN_13,
        p.PIN_14,
        p.PIN_15,
        p.PIN_16,
        p.PIN_17,
        p.PIN_18,
        p.PIN_19,
    );

    let nibble_out = PioNibbleOut::new(
        &mut common,
        sm1,
        p.PIN_23, // nAck    (nibble bit 3)
        p.PIN_24, // Busy    (phase)
        p.PIN_25, // PError  (nibble bit 2)
        p.PIN_26, // Select  (nibble bit 1)
        p.PIN_27, // nFault  (nibble bit 0)
    );

    let phy = SppNibblePhy::new(compat_in, nibble_out);

    let transport = LptTransport::new(phy, DefmtEmit);

    // Phase 3 jumps straight into CAP handshake — no PS/2 detect / DEBUG
    // injection yet. The LED flips to magenta-blink once SEND_BLOCK traffic
    // starts; left as a TODO until the dispatcher emits per-block events.
    lifecycle::set(SupervisorState::ServeCapHandshake);

    info!("LPT SPP-nibble transport ready; entering serve loop");
    serve_loop(transport).await;
}

/// Run forever: receive a packet, dispatch, send any reply, repeat.
///
/// Phase 3 makes no attempt at recovery beyond what `PacketReassembler`
/// already does (resync on bad SOH / CRC / ETX). The telemetry stream is
/// the diagnostic surface for unhealthy wires.
async fn serve_loop<T: Transport>(mut transport: T) -> ! {
    let mut state = SessionState::new();
    let telemetry = DefmtEmit;

    loop {
        let pkt = match transport.recv_packet().await {
            Ok(p) => p,
            Err(_) => continue, // packet_stream already emitted a resync event
        };

        info!(
            "rx cmd=0x{:02X} seq={} payload={}B",
            pkt.cmd,
            pkt.seq,
            pkt.payload.len()
        );

        match handle_packet(&pkt, &mut state, &telemetry) {
            DispatchOutcome::Reply(out) => {
                let _ = transport.send_packet(&out).await;
            }
            DispatchOutcome::Silent | DispatchOutcome::Ignored => {}
        }
    }
}
