#!/usr/bin/env python3
"""Verify packaged inventory, hashes, and archive paths without extracting files."""
import hashlib
import json
from pathlib import Path, PurePosixPath
from report_privacy import write_report, privacy_issues
import tarfile

import check_repository

ROOT = Path(__file__).resolve().parents[1]


def header_issues(entry):
    issues = []
    if entry.uid != 0 or entry.gid != 0 or entry.uname or entry.gname:
        issues.append("archive owner metadata is not anonymous")
    if entry.mtime != 0:
        issues.append("archive timestamp is not normalized")
    if set(entry.pax_headers) - {"path", "size"}:
        issues.append("archive contains extended local metadata")
    if privacy_issues(json.dumps(entry.pax_headers).encode()):
        issues.append("private archive extended metadata")
    return issues


def main():
    expected = {}
    for line in (ROOT / "dist/SHA256SUMS").read_text().splitlines():
        digest, name = line.split(maxsplit=1)
        relative = Path(name.strip())
        if relative.is_absolute() or ".." in relative.parts or relative.parts[0] != "dist":
            raise RuntimeError("unsafe checksum manifest path")
        expected[relative.name] = digest
        actual = hashlib.sha256((ROOT / relative).read_bytes()).hexdigest()
        if actual != digest:
            raise RuntimeError(f"checksum mismatch: {relative}")
    upstream = set(json.loads((ROOT / "docs/reference-manifest.json").read_text())["library_sha256"].values())
    report = dict(checksums_match=True, archives={}, source_files_match_checkout=True,
                  packaged_binary_matches=True)
    for name in sorted(expected):
        if not name.endswith(".tar.gz"):
            continue
        source = "-rebuild-source" in name
        count = 0
        roots = set()
        seen = set()
        installer = Path("scripts/install.sh") if source else Path("install.sh")
        installer_seen = False
        with tarfile.open(ROOT / "dist" / name, "r:gz") as archive:
            for entry in archive:
                if header_issues(entry):
                    raise RuntimeError(f"non-anonymous archive header: {entry.name}")
                path = PurePosixPath(entry.name)
                if path.is_absolute() or ".." in path.parts or not path.parts or entry.name in seen:
                    raise RuntimeError(f"unsafe or duplicate archive member: {entry.name}")
                seen.add(entry.name)
                roots.add(path.parts[0])
                if len(roots) != 1 or not (entry.isdir() or entry.isfile()):
                    raise RuntimeError(f"unsafe archive entry type: {entry.name}")
                relative = Path(*path.parts[1:])
                if relative.parts and relative.parts[0] in check_repository.FORBIDDEN_ROOTS:
                    raise RuntimeError(f"local-only or removed archive path: {entry.name}")
                if any(part.startswith("._") or part in check_repository.FORBIDDEN_DIRECTORIES for part in relative.parts):
                    raise RuntimeError(f"forbidden archive path: {entry.name}")
                if not entry.isfile():
                    continue
                count += 1
                if relative.parts and relative.parts[0] == "reports":
                    raise RuntimeError("development reports are forbidden in product/source archives")
                content = archive.extractfile(entry).read()
                if privacy_issues(content):
                    raise RuntimeError(f"private local metadata in archive: {relative}")
                digest = hashlib.sha256(content).hexdigest()
                if relative == installer:
                    installer_seen = True
                    if content != (ROOT / "scripts/install.sh").read_bytes():
                        raise RuntimeError("archived installer differs from checkout")
                if digest in upstream:
                    raise RuntimeError(f"original Python content in archive: {entry.name}")
                if relative.suffix == ".py" and (not source or relative.parent != Path("scripts") or relative.name not in check_repository.DEV_SCRIPTS):
                    raise RuntimeError(f"unexpected Python source in archive: {entry.name}")
                if source:
                    checkout = ROOT / relative
                    if not checkout.is_file() or checkout.is_symlink() or checkout.read_bytes() != content:
                        raise RuntimeError(f"source archive does not match checkout: {relative}")
                elif relative == Path("rro"):
                    if digest != expected.get("rro"):
                        raise RuntimeError("archived executable differs from dist/rro")
        if not installer_seen:
            raise RuntimeError(f"archive is missing installer: {installer}")
        report["archives"][name] = dict(files=count, forbidden_sources=[], sha256=expected[name])
    if len(report["archives"]) != 2:
        raise RuntimeError("expected binary and rebuild-source archives")
    write_report(ROOT / "reports/archive-audit.json", report)
    print("Archive audit passed: checksums, source snapshot, safe paths, no original Python or legacy source trees.")


if __name__ == "__main__":
    main()
