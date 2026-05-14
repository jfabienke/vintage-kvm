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
use embassy_rp::pio::Pio;
use {defmt_rtt as _, panic_probe as _};

mod crc_sniffer;
mod irqs;
mod lifecycle;
mod lpt;
mod protocol;
mod ps2;
mod status;
mod telemetry;
mod transport;
mod usb;
mod util;

use crc_sniffer::DmaSniffer;
use lifecycle::SupervisorState;
use lpt::compat::SppNibblePhy;
use lpt::pio_compat_in::PioCompatIn;
use lpt::pio_nibble_out::PioNibbleOut;
use protocol::stage_blobs::{EmbeddedStage2, STAGE2_PLACEHOLDER};
use ps2::tx::{AuxTx, KbdTx};
use protocol::{handle_packet, DispatchOutcome, SessionState};
use telemetry::{Event, TelemetryEmit, TELEMETRY};
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

    // Telemetry sink lives for the whole program. `TELEMETRY` is a
    // zero-sized const composite of `DefmtEmit + UsbEmit`, so events
    // land on both the dev probe and the host's events-CDC stream.
    TELEMETRY.emit(Event::Boot {
        fw_version: FW_VERSION,
        phase: 3,
    });

    // DMA-sniffer CRC engines (CRC-32 + CRC-16 over DMA_CH5). Run the
    // standard test vectors at boot so a misconfigured sniffer fails
    // loudly, then compute the Stage 2 image CRC via the same hardware
    // path.
    let mut sniffer = DmaSniffer::new(p.DMA_CH5);
    let crc32_selftest = sniffer.compute_crc32(b"123456789");
    defmt::assert_eq!(
        crc32_selftest,
        0xCBF43926u32,
        "CRC-32 sniffer self-test failed; expected 0xCBF43926, got {:#010X}",
        crc32_selftest
    );
    let crc16_selftest = sniffer.compute_crc16(b"123456789");
    defmt::assert_eq!(
        crc16_selftest,
        0x29B1u16,
        "CRC-16 sniffer self-test failed; expected 0x29B1, got {:#06X}",
        crc16_selftest
    );
    let stage2_crc = sniffer.compute_crc32(STAGE2_PLACEHOLDER);
    let stage2_blob = EmbeddedStage2::with_crc(stage2_crc);
    info!(
        "stage2 placeholder: {} bytes, CRC-32 = 0x{:08X} (hw)",
        protocol::stage_blobs::STAGE2_SIZE,
        stage2_crc
    );

    // GP21 = NeoPixel WS2812 (visible supervisor lifecycle indicator).
    // Owns PIO2 SM0 + DMA_CH0 per docs/pico_firmware_design.md §4.2.
    // We rely on the NeoPixel for visual status; the old GP7 red-LED
    // heartbeat was dropped to free GP7 for the AUX oversampler's
    // width-4 sample window (GP6/7/8/9).
    spawner
        .spawn(status::neopixel::run(p.PIO2, p.PIN_21, p.DMA_CH0).expect("spawn neopixel"));

    // USB composite device — Phase 4a/4b: CDC ACM `events` (IN-only
    // telemetry) + CDC ACM `control` (bidirectional RPC). The
    // `console` and vendor-bulk interfaces will land on top of the
    // same Builder in later phases.
    let usb_stack = usb::build(p.USB);
    spawner.spawn(usb::run_device(usb_stack.device).expect("spawn usb device"));
    spawner.spawn(usb::events::run(usb_stack.events).expect("spawn usb events writer"));
    spawner.spawn(usb::control::run(usb_stack.control).expect("spawn usb control"));

    // PIO1 hosts all four PS/2 state machines:
    //   SM0 = ps2_kbd_oversample  (KBD wire instrumentation, GP2/3/4)
    //   SM1 = ps2_kbd_tx          (KBD TX, GP3=CLK_PULL, GP5=DATA_PULL)
    //   SM2 = ps2_aux_oversample  (AUX wire instrumentation, GP6/7/8/9)
    //   SM3 = ps2_aux_tx          (AUX TX, GP28=CLK_PULL, GP10=DATA_PULL)
    // GP3 and GP28 (CLK_PULL pins) are read by the oversamplers for
    // instrumentation AND driven by the TX SMs — intentional per
    // docs/pio_state_machines_design.md §6.1.
    let Pio {
        common: mut pio1_common,
        sm0: pio1_sm0,
        sm1: pio1_sm1,
        sm2: pio1_sm2,
        sm3: pio1_sm3,
        ..
    } = Pio::new(p.PIO1, ps2::Pio1Irqs);
    let kbd_clk_in = pio1_common.make_pio_pin(p.PIN_2);
    let kbd_clk_pull = pio1_common.make_pio_pin(p.PIN_3);
    let kbd_data_in = pio1_common.make_pio_pin(p.PIN_4);
    let kbd_data_pull = pio1_common.make_pio_pin(p.PIN_5);
    let aux_clk_in = pio1_common.make_pio_pin(p.PIN_6);
    let aux_gap0 = pio1_common.make_pio_pin(p.PIN_7);
    let aux_gap1 = pio1_common.make_pio_pin(p.PIN_8);
    let aux_data_in = pio1_common.make_pio_pin(p.PIN_9);
    let aux_data_pull = pio1_common.make_pio_pin(p.PIN_10);
    let aux_clk_pull = pio1_common.make_pio_pin(p.PIN_28);

    let kbd_oversampler = ps2::oversampler::KbdOversampler::new(
        &mut pio1_common,
        pio1_sm0,
        &kbd_clk_in,
        &kbd_clk_pull,
        &kbd_data_in,
        p.DMA_CH1,
    );
    let kbd_tx = KbdTx::new(
        &mut pio1_common,
        pio1_sm1,
        &kbd_clk_pull,
        &kbd_data_pull,
        "kbd",
    );
    let aux_oversampler = ps2::aux_oversampler::AuxOversampler::new(
        &mut pio1_common,
        pio1_sm2,
        &aux_clk_in,
        &aux_gap0,
        &aux_gap1,
        &aux_data_in,
        p.DMA_CH2,
    );
    let _aux_tx = AuxTx::new(
        &mut pio1_common,
        pio1_sm3,
        &aux_clk_pull,
        &aux_data_pull,
        "aux",
    );

    let injector = ps2::injector::BootstrapInjector::new(kbd_tx);

    spawner.spawn(ps2::oversampler::run(kbd_oversampler).expect("spawn ps2 kbd oversampler"));
    spawner.spawn(ps2::aux_oversampler::run(aux_oversampler).expect("spawn ps2 aux oversampler"));
    spawner.spawn(ps2::supervisor::run().expect("spawn ps2 supervisor"));
    spawner.spawn(ps2::injector::run(injector).expect("spawn ps2 injector"));

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
        p.DMA_CH3,
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
        p.DMA_CH4,
        p.PIN_23, // nAck    (nibble bit 3)
        p.PIN_24, // Busy    (phase)
        p.PIN_25, // PError  (nibble bit 2)
        p.PIN_26, // Select  (nibble bit 1)
        p.PIN_27, // nFault  (nibble bit 0)
    );

    let phy = SppNibblePhy::new(compat_in, nibble_out);

    let transport = LptTransport::new(phy, TELEMETRY);

    // Phase 3 jumps straight into CAP handshake — no PS/2 detect / DEBUG
    // injection yet. The LED flips to magenta-blink once SEND_BLOCK traffic
    // starts; left as a TODO until the dispatcher emits per-block events.
    lifecycle::set(SupervisorState::ServeCapHandshake);

    info!("LPT SPP-nibble transport ready; entering serve loop");
    serve_loop(transport, stage2_blob).await;
}

/// Run forever: receive a packet, dispatch, send any reply, repeat.
///
/// Phase 3 makes no attempt at recovery beyond what `PacketReassembler`
/// already does (resync on bad SOH / CRC / ETX). The telemetry stream is
/// the diagnostic surface for unhealthy wires.
async fn serve_loop<T: Transport>(mut transport: T, blob: EmbeddedStage2) -> ! {
    let mut state = SessionState::with_blob(blob);
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

        match handle_packet(&pkt, &mut state, &TELEMETRY) {
            DispatchOutcome::Reply(out) => {
                let _ = transport.send_packet(&out).await;
            }
            DispatchOutcome::Silent | DispatchOutcome::Ignored => {}
        }
    }
}
