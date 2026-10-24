//! E-AC-3 JOC (Dolby Atmos) object reconstruction and stereo rendering.
//!
//! The crate is a direct port of the reference C++ core (JustOneCacophony): the
//! same bitstream syntax, the same DSP graph and the same float64 arithmetic, so
//! the rendered samples can be compared against the reference at 24-bit
//! precision.  It runs unchanged on the host (for the comparison harness) and in
//! the browser (`wasm32-unknown-unknown`).
//!
//! Pipeline:
//!
//! ```text
//! E-AC-3 syncframes ──> EMDF ──> ID14 (JOC matrix)  ─┐
//!                                   ID11 (OAMD)      ├─> object reconstruction
//! 5.1/7.1 core PCM ──────────────────────────────────┘        (QMF, 16 ch)
//!                                                              │
//!                                     ┌────────────────────────┴──────────┐
//!                              speaker 2.0 (equal-power pan)      binaural (.jochrtf)
//! ```

pub mod binaural;
pub mod bits;
pub mod eac3;
pub mod emdf;
mod huffman;
pub mod id14;
pub mod jochrtf;
pub mod json;
pub mod npy;
pub mod oamd;
pub mod objects16;
pub mod qmf;
pub mod sofa_binaural;
pub mod speaker;
pub mod stream;
pub mod tables_huffman;
pub mod tables_kernels;
pub mod tables_qmf;
pub mod timeline;
pub mod zip;

pub const FRAME_SAMPLES: usize = 1536;
pub const TIMESLOTS: usize = 24;
pub const SUBBANDS: usize = 64;
pub const CORE_CHANNELS: usize = 5;
pub const MAX_CORE_CHANNELS: usize = 7;
pub const OUTPUT_CHANNELS: usize = 16;
pub const MAX_OBJECTS: usize = 15;
pub const MAX_DPOINTS: usize = 2;
pub const MAX_PARAMETER_BANDS: usize = 23;
pub const LFE_DELAY_SAMPLES: usize = 1217;
pub const SPEAKER_BLOCK_SAMPLES: usize = 32;
pub const BINAURAL_BLOCK_SAMPLES: usize = 512;
pub const BINAURAL_QMF_BANDS: usize = 64;
pub const BINAURAL_HYBRID_BANDS: usize = 77;
pub const QMF_HOP_SAMPLES: usize = 64;
pub const BINAURAL_LATENCY_SAMPLES: usize = 961;

/// Error classes, named after the reference's `joc_error` values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    InvalidArgument,
    InvalidConfig,
    InputFormat,
    Eac3Syncframe,
    EmdfTransport,
    EmdfSyntax,
    JocSyntax,
    JocUnsupportedVariant,
    OamdSyntax,
    OamdUnsupportedVariant,
    BitstreamTruncated,
    BitstreamPadding,
    HrtfNotFound,
    HrtfFormat,
    HrtfVersion,
    HrtfHash,
    LayoutUnsupported,
    Render,
    State,
    NotSupported,
}

impl ErrorKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::InvalidConfig => "invalid_config",
            Self::InputFormat => "input_format",
            Self::Eac3Syncframe => "eac3_syncframe",
            Self::EmdfTransport => "emdf_transport",
            Self::EmdfSyntax => "emdf_syntax",
            Self::JocSyntax => "joc_syntax",
            Self::JocUnsupportedVariant => "joc_unsupported_variant",
            Self::OamdSyntax => "oamd_syntax",
            Self::OamdUnsupportedVariant => "oamd_unsupported_variant",
            Self::BitstreamTruncated => "bitstream_truncated",
            Self::BitstreamPadding => "bitstream_padding",
            Self::HrtfNotFound => "hrtf_not_found",
            Self::HrtfFormat => "hrtf_format",
            Self::HrtfVersion => "hrtf_version",
            Self::HrtfHash => "hrtf_hash",
            Self::LayoutUnsupported => "layout_unsupported",
            Self::Render => "render_failed",
            Self::State => "state",
            Self::NotSupported => "not_supported",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub stage: &'static str,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, stage: &'static str, message: impl Into<String>) -> Self {
        Self { kind, stage, message: message.into() }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ({}): {}", self.kind.name(), self.stage, self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;

pub(crate) fn fail<T>(kind: ErrorKind, stage: &'static str, message: impl Into<String>) -> Result<T> {
    Err(Error::new(kind, stage, message))
}

/// Python's floored modulo (`%`), which the JOC matrix differential decoder uses.
pub(crate) fn floored_mod(value: i64, modulus: i64) -> i64 {
    let remainder = value % modulus;
    if remainder < 0 {
        remainder + modulus
    } else {
        remainder
    }
}
