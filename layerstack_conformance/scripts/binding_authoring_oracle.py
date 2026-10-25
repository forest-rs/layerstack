# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's material binding authoring writes, call by call.

Usage: binding_authoring_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

On a new stage with `/Set` (an `Xform` with `CollectionAPI:metal`
including `/Set/Mesh`), `/Set/Mesh` and the materials `/Looks/A` and
`/Looks/B`, both prims with `MaterialBindingAPI` applied, it calls
`UsdShadeMaterialBindingAPI::Bind` (direct and collection),
`UnbindDirectBinding`, `UnbindCollectionBinding` and `UnbindAllBindings`
one at a time. After each it records whether the call was accepted and,
for every binding relationship spec in the root layer, its explicit
targets, its `bindMaterialAs` and whether it is `custom`.

`tests/conformance/binding_authoring.rs` replays the calls through
`MaterialBindingApiEdit` and compares the specs.
"""
import argparse
import json
import pathlib

from pxr import Sdf, Tf, Usd, UsdGeom, UsdShade

OUT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "binding_authoring" / "oracle.json"


def specs(layer):
    found = {}

    def visit(spec):
        for prop in spec.properties:
            if isinstance(prop, Sdf.RelationshipSpec) and prop.name.startswith("material:binding"):
                found[str(prop.path)] = {
                    "targets": [str(p) for p in prop.targetPathList.explicitItems],
                    "explicit": prop.targetPathList.isExplicit,
                    "strength": prop.GetInfo("bindMaterialAs") if prop.HasInfo("bindMaterialAs") else None,
                    "custom": prop.custom,
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
    set_prim = UsdGeom.Xform.Define(stage, "/Set").GetPrim()
    mesh = UsdGeom.Mesh.Define(stage, "/Set/Mesh").GetPrim()
    a = UsdShade.Material.Define(stage, "/Looks/A")
    b = UsdShade.Material.Define(stage, "/Looks/B")
    collection = Usd.CollectionAPI.Apply(set_prim, "metal")
    collection.CreateIncludesRel().AddTarget("/Set/Mesh")
    api = UsdShade.MaterialBindingAPI.Apply(mesh)
    set_api = UsdShade.MaterialBindingAPI.Apply(set_prim)
    T = UsdShade.Tokens
    calls = [
        ("bind A", lambda: api.Bind(a)),
        ("bind A stronger", lambda: api.Bind(a, T.strongerThanDescendants)),
        ("bind B fallback", lambda: api.Bind(b, T.fallbackStrength)),
        ("bind A weaker preview", lambda: api.Bind(a, T.weakerThanDescendants, T.preview)),
        ("bind A purpose 'a b'", lambda: api.Bind(a, T.fallbackStrength, "a b")),
        ("bind collection metalBits", lambda: set_api.Bind(collection, b, "metalBits")),
        ("bind collection unnamed preview",
         lambda: set_api.Bind(collection, b, "", T.fallbackStrength, T.preview)),
        ("bind collection bad:name", lambda: set_api.Bind(collection, b, "bad:name")),
        ("bind collection '1x'", lambda: set_api.Bind(collection, b, "1x")),
        ("unbind", lambda: api.UnbindDirectBinding()),
        ("unbind full", lambda: api.UnbindDirectBinding(T.full)),
        ("unbind collection metalBits", lambda: set_api.UnbindCollectionBinding("metalBits")),
        ("unbind all /Set", lambda: set_api.UnbindAllBindings()),
        ("unbind all /Set/Mesh", lambda: api.UnbindAllBindings()),
    ]
    log = []
    for label, call in calls:
        try:
            accepted = bool(call())
        except Tf.ErrorException:
            accepted = False
        log.append({"call": label, "accepted": accepted, "specs": specs(layer)})
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
