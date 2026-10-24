#!/usr/bin/env python3
"""Seek consistency check for the JOC renderer.

Renders a continuous run and a seek-restarted run (start 64 syncframes before the
target, drop the samples before it, exactly like the browser engine and the
foobar2000 component) and compares the two at the same absolute sample position.

Reports the error in the first frame after the restart (filter warm-up) and after
0.5 s and 2 s, in 24-bit LSBs.  A seek cannot be bit-exact: the renderer state is
rebuilt from the pre-roll, which is the point of the measurement.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

import numpy as np

FRAME = 1536
PREROLL = 64
LSB = 2.0 ** -23


def run_harness(node, harness, args):
    command = [node, str(harness), *args]
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit(result.stderr or result.stdout)


def stats(test, reference, start, count):
    a = test[start : start + count]
    b = reference[start : start + count]
    diff = np.abs(a.astype(np.float64) - b.astype(np.float64)) / LSB
    return {
        "frames": int(count),
        "max_lsb": float(np.max(diff)) if count else 0.0,
        "rms_lsb": float(np.sqrt(np.mean(diff * diff))) if count else 0.0,
        "within_1lsb_pct": float(100.0 * np.mean(diff <= 1.0)) if count else 100.0,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--harness", required=True, type=Path)
    parser.add_argument("--node", default="node")
    parser.add_argument("--ec3-runtime", required=True)
    parser.add_argument("--ec3-wasm", required=True)
    parser.add_argument("--joc-wasm", required=True)
    parser.add_argument("--input", required=True)
    parser.add_argument("--mode", choices=["speaker", "binaural"], default="speaker")
    parser.add_argument("--hrtf")
    parser.add_argument("--frame", type=int, default=400, help="seek target frame")
    parser.add_argument("--length", type=int, default=300, help="frames rendered after the target")
    parser.add_argument("--out-dir", type=Path, default=Path("."))
    args = parser.parse_args()

    target = args.frame
    start = max(0, target - PREROLL)
    skip = (target - start) * FRAME
    total = target + args.length
    common = [
        "--ec3-runtime", args.ec3_runtime,
        "--ec3-wasm", args.ec3_wasm,
        "--joc-wasm", args.joc_wasm,
        "--input", args.input,
        "--mode", args.mode,
    ]
    if args.hrtf:
        common += ["--hrtf", args.hrtf]

    continuous = args.out_dir / f"seek-continuous-{args.mode}.f32"
    seeking = args.out_dir / f"seek-restart-{args.mode}.f32"
    run_harness(args.node, args.harness, common + ["--frames", str(total), "--out", str(continuous)])
    run_harness(
        args.node,
        args.harness,
        common
        + [
            "--skip-frames", str(start),
            "--drop-frames", str(target - start),
            "--frames", str(total),
            "--out", str(seeking),
        ],
    )

    a = np.fromfile(continuous, dtype=np.float32).reshape(-1, 2)
    b = np.fromfile(seeking, dtype=np.float32).reshape(-1, 2)
    if len(b) < args.length:
        raise SystemExit(f"seek run produced {len(b)} frames, expected {args.length}")
    # The restarted run's sample 0 is absolute frame `target`.
    b = b[-(args.length * FRAME) :]
    a = a[target * FRAME : (target + args.length) * FRAME]
    if len(a) != len(b):
        raise SystemExit(f"length mismatch: continuous {len(a)}, restart {len(b)}")

    report = {
        "mode": args.mode,
        "seek_frame": target,
        "seek_seconds": target * FRAME / 48000.0,
        "preroll_frames": PREROLL,
        "first_frame": stats(b, a, 0, FRAME),
        "first_half_second": stats(b, a, 0, 24000),
        "first_two_seconds": stats(b, a, 0, 96000),
        "whole_run": stats(b, a, 0, len(a)),
    }
    import json

    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
