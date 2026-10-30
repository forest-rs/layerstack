# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's base materials: reads, and authoring call by call.

Usage: base_material_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

Reads: on `fixtures/base_material/scene.usda`, for every prim,
`UsdShadeMaterial::GetBaseMaterialPath` (empty as `null`),
`HasBaseMaterial` and the path of `GetBaseMaterial`. The scene specializes
a material, a scope, a scope then a material, a chain of materials, a
missing prim; inherits a material that specializes another; specializes
inside a variant; references an external asset, the scene's own file by asset path and an
internal prim
whose specializes reach outside them (C++ skips a specializes arc that
crosses a reference); and references, once placed and once instanced, a
scope whose material specializes another material in it.

Authoring: on a new stage with materials `/Looks/A`, `/Looks/B` and
`/Looks/M`, it calls `SetBaseMaterialPath`, `SetBaseMaterial` (with a
material and with an invalid one) and `ClearBaseMaterial` one at a time on
`/Looks/M`, recording the root layer's `specializes` list op of `/Looks/M`
and `GetBaseMaterialPath` after each.

`tests/conformance/base_material.rs` compares everything.
"""
import argparse
import json
import pathlib

from pxr import Sdf, Usd, UsdShade

HERE = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "base_material"


def path_or_none(path):
    return str(path) if not path.isEmpty else None


def reads():
    stage = Usd.Stage.Open(str(HERE / "scene.usda"))
    found = {}
    for prim in stage.Traverse(Usd.TraverseInstanceProxies()):
        material = UsdShade.Material(prim)
        base = material.GetBaseMaterial()
        found[str(prim.GetPath())] = {
            "path": path_or_none(material.GetBaseMaterialPath()),
            "has": material.HasBaseMaterial(),
            "material": str(base.GetPath()) if base else None,
        }
    return found


def list_op(spec):
    if spec is None or not spec.HasInfo("specializes"):
        return None
    op = spec.GetInfo("specializes")
    return {
        "explicit": op.isExplicit,
        "explicit_items": [str(p) for p in op.explicitItems],
        "prepended": [str(p) for p in op.prependedItems],
    }


def calls():
    stage = Usd.Stage.CreateInMemory()
    layer = stage.GetRootLayer()
    for name in ["A", "B", "M"]:
        UsdShade.Material.Define(stage, "/Looks/" + name)
    m = UsdShade.Material.Get(stage, "/Looks/M")
    steps = [
        ("set path A", lambda: m.SetBaseMaterialPath("/Looks/A")),
        ("set material B", lambda: m.SetBaseMaterial(UsdShade.Material.Get(stage, "/Looks/B"))),
        ("set path to a missing prim", lambda: m.SetBaseMaterialPath("/Looks/Nowhere")),
        ("set invalid material", lambda: m.SetBaseMaterial(UsdShade.Material())),
        ("set path A again", lambda: m.SetBaseMaterialPath("/Looks/A")),
        ("set empty path", lambda: m.SetBaseMaterialPath(Sdf.Path())),
        ("set material B again", lambda: m.SetBaseMaterial(UsdShade.Material.Get(stage, "/Looks/B"))),
        ("clear", lambda: m.ClearBaseMaterial()),
    ]
    log = []
    for label, step in steps:
        step()
        log.append({
            "call": label,
            "specializes": list_op(layer.GetPrimAtPath("/Looks/M")),
            "path": path_or_none(m.GetBaseMaterialPath()),
        })
    return log


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "reads": reads(),
        "calls": calls(),
    }
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    output = HERE / "oracle.json"
    if args.check:
        assert output.read_text() == text, "oracle.json is out of date"
    else:
        output.write_text(text)


main()
