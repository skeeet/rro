#!/usr/bin/env python3
"""Keep local identities and private absolute paths out of verification artifacts."""
import getpass
import json
from pathlib import Path
import platform
import re
import socket
import subprocess

ROOT = Path(__file__).resolve().parents[1]
PRIVATE_PATH = re.compile(r"/(?:Users|Volumes|home)/[^\s\"'<>\[\](),;]+")
CONCRETE_PRIVATE_PATH = re.compile(rb"/(?:Users|Volumes|home)/[A-Za-z0-9_.-]+/")
CAMERA_IDENTIFIERS = {"cubic", "gaussian", "ideal-lowpass"}
IDENTITY_FIELDS = {"username", "user_name", "user_email", "hostname", "host_name",
                   "computer_name", "machine_name", "author", "committer"}
SEMANTIC_FIELDS = {"camera", "options", "parity_case_names", "image_case_names",
                   "independent_checks"}


def private_tokens():
    values = {Path.home().name, getpass.getuser()}
    for key in ("user.name", "user.email"):
        try:
            result = subprocess.run(["git", "config", "--get", key], cwd=ROOT,
                                    capture_output=True, text=True)
        except OSError:
            continue
        if result.returncode == 0:
            values.add(result.stdout.strip())
    return sorted((value for value in values if len(value) > 3 and value != "root"
                   and value.lower() not in CAMERA_IDENTIFIERS),
                  key=len, reverse=True)


TOKENS = private_tokens()


def sanitize(value):
    """Preserve measurements and hashes while anonymizing report metadata recursively."""
    if isinstance(value, dict):
        return {key: ("<external-comparison-source>" if key == "external_reference_directory"
                      else "<anonymous>" if key in IDENTITY_FIELDS
                      else item if key in SEMANTIC_FIELDS
                      else platform.machine() if key == "hardware" else sanitize(item))
                for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [sanitize(item) for item in value]
    if isinstance(value, str):
        workspace = str(ROOT)
        if value.startswith(workspace + "/"):
            value = value[len(workspace) + 1:]
        value = value.replace(workspace, "<workspace>")
        value = PRIVATE_PATH.sub("<external-path>", value)
        for token in TOKENS:
            value = value.replace(token, "<redacted>")
        return value
    return value


def write_report(path, value):
    Path(path).write_text(json.dumps(sanitize(value), indent=2, allow_nan=False) + "\n")


def privacy_issues(content):
    """Return categories only, without reproducing the private values in diagnostics."""
    issues = []
    if CONCRETE_PRIVATE_PATH.search(content):
        issues.append("private absolute filesystem path")
    lower = content.lower()
    if any(token.lower().encode() in lower for token in TOKENS):
        issues.append("local identity")
    hostname = socket.gethostname().lower()
    # A machine can be named after a camera. Its occurrence as that mathematical
    # identifier is not evidence that a report discloses the machine's identity.
    if len(hostname) > 3 and hostname not in CAMERA_IDENTIFIERS and hostname.encode() in lower:
        issues.append("local hostname")
    return issues
