//! JOC object reconstruction: 5/7-channel core PCM -> 16-channel objects.
//!
//! A port of `eac3joc_core.cpp` with its float64 arithmetic, table order and
//! frame-state layout preserved.  Channel 0 of the output is the delayed,
//! clipped LFE; channels 1..15 are the reconstructed objects, each already scaled
//! by the frame's clipgain and by the caller's gain.

use crate::tables_qmf as tables;
use crate::{FRAME_SAMPLES, LFE_DELAY_SAMPLES, MAX_CORE_CHANNELS, MAX_DPOINTS, MAX_OBJECTS,
            MAX_PARAMETER_BANDS, OUTPUT_CHANNELS, SUBBANDS, TIMESLOTS};

const PI: f64 = 3.141592653589793238462643383279502884;

#[derive(Clone, Copy, Default)]
struct Complex {
    re: f64,
    im: f64,
}

#[inline]
fn cmul(a: Complex, b: Complex) -> Complex {
    Complex {
        re: a.re * b.re - a.im * b.im,
        im: a.re * b.im + a.im * b.re,
    }
}

#[inline]
fn clamp_unit(value: f64) -> f64 {
    value.clamp(-1.0, 1.0)
}

fn parameter_band_map(bands: u8) -> Option<&'static [u8; 64]> {
    match bands {
        1 => Some(&tables::PB_MAP_1),
        3 => Some(&tables::PB_MAP_3),
        5 => Some(&tables::PB_MAP_5),
        7 => Some(&tables::PB_MAP_7),
        9 => Some(&tables::PB_MAP_9),
        12 => Some(&tables::PB_MAP_12),
        15 => Some(&tables::PB_MAP_15),
        23 => Some(&tables::PB_MAP_23),
        _ => None,
    }
}

/// Per-frame descriptors, in the layout the reference kernel consumes.
pub struct ObjectDescriptor {
    pub n_bands: u8,
    pub n_dpoints: u8,
    pub slope_idx: u8,
    pub offset_ts: [u8; MAX_DPOINTS],
    /// `[object][dpoint][channel][band]`
    pub dq: Vec<f64>,
}

impl ObjectDescriptor {
    pub fn new() -> Self {
        Self {
            n_bands: 0,
            n_dpoints: 0,
            slope_idx: 0,
            offset_ts: [0; MAX_DPOINTS],
            dq: vec![0.0; MAX_OBJECTS * MAX_DPOINTS * MAX_CORE_CHANNELS * MAX_PARAMETER_BANDS],
        }
    }

    #[inline]
    fn dq(&self, object: usize, point: usize, channel: usize, band: usize) -> f64 {
        self.dq[(((object * MAX_DPOINTS + point) * MAX_CORE_CHANNELS + channel)
            * MAX_PARAMETER_BANDS)
            + band]
    }
}

