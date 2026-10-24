//! EMDF ID14 (JOC) payload parsing, dequantisation and bed layout.
//!
//! `dq[dp][ch][pb]` holds the dequantised object matrix coefficients; the JOC
//! kernel consumes them directly.  The parser is a port of `joc_decode.py`
//! through the reference C++ implementation, including its bit-exact dequantiser
//! (same operand order and the same two roundings).

use crate::bits::BitReader;
use crate::eac3::Frame;
use crate::emdf::{self, Container};
use crate::huffman::{self, SymbolKind};
use crate::oamd::BedAssignment;
use crate::{
    fail, floored_mod, ErrorKind, Result, CORE_CHANNELS, MAX_CORE_CHANNELS, MAX_DPOINTS,
    MAX_OBJECTS, MAX_PARAMETER_BANDS,
};

fn num_channels_for_config(dmx_config_idx: u8) -> Option<u8> {
    match dmx_config_idx {
        0 | 3 => Some(5),
        1 | 2 | 4 => Some(7),
        _ => None,
    }
}

fn num_bands_for_index(num_bands_idx: u8) -> Option<u8> {
    match num_bands_idx {
        0 => Some(1),
        1 => Some(3),
        2 => Some(5),
        3 => Some(7),
        4 => Some(9),
        5 => Some(12),
        6 => Some(15),
        7 => Some(23),
        _ => None,
    }
}

#[derive(Clone)]
pub struct ObjectParams {
    pub present: bool,
    pub num_bands_idx: u8,
    pub n_bands: u8,
    pub sparse: bool,
    pub quant_idx: u8,
    pub slope_idx: u8,
    pub num_dpoints_bits: u8,
    pub n_dpoints: u8,
    pub offset_ts: [u8; MAX_DPOINTS],
    /// `[dpoint][core channel][parameter band]`
    pub dq: [[[f64; MAX_PARAMETER_BANDS]; MAX_CORE_CHANNELS]; MAX_DPOINTS],
}

impl Default for ObjectParams {
    fn default() -> Self {
        Self {
            present: false,
            num_bands_idx: 0,
            n_bands: 0,
            sparse: false,
            quant_idx: 0,
            slope_idx: 0,
            num_dpoints_bits: 0,
            n_dpoints: 0,
            offset_ts: [0; MAX_DPOINTS],
            dq: [[[0.0; MAX_PARAMETER_BANDS]; MAX_CORE_CHANNELS]; MAX_DPOINTS],
        }
    }
}

#[derive(Clone)]
pub struct FrameParams {
    pub dmx_config_idx: u8,
    pub num_objects_bits: u8,
    pub ext_config_idx: u8,
    pub n_objects: u8,
    pub n_channels: u8,
    pub clipgain_x_bits: u8,
    pub clipgain_y_bits: u8,
    pub objects_clamped: bool,
    pub seq_count: u32,
    pub present_mask: u32,
    pub data_end_bits: u32,
    pub trailing_bits: u32,
    pub tail_bytes: [u8; 8],
    pub clipgain: f64,
    pub objects: [ObjectParams; MAX_OBJECTS],
}

impl Default for FrameParams {
    fn default() -> Self {
        Self {
            dmx_config_idx: 0,
            num_objects_bits: 0,
            ext_config_idx: 0,
            n_objects: 0,
            n_channels: 0,
            clipgain_x_bits: 0,
            clipgain_y_bits: 0,
            objects_clamped: false,
            seq_count: 0,
            present_mask: 0,
            data_end_bits: 0,
            trailing_bits: 0,
            tail_bytes: [0; 8],
            clipgain: 0.0,
            objects: core::array::from_fn(|_| ObjectParams::default()),
        }
    }
}

