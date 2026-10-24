//! E-AC-3 syncframe transport: header parsing and the incremental frame walker.
//!
//! A frame whose header does not fit is *not* silently resynchronised on the next
//! `0x0B77`; the reader fails, exactly like the reference.

use crate::{Error, ErrorKind};

pub const SYNCWORD: u16 = 0x0B77;

const AC3_FRAME_WORDS: [[u16; 38]; 3] = [
    [
        64, 64, 80, 80, 96, 96, 112, 112, 128, 128, 160, 160, 192, 192, 224, 224, 256, 256, 320,
        320, 384, 384, 448, 448, 512, 512, 640, 640, 768, 768, 896, 896, 1024, 1024, 1152, 1152,
        1280, 1280,
    ],
    [
        69, 70, 87, 88, 104, 105, 121, 122, 139, 140, 174, 175, 208, 209, 243, 244, 278, 278, 348,
        348, 417, 418, 487, 487, 557, 557, 696, 696, 835, 835, 975, 975, 1114, 1114, 1253, 1253,
        1393, 1393,
    ],
    [
        96, 96, 120, 120, 144, 144, 168, 168, 192, 192, 240, 240, 288, 288, 336, 336, 384, 384, 480,
        480, 576, 576, 672, 672, 768, 768, 960, 960, 1152, 1152, 1344, 1344, 1536, 1536, 1728,
        1728, 1920, 1920,
    ],
];

/// One syncframe header.  `data` is reached through [`FrameReader::data`] so the
/// walker can keep owning its buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub offset: usize,
    pub size: usize,
    pub codec: u8,
    pub strmtyp: u8,
    pub substreamid: u8,
    pub acmod: u8,
    pub lfeon: u8,
    pub bsid: u8,
    pub has_chanmap: bool,
    pub chanmap: u16,
}

impl Frame {
    pub fn same_stream(&self, other: &Frame) -> bool {
        self.codec == other.codec && self.strmtyp == other.strmtyp && self.substreamid == other.substreamid
    }
}

fn read_bits(data: &[u8], start: usize, count: usize) -> u32 {
    let mut value = 0u32;
    for index in start..start + count {
        let bit = (data[index >> 3] >> (7 - (index & 7))) & 1;
        value = (value << 1) | u32::from(bit);
    }
    value
}

/// Parses the syncframe header at `offset`.  `None` means the syncword is missing
/// or the declared size cannot be derived.
pub fn parse_syncframe_header(data: &[u8], offset: usize) -> Option<Frame> {
    if offset + 6 > data.len() {
        return None;
    }
    if (u16::from(data[offset]) << 8 | u16::from(data[offset + 1])) != SYNCWORD {
        return None;
    }
    let mut frame = Frame { offset, ..Frame::default() };
    let base = offset * 8;
    let bsid = read_bits(data, base + 40, 5);
    frame.bsid = bsid as u8;

    if bsid <= 8 {
        frame.codec = 0;
        let fscod = read_bits(data, base + 32, 2) as usize;
        let frmsizecod = read_bits(data, base + 34, 6) as usize;
        if fscod > 2 || frmsizecod >= 38 {
            return None;
        }
        frame.size = usize::from(AC3_FRAME_WORDS[fscod][frmsizecod]) * 2;
        frame.acmod = read_bits(data, base + 48, 3) as u8;
        let mut bit = 51usize;
        if (frame.acmod & 1) != 0 && frame.acmod != 1 {
            bit += 2; // cmixlev
        }
        if (frame.acmod & 4) != 0 {
            bit += 2; // surmixlev
        }
        if frame.acmod == 2 {
            bit += 2; // dsurmod
        }
        frame.lfeon = read_bits(data, base + bit, 1) as u8;
        return Some(frame);
    }

    frame.codec = 1;
    frame.strmtyp = read_bits(data, base + 16, 2) as u8;
    frame.substreamid = read_bits(data, base + 18, 3) as u8;
    let frmsiz = read_bits(data, base + 21, 11) as usize;
    frame.size = (frmsiz + 1) * 2;
    // fscod==3 also consumes two bits (fscod2), so acmod always starts at bit 36.
    frame.acmod = read_bits(data, base + 36, 3) as u8;
    frame.lfeon = read_bits(data, base + 39, 1) as u8;
    let mut bit = 50usize; // bsid, then dialnorm
    let compre = read_bits(data, base + bit, 1);
    bit += 1;
    if compre != 0 {
        bit += 8;
    }
    if frame.acmod == 0 {
        bit += 5; // dialnorm2
        let compr2 = read_bits(data, base + bit, 1);
        bit += 1;
        if compr2 != 0 {
            bit += 8;
        }
    }
    if frame.strmtyp == 1 && read_bits(data, base + bit, 1) != 0 {
        frame.has_chanmap = true;
        frame.chanmap = read_bits(data, base + bit + 1, 16) as u16;
    }
    Some(frame)
}

