# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's Skeleton and SkelRoot extents and the bounds above them.

Usage: skel_extents_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Opens `fixtures/skel_extents/scene.usda` and, at default time and times 0
and 1, records `UsdGeomBoundable::ComputeExtentFromPlugins` for every
Skeleton and SkelRoot, and `UsdGeomBBoxCache::ComputeWorldBound` (default
purpose) for every prim. The scene has an animated skeleton skinning a
mesh, the same with an animated skeleton transform (with and without a
default value), a scaled and offset `geomBindTransform`, a SkelRoot with
no bindings, and a skeleton whose rest transforms do not match its
joints beside a cube.

`tests/conformance/skel_extents.rs` compares everything.
"""
import argparse
import json
import pathlib

from pxr import Tf, Usd, UsdGeom, UsdSkel

ROOT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "skel_extents"


def vec(v):
    return [float(x) for x in v]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stage = Usd.Stage.Open(str(ROOT / "scene.usda"))
    times = {"default": Usd.TimeCode.Default(), "0": Usd.TimeCode(0), "1": Usd.TimeCode(1)}
    extents = {}
    bounds = {}
    for label, time in times.items():
        extents[label] = {}
        bounds[label] = {}
        cache = UsdGeom.BBoxCache(time, [UsdGeom.Tokens.default_])
        for prim in stage.Traverse():
            path = str(prim.GetPath())
            if prim.IsA(UsdSkel.Skeleton) or prim.IsA(UsdSkel.Root):
                try:
                    extent = UsdGeom.Boundable.ComputeExtentFromPlugins(
                        UsdGeom.Boundable(prim), time)
                    extents[label][path] = [vec(extent[0]), vec(extent[1])] if extent else None
                except Tf.ErrorException:
                    # The plugin produced no extent: C++ posts a coding
                    # error and the bound treats the prim as empty.
                    extents[label][path] = "error"
        # Bounds from the leaves up: a query that posts the skeleton's
        # error caches the empty bound, so its ancestors read without it.
        for prim in reversed(list(stage.Traverse())):
            path = str(prim.GetPath())
            try:
                box = cache.ComputeWorldBound(prim).ComputeAlignedRange()
            except Tf.ErrorException:
                box = cache.ComputeWorldBound(prim).ComputeAlignedRange()
            bounds[label][path] = None if box.IsEmpty() else [vec(box.GetMin()), vec(box.GetMax())]
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "extents": extents,
        "bounds": bounds,
    }
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    output = ROOT / "oracle.json"
    if args.check:
        assert output.read_text() == text, "oracle.json is out of date"
    else:
        output.write_text(text)


main()
