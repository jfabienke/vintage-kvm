//! Red-LED (GP7) heartbeat task.
//!
//! Visible "the firmware is still scheduling" indicator separate from the
//! NeoPixel state machine. 50 ms flash every 1 s. Cheap insurance that a
//! wedged protocol task is visible at a glance.

use embassy_rp::gpio::Output;
use embassy_time::Timer;

#[embassy_executor::task]
pub async fn run(mut led: Output<'static>) {
    loop {
        led.set_high();
        Timer::after_millis(50).await;
        led.set_low();
        Timer::after_millis(950).await;
    }
}
