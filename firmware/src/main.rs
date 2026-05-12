//! vintage-kvm firmware — Phase 0 smoke test.
//!
//! Blinks the on-board red LED on GP7 of the Adafruit Feather RP2350 HSTX.
//! Confirms toolchain, linker script, embassy executor, and time driver
//! are wired correctly before any real interface work begins.
//!
//! The NeoPixel on GP21 is the project's runtime status indicator (per
//! `docs/hardware_reference.md` §6), but it needs a PIO WS2812 driver
//! that doesn't belong in a Phase 0 smoke test. The red LED is plain
//! GPIO and sufficient here.
//!
//! See `docs/design.md` §22 Phase 0 and `docs/hardware_reference.md` §6
//! for the full pin allocation.

#![no_std]
#![no_main]

use defmt::info;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_time::Timer;
use {defmt_rtt as _, panic_probe as _};

#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 4] = [
    embassy_rp::binary_info::rp_program_name!(c"vintage-kvm firmware"),
    embassy_rp::binary_info::rp_program_description!(
        c"Pico1284 RP2350 bridge — Phase 0 red-LED smoke test on GP7 (Feather)"
    ),
    embassy_rp::binary_info::rp_cargo_version!(),
    embassy_rp::binary_info::rp_program_build_attribute!(),
];

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut led = Output::new(p.PIN_7, Level::Low);

    info!("vintage-kvm: phase 0 blink on GP7 (Feather red LED)");

    loop {
        led.set_high();
        Timer::after_millis(250).await;
        led.set_low();
        Timer::after_millis(250).await;
    }
}
