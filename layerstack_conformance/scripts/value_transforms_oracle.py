# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD transforms time codes, durations and path
expressions between layers and the stage, reading and authoring.

Usage: value_transforms_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/value_transforms` by
default, the layers below and `oracle.json`, which
`tests/conformance/value_transforms.rs` replays against layerstack.

The scene: `/Root` references `ref.usda</Ref>` with `(offset = 10;
scale = 2)`, and `ref.usda` selects a variant of `/Ref`; `/Inst`
inherits `/Class`; `/Rock` selects a local variant; `/Sub` is authored
in `sub.usda`, a sublayer with `(offset = 4; scale = 0.5)`. Each source
spec authors the same values: `timecode`, `timecode[]` and `duration`
defaults, `timecode` and `pathExpression` time samples,
`pathExpression` and `pathExpression[]` defaults (absolute, relative and
outside the arc), and `customData` on the prim and on an attribute,
holding the same kinds in nested dictionaries.

It records:

- `read`: what the stage reads on each prim (`GetCustomData`, and each
  attribute's `Get()`, `GetTimeSamples` and `Get(t)` at each sample, and
  `GetCustomData`). OpenUSD maps each value from its layer into the stage
  as it reads it (`_FieldValueToStageXf` in `pxr/usd/usd/stage.cpp`,
  through `VtValueTryTransform`, which visits arrays and dictionaries):
  a time code through the layer offset to the stage, a duration by its
  scale, and a path expression through the node's map to the root
  (`_PathExprToStage`, `PcpMapFunction::MapSourceToTarget`).
- `flatten`: `UsdStage::Flatten`'s `ExportToString`, without its `doc`.
- `cases`: for each edit target, the authoring calls below through it,
  whether each was accepted, the target layer's `ExportToString` after
  them, and what the stage then reads on the prim. OpenUSD maps each
  authored value from the stage into the target layer
  (`_StageValueToFieldXf`, applied by `UsdStage::_SetValue` and
  `_SetMetadataImpl`, which for a dictionary key maps only the value it
  sets): a time code and a duration through the inverse of the target's
  time offset, a path expression made absolute at the prim and mapped
  with `PcpMapFunction::MapTargetToSource` (`_PathExprToField`). Asset
  paths are written as given.

`--check` rewrites everything into a temporary directory and fails unless
the layers and `oracle.json` match the checked-in fixture.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Gf, Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "value_transforms"))


def values(prim, indent):
    """The attributes every source spec authors, for a spec at `prim`."""
    pad = " " * indent
    lines = [
        "custom duration du = 4",
        "custom pathExpression pe = \"%s/Child\"" % prim,
        "custom pathExpression[] pea = [\"%s/Child\", \"Child\", \"/Elsewhere + //Any\"]" % prim,
        "custom pathExpression pes.timeSamples = {",
        "    1: \"%s/Child\"," % prim,
        "}",
        "custom double plain = 1 (",
        "    customData = {",
        "        pathExpression e = \"%s/Child\"" % prim,
        "        dictionary n = {",
        "            timecode t = 5",
        "        }",
        "        timecode t = 5",
        "    }",
        ")",
        "custom pathExpression rpe = \"Child\"",
        "custom timecode tc = 5",
        "custom timecode[] tca = [1, 2]",
        "custom timecode tcs.timeSamples = {",
        "    1: 5,",
        "    2: 6,",
        "}",
    ]
    return "\n".join(pad + line for line in lines)


def custom_data(prim, indent):
    """The prim `customData` every source spec authors."""
    pad = " " * indent
    lines = [
        "customData = {",
        "    dictionary d = {",
        "        timecode deep = 5",
        "        dictionary deeper = {",
        "            pathExpression e = \"%s/Child\"" % prim,
        "            timecode[] ts = [1, 2]",
        "        }",
        "    }",
        "    duration du = 4",
        "    pathExpression e = \"%s/Child\"" % prim,
        "    pathExpression out = \"/Elsewhere + //Any\"",
        "    pathExpression rel = \"Child\"",
        "    timecode t = 5",
        "    timecode[] ts = [1, 2]",
        "}",
    ]
    return "\n".join(pad + line for line in lines)


LAYERS = {
    "scene.usda": '''#usda 1.4
(
    subLayers = [
        @sub.usda@ (offset = 4; scale = 0.5)
    ]
)

def "Root" (
    references = @ref.usda@</Ref> (offset = 10; scale = 2)
)
{
}

class "Class" (
%(class_data)s
)
{
%(class_values)s

    def "Child"
    {
    }
}

def "Inst" (
    inherits = </Class>
)
{
}

def "Rock" (
    variants = {
        string look = "a"
    }
    prepend variantSets = "look"
)
{
    def "Child"
    {
    }
    variantSet "look" = {
        "a" (
%(rock_data)s
        ) {
%(rock_values)s

        }
    }
}
''' % {
        "class_data": custom_data("/Class", 4),
        "class_values": values("/Class", 4),
        "rock_data": custom_data("/Rock", 12),
        "rock_values": values("/Rock", 12),
    },
    "sub.usda": '''#usda 1.4

def "Sub" (
%(data)s
)
{
%(values)s

    def "Child"
    {
    }
}
''' % {"data": custom_data("/Sub", 4), "values": values("/Sub", 4)},
    "ref.usda": '''#usda 1.4

def "Ref" (
%(data)s
    variants = {
        string mode = "x"
    }
    prepend variantSets = "mode"
)
{
%(values)s

    def "Child"
    {
    }
    variantSet "mode" = {
        "x" {
            custom pathExpression vpe = "/Ref/Child"
            custom timecode vtc = 7

        }
    }
}
''' % {"data": custom_data("/Ref", 4), "values": values("/Ref", 4)},
}

PRIMS = ["/Root", "/Inst", "/Rock", "/Sub"]


def calls(prim, values="all"):
    """The authoring calls of a case on the stage prim `prim`: (call,
    attribute or field, key path or time, value). `values` limits them
    to those that author no path expression (`times`) or to those that
    do (`expressions`)."""
    expr = Sdf.PathExpression
    child = expr(prim + "/Child")
    out = [
        ("set", "tc", None, Sdf.TimeCode(30)),
        ("set", "tcs", 30.0, Sdf.TimeCode(30)),
        ("set", "tca", None, Sdf.TimeCodeArray([Sdf.TimeCode(30), Sdf.TimeCode(40)])),
        ("set", "du", None, Gf.Duration(8)),
        ("set", "pe", None, child),
        ("set", "rpe", None, expr("Child")),
        ("set", "pes", 30.0, child),
        ("set", "pea", None, Sdf.PathExpressionArray([child, expr("Child"),
                                                       expr("/Elsewhere + //Any")])),
        ("create", "npe", "pathExpression", expr("Child + /Elsewhere")),
        ("create", "ntc", "timecode", Sdf.TimeCode(30)),
        ("create", "nasset", "asset", Sdf.AssetPath("./tex.png")),
        ("prim_key", "customData", "t", Sdf.TimeCode(30)),
        ("prim_key", "customData", "d:deep", Sdf.TimeCode(30)),
        ("prim_key", "customData", "e", child),
        ("prim_key", "customData", "ts", Sdf.TimeCodeArray([Sdf.TimeCode(30)])),
        ("prim_key", "customData", "du", Gf.Duration(8)),
        ("prim_field", "assetInfo", None, {
            "t": Sdf.TimeCode(30),
            "n": {"e": child, "ts": Sdf.TimeCodeArray([Sdf.TimeCode(30)])},
        }),
        ("attr_key", "plain", "t", Sdf.TimeCode(30)),
        ("attr_key", "plain", "n:t", Sdf.TimeCode(30)),
        ("attr_key", "plain", "e", child),
        ("attr_field", "tc", "customData", {"e": child, "t": Sdf.TimeCode(30)}),
    ]
    if values == "all":
        return out
    return [c for c in out if holds_expression(c[3]) == (values == "expressions")]


def holds_expression(value):
    if isinstance(value, Sdf.PathExpression):
        return True
    if isinstance(value, dict):
        return any(holds_expression(v) for v in value.values())
    if isinstance(value, Sdf.PathExpressionArray):
        return True
    return False


TYPES = {
    "pathExpression": Sdf.ValueTypeNames.PathExpression,
    "timecode": Sdf.ValueTypeNames.TimeCode,
    "asset": Sdf.ValueTypeNames.Asset,
}

# (name, stage prim, edit target, values authored). Through an edit
# target that maps a prim into a variant, OpenUSD maps a path expression's
# prefixes with the variant selection in them (`/Rock{look=a}Child`, which
# `SdfPath` prints without a separator), unlike relationship targets,
# whose selections `UsdRelationship` strips; those cases are apart.
CASES = [
    ("root_layer", "/Root", "root", "all"),
    ("reference", "/Root", "reference", "all"),
    ("reference_variant", "/Root", "variant", "times"),
    ("reference_variant_expressions", "/Root", "variant", "expressions"),
    ("inherit", "/Inst", "inherit", "all"),
    ("variant", "/Rock", "variant", "times"),
    ("variant_expressions", "/Rock", "variant", "expressions"),
    ("local_direct_variant", "/Rock", "local_direct_variant", "times"),
    ("local_direct_variant_expressions", "/Rock", "local_direct_variant", "expressions"),
    ("sublayer_offset", "/Sub", "local_layer", "all"),
    ("sublayer_plain", "/Sub", "layer", "all"),
]


def encode(value):
    """`value` as JSON: kinds that a plain JSON value cannot tell apart
    are tagged."""
    if value is None:
        return None
    if isinstance(value, Gf.TimeCode) or type(value).__name__ == "TimeCode":
        return {"timecode": float(value)}
    if isinstance(value, Gf.Duration):
        return {"duration": float(value)}
    if isinstance(value, Sdf.PathExpression):
        return {"pathExpression": value.GetText()}
    if isinstance(value, Sdf.AssetPath):
        return {"asset": value.path}
    if isinstance(value, dict):
        return {"dictionary": {k: encode(v) for k, v in sorted(value.items())}}
    if isinstance(value, (bool, int, float, str)):
        return value
    return [encode(item) for item in value]


def prim_record(prim):
    attributes = {}
    for attr in prim.GetAttributes():
        times = attr.GetTimeSamples()
        attributes[attr.GetName()] = {
            "default": encode(attr.Get()),
            "samples": [[t, encode(attr.Get(t))] for t in times],
            "customData": encode(attr.GetCustomData()),
        }
    return {
        "customData": encode(prim.GetCustomData()),
        "assetInfo": encode(prim.GetAssetInfo()),
        "attributes": attributes,
    }


def reload_layers():
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)


def node_of(prim, arc):
    stack = [prim.GetPrimIndex().rootNode]
    while stack:
        node = stack.pop(0)
        if str(node.arcType).lower().endswith(arc):
            return node
        stack.extend(node.children)
    raise LookupError(f"{prim.GetPath()} has no {arc} node")


def edit_target(stage, directory, prim, kind):
    root = stage.GetRootLayer()
    if kind == "root":
        return Usd.EditTarget(root)
    if kind == "local_direct_variant":
        return Usd.EditTarget.ForLocalDirectVariant(root, Sdf.Path("/Rock{look=a}"))
    sub = Sdf.Layer.Find(os.path.join(directory, "sub.usda"))
    if kind == "local_layer":
        return stage.GetEditTargetForLocalLayer(sub)
    if kind == "layer":
        return Usd.EditTarget(sub)
    node = node_of(prim, kind)
    return Usd.EditTarget(node.layerStack.layerTree.layer, node)


def author(stage, prim, call):
    kind, name, key, value = call
    if kind == "set":
        attr = prim.GetAttribute(name)
        return attr.Set(value) if key is None else attr.Set(value, key)
    if kind == "create":
        attr = prim.CreateAttribute(name, TYPES[key], custom=True)
        return bool(attr) and attr.Set(value)
    if kind == "prim_key":
        return prim.SetMetadataByDictKey(name, key, value)
    if kind == "prim_field":
        return prim.SetMetadata(name, value)
    if kind == "attr_key":
        return prim.GetAttribute(name).SetMetadataByDictKey("customData", key, value)
    if kind == "attr_field":
        return prim.GetAttribute(name).SetMetadata(key, value)
    raise ValueError(kind)


def call_record(call):
    kind, name, key, value = call
    return {"call": kind, "name": name, "key": key, "value": encode(value)}


def cases_record(directory):
    out = []
    for name, prim_path, kind, authored in CASES:
        reload_layers()
        stage = Usd.Stage.Open(os.path.join(directory, "scene.usda"))
        prim = stage.GetPrimAtPath(prim_path)
        target = edit_target(stage, directory, prim, kind)
        stage.SetEditTarget(target)
        accepted = []
        for call in calls(prim_path, authored):
            try:
                accepted.append(bool(author(stage, prim, call)))
            except Exception:
                accepted.append(False)
        layer = target.GetLayer()
        out.append({
            "name": name,
            "prim": prim_path,
            "target": kind,
            "calls": [call_record(c) for c in calls(prim_path, authored)],
            "accepted": accepted,
            "layer": os.path.basename(layer.realPath),
            "text": layer.ExportToString(),
            "read": prim_record(prim),
        })
    reload_layers()
    return out


def flatten_record(stage):
    lines = stage.Flatten().ExportToString().split("\n")
    # The `doc` names the directory the layers were written in.
    at = next(i for i, line in enumerate(lines) if line.lstrip().startswith("doc = "))
    end = at
    while not lines[end].endswith('"""') or end == at and lines[end].count('"""') < 2:
        end += 1
    lines = lines[:at] + lines[end + 1:]
    # A layer with no other metadata keeps no metadata block.
    if lines[1:3] == ["(", ")"]:
        lines = lines[:1] + lines[3:]
    return "\n".join(lines)


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
        "read": {path: prim_record(stage.GetPrimAtPath(path)) for path in PRIMS},
        "flatten": flatten_record(stage),
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
