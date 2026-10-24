//! EMDF ID11 (OAMD) parsing: object positions, program assignment and bed masks.

use crate::bits::BitReader;
use crate::{fail, ErrorKind, Result};

pub const MAX_SLOTS: usize = 16;
pub const OBJECTS: usize = 15;
pub const AXES: usize = 3;
const NQ12: i32 = 62;
const NQ3: i32 = 15;

#[derive(Clone, Copy, Debug, Default)]
pub struct SlotUpdate {
    /// `None` holds the previous position.
    pub q1: Option<i16>,
    pub q2: Option<i16>,
    pub q3: Option<i16>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OamdUpdate {
    pub slots: [SlotUpdate; MAX_SLOTS],
    pub block_offset_samples: u32,
    pub ramp_duration_samples: u32,
    pub object_count: u32,
    pub dynamic_object_only: bool,
    pub lfe_present: bool,
    pub num_bed_objects: u32,
    pub num_isf_objects: u32,
    pub num_dynamic_objects: i32,
}

impl OamdUpdate {
    pub fn new() -> Self {
        Self { num_dynamic_objects: -1, ..Self::default() }
    }
}

#[derive(Clone)]
pub struct OamdState {
    q: [[i16; AXES]; MAX_SLOTS],
}

impl Default for OamdState {
    fn default() -> Self {
        Self::new()
    }
}

impl OamdState {
    pub fn new() -> Self {
        let mut state = Self { q: [[0; AXES]; MAX_SLOTS] };
        state.reset();
        state
    }

    pub fn reset(&mut self) {
        self.q = [[0; AXES]; MAX_SLOTS];
        self.q[0][0] = 16384;
        self.q[0][1] = 16384;
    }

    pub fn apply(&mut self, update: &OamdUpdate) {
        for slot in 0..MAX_SLOTS {
            let value = &update.slots[slot];
            if let Some(q) = value.q1 {
                self.q[slot][0] = q;
            }
            if let Some(q) = value.q2 {
                self.q[slot][1] = q;
            }
            if let Some(q) = value.q3 {
                self.q[slot][2] = q;
            }
        }
    }

    pub fn q(&self, slot: usize, axis: usize) -> i16 {
        self.q[slot][axis]
    }