/// `1 + (y/32) * 2^(x-4)` as the syntax defines it, with an exact power of two.
fn ldexp2(exponent: i32) -> f64 {
    if exponent >= 0 && exponent < 1024 {
        f64::from_bits((0x3FF + exponent as u64) << 52)
    } else if exponent < 0 && exponent > -1023 {
        f64::from_bits((0x3FF + exponent as u64) << 52)
    } else {
        2f64.powi(exponent)
    }
}

struct Symbols {
    mtx: [[[i16; MAX_PARAMETER_BANDS]; MAX_CORE_CHANNELS]; MAX_DPOINTS],
    idx: [[u8; MAX_PARAMETER_BANDS]; MAX_DPOINTS],
    vec: [[i16; MAX_PARAMETER_BANDS]; MAX_DPOINTS],
}

impl Default for Symbols {
    fn default() -> Self {
        Self {
            mtx: [[[0; MAX_PARAMETER_BANDS]; MAX_CORE_CHANNELS]; MAX_DPOINTS],
            idx: [[0; MAX_PARAMETER_BANDS]; MAX_DPOINTS],
            vec: [[0; MAX_PARAMETER_BANDS]; MAX_DPOINTS],
        }
    }
}

fn syntax<T>(message: impl Into<String>) -> Result<T> {
    fail(ErrorKind::JocSyntax, "joc", message)
}

fn truncated<T>(reader: &BitReader<'_>) -> Result<T> {
    if reader.error().map(|e| e.kind) == Some(ErrorKind::JocSyntax) {
        let message = reader.error().map(|e| e.message.clone()).unwrap_or_default();
        return syntax(message);
    }
    let detail = reader.error().map(|e| e.message.clone()).unwrap_or_default();
    fail(
        ErrorKind::BitstreamTruncated,
        "joc",
        format!("JOC bitstream truncated: {detail}"),
    )
}

fn reconstruct_dense(
    symbols: &Symbols,
    dp: usize,
    n_channels: usize,
    n_bands: usize,
    nquant: i64,
    offset: i64,
    q: &mut [[[i64; MAX_PARAMETER_BANDS]; MAX_CORE_CHANNELS]; MAX_DPOINTS],
) {
    for ch in 0..n_channels {
        q[dp][ch][0] =
            floored_mod(offset + i64::from(symbols.mtx[dp][ch][0]), nquant);
        for pb in 1..n_bands {
            q[dp][ch][pb] = floored_mod(
                q[dp][ch][pb - 1] + i64::from(symbols.mtx[dp][ch][pb]),
                nquant,
            );
        }
    }
}

fn reconstruct_sparse(
    symbols: &Symbols,
    dp: usize,
    n_channels: usize,
    n_bands: usize,
    nquant: i64,
    offset: i64,
    q: &mut [[[i64; MAX_PARAMETER_BANDS]; MAX_CORE_CHANNELS]; MAX_DPOINTS],
) -> Result<()> {
    if n_channels != 5 && n_channels != 7 {
        return fail(
            ErrorKind::JocUnsupportedVariant,
            "joc",
            format!("sparse JOC requires 5 or 7 core channels, got {n_channels}"),
        );
    }
    let initial_channel = i64::from(symbols.idx[dp][0]);
    if initial_channel < 0 || initial_channel >= n_channels as i64 {
        return syntax(format!(
            "sparse JOC initial channel {initial_channel} out of range for {n_channels} channels"
        ));
    }
    for ch in 0..n_channels {
        for pb in 0..n_bands {
            q[dp][ch][pb] = nquant / 2;
        }
    }
    let mut active = initial_channel as usize;
    let mut coefficient = offset;
    for pb in 0..n_bands {
        if pb != 0 {
            active = floored_mod(active as i64 + i64::from(symbols.idx[dp][pb]), n_channels as i64)
                as usize;
        }
        coefficient = floored_mod(coefficient + i64::from(symbols.vec[dp][pb]), nquant);
        q[dp][active][pb] = coefficient;
    }
    Ok(())
}

