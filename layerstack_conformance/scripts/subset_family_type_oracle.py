# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the family type OpenUSD's subset creation authors, call by call.

Usage: subset_family_type_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

On a new stage with the mesh `/M` (two triangles), it calls
`UsdGeomSubset::CreateGeomSubset` and `CreateUniqueGeomSubset` with and
without a family name and a family type, one at a time: a type with no
family, a family with no type, a new family, a second subset changing the
family's type, and a unique subset of another family. After each it records
the specs of the root layer's attributes on `/M` and its subsets (type,
`custom`, `uniform`, default) and `UsdGeomSubset::GetFamilyType` for each
family.

`tests/conformance/subset_family_type.rs` replays the calls through
`GeomSubsetEdit::create` and `create_unique` and compares everything.
"""
import argparse
import json
import pathlib

from pxr import Sdf, Usd, UsdGeom

OUT = (pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "subset_family_type"
       / "oracle.json")
FAMILIES = ["", "parts", "looks"]


def value(v):
    if isinstance(v, (bool, int, float, str)) or v is None:
        return v
    return [value(x) for x in v]


def specs(layer):
    found = {}

    def visit(spec):
        for attr in spec.attributes:
            found[str(attr.path)] = {
                "type": str(attr.typeName),
                "custom": attr.custom,
                "uniform": attr.variability == Sdf.VariabilityUniform,
                "default": value(attr.default) if attr.HasDefaultValue() else None,
            }
        for child in spec.nameChildren:
            visit(child)

    for root in layer.rootPrims:
        visit(root)
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stage = Usd.Stage.CreateInMemory()
    layer = stage.GetRootLayer()
    mesh = UsdGeom.Mesh.Define(stage, "/M")
    mesh.CreatePointsAttr([(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0)])
    mesh.CreateFaceVertexCountsAttr([3, 3])
    mesh.CreateFaceVertexIndicesAttr([0, 1, 2, 1, 3, 2])
    geom = UsdGeom.Imageable(mesh.GetPrim())
    T = UsdGeom.Tokens
    S = UsdGeom.Subset
    calls = [
        ("create a: type, no family",
         lambda: S.CreateGeomSubset(geom, "a", T.face, [0], "", T.partition)),
        ("create b: family parts, no type",
         lambda: S.CreateGeomSubset(geom, "b", T.face, [0], "parts")),
        ("create c: parts partition",
         lambda: S.CreateGeomSubset(geom, "c", T.face, [0], "parts", T.partition)),
        ("create d: parts nonOverlapping",
         lambda: S.CreateGeomSubset(geom, "d", T.face, [1], "parts", T.nonOverlapping)),
        ("create c again: parts unrestricted",
         lambda: S.CreateGeomSubset(geom, "c", T.face, [1], "parts", T.unrestricted)),
        ("create d again: parts, no type",
         lambda: S.CreateGeomSubset(geom, "d", T.face, [0], "parts")),
        ("unique c: looks nonOverlapping",
         lambda: S.CreateUniqueGeomSubset(geom, "c", T.face, [0], "looks", T.nonOverlapping)),
        ("unique c: no family, unrestricted",
         lambda: S.CreateUniqueGeomSubset(geom, "c", T.face, [1], "", T.unrestricted)),
    ]
    log = []
    for label, call in calls:
        subset = call()
        log.append({
            "call": label,
            "subset": str(subset.GetPath()),
            "specs": specs(layer),
            "family_types": {f: str(S.GetFamilyType(geom, f)) for f in FAMILIES},
        })
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "calls": log,
    }
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.check:
        assert OUT.read_text() == text, "oracle.json is out of date"
    else:
        OUT.write_text(text)


main()
