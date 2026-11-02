#!/usr/bin/env python3
"""Development-only parity and benchmark runner for the unmodified pinned Python.

Requires NumPy, not an installed RRO package. Run from any working directory:
  python3 scripts/validate.py --parity
  python3 scripts/validate.py --benchmark

The helper request JSON contains input/output paths, width, height, channels,
options (scale_x/y, width/height, camera and documented numerical controls),
repeats, warmup and optional threads. Raw arrays use HWC little-endian float64.
Its stdout JSON gives output geometry, in-process samples_ms and worker count.
Pixel read/write, startup and worker-pool creation are outside numeric timings;
kernel construction and all resize allocation/computation are inside.
"""
import argparse
import gc
import hashlib
import json
import math
import os
from pathlib import Path
from report_privacy import write_report
import platform
import statistics
import subprocess
import sys
import time
import warnings

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from prepare_reference import PIN, verify_reference
import numpy as np

REFERENCE_DIR = None
REFERENCE_HASHES = None

def configure_reference(path=None):
    global REFERENCE_DIR, REFERENCE_HASHES, Cubic, Gaussian, IdealLowpass, resize
    if path is None and REFERENCE_DIR is not None:
        return REFERENCE_DIR
    external, hashes = verify_reference(path)
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(external))
    import resize_right_optics as oracle
    if external not in Path(oracle.__file__).resolve().parents:
        raise RuntimeError("another resize_right_optics package was imported instead of the pinned external oracle")
    Cubic, Gaussian, IdealLowpass, resize = oracle.Cubic, oracle.Gaussian, oracle.IdealLowpass, oracle.resize
    REFERENCE_DIR, REFERENCE_HASHES = external, hashes
    return external

REPORTS = ROOT / "reports"
WORK = REPORTS / "work"
HELPER = ROOT / "target/release/rro-numeric"


def command(*cmd):
    try:
        return subprocess.check_output(cmd, text=True, stderr=subprocess.PIPE).strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        return "unavailable: " + str(exc)


