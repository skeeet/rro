#!/usr/bin/env python3
"""Development-only HEIC interoperability checks against FFmpeg's software decoder.

Requires NumPy, Pillow, and FFmpeg only when running this validation script. The
normal rro executable has no FFmpeg or Python runtime dependency. All fixtures
are synthetic project test data; no original RRO implementation is needed.

Run after building the release binary:
  python3 scripts/independent_codec_check.py
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
from report_privacy import write_report
import shutil
import subprocess
import sys
import time

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
MAX_CODE_DIFFERENCE = 2


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(command, timeout=30):
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}): {result.stderr.strip()[-2000:]}")
    return result


def convert(binary, source, destination, *flags):
    result = run([str(binary), str(source), str(destination), *flags, "--force"])
    if result.stdout:
        raise RuntimeError("successful image conversion wrote unsolicited stdout")


def ffmpeg_decode(ffmpeg, source, destination):
    # No hardware acceleration requested. One software decoder worker keeps these
    # tiny fixture checks independent of the later CPU performance measurements.
    run([ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "1",
         "-i", str(source), "-map", "0:v:0", "-frames:v", "1", "-pix_fmt", "rgb24",
         "-y", str(destination)])


def metrics(actual_path, expected_path):
    with Image.open(actual_path) as image:
        actual = np.asarray(image.convert("RGB"), dtype=np.float64)
    with Image.open(expected_path) as image:
        expected = np.asarray(image.convert("RGB"), dtype=np.float64)
    if actual.shape != expected.shape:
        return {"actual_shape": list(actual.shape), "expected_shape": list(expected.shape),
                "pass": False, "error": "independent decoder dimensions differ"}
    difference = actual - expected
    maximum = float(np.max(np.abs(difference)))
    mse = float(np.mean(difference * difference))
    return {"shape": list(actual.shape), "max_code_difference": maximum,
            "rmse_codes": math.sqrt(mse),
            "psnr_db": 10 * math.log10(255 * 255 / mse) if mse else None,
            "fraction_differing": float(np.mean(difference != 0)),
            "allowed_max_code_difference": MAX_CODE_DIFFERENCE,
            "pass": maximum <= MAX_CODE_DIFFERENCE}


def input_check(binary, ffmpeg, work, source):
    stem = source.stem
    own = work / f"{stem}-rro.png"
    independent = work / f"{stem}-ffmpeg.png"
    convert(binary, source, own, "--scale", "1")
    ffmpeg_decode(ffmpeg, source, independent)
    return {"fixture": str(source.relative_to(ROOT)), "fixture_sha256": digest(source),
            **metrics(own, independent)}


def output_check(binary, ffmpeg, work):
    # Even dimensions isolate codec conformance from FFmpeg versions that do not
    # apply a HEIF clean-aperture property for odd subsampled source dimensions.
    y, x = np.mgrid[:96, :128]
    pixels = np.stack((24 + x, 30 + 2 * y, 40 + (x + y) // 2), axis=-1).astype(np.uint8)
    source = work / "synthetic-source.png"
    encoded = work / "synthetic-output.heic"
    own = work / "synthetic-output-rro.png"
    independent = work / "synthetic-output-ffmpeg.png"
    Image.fromarray(pixels).save(source)
    convert(binary, source, encoded, "--scale", "1", "--quality", "0.9")
    convert(binary, encoded, own, "--scale", "1")
    ffmpeg_decode(ffmpeg, encoded, independent)
    return {"source_pattern": "128x96 RGB: R=24+x, G=30+2*y, B=40+floor((x+y)/2)",
            "source_png_sha256": digest(source), "heic_sha256": digest(encoded),
            "quality": 0.9, **metrics(own, independent)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/rro")
    parser.add_argument("--ffmpeg", default="ffmpeg")
    args = parser.parse_args()
    binary = args.binary.resolve()
    ffmpeg = shutil.which(args.ffmpeg)
    if not ffmpeg:
        parser.error("FFmpeg is required for this development-only interoperability check")
    if not binary.is_file():
        parser.error("release executable missing; run cargo build --release --locked --bin rro")
    work = ROOT / "reports/work/independent-codec"
    work.mkdir(parents=True, exist_ok=True)
    original_hash = digest(binary)
    report = {
        "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "binary_sha256": original_hash,
        "backend": "oxideav-heif 0.0.13 + oxideav-h265 0.0.15",
        "ffmpeg_version": run([ffmpeg, "-version"]).stdout.splitlines()[0],
        "protocol": "live FFmpeg software HEVC decode, RGB8 comparison, fixed maximum two-code tolerance",
        "notes": ["FFmpeg, NumPy, and Pillow are development tools only, never rro runtime dependencies.",
                  "Input files are independently encoded synthetic project fixtures.",
                  "Opaque identity-scale conversions compare decoded code values before any lossy re-encoding.",
                  "Output codec comparison uses even dimensions; odd HEIF clean-aperture handling is covered separately by Rust tests."],
    }
    checks = [
        ("limited_range_HEIC_input_vs_ffmpeg", lambda: input_check(binary, ffmpeg, work, ROOT / "tests/fixtures/x265-limited-no-nclx.heic")),
        ("kvazaar_HEIC_input_vs_ffmpeg", lambda: input_check(binary, ffmpeg, work, ROOT / "tests/fixtures/kvazaar-smooth.heic")),
        ("our_HEIC_output_decode_vs_ffmpeg", lambda: output_check(binary, ffmpeg, work)),
    ]
    for name, operation in checks:
        try:
            report[name] = operation()
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            report[name] = {"pass": False, "error": str(error)}
    report["binary_unchanged_during_checks"] = digest(binary) == original_hash
    report["count"] = len(checks)
    report["passed"] = sum(report[name]["pass"] for name, _ in checks)
    destination = ROOT / "reports/independent-codec-check.json"
    write_report(destination, report)
    print(f"Independent HEIC checks: {report['passed']}/{report['count']} passed; binary unchanged: {report['binary_unchanged_during_checks']}")
    for name, _ in checks:
        print(name, json.dumps(report[name], sort_keys=True))
    return 0 if report["passed"] == report["count"] and report["binary_unchanged_during_checks"] else 1


if __name__ == "__main__":
    sys.exit(main())
