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

    // PS/2 KBD wire oversampler — passive, always-on. PIO1 SM0, GP2/3/4.
    // Phase 1 bringup: counters update; frame extractor lands next.
    spawner.spawn(
        ps2::oversampler::run(p.PIO1, p.PIN_2, p.PIN_3, p.PIN_4)
            .expect("spawn ps2 kbd oversampler"),
    );

    // LPT forward path (DOS → Pico) via PIO0 SM0. Pin allocation per
    // `docs/hardware_reference.md` §3.3 and `docs/pio_state_machines_design.md`
    // §10.1: IN_BASE=GP11 (host strobe, currently nInit — flagged TBD in
    // pio_state_machines_design.md §4.4), GP12..GP19 = D0..D7.
    let compat_in = PioCompatIn::new(
        p.PIO0, p.PIN_11, p.PIN_12, p.PIN_13, p.PIN_14, p.PIN_15, p.PIN_16, p.PIN_17, p.PIN_18,
        p.PIN_19,
    );

    // Status outputs (peripheral-driven) — nibble + phase. Still bit-bang
    // pending `lpt_nibble_out` PIO.
    //   bit 0 (LSB) → status[3] = nFault  = GP27
    //   bit 1       → status[4] = Select  = GP26
    //   bit 2       → status[5] = PError  = GP25
    //   bit 3 (MSB) → status[6] = nAck    = GP23
    //   phase       → status[7] = Busy    = GP24
    let nibble_bit0 = Output::new(p.PIN_27, Level::Low);
    let nibble_bit1 = Output::new(p.PIN_26, Level::Low);
    let nibble_bit2 = Output::new(p.PIN_25, Level::Low);
    let nibble_bit3 = Output::new(p.PIN_23, Level::Low);
    let phase = Output::new(p.PIN_24, Level::Low);

    let phy = SppNibblePhy::new(
        compat_in,
        nibble_bit0,
        nibble_bit1,
        nibble_bit2,
        nibble_bit3,
        phase,
    );

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