pub fn parse_id14(payload: &[u8], out: &mut FrameParams) -> Result<()> {
    *out = FrameParams::default();
    let mut reader = BitReader::new(payload);

    out.dmx_config_idx = reader.read(3) as u8;
    out.num_objects_bits = reader.read(6) as u8;
    out.ext_config_idx = reader.read(3) as u8;

    let declared_objects = u32::from(out.num_objects_bits) + 1;
    let Some(n_channels) = num_channels_for_config(out.dmx_config_idx) else {
        return syntax(format!(
            "unknown JOC downmix configuration {}",
            out.dmx_config_idx
        ));
    };
    out.n_channels = n_channels;
    out.objects_clamped = declared_objects > MAX_OBJECTS as u32;
    let n_objects = declared_objects.min(MAX_OBJECTS as u32);
    out.n_objects = n_objects as u8;

    out.clipgain_x_bits = reader.read(3) as u8;
    out.clipgain_y_bits = reader.read(5) as u8;
    out.seq_count = reader.read(10);
    out.clipgain = 1.0
        + f64::from(out.clipgain_y_bits) / 32.0
            * ldexp2(i32::from(out.clipgain_x_bits) - 4);

    for obj in 0..n_objects as usize {
        let present = reader.read(1) != 0;
        out.objects[obj].present = present;
        if !present {
            continue;
        }
        let info = &mut out.objects[obj];
        info.num_bands_idx = reader.read(3) as u8;
        let Some(bands) = num_bands_for_index(info.num_bands_idx) else {
            return syntax(format!("unknown JOC num_bands index {}", info.num_bands_idx));
        };
        info.n_bands = bands;
        info.sparse = reader.read(1) != 0;
        info.quant_idx = reader.read(1) as u8;
        info.slope_idx = reader.read(1) as u8;
        info.num_dpoints_bits = reader.read(1) as u8;
        info.n_dpoints = info.num_dpoints_bits + 1;
        if info.slope_idx == 1 {
            for dp in 0..info.n_dpoints as usize {
                info.offset_ts[dp] = (reader.read(5) + 1) as u8;
            }
        }
    }
    if reader.failed() {
        return truncated(&reader);
    }

    // Second pass: read the Huffman-coded coefficients.
    let mut symbols: [Symbols; MAX_OBJECTS] = core::array::from_fn(|_| Symbols::default());
    for obj in 0..n_objects as usize {
        if !out.objects[obj].present {
            continue;
        }
        let info = &out.objects[obj];
        let symbol = &mut symbols[obj];
        for dp in 0..info.n_dpoints as usize {
            if info.sparse {
                let first = reader.read(3);
                if reader.failed() {
                    return truncated(&reader);
                }
                symbol.idx[dp][0] = first as u8;
                for pb in 1..info.n_bands as usize {
                    let Some(value) =
                        huffman::decode(info.quant_idx, SymbolKind::Idx, n_channels, &mut reader)
                    else {
                        return truncated(&reader);
                    };
                    symbol.idx[dp][pb] = value as u8;
                }
                for pb in 0..info.n_bands as usize {
                    let Some(value) =
                        huffman::decode(info.quant_idx, SymbolKind::Vec, n_channels, &mut reader)
                    else {
                        return truncated(&reader);
                    };
                    symbol.vec[dp][pb] = value;
                }
            } else {
                for ch in 0..n_channels as usize {
                    for pb in 0..info.n_bands as usize {
                        let Some(value) = huffman::decode(
                            info.quant_idx,
                            SymbolKind::Mtx,
                            n_channels,
                            &mut reader,
                        ) else {
                            return truncated(&reader);
                        };
                        symbol.mtx[dp][ch][pb] = value;
                    }
                }
            }
        }
    }

    out.data_end_bits = reader.position() as u32;
    out.trailing_bits = (payload.len() * 8).saturating_sub(reader.position()) as u32;
    let tail_offset = reader.position() / 8;
    if tail_offset < payload.len() {
        let tail_bytes = (payload.len() - tail_offset).min(8);
        out.tail_bytes[..tail_bytes]
            .copy_from_slice(&payload[tail_offset..tail_offset + tail_bytes]);
    }

    for obj in 0..n_objects as usize {
        if !out.objects[obj].present {
            continue;
        }
        out.present_mask |= 1 << obj;
        let info = &out.objects[obj];
        let n_bands = info.n_bands as usize;
        let n_points = info.n_dpoints as usize;
        let nquant: i64 = if info.quant_idx == 0 { 96 } else { 192 };
        let mut q = [[[0i64; MAX_PARAMETER_BANDS]; MAX_CORE_CHANNELS]; MAX_DPOINTS];
        for dp in 0..n_points {
            if info.sparse {
                let offset = if info.quant_idx == 0 { 50 } else { 100 };
                reconstruct_sparse(
                    &symbols[obj],
                    dp,
                    n_channels as usize,
                    n_bands,
                    nquant,
                    offset,
                    &mut q,
                )?;
            } else {
                let offset = if info.quant_idx == 0 { 48 } else { 96 };
                reconstruct_dense(
                    &symbols[obj],
                    dp,
                    n_channels as usize,
                    n_bands,
                    nquant,
                    offset,
                    &mut q,
                );
            }
        }
        let nquant_half = nquant as f64 / 2.0;
        let denominator = 4096.0 * f64::from(1 + i32::from(info.quant_idx));
        for dp in 0..n_points {
            for ch in 0..n_channels as usize {
                for pb in 0..n_bands {
                    let value = q[dp][ch][pb] as f64 - nquant_half;
                    out.objects[obj].dq[dp][ch][pb] = value * 820.0 / denominator;
                }
            }
        }
    }
    Ok(())
}

