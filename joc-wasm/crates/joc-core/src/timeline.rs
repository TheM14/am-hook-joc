//! OAMD object-position timeline for the binaural path.
//!
//! The reference converts each OAMD update into an ADM position triple, then
//! builds per-object ramps whose start is shifted by the object delay.  The
//! timeline only answers monotonic queries, which is what the renderer does.

use crate::oamd::OamdUpdate;
use crate::{fail, ErrorKind, Result};

pub const OAMD_UPDATE_QUANTUM_SAMPLES: i64 = 64;
pub const TIMELINE_OBJECTS: usize = 15;

#[derive(Clone, Copy, Debug, Default)]
pub struct PositionTransition {
    pub start_sample: i64,
    pub duration_samples: i64,
    pub origin: [f64; 3],
    pub target: [f64; 3],
}

impl PositionTransition {
    fn end_sample(&self) -> i64 {
        self.start_sample + self.duration_samples
    }
}

#[derive(Clone, Debug)]
pub struct ObjectPositionTrack {
    initial: [f64; 3],
    last_target: [f64; 3],
    transitions: Vec<PositionTransition>,
    cursor: usize,
    last_query_sample: i64,
}

impl Default for ObjectPositionTrack {
    fn default() -> Self {
        Self {
            initial: [0.0; 3],
            last_target: [0.0; 3],
            transitions: Vec::new(),
            cursor: 0,
            last_query_sample: -1,
        }
    }
}

impl ObjectPositionTrack {
    pub fn set_initial(&mut self, position: &[f64; 3]) {
        self.initial = *position;
        self.last_target = *position;
    }

    pub fn append(
        &mut self,
        start_sample: i64,
        duration_samples: i64,
        target: &[f64; 3],
        object_index: usize,
    ) -> Result<()> {
        if start_sample < 0 || duration_samples < 0 {
            return fail(
                ErrorKind::InvalidArgument,
                "oamd",
                "position transition timing must be non-negative",
            );
        }
        if let Some(previous) = self.transitions.last().copied() {
            if start_sample < previous.end_sample() {
                return fail(
                    ErrorKind::OamdUnsupportedVariant,
                    "oamd",
                    format!(
                        "overlapping_binaural_position_ramps: object {object_index} has a new position update at sample {start_sample} before the previous ramp ends at {}",
                        previous.end_sample()
                    ),
                );
            }
            if start_sample == previous.start_sample && previous.duration_samples == 0 {
                self.transitions.last_mut().unwrap().duration_samples = duration_samples;
                self.transitions.last_mut().unwrap().target = *target;
                self.last_target = *target;
                return Ok(());
            }
        }
        self.transitions.push(PositionTransition {
            start_sample,
            duration_samples,
            origin: self.last_target,
            target: *target,
        });
        self.last_target = *target;
        Ok(())
    }

    pub fn position_at(&mut self, sample: i64) -> Result<[f64; 3]> {
        if sample < self.last_query_sample {
            return fail(
                ErrorKind::State,
                "oamd",
                "binaural metadata positions must be queried monotonically",
            );
        }
        self.last_query_sample = sample;
        while self.cursor < self.transitions.len() {
            let transition = self.transitions[self.cursor];
            if sample < transition.end_sample() {
                break;
            }
            self.initial = transition.target;
            self.cursor += 1;
        }
        if self.cursor >= self.transitions.len() {
            return Ok(self.initial);
        }
        let transition = self.transitions[self.cursor];
        if sample < transition.start_sample {
            return Ok(self.initial);
        }
        if transition.duration_samples == 0 {
            return Ok(transition.target);
        }
        let amount = (sample - transition.start_sample) as f64 / transition.duration_samples as f64;
        let mut out = [0.0f64; 3];
        for index in 0..3 {
            out[index] =
                transition.origin[index] + (transition.target[index] - transition.origin[index]) * amount;
        }
        Ok(out)
    }
}

/// `adm_atmos.q_to_adm_xyz`: OAMD Q15 coordinates to the ADM cartesian triple.
pub fn q_to_adm_xyz(q1: i16, q2: i16, q3: i16) -> [f64; 3] {
    let pos_x = (f64::round_ties_even(f64::from(q1) * 62.0 / 32767.0) / 62.0).min(1.0);
    let pos_y = (f64::round_ties_even(f64::from(q2) * 62.0 / 32767.0) / 62.0).min(1.0);
    let mut pos_z = f64::round_ties_even(f64::from(q3) * 15.0 / 32767.0) / 15.0;
    pos_z = pos_z.clamp(-1.0, 1.0);
    [pos_x * 2.0 - 1.0, 1.0 - pos_y * 2.0, pos_z]
}

