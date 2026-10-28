# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the crate file OpenUSD's USDC writer writes for a layer whose
namespace is hundreds of levels deep, and what it reads back from it.

Usage: usdc_namespace_depth_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/usdc_namespace_depth` by
default, `deep.usdc` (the file OpenUSD writes) and `oracle.json`, which
`tests/conformance/usdc_namespace_depth.rs` replays: it builds the same
layer through layerstack's layer API, saves it with `save_usdc` and reads
both files back with `read_usdc`.

The layer is the one `usda_namespace_depth_oracle.py` builds through
`Sdf`: a chain of `DEPTH` prims `def "A"`, the last holding `int x = 1`
and `VARIANT_DEPTH` variant sets nested in each other's only variant
(`v0` holds variant `x`, whose prim spec holds `v1`, ...), the innermost
variant holding a chain of `CHILD_DEPTH` prims `def "C"`; here the first
prim also holds `rel deep`, targeting the last `A`, so the crate's path
table meets that path, and its `DEPTH - 1` ancestors, before their specs. OpenUSD writes
and reads any depth its thread's stack holds: `CrateFile::_AddPath` and
`_BuildCompressedPathDataRecursive` recurse once per namespace level,
and `_BuildDecompressedPathsImpl` loops down a path's children
(`pxr/usd/sdf/crateFile.cpp`), a frame or so per level that its main
thread's stack holds at this depth.

`save_usdc` stores each spec's fields as OpenUSD's text parser stores
them (`custom` and `variability` on every attribute, in the parser's
order), so the file recorded is the one OpenUSD exports for the layer
it reads from its own text file, not for the layer `Sdf` built. The oracle
records whether OpenUSD reads the file back as the layer it wrote
(`reads_back`, comparing the two layers' text), and that text by its
length and 64-bit FNV-1a hash.

`--check` rewrites both files into a temporary directory and fails unless
`oracle.json` matches the checked-in one and OpenUSD reads the same text
from both crate files. The crate's bytes depend on how OpenUSD was built
(the Linux source build and the macOS wheel of 26.11.1 write files of the
same length with different bytes), so the check compares what OpenUSD
reads from them; the Rust test compares `save_usdc` with the checked-in
file.
"""
import argparse
import difflib
import filecmp
import json
import os
import sys
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "usdc_namespace_depth"))

DEPTH = 600
VARIANT_DEPTH = 40
CHILD_DEPTH = 40


def fnv1a64(data):
    h = 0xcbf29ce484222325
    for byte in data:
        h ^= byte
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


def deep_layer():
    layer = Sdf.Layer.CreateAnonymous("deep.usdc")
    owner = layer.pseudoRoot
    for _ in range(DEPTH):
        owner = Sdf.PrimSpec(owner, "A", Sdf.SpecifierDef)
    deep = Sdf.RelationshipSpec(layer.GetPrimAtPath("/A"), "deep", custom=False)
    deep.targetPathList.explicitItems = [owner.path]
    attribute = Sdf.AttributeSpec(owner, "x", Sdf.ValueTypeNames.Int)
    attribute.default = 1
    for i in range(VARIANT_DEPTH):
        variant_set = Sdf.VariantSetSpec(owner, f"v{i}")
        owner = Sdf.VariantSpec(variant_set, "x").primSpec
    for _ in range(CHILD_DEPTH):
        owner = Sdf.PrimSpec(owner, "C", Sdf.SpecifierDef)
    return layer


def write(out):
    os.makedirs(out, exist_ok=True)
    _, minor, patch = Usd.GetVersion()
    text = deep_layer().ExportToString()
    with tempfile.TemporaryDirectory() as tmp:
        # Opened from a file, not `ImportFromString`: importing copies the
        # parsed specs into the layer without the fields that hold their
        # fallback values (`custom`, `variability`).
        source = os.path.join(tmp, "deep.usda")
        with open(source, "w") as f:
            f.write(text)
        layer = Sdf.Layer.OpenAsAnonymous(source)
    if layer is None:
        raise SystemExit("OpenUSD did not read its text back")
    path = os.path.join(out, "deep.usdc")
    if not layer.Export(path):
        raise SystemExit("OpenUSD did not export the layer")
    reread = Sdf.Layer.OpenAsAnonymous(path)
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "depth": DEPTH,
        "variant_depth": VARIANT_DEPTH,
        "child_depth": CHILD_DEPTH,
        "reads_back": reread is not None and reread.ExportToString() == text,
        "usda_bytes": len(text.encode("utf-8")),
        "usda_fnv1a64": fnv1a64(text.encode("utf-8")),
    }
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def read_text(path):
    """The text of the layer OpenUSD reads from the crate file at `path`."""
    return Sdf.Layer.OpenAsAnonymous(path).ExportToString()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        for name in ("oracle.json", "deep.usdc"):
            fresh = os.path.join(tmp, name)
            committed = os.path.join(FIXTURES, name)
            if filecmp.cmp(fresh, committed, shallow=False):
                continue
            if name == "oracle.json":
                with open(committed) as a, open(fresh) as b:
                    diff = difflib.unified_diff(a.readlines(), b.readlines(),
                                                "committed", "regenerated", n=2)
                    sys.stdout.writelines(list(diff)[:200])
            elif read_text(fresh) == read_text(committed):
                continue
            raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
