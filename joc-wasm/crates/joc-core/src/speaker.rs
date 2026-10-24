//! Speaker 2.0 rendering of the reconstructed objects.
//!
//! The reference renders every layout through one geometry table; this port keeps
//! the arithmetic of that renderer and evaluates it for the 2.0 layout only (one
//! axis group from 0 to 32767, mode 1, floor attenuation -4.5 dB), which is what
//! the 2.0 entry of the table describes.  Object 0 of the kernel output is the
//! LFE, which a stereo layout does not carry.

use crate::oamd::{OamdState, OamdUpdate, OBJECTS};
use crate::{FRAME_SAMPLES, OUTPUT_CHANNELS, SPEAKER_BLOCK_SAMPLES};

const PI: f64 = 3.141592653589793238462643383279502884;
const Q15_SCALE: f64 = 32768.0;
const Q15_MAX: f64 = 32767.0 / 32768.0;
/// `layout_attenuation_db(0x1)`: no height channels, two floor channels.
const ATTENUATION_DB: f64 = -4.5;
const GAIN_SNAP_THRESHOLD: f64 = 1.0e-4;
const K_BLOCK: usize = SPEAKER_BLOCK_SAMPLES;

#[inline]
fn equal_power_pair(position: f64) -> (f64, f64) {
    let angle = (PI * 0.5) * position;
    (angle.cos(), angle.sin())
}

/// The 2.0 region: two points at x = 0 and x = 32767, one axis group.
fn render_point(position: &[u16; 3], object_gain: f64, output: &mut [f64; 2]) {
    output.fill(0.0);
    let u = f64::from(position[0]) / Q15_SCALE;
    let v = f64::from(position[1]) / Q15_SCALE;
    let w = f64::from(position[2]) / Q15_SCALE;
    let floor_v = v.clamp(0.0, 1.0);
    let mut floor = [0.0f64; 2];
    // axis0_gains over the single group [0, 1].
    let first_value = 0.0;
    let last_value = Q15_MAX;
    if u <= first_value {
        floor[0] = 1.0;
    } else if u >= last_value {
        floor[1] = 1.0;
    } else if u > first_value && u <= last_value {
        let amount = (u - first_value) / (last_value - first_value);
        let (lower, upper) = equal_power_pair(amount);
        floor[0] = lower;
        floor[1] = upper;
    }
    let _ = floor_v; // mode 1: the vertical axis does not index the geometry
    let y_term = (v / 0.6).clamp(0.0, 1.0);
    let z_term = ((w - 0.2) / 0.8).clamp(0.0, 1.0);
    let amount = (y_term + z_term).clamp(0.0, 1.0);
    let gain = 10f64.powf(ATTENUATION_DB * amount / 20.0) * object_gain;
    output[0] = floor[0] * gain;
    output[1] = floor[1] * gain;
}

pub struct Speaker20 {
    state: OamdState,
    current: [[f64; 2]; OBJECTS],
    target: [[f64; 2]; OBJECTS],
    step: [[f64; 2]; OBJECTS],
    remaining: [[u32; 2]; OBJECTS],
    metadata_offset: u32,
    had_payload: bool,
}

impl Default for Speaker20 {
    fn default() -> Self {
        Self::new()
    }
}

impl Speaker20 {
    pub fn new() -> Self {
        Self {
            state: OamdState::new(),
            current: [[0.0; 2]; OBJECTS],
            target: [[0.0; 2]; OBJECTS],
            step: [[0.0; 2]; OBJECTS],
            remaining: [[0; 2]; OBJECTS],
            metadata_offset: 1473,
            had_payload: false,
        }
    }

    pub fn reset(&mut self) {
        self.state.reset();
        self.current = [[0.0; 2]; OBJECTS];
        self.target = [[0.0; 2]; OBJECTS];
        self.step = [[0.0; 2]; OBJECTS];
        self.remaining = [[0; 2]; OBJECTS];
        self.had_payload = false;
    }

    /// `objects16` is the kernel output (`[16][1536]`, planar), `out` receives
    /// interleaved stereo.  The reference transposes to interleaved float first;
    /// reading the planar frame directly is the same values.
    pub fn process(&mut self, objects16: &[f32], update: Option<&OamdUpdate>, out: &mut [f64]) {
        debug_assert_eq!(objects16.len(), OUTPUT_CHANNELS * FRAME_SAMPLES);
        debug_assert_eq!(out.len(), FRAME_SAMPLES * 2);
        out.fill(0.0);
        self.had_payload = update.is_some();
        let mut ramp = 0u32;
        if let Some(update) = update {
            self.state.apply(update);
            ramp = update.ramp_duration_samples;
        }
        let positions = self.state.object_positions_q15();
        let event_count = usize::from(self.had_payload);
        let event_offset = self.metadata_offset;
        let event_ramp = ramp;
        let total_blocks = FRAME_SAMPLES / K_BLOCK;

        let mut event = 0usize;
        for block in 0..total_blocks {
            while event < event_count && aligned_block(event_offset) == block {
                self.apply_event(event_ramp, &positions);
                event += 1;
            }
            self.mix_block(objects16, out, block);
        }
        while event < event_count && aligned_block(event_offset) == total_blocks {
            self.apply_event(event_ramp, &positions);
            event += 1;
        }
    }

    fn apply_event(&mut self, ramp_duration: u32, positions: &[[u16; 3]; OBJECTS]) {
        let blocks = ramp_blocks(ramp_duration);
        for object in 0..OBJECTS {
            let mut next = [0.0f64; 2];
            render_point(&positions[object], 1.0, &mut next);
            for channel in 0..2 {
                let difference = next[channel] - self.current[object][channel];
                self.target[object][channel] = next[channel];
                if difference.abs() >= GAIN_SNAP_THRESHOLD && blocks != 0 {
                    self.step[object][channel] = difference / f64::from(blocks);
                    self.remaining[object][channel] = blocks;
                } else {
                    self.current[object][channel] = next[channel];
                    self.step[object][channel] = 0.0;
                    self.remaining[object][channel] = 0;
                }
            }
        }
    }

    fn mix_block(&mut self, objects16: &[f32], out: &mut [f64], block: usize) {
        let start = block * K_BLOCK;
        for object in 0..OBJECTS {
            let source = (object + 1) * FRAME_SAMPLES;
            for channel in 0..2 {
                let active = self.remaining[object][channel] != 0;
                let fixed_gain = self.target[object][channel];
                if !active && fixed_gain == 0.0 {
                    continue;
                }
                for sample in 0..K_BLOCK {
                    let gain = if active {
                        self.current[object][channel]
                            + (sample as f64 / K_BLOCK as f64) * self.step[object][channel]
                    } else {
                        fixed_gain
                    };
                    out[(start + sample) * 2 + channel] +=
                        f64::from(objects16[source + start + sample]) * gain;
                }
                if active {
                    self.current[object][channel] += self.step[object][channel];
                    self.remaining[object][channel] -= 1;
                    if self.remaining[object][channel] == 0 {
                        self.current[object][channel] = self.target[object][channel];
                    }
                } else {
                    self.current[object][channel] = self.target[object][channel];
                }
            }
        }
    }
}

fn aligned_block(sample: u32) -> usize {
    (sample as usize + K_BLOCK / 2 - 1) / K_BLOCK
}

fn ramp_blocks(duration: u32) -> u32 {
    ((duration as usize + K_BLOCK / 2 - 1) / K_BLOCK) as u32
}
