# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD's edit targets map spec paths and relationship targets.

Usage: edit_target_maps_oracle.py [FIXTURES [OUT]]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

`tests/edit_target_maps.rs` saves the scene it edits as
`fixtures/edit_target_maps/scene.usda` (with `asset.usda`, which `/Ref`
references): `/Inst` inherits `/Class`, `/Spec` specializes `/Base`,
`/Rock` has a local variant `{look=a}`, and `/Ref`'s referenced `/Asset`
has a variant `{v=x}`. For each case below this script builds OpenUSD's
edit target, `Usd.EditTarget(layer, node)` from the prim index's node of
that arc (or `Usd.EditTarget.ForLocalDirectVariant`), and records:

- `MapToSpecPath` of each probed stage path (`null` when it does not map);
- for each target, alone, what `UsdRelationship::SetTargets` authors on
  the prim's `look` relationship through the edit target (`null` when it
  rejects the target) and what `GetTargets` then reads.

It also opens `authored.usda`, where the test authored every accepted
target of the root-layer cases through its own edit targets, and records
the composed targets of each prim's `look` (`composed`).

Writes `fixtures/edit_target_maps/oracle.json` by default.
"""
import json
import os
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "edit_target_maps"))

# (name, prim, arc, probed stage paths, targets)
CASES = [
    ("inherit", "/Inst", "inherit", ["/Inst", "/Inst/Child", "/Light", "/Class", "/Class/X"],
     ["/Light", "/Light.intensity", "/Inst/Child", "/Class/X"]),
    ("specialize", "/Spec", "specialize", ["/Spec", "/Spec/Child", "/Light", "/Base", "/Base/X"],
     ["/Light", "/Light.intensity", "/Spec/Child", "/Base/X"]),
    ("variant", "/Rock", "variant", ["/Rock", "/Rock/Pebble", "/Light"],
     ["/Light", "/Light.intensity", "/Rock/Pebble"]),
    ("local_direct_variant", "/Rock", "local_direct_variant", ["/Rock", "/Rock/Pebble", "/Light"],
     ["/Light", "/Light.intensity", "/Rock/Pebble"]),
    ("variant_under_reference", "/Ref", "variant", ["/Ref", "/Ref/Child", "/Light"],
     ["/Ref/Child", "/Light"]),
]


def node_of(prim, arc):
    stack = [prim.GetPrimIndex().rootNode]
    while stack:
        node = stack.pop(0)
        if str(node.arcType).lower().endswith(arc):
            return node
        stack.extend(node.children)
    raise LookupError(f"{prim.GetPath()} has no {arc} node")


def main():
    fixtures = sys.argv[1] if len(sys.argv) > 1 else FIXTURES
    out_path = sys.argv[2] if len(sys.argv) > 2 else os.path.join(fixtures, "oracle.json")
    stage = Usd.Stage.Open(os.path.join(fixtures, "scene.usda"))
    root = stage.GetRootLayer()
    cases = {}
    for name, prim_path, arc, paths, targets in CASES:
        prim = stage.GetPrimAtPath(prim_path)
        if arc == "local_direct_variant":
            target = Usd.EditTarget.ForLocalDirectVariant(root, Sdf.Path("/Rock{look=a}"))
        else:
            node = node_of(prim, arc)
            target = Usd.EditTarget(node.layerStack.layerTree.layer, node)
        spec_paths = {}
        for path in paths:
            mapped = target.MapToSpecPath(Sdf.Path(path))
            spec_paths[path] = None if mapped.isEmpty else str(mapped)
        authored = {}
        for path in targets:
            layer = target.GetLayer()
            state = layer.ExportToString()
            stage.SetEditTarget(target)
            rel = prim.CreateRelationship("look")
            try:
                rel.SetTargets([Sdf.Path(path)])
                spec = target.GetPropertySpecForScenePath(rel.GetPath())
                authored[path] = {
                    "authored": [str(t) for t in spec.targetPathList.explicitItems],
                    "reads": [str(t) for t in rel.GetTargets()],
                }
            except Exception:
                authored[path] = None
            stage.SetEditTarget(Usd.EditTarget(root))
            layer.ImportFromString(state)
        cases[name] = {"spec_paths": spec_paths, "targets": authored}

    composed = {}
    authored_path = os.path.join(fixtures, "authored.usda")
    # The test writes `authored.usda` from the cases recorded here, so a
    # first run records the cases alone.
    authored_stage = (
        Usd.Stage.Open(authored_path) if os.path.getsize(authored_path) else None
    )
    for prim_path in ["/Inst", "/Spec", "/Rock"] if authored_stage else []:
        rel = authored_stage.GetPrimAtPath(prim_path).GetRelationship("look")
        composed[prim_path] = [str(t) for t in rel.GetTargets()]

    _, minor, patch = Usd.GetVersion()
    with open(out_path, "w") as f:
        json.dump(
            {"openusd_version": f"{minor}.{patch}", "cases": cases, "composed": composed},
            f,
            indent=1,
            sort_keys=True,
        )
        f.write("\n")


if __name__ == "__main__":
    main()
