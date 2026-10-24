//! EMDF (Extensible Metadata Delivery Format) container parsing.
//!
//! The markers are searched over the eight global bit alignments, every candidate
//! is parsed, and a container that carries both ID11 (OAMD) and ID14 (JOC) is a
//! match.  Nested candidates inside a payload are dropped; several top-level
//! candidates are ambiguous unless a scorer picks exactly one.

use crate::bits::{variable_bits, BitReader};
use crate::{Error, ErrorKind, Result};

pub const SYNCWORD: u16 = 0x5838;
pub const ID_OAMD: u8 = 11;
pub const ID_JOC: u8 = 14;
pub const MAX_PAYLOADS: usize = 16;

#[derive(Clone, Copy, Debug, Default)]
pub struct Payload {
    pub id: u8,
    pub sample_offset: u16,
    /// MSB-first bit position of the payload bytes inside the syncframe.
    pub bit_offset: usize,
    pub size: usize,
    pub group_id: u8,
    pub has_group_id: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Container {
    pub start_bit: usize,
    pub raw_size: usize,
    pub payload_count: usize,
    pub payloads: [Payload; MAX_PAYLOADS],
}

impl Container {
    pub fn find(&self, id: u8) -> Option<&Payload> {
        self.payloads[..self.payload_count].iter().find(|p| p.id == id)
    }
}

fn syntax_fail<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::new(ErrorKind::EmdfSyntax, "emdf", message))
}

fn truncated<T>(reader: &BitReader<'_>) -> Result<T> {
    let detail = reader.error().map(|e| e.message.clone()).unwrap_or_default();
    Err(Error::new(
        ErrorKind::BitstreamTruncated,
        "emdf",
        format!("EMDF bitstream truncated: {detail}"),
    ))
}

fn map_variable<T>(reader: &BitReader<'_>) -> Result<T> {
    if reader.error().map(|e| e.kind) == Some(ErrorKind::BitstreamTruncated) {
        truncated(reader)
    } else {
        let message = reader.error().map(|e| e.message.clone()).unwrap_or_default();
        syntax_fail(message)
    }
}

pub fn parse_at(data: &[u8], start_bit: usize) -> Result<Container> {
    if start_bit + 16 > data.len() * 8 {
        return syntax_fail("EMDF syncword position beyond buffer");
    }
    let mut reader = BitReader::with_start(data, start_bit);
    if reader.read(16) != u32::from(SYNCWORD) {
        return syntax_fail(format!("EMDF syncword mismatch at bit {start_bit}"));
    }
    let length = reader.read(16) as usize;
    let body_start = reader.position();
    let body_end = body_start + length * 8;
    if body_end > reader.limit() {
        return syntax_fail(format!(
            "EMDF container length {length} exceeds buffer at bit {start_bit}"
        ));
    }
    reader.set_limit_bits(body_end);

    let mut version = reader.read(2);
    if version == 3 {
        let extra = match variable_bits(&mut reader, 2, 8) {
            Some(value) => value,
            None => return map_variable(&reader),
        };
        version += extra;
    }
    let mut key_id = reader.read(3);
    if key_id == 7 {
        let extra = match variable_bits(&mut reader, 3, 8) {
            Some(value) => value,
            None => return map_variable(&reader),
        };
        key_id += extra;
    }
    if reader.failed() {
        return truncated(&reader);
    }
    if version != 0 || key_id != 0 {
        return syntax_fail(format!("unsupported EMDF version/key_id {version}/{key_id}"));
    }

    let mut container = Container { start_bit, ..Container::default() };
    let mut terminated = false;

    while reader.position() + 5 <= body_end {
        let mut payload_id = reader.read(5);
        if reader.failed() {
            return truncated(&reader);
        }
        if payload_id == 0 {
            terminated = true;
            break;
        }
        if payload_id == 0x1F {
            let extra = match variable_bits(&mut reader, 5, 8) {
                Some(value) => value,
                None => return map_variable(&reader),
            };
            payload_id += extra;
        }
        if container.payloads[..container.payload_count]
            .iter()
            .any(|p| p.id == payload_id as u8)
        {
            return syntax_fail(format!("duplicate EMDF payload id {payload_id}"));
        }
        if container.payload_count >= MAX_PAYLOADS {
            return syntax_fail(format!("EMDF payload count exceeds {MAX_PAYLOADS}"));
        }

        let has_sample_offset = reader.read(1);
        let mut sample_offset = 0u16;
        if has_sample_offset != 0 {
            sample_offset = (reader.read(12) >> 1) as u16;
        }
        if reader.read(1) != 0 {
            if variable_bits(&mut reader, 11, 8).is_none() {
                return map_variable(&reader);
            }
        }
        let mut group_id = 0u8;
        let mut has_group_id = false;
        if reader.read(1) != 0 {
            match variable_bits(&mut reader, 2, 8) {
                Some(value) => {
                    group_id = value as u8;
                    has_group_id = true;
                }
                None => return map_variable(&reader),
            }
        }
        if reader.read(1) != 0 && !reader.skip(8) {
            return truncated(&reader);
        }
        if reader.read(1) == 0 {
            let mut frame_aligned = false;
            if has_sample_offset == 0 {
                frame_aligned = reader.read(1) != 0;
                if frame_aligned && !reader.skip(2) {
                    return truncated(&reader);
                }
            }
            if (has_sample_offset != 0 || frame_aligned) && !reader.skip(7) {
                return truncated(&reader);
            }
        }
        if reader.failed() {
            return truncated(&reader);
        }

        let payload_size = match variable_bits(&mut reader, 8, 8) {
            Some(value) => value as usize,
            None => return map_variable(&reader),
        };
        let payload_bits = payload_size * 8;
        if reader.position() + payload_bits > body_end {
            return syntax_fail(format!(
                "EMDF payload id {payload_id} extends past container body (size {payload_size} at bit {})",
                reader.position()
            ));
        }

        let index = container.payload_count;
        container.payload_count += 1;
        container.payloads[index] = Payload {
            id: payload_id as u8,
            sample_offset,
            bit_offset: reader.position(),
            size: payload_size,
            group_id,
            has_group_id,
        };
        if !reader.skip(payload_bits) {
            return truncated(&reader);
        }
    }

    if !terminated {
        return syntax_fail("EMDF container has no payload id 0 terminator");
    }
    container.raw_size = 4 + length;
    Ok(container)
}