/// Scores a candidate container as the frame's JOC metadata.
pub fn score_container_as_joc(frame: &[u8], container: &Container) -> i32 {
    let Some(payload) = container.find(emdf::ID_JOC) else {
        return 0;
    };
    let Some(oamd) = container.find(emdf::ID_OAMD) else {
        return 0;
    };
    let mut parses = false;
    let mut bytes = Vec::new();
    if emdf::extract_payload_bytes(frame, payload, &mut bytes).is_ok() {
        let mut params = FrameParams::default();
        parses = parse_id14(&bytes, &mut params).is_ok();
    }
    let associated = oamd.has_group_id
        && payload.has_group_id
        && oamd.group_id == payload.group_id;
    if associated && parses {
        2
    } else if parses {
        1
    } else {
        0
    }
}

pub fn parse_eac3_frame(
    frame: &[u8],
    out: &mut FrameParams,
    container: Option<&mut Container>,
) -> Result<()> {
    let predicate = |candidate: &Container| score_container_as_joc(frame, candidate);
    let (found, _ignored) = emdf::find_joc_emdf(frame, Some(&predicate))?;
    if let Some(slot) = container {
        *slot = found.clone();
    }
    let Some(payload) = found.find(emdf::ID_JOC) else {
        return fail(
            ErrorKind::EmdfTransport,
            "emdf",
            "EMDF container has no ID14 (JOC) payload",
        );
    };
    let mut bytes = Vec::new();
    emdf::extract_payload_bytes(frame, payload, &mut bytes)?;
    parse_id14(&bytes, out)
}

pub fn check_id14_padding(payload: &[u8]) -> Result<u32> {
    let mut params = FrameParams::default();
    parse_id14(payload, &mut params)?;
    if params.trailing_bits > 7 {
        return fail(
            ErrorKind::BitstreamPadding,
            "joc",
            format!(
                "more than 7 bits left after joc_data ({})",
                params.trailing_bits
            ),
        );
    }
    for bit in params.data_end_bits as usize..payload.len() * 8 {
        if (payload[bit >> 3] >> (7 - (bit & 7))) & 1 != 0 {
            return fail(
                ErrorKind::BitstreamPadding,
                "joc",
                format!("non-zero trailing padding bit at {bit}"),
            );
        }
    }
    Ok(params.trailing_bits)
}

