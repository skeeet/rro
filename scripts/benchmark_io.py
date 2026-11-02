#!/usr/bin/env python3
"""Full-process image conversion benchmark (development dependencies only).

The measured executable requires no Python. This runner uses Pillow and NumPy
only to prepare deterministic synthetic 12MP inputs. It measures wall time from
process launch through process exit, plus the CLI's separately reported phases.
Kernel construction remains inside the CLI resize phase. A fresh Python wrapper
is used for each separate peak-RSS run so RUSAGE_CHILDREN is not cumulative over
earlier measurements. RSS runs are not included in timing summaries.

Run only after native compilation and numeric benchmarks have finished:
  python3 scripts/benchmark_io.py --prepare-only
  python3 scripts/benchmark_io.py
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
from report_privacy import write_report as write_private_report
import platform
import re
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BINARY = ROOT / "target/release/rro"
DEFAULT_WORK = ROOT / "reports/work/io-benchmark"
DEFAULT_REPORT = ROOT / "reports/io-benchmark.json"
TIMINGS = re.compile(r"(\d+)x(\d+); decode ([0-9.]+)s, resize ([0-9.]+)s, encode ([0-9.]+)s, total ([0-9.]+)s")
SEED = 20261027
WIDTH, HEIGHT = 4032, 3024


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def description(path):
    return {"path": str(path), "bytes": path.stat().st_size, "sha256": digest(path)}


def summary(values):
    ordered = sorted(values)
    if not ordered:
        return None
    # Linear interpolation, equivalent to NumPy's default percentile method.
    rank = (len(ordered) - 1) * 0.95
    lower = int(rank)
    upper = min(lower + 1, len(ordered) - 1)
    p95 = ordered[lower] + (ordered[upper] - ordered[lower]) * (rank - lower)
    return {"median": statistics.median(values), "p95": p95, "samples": values}


def invoke(command, timeout):
    started = time.perf_counter()
    try:
        complete = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        return {"status": "timeout", "timeout_seconds": timeout,
                "wall_seconds": time.perf_counter() - started,
                "stderr": (error.stderr or b"").decode(errors="replace") if isinstance(error.stderr, bytes) else error.stderr or ""}
    elapsed = time.perf_counter() - started
    result = {"status": "ok" if complete.returncode == 0 else "failed",
              "returncode": complete.returncode, "wall_seconds": elapsed,
              "stdout": complete.stdout, "stderr": complete.stderr}
    match = TIMINGS.search(complete.stderr)
    if match:
        result["output_dimensions"] = [int(match[1]), int(match[2])]
        result["cli_seconds"] = dict(zip(("decode", "resize", "encode", "total"), map(float, match.groups()[2:])))
        result["wall_minus_cli_total_seconds"] = elapsed - result["cli_seconds"]["total"]
    if complete.returncode == 0 and complete.stdout:
        result["status"] = "failed"
        result["validation_error"] = "successful conversion emitted unexpected stdout"
    if complete.returncode == 0 and match is None:
        result["status"] = "failed"
        result["validation_error"] = "CLI timing line was missing"
    return result


def rss_worker(request):
    # This worker starts no subprocess except the single native conversion.
    result = invoke(request["command"], request["timeout"])
    try:
        import resource
        raw = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
        result["peak_rss_bytes"] = int(raw if sys.platform == "darwin" else raw * 1024)
        result["rss_method"] = "fresh worker RUSAGE_CHILDREN, bytes on macOS and KiB converted to bytes on Unix"
    except ImportError:
        result["peak_rss_bytes"] = None
        result["rss_method"] = "resource module unavailable on this platform"
    print(json.dumps(result))


def separate_rss(command, timeout):
    request = json.dumps({"command": command, "timeout": timeout})
    try:
        process = subprocess.run([sys.executable, str(Path(__file__).resolve()), "--rss-worker", request],
                                 capture_output=True, text=True, timeout=timeout + 10)
        if process.returncode:
            return {"status": "failed", "stderr": process.stderr, "returncode": process.returncode}
        return json.loads(process.stdout)
    except subprocess.TimeoutExpired:
        return {"status": "timeout", "timeout_seconds": timeout + 10}


def metadata_command(*command):
    try:
        return subprocess.check_output(command, stderr=subprocess.PIPE, text=True, timeout=10).strip()
    except (OSError, subprocess.SubprocessError) as error:
        return "unavailable: " + str(error)


def codec_versions():
    lock = (ROOT / "Cargo.lock").read_text()
    rust = dict(re.findall(r'name = "(image|moxcms|png|zune-jpeg|oxideav-[a-z0-9]+)"\nversion = "([^\"]+)"', lock))
    return {"rust": rust}



def prepare_inputs(work):
    import numpy as np
    from PIL import Image, __version__ as pillow_version
    work.mkdir(parents=True, exist_ok=True)
    manifest_path = work / "synthetic-inputs.json"
    png_path, jpeg_path = work / "synthetic-12mp.png", work / "synthetic-12mp.jpg"
    if manifest_path.exists() and png_path.exists() and jpeg_path.exists():
        previous = json.loads(manifest_path.read_text())
        if (previous.get("seed"), previous.get("width"), previous.get("height")) == (SEED, WIDTH, HEIGHT):
            if all(digest(Path(item["path"])) == item["sha256"] for item in previous["inputs"]):
                return previous
    rng = np.random.default_rng(SEED)
    x = np.linspace(0, 1, WIDTH, dtype=np.float32)[None, :]
    y = np.linspace(0, 1, HEIGHT, dtype=np.float32)[:, None]
    pixels = np.empty((HEIGHT, WIDTH, 3), dtype=np.uint8)
    fields = [32 + 180 * x + 12 * np.sin(6 * np.pi * y),
              24 + 200 * y + 10 * np.sin(4 * np.pi * x),
              38 + 110 * (0.65 * x + 0.35 * y) + 8 * np.sin(8 * np.pi * (x + y))]
    for channel, field in enumerate(fields):
        noise = rng.normal(0, 2, (HEIGHT, WIDTH)).astype(np.float32)
        pixels[..., channel] = np.rint(np.clip(field + noise, 0, 255)).astype(np.uint8)
    image = Image.fromarray(pixels, "RGB")
    image.save(png_path, compress_level=6)
    image.save(jpeg_path, quality=90, subsampling=2)
    manifest = {"seed": SEED, "width": WIDTH, "height": HEIGHT, "channels": 3,
                "pixel_sha256": hashlib.sha256(pixels.tobytes()).hexdigest(),
                "numpy": np.__version__, "pillow": pillow_version,
                "description": "Deterministic sRGB RGB gradients, sinusoidal color fields, and independent Gaussian noise sigma=2 code values; not a real photograph.",
                "preparation": {"png": "Pillow RGB8 PNG, compress_level=6, untagged sRGB assumption",
                                "jpeg": "Pillow RGB8 JPEG, quality=90, 4:2:0 subsampling, untagged sRGB assumption"},
                "inputs": [description(png_path), description(jpeg_path)]}
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def write_report(report, path):
    path.parent.mkdir(parents=True, exist_ok=True)
    write_private_report(path, report)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--work", type=Path, default=DEFAULT_WORK)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--timeout", type=float, default=180.0, help="bounded timeout per conversion, in seconds")
    parser.add_argument("--prepare-timeout", type=float, default=300.0, help="bounded timeout for the initial full-resolution HEIC encoding")
    parser.add_argument("--prepare-only", action="store_true", help="prepare PNG/JPEG fixtures only; do not launch the executable")
    args = parser.parse_args()
    if not all(math.isfinite(value) and value > 0 for value in (args.timeout, args.prepare_timeout)):
        parser.error("timeouts must be positive")
    inputs = prepare_inputs(args.work)
    if args.prepare_only:
        print(json.dumps(inputs, indent=2))
        return 0
    binary = args.binary.resolve(strict=True)
    initial_sha = digest(binary)
    report = {
        "complete": False,
        "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "environment": {"platform": platform.platform(), "machine": platform.machine(),
                        "logical_cpus": os.cpu_count(), "hardware": platform.machine(),
                        "python": platform.python_version(), "rustc": metadata_command("rustc", "--version"),
                        "binary_version": metadata_command(str(binary), "--version"), "binary": description(binary),
                        "codecs": codec_versions(),
                        "thread_environment": {name: os.environ.get(name) for name in ("RAYON_NUM_THREADS", "OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "VECLIB_MAXIMUM_THREADS")}},
        "protocol": {"measurement": "Full subprocess launch-to-exit wall clock, including startup, decoding, float conversion, kernel construction, resize, quantization, encoding, and atomic publication.",
                     "cli_phase_precision_seconds": 0.001, "warmup_runs": 1, "measured_runs": 5,
                     "percentile_method": "linear interpolation at rank (N-1)*0.95",
                     "camera": "cubic (upstream-derived default)", "scale": 0.5, "quality": 0.9,
                     "core_defaults": {"support": 32, "density": 8, "period_factor": 4, "padding": "reflect", "window": "hann", "table_interp": "cubic", "regularization": 0},
                     "max_memory_mib": 2048, "resize_threads": "Rayon automatic pool; --threads omitted, environment recorded",
                     "codec_threads": "PNG/JPEG portable Rust codecs use their normal build defaults. OxideAV HEIC encode/decode receives a separate available_parallelism worker budget capped at 8. --threads controls resize only.",
                     "rss": "One separate unmeasured native process per format, collected by a fresh resource wrapper; not added to timing samples.",
                     "limitations": ["Synthetic compression characteristics do not establish performance on arbitrary photographs.",
                                     "Lossy inputs contain different quantization/chroma information; cross-format timings are file-workflow measurements, not numerical speed comparisons.",
                                     "CLI total excludes some process startup/shutdown; externally measured wall time includes them.",
                                     "Peak RSS includes codec allocations and runtime overhead; working-buffer budget does not equal process RSS."]},
        "synthetic_inputs": inputs, "cases": []}
    write_report(report, args.report)
    png = args.work / "synthetic-12mp.png"
    heic = args.work / "synthetic-12mp.heic"
    prepare = [str(binary), str(png), str(heic), "--scale", "1", "--quality", "0.9", "--max-memory-mib", "2048", "--timings", "--force"]
    report["heic_input_preparation"] = {"command": prepare, "measurement": invoke(prepare, args.prepare_timeout)}
    if report["heic_input_preparation"]["measurement"]["status"] == "ok":
        report["heic_input_preparation"]["output"] = description(heic)
    write_report(report, args.report)
    all_ok = True
    for format_name, extension in [("png", "png"), ("jpeg", "jpg"), ("heic", "heic")]:
        source = args.work / f"synthetic-12mp.{extension}"
        destination = args.work / f"half.{extension}"
        command = [str(binary), str(source), str(destination), "--scale", "0.5", "--quality", "0.9", "--max-memory-mib", "2048", "--timings", "--force"]
        case = {"format": format_name, "command": command, "source": description(source) if source.exists() else None,
                "input_dimensions": [WIDTH, HEIGHT], "expected_output_dimensions": [WIDTH // 2, HEIGHT // 2], "samples": []}
        report["cases"].append(case)
        if format_name == "heic" and report["heic_input_preparation"]["measurement"]["status"] != "ok":
            case["status"] = "not_run"
            case["reason"] = "Full-resolution HEIC input preparation failed or timed out; not retried."
            all_ok = False
            write_report(report, args.report)
            continue
        case["warmup"] = invoke(command, args.timeout)
        if case["warmup"]["status"] != "ok":
            case["status"] = case["warmup"]["status"]
            all_ok = False
            write_report(report, args.report)
            continue
        for iteration in range(5):
            measured = invoke(command, args.timeout)
            measured["iteration"] = iteration + 1
            if measured["status"] == "ok":
                measured["output_bytes"] = destination.stat().st_size
                if measured.get("output_dimensions") != [WIDTH // 2, HEIGHT // 2]:
                    measured["status"] = "failed"
                    measured["validation_error"] = "unexpected output geometry"
            case["samples"].append(measured)
            write_report(report, args.report)
            if measured["status"] != "ok":
                break
        case["status"] = "ok" if len(case["samples"]) == 5 and all(x["status"] == "ok" for x in case["samples"]) else "incomplete"
        if case["status"] == "ok":
            case["summary_seconds"] = {"wall": summary([x["wall_seconds"] for x in case["samples"]])}
            for phase in ("decode", "resize", "encode", "total"):
                case["summary_seconds"]["cli_" + phase] = summary([x["cli_seconds"][phase] for x in case["samples"]])
            case["output"] = description(destination)
            case["rss_run"] = separate_rss(command, args.timeout)
            all_ok &= case["rss_run"]["status"] == "ok"
        else:
            all_ok = False
        write_report(report, args.report)
    report["binary_unchanged_during_run"] = digest(binary) == initial_sha
    report["complete"] = bool(all_ok and report["binary_unchanged_during_run"])
    write_report(report, args.report)
    print(json.dumps({"complete": report["complete"], "report": str(args.report), "cases": [
        {"format": item["format"], "status": item["status"], "wall_median_seconds": item.get("summary_seconds", {}).get("wall", {}).get("median"),
         "peak_rss_bytes": item.get("rss_run", {}).get("peak_rss_bytes")} for item in report["cases"]]}, indent=2))
    return 0 if report["complete"] else 1


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--rss-worker":
        rss_worker(json.loads(sys.argv[2]))
    else:
        raise SystemExit(main())