pub struct OamdPositionTimeline {
    state: crate::oamd::OamdState,
    tracks: [ObjectPositionTrack; TIMELINE_OBJECTS],
    initialized: bool,
    previous_targets: [[f64; 3]; TIMELINE_OBJECTS],
    has_previous: [bool; TIMELINE_OBJECTS],
    payload_count: u64,
    transition_count: u64,
    last_coded_event_sample: i64,
}

impl Default for OamdPositionTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl OamdPositionTimeline {
    pub fn new() -> Self {
        Self {
            state: crate::oamd::OamdState::new(),
            tracks: core::array::from_fn(|_| ObjectPositionTrack::default()),
            initialized: false,
            previous_targets: [[0.0; 3]; TIMELINE_OBJECTS],
            has_previous: [false; TIMELINE_OBJECTS],
            payload_count: 0,
            transition_count: 0,
            last_coded_event_sample: -1,
        }
    }

    pub fn payload_count(&self) -> u64 {
        self.payload_count
    }

    pub fn transition_count(&self) -> u64 {
        self.transition_count
    }

    pub fn initialized(&self) -> bool {
        self.initialized
    }

    pub fn submit_update(
        &mut self,
        update: &OamdUpdate,
        frame_start_sample: i64,
        outer_sample_offset: i64,
        object_delay_samples: i64,
        processed_sample: i64,
    ) -> Result<()> {
        if frame_start_sample < 0 || outer_sample_offset < 0 || object_delay_samples < 0 {
            return fail(
                ErrorKind::InvalidArgument,
                "oamd",
                "OAMD frame, outer offset and object delay must be non-negative",
            );
        }
        self.state.apply(update);

        let mut targets = [[0.0f64; 3]; TIMELINE_OBJECTS];
        for index in 0..TIMELINE_OBJECTS {
            targets[index] = q_to_adm_xyz(
                self.state.q(index + 1, 0),
                self.state.q(index + 1, 1),
                self.state.q(index + 1, 2),
            );
        }

        let coded_event =
            frame_start_sample + outer_sample_offset + i64::from(update.block_offset_samples);
        if coded_event < self.last_coded_event_sample {
            return fail(
                ErrorKind::OamdUnsupportedVariant,
                "oamd",
                format!(
                    "non_monotonic_binaural_updates: event sample {coded_event} follows {}",
                    self.last_coded_event_sample
                ),
            );
        }
        self.last_coded_event_sample = coded_event;

        if !self.initialized {
            if processed_sample > 0 {
                return fail(
                    ErrorKind::OamdUnsupportedVariant,
                    "oamd",
                    format!(
                        "late_initial_binaural_state: the first OAMD state arrived after sample {processed_sample} had been processed, so sample 0 cannot be backfilled"
                    ),
                );
            }
            for index in 0..TIMELINE_OBJECTS {
                self.tracks[index].set_initial(&targets[index]);
                self.previous_targets[index] = targets[index];
                self.has_previous[index] = true;
            }
            self.initialized = true;
            self.payload_count += 1;
            return Ok(());
        }

        let ramp_duration = i64::from(update.ramp_duration_samples);
        let effective_ramp = if ramp_duration - OAMD_UPDATE_QUANTUM_SAMPLES > 0 {
            ramp_duration - OAMD_UPDATE_QUANTUM_SAMPLES
        } else {
            0
        };
        let mut transition_start = coded_event + object_delay_samples;
        if effective_ramp != 0 {
            transition_start += OAMD_UPDATE_QUANTUM_SAMPLES;
        }
        for index in 0..TIMELINE_OBJECTS {
            let changed =
                !self.has_previous[index] || self.previous_targets[index] != targets[index];
            if !changed {
                continue;
            }
            self.tracks[index].append(
                transition_start,
                effective_ramp,
                &targets[index],
                index + 1,
            )?;
            self.previous_targets[index] = targets[index];
            self.has_previous[index] = true;
            self.transition_count += 1;
        }
        self.payload_count += 1;
        Ok(())
    }

    pub fn positions_at(&mut self, sample: i64) -> Result<[[f64; 3]; TIMELINE_OBJECTS]> {
        let mut positions = [[0.0f64; 3]; TIMELINE_OBJECTS];
        for index in 0..TIMELINE_OBJECTS {
            positions[index] = self.tracks[index].position_at(sample)?;
        }
        Ok(positions)
    }
}