/// Every object is "present" with an all-zero matrix: the kernel renders silence
/// while its analysis and synthesis state keep advancing with the stream.
pub fn make_silent_frame_params() -> FrameParams {
    let mut params = FrameParams::default();
    params.dmx_config_idx = 0;
    params.n_channels = CORE_CHANNELS as u8;
    params.n_objects = MAX_OBJECTS as u8;
    params.present_mask = (1 << MAX_OBJECTS) - 1;
    params.clipgain = 1.0;
    for object in 0..MAX_OBJECTS {
        let info = &mut params.objects[object];
        info.present = true;
        info.num_bands_idx = 0;
        info.n_bands = 1;
        info.sparse = false;
        info.quant_idx = 0;
        info.slope_idx = 0;
        info.num_dpoints_bits = 0;
        info.n_dpoints = 1;
    }
    params
}

// ---------------------------------------------------------------------------
// Channel identity / bed layout
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Left,
    Right,
    Center,
    Lfe,
    LeftSurround,
    RightSurround,
    LeftRearSurround,
    RightRearSurround,
    VerticalHeightLeft,
    VerticalHeightRight,
    TopSurroundLeft,
    TopSurroundRight,
    CenterSurround,
    TopSurround,
    VerticalHeightCenter,
    Lfe2,
}

fn position_bit(position: Position) -> u32 {
    match position {
        Position::Left => 0x1,
        Position::Right => 0x2,
        Position::Center => 0x4,
        Position::Lfe => 0x8,
        Position::LeftRearSurround => 0x10,
        Position::RightRearSurround => 0x20,
        Position::CenterSurround => 0x100,
        Position::LeftSurround => 0x200,
        Position::RightSurround => 0x400,
        Position::TopSurround => 0x800,
        Position::VerticalHeightLeft => 0x1000,
        Position::VerticalHeightCenter => 0x2000,
        Position::VerticalHeightRight => 0x4000,
        Position::TopSurroundLeft => 0x8000,
        Position::TopSurroundRight => 0x20000,
        Position::Lfe2 => 0x0,
    }
}

fn push_unique(positions: &mut Vec<Position>, position: Position) {
    if !positions.contains(&position) {
        positions.push(position);
    }
}

/// Table E2.5; the field's most significant bit is table bit 0.
pub fn expand_chanmap(chanmap: u16) -> Vec<Position> {
    const SINGLES: [Option<Position>; 16] = [
        Some(Position::Left),
        Some(Position::Center),
        Some(Position::Right),
        Some(Position::LeftSurround),
        Some(Position::RightSurround),
        None,
        None,
        Some(Position::CenterSurround),
        Some(Position::TopSurround),
        None,
        None,
        None,
        Some(Position::VerticalHeightCenter),
        None,
        Some(Position::Lfe2),
        Some(Position::Lfe),
    ];
    let mut out = Vec::new();
    for bit in 0..16u32 {
        if (u32::from(chanmap) >> (15 - bit)) & 1 == 0 {
            continue;
        }
        match bit {
            5 => {
                out.push(Position::LeftSurround);
                out.push(Position::RightSurround);
            }
            6 => {
                out.push(Position::LeftRearSurround);
                out.push(Position::RightRearSurround);
            }
            9 | 10 => {}
            11 => {
                out.push(Position::VerticalHeightLeft);
                out.push(Position::VerticalHeightRight);
            }
            13 => {
                out.push(Position::TopSurroundLeft);
                out.push(Position::TopSurroundRight);
            }
            _ => {
                if let Some(position) = SINGLES[bit as usize] {
                    out.push(position);
                }
            }
        }
    }
    out
}

