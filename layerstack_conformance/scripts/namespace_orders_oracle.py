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

A moved child joins the end of its new parent's child list, a renamed one
keeps its place in its own, and composition orders each list by the
orders as authored (AOUSD Core §7.6.2.4.5 `primOrder`, §11.3.1): a
renamed child the order listed falls out of the ordered run, and one
moved under an ordered parent is ordered only if the parent already
names it. A rename that `UsdNamespaceEditor` makes through relocates
(`/V/F`, which only `/V`'s variant authors) changes no spec at all, so
the variant's order keeps naming `F`, and the relocated `F2` keeps its
place among its siblings.

Each case opens its scene afresh and records whether OpenUSD applies it
and then, for each layer, every prim spec and variant prim spec with its
children, properties and orders (`null` when it authors none), the
layer's `ExportToString`, and every composed prim's children
(`GetAllChildrenNames`) and property names (`GetPropertyNames`):

- `cases`, each one call: a `UsdNamespaceEditor` rename, reparent or delete
  of a prim or property through the root layer, of `scene.usda` (into
  parents with and without orders of their own, to and from the root) or
  of `variant.usda` (the variant's host, and a child only the variant
  authors), or through `/V`'s variant (`target` `variant`), which
  `CanApplyEdits` refuses (edit targets that map across arcs are not
  supported, in `_GatherLayersToEdit`); `SdfLayer::Apply` of one
  `SdfBatchNamespaceEdit` on `asset.usda` (`layer_apply`), the edit a
  namespace move through `/R`'s reference node lowers to (OpenUSD's editor
  fails inside `PcpGatherDependentNamespaceEdits` for that target);
  `SdfPrimSpec::RemoveNameChild` and `RemoveProperty` (`remove_spec`); and
  `SdfCopySpec` (`copy_spec`);
- `before`: each scene's record before any call;
- `unreplayed`: for contrast, `SdfPrimSpec::SetName` and
  `SdfPropertySpec::SetName` (`set_name`), and `SdfLayer::Apply` on a
  variant's child (`layer_apply`), in the root layers. No layerstack API
  makes those calls, so the test does not replay them.

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

# (name, call, target, path, destination, scene): `destination` is the new
# path of a move, `None` for a delete; `scene` is the layer the case opens.
CASES = [
    ("rename_child", "editor", "root", "/P/A", "/P/A2", "scene.usda"),
    ("rename_onto_ordered_name", "editor", "root", "/P/A", "/P/Gone", "scene.usda"),
    ("rename_unordered_child", "editor", "root", "/P/E", "/P/E2", "scene.usda"),
    ("rename_root_prim", "editor", "root", "/P", "/P2", "scene.usda"),
    ("reparent_child", "editor", "root", "/P/A", "/Q/A", "scene.usda"),
    ("reparent_and_rename_child", "editor", "root", "/P/B", "/Q/B2", "scene.usda"),
    ("reparent_into_unordered_parent", "editor", "root", "/P/A", "/R/A", "scene.usda"),
    ("reparent_to_root", "editor", "root", "/P/C", "/C", "scene.usda"),
    ("reparent_root_prim", "editor", "root", "/Q", "/P/Q", "scene.usda"),
    ("delete_child", "editor", "root", "/P/A", None, "scene.usda"),
    ("rename_property", "editor", "root", "/P.y", "/P.y2", "scene.usda"),
    ("reparent_property", "editor", "root", "/P.x", "/Q.x", "scene.usda"),
    ("reparent_property_into_unordered", "editor", "root", "/P.y", "/R.y", "scene.usda"),
    ("delete_property", "editor", "root", "/P.z", None, "scene.usda"),
    ("rename_variant_host", "editor", "root", "/V", "/V2", "variant.usda"),
    ("rename_child_from_variant", "editor", "root", "/V/F", "/V/F2", "variant.usda"),
    ("rename_variant_child", "editor", "variant", "/V/F", "/V/F2", "variant.usda"),
    ("delete_variant_child", "editor", "variant", "/V/F", None, "variant.usda"),
    ("rename_asset_child", "layer_apply", "asset.usda", "/Asset/J", "/Asset/J2", "scene.usda"),
    ("remove_child_spec", "remove_spec", "scene.usda", "/P/A", None, "scene.usda"),
    ("remove_property_spec", "remove_spec", "scene.usda", "/P.z", None, "scene.usda"),
    ("copy_child_spec", "copy_spec", "scene.usda", "/P/A", "/Q/A", "scene.usda"),
    ("copy_ordered_spec", "copy_spec", "scene.usda", "/P", "/P3", "scene.usda"),
]

# Calls no layerstack API makes, recorded for contrast: (name, call, scene,
# path, destination). `set_name` is `SdfPrimSpec::SetName` or
# `SdfPropertySpec::SetName` to the destination's name; `layer_apply` is
# `SdfLayer::Apply` of one `SdfBatchNamespaceEdit` on the scene's root
# layer.
UNREPLAYED = [
    ("prim", "set_name", "scene.usda", "/P/A", "/P/A2"),
    ("prim_onto_ordered_name", "set_name", "scene.usda", "/P/A", "/P/Gone"),
    ("root_prim", "set_name", "scene.usda", "/P", "/P2"),
    ("property", "set_name", "scene.usda", "/P.y", "/P.y2"),
    ("variant_prim", "set_name", "variant.usda", "/V{v=x}F", "/V{v=x}F2"),
    ("variant_prim_apply", "layer_apply", "variant.usda", "/V{v=x}F", "/V{v=x}F2"),
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


def record(directory, scene, stage, applied):
    return {
        "applied": applied,
        "layers": {name: layer_record(layer_named(directory, name))
                   for name in SCENES[scene]},
        "text": {name: layer_named(directory, name).ExportToString()
                 for name in SCENES[scene]},
        "composed": composed_record(stage),
    }


def cases_record(directory):
    out = []
    for name, call, target, path, destination, scene in CASES:
        reload_layers()
        stage = Usd.Stage.Open(os.path.join(directory, scene))
        applied = apply(directory, stage, call, target, path, destination)
        case = {"name": name, "scene": scene, "call": call, "target": target,
                "path": path, "destination": destination}
        case.update(record(directory, scene, stage, applied))
        out.append(case)
    reload_layers()
    return out


def unreplayed_record(directory):
    out = []
    for name, call, scene, path, destination in UNREPLAYED:
        reload_layers()
        stage = Usd.Stage.Open(os.path.join(directory, scene))
        layer = stage.GetRootLayer()
        if call == "set_name":
            layer.GetObjectAtPath(path).name = Sdf.Path(destination).name
            applied = True
        else:
            edit = Sdf.NamespaceEdit(path, destination, Sdf.NamespaceEdit.same)
            applied = bool(layer.Apply(Sdf.BatchNamespaceEdit([edit])))
        case = {"name": name, "call": call, "scene": scene, "path": path,
                "destination": destination}
        case.update(record(directory, scene, stage, applied))
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
        "unreplayed": unreplayed_record(out),
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