pub struct Renderer {
    analysis_fifo: [[[f64; 64]; 9]; MAX_CORE_CHANNELS],
    analysis_delay: [[[f32; 64]; 10]; 5],
    analysis_phase: f32,
    surround_delay: [[[Complex; 64]; 10]; 2],
    surround_history: [[Complex; 20]; 2],
    lfe_delay: Vec<f64>,
    matrix_previous: [[[f64; 64]; MAX_CORE_CHANNELS]; MAX_OBJECTS],
    synthesis_state: [[f64; 640]; MAX_OBJECTS],
    x: [[[Complex; TIMESLOTS]; SUBBANDS]; MAX_CORE_CHANNELS],
    z: [[[Complex; TIMESLOTS]; SUBBANDS]; MAX_OBJECTS],
    bit_reverse: [u8; 64],
    fft_twiddle: [Complex; 32],
    rotation_sin: [f64; 64],
    rotation_cos: [f64; 64],
    core_channels: usize,
    phase_shift: bool,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        let mut renderer = Self {
            analysis_fifo: [[[0.0; 64]; 9]; MAX_CORE_CHANNELS],
            analysis_delay: [[[0.0f32; 64]; 10]; 5],
            analysis_phase: -0.5,
            surround_delay: [[[Complex::default(); 64]; 10]; 2],
            surround_history: [[Complex::default(); 20]; 2],
            lfe_delay: vec![0.0; LFE_DELAY_SAMPLES],
            matrix_previous: [[[0.0; 64]; MAX_CORE_CHANNELS]; MAX_OBJECTS],
            synthesis_state: [[0.0; 640]; MAX_OBJECTS],
            x: [[[Complex::default(); TIMESLOTS]; SUBBANDS]; MAX_CORE_CHANNELS],
            z: [[[Complex::default(); TIMESLOTS]; SUBBANDS]; MAX_OBJECTS],
            bit_reverse: [0; 64],
            fft_twiddle: [Complex::default(); 32],
            rotation_sin: [0.0; 64],
            rotation_cos: [0.0; 64],
            core_channels: 5,
            phase_shift: true,
        };
        renderer.initialize_tables();
        renderer.reset();
        renderer
    }

    fn initialize_tables(&mut self) {
        for i in 0..64usize {
            let mut value = i;
            let mut reversed = 0usize;
            for _ in 0..6 {
                reversed = (reversed << 1) | (value & 1);
                value >>= 1;
            }
            self.bit_reverse[i] = reversed as u8;
            let theta = PI * i as f64 / 128.0;
            self.rotation_sin[i] = 0.5 * theta.sin();
            self.rotation_cos[i] = 0.5 * theta.cos();
        }
        for i in 0..32 {
            let angle = -2.0 * PI * i as f64 / 64.0;
            self.fft_twiddle[i] = Complex { re: angle.cos(), im: angle.sin() };
        }
    }

    pub fn reset(&mut self) {
        self.analysis_fifo = [[[0.0; 64]; 9]; MAX_CORE_CHANNELS];
        self.analysis_delay = [[[0.0f32; 64]; 10]; 5];
        self.surround_delay = [[[Complex::default(); 64]; 10]; 2];
        self.surround_history = [[Complex::default(); 20]; 2];
        self.lfe_delay = vec![0.0; LFE_DELAY_SAMPLES];
        self.matrix_previous = [[[0.0; 64]; MAX_CORE_CHANNELS]; MAX_OBJECTS];
        self.synthesis_state = [[0.0; 640]; MAX_OBJECTS];
        self.x = [[[Complex::default(); TIMESLOTS]; SUBBANDS]; MAX_CORE_CHANNELS];
        self.z = [[[Complex::default(); TIMESLOTS]; SUBBANDS]; MAX_OBJECTS];
        // Sentinel: the first frame after reset takes the new scalar directly,
        // without an intra-frame transition.
        self.analysis_phase = -0.5;
    }

    fn fft64_tables(reverse: &[u8; 64], twiddle: &[Complex; 32], values: &mut [Complex; 64]) {
        for i in 0..64 {
            let j = usize::from(reverse[i]);
            if j > i {
                values.swap(i, j);
            }
        }
        let mut length = 2usize;
        while length <= 64 {
            let half = length >> 1;
            let twiddle_step = 64 / length;
            let mut base = 0usize;
            while base < 64 {
                for j in 0..half {
                    let even = values[base + j];
                    let odd = cmul(values[base + j + half], twiddle[j * twiddle_step]);
                    values[base + j] = Complex { re: even.re + odd.re, im: even.im + odd.im };
                    values[base + j + half] =
                        Complex { re: even.re - odd.re, im: even.im - odd.im };
                }
                base += length;
            }
            length <<= 1;
        }
    }

    fn analysis_slot(&mut self, channel: usize, ring: &[f32; 64], timeslot: usize) {
        let mut v36 = [0.0f64; 64];
        let mut v40 = [0.0f64; 64];
        let mut frequency = [Complex::default(); 64];
        for sample in 0..64 {
            v36[sample] = self.analysis_fifo[channel][0][sample]
                * tables::ANALYSIS_WINDOW[8 * 64 + sample]
                + self.analysis_fifo[channel][2][sample] * tables::ANALYSIS_WINDOW[6 * 64 + sample]
                + self.analysis_fifo[channel][4][sample] * tables::ANALYSIS_WINDOW[4 * 64 + sample]
                + self.analysis_fifo[channel][6][sample] * tables::ANALYSIS_WINDOW[2 * 64 + sample]
                + self.analysis_fifo[channel][8][sample] * tables::ANALYSIS_WINDOW[sample];
            v40[sample] = self.analysis_fifo[channel][1][sample]
                * tables::ANALYSIS_WINDOW[7 * 64 + sample]
                + self.analysis_fifo[channel][3][sample] * tables::ANALYSIS_WINDOW[5 * 64 + sample]
                + self.analysis_fifo[channel][5][sample] * tables::ANALYSIS_WINDOW[3 * 64 + sample]
                + self.analysis_fifo[channel][7][sample] * tables::ANALYSIS_WINDOW[64 + sample]
                + f64::from(ring[sample]) * tables::ANALYSIS_WINDOW[9 * 64 + sample];
        }
        for k in 0..64 {
            let source = 63 - k;
            let re = v40[source];
            let im = v36[source];
            let a = self.rotation_sin[k];
            let b = self.rotation_cos[k];
            frequency[k] = Complex { re: im * a - re * b, im: im * b + re * a };
        }
        Self::fft64_tables(&self.bit_reverse, &self.fft_twiddle, &mut frequency);
        const SCALE: f64 = 1.0 / 64.0;
        for k in 0..32 {
            self.x[channel][2 * k][timeslot] =
                Complex { re: frequency[k].re * SCALE, im: -frequency[k].im * SCALE };
            self.x[channel][2 * k + 1][timeslot] =
                Complex { re: frequency[63 - k].re * SCALE, im: frequency[63 - k].im * SCALE };
        }
        for history in (1..9).rev() {
            self.analysis_fifo[channel][history] = self.analysis_fifo[channel][history - 1];
        }
        for sample in 0..64 {
            self.analysis_fifo[channel][0][sample] = f64::from(ring[sample]);
        }
    }

    fn surround_post(&mut self) {
        const DC_A: [f64; 21] = [
            -0.0006242550443857908, -0.0019234686624258757, -0.0042654648423194885,
            -0.008168308064341545, -0.014327201060950756, -0.023759860545396805,
            -0.03757232800126076, -0.05577569454908371, -0.07568276673555374,
            -0.09172472357749939, -0.5979374051094055, -0.09172472357749939,
            -0.07568276673555374, -0.05577569454908371, -0.03757232800126076,
            -0.023759860545396805, -0.014327201060950756, -0.008168308064341545,
            -0.0042654648423194885, -0.0019234686624258757, -0.0006242550443857908,
        ];
        const DC_B: [f64; 21] = [
            0.0013996040215715766, 0.003839150769636035, 0.007512642536312342,
            0.012419373728334904, 0.018367428332567215, 0.0249701626598835,
            0.03167900815606117, 0.03785000368952751, 0.04283412545919418,
            0.04607561603188515, 0.047200120985507965, 0.04607561603188515,
            0.04283412545919418, 0.03785000368952751, 0.03167900815606117,
            0.0249701626598835, 0.018367428332567215, 0.012419373728334904,
            0.007512642536312342, 0.003839150769636035, 0.0013996040215715766,
        ];
        for surround in 0..2 {
            let channel = surround + 3;
            let mut group = 0usize;
            while group < 24 {
                let mut current = [[Complex::default(); 64]; 4];
                let mut dc_buffer = [Complex::default(); 24];
                for slot in 0..4 {
                    for band in 0..64 {
                        current[slot][band] = self.x[channel][band][group + slot];
                        let delayed = self.surround_delay[surround][slot][band];
                        self.x[channel][band][group + slot] = if self.phase_shift {
                            Complex { re: delayed.im, im: -delayed.re }
                        } else {
                            delayed
                        };
                    }
                }
                for i in 0..20 {
                    dc_buffer[i] = self.surround_history[surround][i];
                }
                for i in 0..4 {
                    dc_buffer[20 + i] = current[i][0];
                }
                if self.phase_shift {
                    for slot in 0..4 {
                        let mut sum = Complex::default();
                        for tap in 0..21 {
                            let sample = dc_buffer[slot + tap];
                            let cr = DC_B[tap];
                            let ci = DC_A[tap];
                            sum.re += sample.re * cr - sample.im * ci;
                            sum.im += sample.re * ci + sample.im * cr;
                        }
                        self.x[channel][0][group + slot] =
                            Complex { re: 2.0 * sum.re, im: 2.0 * sum.im };
                    }
                }
                for i in 0..20 {
                    self.surround_history[surround][i] = dc_buffer[i + 4];
                }
                // Shift the 10-slot delay line by four and append this group.
                for slot in 0..6 {
                    self.surround_delay[surround][slot] = self.surround_delay[surround][slot + 4];
                }
                for slot in 0..4 {
                    self.surround_delay[surround][6 + slot] = current[slot];
                }
                group += 4;
            }
        }
    }

    fn analysis_channel(
        &mut self,
        channel: usize,
        bed_planar: &[f32],
        phase_old: f32,
        phase_new: f32,
    ) {
        let ramp_phase = phase_old != phase_new;
        let phase_step = (phase_new - phase_old) / 256.0;
        let mut scaled = [0.0f32; 64];
        for timeslot in 0..TIMESLOTS {
            for sample in 0..64 {
                let frame_sample = timeslot * 64 + sample;
                let mut gain = phase_new;
                if ramp_phase && frame_sample < 256 {
                    gain = phase_old + (frame_sample as f32) * phase_step;
                }
                scaled[sample] = bed_planar[channel * FRAME_SAMPLES + frame_sample] * gain;
            }
            // L/R/C and, for 7 core channels, the extra pair (5 and 6) take the
            // ten-slot delay here; Ls/Rs take the same delay inside surround_post().
            let delay_lane = if channel < 3 { channel } else { channel - 2 };
            let use_delay = channel < 3 || channel >= 5;
            let ring: [f32; 64] = if use_delay {
                let delayed = self.analysis_delay[delay_lane][0];
                for slot in 0..9 {
                    self.analysis_delay[delay_lane][slot] = self.analysis_delay[delay_lane][slot + 1];
                }
                self.analysis_delay[delay_lane][9] = scaled;
                delayed
            } else {
                scaled
            };
            self.analysis_slot(channel, &ring, timeslot);
        }
    }

    fn analysis(&mut self, bed_planar: &[f32], clipgain: f64, phase_new: f32) {
        // Analysis-side scalar = the structural 1/16 times this frame's clipgain.
        let scalar = (f64::from(phase_new) * clipgain) as f32;
        let mut phase_old = self.analysis_phase;
        if phase_old <= 0.0 && phase_old != 0.0 {
            phase_old = scalar;
        }
        for channel in 0..self.core_channels {
            self.analysis_channel(channel, bed_planar, phase_old, scalar);
        }
        self.analysis_phase = scalar;
        self.surround_post();
    }

    fn matrix_object(&mut self, object: usize, descriptors: &ObjectDescriptor) {
        self.z[object] = [[Complex::default(); TIMESLOTS]; SUBBANDS];
        let bands = descriptors.n_bands as usize;
        let points = descriptors.n_dpoints as usize;
        let slope = descriptors.slope_idx;
        let Some(pb_map) = parameter_band_map(descriptors.n_bands) else {
            return;
        };
        for channel in 0..self.core_channels {
            for subband in 0..64 {
                let parameter_band = pb_map[subband] as usize;
                let previous = self.matrix_previous[object][channel][subband];
                let target0 = descriptors.dq(object, 0, channel, parameter_band);
                let target1 = if points == 2 {
                    descriptors.dq(object, 1, channel, parameter_band)
                } else {
                    target0
                };
                let mut last = previous;
                for timeslot in 0..TIMESLOTS {
                    let coefficient = if slope == 0 {
                        if points == 1 {
                            let alpha = (timeslot + 1) as f64 / 24.0;
                            previous * (1.0 - alpha) + target0 * alpha
                        } else if timeslot < 12 {
                            let alpha = (timeslot + 1) as f64 / 12.0;
                            previous * (1.0 - alpha) + target0 * alpha
                        } else {
                            let alpha = (timeslot - 11) as f64 / 12.0;
                            target0 * (1.0 - alpha) + target1 * alpha
                        }
                    } else if points == 1 {
                        if timeslot < descriptors.offset_ts[0] as usize {
                            previous
                        } else {
                            target0
                        }
                    } else {
                        let mut coefficient = if timeslot < descriptors.offset_ts[0] as usize {
                            previous
                        } else {
                            target0
                        };
                        if timeslot >= descriptors.offset_ts[1] as usize {
                            coefficient = target1;
                        }
                        coefficient
                    };
                    self.z[object][subband][timeslot].re +=
                        self.x[channel][subband][timeslot].re * coefficient;
                    self.z[object][subband][timeslot].im +=
                        self.x[channel][subband][timeslot].im * coefficient;
                    last = coefficient;
                }
                self.matrix_previous[object][channel][subband] = last;
            }
        }
        let _ = bands;
    }

    fn qmf5_step(state: &mut [f64; 640], rotated: &[f64; 128], output: &mut [f64; 64]) {
        for block in 0..16 {
            for lane in 0..4 {
                let sample = block * 4 + lane;
                let base = block * 36 + lane;
                let even = rotated[block * 8 + lane * 2];
                let odd = rotated[block * 8 + lane * 2 + 1];
                output[sample] =
                    2.0 * (tables::QMF5_WINDOW[sample] * even + state[base]);
                state[base] = tables::QMF5_WINDOW[64 + sample] * odd + state[base + 4];
                for slot in 0..7 {
                    let alternating = if slot & 1 == 0 { even } else { odd };
                    state[base + (slot + 1) * 4] =
                        tables::QMF5_WINDOW[(slot + 2) * 64 + sample] * alternating
                            + state[base + (slot + 2) * 4];
                }
                state[base + 8 * 4] = tables::QMF5_WINDOW[9 * 64 + sample] * odd;
            }
        }
    }

    fn synthesis_object(&mut self, object: usize, output_scale: f32, output16: &mut [f32]) {
        let mut frequency = [Complex::default(); 64];
        let mut rotated = [0.0f64; 128];
        let mut pcm64 = [0.0f64; 64];
        let sin_table = self.rotation_sin;
        let cos_table = self.rotation_cos;
        let reverse = self.bit_reverse;
        let twiddle = self.fft_twiddle;
        let z = &self.z[object];
        let state = &mut self.synthesis_state[object];
        let destination = (object + 1) * FRAME_SAMPLES;
        for timeslot in 0..TIMESLOTS {
            for k in 0..32 {
                let even = z[2 * k][timeslot];
                let odd = z[2 * k + 1][timeslot];
                frequency[k] = Complex { re: even.re, im: -even.im };
                frequency[63 - k] = Complex { re: odd.re, im: odd.im };
            }
            Self::fft64_tables(&reverse, &twiddle, &mut frequency);
            for k in 0..64 {
                let re = frequency[k].re;
                let im = frequency[k].im;
                let sin_component = sin_table[k];
                let cos_component = cos_table[k];
                rotated[2 * k] = 2.0 * (re * cos_component + im * sin_component);
                rotated[2 * k + 1] = 2.0 * (im * cos_component - re * sin_component);
            }
            Self::qmf5_step(state, &rotated, &mut pcm64);
            for sample in 0..64 {
                let clipped = clamp_unit(16.0 * pcm64[sample]);
                let value = (clipped * f64::from(output_scale)) as f32;
                output16[destination + timeslot * 64 + sample] = value;
            }
        }
    }

    fn render_lfe(&mut self, lfe: Option<&[f32]>, output_scale: f32, output16: &mut [f32]) {
        let Some(lfe) = lfe else {
            return;
        };
        for sample in 0..LFE_DELAY_SAMPLES {
            let value = clamp_unit(self.lfe_delay[sample]);
            output16[sample] = (value * f64::from(output_scale)) as f32;
        }
        for sample in LFE_DELAY_SAMPLES..FRAME_SAMPLES {
            let value = clamp_unit(f64::from(lfe[sample - LFE_DELAY_SAMPLES]));
            output16[sample] = (value * f64::from(output_scale)) as f32;
        }
        for sample in 0..LFE_DELAY_SAMPLES {
            self.lfe_delay[sample] = f64::from(lfe[sample + (FRAME_SAMPLES - LFE_DELAY_SAMPLES)]);
        }
    }

    /// Rebuilds one frame into `output16` (`[16][1536]`, planar, zeroed first).
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        bed_planar: &[f32],
        lfe: Option<&[f32]>,
        core_channels: usize,
        phase_shift: bool,
        object_mask: u32,
        descriptors: &ObjectDescriptor,
        clipgain: f64,
        phase_new: f32,
        output_scale: f32,
        output16: &mut [f32],
    ) -> crate::Result<()> {
        if core_channels != 5 && core_channels != 7 {
            return crate::fail(
                crate::ErrorKind::JocUnsupportedVariant,
                "dsp",
                "core_channel_count must be 5 or 7",
            );
        }
        if bed_planar.len() < core_channels * FRAME_SAMPLES {
            return crate::fail(
                crate::ErrorKind::InvalidArgument,
                "dsp",
                "bed PCM is shorter than one frame",
            );
        }
        self.core_channels = core_channels;
        self.phase_shift = phase_shift;
        output16[..OUTPUT_CHANNELS * FRAME_SAMPLES].fill(0.0);
        self.analysis(bed_planar, clipgain, phase_new);
        self.render_lfe(lfe, output_scale, output16);
        for object in 0..MAX_OBJECTS {
            if object_mask & (1 << object) == 0 {
                continue;
            }
            self.matrix_object(object, descriptors);
            self.synthesis_object(object, output_scale, output16);
        }
        Ok(())
    }
}
