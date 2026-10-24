#!/usr/bin/env python3
"""Native counterpart of tests/offline_joc.mjs.

Renders the committed fixture with the native CLI (speaker and binaural) and
compares every sample against the reference files. The core PCM is generated here
from the same recipe the JavaScript test uses, so both paths share one fixture.

    python tests/native_fixture.py --cli joc-wasm/target/release/joc-cli
"""
from __future__ import annotations

import argparse
import json
import math
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "fixtures"
FRAME = 1536


def read_f32(path: Path) -> list[float]:
    data = path.read_bytes()
    return list(struct.unpack(f"<{len(data) // 4}f", data))


def max_diff(actual: list[float], expected: list[float]) -> float:
    if len(actual) != len(expected):
        raise SystemExit(f"length mismatch: {len(actual)} != {len(expected)}")
    return max((abs(a - b) for a, b in zip(actual, expected)), default=0.0)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--hrtf", type=Path, default=ROOT / "hrtf" / "D2.jochrtf")
    args = parser.parse_args()

    spec = json.loads((FIXTURES / "joc_fixture.json").read_text())
    frames = spec["frames"]
    channels = spec["bed"]["channels"]
    tones = spec["bed"]["tones_hz"]
    amplitude = spec["bed"]["amplitude"]
    samples = frames * FRAME
    bed = bytearray()
    for sample in range(samples):
        t = sample / 48000.0
        for channel in range(channels):
            value = amplitude * math.sin(2.0 * math.pi * tones[channel] * t + channel * 0.3)
            bed += struct.pack("<f", value)

    with tempfile.TemporaryDirectory() as work:
        work = Path(work)
        bed_path = work / "bed.f32le"
        bed_path.write_bytes(bed)
        speaker_path = work / "speaker.f32"
        binaural_path = work / "binaural.f32"
        for mode, output, extra in (
            ("speaker", speaker_path, []),
            ("binaural", binaural_path, ["--hrtf", str(args.hrtf)]),
        ):
            command = [str(args.cli), "--input", str(FIXTURES / "joc_fixture.eac3"),
                       "--bed", str(bed_path), "--mode", mode, "--frames", str(frames),
                       "--out", str(output), *extra]
            result = subprocess.run(command, capture_output=True, text=True)
            if result.returncode != 0:
                print(result.stderr[-800:], file=sys.stderr)
                return 1
        expected_speaker = read_f32(FIXTURES / "test_speaker.f32")
        expected_binaural = read_f32(FIXTURES / "test_binaural.f32")
        speaker = read_f32(speaker_path)
        binaural = read_f32(binaural_path)[: len(expected_binaural)]

    diff_speaker = max_diff(speaker, expected_speaker)
    diff_binaural = max_diff(binaural, expected_binaural)
    print(json.dumps({"speaker_max_abs_diff": diff_speaker,
                      "binaural_max_abs_diff": diff_binaural}, indent=2))
    if diff_speaker > 1e-6 or diff_binaural > 1e-6:
        print("FAIL: native fixture differs from the reference outputs", file=sys.stderr)
        return 1
    print("PASS: native fixture matches the reference outputs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
