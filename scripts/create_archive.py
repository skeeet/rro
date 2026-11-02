#!/usr/bin/env python3
"""Create a release tarball without local owner, timestamps, or extended metadata."""
import argparse
import gzip
from pathlib import Path
import tarfile
import tempfile


def anonymous_header(member):
    if not (member.isfile() or member.isdir()):
        raise ValueError("release archives allow only ordinary files and directories")
    member.uid = member.gid = 0
    member.uname = member.gname = ""
    member.mtime = 0
    member.mode = 0o755 if member.isdir() or member.mode & 0o111 else 0o644
    member.pax_headers = {}
    return member


def create(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    staging = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination.parent, prefix=".archive-", delete=False) as output:
            staging = Path(output.name)
            with gzip.GzipFile(filename="", fileobj=output, mode="wb", mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT,
                                  dereference=True) as archive:
                    archive.add(source, arcname=source.name, filter=anonymous_header)
        staging.replace(destination)
    finally:
        if staging is not None:
            staging.unlink(missing_ok=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    arguments = parser.parse_args()
    create(arguments.source, arguments.destination)
