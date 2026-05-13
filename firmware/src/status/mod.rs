//! Status indicators: red LED (GP7) heartbeat + NeoPixel (GP21) state-driven
//! animations.
//!
//! Phase 0 already lit GP7. The NeoPixel WS2812 driver via PIO is Phase 1+
//! work; see `docs/pico_firmware_design.md` §5.9 and
//! `docs/instrumentation_surface.md` for the state→color mapping.

pub mod heartbeat;

#[allow(dead_code)] // NeoPixel driver lands at Phase 1+
pub mod neopixel {
    //! PIO WS2812 driver. Stub.
}

#[allow(dead_code)] // Animation table lands when SessionState is wired up
pub mod animations {
    //! Per-`SessionState` color/blink pattern. Stub.
}
