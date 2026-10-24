#!/usr/bin/env python3
"""Compare two rendered signals at 24-bit precision.

The reference is a WAV written by `joc_cli` (int24 or float32); the candidate is a
raw little-endian float32 stream written by the Rust harness.  Both are quantised
with the reference's own int24 rule: clamp to [-1, 1], multiply by 8388607 in
float32, truncate toward zero.  The report is in LSBs, so one unit is the 24-bit
step the release acceptance talks about.

Usage:
    python tests/compare.py --reference out.wav --candidate out.f32 \
        --reference-channels 2 --candidate-channels 2 --frames 480768 \
        [--json report.json] [--window 48000]
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
from pathlib import Path

import numpy as np

LSB = 2.0 ** -23


def load_wav(path: Path) -> tuple[np.ndarray, int, int, int]:
    data = path.read_bytes()
    if len(data) < 12 or data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise SystemExit(f"{path}: not a RIFF/WAVE file")
    offset = 12
    channels = rate = bits = 0
    fmt = 0
    payload = b""
    while offset + 8 <= len(data):
        chunk_id = data[offset : offset + 4]
        size = struct.unpack_from("<I", data, offset + 4)[0]
        body = data[offset + 8 : offset + 8 + size]
        if chunk_id == b"fmt ":
            fmt, channels, rate, _byte_rate, _align, bits = struct.unpack_from("<HHIIHH", body, 0)
        elif chunk_id == b"data":
            payload = body
        offset += 8 + size + (size & 1)
    if not channels or not payload:
        raise SystemExit(f"{path}: missing fmt/data chunk")
    if bits == 24:
        raw = np.frombuffer(payload, dtype=np.uint8).reshape(-1, 3).astype(np.int32)
        values = raw[:, 0] | (raw[:, 1] << 8) | (raw[:, 2] << 16)
        values = np.where(values & 0x800000, values - 0x1000000, values)
        return values.astype(np.int32), channels, rate, 24
    if bits == 32 and fmt in (3, 0xFFFE):
        return np.frombuffer(payload, dtype="<f4").copy(), channels, rate, 32
    if bits == 32:
        return np.frombuffer(payload, dtype="<i4").copy(), channels, rate, 32
    if bits == 16:
        return np.frombuffer(payload, dtype="<i2").copy(), channels, rate, 16
    raise SystemExit(f"{path}: unsupported sample format ({fmt}, {bits} bit)")


def load_f32(path: Path) -> np.ndarray:
    values = np.fromfile(path, dtype="<f4")
    return values


def to_int24(values: np.ndarray) -> np.ndarray:
    """The reference wav_writer rule, applied in float32."""
    clipped = np.clip(values.astype(np.float32), np.float32(-1.0), np.float32(1.0))
    scaled = np.float32(8388607.0) * clipped
    return scaled.astype(np.int32)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--reference-channels", type=int, default=2)
    parser.add_argument("--candidate-channels", type=int, default=2)
    parser.add_argument("--frames", type=int, default=0)
    parser.add_argument("--skip-reference-frames", type=int, default=0)
    parser.add_argument("--skip-candidate-frames", type=int, default=0)
    parser.add_argument("--window", type=int, default=48000 * 10)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()

    reference, ref_channels, ref_rate, ref_bits = load_wav(args.reference)
    if args.reference_channels != ref_channels:
        raise SystemExit(f"reference has {ref_channels} channels, expected {args.reference_channels}")
    candidate = load_f32(args.candidate)

    ref = reference.reshape(-1, ref_channels)
    cand = candidate.reshape(-1, args.candidate_channels)
    if args.skip_reference_frames:
        ref = ref[args.skip_reference_frames :]
    if args.skip_candidate_frames:
        cand = cand[args.skip_candidate_frames :]
    frames = args.frames or min(len(ref), len(cand))
    frames = min(frames, len(ref), len(cand))
    ref = ref[:frames]
    cand = cand[:frames]

    if ref_bits == 24:
        ref_i = ref.astype(np.int32)
    elif ref_bits == 32 and reference.dtype == np.float32:
        ref_i = to_int24(ref)
    else:
        ref_i = (ref.astype(np.float64) / 2 ** (ref_bits - 1) * 8388607.0).astype(np.int32)
    cand_i = to_int24(cand)

    diff = ref_i.astype(np.int64) - cand_i.astype(np.int64)
    finite = np.isfinite(cand).all(axis=1)
    stats = {
        "reference": str(args.reference),
        "candidate": str(args.candidate),
        "frames": int(frames),
        "channels": args.candidate_channels,
        "max_abs_lsb": int(np.max(np.abs(diff))) if frames else 0,
        "rms_lsb": float(np.sqrt(np.mean(diff.astype(np.float64) ** 2))) if frames else 0.0,
        "mean_lsb": float(np.mean(diff)) if frames else 0.0,
        "count_gt_1lsb": int(np.sum(np.abs(diff) > 1)),
        "count_gt_0": int(np.count_nonzero(diff)),
        "non_finite_candidate": int(np.count_nonzero(~finite)),
        "reference_peak": float(np.max(np.abs(ref))),
        "candidate_peak": float(np.max(np.abs(cand))),
    }

    # Worst window: long-run stability is as important as the aggregate.
    window = args.window
    if frames > window:
        worst = 0
        worst_at = 0
        for start in range(0, frames - window + 1, window):
            local = int(np.max(np.abs(diff[start : start + window])))
            if local > worst:
                worst = local
                worst_at = start
        stats["worst_window_max_lsb"] = worst
        stats["worst_window_start_frame"] = worst_at
        stats["worst_window_seconds"] = worst_at / 48000.0

    print(json.dumps(stats, indent=2))
    if args.json:
        args.json.write_text(json.dumps(stats, indent=2) + "\n", encoding="utf-8")
    if stats["max_abs_lsb"] > 1:
        print(f"FAIL: max |diff| = {stats['max_abs_lsb']} LSB (> 1)", file=sys.stderr)
        return 1
    print("PASS: every sample is within 1 LSB of the 24-bit reference", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
