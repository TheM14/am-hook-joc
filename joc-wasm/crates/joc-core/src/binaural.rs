//! Binaural runtime: 16-channel objects in, stereo out.
//!
//! The runtime stages the object frame into 512-sample blocks (the renderer's
//! granularity), advances the OAMD position timeline per block, and keeps the
//! rendered samples minus the 961-sample kernel latency.  It is a port of the
//! reference `SofaBinauralRuntime`.

use crate::jochrtf::{Field, Kernels};
use crate::oamd::OamdUpdate;
use crate::sofa_binaural::{BinauralRenderer, RoomConstants};
use crate::timeline::OamdPositionTimeline;
use crate::{fail, ErrorKind, Result, BINAURAL_BLOCK_SAMPLES, FRAME_SAMPLES};

const SOURCE_COUNT: usize = 16;
const LFE_POSITION: [f64; 3] = [0.0, 1.0, 0.0];

pub struct BinauralRuntime {
    renderer: BinauralRenderer,
    profile: u32,
    timeline: OamdPositionTimeline,
    staging: Vec<f64>,
    staged: usize,
    block_output: Vec<f64>,
    output: Vec<f64>,
    input_samples: u64,
    processed_samples: i64,
    blocks_processed: u64,
    hrtf_history_slots: u32,
    tail_samples: u32,
}

impl Default for BinauralRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl BinauralRuntime {
    pub fn new() -> Self {
        Self {
            renderer: BinauralRenderer::new(),
            profile: 1,
            timeline: OamdPositionTimeline::new(),
            staging: Vec::new(),
            staged: 0,
            block_output: Vec::new(),
            output: Vec::new(),
            input_samples: 0,
            processed_samples: 0,
            blocks_processed: 0,
            hrtf_history_slots: 1,
            tail_samples: 61200,
        }
    }

    pub fn open(&mut self, field: &Field, kernels: &Kernels, profile: u32) -> Result<()> {
        self.renderer.configure_kernels(kernels)?;
        self.renderer.configure_field(field)?;
        self.renderer.configure_room(&RoomConstants::default())?;
        self.profile = profile;
        let maximum = field
            .delay_bounds
            .iter()
            .copied()
            .fold(0.0f64, f64::max);
        let maximum_delay = (maximum - 1e-9).ceil() as i64;
        self.hrtf_history_slots = ((maximum_delay + 63) / 64).max(1) as u32;
        self.staging = vec![0.0; BINAURAL_BLOCK_SAMPLES * SOURCE_COUNT];
        self.block_output = vec![0.0; BINAURAL_BLOCK_SAMPLES * 2];
        self.output.clear();
        self.staged = 0;
        self.input_samples = 0;
        self.processed_samples = 0;
        self.blocks_processed = 0;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.renderer.reset();
        self.timeline = OamdPositionTimeline::new();
        self.output.clear();
        self.staged = 0;
        self.input_samples = 0;
        self.processed_samples = 0;
        self.blocks_processed = 0;
    }

    pub fn submit_frame(
        &mut self,
        objects16: &[f32],
        update: Option<&OamdUpdate>,
        frame_index: i64,
        outer_sample_offset: i64,
        object_delay_samples: i64,
    ) -> Result<()> {
        if objects16.len() < SOURCE_COUNT * FRAME_SAMPLES {
            return fail(ErrorKind::InvalidArgument, "render", "null frame");
        }
        if let Some(update) = update {
            self.timeline.submit_update(
                update,
                frame_index * FRAME_SAMPLES as i64,
                outer_sample_offset,
                object_delay_samples,
                self.input_samples as i64,
            )?;
        }
        let mut offset = 0usize;
        while offset < FRAME_SAMPLES {
            let room = BINAURAL_BLOCK_SAMPLES - self.staged;
            let count = room.min(FRAME_SAMPLES - offset);
            for sample in 0..count {
                let row = (self.staged + sample) * SOURCE_COUNT;
                for channel in 0..SOURCE_COUNT {
                    self.staging[row + channel] =
                        f64::from(objects16[channel * FRAME_SAMPLES + offset + sample]);
                }
            }
            self.staged += count;
            offset += count;
            if self.staged == BINAURAL_BLOCK_SAMPLES {
                self.process_block()?;
            }
        }
        self.input_samples += FRAME_SAMPLES as u64;
        Ok(())
    }

    fn process_block(&mut self) -> Result<()> {
        let positions = self
            .timeline
            .positions_at(self.processed_samples)?;
        // The reference sets every source on every block and relies on the
        // per-object crossfade, so `fade` is on.
        let fade = true;
        self.renderer.set_source(
            0,
            &LFE_POSITION,
            self.profile,
            1.0,
            true,
            true,
            fade,
        )?;
        for source in 0..15 {
            self.renderer.set_source(
                source + 1,
                &positions[source],
                self.profile,
                1.0,
                true,
                false,
                fade,
            )?;
        }
        let produced = self.renderer.process(
            &self.staging,
            BINAURAL_BLOCK_SAMPLES,
            1.0,
            &mut self.block_output,
        )?;
        if produced > 0 {
            self.output.extend_from_slice(&self.block_output[..produced * 2]);
        }
        self.staged = 0;
        self.processed_samples += BINAURAL_BLOCK_SAMPLES as i64;
        self.blocks_processed += 1;
        Ok(())
    }

    /// Rendered samples that have not been taken yet.
    pub fn take_output(&mut self, out: &mut Vec<f64>) {
        out.append(&mut self.output);
    }

    pub fn finish_capacity(&self, tail_seconds: f64) -> u32 {
        let mut requested = self.tail_samples as i64;
        if tail_seconds >= 0.0 {
            requested = (tail_seconds * 48000.0 - 1e-9).ceil() as i64;
        }
        let hrtf_bound = i64::from(self.hrtf_history_slots) * 64;
        let early_bound = hrtf_bound + 2048 + 256 * 64;
        let mut drain = requested.max(early_bound) + 961;
        drain = ((drain + 63) / 64) * 64;
        drain.max(0) as u32
    }

    pub fn finish(&mut self, mut flush_samples: u32, out: &mut Vec<f64>) -> Result<()> {
        out.clear();
        if flush_samples == 0 {
            return Ok(());
        }
        let mut chunk = vec![0.0f64; BINAURAL_BLOCK_SAMPLES * 2];
        while flush_samples > 0 {
            let request = flush_samples.min(BINAURAL_BLOCK_SAMPLES as u32) as usize;
            let produced = self.renderer.finish(request, &mut chunk)?;
            if produced == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..produced * 2]);
            flush_samples -= produced as u32;
        }
        Ok(())
    }
}

