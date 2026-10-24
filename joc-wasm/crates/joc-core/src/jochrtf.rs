//! `.jochrtf` reader: a compiled HRTF field.
//!
//! The container is a ZIP of five `.npy` members, and the metadata member carries
//! the magic, the schema, a source/cache key and a payload digest over the four
//! numeric members.  All of those are verified here, so a cache compiled for a
//! different filterbank or a truncated file fails instead of rendering wrong.

use crate::json;
use crate::npy::NpyArray;
use crate::zip::ZipArchive;
use crate::{fail, ErrorKind, Result};
use sha2::{Digest, Sha256};

pub const SH_TERMS: usize = 36;
pub const EARS: usize = 2;
pub const HYBRID_BANDS: usize = 77;
pub const FORMAT_VERSION: i64 = 1;
pub const MAGIC: &str = "JOC-HRTF-CACHE";
pub const CACHE_SCHEMA: &str = "joc-compiled-hrtf-v1";

#[derive(Clone, Debug, Default)]
pub struct Field {
    /// `[sh term][ear][band]` interleaved real/imaginary.
    pub coefficients: Vec<f64>,
    pub delay_coefficients: Vec<f64>,
    pub delay_bounds: Vec<f64>,
    pub band_centers_hz: Vec<f64>,
    pub measurement_radius_m: f64,
    pub order: i64,
    pub source_sha256: String,
    pub cache_key: String,
    pub payload_sha256: String,
    pub source_display_name: String,
}

fn hrtf<T>(message: impl Into<String>) -> Result<T> {
    fail(ErrorKind::HrtfFormat, "render", message)
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

fn payload_digest(
    centers: &[f64],
    coefficients: &[f64],
    delay_coefficients: &[f64],
    delay_bounds: &[f64],
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"JOC-HRTF-CACHE-PAYLOAD-V1");
    hash.update([0u8]);
    let entries: [(&str, &str, &[f64]); 4] = [
        ("band_center_frequencies_hz", "<f8", centers),
        ("coefficients", "<c16", coefficients),
        ("delay_coefficients", "<f8", delay_coefficients),
        ("delay_bounds", "<f8", delay_bounds),
    ];
    let shapes = ["[77]", "[36, 2, 77]", "[36, 2]", "[2, 2]"];
    for (index, (name, dtype, values)) in entries.iter().enumerate() {
        let shape_text = shapes[index];
        hash.update(name.as_bytes());
        hash.update([0u8]);
        hash.update(dtype.as_bytes());
        hash.update([0u8]);
        hash.update(shape_text.as_bytes());
        hash.update([0u8]);
        for value in values.iter() {
            hash.update(value.to_le_bytes());
        }
    }
    let digest = hash.finalize();
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push_str(&format!("{byte:02X}"));
    }
    text
}

