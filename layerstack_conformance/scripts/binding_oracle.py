# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's material bindings and collection memberships.

Usage: binding_oracle.py [FIXTURES [OUT]]
       binding_oracle.py --check [FIXTURES]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Opens `fixtures/binding/scene.usda` (which references `asset.usda`) and
writes `oracle.json` beside it:

- for every prim, for each material purpose (all-purpose, `preview`,
  `full`) and with legacy bindings supported and not
  (`supportLegacyBindings`), `UsdShadeMaterialBindingAPI::
  ComputeBoundMaterial`: the bound material's path (or `null`) and the
  winning binding relationship's path (or `null`);
- for every `CollectionAPI` instance, `ComputeMembershipQuery`: whether it
  uses its rule map, its `mode`, and which of the stage's prim
  and property paths (`GetPropertyNames`), and the pseudo-root,
  `IsPathIncluded` includes.

`tests/binding.rs` compares everything.
"""
import json
import os
import sys

from pxr import Sdf, Usd, UsdShade

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "binding"))
PURPOSES = ["", "preview", "full"]


def record(scene):
    stage = Usd.Stage.Open(scene)
    prims = list(stage.Traverse())
    # The pseudo-root too: `includeRoot` includes its descendants, never it.
    paths = ["/"]
    for prim in prims:
        paths.append(str(prim.GetPath()))
        paths.extend(str(prim.GetPath().AppendProperty(n)) for n in prim.GetPropertyNames())
    bindings = {}
    for prim in prims:
        api = UsdShade.MaterialBindingAPI(prim)
        entry = {}
        for purpose in PURPOSES:
            for legacy in (True, False):
                material, rel = api.ComputeBoundMaterial(purpose, supportLegacyBindings=legacy)
                entry[f"{purpose or 'all'}@{'legacy' if legacy else 'strict'}"] = {
                    "material": str(material.GetPath()) if material else None,
                    "relationship": str(rel.GetPath()) if rel else None,
                }
        bindings[str(prim.GetPath())] = entry
    collections = {}
    for prim in prims:
        for collection in Usd.CollectionAPI.GetAllCollections(prim):
            query = collection.ComputeMembershipQuery()
            included = [p for p in paths if query.IsPathIncluded(Sdf.Path(p))]
            collections[str(collection.GetCollectionPath())] = {
                "uses_rule_map": query.UsesPathExpansionRuleMap(),
                "mode": str(collection.GetModeAttr().Get()),
                "included": included,
            }
    return bindings, collections


def main():
    args = sys.argv[1:]
    check = bool(args) and args[0] == "--check"
    if check:
        args = args[1:]
    fixtures = args[0] if args else FIXTURES
    out_path = args[1] if len(args) > 1 else os.path.join(fixtures, "oracle.json")
    bindings, collections = record(os.path.join(fixtures, "scene.usda"))
    _, minor, patch = Usd.GetVersion()
    result = {"openusd_version": f"{minor}.{patch}", "bindings": bindings, "collections": collections}
    if check:
        with open(os.path.join(fixtures, "oracle.json")) as f:
            if json.load(f) != json.loads(json.dumps(result)):
                sys.exit("oracle.json is not what OpenUSD computes; rerun binding_oracle.py")
        return
    with open(out_path, "w") as f:
        json.dump(result, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
