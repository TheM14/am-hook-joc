//! Native harness: renders an E-AC-3 JOC stream plus its predecoded core PCM and
//! writes float32 output, so the result can be diffed against the C++ reference.
//!
//! It is deliberately the same code path as the browser build (`joc_core::stream`),
//! only driven from a file instead of a worker.

use std::io::Write;
use std::process::ExitCode;

use joc_core::stream::{BinauralProfile, Mode, Stream, StreamConfig, StreamInfo};
use joc_core::FRAME_SAMPLES;

struct Options {
    input: String,
    bed: String,
    bed_channels: usize,
    mode: Mode,
    hrtf: Option<String>,
    gain_db: f64,
    frames: Option<usize>,
    skip_frames: usize,
    drop_frames: usize,
    output: String,
    objects16: Option<String>,
    stats: bool,
}

fn usage() -> String {
    "\
usage: joc-cli --input <file.eac3> --bed <core.f32le> [options]

  --input PATH         bare E-AC-3 stream
  --bed PATH           interleaved float32 little-endian core PCM (6 or 8 channels)
  --bed-channels N     channels in --bed (default 6)
  --mode MODE          speaker (2.0) | binaural | objects16 (default speaker)
  --hrtf PATH          .jochrtf for binaural
  --gain-db X          output gain in dB (default 0)
  --frames N           stop after N input frames
  --skip-frames N      start at input frame N (state starts cold, like a seek)
  --drop-frames N      discard the first N output frames (seek pre-roll)
  --out PATH           float32 output: interleaved stereo, or planar 16ch for objects16
  --objects16 PATH     additionally write the reconstructed objects (planar 16ch)
  --stats              print stream statistics
"
    .to_string()
}

fn parse() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut input = None;
    let mut bed = None;
    let mut bed_channels = 6usize;
    let mut mode = Mode::Speaker20;
    let mut hrtf = None;
    let mut gain_db = 0.0f64;
    let mut frames = None;
    let mut skip_frames = 0usize;
    let mut drop_frames = 0usize;
    let mut output = "out.f32".to_string();
    let mut objects16 = None;
    let mut stats = false;
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--input" => input = Some(value()?),
            "--bed" => bed = Some(value()?),
            "--bed-channels" => bed_channels = value()?.parse().map_err(|_| "bad channel count")?,
            "--mode" => {
                mode = match value()?.as_str() {
                    "speaker" => Mode::Speaker20,
                    "binaural" => Mode::Binaural,
                    "objects16" => Mode::Objects16,
                    other => return Err(format!("unknown mode {other}")),
                }
            }
            "--hrtf" => hrtf = Some(value()?),
            "--gain-db" => gain_db = value()?.parse().map_err(|_| "bad gain")?,
            "--frames" => frames = Some(value()?.parse().map_err(|_| "bad frame count")?),
            "--skip-frames" => skip_frames = value()?.parse().map_err(|_| "bad skip")?,
            "--drop-frames" => drop_frames = value()?.parse().map_err(|_| "bad drop")?,
            "--out" => output = value()?,
            "--objects16" => objects16 = Some(value()?),
            "--stats" => stats = true,
            "--help" | "-h" => return Err(usage()),
            other => return Err(format!("unknown option {other}\n{}", usage())),
        }
    }
    Ok(Options {
        input: input.ok_or("missing --input")?,
        bed: bed.ok_or("missing --bed")?,
        bed_channels,
        mode,
        hrtf,
        gain_db,
        frames,
        skip_frames,
        drop_frames,
        output,
        objects16,
        stats,
    })
}

/// Splits a bare E-AC-3 stream into syncframes using the library's own walker.
fn split_frames(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut reader = joc_core::eac3::FrameReader::new();
    reader.push(data);
    reader.finish();
    let mut frames = Vec::new();
    loop {
        match reader.next() {
            joc_core::eac3::Next::Frame(frame) => {
                // The walker owns its buffer, so copy the bytes out.
                frames.push(reader.frame_at(&frame).to_vec());
            }
            joc_core::eac3::Next::End => break,
            joc_core::eac3::Next::Fail(error) => return Err(error.to_string()),
        }
    }
    Ok(frames)
}

