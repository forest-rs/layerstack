# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD reads from the scene the schema views author.

Usage: schema_views_oracle.py [SCENE [OUT]]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`,
and an OpenUSD checkout of that release in `LAYERSTACK_OPENUSD_SOURCE` for
the schemas the wheel is built without (see `openusd_source.py`).

`tests/schema_views.rs` authors `fixtures/schema_views/scene.usda` through
every setter of `layerstack_schemas`'s views (see
`tests/generated/schema_views.rs`): a `/Fallback_<S>` prim, which authors
nothing but the schema, and an `/Authored_<S>` prim, where every setter
authors a value, for each schema. This script opens that layer in OpenUSD
and writes `fixtures/schema_views/oracle.json` by default: for every
property authored on an `/Authored_<S>` prim, on both prims of the pair,

- an attribute's `Get()` at the default time (`default`) and at time 2
  (`sample`), and its `allowedTokens` (`allowed`, when it has them);
- a relationship's `GetTargets()` (`targets`).

It also opens `mapped.usda` beside the scene, whose `/Instance` references
`/Asset` in `mapped_asset.usda` and whose collection targets the test
authored through that reference, and records the composed targets of
every relationship on `/Instance` (`mapped`). Likewise for
`local_variant.usda`, where the test authored a collection inside
`/Rock{look=a}` through a local variant edit target, it records every
relationship on `/Rock` (`local_variant`).

Values are JSON: vectors, quaternions (`[i, j, k, r]`) and arrays as lists,
matrices as lists of rows, assets as their authored path, path expressions
as text, and non-finite floats as the strings `inf`, `-inf` and `nan`.
"""
import json
import math
import os
import sys

import openusd_source  # Before `pxr`: loads the source-only schemas.
from pxr import Gf, Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "schema_views"))
SAMPLE_TIME = 2.0


def value(v):
    """`v` as JSON."""
    if v is None or isinstance(v, (bool, int, str)):
        return v
    if isinstance(v, float):
        if math.isnan(v):
            return "nan"
        if math.isinf(v):
            return "inf" if v > 0 else "-inf"
        return v
    if isinstance(v, Sdf.AssetPath):
        return v.path
    if isinstance(v, Sdf.PathExpression):
        return v.GetText()
    if isinstance(v, Sdf.TimeCode):
        return value(v.GetValue())
    if isinstance(v, (Gf.Quatf, Gf.Quatd, Gf.Quath)):
        return [value(float(x)) for x in v.GetImaginary()] + [value(float(v.GetReal()))]
    if isinstance(v, (Gf.Matrix2d, Gf.Matrix3d, Gf.Matrix4d)):
        n = v.dimension[0]
        return [[value(v[row][col]) for col in range(n)] for row in range(n)]
    if hasattr(v, "__len__"):
        return [value(x) for x in v]
    raise TypeError(f"no JSON for {type(v).__name__}")


def record(prim, name):
    prop = prim.GetProperty(name)
    if isinstance(prop, Usd.Relationship):
        return {"targets": [str(t) for t in prop.GetTargets()]}
    attr = prim.GetAttribute(name)
    out = {
        "default": value(attr.Get()),
        "sample": value(attr.Get(SAMPLE_TIME)),
    }
    allowed = attr.GetMetadata("allowedTokens")
    if allowed:
        out["allowed"] = [str(t) for t in allowed]
    return out


def main():
    scene = sys.argv[1] if len(sys.argv) > 1 else os.path.join(FIXTURES, "scene.usda")
    out_path = sys.argv[2] if len(sys.argv) > 2 else os.path.join(FIXTURES, "oracle.json")
    stage = Usd.Stage.Open(scene)
    properties = {}
    for prim in stage.Traverse():
        name = prim.GetName()
        if not name.startswith("Authored_"):
            continue
        twin = stage.GetPrimAtPath("/Fallback_" + name[len("Authored_"):])
        for prop in prim.GetAuthoredPropertyNames():
            for p in (twin, prim):
                properties[f"{p.GetPath()}.{prop}"] = record(p, prop)
    def relationships(layer, prim_path):
        stage = Usd.Stage.Open(os.path.join(os.path.dirname(scene), layer))
        prim = stage.GetPrimAtPath(prim_path)
        return {
            f"{prim.GetPath()}.{rel.GetName()}": [str(t) for t in rel.GetTargets()]
            for rel in prim.GetRelationships()
        }

    mapped = relationships("mapped.usda", "/Instance")
    local_variant = relationships("local_variant.usda", "/Rock")
    _, minor, patch = Usd.GetVersion()
    with open(out_path, "w") as f:
        json.dump(
            {
                "openusd_version": f"{minor}.{patch}",
                "properties": properties,
                "mapped": mapped,
                "local_variant": local_variant,
            },
            f,
            indent=1,
            sort_keys=True,
        )
        f.write("\n")


if __name__ == "__main__":
    main()
