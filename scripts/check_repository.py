#!/usr/bin/env python3
"""Reject legacy sources and unsafe links in product/rebuild file inventories."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from report_privacy import privacy_issues

ROOT = Path(__file__).resolve().parents[1]
DEV_SCRIPTS = {
    "benchmark_io.py", "check_release.py", "collect_licenses.py", "core_fuzz.py",
    "image_validate.py", "independent_codec_check.py", "prepare_reference.py",
    "validate.py", "test_release_checks.py", "check_repository.py", "audit_archives.py", "report_privacy.py", "create_archive.py",
}
FORBIDDEN_DIRECTORIES = {
    "upstream", "native", "vendor", "__pycache__", ".venv", "venv",
    ".git", ".idea", ".agents", ".claude", ".zcode", "reports",
}
FORBIDDEN_SUFFIXES = {".pyc", ".pyo", ".c", ".h", ".cc", ".cpp", ".a", ".o", ".so", ".dylib"}
FORBIDDEN_ROOTS = {"skills-lock.json", "provenance"}


def problems(path, relative, original_hashes):
    errors = []
    if relative.is_absolute() or ".." in relative.parts:
        errors.append("unsafe archive path")
    if relative.parts and relative.parts[0] in FORBIDDEN_ROOTS:
        errors.append("local-only or removed root path")
    if any(part in FORBIDDEN_DIRECTORIES or part.startswith("._") for part in relative.parts):
        errors.append("legacy/generated/local-only path")
    if path.is_symlink():
        return errors + ["symbolic links are not allowed in product source inventories"]
    if not path.is_file():
        return errors
    if relative.suffix.lower() in FORBIDDEN_SUFFIXES:
        errors.append("legacy implementation/build artifact")
    if relative.suffix == ".py" and (relative.parent != Path("scripts") or relative.name not in DEV_SCRIPTS):
        errors.append("Python outside the explicit development-tool allowlist")
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    errors.extend(privacy_issues(path.read_bytes()))
    if digest in original_hashes:
        errors.append("pinned original Python implementation content")
    return errors


def check(root, staged=False):
    manifest = json.loads((ROOT / "docs/reference-manifest.json").read_text())
    hashes = set(manifest["library_sha256"].values())
    if staged:
        files = [path for path in root.rglob("*") if not path.is_dir() or path.is_symlink()]
    else:
        git_root = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=root,
                                  capture_output=True, text=True)
        if git_root.returncode == 0 and Path(git_root.stdout.strip()).resolve() == root.resolve():
            names = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root)
            files = [root / name.decode() for name in names.split(b"\0") if name]
        else:
            # Rebuild archives intentionally have no Git metadata. Apply only the
            # documented local-output exclusions; legacy source directories still fail.
            excluded = {"target", "dist", "reports", ".git", ".idea", ".agents", ".claude", ".zcode"}
            files = [path for path in root.rglob("*")
                     if (not path.is_dir() or path.is_symlink())
                     and not any(part in excluded or part == "__pycache__" for part in path.relative_to(root).parts)
                     and path.relative_to(root).parts[:2] != ("reports", "work")]
    findings = {str(path.relative_to(root)): issues for path in files
                if (issues := problems(path, path.relative_to(root), hashes))}
    if findings:
        raise SystemExit(json.dumps(findings, indent=2))
    print(f"Repository inventory passed: {len(files)} files, no original Python or legacy native sources.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--staged-directory", type=Path)
    args = parser.parse_args()
    check(args.staged_directory.resolve() if args.staged_directory else ROOT,
          staged=args.staged_directory is not None)
