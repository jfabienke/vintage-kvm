//! vintage-kvm firmware — Phase 3+ MVP.
//!
//! Serves Stage 1 v1.0 over the IEEE 1284 SPP-nibble bootstrap channel:
//! CAP_REQ → CAP_RSP, PING → PONG, and SEND_BLOCK → RECV_BLOCK for the
//! embedded Stage 2 placeholder.
//!
//! Phase 0's GP7 red-LED blink is preserved as a heartbeat on a separate
//! task, so a wedged protocol task is visible at a glance.
//!
//! See `docs/pico_phase3_design.md` for design + bring-up sequence.

#![no_std]
#![no_main]

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_time::Timer;
use {defmt_rtt as _, panic_probe as _};

mod lifecycle;
mod lpt;
mod packet_stream;
mod protocol;

use lpt::compat::SppNibblePhy;
use packet_stream::PacketReassembler;
use protocol::{handle_packet, DispatchOutcome, SessionState};

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

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    info!("vintage-kvm: Phase 3 MVP starting");
    info!(
        "stage2 placeholder: {} bytes, CRC-32 = 0x{:08X}",
        protocol::stage_blobs::STAGE2_SIZE,
        protocol::stage_blobs::stage2_crc32()
    );

    // GP7 = on-board red LED (status heartbeat).
    let led = Output::new(p.PIN_7, Level::Low);
    spawner.spawn(heartbeat(led).expect("spawn heartbeat"));

    // LPT pin allocation per `docs/hardware_reference.md` §3.3 and
    // `docs/pico_phase3_design.md`. nInit (host strobe) routing TBD;
    // GP11 used as the placeholder for now (flagged in lpt::compat).
    let host_strobe = Input::new(p.PIN_11, Pull::Up);

    let data: [Input<'static>; 8] = [
        Input::new(p.PIN_12, Pull::None),
        Input::new(p.PIN_13, Pull::None),
        Input::new(p.PIN_14, Pull::None),
        Input::new(p.PIN_15, Pull::None),
        Input::new(p.PIN_16, Pull::None),
        Input::new(p.PIN_17, Pull::None),
        Input::new(p.PIN_18, Pull::None),
        Input::new(p.PIN_19, Pull::None),
    ];

    // Status outputs (peripheral-driven) — nibble + phase.
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
        data,
        host_strobe,
        nibble_bit0,
        nibble_bit1,
        nibble_bit2,
        nibble_bit3,
        phase,
    );

    info!("LPT SPP-nibble phy ready; entering serve loop");
    serve_loop(phy).await;
}

#[embassy_executor::task]
async fn heartbeat(mut led: Output<'static>) {
    loop {
        led.set_high();
        Timer::after_millis(50).await;
        led.set_low();
        Timer::after_millis(950).await;
    }
}

/// Run forever: receive a packet, dispatch, send any reply, repeat.
///
/// Phase 3 makes no attempt at recovery beyond what `PacketReassembler`
/// already does (resync on bad SOH / CRC / ETX). Persistent timeouts /
/// faults will surface as defmt warnings.
async fn serve_loop(mut phy: SppNibblePhy) -> ! {
    let mut reassembler = PacketReassembler::new();
    let mut state = SessionState::new();

    loop {
        let pkt = reassembler.next_packet(&mut phy).await;
        info!(
            "rx cmd=0x{:02X} seq={} payload={}B",
            pkt.cmd,
            pkt.seq,
            pkt.payload.len()
        );

        match handle_packet(&pkt, &mut state) {
            DispatchOutcome::Reply(bytes) => {
                for &b in bytes.iter() {
                    if let Err(e) = phy.send_byte(b).await {
                        warn!("LPT tx error: {}", e);
                        break;
                    }
                }
            }
            DispatchOutcome::Silent => {}
            DispatchOutcome::Ignored => {}
        }
    }
}
