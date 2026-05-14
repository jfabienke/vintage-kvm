//! PS/2 wire frame extractor.
//!
//! Pure-logic state machine that consumes a 1 µs (CLK, DATA, timestamp)
//! stream and emits one [`Ps2Frame`] per detected wire frame. Handles both
//! protocol variants — XT (9-bit, start=1, no parity/stop) and AT/PS/2
//! (11-bit, start=0 + 8 data + odd parity + stop=1) — distinguished by
//! the start bit's polarity.
//!
//! `no_std` + no alloc; tested on host and consumed by the firmware
//! oversampler task. Reference design lives in
//! [`docs/pio_state_machines_design.md`](https://github.com/jfabienke/vintage-kvm/blob/master/docs/pio_state_machines_design.md)
//! §7 and [`docs/ps2_eras_reference.md`](https://github.com/jfabienke/vintage-kvm/blob/master/docs/ps2_eras_reference.md).

#![no_std]

/// CLK glitch threshold. PS/2 transitions are guaranteed ≥ 25 µs;
/// anything shorter than 4 samples (4 µs) is electrical noise.
pub const GLITCH_THRESHOLD_US: u32 = 4;

/// Inter-edge timeout. After this much idle time we give up on whatever
/// in-flight frame we have and reset.
pub const IDLE_TIMEOUT_US: u32 = 200;

/// XT frame length (start + 8 data).
const XT_BIT_COUNT: u8 = 9;
/// AT/PS2 frame length (start + 8 data + parity + stop).
const AT_BIT_COUNT: u8 = 11;

/// One PS/2 frame extracted from the wire. Carries timing metadata from
/// the oversampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Ps2Frame {
    pub data: u8,
    pub parity_ok: bool,
    pub framing_ok: bool,
    pub start_timestamp_us: u64,
    pub timing: FrameTiming,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FrameTiming {
    /// Measured period between CLK falling edges for each bit slot.
    /// Index 0 is start→D0, index 9 is parity→stop (AT/PS2).
    pub bit_periods_us: [u16; 11],
    /// Signed CLK→DATA edge skew at the start bit (positive = DATA settles
    /// after CLK).
    pub clk_data_skew_us: i8,
    /// CLK transitions shorter than the glitch threshold.
    pub glitch_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    /// Receiving a frame. `bits` counts CLK falling edges seen so far
    /// (including the start bit).
    Receiving { bits: u8 },
}

#[derive(Debug, Clone, Copy)]
pub struct Framer {
    state: State,

    /// Last sampled CLK level (for edge detection). `None` before the
    /// first sample.
    last_clk: Option<bool>,
    /// Sample timestamp at which CLK last changed. Used to filter
    /// glitches and to compute per-bit periods.
    last_edge_us: u64,

    /// Bits assembled so far, packed LSB-first into a u16. Bit 0 is the
    /// start bit; bits 1..9 are D0..D7; bit 9 is parity (AT/PS2 only);
    /// bit 10 is stop (AT/PS2 only).
    assembled: u16,
    /// Timestamp of the start bit (first CLK fall after idle).
    frame_start_us: u64,
    /// Timestamp of the most recent CLK falling edge — used to compute the
    /// next bit period.
    last_falling_us: u64,

    periods_us: [u16; 11],
    glitch_count: u8,
    clk_data_skew_us: i8,

    last_data_change_us: u64,
    last_data: Option<bool>,
}

impl Framer {
    pub const fn new() -> Self {
        Self {
            state: State::Idle,
            last_clk: None,
            last_edge_us: 0,
            assembled: 0,
            frame_start_us: 0,
            last_falling_us: 0,
            periods_us: [0; 11],
            glitch_count: 0,
            clk_data_skew_us: 0,
            last_data_change_us: 0,
            last_data: None,
        }
    }

    fn reset_idle(&mut self) {
        self.state = State::Idle;
        self.assembled = 0;
        self.frame_start_us = 0;
        self.last_falling_us = 0;
        self.periods_us = [0; 11];
        self.glitch_count = 0;
        self.clk_data_skew_us = 0;
    }