    /// Objects 1..15 as q15 triples, the layout the speaker renderer consumes.
    pub fn object_positions_q15(&self) -> [[u16; AXES]; OBJECTS] {
        let mut out = [[0u16; AXES]; OBJECTS];
        for object in 0..OBJECTS {
            for axis in 0..AXES {
                out[object][axis] = self.q[object + 1][axis] as u16;
            }
        }
        out
    }
}

/// `min(32767, floor(32768 k / n + 0.5))`, the reference's quantiser.
pub fn q_of(k: i32, n: i32) -> i16 {
    let value = (32768.0 * f64::from(k) / f64::from(n) + 0.5).floor();
    value.min(32767.0) as i16
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BedAssignment {
    pub lfe_only: bool,
    pub standard: bool,
    pub mask: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ParseTrace {
    pub bed_assignments: Vec<BedAssignment>,
    pub element_count: u32,
    pub object_element_count: u32,
    pub sample_offset_code: u32,
    pub sample_offset: u32,
    pub block_count: u32,
    pub parsed_end_bit: u32,
    pub reserved_data_not_present: bool,
}

fn syntax<T>(message: impl Into<String>) -> Result<T> {
    fail(ErrorKind::OamdSyntax, "oamd", message)
}

fn variant<T>(name: &str, message: impl core::fmt::Display) -> Result<T> {
    fail(
        ErrorKind::OamdUnsupportedVariant,
        "oamd",
        format!("{name}: {message}"),
    )
}

fn variable_bits_max(reader: &mut BitReader<'_>, width: u32, max_groups: u32) -> Option<u32> {
    let mut value = reader.read(width);
    if reader.failed() {
        return None;
    }
    let mut more = reader.read(1);
    if reader.failed() {
        return None;
    }
    let mut num_group = 1u32;
    if max_groups > num_group {
        if more != 0 {
            value = (value + 1) << width;
        }
        while more != 0 {
            value += reader.read(width);
            more = reader.read(1);
            if reader.failed() {
                return None;
            }
            if num_group >= max_groups {
                break;
            }
            if more != 0 {
                value = (value + 1) << width;
                num_group += 1;
            }
        }
    }
    Some(value)
}

const SAMPLE_OFFSET_INDEX: [i32; 4] = [8, 16, 18, 24];
const RAMP_DURATIONS: [i32; 3] = [0, 512, 1536];
const RAMP_DURATION_INDEX: [i32; 16] = [
    32, 64, 128, 256, 320, 480, 1000, 1001, 1024, 1600, 1601, 1602, 1920, 2000, 2002, 2048,
];
const OBJECT_ELEMENT_ID: u32 = 1;
const ISF_OBJECT_COUNTS: [i32; 6] = [4, 8, 10, 14, 15, 30];
const STANDARD_BED_CHANNEL_COUNT: [i32; 10] = [2, 1, 1, 2, 2, 2, 2, 2, 2, 1];

struct ProgramInfo {
    dynamic_object_only: bool,
    lfe_present: bool,
    num_bed_objects: u32,
    num_isf_objects: u32,
    num_dynamic_objects: i32,
    bed_assignments: Vec<BedAssignment>,
}

impl Default for ProgramInfo {
    fn default() -> Self {
        Self {
            dynamic_object_only: false,
            lfe_present: false,
            num_bed_objects: 0,
            num_isf_objects: 0,
            num_dynamic_objects: -1,
            bed_assignments: Vec::new(),
        }
    }
}

fn parse_program_assignment(reader: &mut BitReader<'_>, program: &mut ProgramInfo) -> Result<()> {
    program.dynamic_object_only = reader.read(1) != 0;
    if reader.failed() {
        return syntax("program_assignment truncated");
    }
    if program.dynamic_object_only {
        program.lfe_present = reader.read(1) != 0;
        if reader.failed() {
            return syntax("program_assignment truncated");
        }
        program.num_bed_objects = u32::from(program.lfe_present);
        return Ok(());
    }

    let mask = reader.read(4);
    if reader.failed() {
        return syntax("program_assignment truncated");
    }
    if mask & 1 != 0 {
        reader.read(1);
        let multi = reader.read(1);
        if reader.failed() {
            return syntax("program_assignment truncated");
        }
        let instances = if multi != 0 { reader.read(3) + 2 } else { 1 };
        for _ in 0..instances {
            let mut assignment = BedAssignment::default();
            if reader.read(1) != 0 {
                assignment.lfe_only = true;
                assignment.mask = 0;
                program.bed_assignments.push(assignment);
                program.num_bed_objects += 1;
                continue;
            }
            if reader.read(1) != 0 {
                assignment.standard = true;
                assignment.mask = reader.read(10);
                for bit in 0..10 {
                    if (assignment.mask >> bit) & 1 != 0 {
                        program.num_bed_objects += STANDARD_BED_CHANNEL_COUNT[bit] as u32;
                    }
                }
            } else {
                assignment.standard = false;
                assignment.mask = reader.read(17);
                for bit in 0..17 {
                    if (assignment.mask >> bit) & 1 != 0 {
                        program.num_bed_objects += 1;
                    }
                }
            }
            program.bed_assignments.push(assignment);
            if reader.failed() {
                return syntax("program_assignment truncated");
            }
        }
    }
    if mask & 2 != 0 {
        let isf_idx = reader.read(3);
        if reader.failed() {
            return syntax("program_assignment truncated");
        }
        program.num_isf_objects = if isf_idx < 6 {
            ISF_OBJECT_COUNTS[isf_idx as usize] as u32
        } else {
            0
        };
    }
    if mask & 4 != 0 {
        let mut count = reader.read(5);
        if count == 0x1F {
            count += reader.read(7);
        }
        if reader.failed() {
            return syntax("program_assignment truncated");
        }
        program.num_dynamic_objects = count as i32 + 1;
    }
    if mask & 8 != 0 {
        let reserved_bytes = reader.read(4) + 1;
        if !reader.skip(reserved_bytes as usize * 8) {
            return syntax("program_assignment reserved data truncated");
        }
    }
    Ok(())
}

struct ObjectElementInfo {
    sample_offset_code: u32,
    sample_offset: u32,
    block_count: u32,
    block_offset_samples: u32,
    ramp_duration_samples: u32,
    reserved_data_not_present: bool,
}

impl Default for ObjectElementInfo {
    fn default() -> Self {
        Self {
            sample_offset_code: 0,
            sample_offset: 0,
            block_count: 0,
            block_offset_samples: 0,
            ramp_duration_samples: 0,
            reserved_data_not_present: false,
        }
    }
}

fn parse_object_info_block(
    reader: &mut BitReader<'_>,
    in_bed_or_isf: bool,
    position: &mut [i32; 3],
    has_position: &mut bool,
) -> Result<()> {
    *has_position = false;
    let not_active = reader.read(1) != 0;
    if reader.failed() {
        return syntax("object_info_block truncated");
    }
    if !not_active {
        let gain_idx = reader.read(2);
        if gain_idx == 2 {
            reader.read(6);
        }
        let default_priority = reader.read(1) != 0;
        if !default_priority {
            reader.read(5);
        }
        if reader.failed() {
            return syntax("object_info_block truncated");
        }
    }
    let render_status = !not_active && !in_bed_or_isf;
    if render_status {
        let x = reader.read(6) as i32;
        let y = reader.read(6) as i32;
        let z_sign = reader.read(1) as i32;
        let z = reader.read(4) as i32;
        if reader.failed() {
            return syntax("object_info_block truncated");
        }
        position[0] = x;
        position[1] = y;
        position[2] = if z_sign != 0 { z } else { -z };
        *has_position = true;
        if reader.read(1) != 0 {
            if reader.read(1) == 0 {
                reader.read(4);
            }
        }
        reader.read(3);
        reader.read(1);
        let size_idx = reader.read(2);
        if size_idx == 1 {
            reader.read(5);
        } else if size_idx == 2 {
            reader.read(15);
        }
        if reader.read(1) != 0 {
            reader.read(3);
            reader.read(2);
        }
        reader.read(1);
        if reader.failed() {
            return syntax("object_info_block truncated");
        }
    }
    if reader.read(1) != 0 {
        let additional_bytes = reader.read(4) + 1;
        if !reader.skip(additional_bytes as usize * 8) {
            return syntax("object_info_block additional table truncated");
        }
    }
    Ok(())
}

fn parse_object_element(
    reader: &mut BitReader<'_>,
    object_count: u32,
    bed_isf_objects: u32,
    out: &mut ObjectElementInfo,
    update: &mut OamdUpdate,
) -> Result<()> {
    out.sample_offset_code = reader.read(2);
    if reader.failed() {
        return syntax("object_element truncated");
    }
    match out.sample_offset_code {
        0 => out.sample_offset = 0,
        1 => {
            let index = reader.read(2) as usize & 3;
            if reader.failed() {
                return syntax("object_element truncated");
            }
            out.sample_offset = SAMPLE_OFFSET_INDEX[index] as u32;
        }
        2 => out.sample_offset = reader.read(5),
        code => {
            return variant(
                "md_sample_offset_mode",
                format!("MD sample-offset mode {code} is not covered by the 16-slot model"),
            )
        }
    }
    out.block_count = reader.read(3) + 1;
    if reader.failed() {
        return syntax("object_element truncated");
    }
    for block in 0..out.block_count {
        let block_offset_factor = reader.read(6);
        let ramp_code = reader.read(2);
        let ramp_duration = if ramp_code == 3 {
            if reader.read(1) != 0 {
                let index = reader.read(4) as usize & 15;
                if reader.failed() {
                    return syntax("object_element truncated");
                }
                RAMP_DURATION_INDEX[index] as u32
            } else {
                reader.read(11)
            }
        } else {
            RAMP_DURATIONS[ramp_code as usize] as u32
        };
        if reader.failed() {
            return syntax("object_element truncated");
        }
        if block == 0 {
            out.block_offset_samples = out.sample_offset + block_offset_factor * 32;
            out.ramp_duration_samples = ramp_duration;
        }
    }
    out.reserved_data_not_present = reader.read(1) != 0;
    if !out.reserved_data_not_present {
        reader.read(5);
    }
    if reader.failed() {
        return syntax("object_element truncated");
    }

    for index in 0..object_count {
        let mut position = [0i32; 3];
        let mut has_position = false;
        let in_bed_or_isf = index < bed_isf_objects;
        parse_object_info_block(reader, in_bed_or_isf, &mut position, &mut has_position)?;
        if !has_position || index >= MAX_SLOTS as u32 {
            continue;
        }
        let [x, y, z] = position;
        update.slots[index as usize].q1 = if (0..=NQ12).contains(&x) {
            Some(q_of(x, NQ12))
        } else {
            None
        };
        update.slots[index as usize].q2 = if (0..=NQ12).contains(&y) {
            Some(q_of(y, NQ12))
        } else {
            None
        };
        update.slots[index as usize].q3 = if (0..=NQ3).contains(&z) {
            Some(q_of(z, NQ3))
        } else if z < 0 {
            Some(0)
        } else {
            None
        };
    }
    Ok(())
}

pub fn parse_id11(payload: &[u8]) -> Result<OamdUpdate> {
    parse_id11_verbose(payload, None)
}

pub fn parse_id11_bed_assignments(payload: &[u8]) -> Result<Vec<BedAssignment>> {
    if payload.is_empty() {
        return variant("header_truncated", "OAMD payload is empty");
    }
    let mut reader = BitReader::new(payload);
    let mut version = reader.read(2);
    if reader.failed() {
        return variant("header_truncated", "OAMD payload cannot hold the 2-bit version");
    }
    if version == 3 {
        version += reader.read(3);
        if reader.failed() {
            return syntax("OAMD version extension truncated");
        }
    }
    let mut object_count_bits = reader.read(5);
    if object_count_bits == 0x1F {
        object_count_bits += reader.read(7);
    }
    if reader.failed() {
        return syntax("OAMD object count truncated");
    }
    let _ = (version, object_count_bits);
    let mut program = ProgramInfo::default();
    parse_program_assignment(&mut reader, &mut program)?;
    Ok(program.bed_assignments)
}

pub fn parse_id11_verbose(payload: &[u8], trace: Option<&mut ParseTrace>) -> Result<OamdUpdate> {
    if payload.is_empty() {
        return variant("header_truncated", "OAMD payload is empty");
    }
    let mut update = OamdUpdate::new();
    let mut reader = BitReader::new(payload);

    let mut version = reader.read(2);
    if reader.failed() {
        return variant("header_truncated", "OAMD payload cannot hold the 2-bit version");
    }
    if version == 3 {
        version += reader.read(3);
        if reader.failed() {
            return syntax("OAMD version extension truncated");
        }
    }
    let mut object_count_bits = reader.read(5);
    if object_count_bits == 0x1F {
        object_count_bits += reader.read(7);
    }
    if reader.failed() {
        return syntax("OAMD object count truncated");
    }
    let object_count = object_count_bits + 1;

    let mut program = ProgramInfo::default();
    parse_program_assignment(&mut reader, &mut program)?;
    let alternate_present = reader.read(1);
    if reader.failed() {
        return syntax("OAMD alternate data flag truncated");
    }

    let mut element_count = reader.read(4);
    if element_count == 0xF {
        element_count += reader.read(5);
    }
    if reader.failed() {
        return syntax("OAMD element count truncated");
    }
    if element_count == 0 {
        return variant("missing_object_element", "OAMD declares no element");
    }
    let bed_isf_objects = program.num_bed_objects + program.num_isf_objects;

    let mut have_object_element = false;
    let mut object_element = ObjectElementInfo::default();
    let mut parsed_end_bit = 0usize;
    for ordinal in 0..element_count {
        let header_start = reader.position();
        let element_id = reader.read(4);
        let Some(size_field) = variable_bits_max(&mut reader, 4, 4) else {
            return syntax(format!("OAMD element header truncated at ordinal {ordinal}"));
        };
        let size_bytes = size_field + 1;
        let region_start = reader.position();
        let region_end = region_start + size_bytes as usize * 8;
        if region_end > reader.limit() {
            return variant(
                "element_bounds",
                format!(
                    "element {ordinal} (id {element_id}) declares {size_bytes} bytes, beyond the payload (header at bit {header_start})"
                ),
            );
        }
        if alternate_present != 0 {
            reader.read(4);
        }
        reader.read(1);
        if reader.failed() {
            return syntax(format!(
                "OAMD element control fields truncated at ordinal {ordinal}"
            ));
        }

        if element_id == OBJECT_ELEMENT_ID {
            if have_object_element {
                return variant(
                    "multiple_object_elements",
                    "OAMD contains several object elements",
                );
            }
            have_object_element = true;
            parse_object_element(&mut reader, object_count, bed_isf_objects, &mut object_element, &mut update)?;
            parsed_end_bit = reader.position();
            if reader.position() < region_end {
                let remaining = region_end - reader.position();
                if !reader.skip(remaining) {
                    return syntax("OAMD element skip past payload end");
                }
            }
        } else {
            let remaining = region_end.saturating_sub(reader.position());
            if !reader.skip(remaining) {
                return syntax("OAMD element skip past payload end");
            }
        }
        if reader.failed() {
            return syntax(format!("OAMD element parse failed at ordinal {ordinal}"));
        }
    }

    if !have_object_element {
        return variant("missing_object_element", "OAMD has no object element");
    }

    {
        let mut tail = BitReader::with_start(payload, reader.position());
        while tail.remaining_bits() > 0 {
            let chunk = tail.remaining_bits().min(32) as u32;
            if tail.read(chunk) != 0 {
                return fail(
                    ErrorKind::BitstreamPadding,
                    "oamd",
                    format!(
                        "OAMD payload padding is not zero (from bit {})",
                        reader.position()
                    ),
                );
            }
        }
    }

    if version != 0 {
        return variant(
            "oamd_version",
            format!("OAMD syntax version {version} is not covered"),
        );
    }
    if object_count > MAX_SLOTS as u32 {
        return variant(
            "object_count",
            format!("OAMD object count {object_count} exceeds the 16-slot model"),
        );
    }
    if object_element.block_count != 1 {
        return variant(
            "multiple_position_blocks",
            format!(
                "OAMD frame carries {} position blocks; a single frame_update cannot express that",
                object_element.block_count
            ),
        );
    }

    update.block_offset_samples = object_element.block_offset_samples;
    update.ramp_duration_samples = object_element.ramp_duration_samples;
    update.object_count = object_count;
    update.dynamic_object_only = program.dynamic_object_only;
    update.lfe_present = program.lfe_present;
    update.num_bed_objects = program.num_bed_objects;
    update.num_isf_objects = program.num_isf_objects;
    update.num_dynamic_objects = program.num_dynamic_objects;

    if let Some(trace) = trace {
        trace.bed_assignments = program.bed_assignments.clone();
        trace.element_count = element_count;
        trace.object_element_count = u32::from(have_object_element);
        trace.sample_offset_code = object_element.sample_offset_code;
        trace.sample_offset = object_element.sample_offset;
        trace.block_count = object_element.block_count;
        trace.reserved_data_not_present = object_element.reserved_data_not_present;
        trace.parsed_end_bit = parsed_end_bit as u32;
    }
    Ok(update)
}

