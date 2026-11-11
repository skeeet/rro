#!/usr/bin/env python3
"""Reject stale or failing functional evidence before making a release archive."""
import hashlib
import json
import math
from pathlib import Path
import statistics

ROOT=Path(__file__).resolve().parents[1]

def exact_ids(rows, field, expected):
    actual=[row.get(field) for row in rows]
    return len(actual)==len(expected) and len(set(actual))==len(actual) and set(actual)==set(expected)

def finite_between(value, lower, upper):
    return isinstance(value,(int,float)) and not isinstance(value,bool) and math.isfinite(value) and lower<=value<=upper

def numeric_metrics_pass(rows, maximum_key, contract):
    return all(finite_between(row.get(maximum_key),0,contract["absolute_tolerance"])
               and finite_between(row.get("rmse"),0,contract["rmse_tolerance"]) for row in rows)

def timings_pass(rows):
    for row in rows:
        medians={}
        for key in ("numpy","rust_1thread","rust_default"):
            timing=row.get(key,{})
            samples=timing.get("samples_ms",[])
            if len(samples)!=5 or not all(finite_between(value,0,1e12) for value in samples):
                return False
            median=statistics.median(samples)
            if not math.isclose(timing.get("median_ms",-1),median,rel_tol=1e-12,abs_tol=1e-12):
                return False
            medians[key]=median
        if row["input_shape"][0]>=1024 and medians["rust_default"]>medians["numpy"]:
            return False
        if row["input_shape"][0]==3024 and row["scale"]==.5 and medians["rust_default"]>=5000:
            return False
    return True

def image_metrics_pass(rows, contract):
    for row in rows:
        name=row.get("name","")
        maximum=row.get("max_code_difference")
        expected_type=contract["png_color_types"].get(name)
        if expected_type is not None and row.get("png_color_type")!=expected_type:
            return False
        if name.startswith("orientation-png-") and (
                "output_orientation" not in row or row["output_orientation"] not in (None,1)):
            return False
        expected_size=contract["image_output_sizes"].get(name)
        if expected_size is not None and (row.get("output_size")!=expected_size
                or row.get("output_xmp_present") is not False
                or row.get("source_unchanged") is not True):
            return False
        if name.startswith(("png-","alpha-","orientation-png-")):
            if not finite_between(maximum,0,1):
                return False
        elif name.startswith("orientation-"):
            if not finite_between(maximum,0,3):
                return False
        elif not (finite_between(maximum,0,255) and
                  (row.get("psnr_db") is None and maximum==0 or finite_between(row.get("psnr_db"),30,1e12))):
            return False
    return True

def check(root=ROOT):
    """Validate complete, internally consistent evidence against exact artifacts."""
    binary=hashlib.sha256((root/"target/release/rro").read_bytes()).hexdigest()
    helper=hashlib.sha256((root/"target/release/rro-numeric").read_bytes()).hexdigest()
    contract=json.loads((root/"docs/release-contract.json").read_text())
    manifest=json.loads((root/"docs/reference-manifest.json").read_text())
    parity=json.loads((root/"reports/parity.json").read_text())
    if (parity.get("passed")!=202 or parity.get("count")!=202
            or len(parity.get("cases",[]))!=202 or not all(row.get("passed") is True for row in parity["cases"])
            or not exact_ids(parity["cases"],"name",contract["parity_case_names"])
            or not numeric_metrics_pass(parity["cases"],"max_abs",contract)
            or parity.get("environment",{}).get("reference_sha256")!=manifest["library_sha256"]
            or parity.get("environment",{}).get("helper_sha256")!=helper):
        raise SystemExit("Release rejected: run original-Python parity against the current numeric helper.")
    images=json.loads((root/"reports/image-parity.json").read_text())
    expected_image_count=len(contract["image_case_names"])
    if (images.get("passed")!=expected_image_count or images.get("count")!=expected_image_count or len(images.get("cases",[]))!=expected_image_count
            or not all(row.get("passed") is True for row in images["cases"])
            or not exact_ids(images["cases"],"name",contract["image_case_names"])
            or not image_metrics_pass(images["cases"],contract)
            or images.get("reference_sha256")!=manifest["library_sha256"]
            or images.get("failed") or images.get("unverified") or images.get("binary_sha256")!=binary
            or images.get("binary_unchanged_during_checks") is not True):
        raise SystemExit("Release rejected: run all image pipeline checks against the current executable.")
    independent=json.loads((root/"reports/independent-codec-check.json").read_text())
    names=("limited_range_HEIC_input_vs_ffmpeg", "kvazaar_HEIC_input_vs_ffmpeg", "our_HEIC_output_decode_vs_ffmpeg")
    if (independent.get("binary_sha256")!=binary or independent.get("count")!=3 or independent.get("passed")!=3
            or independent.get("binary_unchanged_during_checks") is not True
            or not all(independent.get(name,{}).get("pass") is True
                       and independent[name].get("allowed_max_code_difference")==2
                       and finite_between(independent[name].get("max_code_difference"),0,2) for name in names)):
        raise SystemExit("Release rejected: independent HEIC input/output interoperability must pass for the current executable.")
    fuzz=json.loads((root/"reports/core-fuzz.json").read_text())
    if (fuzz.get("passed") is not True or fuzz.get("cases")!=100 or len(fuzz.get("results",[]))!=100
            or not all(row.get("passed") is True for row in fuzz["results"])
            or not exact_ids(fuzz["results"],"number",contract["randomized_case_numbers"])
            or not numeric_metrics_pass(fuzz["results"],"max_abs_error",contract)
            or fuzz.get("tolerances")!={"max_abs_error":contract["absolute_tolerance"],"rmse":contract["rmse_tolerance"]}
            or fuzz.get("environment",{}).get("reference_sha256")!=manifest["library_sha256"]
            or fuzz.get("environment",{}).get("helper_sha256")!=helper):
        raise SystemExit("Release rejected: refresh all 100 randomized parity cases against the current numeric helper.")
    benchmark=json.loads((root/"reports/benchmark.json").read_text())
    benchmark_ids=[tuple(row.get("input_shape",[]))+ (row.get("camera"),row.get("scale")) for row in benchmark.get("cases",[])]
    expected_benchmark_ids=[tuple(row) for row in contract["benchmark_cases"]]
    if (benchmark.get("complete") is not True or len(benchmark.get("cases",[]))!=18
            or len(set(benchmark_ids))!=18 or set(benchmark_ids)!=set(expected_benchmark_ids)
            or not timings_pass(benchmark["cases"])
            or benchmark.get("environment",{}).get("helper_sha256")!=helper
            or set(benchmark.get("targets",{}))!=set(contract["performance_targets"])
            or not all(value is True for value in benchmark["targets"].values())
            or benchmark.get("benchmark_output_parity",{}).get("status") not in {"passed","not_checked"}):
        raise SystemExit("Release rejected: refresh the complete performance matrix and pass its measured targets for the current numeric helper.")
    io=json.loads((root/"reports/io-benchmark.json").read_text())
    if (io.get("complete") is not True or io.get("binary_unchanged_during_run") is not True
            or io.get("environment",{}).get("binary",{}).get("sha256")!=binary
            or {case.get("format") for case in io.get("cases",[])}!={"png","jpeg","heic"}
            or len(io.get("cases",[]))!=3
            or not all(case.get("status")=="ok" for case in io["cases"])):
        raise SystemExit("Release rejected: refresh the complete PNG/JPEG/HEIC file benchmark for the current executable.")
    print(f"Functional and performance evidence matches executable {binary} and numeric helper {helper}.")

if __name__=="__main__": check()
