# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records which relationship targets, connections and collection paths
OpenUSD finds nothing at, by kind.

Usage: dangling_targets_oracle.py [--check] [OUT]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes `layerstack_conformance/fixtures/dangling_targets/oracle.json` by
default, which `tests/dangling_targets.rs` checks layerstack's
`DanglingReport` against.

The fixture's `scene.usda` is opened with every payload loaded, then
`/Terrain/Quarry` unloaded (`UsdStage::Unload`); a second record opens it
masked to `/Placements` and `/Lights` (`UsdStage::OpenMasked`), with the
same payload unloaded. For each, every prim of
`Traverse(TraverseInstanceProxies(PrimAllPrimsPredicate))` is walked and, for
each of its authored properties in order (`GetAuthoredProperties`), the
composed list (`UsdRelationship::GetTargets`,
`UsdAttribute::GetConnections`) is checked path by path:

- the path resolves when `UsdStage::GetObjectAtPath` returns an object whose
  prim is active (`IsActive`), and is `inactive` when that prim is not;
- otherwise the nearest prim `GetPrimAtPath` returns above the path (the
  prim itself, for a property path) says why: `inactive` when it is not
  active, `unloaded` when it is not loaded (`IsLoaded`), and `no_property`
  when it is the property's prim; failing that, the path is `masked` when
  the stage's population mask does not include it, else `no_prim`.

Counts are grouped by relationship name and by connection name, with the
first three dangling paths of each. Every `CollectionAPI` instance
(`GetAllCollections`, by name) records its includes and excludes
(`GetIncludesRel().GetTargets()`, `GetExcludesRel().GetTargets()`) checked
the same way, and the includes of a collection path
(`IsCollectionAPIPath`) whose prim resolves without that instance applied
(`HasAPI`), which `ComputeMembershipQuery` reads as an empty collection.

`--check` rewrites the oracle into a temporary file and fails unless it
matches the checked-in one.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE = os.path.normpath(os.path.join(HERE, "..", "fixtures", "dangling_targets"))
ORACLE = os.path.join(FIXTURE, "oracle.json")
UNLOADED = "/Terrain/Quarry"
MASK = ["/Placements", "/Lights"]
EXAMPLES = 3
KINDS = ["no_prim", "no_property", "inactive", "unloaded", "masked"]


def kind_of(stage, path):
    """Why `path` names nothing usable, or `None` when it resolves."""
    obj = stage.GetObjectAtPath(path)
    if obj:
        prim = obj if path.IsPrimPath() else obj.GetPrim()
        return None if prim.IsActive() else "inactive"
    prim_path = path.GetPrimPath()
    at = prim_path if path.IsPropertyPath() else prim_path.GetParentPath()
    while not at.isEmpty:
        prim = stage.GetPrimAtPath(at)
        if prim:
            if not prim.IsActive():
                return "inactive"
            if not prim.IsLoaded():
                return "unloaded"
            if at == prim_path:
                return "no_property"
            break
        at = at.GetParentPath()
    if not stage.GetPopulationMask().Includes(prim_path):
        return "masked"
    return "no_prim"


def counts():
    return {kind: 0 for kind in KINDS}


def walk(stage):
    prims = list(stage.Traverse(Usd.TraverseInstanceProxies(Usd.PrimAllPrimsPredicate)))
    groups = {"relationships": {}, "connections": {}}
    for prim in prims:
        for prop in prim.GetAuthoredProperties():
            if isinstance(prop, Usd.Relationship):
                section, targets = "relationships", prop.GetTargets()
            elif isinstance(prop, Usd.Attribute):
                section, targets = "connections", prop.GetConnections()
            else:
                continue
            if not targets:
                continue
            group = groups[section].setdefault(prop.GetName(), {
                "properties": 0, "targets": 0, "dangling": counts(), "examples": []})
            group["properties"] += 1
            group["targets"] += len(targets)
            for target in targets:
                kind = kind_of(stage, target)
                if kind is None:
                    continue
                group["dangling"][kind] += 1
                if len(group["examples"]) < EXAMPLES:
                    group["examples"].append([str(prop.GetPath()), str(target), kind])
    collections = []
    for prim in prims:
        found = sorted(Usd.CollectionAPI.GetAllCollections(prim), key=lambda c: c.GetName())
        for collection in found:
            entry = {
                "owner": str(prim.GetPath()),
                "name": collection.GetName(),
                "includes": 0,
                "excludes": 0,
                "dangling_includes": counts(),
                "dangling_excludes": counts(),
                "missing_collections": 0,
                "examples": [],
            }
            lists = [("includes", collection.GetIncludesRel().GetTargets()),
                     ("excludes", collection.GetExcludesRel().GetTargets())]
            for name, targets in lists:
                entry[name] = len(targets)
                for target in targets:
                    kind = kind_of(stage, target)
                    problem = kind
                    is_collection = Usd.CollectionAPI.IsCollectionAPIPath(target)
                    included = target.name[len("collection:"):]
                    if (name == "includes" and is_collection
                            and kind in (None, "no_property")
                            and not stage.GetPrimAtPath(target.GetPrimPath()).HasAPI(
                                Usd.CollectionAPI, included)):
                        problem = "no_collection"
                        entry["missing_collections"] += 1
                    elif kind is None:
                        continue
                    else:
                        entry["dangling_" + name][kind] += 1
                    if len(entry["examples"]) < EXAMPLES:
                        entry["examples"].append([name, str(target), problem])
            collections.append(entry)
    return {**groups, "collections": collections}


def record():
    scene = os.path.join(FIXTURE, "scene.usda")
    stage = Usd.Stage.Open(scene, Usd.Stage.LoadAll)
    stage.Unload(Sdf.Path(UNLOADED))
    masked = Usd.Stage.OpenMasked(
        scene, Usd.StagePopulationMask([Sdf.Path(p) for p in MASK]), Usd.Stage.LoadAll)
    masked.Unload(Sdf.Path(UNLOADED))
    _, minor, patch = Usd.GetVersion()
    return {
        "openusd_version": f"{minor}.{patch}",
        "unloaded": UNLOADED,
        "mask": MASK,
        "examples": EXAMPLES,
        "stage": walk(stage),
        "masked": walk(masked),
    }


def write(path):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(record(), f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=ORACLE)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "oracle.json")
        write(path)
        if not filecmp.cmp(path, ORACLE, shallow=False):
            raise SystemExit(f"{ORACLE} is stale")


if __name__ == "__main__":
    main()