fn write_float_file(path: &str, data: &[f32]) -> Result<(), String> {
    let mut file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for value in data {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    file.write_all(&bytes).map_err(|e| format!("{path}: {e}"))
}

fn main() -> ExitCode {
    let options = match parse() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    // The renderer keeps its frame state in fixed arrays, so give the worker a
    // generous stack instead of relying on the platform default.
    let worker = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || run(&options))
        .expect("cannot start the render thread");
    match worker.join() {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(message)) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
        Err(_) => ExitCode::FAILURE,
    }
}

fn run(options: &Options) -> Result<(), String> {
    let eac3 = std::fs::read(&options.input).map_err(|e| format!("{}: {e}", options.input))?;
    let bed = std::fs::read(&options.bed).map_err(|e| format!("{}: {e}", options.bed))?;
    let bed: Vec<f32> = bed
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect();
    let frames = split_frames(&eac3)?;
    let hrtf = match &options.hrtf {
        Some(path) => Some(std::fs::read(path).map_err(|e| format!("{path}: {e}"))?),
        None => None,
    };
    let config = StreamConfig {
        mode: options.mode,
        gain_db: options.gain_db,
        object_delay_samples: 1473,
        tail_seconds: 5.0,
        binaural_profile: BinauralProfile::Mid,
        hrtf,
    };
    let mut stream = Stream::new(config).map_err(|e| e.to_string())?;
    let frame_count = options.frames.unwrap_or(frames.len()).min(frames.len());
    let start = options.skip_frames.min(frame_count);
    let bed_frame = FRAME_SAMPLES * options.bed_channels;

    let mut output = Vec::<f32>::new();
    let mut objects = Vec::<f32>::new();
    let mut scratch = vec![0.0f32; 16 * FRAME_SAMPLES * 2];
    for index in start..frame_count {
        stream.push_eac3(&frames[index]).map_err(|e| e.to_string())?;
        let bed_start = index * bed_frame;
        if bed_start + bed_frame <= bed.len() {
            stream
                .push_pcm(&bed[bed_start..bed_start + bed_frame], options.bed_channels)
                .map_err(|e| e.to_string())?;
        }
        if options.mode == Mode::Objects16 {
            loop {
                let produced = stream
                    .pull_objects16(&mut scratch)
                    .map_err(|e| e.to_string())?;
                if produced == 0 {
                    break;
                }
                objects.extend_from_slice(&scratch[..produced * 16]);
            }
        } else {
            loop {
                let produced = stream.pull(&mut scratch, 4096).map_err(|e| e.to_string())?;
                if produced == 0 {
                    break;
                }
                output.extend_from_slice(&scratch[..produced * 2]);
            }
        }
    }
    stream.flush().map_err(|e| e.to_string())?;
    loop {
        if options.mode == Mode::Objects16 {
            let produced = stream
                .pull_objects16(&mut scratch)
                .map_err(|e| e.to_string())?;
            if produced == 0 {
                break;
            }
            objects.extend_from_slice(&scratch[..produced * 16]);
        } else {
            let produced = stream.pull(&mut scratch, 4096).map_err(|e| e.to_string())?;
            if produced == 0 {
                break;
            }
            output.extend_from_slice(&scratch[..produced * 2]);
        }
    }

    if options.drop_frames > 0 && options.mode != Mode::Objects16 {
        let values = options.drop_frames * FRAME_SAMPLES * 2;
        let drop = values.min(output.len());
        output.drain(..drop);
    }
    match options.mode {
        Mode::Objects16 => write_float_file(&options.output, &objects)?,
        _ => write_float_file(&options.output, &output)?,
    }
    if let Some(path) = &options.objects16 {
        let info: StreamInfo = stream.info();
        let _ = info;
        write_float_file(path, &objects)?;
    }
    if options.stats {
        let info = stream.info();
        eprintln!(
            "frames_in={} frames_out={} samples_in={} samples_out={} missing_joc={} clamped={} peak={:.9} over_unity={} non_finite={}",
            info.frames_in,
            info.frames_out,
            info.samples_in,
            info.samples_out,
            info.missing_joc_frames,
            info.objects_clamped_frames,
            info.peak,
            info.over_unity,
            info.non_finite
        );
        eprintln!("output values: {}", if options.mode == Mode::Objects16 { objects.len() } else { output.len() });
    }
    Ok(())
}
