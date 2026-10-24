#!/usr/bin/env python3
"""Isolates the JOC renderer's seek-state convergence.

Both runs read the *same* continuous core PCM (so the EC-3 decoder is out of the
picture): one renders from frame 0, the other restarts at target-64 and drops the
pre-roll samples, exactly like the browser engine. The difference is therefore
the renderer state (QMF history, matrix interpolation, positions, LFE delay, room
tail) rebuilding itself.

Run with the native CLI (release build) for speed.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys

import numpy as np

FRAME = 1536
LSB = 2.0 ** -23


def run(cli, args):
    result = subprocess.run([cli, *args], capture_output=True, text=True)
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
    parser.add_argument("--cli", required=True)
    parser.add_argument("--input", required=True)
    parser.add_argument("--bed", required=True)
    parser.add_argument("--mode", choices=["speaker", "binaural"], default="speaker")
    parser.add_argument("--hrtf")
    parser.add_argument("--frame", type=int, default=400)
    parser.add_argument("--length", type=int, default=300)
    parser.add_argument("--preroll", type=int, default=64)
    parser.add_argument("--out-dir", default=".")
    args = parser.parse_args()

    target = args.frame
    start = max(0, target - args.preroll)
    total = target + args.length
    common = ["--input", args.input, "--bed", args.bed, "--mode", args.mode, "--gain-db", "0"]
    if args.hrtf:
        common += ["--hrtf", args.hrtf]
    continuous = f"{args.out_dir}/jocseek-continuous-{args.mode}.f32"
    restarted = f"{args.out_dir}/jocseek-restart-{args.mode}.f32"
    run(args.cli, common + ["--frames", str(total), "--out", continuous])
    run(
        args.cli,
        common
        + [
            "--skip-frames", str(start),
            "--drop-frames", str(target - start),
            "--frames", str(total),
            "--out", restarted,
        ],
    )

    a = np.fromfile(continuous, dtype=np.float32).reshape(-1, 2)
    b = np.fromfile(restarted, dtype=np.float32).reshape(-1, 2)
    # The binaural kernel holds 961 samples back at the start of a stream; the room
    # tail follows the programme and is not part of this comparison.
    programme = args.length * FRAME - (961 if args.mode == "binaural" else 0)
    if len(b) < programme:
        raise SystemExit(f"restarted run produced {len(b)} frames, expected {programme}")
    b = b[:programme]
    a = a[target * FRAME : target * FRAME + programme]
    if len(a) != len(b):
        raise SystemExit(f"length mismatch: continuous {len(a)}, restart {len(b)}")

    report = {
        "mode": args.mode,
        "seek_frame": target,
        "seek_seconds": target * FRAME / 48000.0,
        "preroll_frames": target - start,
        "first_frame": stats(b, a, 0, FRAME),
        "first_half_second": stats(b, a, 0, 24000),
        "first_two_seconds": stats(b, a, 0, 96000),
        "whole_run": stats(b, a, 0, len(a)),
    }
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
