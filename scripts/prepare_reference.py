#!/usr/bin/env python3
"""Prepare the development-only Python oracle in an external cache.

No upstream implementation is stored in this repository or shipped with the
application/source release. This command downloads exactly the pinned library
files and license to ~/.cache/rro/reference/<commit>/python by default.
Set RRO_REFERENCE_DIR or --reference-dir to another external directory.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = json.loads((ROOT / "docs/reference-manifest.json").read_text())
PIN = MANIFEST["commit"]


def reference_directory(explicit=None):
    selected = explicit or os.environ.get("RRO_REFERENCE_DIR")
    path = Path(selected).expanduser() if selected else Path.home() / ".cache/rro/reference" / PIN / "python"
    path = path.resolve()
    if path == ROOT or ROOT in path.parents:
        raise RuntimeError("the original Python reference must be outside the project directory")
    return path


def verify_reference(explicit=None):
    path = reference_directory(explicit)
    hashes = {}
    for name, expected in MANIFEST["library_sha256"].items():
        source = path / "resize_right_optics" / name
        if not source.is_file():
            raise RuntimeError(f"Python oracle is absent at {path}; run scripts/prepare_reference.py --reference-dir {path}")
        actual = hashlib.sha256(source.read_bytes()).hexdigest()
        if actual != expected:
            raise RuntimeError(f"Python oracle hash mismatch for {name}; expected pinned commit {PIN}")
        hashes[name] = actual
    return path, hashes


def prepare(explicit=None):
    path = reference_directory(explicit)
    package = path / "resize_right_optics"
    package.mkdir(parents=True, exist_ok=True)
    base = f"https://raw.githubusercontent.com/assafshocher/ResizeRightOptics/{PIN}"
    for name, expected in MANIFEST["library_sha256"].items():
        destination = package / name
        if destination.is_file() and hashlib.sha256(destination.read_bytes()).hexdigest() == expected:
            continue
        with urllib.request.urlopen(f"{base}/resize_right_optics/{name}", timeout=60) as response:
            content = response.read(262144)
        if hashlib.sha256(content).hexdigest() != expected:
            raise RuntimeError(f"downloaded reference did not match pinned hash: {name}")
        # Keep partially downloaded files out of the importable package.
        with tempfile.NamedTemporaryFile(dir=package, prefix=".download-", delete=False) as staging:
            staging.write(content)
            staging_path = Path(staging.name)
        staging_path.replace(destination)
    license_path = path / "LICENSE"
    if not license_path.exists():
        with urllib.request.urlopen(f"{base}/LICENSE", timeout=60) as response:
            license_path.write_bytes(response.read(65536))
    verify_reference(path)
    print(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-dir", type=Path)
    args = parser.parse_args()
    prepare(args.reference_dir)

if __name__ == "__main__":
    main()