pub fn acmod_positions(acmod: u8) -> Vec<Position> {
    match acmod {
        0 | 1 => vec![Position::Center],
        2 => vec![Position::Left, Position::Right],
        3 => vec![Position::Left, Position::Center, Position::Right],
        4 => vec![Position::Left, Position::Right, Position::CenterSurround],
        5 => vec![
            Position::Left,
            Position::Center,
            Position::Right,
            Position::CenterSurround,
        ],
        6 => vec![
            Position::Left,
            Position::Right,
            Position::LeftSurround,
            Position::RightSurround,
        ],
        _ => vec![
            Position::Left,
            Position::Center,
            Position::Right,
            Position::LeftSurround,
            Position::RightSurround,
        ],
    }
}

pub fn expected_pair(dmx_config_idx: u8) -> Vec<Position> {
    match dmx_config_idx {
        1 => vec![Position::LeftRearSurround, Position::RightRearSurround],
        2 | 4 => vec![Position::VerticalHeightLeft, Position::VerticalHeightRight],
        _ => Vec::new(),
    }
}

pub fn chanmap_extra_pair(core: &[Position], chanmap: u16) -> Vec<Position> {
    let mut extra = Vec::new();
    for position in expand_chanmap(chanmap) {
        if !core.contains(&position) {
            push_unique(&mut extra, position);
        }
    }
    extra
}

pub fn bed_channels_for_core(core_channels: usize) -> usize {
    if core_channels == 7 {
        8
    } else {
        6
    }
}

/// `bed[lane] = decoded PCM channel index feeding JOC lane lane`.
#[derive(Clone, Copy, Debug)]
pub struct BedPlan {
    pub channels: usize,
    pub lfe_channel: Option<usize>,
    pub lane: [Option<usize>; MAX_CORE_CHANNELS],
    pub extra_pair_slot: [i32; 2],
}

impl Default for BedPlan {
    fn default() -> Self {
        Self {
            channels: 0,
            lfe_channel: None,
            lane: [None; MAX_CORE_CHANNELS],
            extra_pair_slot: [-1, -1],
        }
    }
}

fn variant_error<T>(stage: &'static str, name: &str, message: impl core::fmt::Display) -> Result<T> {
    let text = format!("{name}: {message}");
    fail(ErrorKind::JocUnsupportedVariant, stage, text)
}

const EXTRA_PAIR_SLOTS: [[i32; 2]; 5] = [[-1, -1], [6, 7], [8, 9], [-1, -1], [8, 9]];

