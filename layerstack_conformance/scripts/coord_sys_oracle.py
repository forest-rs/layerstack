# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's coordinate-system bindings with inheritance.

Usage: coord_sys_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Opens `fixtures/coord_sys/scene.usda` and, for every prim, records
`UsdShadeCoordSysAPI::GetLocalBindingsForPrim`,
`FindBindingsWithInheritanceForPrim` and, for each name the scene binds,
`FindBindingWithInheritance`. The scene overrides an inherited name,
blocks one (so it is inherited from further up), authors a relationship
without its applied instance, applies an instance with no relationship,
and binds through a forwarding relationship.

`tests/coord_sys.rs` compares everything.
"""
import argparse
import json
import pathlib

from pxr import Usd, UsdShade

ROOT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "coord_sys"
NAMES = ["worldSpace", "modelSpace", "paintSpace", "projection", "unapplied"]


def binding(b):
    name, relationship, target = b
    return {"name": str(name), "relationship": str(relationship), "target": str(target)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stage = Usd.Stage.Open(str(ROOT / "scene.usda"))
    prims = {}
    for prim in stage.Traverse():
        found = {}
        for name in NAMES:
            b = UsdShade.CoordSysAPI(prim, name).FindBindingWithInheritance()
            found[name] = binding(b) if str(b[1]) else None
        prims[str(prim.GetPath())] = {
            "local": [binding(b) for b in UsdShade.CoordSysAPI.GetLocalBindingsForPrim(prim)],
            "inherited": [binding(b) for b in
                          UsdShade.CoordSysAPI.FindBindingsWithInheritanceForPrim(prim)],
            "find": found,
        }
    result = {"openusd_version": ".".join(map(str, Usd.GetVersion()[1:])), "prims": prims}
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    output = ROOT / "oracle.json"
    if args.check:
        assert output.read_text() == text, "oracle.json is out of date"
    else:
        output.write_text(text)


main()
