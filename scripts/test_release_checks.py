#!/usr/bin/env python3
"""Regression tests for release evidence gates; no oracle or network required."""
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tarfile
import unittest
from unittest import mock

import check_release
import check_repository
import report_privacy
import create_archive
import audit_archives


class InstallerTests(unittest.TestCase):
    def setUp(self):
        work = Path(__file__).resolve().parents[1] / "reports/work"
        work.mkdir(parents=True, exist_ok=True)
        directory = tempfile.TemporaryDirectory(dir=work, prefix="installer-test-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.home = self.root / "home space é"
        self.home.mkdir()
        self.source = self.root / "binary space é"
        self.source.write_bytes(b"synthetic executable payload\n")
        self.script = Path(__file__).resolve().with_name("install.sh")
        self.destination = self.home / ".local/bin/rro"

    def run_install(self, *arguments, script=None, success=True):
        result = subprocess.run(
            ["/bin/sh", str(script or self.script), *map(str, arguments)],
            cwd=self.root, env={**os.environ, "HOME": str(self.home)},
            capture_output=True, text=True,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def test_default_install_preserves_source_and_sets_executable_mode(self):
        self.run_install(self.source)
        self.assertEqual(self.destination.read_bytes(), self.source.read_bytes())
        self.assertTrue(self.destination.stat().st_mode & 0o111)
        self.assertEqual(list(self.destination.parent.iterdir()), [self.destination])
        self.assertFalse((self.home / ".zprofile").exists())

    def test_custom_prefix_and_forced_update(self):
        prefix = self.root / "custom prefix é"
        self.run_install("--prefix", prefix, self.source)
        target = prefix / "bin/rro"
        self.source.write_bytes(b"updated executable\n")
        self.run_install("--prefix", prefix, self.source, success=False)
        self.assertEqual(target.read_bytes(), b"synthetic executable payload\n")
        self.run_install("--force", "--prefix", prefix, self.source)
        self.assertEqual(target.read_bytes(), self.source.read_bytes())

    def test_existing_symlink_is_not_followed(self):
        self.destination.parent.mkdir(parents=True)
        outside = self.root / "other file"
        outside.write_bytes(b"preserve this\n")
        self.destination.symlink_to(outside)
        self.run_install(self.source, success=False)
        self.assertTrue(self.destination.is_symlink())
        self.run_install("--force", self.source)
        self.assertFalse(self.destination.is_symlink())
        self.assertEqual(outside.read_bytes(), b"preserve this\n")
        self.assertEqual(self.destination.read_bytes(), self.source.read_bytes())

    def test_dangling_symlink_and_destination_directory_are_rejected(self):
        self.destination.parent.mkdir(parents=True)
        self.destination.symlink_to(self.root / "missing")
        self.run_install(self.source, success=False)
        self.assertTrue(self.destination.is_symlink())
        self.destination.unlink()
        self.destination.mkdir()
        self.run_install("--force", self.source, success=False)
        self.assertEqual(list(self.destination.iterdir()), [])

    def test_invalid_arguments_and_sources_do_not_install(self):
        empty = self.root / "empty"
        empty.touch()
        for arguments in (("--unknown",), ("--prefix",), ("--prefix", ""),
                          (self.source, self.source), (self.root / "missing",),
                          (empty,), (self.root,)):
            with self.subTest(arguments=arguments):
                self.run_install(*arguments, success=False)
                self.assertFalse(self.destination.exists())

    def test_help_does_not_install(self):
        result = self.run_install("--help")
        self.assertIn("--prefix", result.stdout)
        self.assertFalse(self.destination.exists())

    def test_source_destination_aliases_are_rejected_without_damage(self):
        self.run_install(self.source)
        self.run_install("--force", self.destination, success=False)
        alias = self.root / "hard link"
        os.link(self.destination, alias)
        self.run_install("--force", alias, success=False)
        self.assertEqual(self.destination.read_bytes(), self.source.read_bytes())

    def test_failed_forced_update_preserves_existing_installation(self):
        self.run_install(self.source)
        self.run_install("--force", self.root / "missing", success=False)
        self.assertEqual(self.destination.read_bytes(), self.source.read_bytes())

    def test_archive_layout_works_from_another_working_directory(self):
        archive = self.root / "extracted archive"
        archive.mkdir()
        script = archive / "install.sh"
        script.write_bytes(self.script.read_bytes())
        (archive / "rro").write_bytes(self.source.read_bytes())
        self.run_install(script=script)
        self.assertEqual(self.destination.read_bytes(), self.source.read_bytes())

    def test_checkout_source_priority_and_missing_binary(self):
        checkout = self.root / "source checkout"
        scripts = checkout / "scripts"
        scripts.mkdir(parents=True)
        script = scripts / "install.sh"
        script.write_bytes(self.script.read_bytes())
        self.run_install(script=script, success=False)
        self.assertFalse(self.destination.exists())
        built = checkout / "target/release/rro"
        built.parent.mkdir(parents=True)
        built.write_bytes(b"built executable\n")
        self.run_install(script=script)
        self.assertEqual(self.destination.read_bytes(), built.read_bytes())
        packaged = checkout / "dist/rro"
        packaged.parent.mkdir()
        packaged.write_bytes(b"packaged executable\n")
        self.run_install("--force", script=script)
        self.assertEqual(self.destination.read_bytes(), packaged.read_bytes())


class ReleaseEvidenceTests(unittest.TestCase):
    def setUp(self):
        work = Path(__file__).resolve().parents[1] / "reports/work"
        work.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=work, prefix="release-gate-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "target/release").mkdir(parents=True)
        (self.root / "reports").mkdir()
        for name in ("rro", "rro-numeric"):
            (self.root / "target/release" / name).write_bytes(b"test-artifact")
        (self.root / "docs").mkdir(parents=True)
        source = Path(__file__).resolve().parents[1]
        for relative in ("docs/release-contract.json", "docs/reference-manifest.json"):
            (self.root / relative).write_bytes((source / relative).read_bytes())
        contract = json.loads((source / "docs/release-contract.json").read_text())
        reference = json.loads((source / "docs/reference-manifest.json").read_text())["library_sha256"]
        digest = hashlib.sha256(b"test-artifact").hexdigest()
        self.reports = {
            "parity": dict(count=202, passed=202, cases=[dict(name=name,passed=True,max_abs=0.0,rmse=0.0) for name in contract["parity_case_names"]],
                           environment=dict(helper_sha256=digest,reference_sha256=reference)),
            "image-parity": dict(count=len(contract["image_case_names"]), passed=len(contract["image_case_names"]), cases=[dict(name=name,passed=True,max_code_difference=0.0,psnr_db=None,
                                 **({"png_color_type":contract["png_color_types"][name]} if name in contract["png_color_types"] else {}),
                                 **({"output_orientation":None} if name.startswith("orientation-png-") else {})) for name in contract["image_case_names"]], reference_sha256=reference,
                                 binary_sha256=digest, binary_unchanged_during_checks=True),
            "core-fuzz": dict(cases=100, passed=True, tolerances=dict(max_abs_error=1e-9,rmse=1e-10), results=[dict(number=number,passed=True,max_abs_error=0.0,rmse=0.0) for number in range(100)],
                              environment=dict(helper_sha256=digest,reference_sha256=reference)),
            "independent-codec-check": dict(count=3, passed=3, binary_sha256=digest,
                binary_unchanged_during_checks=True,
                limited_range_HEIC_input_vs_ffmpeg=dict(pass_=True),
                kvazaar_HEIC_input_vs_ffmpeg=dict(pass_=True),
                our_HEIC_output_decode_vs_ffmpeg=dict(pass_=True)),
            "benchmark": dict(complete=True, cases=[dict(input_shape=row[:3],camera=row[3],scale=row[4], **{key:dict(samples_ms=[1.0]*5,median_ms=1.0) for key in ("numpy","rust_1thread","rust_default")}) for row in contract["benchmark_cases"]], targets={key:True for key in contract["performance_targets"]},
                              environment=dict(helper_sha256=digest,reference_sha256=reference),
                              benchmark_output_parity=dict(status="not_checked")),
            "io-benchmark": dict(complete=True, binary_unchanged_during_run=True,
                environment=dict(binary=dict(sha256=digest)),
                cases=[dict(format=name, status="ok") for name in ("png", "jpeg", "heic")]),
        }
        for name in ("limited_range_HEIC_input_vs_ffmpeg", "kvazaar_HEIC_input_vs_ffmpeg", "our_HEIC_output_decode_vs_ffmpeg"):
            self.reports["independent-codec-check"][name] = {"pass": True, "allowed_max_code_difference":2,"max_code_difference":0.0}

    def write(self, reports):
        for name, report in reports.items():
            (self.root / "reports" / f"{name}.json").write_text(json.dumps(report))

    def test_complete_evidence_passes(self):
        self.write(self.reports)
        check_release.check(self.root)

    def test_incomplete_evidence_is_rejected(self):
        mutations = (
            ("parity", lambda report: report.update(count=1, passed=1, cases=[dict(passed=True)])),
            ("image-parity", lambda report: report["cases"].pop()),
            ("parity", lambda report: report["cases"].__setitem__(0, report["cases"][1])),
            ("core-fuzz", lambda report: report["results"].__setitem__(0, report["results"][1])),
            ("parity", lambda report: report["environment"].update(helper_sha256="wrong")),
            ("parity", lambda report: report["environment"].update(reference_sha256={})),
            ("image-parity", lambda report: report.update(binary_unchanged_during_checks=False)),
            ("core-fuzz", lambda report: report.update(cases=0, results=[])),
            ("independent-codec-check", lambda report: report.pop("our_HEIC_output_decode_vs_ffmpeg")),
            ("independent-codec-check", lambda report: report.update(binary_unchanged_during_checks=False)),
            ("benchmark", lambda report: report.pop("benchmark_output_parity")),
            ("parity", lambda report: report["cases"][0].update(max_abs=1.0)),
            ("image-parity", lambda report: report["cases"][0].update(max_code_difference=2.0)),
            ("image-parity", lambda report: report["cases"][0].update(png_color_type=2)),
            ("image-parity", lambda report: next(row for row in report["cases"] if row["name"]=="alpha-rgb-png").update(png_color_type=6)),
            ("image-parity", lambda report: next(row for row in report["cases"] if row["name"]=="alpha-rgba-opaque-png").update(png_color_type=2)),
            ("image-parity", lambda report: next(row for row in report["cases"] if row["name"]=="orientation-png-6").update(output_orientation=6)),
            ("independent-codec-check", lambda report: report["our_HEIC_output_decode_vs_ffmpeg"].update(max_code_difference=3.0)),
            ("core-fuzz", lambda report: report["results"][0].update(rmse=float("nan"))),
            ("benchmark", lambda report: report["cases"][0]["rust_default"].update(median_ms=0.0)),
        )
        for name, mutate in mutations:
            with self.subTest(report=name, mutation=mutate):
                reports = copy.deepcopy(self.reports)
                mutate(reports[name])
                self.write(reports)
                with self.assertRaises(SystemExit):
                    check_release.check(self.root)

    def test_missing_randomized_evidence_is_rejected(self):
        self.write(self.reports)
        (self.root / "reports/core-fuzz.json").unlink()
        with self.assertRaises(FileNotFoundError):
            check_release.check(self.root)

    def test_repository_rejects_disguised_oracle_and_legacy_sources(self):
        source = self.root / "renamed.txt"
        source.write_bytes(b"synthetic-oracle-content")
        hashes = {hashlib.sha256(source.read_bytes()).hexdigest()}
        self.assertIn("pinned original Python implementation content",
                      check_repository.problems(source, Path("renamed.txt"), hashes))
        for name in ("native/file.rs", "scripts/old_backend.py", "module.pyc", "../escape",
                     "skills-lock.json", "provenance/manifest.json"):
            self.assertTrue(check_repository.problems(source, Path(name), set()), name)

    def test_repository_rejects_symbolic_links(self):
        link = self.root / "link"
        link.symlink_to("missing-outside-target")
        self.assertTrue(check_repository.problems(link, Path("link"), set()))

    def test_report_anonymization_preserves_measurements_and_source_identity(self):
        original = dict(path=str(report_privacy.ROOT / "reports/result.json"),
                        external_reference_directory="/" + "Volumes/example/cache",
                        stderr=f"build at {report_privacy.ROOT}/src/core.rs",
                        hardware="example-model", samples_ms=[0.25, 1.75],
                        max_abs=1e-13, sha256="a" * 64, attribution="Assaf Shocher")
        anonymous = report_privacy.sanitize(original)
        self.assertEqual(anonymous["path"], "reports/result.json")
        self.assertEqual(anonymous["external_reference_directory"], "<external-comparison-source>")
        self.assertEqual(anonymous["samples_ms"], original["samples_ms"])
        self.assertEqual(anonymous["max_abs"], original["max_abs"])
        self.assertEqual(anonymous["sha256"], original["sha256"])
        self.assertEqual(anonymous["attribution"], original["attribution"])
        self.assertEqual(report_privacy.privacy_issues(json.dumps(anonymous).encode()), [])

    def test_privacy_inventory_rejects_local_paths_and_identities(self):
        private = ("/" + "Users/example/private/file").encode()
        self.assertTrue(report_privacy.privacy_issues(private))
        if report_privacy.TOKENS:
            self.assertTrue(report_privacy.privacy_issues(report_privacy.TOKENS[0].encode()))

    def test_anonymization_preserves_camera_and_release_contract_identifiers(self):
        contract = json.loads((Path(__file__).resolve().parents[1] / "docs/release-contract.json").read_text())
        self.assertEqual(report_privacy.sanitize(contract), contract)
        for camera in ("cubic", "gaussian", "ideal-lowpass"):
            data = dict(hostname=camera, camera=camera, options=dict(camera=camera, scale_x=.5),
                        samples_ms=[1.0, 2.0], sha256="f" * 64)
            with mock.patch.object(report_privacy.socket, "gethostname", return_value=camera):
                anonymous = report_privacy.sanitize(data)
                self.assertEqual(anonymous["hostname"], "<anonymous>")
                for key in ("camera", "options", "samples_ms", "sha256"):
                    self.assertEqual(anonymous[key], data[key])
                self.assertEqual(report_privacy.privacy_issues(json.dumps(anonymous).encode()), [])

    def test_archive_owners_timestamps_and_extended_metadata_are_removed(self):
        source = self.root / "source"
        source.mkdir()
        (source / "payload.txt").write_bytes(b"synthetic payload")
        destination = self.root / "release.tar.gz"
        create_archive.create(source, destination)
        with tarfile.open(destination, "r:gz") as archive:
            for member in archive:
                self.assertEqual(audit_archives.header_issues(member), [], member.name)
            self.assertEqual(archive.extractfile("source/payload.txt").read(), b"synthetic payload")
        gzip_header = destination.read_bytes()[:10]
        self.assertEqual(gzip_header[3] & 8, 0, "gzip must not retain a local filename")
        self.assertEqual(gzip_header[4:8], bytes(4), "gzip timestamp must be normalized")

    def test_archive_audit_rejects_local_owner_and_extended_metadata(self):
        member = tarfile.TarInfo("source/payload.txt")
        member.uid, member.gid = 501, 20
        member.uname = "developer"
        member.pax_headers = {"SCHILY.xattr.user.synthetic": "example"}
        self.assertTrue(audit_archives.header_issues(member))
        create_archive.anonymous_header(member)
        self.assertEqual(audit_archives.header_issues(member), [])

    def test_archive_audit_requires_matching_installers_in_both_archives(self):
        scripts = self.root / "scripts"
        scripts.mkdir()
        installer = scripts / "install.sh"
        installer.write_bytes(b"#!/bin/sh\n# synthetic installer\n")
        dist = self.root / "dist"
        dist.mkdir()
        (dist / "rro").write_bytes(b"synthetic executable")
        binary_stage = self.root / "binary-stage"
        binary_stage.mkdir()
        (binary_stage / "rro").write_bytes((dist / "rro").read_bytes())
        source_stage = self.root / "source-stage"
        (source_stage / "scripts").mkdir(parents=True)
        (source_stage / "scripts/install.sh").write_bytes(installer.read_bytes())

        def archive_pair():
            create_archive.create(binary_stage, dist / "binary.tar.gz")
            create_archive.create(source_stage, dist / "binary-rebuild-source.tar.gz")
            names = ("rro", "binary.tar.gz", "binary-rebuild-source.tar.gz")
            (dist / "SHA256SUMS").write_text("".join(
                f"{hashlib.sha256((dist / name).read_bytes()).hexdigest()}  dist/{name}\n"
                for name in names
            ))

        with mock.patch.object(audit_archives, "ROOT", self.root):
            archive_pair()
            with self.assertRaisesRegex(RuntimeError, "missing installer"):
                audit_archives.main()
            (binary_stage / "install.sh").write_bytes(b"different installer")
            archive_pair()
            with self.assertRaisesRegex(RuntimeError, "installer differs"):
                audit_archives.main()
            (binary_stage / "install.sh").write_bytes(installer.read_bytes())
            archive_pair()
            audit_archives.main()
            for stage, relative in ((binary_stage, Path("skills-lock.json")),
                                    (source_stage, Path("skills-lock.json")),
                                    (source_stage, Path("provenance/manifest.json"))):
                excluded = stage / relative
                excluded.parent.mkdir(parents=True, exist_ok=True)
                excluded.write_bytes(b"excluded metadata")
                archive_pair()
                with self.assertRaisesRegex(RuntimeError, "local-only or removed archive path"):
                    audit_archives.main()
                excluded.unlink()
                if relative.parent != Path("."):
                    excluded.parent.rmdir()
            (source_stage / "scripts/install.sh").unlink()
            archive_pair()
            with self.assertRaisesRegex(RuntimeError, "missing installer"):
                audit_archives.main()


if __name__ == "__main__":
    unittest.main()
