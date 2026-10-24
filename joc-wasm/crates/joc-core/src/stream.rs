//! Streaming JOC decoder: E-AC-3 syncframes + the core PCM of those frames in,
//! rendered stereo out.  Mirrors the reference push/pull stream, including its
//! bed planning, missing-metadata handling and binaural tail.

use std::collections::VecDeque;

use crate::binaural::BinauralRuntime;
use crate::eac3::{Frame, FrameReader, Next};
use crate::id14::{self, BedPlan, CrossLayerNotes, FrameParams};
use crate::jochrtf::{self, Field};
use crate::oamd::{self, BedAssignment, OamdUpdate};
use crate::objects16;
use crate::qmf::Renderer as JocKernel;
use crate::speaker::Speaker20;
use crate::{fail, ErrorKind, Result, FRAME_SAMPLES, OUTPUT_CHANNELS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Speaker20,
    Binaural,
    /// Reconstructed `[16][1536]` frames, for the comparison harness.
    Objects16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinauralProfile {
    Near = 0,
    Mid = 1,
    Far = 2,
}

pub struct StreamConfig {
    pub mode: Mode,
    pub gain_db: f64,
    pub object_delay_samples: u32,
    pub tail_seconds: f64,
    pub binaural_profile: BinauralProfile,
    /// `.jochrtf` bytes; required for binaural.
    pub hrtf: Option<Vec<u8>>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Speaker20,
            gain_db: 0.0,
            object_delay_samples: 1473,
            tail_seconds: 5.0,
            binaural_profile: BinauralProfile::Mid,
            hrtf: None,
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct StreamInfo {
    pub frames_in: u64,
    pub frames_out: u64,
    pub samples_in: u64,
    pub samples_out: u64,
    pub output_channels: u32,
    pub missing_joc_frames: u64,
    pub objects_clamped_frames: u64,
    pub chanmap_pair_mismatch_frames: u64,
    pub bed_mask_mismatch_frames: u64,
    pub oamd_payloads: u64,
    pub peak: f64,
    pub over_unity: u64,
    pub non_finite: u64,
    pub ended: bool,
}

struct FrameMetadata {
    streams: Vec<Frame>,
    core_frame: Option<Frame>,
    has_core: bool,
    has_dependent: bool,
    has_chanmap: bool,
    chanmap: u16,
    params: FrameParams,
    has_joc: bool,
    has_update: bool,
    update: OamdUpdate,
    bed_assignments: Vec<BedAssignment>,
    outer_offset: i64,
    syncframe_offset: usize,
    syncframe_size: usize,
}

impl FrameMetadata {
    fn new() -> Self {
        Self {
            streams: Vec::new(),
            core_frame: None,
            has_core: false,
            has_dependent: false,
            has_chanmap: false,
            chanmap: 0,
            params: FrameParams::default(),
            has_joc: false,
            has_update: false,
            update: OamdUpdate::new(),
            bed_assignments: Vec::new(),
            outer_offset: 0,
            syncframe_offset: 0,
            syncframe_size: 0,
        }
    }
}

pub struct Stream {
    config: StreamConfig,
    reader: FrameReader,
    pending_frame: FrameMetadata,
    metadata: VecDeque<FrameMetadata>,
    bed_pending: Vec<f32>,
    bed_read_offset: usize,
    bed_plan: BedPlan,
    bed_channels: usize,
    config_idx: i32,
    core_channels: i32,
    bed_plan_failed: Option<crate::Error>,
    bed_input_channels: usize,
    gain: f32,
    kernel: JocKernel,
    speaker: Speaker20,
    binaural: Option<BinauralRuntime>,
    bed5: Vec<f32>,
    lfe: Vec<f32>,
    objects16: Vec<f32>,
    output: Vec<f32>,
    read_offset: usize,
    objects_queue: Vec<f32>,
    info: StreamInfo,
}

impl Stream {
    pub fn new(config: StreamConfig) -> Result<Self> {
        let gain = 10f64.powf(config.gain_db / 20.0) as f32;
        let binaural = if config.mode == Mode::Binaural {
            let Some(bytes) = config.hrtf.as_deref() else {
                return fail(
                    ErrorKind::HrtfNotFound,
                    "render",
                    "binaural playback needs a compiled .jochrtf file",
                );
            };
            let field: Field = jochrtf::load_jochrtf(bytes)?;
            let kernels = jochrtf::builtin_kernels();
            let mut runtime = BinauralRuntime::new();
            runtime.open(&field, &kernels, config.binaural_profile as u32)?;
            Some(runtime)
        } else {
            None
        };
        let output_channels = if binaural.is_some() { 2 } else { 2 };
        Ok(Self {
            config,
            reader: FrameReader::new(),
            pending_frame: FrameMetadata::new(),
            metadata: VecDeque::new(),
            bed_pending: Vec::new(),
            bed_read_offset: 0,
            bed_plan: BedPlan::default(),
            bed_channels: 0,
            config_idx: -1,
            core_channels: 0,
            bed_plan_failed: None,
            bed_input_channels: 0,
            gain,
            kernel: JocKernel::new(),
            speaker: Speaker20::new(),
            binaural,
            bed5: Vec::new(),
            lfe: Vec::new(),
            objects16: vec![0.0; OUTPUT_CHANNELS * FRAME_SAMPLES],
            output: Vec::new(),
            read_offset: 0,
            objects_queue: Vec::new(),
            info: StreamInfo { output_channels, ..StreamInfo::default() },
        })
    }

    pub fn info(&self) -> StreamInfo {
        self.info
    }

    /// Updates the output gain; the next rendered frame uses the new value.
    pub fn set_gain_db(&mut self, gain_db: f64) {
        self.config.gain_db = gain_db;
        self.gain = 10f64.powf(gain_db / 20.0) as f32;
    }

    pub fn reset(&mut self) {
        self.reader = FrameReader::new();
        self.metadata.clear();
        self.pending_frame = FrameMetadata::new();
        self.bed_plan = BedPlan::default();
        self.bed_channels = 0;
        self.config_idx = -1;
        self.core_channels = 0;
        self.bed_plan_failed = None;
        self.bed_input_channels = 0;
        self.bed_pending.clear();
        self.bed_read_offset = 0;
        self.output.clear();
        self.read_offset = 0;
        self.objects_queue.clear();
        self.objects16.fill(0.0);
        self.kernel.reset();
        self.speaker.reset();
        if let Some(binaural) = self.binaural.as_mut() {
            binaural.reset();
        }
        self.info.frames_in = 0;
        self.info.frames_out = 0;
        self.info.samples_in = 0;
        self.info.samples_out = 0;
        self.info.ended = false;
    }

    /// Feeds one E-AC-3 syncframe (the metadata stream).
    pub fn push_eac3(&mut self, data: &[u8]) -> Result<()> {
        self.reader.push(data);
        loop {
            let next = self.reader.next();
            let frame = match next {
                Next::End => break,
                Next::Fail(error) => return Err(error),
                Next::Frame(frame) => frame,
            };
            let bytes = self.reader.frame_at(&frame).to_vec();
            self.accept_frame(&frame, &bytes)?;
        }
        self.process_ready_frames()?;
        Ok(())
    }

    fn accept_frame(&mut self, frame: &Frame, bytes: &[u8]) -> Result<()> {
        let repeats = self.pending_frame.streams.iter().any(|prior| prior.same_stream(frame));
        if repeats {
            self.flush_pending_frame()?;
        }
        let mut header = *frame;
        header.size = frame.size;
        self.pending_frame.streams.push(header);
        if header.codec == 0 || header.strmtyp == 0 {
            self.pending_frame.core_frame = Some(header);
            self.pending_frame.has_core = true;
        } else {
            self.pending_frame.has_dependent = true;
            if header.has_chanmap {
                self.pending_frame.has_chanmap = true;
                self.pending_frame.chanmap = header.chanmap;
            }
        }

        let predicate = |container: &crate::emdf::Container| id14::score_container_as_joc(bytes, container);
        let found = crate::emdf::find_joc_emdf_optional(bytes, Some(&predicate))?;
        let Some((container, _ignored_extra)) = found else {
            return Ok(());
        };
        if container.find(crate::emdf::ID_JOC).is_none() {
            return Ok(());
        }
        let mut params = FrameParams::default();
        let mut parsed_container = crate::emdf::Container::default();
        id14::parse_eac3_frame(bytes, &mut params, Some(&mut parsed_container))?;
        self.pending_frame.params = params;
        self.pending_frame.has_joc = true;
        self.pending_frame.syncframe_offset = frame.offset;
        self.pending_frame.syncframe_size = frame.size;
        if let Some(payload) = container.find(crate::emdf::ID_OAMD) {
            let mut payload_bytes = Vec::new();
            crate::emdf::extract_payload_bytes(bytes, payload, &mut payload_bytes)?;
            let update = oamd::parse_id11(&payload_bytes)?;
            let assignments = oamd::parse_id11_bed_assignments(&payload_bytes)?;
            self.pending_frame.update = update;
            self.pending_frame.has_update = true;
            self.pending_frame.outer_offset = i64::from(payload.sample_offset);
            self.pending_frame.bed_assignments = assignments;
        }
        Ok(())
    }

    fn flush_pending_frame(&mut self) -> Result<()> {
        if self.pending_frame.streams.is_empty() {
            return Ok(());
        }
        let mut entry = core::mem::replace(&mut self.pending_frame, FrameMetadata::new());
        if entry.has_joc {
            let assignments = if entry.bed_assignments.is_empty() {
                None
            } else {
                Some(entry.bed_assignments.as_slice())
            };
            let notes: CrossLayerNotes = id14::cross_layer_for_frame(
                &entry.params,
                entry.core_frame.as_ref(),
                entry.has_core,
                entry.has_dependent,
                entry.has_chanmap,
                entry.chanmap,
                assignments,
            )?;
            if notes.chanmap_pair_mismatch {
                self.info.chanmap_pair_mismatch_frames += 1;
            }
            if notes.bed_mask_mismatch {
                self.info.bed_mask_mismatch_frames += 1;
            }
            if entry.params.objects_clamped {
                self.info.objects_clamped_frames += 1;
            }
        } else {
            self.info.missing_joc_frames += 1;
            entry.params = id14::make_silent_frame_params();
            entry.has_update = false;
        }
        if entry.has_update {
            self.info.oamd_payloads += 1;
        }
        self.metadata.push_back(entry);
        Ok(())
    }

    /// Feeds the core PCM of the pushed syncframes, interleaved 6 or 8 channels.
    pub fn push_pcm(&mut self, interleaved: &[f32], channels: usize) -> Result<()> {
        if channels != 6 && channels != 8 {
            return fail(
                ErrorKind::InvalidArgument,
                "dsp",
                format!("the core PCM must be interleaved as 6 or 8 channels, got {channels}"),
            );
        }
        if self.bed_input_channels == 0 {
            self.bed_input_channels = channels;
        } else if self.bed_input_channels != channels {
            return fail(
                ErrorKind::InvalidArgument,
                "dsp",
                format!(
                    "the core PCM channel count changed from {} to {channels}",
                    self.bed_input_channels
                ),
            );
        }
        self.bed_pending.extend_from_slice(interleaved);
        self.process_ready_frames()
    }

    fn resolve_bed_plan(&mut self) -> bool {
        if self.bed_channels != 0 {
            return true;
        }
        let Some(entry) = self.metadata.iter().find(|entry| entry.has_joc) else {
            return false;
        };
        let mut plan = BedPlan::default();
        let status = id14::compute_bed_plan(
            entry.core_frame.as_ref(),
            entry.has_core,
            entry.has_dependent,
            entry.has_chanmap,
            entry.chanmap,
            entry.params.dmx_config_idx,
            &mut plan,
        );
        if let Err(error) = status {
            self.bed_plan_failed = Some(error);
            return false;
        }
        self.bed_plan = plan;
        self.bed_channels = plan.channels;
        self.config_idx = i32::from(entry.params.dmx_config_idx);
        self.core_channels = i32::from(entry.params.n_channels);
        true
    }

    fn process_ready_frames(&mut self) -> Result<()> {
        if let Some(error) = &self.bed_plan_failed {
            return fail(error.kind, error.stage, error.message.clone());
        }
        self.resolve_bed_plan();
        if self.bed_channels == 0 {
            return Ok(());
        }
        if self.bed_input_channels != 0 && self.bed_input_channels != self.bed_channels {
            return fail(
                ErrorKind::JocUnsupportedVariant,
                "eac3",
                format!(
                    "core_channel_count_mismatch: the caller pushed {}-channel bed PCM but joc_dmx_config_idx={} decodes to {} channels",
                    self.bed_input_channels, self.config_idx, self.bed_channels
                ),
            );
        }
        while self.bed_pending.len() - self.bed_read_offset >= FRAME_SAMPLES * self.bed_channels
            && !self.metadata.is_empty()
        {
            let entry = self.metadata.pop_front().unwrap();
            let core_channels = if entry.has_joc {
                entry.params.n_channels as i32
            } else {
                self.core_channels
            };
            if core_channels <= 0 {
                return fail(ErrorKind::State, "dsp", "the JOC core channel count is not known yet");
            }
            let mut params = entry.params.clone();
            if !entry.has_joc {
                params.dmx_config_idx = self.config_idx as u8;
                params.n_channels = core_channels as u8;
            }
            let channels = self.bed_channels;
            self.bed5.clear();
            self.bed5.resize(core_channels as usize * FRAME_SAMPLES, 0.0);
            self.lfe.clear();
            self.lfe.resize(FRAME_SAMPLES, 0.0);
            let bed = &self.bed_pending[self.bed_read_offset..];
            for sample in 0..FRAME_SAMPLES {
                for lane in 0..core_channels as usize {
                    if let Some(source) = self.bed_plan.lane[lane] {
                        self.bed5[lane * FRAME_SAMPLES + sample] =
                            bed[sample * channels + source];
                    }
                }
                if let Some(channel) = self.bed_plan.lfe_channel {
                    self.lfe[sample] = bed[sample * channels + channel];
                }
            }
            self.bed_read_offset += FRAME_SAMPLES * channels;
            self.compact_bed_pending();

            objects16::rebuild(
                &mut self.kernel,
                &params,
                &self.bed5,
                &self.lfe,
                self.gain,
                &mut self.objects16,
            )?;
            self.render_objects(&entry, &params)?;
            self.info.frames_in += 1;
            self.info.samples_in += FRAME_SAMPLES as u64;
        }
        Ok(())
    }

    fn compact_bed_pending(&mut self) {
        if self.bed_read_offset == 0 {
            return;
        }
        if self.bed_read_offset == self.bed_pending.len() {
            self.bed_pending.clear();
            self.bed_read_offset = 0;
        } else if self.bed_read_offset >= 1 << 20 {
            self.bed_pending.drain(..self.bed_read_offset);
            self.bed_read_offset = 0;
        }
    }

    fn render_objects(&mut self, entry: &FrameMetadata, params: &FrameParams) -> Result<()> {
        let _ = params;
        let update = if entry.has_update { Some(&entry.update) } else { None };
        let frames_out = self.info.frames_out;
        if self.config.mode == Mode::Objects16 {
            self.objects_queue.extend_from_slice(&self.objects16);
            self.info.frames_out += 1;
            return Ok(());
        }
        if let Some(binaural) = self.binaural.as_mut() {
            binaural.submit_frame(
                &self.objects16,
                update,
                frames_out as i64,
                entry.outer_offset,
                i64::from(self.config.object_delay_samples),
            )?;
            let mut rendered = Vec::new();
            binaural.take_output(&mut rendered);
            self.info.samples_out += (rendered.len() / 2) as u64;
            self.output.reserve(rendered.len());
            for value in rendered {
                self.output.push(value as f32);
            }
        } else {
            let mut buffer = vec![0.0f64; FRAME_SAMPLES * 2];
            self.speaker.process(&self.objects16, update, &mut buffer);
            self.output.reserve(buffer.len());
            for value in buffer {
                self.output.push(value as f32);
            }
            self.info.samples_out += FRAME_SAMPLES as u64;
        }
        self.info.frames_out += 1;
        Ok(())
    }

    /// Copies whole reconstructed object frames (`[16][1536]` planar) out.
    pub fn pull_objects16(&mut self, out: &mut [f32]) -> Result<usize> {
        let available = self.objects_queue.len() / OUTPUT_CHANNELS;
        let count = available.min(out.len() / OUTPUT_CHANNELS) * OUTPUT_CHANNELS;
        if count != 0 {
            out[..count].copy_from_slice(&self.objects_queue[..count]);
            self.objects_queue.drain(..count);
        }
        Ok(count / OUTPUT_CHANNELS)
    }

    /// Copies interleaved stereo out, up to `frames` frames.  Returns the count.
    pub fn pull(&mut self, out: &mut [f32], frames: usize) -> Result<usize> {
        let available = (self.output.len() - self.read_offset) / 2;
        let count = available.min(frames);
        if count != 0 {
            let start = self.read_offset;
            let end = start + count * 2;
            out[..count * 2].copy_from_slice(&self.output[start..end]);
            for value in &out[..count * 2] {
                let magnitude = f64::from(value.abs());
                if !value.is_finite() {
                    self.info.non_finite += 1;
                } else {
                    if magnitude > self.info.peak {
                        self.info.peak = magnitude;
                    }
                    if magnitude > 1.0 {
                        self.info.over_unity += 1;
                    }
                }
            }
            self.read_offset = end;
            if self.read_offset == self.output.len() {
                self.output.clear();
                self.read_offset = 0;
            } else if self.read_offset >= 1 << 20 {
                self.output.drain(..self.read_offset);
                self.read_offset = 0;
            }
        }
        Ok(count)
    }

    pub fn buffered_frames(&self) -> usize {
        (self.output.len() - self.read_offset) / 2
    }

    /// Renders the frame group that is still pending, without ending the stream.
    /// The browser worker calls this at a fragment boundary: every syncframe of
    /// the group has been pushed by then, so the frame is complete.
    pub fn flush_pending(&mut self) -> Result<()> {
        self.flush_pending_frame()?;
        self.process_ready_frames()
    }

    /// End of input: drains the last grouped frame and the binaural room tail.
    pub fn flush(&mut self) -> Result<()> {
        self.flush_pending_frame()?;
        self.process_ready_frames()?;
        if let Some(binaural) = self.binaural.as_mut() {
            let capacity = binaural.finish_capacity(self.config.tail_seconds);
            let mut tail = Vec::new();
            binaural.finish(capacity, &mut tail)?;
            if !tail.is_empty() {
                self.output.reserve(tail.len());
                for value in tail {
                    self.output.push(value as f32);
                }
            }
        }
        self.info.ended = true;
        Ok(())
    }

    /// Number of output frames the binaural tail still holds (for UI progress).
    pub fn tail_capacity(&self) -> u32 {
        self.binaural
            .as_ref()
            .map(|runtime| runtime.finish_capacity(self.config.tail_seconds))
            .unwrap_or(0)
    }
}
