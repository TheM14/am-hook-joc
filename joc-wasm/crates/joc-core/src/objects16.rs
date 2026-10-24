//! Glue between the parsed frame parameters and the object-reconstruction kernel.

use crate::id14::FrameParams;
use crate::qmf::{ObjectDescriptor, Renderer};
use crate::{
    fail, ErrorKind, Result, FRAME_SAMPLES, MAX_CORE_CHANNELS, MAX_DPOINTS, MAX_OBJECTS,
    MAX_PARAMETER_BANDS, OUTPUT_CHANNELS,
};

/// Flattens `FrameParams` into the kernel's descriptor layout.
pub fn descriptor_from_params(params: &FrameParams) -> ObjectDescriptor {
    let mut descriptor = ObjectDescriptor::new();
    for object in 0..MAX_OBJECTS {
        let info = &params.objects[object];
        if !info.present {
            continue;
        }
        descriptor.n_bands = info.n_bands;
        descriptor.n_dpoints = info.n_dpoints;
        descriptor.slope_idx = info.slope_idx;
        descriptor.offset_ts = info.offset_ts;
        for point in 0..MAX_DPOINTS {
            for channel in 0..params.n_channels as usize {
                for band in 0..info.n_bands as usize {
                    let index = (((object * MAX_DPOINTS + point) * MAX_CORE_CHANNELS + channel)
                        * MAX_PARAMETER_BANDS)
                        + band;
                    descriptor.dq[index] = info.dq[point][channel][band];
                }
            }
        }
    }
    descriptor
}

/// `bed_planar` is `[n_channels][1536]`; `lfe` is `[1536]` or empty.
pub fn rebuild(
    renderer: &mut Renderer,
    params: &FrameParams,
    bed_planar: &[f32],
    lfe: &[f32],
    gain: f32,
    out16: &mut [f32],
) -> Result<()> {
    if params.n_channels != 5 && params.n_channels != 7 {
        return fail(
            ErrorKind::JocUnsupportedVariant,
            "dsp",
            format!(
                "the JOC kernel requires 5 or 7 core channels, frame declares {}",
                params.n_channels
            ),
        );
    }
    let descriptor = descriptor_from_params(params);
    // sigma(k) = (k in {3,4}) gates the Ls/Rs -j rotation and the band-0 21-tap
    // DC compensation together.
    let phase_shift = params.dmx_config_idx == 3 || params.dmx_config_idx == 4;
    out16[..OUTPUT_CHANNELS * FRAME_SAMPLES].fill(0.0);
    renderer.process(
        bed_planar,
        if lfe.is_empty() { None } else { Some(lfe) },
        params.n_channels as usize,
        phase_shift,
        params.present_mask,
        &descriptor,
        params.clipgain,
        0.0625,
        gain,
        out16,
    )
}
