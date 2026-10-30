# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's coordinate-system binding authoring, call by call.

Usage: coord_sys_authoring_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

On a new stage with `/World/Space`, `/World/Other`, `/World/Model` (with
`CoordSysAPI:modelSpace` applied) and `/World/Plain`, it calls the
multiple-apply `UsdShadeCoordSysAPI` authoring API one at a time: `Bind`
(on an applied and an unapplied instance, and again to replace),
`BlockBinding` and `ClearBinding` with and without `removeSpec`, including
on a relationship that does not exist. After each it records whether the
call was accepted, every `coordSys:` relationship spec of the root layer
(`custom`, whether its list is explicit, its explicit and prepended
targets), and for each prim `HasLocalBindingsForPrim`,
`GetLocalBindingsForPrim` and each instance's `GetLocalBinding` (whose
`name` is `GetBindingBaseName`).

It also records the static `GetBindingBaseName` (which is
`SdfPath::StripPrefixNamespace(name, "coordSys")`, the only form Python
reaches) and `CanContainPropertyName` for a set of names.

`tests/conformance/coord_sys_authoring.rs` replays the calls through
`CoordSysApiEdit` and compares everything.
"""
import argparse
import json
import pathlib

from pxr import Sdf, Tf, Usd, UsdGeom, UsdShade

OUT = (pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "coord_sys_authoring"
       / "oracle.json")
PRIMS = ["/World/Model", "/World/Plain"]
INSTANCES = ["modelSpace", "paint", "fresh", "missing"]
NAMES = ["coordSys:worldSpace:binding", "coordSys:x", "coordSys", "coordSys:",
         "coordSysX:y", "coordSystem", "other:coordSys:a", "", "CoordSys:a"]


def specs(layer):
    found = {}

    def visit(spec):
        for prop in spec.properties:
            if isinstance(prop, Sdf.RelationshipSpec) and prop.name.startswith("coordSys:"):
                op = prop.targetPathList
                found[str(prop.path)] = {
                    "custom": prop.custom,
                    "explicit": op.isExplicit,
                    "explicit_targets": [str(p) for p in op.explicitItems],
                    "prepended": [str(p) for p in op.prependedItems],
                }
        for child in spec.nameChildren:
            visit(child)

    for root in layer.rootPrims:
        visit(root)
    return found


def binding(b):
    name, relationship, target = b
    if not str(relationship):
        return None
    return {"name": str(name), "relationship": str(relationship), "target": str(target)}


def reads(stage):
    found = {}
    for path in PRIMS:
        prim = stage.GetPrimAtPath(path)
        found[path] = {
            "has_local": UsdShade.CoordSysAPI.HasLocalBindingsForPrim(prim),
            "local": [binding(b) for b in UsdShade.CoordSysAPI.GetLocalBindingsForPrim(prim)],
            "instance": {name: binding(UsdShade.CoordSysAPI(prim, name).GetLocalBinding())
                         for name in INSTANCES},
        }
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stage = Usd.Stage.CreateInMemory()
    layer = stage.GetRootLayer()
    for path in ["/World", "/World/Space", "/World/Other", "/World/Model", "/World/Plain"]:
        UsdGeom.Xform.Define(stage, path)
    model = stage.GetPrimAtPath("/World/Model")
    plain = stage.GetPrimAtPath("/World/Plain")
    UsdShade.CoordSysAPI.Apply(model, "modelSpace")
    api = UsdShade.CoordSysAPI
    space, other = Sdf.Path("/World/Space"), Sdf.Path("/World/Other")
    calls = [
        ("bind modelSpace", lambda: api(model, "modelSpace").Bind(space)),
        ("bind paint unapplied", lambda: api(plain, "paint").Bind(space)),
        ("bind modelSpace again", lambda: api(model, "modelSpace").Bind(other)),
        ("block modelSpace", lambda: api(model, "modelSpace").BlockBinding()),
        ("clear modelSpace", lambda: api(model, "modelSpace").ClearBinding(False)),
        ("bind modelSpace after clear", lambda: api(model, "modelSpace").Bind(space)),
        ("clear paint removing spec", lambda: api(plain, "paint").ClearBinding(True)),
        ("clear missing", lambda: api(plain, "missing").ClearBinding(False)),
        ("block fresh unapplied", lambda: api(plain, "fresh").BlockBinding()),
        ("clear fresh removing spec", lambda: api(plain, "fresh").ClearBinding(True)),
        ("clear modelSpace removing spec", lambda: api(model, "modelSpace").ClearBinding(True)),
    ]
    log = []
    for label, call in calls:
        try:
            accepted = bool(call())
        except Tf.ErrorException:
            accepted = False
        log.append({"call": label, "accepted": accepted, "specs": specs(layer),
                    "reads": reads(stage)})
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "calls": log,
        "base_names": {name: Sdf.Path.StripPrefixNamespace(name, "coordSys")[0]
                       for name in NAMES},
        "can_contain": {name: api.CanContainPropertyName(name) for name in NAMES},
    }
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.check:
        assert OUT.read_text() == text, "oracle.json is out of date"
    else:
        OUT.write_text(text)


main()
