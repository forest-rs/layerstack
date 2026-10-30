# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records which authored paths OpenUSD's namespace edits leave naming the
edited object, call by call.

Usage: namespace_unrepaired_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/namespace_unrepaired` by
default, the layers below (`scene.usda`, whose `/World/Shelf` references
`asset.usda`) and `oracle.json`, which
`tests/conformance/namespace_unrepaired.rs` replays against layerstack.

`/World/Room` selects `light=on`; its `off` variant authors every kind of
path naming `/World/Lamp` (references, payloads, inherits, specializes,
relationship targets, attribute connections, path expressions in a
default and a time sample), as do composed specs beside it. `asset.usda`,
referenced by `/World/Shelf` and through a payload by `/World/Rack`,
names the lamp from an unselected variant of the composed `/Asset` and
from `/Unused`, a prim no arc reaches, and arcs back into the stage from
composed specs: `/Asset/Front` references the lamp and overs its bulb
and `intensity`, `/Asset/Base` (inherited by `/Asset/Side`) payloads the
bulb, and the selected variant's `/Asset{mode=plain}Under` references the
lamp and overs its bulb. `/World/Switch` is named only from composed
specs.

Each case opens the scene afresh, queues one `UsdNamespaceEditor` move or
delete through the root layer and applies it. It records each layer's
`ExportToString` after the edit and `unrepaired`: every path the edited
layers then author, in any spec, that names the edited object or
something beneath it (`SdfPath::HasPrefix`), as
`[layer, spec path, field, path]`. A path expression's paths are the
prefixes of its patterns and the paths of its expression references,
made absolute at their prim (`SdfPathExpression::Walk`). References and
payloads count when internal or into `scene.usda`, whose namespace the
edit changes. What it shows, with the 26.11 code that does it: the
editor repairs only the paths of composed specs (`_DependencyCollector`,
`_GatherPathBearingFieldEditsForStage` and
`PcpGatherDependentNamespaceEdits`, `pxr/usd/usd/namespaceEditor.cpp`),
so every recorded path is in a spec no composed prim uses, and the
specs of an unselected variant move with their prim. An arc back into
the stage from a composed spec of another layer is repaired there, and
the specs that compose over the edited namespace through it move or go
in every layer stack on the way up to the stage
(`_PrimIndexDependentNodeEditProcessor`,
`pxr/usd/pcp/dependentNamespaceEditUtils.cpp`).

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
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "namespace_unrepaired"))

LAYERS = {
    "scene.usda": '''#usda 1.0

def "World"
{
    # The prim the cases move and delete.
    def "Lamp"
    {
        float intensity = 1

        def "Bulb"
        {
        }
    }

    # Named only from composed specs.
    def "Switch"
    {
    }

    def "Group"
    {
    }

    # `off` is not selected: its specs compose nothing.
    def "Room" (
        variants = {
            string light = "on"
        }
        prepend variantSets = "light"
    )
    {
        rel switch = </World/Switch>

        variantSet "light" = {
            "off" (
                prepend inherits = </World/Lamp>
            ) {
                float glow.connect = </World/Lamp.intensity>
                rel lamp = </World/Lamp>
                pathExpression members = "/World/Lamp//* + ../Group"
                rel self = </World/Room>
                pathExpression timed.timeSamples = {
                    1: "%/World/Lamp/Bulb:bulbs",
                }

                def "Fixture" (
                    prepend payload = </World/Lamp/Bulb>
                    prepend references = </World/Lamp>
                    prepend specializes = </World/Lamp>
                )
                {
                    rel bulb = </World/Lamp/Bulb>
                }

            }
            "on" {
                rel lamp = </World/Lamp>
                rel switch = </World/Switch>

                def "Fixture" (
                    prepend references = </World/Lamp>
                )
                {
                    float glow.connect = </World/Lamp.intensity>
                }

            }
        }
    }

    def "Shelf" (
        prepend references = @asset.usda@</Asset>
    )
    {
        # Over the lamp `asset.usda`'s `/Asset/Front` references back.
        over "Front"
        {
            float intensity = 3

            over "Bulb"
            {
            }
        }
    }

    # The asset again, through a payload.
    def "Rack" (
        prepend payload = @asset.usda@</Asset>
    )
    {
    }
}
''',
    "asset.usda": '''#usda 1.0

def "Asset" (
    variants = {
        string mode = "plain"
    }
    prepend variantSets = "mode"
)
{
    # Composed through `/World/Shelf` and `/World/Rack`: the edit repairs
    # its arc back into the stage and moves its specs over the lamp.
    def "Front" (
        prepend references = @scene.usda@</World/Lamp>
    )
    {
        float intensity = 2

        over "Bulb"
        {
        }
    }

    # Reaches the bulb through an inherit.
    def "Side" (
        prepend inherits = </Asset/Base>
    )
    {
    }

    class "Base" (
        prepend payload = @scene.usda@</World/Lamp/Bulb>
    )
    {
    }

    variantSet "mode" = {
        "lit" {
            def "Back" (
                prepend references = @scene.usda@</World/Lamp>
            )
            {
            }

        }
        "plain" {
            def "Under" (
                prepend references = @scene.usda@</World/Lamp>
            )
            {
                over "Bulb"
                {
                }
            }

        }
    }
}

# No arc reaches it.
def "Unused" (
    prepend references = @scene.usda@</World/Lamp>
)
{
}
''',
}

# Every layer a record lists.
RECORDED = ["scene.usda", "asset.usda"]

# (name, path, destination): `destination` is the new path of a move,
# `None` for a delete.
CASES = [
    ("rename_lamp", "/World/Lamp", "/World/Lamp2"),
    ("reparent_lamp", "/World/Lamp", "/World/Group/Lamp"),
    ("delete_lamp", "/World/Lamp", None),
    ("rename_bulb", "/World/Lamp/Bulb", "/World/Lamp/Bulb2"),
    ("reparent_bulb", "/World/Lamp/Bulb", "/World/Group/Bulb"),
    ("delete_bulb", "/World/Lamp/Bulb", None),
    ("rename_property", "/World/Lamp.intensity", "/World/Lamp.power"),
    ("delete_property", "/World/Lamp.intensity", None),
    ("rename_room", "/World/Room", "/World/Room2"),
    ("rename_switch", "/World/Switch", "/World/Switch2"),
    ("delete_switch", "/World/Switch", None),
]


def reload_layers():
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)


def layer_named(directory, name):
    return Sdf.Layer.FindOrOpen(os.path.join(directory, name))


def layers_record(directory):
    return {name: layer_named(directory, name).ExportToString() for name in RECORDED}


def list_items(op):
    return (list(op.explicitItems) + list(op.prependedItems) + list(op.appendedItems)
            + list(op.addedItems) + list(op.deletedItems) + list(op.orderedItems))


def expression_paths(expression, anchor):
    out = []
    expression.MakeAbsolute(anchor).Walk(
        lambda op, index: None,
        lambda reference: out.append(reference.path),
        lambda pattern: out.append(pattern.GetPrefix()),
    )
    return [p for p in out if not p.isEmpty]


def value_paths(value, anchor):
    if isinstance(value, Sdf.PathExpression):
        return expression_paths(value, anchor)
    return []


def unrepaired(directory, source):
    """Every path the recorded layers author that names `source` or
    something beneath it, as `[layer, spec, field, path]`."""
    source = Sdf.Path(source)
    scene = layer_named(directory, "scene.usda")
    out = []
    for name in RECORDED:
        layer = layer_named(directory, name)

        def arc_target(arc):
            internal = not arc.assetPath
            into_scene = arc.assetPath and Sdf.Layer.FindOrOpenRelativeToLayer(
                layer, arc.assetPath) == scene
            return arc.primPath if internal or into_scene else None

        def visit(path):
            spec = layer.GetObjectAtPath(path)
            found = []
            if isinstance(spec, (Sdf.PrimSpec, Sdf.VariantSpec)):
                prim = spec if isinstance(spec, Sdf.PrimSpec) else spec.primSpec
                for field, op in (("references", prim.referenceList),
                                  ("payload", prim.payloadList)):
                    found += [(field, arc_target(arc)) for arc in list_items(op)]
                found += [("inheritPaths", p) for p in list_items(prim.inheritPathList)]
                found += [("specializes", p) for p in list_items(prim.specializesList)]
            elif isinstance(spec, Sdf.RelationshipSpec):
                found += [("targetPaths", p) for p in list_items(spec.targetPathList)]
            elif isinstance(spec, Sdf.AttributeSpec):
                found += [("connectionPaths", p) for p in list_items(spec.connectionPathList)]
                anchor = path.GetPrimPath().StripAllVariantSelections()
                if spec.HasDefaultValue():
                    found += [("default", p) for p in value_paths(spec.default, anchor)]
                for time in layer.ListTimeSamplesForPath(path):
                    value = layer.QueryTimeSample(path, time)
                    found += [("timeSamples", p) for p in value_paths(value, anchor)]
            for field, named in found:
                if named is not None and named.HasPrefix(source):
                    out.append([name, str(path), field, str(named)])

        layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    return sorted(out)


def cases_record(directory):
    out = []
    for name, path, destination in CASES:
        reload_layers()
        stage = Usd.Stage.Open(os.path.join(directory, "scene.usda"))
        editor = Usd.NamespaceEditor(stage)
        edited = Sdf.Path(path)
        if edited.IsPrimPath():
            if destination is None:
                assert editor.DeletePrimAtPath(edited)
            else:
                assert editor.MovePrimAtPath(edited, Sdf.Path(destination))
        elif destination is None:
            assert editor.DeletePropertyAtPath(edited)
        else:
            assert editor.MovePropertyAtPath(edited, Sdf.Path(destination))
        result = editor.CanApplyEdits()
        assert result, f"{name}: {result.errors}"
        assert editor.ApplyEdits(), name
        out.append({
            "name": name, "path": path, "destination": destination,
            "layers": layers_record(directory),
            "unrepaired": unrepaired(directory, path),
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
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "before": layers_record(out),
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
