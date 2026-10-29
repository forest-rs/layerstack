# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's namespace edits do to relational attribute and
mapper targets (`</A.b[/C].d>`, `</A.x.mapper[/C.y]>`), call by call.

Usage: namespace_brackets_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/namespace_brackets` by
default, the layers below (`scene.usda` over `sub.usda`, `/World/R`
referencing `asset.usda`) and `oracle.json`, which
`tests/conformance/namespace_brackets.rs` replays against layerstack.

The scene's relationships and connections name the edited prims and
properties before the brackets (the owning prim or property of a
relational attribute or mapper), inside them, and as the relational
attribute's name after them, in explicit and edit list ops, in a
relationship's `default`, in a selected and an unselected variant and
across a reference. Each case opens the scene afresh and queues one
`UsdNamespaceEditor` move or delete of a prim or property through the
root layer. It records `CanApplyEdits` (its errors and warnings), and
after `ApplyEdits` each layer's `ExportToString` and every composed prim
(`TraverseAll`) with its children, properties, relationship targets and
attribute connections (when any).

What the records show, with the 26.11 code that does it:

- A relationship's or connection's list op item is edited only when the
  edited path is its prefix (`_PathListOpNamespaceEdit`,
  `pxr/usd/usd/namespaceEditor.cpp`, which tests `SdfPath::HasPrefix`):
  the prim part, or the property before the brackets, but never a path
  inside them or the relational attribute's name after them. Such an item
  is replaced by `SdfPath::ReplacePrefix(old, new)`, which by default
  also fixes the paths in its brackets (`fixTargetPaths`), or dropped
  when the edit is a delete.
- So a path that names the edited prim or property only inside the
  brackets keeps naming it, and is kept after a delete.
- Which composed objects are fixed at all is decided by the paths their
  fields hold (`_DependencyCollector`), looked up in an `SdfPathTable` by
  the edited path's subtree, which holds a bracketed path beneath its
  owning property's path and not beneath the paths in its brackets. A
  relationship's `default`, an `SdfPath`, is fixed by `ReplacePrefix`
  without a prefix test (`_PathReplacePrefix`), but only when one of the
  relationship's paths is affected, in any layer.
- Path expressions cannot hold bracketed paths (`SdfPathExpression`
  refuses `[`).

`--check` rewrites everything into a temporary directory and fails unless
the layers and `oracle.json` match the checked-in fixture.
"""
import argparse
import filecmp
import json
import os
import re
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "namespace_brackets"))

LAYERS = {
    "scene.usda": '''#usda 1.0
(
    subLayers = [
        @sub.usda@
    ]
)

def "World"
{
    # The prim whose properties own relational attributes and mappers.
    def "A"
    {
        rel b = </World/C>
        int x = 1

        def "Kid"
        {
            rel b = </World/C>
        }
    }

    # The prim the brackets name.
    def "C"
    {
        rel b = </World/A>
        int y = 1
    }

    def "Group"
    {
    }

    def "User"
    {
        # The edited prim or property before the brackets.
        rel before = [</World/A.b[/World/C].d>, </World/A/Kid.b[/World/C].d>, </World/A.x.mapper[/World/C.y]>]
        # ... inside them.
        rel inside = [</World/C.b[/World/A].d>, </World/C.b[/World/A/Kid].d>, </World/C.y.mapper[/World/A.x]>, </World/C.b[/World/C.b[/World/A].d].e>]
        # ... both.
        rel both = [</World/A.b[/World/A/Kid].d>, </World/A.x.mapper[/World/A.x]>]
        # A relative path in the brackets, which the parser keeps as
        # written beneath an absolute path.
        rel relative = </World/A.b[../C].d>
        # The edited property's name as the relational attribute's.
        rel after = [</World/A.b[/World/C].x>, </World/C.b[/World/A].y>]
        # Edit list ops.
        delete rel edits = </World/C.b[/World/A].d>
        prepend rel edits = [</World/A.b[/World/C].d>, </World/C.b[/World/A].d>]
        append rel edits = </World/A.x.mapper[/World/C.y]>
        reorder rel edits = [</World/C.b[/World/A].d>, </World/A.b[/World/C].d>]
        # A relationship's default, beside a target that depends on the
        # edit, and one beside a target that does not.
        rel dependent = [</World/A/Kid>, </World/C.y>, </World/A.x>]
        rel dependent.default = </World/C.b[/World/A/Kid].d>
        rel independent = </World/Group>
        rel independent.default = </World/C.b[/World/A/Kid].d>
        # A default whose relationship's targets `sub.usda` authors.
        rel across.default = </World/C.b[/World/A/Kid].d>
        int conn.connect = [</World/A.b[/World/C].d>, </World/C.b[/World/A].d>, </World/A/Kid.b[/World/C].d>]
        delete int listed.connect = </World/A/Kid.b[/World/C].d>
        prepend int listed.connect = </World/A.b[/World/A/Kid].d>
        append int listed.connect = </World/C.b[/World/A].d>
        pathExpression expr = "/World/A/Kid /World/C.y /World/A.x"
    }

    # A selected variant and an unselected one.
    def "V" (
        variants = {
            string s = "on"
        }
        prepend variantSets = "s"
    )
    {
        variantSet "s" = {
            "off" {
                int c.connect = </World/A.b[/World/C].d>
                rel r = [</World/A.b[/World/C].d>, </World/C.b[/World/A].d>, </World/A/Kid>]

            }
            "on" {
                int c.connect = </World/A.b[/World/C].d>
                rel r = [</World/A.b[/World/C].d>, </World/C.b[/World/A].d>, </World/A/Kid>]

            }
        }
    }

    # Relational attribute and mapper targets across a reference.
    def "R" (
        prepend references = @asset.usda@</Asset>
    )
    {
        rel toR = [</World/R.b[/World/R/Part].d>, </World/R/Part.x.mapper[/World/A.x]>]
    }
}
''',
    "sub.usda": '''#usda 1.0

over "World"
{
    over "User"
    {
        rel across = </World/A/Kid>
        rel sub = [</World/A.b[/World/C].d>, </World/C.b[/World/A].d>]
    }
}
''',
    "asset.usda": '''#usda 1.0
(
    defaultPrim = "Asset"
)

def "Asset"
{
    int c.connect = </Asset/Part.b[/Asset].d>
    rel r = [</Asset.b[/Asset/Part].d>, </Asset/Part.b[/Asset].d>, </Asset/Part.x.mapper[/Asset/Part.y]>]

    def "Part"
    {
        int x = 1
        int y = 1
    }
}
''',
}

# Every layer a record lists.
RECORDED = ["scene.usda", "sub.usda", "asset.usda"]

# (name, path, destination): `destination` is the new path of a move,
# `None` for a delete.
CASES = [
    # The owning prim of relational attributes and mappers, and a prim in
    # their brackets.
    ("rename_owner", "/World/A", "/World/A2"),
    ("reparent_owner", "/World/A", "/World/Group/A"),
    ("delete_owner", "/World/A", None),
    ("rename_owner_child", "/World/A/Kid", "/World/A/Kid2"),
    ("delete_owner_child", "/World/A/Kid", None),
    ("rename_bracketed", "/World/C", "/World/C2"),
    ("reparent_bracketed", "/World/C", "/World/Group/C"),
    ("delete_bracketed", "/World/C", None),
    ("rename_ancestor", "/World", "/Earth"),
    # The owning property before the brackets, inside them, and a
    # property named as a relational attribute is.
    ("rename_owner_property", "/World/A.b", "/World/A.b2"),
    ("delete_owner_property", "/World/A.b", None),
    ("rename_mapper_property", "/World/A.x", "/World/A.x2"),
    ("reparent_mapper_property", "/World/A.x", "/World/Group.x"),
    ("delete_mapper_property", "/World/A.x", None),
    ("rename_bracketed_property", "/World/C.y", "/World/C.y2"),
    ("delete_bracketed_property", "/World/C.y", None),
    ("rename_bracketed_owner_property", "/World/C.b", "/World/C.b2"),
    # Across a reference.
    ("rename_referencing", "/World/R", "/World/R2"),
    ("rename_referenced_child", "/World/R/Part", "/World/R/Part2"),
    ("delete_referenced_child", "/World/R/Part", None),
    ("rename_referenced_property", "/World/R/Part.x", "/World/R/Part.x2"),
]


def composed_record(stage):
    out = {}
    for prim in stage.TraverseAll():
        targets = {}
        connections = {}
        for rel in prim.GetRelationships():
            targets[rel.GetName()] = [str(p) for p in rel.GetTargets()]
        for attr in prim.GetAttributes():
            if attr.GetConnections():
                connections[attr.GetName()] = [str(p) for p in attr.GetConnections()]
        out[str(prim.GetPath())] = {
            "children": [str(n) for n in prim.GetAllChildrenNames()],
            "properties": [str(n) for n in prim.GetPropertyNames()],
            "targets": targets,
            "connections": connections,
        }
    return out


def reload_layers():
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)


def layer_named(directory, name):
    return Sdf.Layer.FindOrOpen(os.path.join(directory, name))


def layers_record(directory):
    return {name: layer_named(directory, name).ExportToString() for name in RECORDED}


def edit(stage, path, destination):
    """Queues the case's edit; returns the editor."""
    path = Sdf.Path(path)
    editor = Usd.NamespaceEditor(stage)
    if path.IsPrimPath():
        if destination is None:
            assert editor.DeletePrimAtPath(path)
        else:
            assert editor.MovePrimAtPath(path, Sdf.Path(destination))
    elif destination is None:
        assert editor.DeletePropertyAtPath(path)
    else:
        assert editor.MovePropertyAtPath(path, Sdf.Path(destination))
    return editor


def anonymous(message, directory):
    """`message` without the directory it was written in or the addresses
    of anonymous layers."""
    for prefix in {directory, os.path.realpath(directory)}:
        message = message.replace(prefix + os.sep, "")
    return re.sub(r"anon:0x[0-9a-f]+:", "anon:", message)


def cases_record(directory):
    out = []
    for name, path, destination in CASES:
        reload_layers()
        stage = Usd.Stage.Open(os.path.join(directory, "scene.usda"))
        editor = edit(stage, path, destination)
        result = editor.CanApplyEdits()
        applied = bool(result) and bool(editor.ApplyEdits())
        out.append({
            "name": name, "path": path, "destination": destination,
            "can_apply": bool(result),
            "errors": [anonymous(e, directory) for e in result.errors],
            "warnings": [anonymous(w, directory) for w in result.warnings],
            "applied": applied,
            "layers": layers_record(directory),
            "composed": composed_record(stage),
        })
    reload_layers()
    return out


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    reload_layers()
    stage = Usd.Stage.Open(os.path.join(out, "scene.usda"))
    errors = stage.GetCompositionErrors()
    if errors:
        raise SystemExit(f"unexpected composition errors: {errors}")
    before = {"layers": layers_record(out), "composed": composed_record(stage)}
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "before": before,
        "cases": cases_record(out),
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
