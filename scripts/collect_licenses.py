#!/usr/bin/env python3
"""Collect the actual host-target Cargo dependency licenses for release packaging.
Development only; requires Python 3 and a populated Cargo cache.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess

ROOT=Path(__file__).resolve().parents[1]

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination",type=Path)
    args=parser.parse_args()
    host=next(line.split(": ",1)[1] for line in subprocess.check_output(["rustc","-vV"],text=True).splitlines() if line.startswith("host: "))
    metadata=json.loads(subprocess.check_output(["cargo","metadata","--locked","--offline","--format-version","1","--filter-platform",host],cwd=ROOT,text=True))
    index=[]
    for package in sorted(metadata["packages"],key=lambda item:(item["name"],item["version"])):
        name,version=package["name"],package["version"]
        if name=="rro": continue
        source=Path(package["manifest_path"]).parent
        dest=args.destination/"rust"/f"{name}-{version}"
        dest.mkdir(parents=True,exist_ok=True)
        paths=[p for p in source.iterdir() if p.name.lower().startswith(("license","copying","copyright","notice"))]
        specified=package.get("license_file")
        if specified:
            specified_path=Path(specified)
            if not specified_path.is_absolute(): specified_path=source/specified_path
            if specified_path not in paths: paths.append(specified_path)
        # Workspace crates can share the repository's license file. Search the
        # immediate workspace ancestors only when Cargo points to a Git source.
        if not paths and (package.get("source") or "").startswith("git+"):
            for ancestor in source.parents:
                if not (ancestor/"Cargo.toml").exists(): break
                paths=[p for p in ancestor.iterdir() if p.name.lower().startswith(("license","copying","copyright","notice"))]
                if paths: break
        if not paths: raise RuntimeError(f"dependency has no bundled full license text: {name} {version}")
        for p in paths:
            if p.is_dir(): shutil.copytree(p,dest/p.name,dirs_exist_ok=True)
            else: shutil.copy2(p,dest/p.name)
        index.append(dict(name=name,version=version,source=package.get("source"),license=package.get("license"),files=[p.name for p in paths]))
    sysroot=Path(subprocess.check_output(["rustc","--print","sysroot"],text=True).strip())
    rust_docs=sysroot/"share/doc/rust"
    for p in rust_docs.glob("*"):
        if p.name.lower().startswith(("license","copyright")):
            dest=args.destination/"rust-toolchain"
            dest.mkdir(parents=True,exist_ok=True)
            if p.is_dir(): shutil.copytree(p,dest/p.name,dirs_exist_ok=True)
            else: shutil.copy2(p,dest/p.name)
    (args.destination/"RUST-DEPENDENCIES.json").write_text(json.dumps(dict(target=host,packages=index),indent=2)+"\n")
    print(f"Collected complete license files for {len(index)} Cargo packages targeting {host}.")

if __name__=="__main__": main()
