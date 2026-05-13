//! Status indicators: red LED (GP7) heartbeat + NeoPixel (GP21) state-driven
//! animations.
//!
//! Phase 0 already lit GP7. The NeoPixel WS2812 driver via PIO2 SM0 went in
//! with the PIO toolchain bringup; see `docs/pico_firmware_design.md` §5.9
//! and `docs/instrumentation_surface.md` §4.7 for the state→color mapping.

pub mod animations;
pub mod heartbeat;
pub mod neopixel;
