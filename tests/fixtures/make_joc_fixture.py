#!/usr/bin/env python3
"""Builds the offline JOC fixture from a real E-AC-3 stream (dev-time tool).

The committed fixture keeps the real EMDF containers (ID14/OAMD) of the first
frames but replaces the audio payload with zeros; the tests supply the core PCM
themselves, so no recorded audio is redistributed. The JOC parsers, object
reconstruction and rendering therefore run on real metadata.

    python tests/fixtures/make_joc_fixture.py \
        --input /path/to/gold_forever.eac3 --frames 24 --out tests/fixtures
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def frame_size(data: bytes, offset: int) -> int:
    return ((((data[offset + 2] & 0x07) << 8) | data[offset + 3]) + 1) * 2


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--frames", type=int, default=24)
    parser.add_argument("--out", type=Path, default=Path("tests/fixtures"))
    args = parser.parse_args()

    data = args.input.read_bytes()
    args.out.mkdir(parents=True, exist_ok=True)
    frames = []
    offset = 0
    with_joc = 0
    while len(frames) < args.frames and offset + 8 <= len(data):
        size = frame_size(data, offset)
        if size <= 8 or offset + size > len(data):
            break
        frame = bytearray(data[offset:offset + size])
        marker = frame.find(b"\x58\x38", 16)
        if marker >= 0:
            length = int.from_bytes(frame[marker + 2:marker + 4], "big")
            end = marker + 4 + length
            keep = bytearray(size)
            keep[:16] = frame[:16]
            keep[marker:end] = frame[marker:end]
            frame = keep
            with_joc += 1
        else:
            frame = bytearray(size)  # no metadata: silent filler frame
            frame[:16] = data[offset:offset + 16]
        frames.append(bytes(frame))
        offset += size

    total = sum(len(frame) for frame in frames)
    (args.out / "joc_fixture.eac3").write_bytes(b"".join(frames))
    (args.out / "joc_fixture.json").write_text(json.dumps({
        "frames": len(frames),
        "frames_with_joc": with_joc,
        "frame_bytes": total,
        "bed": {
            "channels": 6,
            "tones_hz": [440, 554, 659, 880, 110, 55],
            "amplitude": 0.08,
        },
    }, indent=2) + "\n", encoding="utf-8")
    print(f"{len(frames)} frames ({with_joc} with JOC), {total} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
