# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's skinning bake writes.

Usage: skel_bake_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Opens `fixtures/skel_bake/scene.usda` and bakes it with
`UsdSkel.BakeSkinning`, once over every SkelRoot of the stage
(`UsdPrimRange`, every time) and once for `/Character/Root` over the time
codes 1 to 3, each into the stage's root layer. For each, records every
prim spec the root layer then holds: specifier, type, `apiSchemas`,
`kind`, and each property (an attribute's type, variability,
`interpolation`, `elementSize`, default and time samples; a
relationship's targets).

The scene has a skinned and blend-shaped mesh (with an inbetween and
normal offsets) under an animated skeleton transform, a dual-quaternion
mesh with face-varying normals and its own joint order, rigid prims (a
cube and a mesh), skinned points with widths, a blend-shape-only mesh,
a component model with an `extentsHint`, and a second SkelRoot whose
skeleton only has a rest pose.

With `--check`, compares a fresh run with `fixtures/skel_bake/oracle.json`:
names, types and structure exactly, numbers within 1e-5 (relative above
one).

`tests/conformance/skel_bake.rs` bakes the same scene and compares every spec.
"""
import argparse
import json
import math
import pathlib

from pxr import Gf, Sdf, Usd, UsdSkel

ROOT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "skel_bake"
TOLERANCE = 1e-5


def value(v):
    if v is None:
        return None
    if isinstance(v, (bool, int, str)):
        return v
    if isinstance(v, float):
        return v
    if isinstance(v, (Gf.Quatf, Gf.Quatd, Gf.Quath)):
        return [float(x) for x in v.GetImaginary()] + [float(v.GetReal())]
    if hasattr(v, "__len__"):
        return [value(x) for x in v]
    return float(v)


def items(list_op):
    return [str(x) for x in list_op.GetAddedOrExplicitItems()]


def prim(layer, spec):
    properties = {}
    for prop in spec.properties:
        if isinstance(prop, Sdf.AttributeSpec):
            properties[prop.name] = {
                "type": str(prop.typeName),
                "uniform": prop.variability == Sdf.VariabilityUniform,
                "interpolation": prop.GetInfo("interpolation") if prop.HasInfo("interpolation") else None,
                "elementSize": prop.GetInfo("elementSize") if prop.HasInfo("elementSize") else None,
                "default": value(prop.default) if prop.HasDefaultValue() else None,
                "timeSamples": {
                    repr(float(t)): value(layer.QueryTimeSample(prop.path, t))
                    for t in layer.ListTimeSamplesForPath(prop.path)
                },
            }
        else:
            properties[prop.name] = {"targets": items(prop.targetPathList)}
    return {
        "specifier": str(spec.specifier).split(".")[-1].lower().replace("specifier", ""),
        "type": spec.typeName or None,
        "apiSchemas": items(spec.GetInfo("apiSchemas")) if spec.HasInfo("apiSchemas") else [],
        "kind": spec.kind or None,
        "properties": properties,
    }


def dump(layer):
    out = {}

    def visit(path):
        spec = layer.GetObjectAtPath(path)
        if isinstance(spec, Sdf.PrimSpec) and path != Sdf.Path.absoluteRootPath:
            out[str(path)] = prim(layer, spec)

    layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    return out


def bake(case):
    stage = Usd.Stage.Open(str(ROOT / "scene.usda"))
    if case == "stage":
        ok = UsdSkel.BakeSkinning(stage.Traverse())
    else:
        root = UsdSkel.Root(stage.GetPrimAtPath("/Character/Root"))
        ok = UsdSkel.BakeSkinning(root, Gf.Interval(1, 3))
    assert ok, case
    return dump(stage.GetRootLayer())


def agree(a, b):
    if isinstance(a, dict):
        return isinstance(b, dict) and a.keys() == b.keys() and all(agree(a[k], b[k]) for k in a)
    if isinstance(a, list):
        return isinstance(b, list) and len(a) == len(b) and all(agree(x, y) for x, y in zip(a, b))
    if isinstance(a, float) or isinstance(b, float):
        return math.isclose(a, b, rel_tol=TOLERANCE, abs_tol=TOLERANCE)
    return a == b


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "stage": bake("stage"),
        "root_interval": bake("root_interval"),
    }
    output = ROOT / "oracle.json"
    if args.check:
        assert agree(json.loads(output.read_text()), result), "oracle.json is out of date"
    else:
        output.write_text(json.dumps(result, indent=1, sort_keys=True) + "\n")


main()