pub fn compute_bed_plan(
    core_frame: Option<&Frame>,
    has_core: bool,
    has_dependent: bool,
    has_chanmap: bool,
    chanmap: u16,
    dmx_config_idx: u8,
    out: &mut BedPlan,
) -> Result<()> {
    let Some(core_channels) = num_channels_for_config(dmx_config_idx) else {
        return variant_error(
            "joc",
            "unsupported_configuration",
            format!("unknown JOC downmix configuration {dmx_config_idx}"),
        );
    };
    let mut core_positions = Vec::new();
    if has_core {
        if let Some(frame) = core_frame {
            core_positions = acmod_positions(frame.acmod);
            if frame.lfeon != 0 {
                core_positions.push(Position::Lfe);
            }
        }
    }
    let mut extra = Vec::new();
    let mut locations = core_positions.clone();
    if has_dependent && has_chanmap {
        let declared = expand_chanmap(chanmap);
        extra = chanmap_extra_pair(&core_positions, chanmap);
        locations.extend(declared);
    }

    let mut ordered: Vec<Position> = Vec::new();
    for position in locations {
        push_unique(&mut ordered, position);
    }
    ordered.sort_by_key(|p| position_bit(*p));
    for position in &ordered {
        if position_bit(*position) == 0 {
            return variant_error(
                "eac3",
                "bed_channel_mapping_unsupported",
                format!("unmapped channel position {position:?}"),
            );
        }
    }

    let mut plan = BedPlan { channels: ordered.len(), ..BedPlan::default() };
    let expected_channels = bed_channels_for_core(core_channels as usize);
    if plan.channels != expected_channels {
        return variant_error(
            "eac3",
            "core_channel_count_mismatch",
            format!(
                "the bed declares {} channels, configuration {dmx_config_idx} needs {expected_channels}",
                plan.channels
            ),
        );
    }
    for (index, position) in ordered.iter().enumerate() {
        if *position == Position::Lfe {
            plan.lfe_channel = Some(index);
        }
    }

    let mut wanted = vec![
        Position::Left,
        Position::Right,
        Position::Center,
        Position::LeftSurround,
        Position::RightSurround,
    ];
    wanted.extend(extra.iter().copied());
    if wanted.len() != core_channels as usize {
        return variant_error(
            "eac3",
            "chanmap_cfg_mismatch",
            format!(
                "the bed cannot supply {core_channels} JOC core channels (found {})",
                wanted.len()
            ),
        );
    }
    for (lane, position) in wanted.iter().enumerate() {
        let Some(found) = ordered.iter().position(|p| p == position) else {
            return variant_error(
                "eac3",
                "bed_channel_mapping_unsupported",
                format!("channel {position:?} is missing from the bed {ordered:?}"),
            );
        };
        plan.lane[lane] = Some(found);
    }
    if dmx_config_idx < 5 {
        plan.extra_pair_slot = EXTRA_PAIR_SLOTS[dmx_config_idx as usize];
    }
    *out = plan;
    Ok(())
}

pub fn compute_bed_plan_for_config(dmx_config_idx: u8, out: &mut BedPlan) -> Result<()> {
    let Some(core_channels) = num_channels_for_config(dmx_config_idx) else {
        return variant_error(
            "joc",
            "unsupported_configuration",
            format!("unknown JOC downmix configuration {dmx_config_idx}"),
        );
    };
    let extra = expected_pair(dmx_config_idx);
    let mut wanted = vec![
        Position::Left,
        Position::Right,
        Position::Center,
        Position::LeftSurround,
        Position::RightSurround,
    ];
    wanted.extend(extra.iter().copied());

    let mut ordered = vec![
        Position::Left,
        Position::Right,
        Position::Center,
        Position::Lfe,
        Position::LeftSurround,
        Position::RightSurround,
    ];
    ordered.extend(extra.iter().copied());
    ordered.sort_by_key(|p| position_bit(*p));
    ordered.dedup();

    let mut plan = BedPlan { channels: ordered.len(), ..BedPlan::default() };
    for (index, position) in ordered.iter().enumerate() {
        if *position == Position::Lfe {
            plan.lfe_channel = Some(index);
        }
    }
    for (lane, position) in wanted.iter().enumerate() {
        let Some(index) = ordered.iter().position(|p| p == position) else {
            return variant_error(
                "eac3",
                "bed_channel_mapping_unsupported",
                format!("channel {position:?} is missing from the bed {ordered:?}"),
            );
        };
        plan.lane[lane] = Some(index);
    }
    let _ = core_channels;
    if dmx_config_idx < 5 {
        plan.extra_pair_slot = EXTRA_PAIR_SLOTS[dmx_config_idx as usize];
    }
    *out = plan;
    Ok(())
}

#[derive(Default, Clone, Copy)]
pub struct CrossLayerNotes {
    pub chanmap_pair_mismatch: bool,
    pub bed_mask_mismatch: bool,
}

