//! Minimal `.npy` reader for the members a `.jochrtf` carries.
//!
//! Supported dtypes: `<f8`, `<f4`, `<i2`, `<c16` and `<U<n>` (fixed-width
//! UCS-4LE text).  C-contiguous data only; the header syntax is the one NumPy
//! writes.

use crate::{fail, ErrorKind, Result};

pub struct NpyArray<'a> {
    pub descr: String,
    pub fortran_order: bool,
    pub shape: Vec<usize>,
    data: &'a [u8],
}

fn header_value<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("'{key}'");
    let position = header.find(&needle)?;
    let colon = header[position + needle.len()..].find(':')? + position + needle.len();
    Some(header[colon + 1..].trim_start())
}

impl<'a> NpyArray<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < 10 || &data[..6] != b"\x93NUMPY" {
            return fail(ErrorKind::HrtfFormat, "render", "not a .npy image");
        }
        let major = data[6];
        let (header_length, header_offset) = match major {
            1 => (u16::from_le_bytes([data[8], data[9]]) as usize, 10usize),
            2 | 3 => (
                u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize,
                12usize,
            ),
            other => {
                return fail(
                    ErrorKind::HrtfFormat,
                    "render",
                    format!("unsupported .npy version {other}"),
                )
            }
        };
        if header_offset + header_length > data.len() {
            return fail(ErrorKind::HrtfFormat, "render", "truncated .npy header");
        }
        let header = core::str::from_utf8(&data[header_offset..header_offset + header_length])
            .map_err(|_| crate::Error::new(ErrorKind::HrtfFormat, "render", "non-UTF-8 .npy header"))?;

        let descr_field = header_value(header, "descr")
            .ok_or_else(|| crate::Error::new(ErrorKind::HrtfFormat, "render", ".npy header without descr"))?;
        let first = descr_field.find('\'').ok_or_else(|| {
            crate::Error::new(ErrorKind::HrtfFormat, "render", ".npy descr is not quoted")
        })?;
        let second = descr_field[first + 1..]
            .find('\'')
            .map(|offset| offset + first + 1)
            .ok_or_else(|| {
                crate::Error::new(ErrorKind::HrtfFormat, "render", ".npy descr is not quoted")
            })?;
        let descr = descr_field[first + 1..second].to_string();

        let mut shape = Vec::new();
        if let Some(shape_field) = header_value(header, "shape") {
            let bytes = shape_field.as_bytes();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index].is_ascii_digit() {
                    let mut value = 0usize;
                    while index < bytes.len() && bytes[index].is_ascii_digit() {
                        value = value * 10 + usize::from(bytes[index] - b'0');
                        index += 1;
                    }
                    shape.push(value);
                } else if bytes[index] == b')' {
                    break;
                } else {
                    index += 1;
                }
            }
        }
        let fortran_order = header.contains("'fortran_order': True");
        let array = Self {
            descr,
            fortran_order,
            shape,
            data: &data[header_offset + header_length..],
        };
        let required = array.element_count() * array.element_size();
        if array.data.len() < required {
            return fail(
                ErrorKind::HrtfFormat,
                "render",
                format!(".npy payload truncated (need {required} bytes)"),
            );
        }
        Ok(array)
    }

    pub fn element_count(&self) -> usize {
        self.shape.iter().product()
    }

    pub fn element_size(&self) -> usize {
        match self.descr.as_str() {
            "<f8" | "=f8" | "<i8" | "=i8" => 8,
            "<f4" | "=f4" | "<i4" | "=i4" => 4,
            "<i2" | "=i2" => 2,
            "|u1" | "<u1" => 1,
            "<c16" | "=c16" => 16,
            _ => self.unicode_length().map_or(0, |length| length * 4),
        }
    }

    fn unicode_length(&self) -> Option<usize> {
        let mut chars = self.descr.chars();
        let mut first = chars.next()?;
        if first == '<' || first == '=' {
            first = chars.next()?;
        }
        if first != 'U' {
            return None;
        }
        let digits: String = chars.collect();
        digits.parse().ok()
    }

    pub fn dtype_is(&self, expected: &str) -> bool {
        self.descr == expected
    }

    pub fn shape_is(&self, expected: &[usize]) -> bool {
        self.shape == expected
    }

    pub fn to_f64(&self) -> Result<Vec<f64>> {
        let count = self.element_count();
        let mut out = Vec::with_capacity(count);
        match self.descr.as_str() {
            "<f8" | "=f8" => {
                for chunk in self.data[..count * 8].chunks_exact(8) {
                    out.push(f64::from_le_bytes(chunk.try_into().unwrap()));
                }
            }
            "<c16" | "=c16" => {
                for chunk in self.data[..count * 16].chunks_exact(8) {
                    out.push(f64::from_le_bytes(chunk.try_into().unwrap()));
                }
            }
            "<f4" | "=f4" => {
                for chunk in self.data[..count * 4].chunks_exact(4) {
                    out.push(f64::from(f32::from_le_bytes(chunk.try_into().unwrap())));
                }
            }
            "<i8" | "=i8" => {
                for chunk in self.data[..count * 8].chunks_exact(8) {
                    out.push(i64::from_le_bytes(chunk.try_into().unwrap()) as f64);
                }
            }
            "<i4" | "=i4" => {
                for chunk in self.data[..count * 4].chunks_exact(4) {
                    out.push(i32::from_le_bytes(chunk.try_into().unwrap()) as f64);
                }
            }
            "<i2" | "=i2" => {
                for chunk in self.data[..count * 2].chunks_exact(2) {
                    out.push(i16::from_le_bytes(chunk.try_into().unwrap()) as f64);
                }
            }
            other => {
                return fail(
                    ErrorKind::HrtfFormat,
                    "render",
                    format!("cannot convert .npy dtype {other} to double"),
                )
            }
        }
        Ok(out)
    }

    pub fn to_i16(&self) -> Result<Vec<i16>> {
        let count = self.element_count();
        let mut out = Vec::with_capacity(count);
        match self.descr.as_str() {
            "<i2" | "=i2" => {
                for chunk in self.data[..count * 2].chunks_exact(2) {
                    out.push(i16::from_le_bytes(chunk.try_into().unwrap()));
                }
            }
            "<i4" | "=i4" => {
                for chunk in self.data[..count * 4].chunks_exact(4) {
                    out.push(i32::from_le_bytes(chunk.try_into().unwrap()) as i16);
                }
            }
            "<i8" | "=i8" => {
                for chunk in self.data[..count * 8].chunks_exact(8) {
                    out.push(i64::from_le_bytes(chunk.try_into().unwrap()) as i16);
                }
            }
            other => {
                return fail(
                    ErrorKind::HrtfFormat,
                    "render",
                    format!("cannot convert .npy dtype {other} to int16"),
                )
            }
        }
        Ok(out)
    }

    /// Fixed-width UCS-4LE text, terminated by a zero code point.
    pub fn to_text(&self) -> Result<String> {
        let length = self
            .unicode_length()
            .ok_or_else(|| crate::Error::new(ErrorKind::HrtfFormat, "render", "not a unicode .npy"))?;
        let count = self.element_count().max(1);
        let bytes = &self.data[..count * length * 4];
        let mut text = String::new();
        for chunk in bytes.chunks_exact(4) {
            let code = u32::from_le_bytes(chunk.try_into().unwrap());
            if code == 0 {
                break;
            }
            text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
        }
        Ok(text)
    }
}
