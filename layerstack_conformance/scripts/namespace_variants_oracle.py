# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's namespace edits do on a stage that composes
variant sets, call by call.

Usage: namespace_variants_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/namespace_variants` by
default, the layers below (`scene.usda` over `sub.usda`, `/World/R`
referencing `asset.usda`) and `oracle.json`, which
`tests/conformance/namespace_variants.rs` replays against layerstack.

Each case opens the scene afresh and queues one `UsdNamespaceEditor`
move or delete of a prim or property, through the root layer, or (target
`variant`) through `/World/A`'s `look=red` variant edit target. It records
`CanApplyEdits` (its errors and warnings), and after `ApplyEdits` each
layer's `ExportToString`, then every composed prim (`TraverseAll`) with its
children, properties, relationship targets, attribute connections (when
any) and variant selections. It then selects other variants in the root layer
(`switch`: `look=blue` on `/World/A`, `v=off` on `/World/Host`,
`inner=y` on `/World/N`, `mode=two` on `/World/R`, each at the prim's
path after the edit, when it is still composed) and records the stage
again, so a replay sees what the edit left in the variants it did not
compose.

What the records show, with the 26.11 code that does it:

- The editor moves or deletes every spec at the edited path in the root
  layer stack as one `SdfBatchNamespaceEdit` per layer
  (`PcpGatherLayersToEditForSpecMove`, `_ApplyLayerSpecMove`,
  `pxr/usd/usd/namespaceEditor.cpp`): a prim's variant sets and every
  variant beneath them move or go with it, composed or not.
- Path-bearing fields (relationship targets, connections, path
  expressions) are fixed only for the composed objects that depend on the
  edited path, from the specs of their local and local-variant nodes in
  root-stack layers (`_DependencyCollector`, which visits
  `GetPseudoRoot().GetAllDescendants()` through `Usd_Resolver`, and
  `_GatherPathBearingFieldEditsForStage`). Inherits, specializes,
  references and payloads are fixed only where a composed prim index
  depends on the edited namespace (`PcpGatherDependentNamespaceEdits`).
  So specs in the selected variants are fixed and specs in variants no
  prim composes keep naming the old path, even inside the moved prim.
- A prim composed only from a variant of an ancestor, or with opinions
  in one, composes opinions across an ancestral arc
  (`_ProcessPrimEditRequiresRelocates`): the editor moves the local specs
  and authors a layer relocate (to `<>` for a delete) instead of editing
  the variant.
- A property with an opinion in a composed variant needs a relocate, which
  is never authored for properties (`_ProcessPropEditRequiresRelocates`):
  `CanApplyEdits` fails. A variant no prim composes contributes nothing.
- An edit target that maps paths across a variant arc is refused
  (`_GatherLayersToEdit`).

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
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "namespace_variants"))

LAYERS = {
    "scene.usda": '''#usda 1.0
(
    subLayers = [
        @sub.usda@
    ]
)

def "World"
{
    # A prim with a variant set whose selection `sub.usda` authors: `red`
    # is composed, `blue` is not.
    def "A" (
        prepend variantSets = "look"
    )
    {
        int a = 1
        rel r = </World/A/C>

        def "C"
        {
        }
        variantSet "look" = {
            "blue" {
                int a = 3
                rel toA = </World/A/C>
                rel toB = </World/B>

                def "D"
                {
                    rel toB = </World/B>
                }

            }
            "red" {
                int red = 1
                rel toB = </World/B>

                def "C"
                {
                    rel toA = </World/A>
                    rel toB = </World/B>
                }

            }
        }
    }

    # The prim the variants target, connect to and inherit.
    def "B"
    {
        int x = 1
        rel self = </World/B.x>

        def "Kid"
        {
        }
    }

    def "Group"
    {
    }

    # The variant set is `sub.usda`'s; the selection is authored here.
    def "Host" (
        variants = {
            string v = "on"
        }
    )
    {
    }

    # Nested variant sets: `outer=a` selects a variant that authors its
    # own set and selection, `inner=x`.
    def "N" (
        variants = {
            string outer = "a"
        }
        prepend variantSets = "outer"
    )
    {
        variantSet "outer" = {
            "a" (
                variants = {
                    string inner = "x"
                }
                prepend variantSets = "inner"
            ) {
                variantSet "inner" = {
                    "x" {
                        def "Deep"
                        {
                            rel toB = </World/B>
                            rel toN = </World/N/Deep>
                        }

                    }
                    "y" {
                        def "Deep"
                        {
                            rel toB = </World/B>
                        }

                    }
                }

            }
            "b" {
                def "Shallow"
                {
                    rel toB = </World/B>
                }

            }
        }
    }

    # Variants across a reference.
    def "R" (
        prepend references = @asset.usda@</Asset>
    )
    {
    }

    # `/World/A` across an internal reference, with the variant it does
    # not compose itself.
    def "Copy" (
        variants = {
            string look = "blue"
        }
        prepend references = </World/A>
    )
    {
    }
}
''',
    "sub.usda": '''#usda 1.0

over "World"
{
    over "A" (
        variants = {
            string look = "red"
        }
    )
    {
    }

    over "Host" (
        prepend variantSets = "v"
    )
    {
        variantSet "v" = {
            "off" {
                rel toB = </World/B>

                def "Dormant" (
                    prepend inherits = </World/B>
                )
                {
                    int c.connect = </World/B.x>
                    rel toB = </World/B>

                    over "Kid"
                    {
                        int k = 0
                    }
                }

            }
            "on" {
                rel toB = </World/B>

                def "Inner" (
                    prepend inherits = </World/B>
                )
                {
                    int c.connect = </World/B.x>
                    rel toB = </World/B>

                    over "Kid"
                    {
                        int k = 1
                    }
                }

            }
        }
    }
}
''',
    "asset.usda": '''#usda 1.0
(
    defaultPrim = "Asset"
)

def "Asset" (
    variants = {
        string mode = "one"
    }
    prepend variantSets = "mode"
)
{
    variantSet "mode" = {
        "one" {
            def "Part"
            {
                rel toPart = </Asset/Part>
            }

        }
        "two" {
            def "Other"
            {
            }

        }
    }
}
''',
}

# Every layer a record lists.
RECORDED = ["scene.usda", "sub.usda", "asset.usda"]

# (name, target, path, destination): `destination` is the new path of a
# move, `None` for a delete; `target` is `root` (the root layer) or
# `variant` (`/World/A`'s `look=red` variant).
CASES = [
    # (a) the edited prim has variant sets.
    ("rename_variant_host", "root", "/World/A", "/World/A2"),
    ("reparent_variant_host", "root", "/World/A", "/World/Group/A"),
    ("delete_variant_host", "root", "/World/A", None),
    ("rename_variant_set_parent", "root", "/World", "/Earth"),
    # (f) nested variants.
    ("rename_nested_variant_host", "root", "/World/N", "/World/N2"),
    # (b) the edited prim lives in a selected variant's namespace.
    ("rename_variant_child", "root", "/World/Host/Inner", "/World/Host/Inner2"),
    ("reparent_variant_child", "root", "/World/Host/Inner", "/World/Group/Inner"),
    ("delete_variant_child", "root", "/World/Host/Inner", None),
    ("rename_mixed_child", "root", "/World/A/C", "/World/A/C2"),
    ("delete_mixed_child", "root", "/World/A/C", None),
    ("rename_nested_variant_child", "root", "/World/N/Deep", "/World/N/Deep2"),
    ("rename_through_variant", "variant", "/World/A/C", "/World/A/C2"),
    # (d) specs in variants target, connect to and inherit the edited
    # path; (c) unselected variants hold specs that do too.
    ("rename_targeted", "root", "/World/B", "/World/B2"),
    ("reparent_targeted", "root", "/World/B", "/World/Group/B"),
    ("delete_targeted", "root", "/World/B", None),
    ("rename_inherited_child", "root", "/World/B/Kid", "/World/B/Kid2"),
    ("delete_inherited_child", "root", "/World/B/Kid", None),
    # (g) variants across a reference.
    ("rename_referencing", "root", "/World/R", "/World/R2"),
    ("rename_referenced_variant_child", "root", "/World/R/Part", "/World/R/Part2"),
    # Properties.
    ("rename_property_with_dormant_opinion", "root", "/World/A.a", "/World/A.a2"),
    ("delete_property_with_dormant_opinion", "root", "/World/A.a", None),
    ("rename_targeting_property", "root", "/World/A.r", "/World/A.r2"),
    ("rename_variant_property", "root", "/World/A.red", "/World/A.red2"),
    ("delete_variant_property", "root", "/World/A.red", None),
    ("rename_targeted_property", "root", "/World/B.x", "/World/B.x2"),
    ("reparent_targeted_property", "root", "/World/B.x", "/World/Group.x"),
    ("delete_targeted_property", "root", "/World/B.x", None),
    ("rename_variant_child_property", "root", "/World/Host/Inner.c", "/World/Host/Inner.c2"),
]

# The selections `switch` authors in the root layer: (prim, set, variant).
SWITCH = [
    ("/World/A", "look", "blue"),
    ("/World/Host", "v", "off"),
    ("/World/N", "inner", "y"),
    ("/World/R", "mode", "two"),
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
            "selections": dict(sorted(prim.GetVariantSets().GetAllVariantSelections().items())),
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


def edit(stage, target, path, destination):
    """Queues the case's edit; returns the editor."""
    path = Sdf.Path(path)
    if target == "variant":
        prim = stage.GetPrimAtPath("/World/A")
        stage.SetEditTarget(prim.GetVariantSet("look").GetVariantEditTarget())
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


def moved(path, source, destination):
    """`path` after the case's edit: `None` once deleted."""
    path, source = Sdf.Path(path), Sdf.Path(source)
    if not source.IsPrimPath() or not path.HasPrefix(source):
        return path
    if destination is None:
        return None
    return path.ReplacePrefix(source, Sdf.Path(destination))


def switch(stage, source, destination):
    stage.SetEditTarget(Usd.EditTarget(stage.GetRootLayer()))
    for path, set_name, variant in SWITCH:
        path = moved(path, source, destination)
        prim = stage.GetPrimAtPath(path) if path else None
        if prim:
            assert prim.GetVariantSet(set_name).SetVariantSelection(variant)


def anonymous(message, directory):
    """`message` without the directory it was written in or the addresses
    of anonymous layers."""
    for prefix in {directory, os.path.realpath(directory)}:
        message = message.replace(prefix + os.sep, "")
    return re.sub(r"anon:0x[0-9a-f]+:", "anon:", message)


def cases_record(directory):
    out = []
    for name, target, path, destination in CASES:
        reload_layers()
        stage = Usd.Stage.Open(os.path.join(directory, "scene.usda"))
        editor = edit(stage, target, path, destination)
        result = editor.CanApplyEdits()
        applied = bool(result) and bool(editor.ApplyEdits())
        case = {
            "name": name, "target": target, "path": path, "destination": destination,
            "can_apply": bool(result),
            "errors": [anonymous(e, directory) for e in result.errors],
            "warnings": [anonymous(w, directory) for w in result.warnings],
            "applied": applied,
            "layers": layers_record(directory),
            "composed": composed_record(stage),
        }
        switch(stage, path, destination)
        case["switched"] = composed_record(stage)
        out.append(case)
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