pub fn check_cross_layer(
    params: &FrameParams,
    core_frame: Option<&Frame>,
    has_core: bool,
    // Kept for parity with the reference; the DSP only needs the chanmap.
    _has_dependent: bool,
    has_chanmap: bool,
    chanmap: u16,
    bed_assignments: Option<&[BedAssignment]>,
    notes: &mut CrossLayerNotes,
) -> Result<()> {
    let config_idx = params.dmx_config_idx;
    let core_channels = params.n_channels;
    let wanted = expected_pair(config_idx);

    if params.ext_config_idx != 0 {
        return variant_error(
            "joc",
            "ext_config_idx_unsupported",
            format!(
                "joc_ext_config_idx={} has no mapping in this implementation",
                params.ext_config_idx
            ),
        );
    }
    if core_channels as usize != CORE_CHANNELS && core_channels as usize != MAX_CORE_CHANNELS {
        return variant_error(
            "joc",
            "unsupported_configuration",
            format!("unsupported JOC core channel count {core_channels}"),
        );
    }

    let mut core_positions = Vec::new();
    if has_core {
        if let Some(frame) = core_frame {
            core_positions = acmod_positions(frame.acmod);
            if frame.lfeon != 0 {
                core_positions.push(Position::Lfe);
            }
        }
    }

    if core_channels as usize == MAX_CORE_CHANNELS {
        if !has_core || !has_chanmap {
            return variant_error(
                "eac3",
                "core_extension_pair",
                "a 7-channel configuration needs both an independent substream and a \
                 dependent-substream chanmap",
            );
        }
        let pair = chanmap_extra_pair(&core_positions, chanmap);
        if pair != wanted {
            notes.chanmap_pair_mismatch = true;
        }
    } else if has_chanmap {
        let pair = chanmap_extra_pair(&core_positions, chanmap);
        if !pair.is_empty() {
            return variant_error(
                "eac3",
                "core_channel_count_mismatch",
                format!(
                    "a 5-channel configuration declares extra dependent channels {pair:?}"
                ),
            );
        }
    }

    let mut declared = core_positions;
    if has_chanmap {
        for position in expand_chanmap(chanmap) {
            push_unique(&mut declared, position);
        }
    }
    let mut full_band = 0;
    for position in &declared {
        if *position != Position::Lfe && *position != Position::Lfe2 {
            full_band += 1;
        }
    }
    if !declared.is_empty() && full_band != core_channels as usize {
        return variant_error(
            "eac3",
            "core_channel_count_mismatch",
            format!(
                "the substreams declare {full_band} full-band channels, joc_dmx_config_idx={config_idx} declares {core_channels}"
            ),
        );
    }

    if let Some(assignments) = bed_assignments {
        const BED_BIT_LEFT_REAR: u32 = 1 << 4;
        const BED_BIT_TOP_FRONT: u32 = 1 << 5;
        let mut declared_pair = Vec::new();
        for assignment in assignments {
            if !assignment.standard || assignment.mask == 0 {
                continue;
            }
            if assignment.mask & BED_BIT_LEFT_REAR != 0 {
                push_unique(&mut declared_pair, Position::LeftRearSurround);
                push_unique(&mut declared_pair, Position::RightRearSurround);
            }
            if assignment.mask & BED_BIT_TOP_FRONT != 0 {
                push_unique(&mut declared_pair, Position::VerticalHeightLeft);
                push_unique(&mut declared_pair, Position::VerticalHeightRight);
            }
        }
        if !declared_pair.is_empty() && declared_pair != wanted {
            notes.bed_mask_mismatch = true;
        }
    }
    Ok(())
}

/// The two checks the stream performs while grouping one audio frame.
pub fn cross_layer_for_frame(
    params: &FrameParams,
    core: Option<&Frame>,
    has_core: bool,
    has_dependent: bool,
    has_chanmap: bool,
    chanmap: u16,
    bed_assignments: Option<&[BedAssignment]>,
) -> Result<CrossLayerNotes> {
    let mut notes = CrossLayerNotes::default();
    check_cross_layer(
        params,
        core,
        has_core,
        has_dependent,
        has_chanmap,
        chanmap,
        bed_assignments,
        &mut notes,
    )?;
    Ok(notes)
}