pub fn load_jochrtf(data: &[u8]) -> Result<Field> {
    let archive = ZipArchive::open(data)?;
    const MEMBERS: [&str; 5] = [
        "metadata_json.npy",
        "band_center_frequencies_hz.npy",
        "coefficients.npy",
        "delay_coefficients.npy",
        "delay_bounds.npy",
    ];
    if archive.entries().len() != MEMBERS.len() {
        return hrtf(format!(
            "compiled HRTF cache has an invalid member set ({} members)",
            archive.entries().len()
        ));
    }
    for name in MEMBERS {
        if archive.find(name).is_none() {
            return hrtf(format!("compiled HRTF cache is missing {name}"));
        }
    }

    let metadata_raw = archive.read(archive.find("metadata_json.npy").unwrap())?;
    let metadata = NpyArray::parse(&metadata_raw)?;
    let metadata_text = metadata.to_text()?;
    if metadata_text.len() > 64 * 1024 {
        return hrtf("compiled HRTF metadata is too large");
    }

    let centers_raw = archive.read(archive.find("band_center_frequencies_hz.npy").unwrap())?;
    let centers = NpyArray::parse(&centers_raw)?;
    if !centers.dtype_is("<f8") || !centers.shape_is(&[HYBRID_BANDS]) {
        return hrtf("band_center_frequencies_hz must be <f8(77,)");
    }
    let centers = centers.to_f64()?;

    let coefficients_raw = archive.read(archive.find("coefficients.npy").unwrap())?;
    let coefficients = NpyArray::parse(&coefficients_raw)?;
    if !coefficients.dtype_is("<c16")
        || !coefficients.shape_is(&[SH_TERMS, EARS, HYBRID_BANDS])
    {
        return hrtf("coefficients must be <c16(36, 2, 77)");
    }
    let coefficients = coefficients.to_f64()?;

    let delay_coefficients_raw =
        archive.read(archive.find("delay_coefficients.npy").unwrap())?;
    let delay_coefficients = NpyArray::parse(&delay_coefficients_raw)?;
    if !delay_coefficients.dtype_is("<f8") || !delay_coefficients.shape_is(&[SH_TERMS, EARS]) {
        return hrtf("delay_coefficients must be <f8(36, 2)");
    }
    let delay_coefficients = delay_coefficients.to_f64()?;

    let delay_bounds_raw = archive.read(archive.find("delay_bounds.npy").unwrap())?;
    let delay_bounds = NpyArray::parse(&delay_bounds_raw)?;
    if !delay_bounds.dtype_is("<f8") || !delay_bounds.shape_is(&[2, 2]) {
        return hrtf("delay_bounds must be <f8(2, 2)");
    }
    let delay_bounds = delay_bounds.to_f64()?;

    let members = json::parse_object(&metadata_text)
        .map_err(|error| crate::Error::new(ErrorKind::HrtfFormat, "render", format!("compiled HRTF metadata: {error}")))?;
    let require_string = |key: &str| -> Result<String> {
        json::find(&members, key)
            .and_then(|member| member.value.as_str().map(str::to_owned))
            .ok_or_else(|| {
                crate::Error::new(
                    ErrorKind::HrtfFormat,
                    "render",
                    format!("compiled HRTF metadata is missing {key}"),
                )
            })
    };
    let magic = require_string("magic")?;
    let schema = require_string("cache_schema")?;
    let source_sha256 = require_string("source_sha256")?;
    let cache_key = require_string("cache_key")?;
    let payload_hash = require_string("payload_sha256")?;
    let _delay_source = require_string("delay_source")?;

    if magic != MAGIC {
        return hrtf(format!("compiled HRTF magic mismatch: {magic}"));
    }
    if schema != CACHE_SCHEMA {
        return hrtf(format!("compiled HRTF cache schema mismatch: {schema}"));
    }
    let version = json::find(&members, "format_version")
        .and_then(|member| member.value.as_i64())
        .ok_or_else(|| {
            crate::Error::new(
                ErrorKind::HrtfFormat,
                "render",
                "compiled HRTF metadata is missing format_version",
            )
        })?;
    if version != FORMAT_VERSION {
        return fail(
            ErrorKind::HrtfVersion,
            "render",
            format!("unsupported .jochrtf version {version}; rebuild it from the source SOFA"),
        );
    }
    let source_sha256 = source_sha256.to_uppercase();
    let cache_key = cache_key.to_uppercase();
    if !is_sha256_hex(&source_sha256) {
        return hrtf("compiled HRTF source_sha256 is not a 64-digit digest");
    }
    if !is_sha256_hex(&cache_key) {
        return hrtf("compiled HRTF cache_key is not a 64-digit digest");
    }
    let expected = payload_digest(&centers, &coefficients, &delay_coefficients, &delay_bounds);
    if payload_hash.to_uppercase() != expected {
        return fail(
            ErrorKind::HrtfHash,
            "render",
            "compiled HRTF payload hash mismatch",
        );
    }

    let radius = json::find(&members, "measurement_radius_m")
        .and_then(|member| member.value.as_f64())
        .filter(|value| *value > 0.0)
        .ok_or_else(|| {
            crate::Error::new(
                ErrorKind::HrtfFormat,
                "render",
                "compiled HRTF measurement_radius_m must be a positive number",
            )
        })?;
    let order = json::find(&members, "order")
        .and_then(|member| member.value.as_i64())
        .filter(|value| *value > 0 && (*value * *value) as usize <= SH_TERMS)
        .ok_or_else(|| {
            crate::Error::new(ErrorKind::HrtfFormat, "render", "compiled HRTF order is out of range")
        })?;

    if coefficients.iter().any(|value| !value.is_finite()) {
        return hrtf("compiled HRTF coefficients contain non-finite values");
    }
    if delay_coefficients
        .iter()
        .any(|value| !value.is_finite() || value.abs() > 48000.0 * 64.0)
    {
        return hrtf("compiled HRTF delay coefficients are out of range");
    }
    if delay_bounds.iter().any(|value| !value.is_finite()) {
        return hrtf("compiled HRTF delay bounds contain non-finite values");
    }
    if delay_bounds.len() == 4 && delay_bounds[0] > delay_bounds[1] {
        return hrtf("compiled HRTF delay bounds are inverted");
    }

    let source_display_name = json::find(&members, "source_display_name")
        .and_then(|member| member.value.as_str().map(str::to_owned))
        .unwrap_or_default();

    Ok(Field {
        coefficients,
        delay_coefficients,
        delay_bounds,
        band_centers_hz: centers,
        measurement_radius_m: radius,
        order,
        source_sha256,
        cache_key,
        payload_sha256: payload_hash.to_uppercase(),
        source_display_name,
    })
}

/// The filterbank constants the binaural renderer needs.
#[derive(Clone, Copy)]
pub struct Kernels {
    pub qmf_analysis: [f64; 640],
    pub hybrid_low: [f64; 2496],
    pub hybrid_indices: &'static [i16; 616],
    pub hybrid_values: [f64; 154],
    pub qmf_basis: &'static [f64; 32768],
    pub qmf_taps: &'static [f64; 2560],
    pub hybrid_count: u32,
}

/// The public filterbank tables compiled into the reference library.
pub fn builtin_kernels() -> Kernels {
    use crate::tables_kernels as tables;
    let mut kernels = Kernels {
        qmf_analysis: [0.0; 640],
        hybrid_low: [0.0; 2496],
        hybrid_indices: &tables::HYBRID_INDICES,
        hybrid_values: [0.0; 154],
        qmf_basis: &tables::QMF_BASIS,
        qmf_taps: &tables::QMF_TAPS,
        hybrid_count: 154,
    };
    for (index, bits) in tables::QMF_ANALYSIS_BITS.iter().enumerate() {
        kernels.qmf_analysis[index] = f64::from(f32::from_bits(*bits));
    }
    for (index, bits) in tables::HYBRID_LOW_BITS.iter().enumerate() {
        kernels.hybrid_low[index] = f64::from(f32::from_bits(*bits));
    }
    for (index, bits) in tables::HYBRID_VALUES_BITS.iter().enumerate() {
        kernels.hybrid_values[index] = f64::from(f32::from_bits(*bits));
    }
    kernels
}
