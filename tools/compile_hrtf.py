#!/usr/bin/env python3
"""Compile a SOFA HRIR set into a .jochrtf field with the reference compiler.

The conversion uses the Python reference implementation (``sofa_hrtf_field``)
directly, so it reads the SOFA file with h5py and does not depend on the C++
HDF5 reader or its superblock restrictions. Every attribute the SOFA file
carries (root attributes and per-dataset attributes such as ``Units``/``Type``)
is copied verbatim into ``sofa_metadata`` / ``sofa_dataset_metadata``.

    python tools/compile_hrtf.py field.sofa field.jochrtf \
        --reference /path/to/JustOneCacophony/src

Options: ``--radius`` (measurement shell, default 1.0 m), ``--order`` (fixed at 5),
``--projection-ridge`` and ``--sh-ridge`` (compiler defaults), ``--minimum-phase``.

A measurement whose energy sits well before its peak is approximately linear
phase (diffuse-field equalised sets are typical). The reference compiler removes
the peak delay by advancing the FIR, which truncates that pre-peak energy and
corrupts the fit. ``--minimum-phase`` rewrites such IRs to minimum phase and
carries the peak delay in ``Data.Delay`` instead: magnitude and ITD are kept, the
FIR stays causal, and the fit targets the whole response.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import sys
import tempfile
import zipfile
from pathlib import Path

import h5py
import numpy as np


def _attribute_value(value):
    """JSON-safe copy of one HDF5 attribute, without altering its text."""
    if isinstance(value, h5py.Reference):
        return None  # HDF5 object reference, not metadata
    if isinstance(value, bytes):
        try:
            return value.decode("utf-8")
        except UnicodeDecodeError:
            return value.decode("latin-1")
    if isinstance(value, np.ndarray):
        items = [_attribute_value(item) for item in value.tolist()]
        return None if any(item is None for item in items) else items
    if isinstance(value, np.generic):
        return value.item()
    return value


def _dataset_metadata(handle: h5py.File) -> dict:
    """Attributes of every dataset that describes the measurement."""
    result = {}
    for name in ("Data.IR", "Data.Delay", "Data.SamplingRate", "SourcePosition",
                 "ReceiverPosition", "ListenerPosition", "ListenerView",
                 "ListenerUp", "EmitterPosition", "N", "M", "C", "R", "E",
                 "I", "S"):
        if name not in handle:
            continue
        attributes = {}
        for key, value in handle[name].attrs.items():
            if key in ("DIMENSION_LIST", "REFERENCE_LIST"):
                continue  # HDF5 plumbing (object references), not metadata
            converted = _attribute_value(value)
            if converted is not None:
                attributes[key] = converted
        if attributes:
            result[name] = attributes
    return result


def _minimum_phase_copy(source: Path) -> Path:
    """Write a temporary copy with minimum-phase IRs and per-ear peak delays."""
    import tempfile

    handle, name = tempfile.mkstemp(prefix=".minimum-phase.", suffix=".sofa",
                                    dir=source.parent)
    os.close(handle)
    target = Path(name)
    with h5py.File(source, "r") as src, h5py.File(target, "w") as dst:
        for key, value in src.attrs.items():
            converted = _attribute_value(value)
            if converted is not None:
                if isinstance(value, bytes):
                    dst.attrs.create(key, value, dtype=f"S{len(value)}")
                else:
                    dst.attrs[key] = converted
        for key, item in src.items():
            if isinstance(item, h5py.Dataset):
                data = item[()]
                if key == "Data.IR":
                    data = _minimum_phase_ir(np.asarray(data, dtype=np.float64))
                dst.create_dataset(key, data=data, dtype=item.dtype)
                for name_, value in item.attrs.items():
                    if name_ in ("DIMENSION_LIST", "REFERENCE_LIST"):
                        continue
                    converted = _attribute_value(value)
                    if converted is None:
                        continue
                    if isinstance(value, bytes):
                        dst[key].attrs.create(name_, value, dtype=f"S{len(value)}")
                    else:
                        dst[key].attrs[name_] = converted
            else:
                dst.require_group(key)
        peaks = np.argmax(np.abs(src["Data.IR"][()]), axis=2).astype(np.float64)
        if "Data.Delay" in dst:
            del dst["Data.Delay"]
        delay = dst.create_dataset("Data.Delay", data=peaks, dtype="<f8")
        delay.attrs["Units"] = "samples"
    return target


def _minimum_phase_ir(irs: np.ndarray) -> np.ndarray:
    """Real-cepstrum minimum phase, per measurement and ear."""
    length = irs.shape[-1]
    size = 1 << (4 * length - 1).bit_length()
    log_magnitude = np.log(np.maximum(np.abs(np.fft.rfft(irs, size, axis=-1)), 1e-12))
    cepstrum = np.fft.irfft(log_magnitude, size, axis=-1)
    folded = np.zeros_like(cepstrum)
    folded[..., 0] = cepstrum[..., 0]
    folded[..., 1:size // 2] = 2.0 * cepstrum[..., 1:size // 2]
    folded[..., size // 2] = cepstrum[..., size // 2]
    minimum = np.fft.irfft(np.exp(np.fft.rfft(folded, axis=-1)), size, axis=-1)
    return np.ascontiguousarray(minimum[..., :length])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sofa", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--reference", type=Path, required=True,
                        help="the reference checkout's src/ directory")
    parser.add_argument("--radius", type=float, default=1.0)
    parser.add_argument("--order", type=int, default=5)
    parser.add_argument("--projection-ridge", type=float, default=1.0e-3)
    parser.add_argument("--sh-ridge", type=float, default=1.0e-5)
    parser.add_argument("--minimum-phase", action="store_true",
                        help="convert linear-phase measurements before compiling")
    args = parser.parse_args()

    sys.path.insert(0, str(args.reference.expanduser().resolve()))
    from sofa_hrtf_field import (  # noqa: PLC0415
        _json_bytes, compile_sofa_hrtf, compiled_hrtf_cache_key)

    source = args.sofa.expanduser().resolve()
    work = source
    convert = args.minimum_phase
    if convert:
        work = _minimum_phase_copy(source)
    with h5py.File(source, "r") as handle:
        sofa_metadata = {}
        for key, value in handle.attrs.items():
            converted = _attribute_value(value)
            if converted is not None:
                sofa_metadata[key] = converted
        dataset_metadata = _dataset_metadata(handle)

    try:
        field = compile_sofa_hrtf(
            work,
            shell_radius_m=args.radius,
            order=args.order,
            projection_ridge=args.projection_ridge,
            sh_ridge=args.sh_ridge,
        )
    finally:
        if work != source:
            work.unlink(missing_ok=True)

    # Keep the identity of the file the user supplied, not the temporary copy.
    source_sha256 = hashlib.sha256(source.read_bytes()).hexdigest().upper()

    # The field metadata keeps the reference schema unchanged; the SOFA's own
    # metadata travels in the ZIP comment so every reader still accepts the file.
    metadata = field._metadata()
    metadata["source_sha256"] = source_sha256
    metadata["source_display_name"] = source.name
    metadata["cache_key"] = compiled_hrtf_cache_key(
        source_sha256=source_sha256,
        sample_rate_hz=field.sample_rate_hz,
        shell_radius_m=field.measurement_radius_m,
        order=field.order,
        projection_ridge=field.projection_ridge,
        sh_ridge=field.spherical_harmonic_ridge,
    )
    metadata_text = _json_bytes(metadata).decode("utf-8")
    sofa_note = json.dumps(
        {"sofa_metadata": sofa_metadata,
         "sofa_dataset_metadata": dataset_metadata},
        ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    if len(sofa_note) > 0xFFFF:
        raise SystemExit("SOFA metadata does not fit into the ZIP comment field")

    buffer = io.BytesIO()
    np.savez_compressed(
        buffer,
        metadata_json=np.asarray(metadata_text),
        band_center_frequencies_hz=np.asarray(
            field.band_center_frequencies_hz, dtype="<f8"),
        coefficients=np.asarray(field.coefficients, dtype="<c16"),
        delay_coefficients=np.asarray(field.delay_coefficients, dtype="<f8"),
        delay_bounds=np.asarray(field.delay_bounds, dtype="<f8"),
    )
    with zipfile.ZipFile(io.BytesIO(buffer.getvalue())) as archive:
        members = [(info.filename, archive.read(info.filename))
                   for info in archive.infolist()]
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, payload in members:
            archive.writestr(name, payload)
        archive.comment = sofa_note

    target = args.output.expanduser().resolve()
    target.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{target.name}.", suffix=".tmp", dir=target.parent)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(buffer.getvalue())
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary_name, target)
    finally:
        try:
            os.unlink(temporary_name)
        except FileNotFoundError:
            pass

    print(target)
    print(f"source: {metadata['source_display_name']} ({source_sha256[:16]}...)")
    print(f"radius: {field.measurement_radius_m} m, order {field.order}, "
          f"{field.coefficients.shape[0]} terms, {field.coefficients.shape[2]} bands")
    print(f"sofa metadata: {len(sofa_metadata)} root attributes, "
          f"{len(dataset_metadata)} datasets (ZIP comment, {len(sofa_note)} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
