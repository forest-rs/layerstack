# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT

"""Native OpenUSD field and media audit for scene round trips.

Requires Python with pxr. Compares authored values, declarations, metadata,
list operations and child/property order; floating arrays are compared by
buffer bytes. This does not evaluate rendered appearance or external layers.
Asset spelling changes are allowed only for explicitly marked outputs.
A stage_loading `layers.tsv` manifest audits each imported source layer
against its separate USDC export, preserving composition arcs as authored.
"""

import argparse
import hashlib
import json
from pathlib import Path
import struct
import zipfile

from pxr import Sdf, Usd


def inventory(layer):
    paths = []
    layer.Traverse(Sdf.Path.absoluteRootPath, paths.append)
    return set(paths)


def equal(left, right, localized):
    if type(left) is not type(right):
        return False
    if isinstance(left, Sdf.AssetPath):
        spelling = left.path.replace("\\", "/") if localized else left.path
        return spelling == right.path
    if isinstance(left, float):
        return struct.pack("=d", left) == struct.pack("=d", right)
    if isinstance(left, Sdf.LayerOffset):
        return equal(left.offset, right.offset, localized) and equal(
            left.scale, right.scale, localized
        )
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(
            equal(left[key], right[key], localized) for key in left
        )
    try:
        return memoryview(left).cast("B") == memoryview(right).cast("B")
    except (TypeError, ValueError):
        pass
    if isinstance(left, (list, tuple)):
        return len(left) == len(right) and all(
            equal(a, b, localized) for a, b in zip(left, right)
        )
    return left == right


def compare(source, file, localized):
    layer = Sdf.Layer.FindOrOpen(str(file))
    assert layer, f"native OpenUSD opens output {file}"
    left = inventory(source)
    right = inventory(layer)
    differences = []
    fields = 0
    target_containers = 0
    for path in sorted(left | right):
        if path not in left or path not in right:
            missing = "source" if path not in left else "output"
            differences.append((str(path), "missing spec", missing))
            continue
        if path == Sdf.Path.absoluteRootPath:
            a, b = source.pseudoRoot, layer.pseudoRoot
        else:
            a, b = source.GetObjectAtPath(path), layer.GetObjectAtPath(path)
        if a is None or b is None:
            assert a is None and b is None and path.IsTargetPath(), str(path)
            # Python has no object wrapper for these empty containers. Their
            # exact paths are checked above and their parent target/connection
            # list operations are checked with the other authored fields.
            target_containers += 1
            continue
        a_keys, b_keys = set(a.ListInfoKeys()), set(b.ListInfoKeys())
        for key in sorted(a_keys | b_keys):
            fields += 1
            if key not in a_keys or key not in b_keys:
                differences.append((str(path), key, "authored field presence"))
                continue
            if key == "subLayerOffsets" and path == Sdf.Path.absoluteRootPath:
                # GetInfo cannot wrap vector<SdfLayerOffset> in native Python;
                # the layer's dedicated proxy exposes the same field values.
                a_value, b_value = list(source.subLayerOffsets), list(layer.subLayerOffsets)
            else:
                a_value, b_value = a.GetInfo(key), b.GetInfo(key)
            if not equal(a_value, b_value, localized):
                differences.append(
                    (str(path), key, "value/type mismatch",
                     type(a_value).__name__, type(b_value).__name__)
                )
        if isinstance(a, Sdf.PrimSpec):
            if list(a.nameChildren.keys()) != list(b.nameChildren.keys()):
                differences.append((str(path), "primChildren", "order mismatch"))
            if list(a.properties.keys()) != list(b.properties.keys()):
                differences.append((str(path), "properties", "order mismatch"))
    result = {
        "file": str(file),
        "source": source.identifier,
        "bytes": file.stat().st_size,
        "specs": len(right),
        "fields": fields,
        "target_containers": target_containers,
        "difference_count": len(differences),
        "differences": differences[:60],
    }
    print(json.dumps(result), flush=True)
    return result


def audit_media(file, media_root):
    stage = Usd.Stage.Open(str(file))
    assert stage, f"native OpenUSD opens package stage {file}"
    assets = []
    for prim in stage.Traverse():
        for attr in prim.GetAttributes():
            if attr.GetTypeName() == Sdf.ValueTypeNames.Asset:
                value = attr.Get()
                if value:
                    assets.append((str(attr.GetPath()), value.path, value.resolvedPath))
    with zipfile.ZipFile(file) as archive:
        # This probe is for a self-contained layer and media, not a
        # recursively rewritten hierarchy of external composition layers.
        media = archive.infolist()[1:]
        mismatches = []
        for entry in media:
            left, right = hashlib.sha256(), hashlib.sha256()
            with archive.open(entry) as a, (media_root / entry.filename).open("rb") as b:
                for block in iter(lambda: a.read(1024 * 1024), b""):
                    left.update(block)
                for block in iter(lambda: b.read(1024 * 1024), b""):
                    right.update(block)
            if left.digest() != right.digest():
                mismatches.append(entry.filename)
        result = {
            "file": str(file),
            "asset_attributes": len(assets),
            "unresolved_assets": [asset for asset in assets if not asset[2]],
            "media_entries": len(media),
            "media_bytes": sum(entry.file_size for entry in media),
            "media_hash_mismatches": mismatches,
        }
    print(json.dumps(result), flush=True)
    return result


def main():
    parser = argparse.ArgumentParser(
        description="Compare authored root-layer fields with native OpenUSD."
    )
    parser.add_argument("source", nargs="?", type=Path)
    parser.add_argument("outputs", nargs="*", type=Path)
    parser.add_argument(
        "--manifest", type=Path,
        help="Audit a stage_loading layers.tsv manifest instead of positional paths.",
    )
    parser.add_argument(
        "--localized", action="append", type=Path, default=[],
        help="Allow Windows-to-POSIX spelling changes only for these outputs' asset paths.",
    )
    parser.add_argument(
        "--media-root", type=Path,
        help="Verify package media against this tree with the same relative filenames.",
    )
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if args.manifest:
        if args.source or args.outputs or args.localized or args.media_root:
            parser.error("manifest mode accepts only --manifest and --report")
        pairs = []
        for line in args.manifest.read_text().splitlines():
            layer_id, source_file = line.split("\t", 1)
            if not layer_id.isascii() or not layer_id.isdigit():
                parser.error("manifest layer IDs must be decimal integers")
            pairs.append((Path(source_file), args.manifest.parent / f"{layer_id}.usdc"))
        if not pairs:
            parser.error("manifest contains no layers")
    else:
        if not args.source or not args.outputs:
            parser.error("provide a source and outputs, or --manifest")
        pairs = [(args.source, path) for path in args.outputs]
    localized = {path.resolve() for path in args.localized}
    results = []
    for source_file, path in pairs:
        source = Sdf.Layer.FindOrOpen(str(source_file))
        assert source, f"native OpenUSD opens source {source_file}"
        results.append(compare(source, path, path.resolve() in localized))
    packages = []
    if args.media_root:
        packages = [
            audit_media(path, args.media_root)
            for path in args.outputs if path.suffix.lower() == ".usdz"
        ]
    report = {"layers": results, "packages": packages}
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    if any(row["difference_count"] for row in results) or any(
        row["media_hash_mismatches"] or row["unresolved_assets"] for row in packages
    ):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
