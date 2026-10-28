# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the text OpenUSD's USDA writer writes for a layer whose
namespace is far deeper than the USDA reader of layerstack reads.

Usage: usda_namespace_depth_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/usda_namespace_depth` by
default, `oracle.json`, which `tests/conformance/usda_namespace_depth.rs`
replays: it builds the same layer through layerstack's layer API and
saves it with `save_usda`.

The layer is authored through `Sdf`, not read from text: a chain of
`DEPTH` prims `def "A"`, the last holding `int x = 1` and `VARIANT_DEPTH`
variant sets nested in each other's only variant (`v0` holds variant `x`,
whose prim spec holds `v1`, ...), the innermost variant holding a chain of
`CHILD_DEPTH` prims `def "C"`. OpenUSD writes any depth (the text writer
recurses per prim, `Sdf_WritePrim`, `pxr/usd/sdf/fileIO_Common.h`) and
reads its own text back (`reads_back`); the text is recorded by its
length, line count and 64-bit FNV-1a hash, with its first and last lines.

`--check` rewrites `oracle.json` into a temporary directory and fails
unless it matches the checked-in fixture.
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
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "usda_namespace_depth"))

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
    layer = Sdf.Layer.CreateAnonymous("deep.usda")
    owner = layer.pseudoRoot
    for _ in range(DEPTH):
        owner = Sdf.PrimSpec(owner, "A", Sdf.SpecifierDef)
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
    data = text.encode("utf-8")
    lines = text.split("\n")
    reread = Sdf.Layer.CreateAnonymous("reread.usda")
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "depth": DEPTH,
        "variant_depth": VARIANT_DEPTH,
        "child_depth": CHILD_DEPTH,
        "bytes": len(data),
        "lines": len(lines),
        "fnv1a64": fnv1a64(data),
        "head": [line.strip() for line in lines[:6]],
        "tail": [line.strip() for line in lines[-6:]],
        "reads_back": bool(reread.ImportFromString(text)),
    }
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


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
        fresh = os.path.join(tmp, "oracle.json")
        committed = os.path.join(FIXTURES, "oracle.json")
        if not filecmp.cmp(fresh, committed, shallow=False):
            with open(committed) as a, open(fresh) as b:
                diff = difflib.unified_diff(a.readlines(), b.readlines(),
                                            "committed", "regenerated", n=2)
                sys.stdout.writelines(list(diff)[:200])
            raise SystemExit(f"oracle.json differs from {FIXTURES}")


if __name__ == "__main__":
    main()