def environment():
    configure_reference()
    return {"upstream_commit": PIN, "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "platform": platform.platform(), "machine": platform.machine(), "python": platform.python_version(),
            "numpy": np.__version__, "rustc": command("rustc", "--version"),
            "hardware": platform.machine(), "logical_cpus": os.cpu_count(),
            "thread_environment": {k: os.environ.get(k) for k in ("OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "VECLIB_MAXIMUM_THREADS", "RAYON_NUM_THREADS")},
            "reference_sha256": REFERENCE_HASHES, "external_reference_directory": "<external-comparison-source>",
            "helper_sha256": hashlib.sha256(HELPER.read_bytes()).hexdigest()}


def original(data, options):
    configure_reference()
    camera_name = options.get("camera", "cubic")
    camera = {"cubic": lambda: Cubic(), "gaussian": lambda: Gaussian(options.get("sigma", .7)),
              "ideal-lowpass": lambda: IdealLowpass(options.get("cutoff", math.pi))}[camera_name]()
    scales = [options["scale_y"], options["scale_x"]] if "scale_x" in options else None
    sizes = [options["height"], options["width"]] if "width" in options else None
    settings = dict(camera=camera, dims=[0, 1], support_sz=options.get("support", 32),
                    density=options.get("density", 8), period_factor=options.get("period_factor", 4),
                    pad_mode=options.get("padding", "reflect"), normalize=options.get("normalize", True),
                    window=None if options.get("window") == "none" else options.get("window", "hann"),
                    table_interp=options.get("table_interp", "cubic"), regularization=options.get("regularization", 0),
                    rcond=options.get("rcond", 1e-12))
    return resize(data, scales, out_shape=sizes, **settings)



def benchmark_output_parity(rows):
    comparisons = [row[key]["parity_passed"] for row in rows
                   for key in ("rust_1thread", "rust_default")
                   if "parity_passed" in row.get(key, {})]
    return dict(status=("passed" if all(comparisons) else "failed") if comparisons else "not_checked",
                comparisons_checked=len(comparisons), comparisons_possible=len(rows) * 2,
                reason="Fresh Python outputs are compared when available; reused timing baselines contain no output arrays. Direct fixture parity is recorded separately.")


def rss_command_worker(request):
    command = request["command"]
    try:
        completed = subprocess.run(command, capture_output=True, text=True, timeout=request["timeout"])
        payload = dict(returncode=completed.returncode, stdout=completed.stdout, stderr=completed.stderr)
    except subprocess.TimeoutExpired:
        payload = dict(returncode=-1, stdout="", stderr="RSS measurement subprocess timed out")
    try:
        import resource
        raw = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
        payload["peak_rss_bytes"] = int(raw if sys.platform == "darwin" else raw * 1024)
        payload["rss_method"] = "fresh Python worker RUSAGE_CHILDREN; Darwin bytes, other Unix KiB converted to bytes"
    except ImportError:
        payload["peak_rss_bytes"] = None
        payload["rss_method"] = "resource module unavailable on this platform"
    print(json.dumps(payload))


def measured_process(command, timeout=600):
    request = json.dumps(dict(command=command, timeout=timeout))
    worker = subprocess.run([sys.executable, str(Path(__file__).resolve()), "--rss-command", request],
                            capture_output=True, text=True, timeout=timeout + 30)
    if worker.returncode:
        raise RuntimeError(worker.stderr[-4000:])
    return json.loads(worker.stdout)


def rust(data, options, name="case", repeats=1, warmup=0, threads=1, rss=False):
    prefix = WORK / name
    raw_input = prefix.with_suffix(".in.f64")
    raw_output = prefix.with_suffix(".out.f64")
    request_path = prefix.with_suffix(".json")
    data.astype("<f8", copy=False).tofile(raw_input)
    request = dict(input=str(raw_input), output=str(raw_output), width=data.shape[1], height=data.shape[0],
                   channels=data.shape[2], options=options, repeats=repeats, warmup=warmup, threads=threads)
    request_path.write_text(json.dumps(request))
    invocation = [str(HELPER), str(request_path)]
    measured = measured_process(invocation) if rss else None
    completed = subprocess.run(invocation, capture_output=True, text=True, timeout=600) if not rss else None
    returncode = measured["returncode"] if measured else completed.returncode
    stdout = measured["stdout"] if measured else completed.stdout
    stderr = measured["stderr"] if measured else completed.stderr
    if returncode:
        raise RuntimeError(stderr[-4000:])
    result = json.loads(stdout)
    output = np.fromfile(raw_output, dtype="<f8").reshape(result["height"], result["width"], result["channels"])
    result["warnings"] = stderr.strip()
    if measured:
        result["peak_rss_bytes"] = measured["peak_rss_bytes"]
        result["rss_method"] = measured["rss_method"]
    return output, result


def patterns(shape):
    h, w, c = shape
    y, x = np.mgrid[:h, :w]
    rng = np.random.default_rng(h * 1009 + w * 13 + c)
    return {
        "random": rng.random(shape),
        "constant": np.full(shape, .37),
        "impulse": np.broadcast_to(((x == w // 2) & (y == h // 2))[..., None], shape).astype(float).copy(),
        "ramp": np.broadcast_to((x / max(1, w - 1) + y / max(1, h - 1))[..., None] / 2, shape).copy(),
        "checkerboard": np.broadcast_to(((x + y) % 2)[..., None], shape).astype(float).copy(),
        "sinusoidal": np.broadcast_to((.5 + .2 * np.sin(.21 * x) + .2 * np.cos(.31 * y))[..., None], shape).copy(),
        "signed": rng.normal(size=shape) * 4,
    }


def parity():
    fixtures = []
    shapes = [(1, 1, 1), (1, 7, 3), (7, 1, 4), (7, 9, 3), (8, 12, 3), (17, 13, 4), (31, 37, 3)]
    scales = [.2, .5, .75, 1., 1.5, math.sqrt(2)]
    cameras = ["cubic", "gaussian", "ideal-lowpass"]
    pattern_names = list(patterns((1, 1, 1)))
    for ci, camera in enumerate(cameras):
        for si, shape in enumerate(shapes):
            for qi, scale in enumerate(scales):
                pattern = pattern_names[(ci + si + qi) % len(pattern_names)]
                fixtures.append((f"{camera}-{shape}-{scale:g}-{pattern}", patterns(shape)[pattern],
                                 dict(camera=camera, scale_x=scale, scale_y=scale)))
    base = patterns((11, 17, 4))
    for camera in cameras:
        for padding in ("reflect", "symmetric", "edge", "wrap", "constant"):
            for window in ("hann", "lanczos", "none"):
                fixtures.append((f"modes-{camera}-{padding}-{window}", base["signed"], dict(camera=camera,
                    padding=padding, window=window, scale_x=.66, scale_y=1.3)))
        for interpolation in ("linear", "cubic"):
            fixtures.append((f"nonpower-{camera}-{interpolation}", base["impulse"], dict(camera=camera,
                scale_x=.63, scale_y=1.27, table_interp=interpolation, support=17.25, density=5.5, period_factor=3.25)))
        fixtures += [
            (f"sizes-{camera}", base["random"], dict(camera=camera, width=9, height=16)),
            (f"scale-and-sizes-{camera}", base["ramp"], dict(camera=camera, scale_x=.5, scale_y=.75, width=11, height=10)),
            (f"center-crop-{camera}", base["random"], dict(camera=camera, scale_x=1., scale_y=1., width=12, height=7)),
            (f"regularized-{camera}", base["sinusoidal"], dict(camera=camera, scale_x=2., scale_y=1.5, regularization=.01)),
            (f"unnormalized-{camera}", base["random"], dict(camera=camera, scale_x=.7, scale_y=.9, normalize=False)),
            (f"rcond-{camera}", base["random"], dict(camera=camera, scale_x=.5, scale_y=1.5, rcond=.05)),
            (f"wide-{camera}", base["checkerboard"], dict(camera=camera, scale_x=.1, scale_y=1.75, support=64, density=16, period_factor=8)),
        ]
    # Floating products at integer boundaries exercise upstream's ceil roundoff guard.
    for s in (.1, .2, .3, 2/3):
        fixtures.append((f"rounding-{s}", patterns((30, 90, 3))["ramp"], dict(scale_x=s, scale_y=s)))
    rows = []
    for i, (name, data, options) in enumerate(fixtures):
        with warnings.catch_warnings(record=True) as caught:
            expected = original(data, options)
        try:
            actual, payload = rust(data, options)
            if expected.shape != actual.shape:
                raise AssertionError(f"shape mismatch: {expected.shape} vs {actual.shape}")
            diff = actual - expected
            maximum = float(np.max(np.abs(diff)))
            rmse = float(np.sqrt(np.mean(diff ** 2)))
            passed = bool(np.isfinite(actual).all() and maximum <= 1e-9 and rmse <= 1e-10)
            rows.append(dict(name=name, input_shape=data.shape, output_shape=actual.shape, options=options,
                max_abs=maximum, rmse=rmse, passed=passed, python_warnings=[str(w.message) for w in caught], rust_warnings=payload["warnings"]))
        except Exception as e:
            rows.append(dict(name=name, input_shape=data.shape, options=options, passed=False, error=str(e)))
        if (i + 1) % 25 == 0:
            print(f"parity {i + 1}/{len(fixtures)}", flush=True)
    report = dict(environment=environment(), protocol="float64 HWC, pre-clipping direct core channels; maxabs1e-9,RMSE1e-10",
                  count=len(rows), passed=sum(row["passed"] for row in rows), cases=rows)
    write_report(REPORTS / "parity.json", report)
    failed = [r for r in rows if not r["passed"]]
    worst = max((r.get("max_abs", 0) for r in rows), default=0)
    print(f"Parity: {report['passed']}/{len(rows)} passed; worst absolute error {worst:.4g}", flush=True)
    if failed:
        print(json.dumps(failed[:10], indent=2))
    return not failed


def timing_summary(samples):
    return dict(samples_ms=samples, median_ms=statistics.median(samples), p95_ms=float(np.percentile(samples, 95)))


def python_benchmark(data, options, timeout):
    prefix = WORK / "python-benchmark"
    raw_input = prefix.with_suffix(".in.f64")
    raw_output = prefix.with_suffix(".out.f64")
    request_path = prefix.with_suffix(".json")
    data.astype("<f8", copy=False).tofile(raw_input)
    request = dict(input=str(raw_input), output=str(raw_output), width=data.shape[1], height=data.shape[0],
                   channels=data.shape[2], options=options, repeats=5, warmup=1)
    request_path.write_text(json.dumps(request))
    invocation = [sys.executable, str(Path(__file__).resolve()), "--bench-python", str(request_path), "--reference-dir", str(REFERENCE_DIR)]
    measured = subprocess.run(invocation, capture_output=True, text=True, timeout=timeout)
    if measured.returncode:
        raise RuntimeError(measured.stderr[-4000:])
    payload = json.loads(measured.stdout)
    output = np.fromfile(raw_output, dtype="<f8").reshape(payload["shape"])
    return output, timing_summary(payload["samples_ms"])


def benchmark(reuse=None, reference_only=False, python_timeout=600):
    prior = None
    if reuse:
        prior = json.loads(Path(reuse).read_text())
        prior_environment = prior["environment"]
        if prior_environment["upstream_commit"] != PIN or prior_environment["numpy"] != np.__version__ or prior_environment["reference_sha256"] != REFERENCE_HASHES:
            raise RuntimeError("cached baseline uses another oracle pin or NumPy version")
    cached = {(tuple(row["input_shape"]), row["camera"], row["scale"]): row for row in (prior or {}).get("cases", [])}
    report_path = REPORTS / ("numpy-baseline.json" if reference_only else "benchmark.json")
    history = dict(source=str(Path(reuse).resolve()), sha256=hashlib.sha256(Path(reuse).read_bytes()).hexdigest(),
                   environment=prior["environment"], note="Reused complete five-sample NumPy results from the same deterministic fixture and pinned implementation; measurement phases may overlap unrelated compilation.") if prior else None
    rows = []
    for h, w in ((192, 192), (1024, 1024), (3024, 4032)):
        y, x = np.mgrid[:h, :w]
        base = .5 + .15 * np.sin(x * .02) + .15 * np.cos(y * .013)
        data = np.stack((base, base * .8, base * .6), axis=-1).astype(np.float64)
        del x, y, base
        for camera in ("cubic", "gaussian"):
            for scale in (.5, .75, 1.5):
                options = dict(camera=camera, scale_x=scale, scale_y=scale)
                row = dict(input_shape=data.shape, camera=camera, scale=scale, options=options)
                print(f"benchmark {h}x{w} {camera} scale={scale}", flush=True)
                reference_output = None
                cached_row = cached.get((data.shape, camera, scale))
                if cached_row and "median_ms" in cached_row.get("numpy", {}) and cached_row.get("options") == options:
                    row["numpy"] = cached_row["numpy"]
                    row["numpy_baseline_reused"] = True
                else:
                    try:
                        reference_output, row["numpy"] = python_benchmark(data, options, python_timeout)
                        row["numpy_baseline_reused"] = False
                    except subprocess.TimeoutExpired:
                        row["numpy"] = dict(error=f"warmup plus five measured iterations exceeded {python_timeout}s", timeout_seconds=python_timeout)
                    except Exception as e:
                        row["numpy"] = dict(error=str(e))
                if not reference_only:
                    for threads, key in ((1, "rust_1thread"), (None, "rust_default")):
                        try:
                            output, payload = rust(data, options, "bench", repeats=5, warmup=1, threads=threads)
                            row[key] = dict(**timing_summary(payload["samples_ms"]), threads=payload["threads"])
                            if reference_output is not None:
                                if reference_output.shape != output.shape:
                                    raise AssertionError("benchmark output shape mismatch")
                                expected_flat = reference_output.reshape(-1)
                                actual_flat = output.reshape(-1)
                                maximum = 0.0
                                sum_squares = 0.0
                                for offset in range(0, len(expected_flat), 1048576):
                                    difference = actual_flat[offset:offset+1048576] - expected_flat[offset:offset+1048576]
                                    maximum = max(maximum, float(np.max(np.abs(difference))))
                                    sum_squares += float(np.sum(difference ** 2))
                                rmse = math.sqrt(sum_squares / len(expected_flat))
                                row[key]["max_abs_vs_numpy"] = maximum
                                row[key]["rmse_vs_numpy"] = rmse
                                row[key]["parity_passed"] = maximum <= 1e-9 and rmse <= 1e-10
                                del expected_flat, actual_flat, difference
                            del output
                        except Exception as e:
                            row[key] = dict(error=str(e))
                    if "median_ms" in row.get("numpy", {}) and "median_ms" in row.get("rust_default", {}):
                        row["speedup_default_vs_numpy"] = row["numpy"]["median_ms"] / row["rust_default"]["median_ms"]
                del reference_output
                rows.append(row)
                write_report(report_path, dict(environment=environment(), complete=False, baseline_history=history, cases=rows))
        del data
        gc.collect()
    if reference_only:
        complete = all("median_ms" in row.get("numpy", {}) for row in rows)
        write_report(report_path, dict(environment=environment(), complete=complete, baseline_history=history, protocol="warmup1, measured5 in process; kernel construction included; input/output and process start excluded", cases=rows))
        print(f"NumPy baseline: {len(rows)} cases; complete={complete}", flush=True)
        return complete
    rss = []
    data = np.full((3024, 4032, 3), .5, dtype=np.float64)
    for camera in ("cubic", "gaussian"):
        options = dict(camera=camera, scale_x=.5, scale_y=.5)
        try:
            output, payload = rust(data, options, "rss", rss=True, threads=None)
            del output
            rust_rss = payload.get("peak_rss_bytes")
            request = WORK / "rss.json"
            measured = measured_process([sys.executable, str(Path(__file__).resolve()), "--rss-python", str(request), "--reference-dir", str(REFERENCE_DIR)], timeout=python_timeout)
            rss.append(dict(camera=camera, shape=data.shape, scale=.5, rust_peak_rss_bytes=rust_rss,
                            python_peak_rss_bytes=measured["peak_rss_bytes"],
                            rust_rss_method=payload["rss_method"], python_rss_method=measured["rss_method"],
                            python_returncode=measured["returncode"], python_stderr=measured["stderr"]))
        except Exception as e:
            rss.append(dict(camera=camera, error=str(e)))
    targets = {
        "large_images_no_slower_than_numpy": all(row.get("speedup_default_vs_numpy", 0) >= 1 for row in rows if row["input_shape"][0] >= 1024),
        "12mp_halfscale_under_5s": all(row.get("rust_default", {}).get("median_ms", float("inf")) < 5000 for row in rows if row["input_shape"][0] == 3024 and row["scale"] == .5),
    }
    report = dict(environment=environment(), complete=all("median_ms" in row.get("numpy", {}) and "median_ms" in row.get("rust_default", {}) and "median_ms" in row.get("rust_1thread", {}) for row in rows),
                  protocol="warmup1, measured5, kernel construction included, I/O/startup/pool creation excluded", baseline_history=history, cases=rows,
                  peak_rss=rss, targets=targets, benchmark_output_parity=benchmark_output_parity(rows),
                  parity_note="202-fixture parity is recorded separately. Where fresh Python benchmark output was available it was also compared directly; reused timing baselines do not retain output arrays.",
                  rss_note="Fresh resource.RUSAGE_CHILDREN worker measures one child per run; Darwin bytes, other Unix KiB normalized to bytes. Includes child startup, helper interchange or Python imports, not just resize buffers. /usr/bin/time -l is unavailable in this restricted environment.")
    write_report(report_path, report)
    print("Benchmark targets:", targets, flush=True)
    return report["complete"] and all(targets.values()) and report["benchmark_output_parity"]["status"] != "failed"


def bench_python(path):
    request = json.loads(Path(path).read_text())
    data = np.fromfile(request["input"], dtype="<f8").reshape(request["height"], request["width"], request["channels"])
    for _ in range(request.get("warmup", 1)):
        output = original(data, request["options"])
        del output
    samples = []
    output = None
    for sample in range(request.get("repeats", 5)):
        del output
        gc.collect()
        start = time.perf_counter()
        output = original(data, request["options"])
        samples.append((time.perf_counter() - start) * 1000)
        Path(path).with_suffix(".progress.json").write_text(json.dumps(dict(completed_samples=sample+1, samples_ms=samples)))
    output.astype("<f8", copy=False).tofile(request["output"])
    print(json.dumps(dict(shape=output.shape, samples_ms=samples)))


def rss_python(path):
    request = json.loads(Path(path).read_text())
    data = np.fromfile(request["input"], dtype="<f8").reshape(request["height"], request["width"], request["channels"])
    output = original(data, request["options"])
    print(output.shape)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parity", action="store_true")
    parser.add_argument("--benchmark", action="store_true")
    parser.add_argument("--rss-command")
    parser.add_argument("--rss-python")
    parser.add_argument("--bench-python")
    parser.add_argument("--reuse-python-baseline", type=Path)
    parser.add_argument("--reference-only", action="store_true", help="measure NumPy only and save reports/numpy-baseline.json")
    parser.add_argument("--python-case-timeout", type=float, default=600, help="maximum subprocess seconds for warmup plus five original iterations")
    parser.add_argument("--reference-dir", type=Path, help="external directory containing the pinned resize_right_optics package")
    args = parser.parse_args()
    if args.rss_command:
        rss_command_worker(json.loads(args.rss_command))
        return
    configure_reference(args.reference_dir)
    if args.rss_python:
        rss_python(args.rss_python)
        return
    if args.bench_python:
        bench_python(args.bench_python)
        return
    REPORTS.mkdir(exist_ok=True)
    WORK.mkdir(exist_ok=True)
    if not HELPER.exists():
        parser.error("build release binaries first: cargo build --release --offline --bins")
    success = True
    if args.parity or not args.benchmark:
        success = parity() and success
    if args.benchmark:
        success = benchmark(args.reuse_python_baseline, args.reference_only, args.python_case_timeout) and success
    sys.exit(0 if success else 1)

if __name__ == "__main__":
    main()
