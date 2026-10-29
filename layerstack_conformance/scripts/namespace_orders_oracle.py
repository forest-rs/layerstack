# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's namespace edits do to the child and property
orders a prim spec authors.

Usage: namespace_orders_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/namespace_orders` by default,
the layers below (`scene.usda` over `sub.usda`, `/R` referencing
`asset.usda`; `variant.usda` on its own) and `oracle.json`, which
`tests/conformance/namespace_orders.rs` replays against layerstack.

A prim spec's `primOrder` (`reorder nameChildren`, `reorder rootPrims` on
the pseudo-root) and `propertyOrder` (`reorder properties`) are fields of
their own, apart from the child and property lists. OpenUSD's namespace
edits change only the lists: `SdfLayer::Apply` moves and removes children
through `Sdf_ChildrenUtils::MoveChildForBatchNamespaceEdit` and
`RemoveChildForBatchNamespaceEdit` (`pxr/usd/sdf/childrenUtils.cpp`), and
`UsdNamespaceEditor` applies every layer's move or delete as one
`SdfBatchNamespaceEdit` (`_ApplyLayerSpecMove`,
`pxr/usd/usd/namespaceEditor.cpp`), so a renamed, reparented or deleted
child keeps its old name in its parent's orders and its new name is listed
in none. `SdfPrimSpec::RemoveNameChild`, `RemoveProperty` and `SdfCopySpec`
leave the orders as they are too. Only `SdfPrimSpec::SetName` replaces the
old name with the new one in its parent's `primOrder`
(`pxr/usd/sdf/primSpec.cpp`); `SdfPropertySpec::SetName` leaves
`propertyOrder` alone.

Each case opens its scene afresh and records whether OpenUSD applies it
and then, for each layer, every prim spec and variant prim spec with its
children, properties and orders (`null` when it authors none), and every
composed prim's children (`GetAllChildrenNames`) and property names
(`GetPropertyNames`):

- `cases`, each one call: a `UsdNamespaceEditor` rename, reparent or delete
  of a prim or property through the root layer, or, in `variant.usda`,
  through `/V`'s variant (`target` `variant`), which `CanApplyEdits`
  refuses (edit targets that map across arcs are not supported, in
  `_GatherLayersToEdit`); `SdfLayer::Apply` of one
  `SdfBatchNamespaceEdit` on `asset.usda` (`layer_apply`), the edit a
  namespace move through `/R`'s reference node lowers to (OpenUSD's editor
  fails inside `PcpGatherDependentNamespaceEdits` for that target);
  `SdfPrimSpec::RemoveNameChild` and `RemoveProperty` (`remove_spec`); and
  `SdfCopySpec` (`copy_spec`);
- `before`: each scene's record before any call;
- `set_name`: `SdfPrimSpec::SetName` and `SdfPropertySpec::SetName` on
  `scene.usda`, for contrast. No layerstack API makes those calls, so the
  test does not replay them.

`--check` rewrites everything into a temporary directory and fails unless
the layers and `oracle.json` match the checked-in fixture.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "namespace_orders"))

LAYERS = {
    "scene.usda": '''#usda 1.0
(
    subLayers = [
        @sub.usda@
    ]
)

reorder rootPrims = ["Q", "R", "P"]

# `/P` orders a child it does not have (`Gone`) and leaves one out (`E`).
def "P"
{
    reorder nameChildren = ["C", "A", "Gone", "B"]
    reorder properties = ["z", "y", "x"]
    int x = 0
    int y = 1
    int z = 2

    def "A"
    {
    }

    def "B"
    {
    }

    def "C"
    {
    }

    def "E"
    {
    }
}

# `/Q` orders a child named as `/P/A` before it has one.
def "Q"
{
    reorder nameChildren = ["D", "A"]
    reorder properties = ["w", "x"]
    int w = 0

    def "D"
    {
    }
}

def "R" (
    prepend references = @asset.usda@</Asset>
)
{
}
''',
    "sub.usda": '''#usda 1.0

# A weaker opinion on `/P` with an order of its own.
over "P"
{
    reorder nameChildren = ["B", "A"]
    reorder properties = ["x", "y"]
    int y = 5

    over "A"
    {
    }

    over "B"
    {
    }
}
''',
    "variant.usda": '''#usda 1.0

def "V" (
    variants = {
        string v = "x"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "x" {
            reorder nameChildren = ["G", "F"]
            def "F"
            {
            }

            def "G"
            {
            }

        }
    }
}
''',
    "asset.usda": '''#usda 1.0

def "Asset"
{
    reorder nameChildren = ["K", "J"]
    def "J"
    {
    }

    def "K"
    {
    }
}
''',
}

# The layer each scene opens, and the layers a record of it lists.
SCENES = {
    "scene.usda": ["scene.usda", "sub.usda", "asset.usda"],
    "variant.usda": ["variant.usda"],
}

# (name, call, target, path, destination): `destination` is the new path of
# a move, `None` for a delete. The `variant` target edits `variant.usda`,
# every other case `scene.usda`.
CASES = [
    ("rename_child", "editor", "root", "/P/A", "/P/A2"),
    ("rename_onto_ordered_name", "editor", "root", "/P/A", "/P/Gone"),
    ("rename_unordered_child", "editor", "root", "/P/E", "/P/E2"),
    ("rename_root_prim", "editor", "root", "/P", "/P2"),
    ("reparent_child", "editor", "root", "/P/A", "/Q/A"),
    ("reparent_and_rename_child", "editor", "root", "/P/B", "/Q/B2"),
    ("delete_child", "editor", "root", "/P/A", None),
    ("rename_property", "editor", "root", "/P.y", "/P.y2"),
    ("reparent_property", "editor", "root", "/P.x", "/Q.x"),
    ("delete_property", "editor", "root", "/P.z", None),
    ("rename_variant_child", "editor", "variant", "/V/F", "/V/F2"),
    ("delete_variant_child", "editor", "variant", "/V/F", None),
    ("rename_asset_child", "layer_apply", "asset.usda", "/Asset/J", "/Asset/J2"),
    ("remove_child_spec", "remove_spec", "scene.usda", "/P/A", None),
    ("remove_property_spec", "remove_spec", "scene.usda", "/P.z", None),
    ("copy_child_spec", "copy_spec", "scene.usda", "/P/A", "/Q/A"),
    ("copy_ordered_spec", "copy_spec", "scene.usda", "/P", "/P3"),
]

SET_NAME = [
    ("prim", "/P/A", "A2"),
    ("prim_onto_ordered_name", "/P/A", "Gone"),
    ("root_prim", "/P", "P2"),
    ("property", "/P.y", "y2"),
]


def order(spec, key):
    return [str(n) for n in spec.GetInfo(key)] if spec.HasInfo(key) else None


def layer_record(layer):
    """Every prim spec and variant prim spec of `layer`, the pseudo-root's
    included, with its children, properties and orders."""
    out = {}

    def add(path):
        spec = layer.GetPrimAtPath(path)
        out[str(path)] = {
            "children": [c.name for c in spec.nameChildren],
            "primOrder": order(spec, "primOrder"),
            "properties": [p.name for p in spec.properties],
            "propertyOrder": order(spec, "propertyOrder"),
        }

    def visit(path):
        # A variant set's own path (`/V{v=}`) is no prim spec.
        if path.IsPrimPath() or (path.IsPrimVariantSelectionPath()
                                 and path.GetVariantSelection()[1]):
            add(path)

    add(Sdf.Path.absoluteRootPath)
    layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    return dict(sorted(out.items()))


def composed_record(stage):
    out = {}
    for prim in [stage.GetPseudoRoot()] + list(stage.TraverseAll()):
        out[str(prim.GetPath())] = {
            "children": [str(n) for n in prim.GetAllChildrenNames()],
            "properties": [str(n) for n in prim.GetPropertyNames()],
        }
    return out


def reload_layers():
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)


def layer_named(directory, name):
    return Sdf.Layer.FindOrOpen(os.path.join(directory, name))


def apply(directory, stage, call, target, path, destination):
    path = Sdf.Path(path)
    destination = Sdf.Path(destination) if destination else None
    if call == "editor":
        if target == "variant":
            prim = stage.GetPrimAtPath("/V")
            stage.SetEditTarget(prim.GetVariantSet("v").GetVariantEditTarget())
        editor = Usd.NamespaceEditor(stage)
        if path.IsPrimPath():
            prim = stage.GetPrimAtPath(path)
            if destination is None:
                editor.DeletePrim(prim)
            elif destination.GetParentPath() == path.GetParentPath():
                editor.RenamePrim(prim, destination.name)
            else:
                editor.ReparentPrim(
                    prim, stage.GetPrimAtPath(destination.GetParentPath()), destination.name)
        else:
            prop = stage.GetPropertyAtPath(path)
            if destination is None:
                editor.DeleteProperty(prop)
            elif destination.GetPrimPath() == path.GetPrimPath():
                editor.RenameProperty(prop, destination.name)
            else:
                editor.ReparentProperty(
                    prop, stage.GetPrimAtPath(destination.GetPrimPath()), destination.name)
        if not editor.CanApplyEdits():
            return False
        return bool(editor.ApplyEdits())
    layer = layer_named(directory, target)
    if call == "layer_apply":
        edit = Sdf.NamespaceEdit(path, destination or Sdf.Path(), Sdf.NamespaceEdit.same)
        return bool(layer.Apply(Sdf.BatchNamespaceEdit([edit])))
    if call == "remove_spec":
        if path.IsPrimPath():
            # `SdfPrimSpec::RemoveNameChild`, as the children proxy deletes.
            del layer.GetPrimAtPath(path.GetParentPath()).nameChildren[path.name]
        else:
            owner = layer.GetPrimAtPath(path.GetPrimPath())
            owner.RemoveProperty(layer.GetPropertyAtPath(path))
        return True
    if call == "copy_spec":
        return bool(Sdf.CopySpec(layer, path, layer, destination))
    raise ValueError(call)


def scene_of(target):
    return "variant.usda" if target == "variant" else "scene.usda"


def record(directory, scene, stage, applied):
    return {
        "applied": applied,
        "layers": {name: layer_record(layer_named(directory, name))
                   for name in SCENES[scene]},
        "composed": composed_record(stage),
    }


def cases_record(directory):
    out = []
    for name, call, target, path, destination in CASES:
        reload_layers()
        scene = scene_of(target)
        stage = Usd.Stage.Open(os.path.join(directory, scene))
        applied = apply(directory, stage, call, target, path, destination)
        case = {"name": name, "scene": scene, "call": call, "target": target,
                "path": path, "destination": destination}
        case.update(record(directory, scene, stage, applied))
        out.append(case)
    reload_layers()
    return out


def set_name_record(directory):
    out = []
    for name, path, new_name in SET_NAME:
        reload_layers()
        stage = Usd.Stage.Open(os.path.join(directory, "scene.usda"))
        layer = stage.GetRootLayer()
        spec = layer.GetObjectAtPath(path)
        spec.name = new_name
        case = {"name": name, "path": path, "new_name": new_name}
        case.update(record(directory, "scene.usda", stage, True))
        out.append(case)
    reload_layers()
    return out


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    before = {}
    for scene in SCENES:
        stage = Usd.Stage.Open(os.path.join(out, scene))
        errors = stage.GetCompositionErrors()
        if errors:
            raise SystemExit(f"unexpected composition errors: {errors}")
        before[scene] = record(out, scene, stage, True)
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "before": before,
        "cases": cases_record(out),
        "set_name": set_name_record(out),
    }
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        for name in list(LAYERS) + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