pub enum Next {
    Frame(Frame),
    End,
    Fail(Error),
}

pub struct FrameReader {
    buffer: Vec<u8>,
    consumed: usize,
    base_offset: usize,
    finished: bool,
    failed: Option<Error>,
    frames_emitted: usize,
    stream_offset: usize,
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameReader {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            consumed: 0,
            base_offset: 0,
            finished: false,
            failed: None,
            frames_emitted: 0,
            stream_offset: 0,
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        if self.failed.is_some() || data.is_empty() {
            return;
        }
        if self.consumed > 0 {
            self.compact();
        }
        self.buffer.extend_from_slice(data);
    }

    pub fn finish(&mut self) {
        self.finished = true;
    }

    pub fn data(&self) -> &[u8] {
        &self.buffer
    }

    pub fn frame_at(&self, frame: &Frame) -> &[u8] {
        let start = frame.offset - self.base_offset;
        &self.buffer[start..start + frame.size]
    }

    pub fn frames_emitted(&self) -> usize {
        self.frames_emitted
    }

    pub fn stream_offset(&self) -> usize {
        self.stream_offset
    }

    fn compact(&mut self) {
        if self.consumed == 0 {
            return;
        }
        self.buffer.drain(..self.consumed);
        self.base_offset += self.consumed;
        self.consumed = 0;
    }

    pub fn next(&mut self) -> Next {
        if let Some(error) = self.failed.clone() {
            return Next::Fail(error);
        }
        let available = self.buffer.len() - self.consumed;
        if available == 0 {
            return Next::End;
        }
        if available < 4 {
            if self.finished {
                let error = Error::new(
                    ErrorKind::Eac3Syncframe,
                    "eac3",
                    "syncframe header truncated at end of input",
                );
                self.failed = Some(error.clone());
                return Next::Fail(error);
            }
            return Next::End;
        }
        let start = self.consumed;
        let Some(mut frame) = parse_syncframe_header(&self.buffer[start..], 0) else {
            let error = Error::new(
                ErrorKind::Eac3Syncframe,
                "eac3",
                "invalid syncframe header (silent resynchronisation is not allowed)",
            );
            self.failed = Some(error.clone());
            return Next::Fail(error);
        };
        if frame.size == 0 {
            let error = Error::new(
                ErrorKind::Eac3Syncframe,
                "eac3",
                "invalid syncframe header (silent resynchronisation is not allowed)",
            );
            self.failed = Some(error.clone());
            return Next::Fail(error);
        }
        if frame.size > available {
            if !self.finished {
                return Next::End;
            }
            let error = Error::new(
                ErrorKind::BitstreamTruncated,
                "eac3",
                format!(
                    "last syncframe extends past end of input (declared {} bytes, remaining {})",
                    frame.size, available
                ),
            );
            self.failed = Some(error.clone());
            return Next::Fail(error);
        }
        frame.offset = self.base_offset + self.consumed;
        self.consumed += frame.size;
        self.stream_offset = self.base_offset + self.consumed;
        self.frames_emitted += 1;
        Next::Frame(frame)
    }
}
