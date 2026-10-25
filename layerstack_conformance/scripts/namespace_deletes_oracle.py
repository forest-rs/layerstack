# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's UsdNamespaceEditor does when it deletes a prim or property.

Usage: namespace_deletes_oracle.py [FIXTURES [OUT]]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

Each case opens `fixtures/namespace_deletes/scene.usda` afresh (its root
layer stack is `scene.usda` over `sub.usda`; `/R` references `asset.usda`)
and deletes one object with `DeletePrimAtPath` or `DeletePropertyAtPath`.
It records whether the editor can apply the edit (or that applying it
fails inside OpenUSD) and, after applying it,
the prims each root-stack layer authors, the root layer's relocates and
`defaultPrim`, the fields of `/Watch` that name other objects, and every
prim the stage composes (`TraverseAll`).

`tests/conformance/namespace_deletes.rs` makes the same deletes with layerstack's
`NamespaceEdit::prepare_delete` and must agree with every record.

Writes `fixtures/namespace_deletes/oracle.json` by default.
"""
import json
import os
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "namespace_deletes"))

CASES = [
    ("root_prim", "/A"),
    ("child", "/A/Child"),
    ("variant_child", "/A/InX"),
    ("class", "/Cls"),
    ("prim_with_reference", "/R"),
    ("referenced_child", "/R/FromAsset"),
    ("referenced_child_with_local_over", "/R/Local"),
    ("property", "/A.y"),
    ("referenced_property", "/R/FromAsset.p"),
    ("missing", "/Nope"),
    ("pseudo_root_child_of_missing", "/Nope/Deeper"),
]


def list_op(op):
    return {
        "explicit": [str(x) for x in op.explicitItems] if op.isExplicit else None,
        "prepend": [str(x) for x in op.prependedItems],
        "append": [str(x) for x in op.appendedItems],
        "delete": [str(x) for x in op.deletedItems],
    }


def arcs(op):
    def item(r):
        return {"asset": r.assetPath, "path": str(r.primPath)}
    return {
        "explicit": [item(x) for x in op.explicitItems] if op.isExplicit else None,
        "prepend": [item(x) for x in op.prependedItems],
        "append": [item(x) for x in op.appendedItems],
        "delete": [item(x) for x in op.deletedItems],
    }


def layer_prims(layer):
    out = []
    def visit(path):
        if path.IsPrimPath() or path.IsPrimVariantSelectionPath():
            out.append(str(path))
    layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    return sorted(out)


def watch(layer):
    spec = layer.GetPrimAtPath("/Watch")
    out = {}
    for key, read in (("inheritPaths", list_op), ("specializes", list_op), ("references", arcs)):
        if spec.HasInfo(key):
            out[key] = read(spec.GetInfo(key))
    look = spec.relationships["look"]
    out["look"] = list_op(look.GetInfo("targetPaths"))
    z = spec.attributes["z"]
    out["connections"] = list_op(z.GetInfo("connectionPaths")) if z.HasInfo("connectionPaths") else None
    out["expr"] = spec.attributes["expr"].default.GetText()
    return out


def main():
    fixtures = sys.argv[1] if len(sys.argv) > 1 else FIXTURES
    out_path = sys.argv[2] if len(sys.argv) > 2 else os.path.join(fixtures, "oracle.json")
    cases = []
    for name, path in CASES:
        for loaded in list(Sdf.Layer.GetLoadedLayers()):
            if not loaded.anonymous:
                loaded.Reload(force=True)
        stage = Usd.Stage.Open(os.path.join(fixtures, "scene.usda"))
        editor = Usd.NamespaceEditor(stage)
        target = Sdf.Path(path)
        if target.IsPropertyPath():
            editor.DeletePropertyAtPath(target)
        else:
            editor.DeletePrimAtPath(target)
        # OpenUSD 26.08 fails a verification in `Pcp_ForEachDependentNode`
        # while checking some deletes; record that instead of a result.
        failed = False
        can_apply = False
        try:
            can_apply = bool(editor.CanApplyEdits())
            if can_apply:
                assert editor.ApplyEdits()
        except Exception:
            failed = True
        if failed:
            cases.append({"name": name, "path": path, "applied": "failed"})
            continue
        root = stage.GetRootLayer()
        sub = Sdf.Layer.Find(os.path.join(fixtures, "sub.usda"))
        cases.append({
            "name": name,
            "path": path,
            "applied": can_apply,
            "layers": {"scene.usda": layer_prims(root), "sub.usda": layer_prims(sub)},
            "relocates": [[str(s), "" if t.isEmpty else str(t)] for s, t in root.relocates],
            "defaultPrim": root.defaultPrim,
            "watch": watch(root),
            "prims": [str(p.GetPath()) for p in stage.TraverseAll()],
        })
        for loaded in list(Sdf.Layer.GetLoadedLayers()):
            if not loaded.anonymous:
                loaded.Reload(force=True)

    _, minor, patch = Usd.GetVersion()
    with open(out_path, "w") as f:
        json.dump({"openusd_version": f"{minor}.{patch}", "cases": cases}, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
