//! MSB-first bit reader with the reference's truncation semantics.
//!
//! Reading past the limit is not a panic: it records `BitstreamTruncated` and
//! returns zero, and every caller checks [`BitReader::failed`] the way the
//! reference checks its reader.  A syntax error raised by a Huffman walk is kept
//! distinctly so the caller can report it as a syntax problem.

use crate::{Error, ErrorKind};

#[derive(Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    size_bits: usize,
    pos: usize,
    limit: usize,
    error: Option<Error>,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self::with_start(data, 0)
    }

    pub fn with_start(data: &'a [u8], start_bit: usize) -> Self {
        Self {
            data,
            size_bits: data.len() * 8,
            pos: start_bit,
            limit: data.len() * 8,
            error: None,
        }
    }

    pub fn set_limit_bits(&mut self, limit: usize) {
        self.limit = limit.min(self.size_bits);
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn remaining_bits(&self) -> usize {
        self.limit.saturating_sub(self.pos)
    }

    pub fn failed(&self) -> bool {
        self.error.is_some()
    }

    pub fn error(&self) -> Option<&Error> {
        self.error.as_ref()
    }

    pub fn take_error(&mut self) -> Option<Error> {
        self.error.take()
    }

    pub fn fail(&mut self, kind: ErrorKind, message: &'static str) {
        if self.error.is_none() {
            self.error = Some(Error::new(kind, "bitstream", message));
        }
    }

    pub fn can_read(&self, count: usize) -> bool {
        self.error.is_none() && count <= self.limit && self.pos <= self.limit - count
    }

    pub fn read(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let count = count as usize;
        if !self.can_read(count) {
            self.fail(ErrorKind::BitstreamTruncated, "bit read past end of buffer");
            return 0;
        }
        let mut value: u32 = 0;
        let mut remaining = count;
        if self.pos & 7 == 0 && remaining >= 8 {
            while remaining >= 8 {
                value = (value << 8) | u32::from(self.data[self.pos >> 3]);
                self.pos += 8;
                remaining -= 8;
            }
        }
        while remaining > 0 {
            let bit = (self.data[self.pos >> 3] >> (7 - (self.pos & 7))) & 1;
            value = (value << 1) | u32::from(bit);
            self.pos += 1;
            remaining -= 1;
        }
        value
    }

    pub fn skip(&mut self, count: usize) -> bool {
        if !self.can_read(count) {
            self.fail(ErrorKind::BitstreamTruncated, "bit read past end of buffer");
            return false;
        }
        self.pos += count;
        true
    }

    pub fn read_bytes(&mut self, out: &mut [u8]) -> bool {
        for slot in out.iter_mut() {
            *slot = self.read(8) as u8;
            if self.failed() {
                return false;
            }
        }
        true
    }
}

/// Extension-coded integer: `width` bits, then a continuation bit, repeated up to
/// `max_groups` times.  A chain that never terminates is a syntax error.
pub fn variable_bits(reader: &mut BitReader<'_>, width: u32, max_groups: u32) -> Option<u32> {
    let mut value: u32 = 0;
    for _ in 0..max_groups {
        value += reader.read(width);
        if reader.failed() {
            return None;
        }
        let more = reader.read(1);
        if reader.failed() {
            return None;
        }
        if more == 0 {
            return Some(value);
        }
        value = (value + 1) << width;
    }
    reader.fail(ErrorKind::EmdfSyntax, "variable_bits extension groups exceeded");
    None
}
