//! NeoPixel animation table — maps `SupervisorState` to a color/blink
//! pattern. Canonical reference is `docs/instrumentation_surface.md` §4.7.

use smart_leds::RGB8;

use crate::lifecycle::SupervisorState;

/// Animation pattern shown on the WS2812.
///
/// `Solid` paints one color forever. `Blink` alternates between `color`
/// (for `on_ms`) and off (for `off_ms`). The off period is also `off_ms`
/// rather than a separate "second color" because every state we care about
/// is "this color, present or absent" — a two-color blink would just
/// confuse the lookup.
#[derive(Debug, Clone, Copy)]
pub enum Animation {
    Solid(RGB8),
    Blink { color: RGB8, on_ms: u32, off_ms: u32 },
}

const RED: RGB8 = RGB8 { r: 64, g: 0, b: 0 };
const YELLOW: RGB8 = RGB8 { r: 48, g: 32, b: 0 };
const ORANGE: RGB8 = RGB8 { r: 64, g: 16, b: 0 };
const GREEN: RGB8 = RGB8 { r: 0, g: 64, b: 0 };
const CYAN: RGB8 = RGB8 { r: 0, g: 48, b: 48 };
const BLUE: RGB8 = RGB8 { r: 0, g: 0, b: 64 };
const MAGENTA: RGB8 = RGB8 { r: 48, g: 0, b: 48 };

pub fn for_state(s: SupervisorState) -> Animation {
    use SupervisorState::*;
    match s {
        Boot => Animation::Solid(RED),
        Selftest => Animation::Blink { color: RED, on_ms: 125, off_ms: 125 },
        AwaitHostPower => Animation::Solid(YELLOW),
        DetectMachineClass => Animation::Blink { color: CYAN, on_ms: 250, off_ms: 250 },
        InjectDebug => Animation::Solid(CYAN),
        ServeStage0Download => Animation::Blink { color: BLUE, on_ms: 250, off_ms: 250 },
        ServeStage1Handoff => Animation::Blink { color: BLUE, on_ms: 125, off_ms: 125 },
        ServeCapHandshake => Animation::Solid(BLUE),
        ServeStage2Download => Animation::Blink { color: MAGENTA, on_ms: 250, off_ms: 250 },
        DpReady => Animation::Solid(MAGENTA),
        DpActive => Animation::Solid(GREEN),
        ErrorRecoverable => Animation::Blink { color: ORANGE, on_ms: 125, off_ms: 125 },
        FaultUnrecoverable => Animation::Blink { color: RED, on_ms: 62, off_ms: 62 },
    }
}