    /// Ingest one (clk, data, t_us) sample. Returns `Some(Ps2Frame)` when
    /// a frame completes (either bit-count reached or timeout); otherwise
    /// `None`. Caller is expected to invoke this at 1 µs cadence; gaps in
    /// time are fine but reduce edge-skew accuracy.
    pub fn ingest(&mut self, clk: bool, data: bool, t_us: u64) -> Option<Ps2Frame> {
        if let State::Receiving { bits } = self.state {
            if t_us.saturating_sub(self.last_falling_us) as u32 >= IDLE_TIMEOUT_US {
                let frame = self.emit_on_timeout(bits);
                self.reset_idle();
                self.last_clk = Some(clk);
                self.last_data = Some(data);
                return frame;
            }
        }

        if self.last_data != Some(data) {
            self.last_data_change_us = t_us;
            self.last_data = Some(data);
        }

        let prev_clk = self.last_clk;
        self.last_clk = Some(clk);

        let edge = match prev_clk {
            Some(p) => p != clk,
            None => false,
        };
        if !edge {
            return None;
        }

        let dur_us = t_us.saturating_sub(self.last_edge_us) as u32;
        let prev_level = prev_clk.unwrap();
        self.last_edge_us = t_us;

        // Glitch filter: a transition pair shorter than the threshold is
        // counted and the bit-clock not advanced. Mirrors §7.5 of the design.
        if dur_us < GLITCH_THRESHOLD_US {
            if let State::Receiving { .. } = self.state {
                self.glitch_count = self.glitch_count.saturating_add(1);
            }
            return None;
        }

        if prev_level && !clk {
            return self.on_clk_fall(data, t_us);
        }
        None
    }

    fn on_clk_fall(&mut self, data: bool, t_us: u64) -> Option<Ps2Frame> {
        match self.state {
            State::Idle => {
                self.assembled = u16::from(data);
                self.frame_start_us = t_us;
                self.last_falling_us = t_us;
                self.periods_us = [0; 11];
                self.glitch_count = 0;
                let skew_us = (t_us as i64) - (self.last_data_change_us as i64);
                self.clk_data_skew_us = skew_us.clamp(i8::MIN as i64, i8::MAX as i64) as i8;
                self.state = State::Receiving { bits: 1 };
                None
            }
            State::Receiving { bits } => {
                let period_us = t_us.saturating_sub(self.last_falling_us) as u16;
                if (bits as usize) <= self.periods_us.len() {
                    self.periods_us[(bits - 1) as usize] = period_us;
                }
                self.last_falling_us = t_us;
                self.assembled |= u16::from(data) << bits;
                let bits = bits + 1;
                self.state = State::Receiving { bits };

                if bits >= AT_BIT_COUNT {
                    let f = self.build_frame(AT_BIT_COUNT);
                    self.reset_idle();
                    return Some(f);
                }
                // XT frame completion is detected by the inter-frame
                // timeout, not bit count — we can't tell mid-stream whether
                // bit 9 is "AT parity" or "XT idle past the end of frame".
                None
            }
        }
    }

    fn emit_on_timeout(&mut self, bits: u8) -> Option<Ps2Frame> {
        if bits >= AT_BIT_COUNT {
            return None;
        }
        if bits == XT_BIT_COUNT && (self.assembled & 0x1) != 0 {
            return Some(self.build_frame(XT_BIT_COUNT));
        }
        if bits > 0 {
            return Some(self.build_frame_invalid());
        }
        None
    }

    fn build_frame(&self, total_bits: u8) -> Ps2Frame {
        let data = ((self.assembled >> 1) & 0xFF) as u8;
        let (parity_ok, framing_ok) = if total_bits == AT_BIT_COUNT {
            let parity_bit = ((self.assembled >> 9) & 1) as u8;
            let stop_bit = ((self.assembled >> 10) & 1) as u8;
            let start_bit = (self.assembled & 1) as u8;
            let ones = (data.count_ones() as u8) + parity_bit;
            let parity_ok = (ones & 1) == 1;
            let framing_ok = start_bit == 0 && stop_bit == 1;
            (parity_ok, framing_ok)
        } else {
            let start_bit = (self.assembled & 1) as u8;
            (true, start_bit == 1)
        };

        Ps2Frame {
            data,
            parity_ok,
            framing_ok,
            start_timestamp_us: self.frame_start_us,
            timing: FrameTiming {
                bit_periods_us: self.periods_us,
                clk_data_skew_us: self.clk_data_skew_us,
                glitch_count: self.glitch_count,
            },
        }
    }

    fn build_frame_invalid(&self) -> Ps2Frame {
        let data = ((self.assembled >> 1) & 0xFF) as u8;
        Ps2Frame {
            data,
            parity_ok: false,
            framing_ok: false,
            start_timestamp_us: self.frame_start_us,
            timing: FrameTiming {
                bit_periods_us: self.periods_us,
                clk_data_skew_us: self.clk_data_skew_us,
                glitch_count: self.glitch_count,
            },
        }
    }
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}
