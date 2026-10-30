# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD lists and describes every prim's primvars.

Usage: primvar_listing_oracle.py [--check]

Needs OpenUSD through Python `pxr` (`pip install usd-core==26.11`).

For every prim of `fixtures/primvar_behavior.usda` and
`fixtures/primvar_queries.usda`, records `UsdGeomPrimvarsAPI::GetPrimvars`
(schema-declared primvars included) in name order, each primvar as
`[name, typeName, interpolation, elementSize, indexed, idTarget,
indices]`, `indices` being `GetIndices` at the default time (`null` when it
has none), and `FindPrimvarsWithInheritance` as `[name, prim]` pairs.
This is the listing `@forest-rs/layerstack`'s `stage.primvars(path)`
returns (`packages/layerstack/test/geometry.test.ts`).

Writes `fixtures/primvar_listing.json`; with `--check` it writes nothing and
fails unless the committed file is what OpenUSD records here.
"""
import json
import os
import sys

from pxr import Usd, UsdGeom

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures"))
SCENES = ["primvar_behavior.usda", "primvar_queries.usda"]


def describe(primvar):
    indices = primvar.GetIndices() if primvar.IsIndexed() else None
    return [
        str(primvar.GetPrimvarName()),
        str(primvar.GetTypeName()),
        str(primvar.GetInterpolation()),
        primvar.GetElementSize(),
        primvar.IsIndexed(),
        primvar.IsIdTarget(),
        None if indices is None else list(indices),
    ]


def record():
    scenes = {}
    for scene in SCENES:
        stage = Usd.Stage.Open(os.path.join(FIXTURES, scene))
        prims = {}
        for prim in stage.Traverse():
            api = UsdGeom.PrimvarsAPI(prim)
            prims[str(prim.GetPath())] = {
                "primvars": sorted(
                    (describe(primvar) for primvar in api.GetPrimvars()), key=lambda entry: entry[0]
                ),
                "inherited": sorted(
                    [str(primvar.GetPrimvarName()), str(primvar.GetAttr().GetPrimPath())]
                    for primvar in api.FindPrimvarsWithInheritance()
                ),
            }
        scenes[scene] = prims
    return {"openusd_version": ".".join(map(str, Usd.GetVersion()[1:])), "scenes": scenes}


def main():
    text = json.dumps(record(), indent=1) + "\n"
    dest = os.path.join(FIXTURES, "primvar_listing.json")
    if "--check" in sys.argv:
        with open(dest) as f:
            assert f.read() == text, "the primvar listing oracle differs"
        print("primvar listings agree with the committed oracle")
    else:
        with open(dest, "w") as f:
            f.write(text)
        print(f"wrote {dest}")


main()
