//! Minimal ZIP reader: enough for the `.jochrtf` container (stored or deflated
//! members, no encryption, no multi-disk).

use crate::{fail, ErrorKind, Result};
use std::io::Read;

pub struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

pub struct Entry {
    pub name: String,
    method: u16,
    compressed_size: usize,
    uncompressed_size: usize,
    local_offset: usize,
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

impl<'a> ZipArchive<'a> {
    pub fn open(data: &'a [u8]) -> Result<Self> {
        // End of central directory: 0x06054b50, scanned from the tail because the
        // comment length is variable.
        let mut eocd = None;
        let lower = data.len().saturating_sub(65_557);
        let mut index = data.len().saturating_sub(22);
        loop {
            if index < lower {
                break;
            }
            if u32_at(data, index) == 0x0605_4b50 {
                eocd = Some(index);
                break;
            }
            if index == 0 {
                break;
            }
            index -= 1;
        }
        let Some(eocd) = eocd else {
            return fail(ErrorKind::HrtfFormat, "render", "not a ZIP archive (no end record)");
        };
        let count = u16_at(data, eocd + 10) as usize;
        let directory_offset = u32_at(data, eocd + 16) as usize;
        let mut entries = Vec::with_capacity(count);
        let mut cursor = directory_offset;
        for _ in 0..count {
            if cursor + 46 > data.len() || u32_at(data, cursor) != 0x0201_4b50 {
                return fail(
                    ErrorKind::HrtfFormat,
                    "render",
                    "ZIP central directory is malformed",
                );
            }
            let method = u16_at(data, cursor + 10);
            let compressed_size = u32_at(data, cursor + 20) as usize;
            let uncompressed_size = u32_at(data, cursor + 24) as usize;
            let name_len = u16_at(data, cursor + 28) as usize;
            let extra_len = u16_at(data, cursor + 30) as usize;
            let comment_len = u16_at(data, cursor + 32) as usize;
            let local_offset = u32_at(data, cursor + 42) as usize;
            let name_bytes = &data[cursor + 46..cursor + 46 + name_len];
            let name = String::from_utf8_lossy(name_bytes).into_owned();
            entries.push(Entry {
                name,
                method,
                compressed_size,
                uncompressed_size,
                local_offset,
            });
            cursor += 46 + name_len + extra_len + comment_len;
        }
        Ok(Self { data, entries })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>> {
        let offset = entry.local_offset;
        if offset + 30 > self.data.len() || u32_at(self.data, offset) != 0x0403_4b50 {
            return fail(ErrorKind::HrtfFormat, "render", "ZIP local header is malformed");
        }
        let name_len = u16_at(self.data, offset + 26) as usize;
        let extra_len = u16_at(self.data, offset + 28) as usize;
        let start = offset + 30 + name_len + extra_len;
        let end = start + entry.compressed_size;
        if end > self.data.len() {
            return fail(ErrorKind::HrtfFormat, "render", "ZIP member extends past the file");
        }
        let raw = &self.data[start..end];
        match entry.method {
            0 => Ok(raw.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(entry.uncompressed_size);
                let mut decoder = flate2::read::DeflateDecoder::new(raw);
                decoder
                    .read_to_end(&mut out)
                    .map_err(|e| crate::Error::new(ErrorKind::HrtfFormat, "render", format!("deflate: {e}")))?;
                Ok(out)
            }
            method => fail(
                ErrorKind::HrtfFormat,
                "render",
                format!("unsupported ZIP compression method {method}"),
            ),
        }
    }
}
