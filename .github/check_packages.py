#!/usr/bin/env python3
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT

"""Check the actual Cargo archives for every publishable workspace crate."""
import argparse
import json
from pathlib import Path
import subprocess
import tarfile
import tomllib


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="Cargo's target/package directory")
    args = parser.parse_args()
    repository = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version=1", "--locked"],
        cwd=repository,
        text=True,
    ))
    members = set(metadata["workspace_members"])
    packages = [p for p in metadata["packages"] if p["id"] in members and p["publish"] != []]
    internal = {p["name"]: p["version"] for p in packages}
    failures = []
    for package in sorted(packages, key=lambda p: p["name"]):
        prefix = f'{package["name"]}-{package["version"]}'
        archive = args.directory / f"{prefix}.crate"
        if not archive.is_file():
            failures.append(f"{archive}: missing package")
            continue
        with tarfile.open(archive, "r:gz") as tar:
            entries = {entry.name: entry for entry in tar.getmembers()}
            required = ["Cargo.toml", "README.md", "CHANGELOG.md", "LICENSE-APACHE", "LICENSE-MIT"]
            if "LicenseRef-TOST-1.0" in (package["license"] or ""):
                required += ["LICENSE-TOST-1.0", "NOTICE"]
            for name in required:
                entry = entries.get(f"{prefix}/{name}")
                if entry is None or not entry.isfile() or not entry.size:
                    failures.append(f"{prefix}: missing or empty {name}")
            manifest_entry = entries.get(f"{prefix}/Cargo.toml")
            if manifest_entry is None or not manifest_entry.isfile():
                continue
            manifest_file = tar.extractfile(manifest_entry)
            if manifest_file is None:
                continue
            manifest = tomllib.loads(manifest_file.read().decode())
            actual = manifest["package"]
            for key in ["name", "version", "edition", "rust-version", "license"]:
                expected = package["rust_version"] if key == "rust-version" else package[key]
                if actual.get(key) != expected:
                    failures.append(f"{prefix}: packaged {key} differs from workspace metadata")
            sections = [manifest, *manifest.get("target", {}).values()]
            for section in sections:
                for kind in ["dependencies", "build-dependencies", "dev-dependencies"]:
                    for alias, dependency in section.get(kind, {}).items():
                        if isinstance(dependency, str):
                            dependency = {"version": dependency}
                        name = dependency.get("package", alias)
                        if "path" in dependency or "workspace" in dependency:
                            failures.append(f"{prefix}: unresolved workspace dependency {alias}")
                        if name in internal and dependency.get("version") != internal[name]:
                            failures.append(f"{prefix}: {alias} does not require {internal[name]}")
            size = sum(entry.size for entry in entries.values())
            print(f"{prefix}: {len(entries)} files, {size} bytes")
    if failures:
        raise SystemExit("\n".join(failures))
    print(f"Verified {len(packages)} release archives and their internal dependency versions.")


if __name__ == "__main__":
    main()
