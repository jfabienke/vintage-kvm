//! NeoPixel (WS2812) status driver.
//!
//! Owns PIO2 SM0 and GP21 (per `docs/hardware_reference.md` §3.3 and
//! `docs/pico_firmware_design.md` §4.2). Reuses embassy-rp's
//! `pio_programs::ws2812` driver — that program is identical to the canonical
//! WS2812 PIO, and using it here doubles as a smoke test for the whole PIO
//! toolchain before we write our own LPT / PS/2 programs.
//!
//! The task polls `lifecycle::SUPERVISOR_STATE` on each animation step
//! rather than using a Signal/Channel because the supervisor side writes
//! infrequently and the read side already ticks at 60–250 ms anyway.

use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::{DMA_CH0, PIN_21, PIO2};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program};
use embassy_time::{Duration, Timer};
use smart_leds::RGB8;

use crate::irqs::DmaIrqs;
use crate::lifecycle;
use crate::status::animations::{self, Animation};

bind_interrupts!(struct Pio2Irqs {
    PIO2_IRQ_0 => PioInterruptHandler<PIO2>;
});

const OFF: RGB8 = RGB8 { r: 0, g: 0, b: 0 };

/// Resolution of the "is state still the same?" poll. Also caps the worst-
/// case latency between a `lifecycle::set()` call and the LED reflecting it.
const POLL_TICK: Duration = Duration::from_millis(50);

/// NeoPixel driver task. Loads the WS2812 PIO program onto PIO2 SM0,
/// then drives GP21 from the supervisor state cell forever.
#[embassy_executor::task]
pub async fn run(
    pio: Peri<'static, PIO2>,
    pin: Peri<'static, PIN_21>,
    dma: Peri<'static, DMA_CH0>,
) {
    let Pio { mut common, sm0, .. } = Pio::new(pio, Pio2Irqs);
    let program = PioWs2812Program::new(&mut common);
    let mut ws: PioWs2812<'_, PIO2, 0, 1, _> =
        PioWs2812::new(&mut common, sm0, dma, DmaIrqs, pin, &program);

    let mut state = lifecycle::get();
    let mut anim = animations::for_state(state);
    let mut phase_on = true;
    write_color(&mut ws, color_for_phase(&anim, phase_on)).await;
    let mut remaining = phase_duration(&anim, phase_on);

    loop {
        let tick = remaining.min(POLL_TICK);
        Timer::after(tick).await;
        remaining = remaining.checked_sub(tick).unwrap_or(Duration::from_ticks(0));

        let now = lifecycle::get();
        if now != state {
            state = now;
            anim = animations::for_state(state);
            phase_on = true;
            write_color(&mut ws, color_for_phase(&anim, phase_on)).await;
            remaining = phase_duration(&anim, phase_on);
            continue;
        }

        if remaining == Duration::from_ticks(0) {
            phase_on = !phase_on;
            write_color(&mut ws, color_for_phase(&anim, phase_on)).await;
            remaining = phase_duration(&anim, phase_on);
        }
    }
}

async fn write_color<O>(ws: &mut PioWs2812<'_, PIO2, 0, 1, O>, color: RGB8)
where
    O: embassy_rp::pio_programs::ws2812::RgbColorOrder,
{
    let frame: [RGB8; 1] = [color];
    ws.write(&frame).await;
}

fn color_for_phase(a: &Animation, on: bool) -> RGB8 {
    match a {
        Animation::Solid(c) => *c,
        Animation::Blink { color, .. } => {
            if on {
                *color
            } else {
                OFF
            }
        }
    }
}

fn phase_duration(a: &Animation, on: bool) -> Duration {
    match a {
        // Solid has no scheduled toggle — pick a large value; the outer
        // `min(POLL_TICK)` ensures we still re-check supervisor state.
        Animation::Solid(_) => Duration::from_secs(60),
        Animation::Blink { on_ms, off_ms, .. } => {
            Duration::from_millis(if on { *on_ms as u64 } else { *off_ms as u64 })
        }
    }
}