/// All candidate `0x5838` bit offsets over the eight alignments, ascending.
pub fn marker_offsets(data: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let size = data.len();
    if size < 4 {
        return offsets;
    }
    for shift in 0..8usize {
        let aligned_len = if shift == 0 { size } else { size - 1 };
        if aligned_len < 2 {
            continue;
        }
        let aligned_byte = |index: usize| -> u8 {
            if shift == 0 {
                data[index]
            } else {
                let high = (u16::from(data[index]) << shift) as u16;
                let low = u16::from(data[index + 1]) >> (8 - shift);
                ((high | low) & 0xFF) as u8
            }
        };
        for index in 0..aligned_len - 1 {
            if aligned_byte(index) == 0x58 && aligned_byte(index + 1) == 0x38 {
                offsets.push(index * 8 + shift);
            }
        }
    }
    offsets.sort_unstable();
    offsets
}

struct FindFailure {
    error: Error,
    ambiguous: bool,
}

/// Scores a candidate container as the frame's JOC metadata (0 = not at all).
pub type ContainerPredicate<'a> = &'a dyn Fn(&Container) -> i32;

fn find_internal(
    data: &[u8],
    usable: Option<ContainerPredicate<'_>>,
) -> core::result::Result<(Container, bool), FindFailure> {
    let offsets = marker_offsets(data);
    let marker_count = offsets.len();
    let mut matches: Vec<Container> = Vec::new();
    let mut parse_errors = 0usize;
    let mut first_parse_error = String::new();
    for start_bit in offsets {
        match parse_at(data, start_bit) {
            Ok(candidate) => {
                if candidate.find(ID_OAMD).is_some() && candidate.find(ID_JOC).is_some() {
                    matches.push(candidate);
                }
            }
            Err(error) => {
                parse_errors += 1;
                if first_parse_error.is_empty() {
                    first_parse_error = format!("@bit{start_bit}: {}", error.message);
                }
            }
        }
    }
    if matches.is_empty() {
        let mut message = format!(
            "no contiguous EMDF container carrying ID11+ID14 in this syncframe (markers={marker_count}, parse_failures={parse_errors})"
        );
        if !first_parse_error.is_empty() {
            message.push_str(&format!("; first candidate error {first_parse_error}"));
        }
        return Err(FindFailure {
            error: Error::new(ErrorKind::EmdfTransport, "emdf", message),
            ambiguous: false,
        });
    }
    matches.sort_by_key(|c| c.start_bit);

    let mut top_level: Vec<Container> = Vec::new();
    for candidate in matches {
        let nested = top_level.iter().any(|parent| {
            parent.start_bit < candidate.start_bit
                && candidate.start_bit < parent.start_bit + parent.raw_size * 8
        });
        if !nested {
            top_level.push(candidate);
        }
    }

    if top_level.len() != 1 {
        if let Some(score) = usable {
            let mut best = 0;
            let mut winner: Option<&Container> = None;
            let mut winners = 0;
            for candidate in &top_level {
                let value = score(candidate);
                if value > best {
                    best = value;
                    winner = Some(candidate);
                    winners = 1;
                } else if value == best && value > 0 {
                    winners += 1;
                }
            }
            if best > 0 && winners == 1 {
                if let Some(winner) = winner {
                    return Ok((winner.clone(), true));
                }
            }
        }
        return Err(FindFailure {
            error: Error::new(
                ErrorKind::EmdfTransport,
                "emdf",
                format!(
                    "multiple top-level JOC EMDF containers ({}); automatic selection is not defined",
                    top_level.len()
                ),
            ),
            ambiguous: true,
        });
    }
    Ok((top_level.remove(0), false))
}

pub fn find_joc_emdf(data: &[u8], usable: Option<ContainerPredicate<'_>>) -> Result<(Container, bool)> {
    find_internal(data, usable).map_err(|failure| failure.error)
}

/// A syncframe without any usable JOC container is "absent" (normal for a core
/// substream); an ambiguous frame is still an error.
pub fn find_joc_emdf_optional(
    data: &[u8],
    usable: Option<ContainerPredicate<'_>>,
) -> Result<Option<(Container, bool)>> {
    match find_internal(data, usable) {
        Ok(found) => Ok(Some(found)),
        Err(failure) if !failure.ambiguous => Ok(None),
        Err(failure) => Err(failure.error),
    }
}

pub fn extract_payload_bytes(data: &[u8], payload: &Payload, out: &mut Vec<u8>) -> Result<()> {
    out.clear();
    out.resize(payload.size, 0);
    if payload.size == 0 {
        return Ok(());
    }
    let mut reader = BitReader::with_start(data, payload.bit_offset);
    if !reader.read_bytes(out) {
        return Err(Error::new(
            ErrorKind::BitstreamTruncated,
            "emdf",
            "payload bytes extend past the syncframe",
        ));
    }
    Ok(())
}
