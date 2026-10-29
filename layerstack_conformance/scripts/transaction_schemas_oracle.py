# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD authors schema properties after the prim's type or
applied schemas change, call by call.

Usage: transaction_schemas_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/transaction_schemas` by
default, the layers below as OpenUSD exports them and `oracle.json`, which
`tests/conformance/transaction_schemas.rs` replays against layerstack's
`Transaction`, every call of a case in one transaction.

OpenUSD recomposes after each call, so a call that sets a value of a
property no spec declares (`UsdAttribute::Set`, `UsdRelationship::AddTarget`)
creates its spec from the prim definition the earlier calls left:
`UsdStage::_CreateAttributeSpecForEditing` stamps the definition's type and
variability, and refuses (a coding error, nothing authored) when nothing
defines the attribute and no spec declares it. A value that does not
conform to the declared type is refused too (`UsdStage::_SetValue`). The
prim definition comes from the composed `typeName` and `apiSchemas`
(`UsdPrimDefinition`, `Usd_PrimTypeInfoCache`), so an opinion authored in a
weaker site than the strongest one does not change it.

Every case starts from fresh copies of `scene.usda` (the root layer),
`weak.usda`, its sublayer, and `asset.usda`, which it references, sets the
edit target the case names, and makes its calls in order. For each call the
oracle records whether OpenUSD accepted it (no error raised, and a true
result where the call returns one), the exported text of every layer after
it, the prim's composed type name and applied schemas, and for a value or
target call the property as OpenUSD reads it back: its type name and
variability, and its value or targets. `before` is each layer's text before
any call.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "transaction_schemas"))

SCENE = '''#usda 1.0
(
    fallbackPrimTypes = {
        token[] RoundThing = ["Sphere"]
    }
    subLayers = [
        @weak.usda@
    ]
)

def Cube "Cube"
{
}

def "Ref" (
    prepend references = @asset.usda@</Asset>
)
{
}

def Cube "Strong" (
    prepend references = @asset.usda@</Other>
)
{
}

def "Var" (
    variants = {
        string shape = "round"
    }
    prepend variantSets = "shape"
)
{
    variantSet "shape" = {
        "round" {

        }
    }
}
'''

WEAK = '''#usda 1.0

def Cube "Weak"
{
}
'''

ASSET = '''#usda 1.0

def Xform "Asset"
{
}

def Xform "Other"
{
}
'''

LAYERS = {"scene.usda": SCENE, "weak.usda": WEAK, "asset.usda": ASSET}

# (name, edit target, calls). A target is a layer name, `ref:<prim>` for the
# reference node of that prim (in asset.usda) or `variant:<prim>` for the
# selected variant of its `shape` set. A call is one of
#   ("define", prim, type)           UsdStage::DefinePrim
#   ("set_type", prim, type)         UsdPrim::SetTypeName
#   ("clear_type", prim)             UsdPrim::ClearTypeName
#   ("apply", prim, schema)          UsdPrim::AddAppliedSchema
#   ("unapply", prim, schema)        UsdPrim::RemoveAppliedSchema
#   ("set", property, value)         UsdAttribute::Set at the default time
#   ("sample", property, time, value) UsdAttribute::Set at a time
#   ("add_target", property, path)   UsdRelationship::AddTarget
# and a value is {type: value} with type `double`, `float`, `token` or
# `string`.
CASES = [
    ("define_then_set", "scene.usda", [
        ("define", "/New", "Sphere"),
        ("set", "/New.radius", {"double": 2.0}),
        ("sample", "/New.radius", 1.0, {"double": 3.0}),
    ]),
    ("define_fallback_type_then_set", "scene.usda", [
        ("define", "/New", "RoundThing"),
        ("set", "/New.radius", {"double": 2.0}),
    ]),
    ("retype_then_set", "scene.usda", [
        ("set_type", "/Cube", "Sphere"),
        ("set", "/Cube.radius", {"double": 3.0}),
    ]),
    ("retype_then_wrong_value", "scene.usda", [
        ("set_type", "/Cube", "Sphere"),
        ("set", "/Cube.radius", {"string": "wide"}),
    ]),
    ("define_then_undeclared", "scene.usda", [
        ("define", "/New", "Sphere"),
        ("set", "/New.bogus", {"double": 1.0}),
    ]),
    ("apply_then_set", "scene.usda", [
        ("apply", "/Cube", "MotionAPI"),
        ("set", "/Cube.motion:blurScale", {"float": 0.5}),
        ("apply", "/Cube", "CollectionAPI:look"),
        ("set", "/Cube.collection:look:expansionRule", {"token": "expandPrims"}),
    ]),
    ("apply_then_target", "scene.usda", [
        ("apply", "/Cube", "MaterialBindingAPI"),
        ("add_target", "/Cube.material:binding", "/Looks/Red"),
    ]),
    ("apply_then_schema_target", "scene.usda", [
        ("apply", "/Cube", "CollectionAPI:look"),
        ("add_target", "/Cube.collection:look:includes", "/Ref"),
    ]),
    ("define_then_schema_target", "scene.usda", [
        ("define", "/New", "Xform"),
        ("add_target", "/New.proxyPrim", "/Cube"),
    ]),
    ("target_without_api", "scene.usda", [
        ("add_target", "/Cube.material:binding", "/Looks/Red"),
    ]),
    ("unapply_then_set", "scene.usda", [
        ("apply", "/Cube", "MotionAPI"),
        ("unapply", "/Cube", "MotionAPI"),
        ("set", "/Cube.motion:blurScale", {"float": 0.5}),
    ]),
    ("clear_type_then_set", "scene.usda", [
        ("clear_type", "/Cube"),
        ("set", "/Cube.size", {"double": 1.0}),
    ]),
    ("retype_keeps_authored", "scene.usda", [
        ("set", "/Cube.size", {"double": 2.0}),
        ("set_type", "/Cube", "Sphere"),
        ("set", "/Cube.size", {"double": 3.0}),
        ("set", "/Cube.radius", {"double": 1.0}),
        ("set_type", "/Cube", "Cube"),
        ("set", "/Cube.radius", {"double": 4.0}),
    ]),
    ("retype_over_sublayer", "scene.usda", [
        ("set_type", "/Weak", "Sphere"),
        ("set", "/Weak.radius", {"double": 2.0}),
    ]),
    ("weaker_type_is_masked", "weak.usda", [
        ("set_type", "/Cube", "Sphere"),
        ("set", "/Cube.radius", {"double": 2.0}),
        ("set", "/Cube.size", {"double": 2.0}),
    ]),
    ("reference_target", "ref:/Ref", [
        ("set_type", "/Ref", "Sphere"),
        ("set", "/Ref.radius", {"double": 2.0}),
    ]),
    ("reference_type_is_masked", "ref:/Strong", [
        ("set_type", "/Strong", "Sphere"),
        ("set", "/Strong.radius", {"double": 2.0}),
        ("set", "/Strong.size", {"double": 2.0}),
    ]),
    ("variant_api", "variant:/Var", [
        ("apply", "/Var", "MotionAPI"),
        ("set", "/Var.motion:blurScale", {"float": 0.5}),
    ]),
    # A variant's prim spec holds a type name, which USDA cannot write.
    ("variant_type", "variant:/Var", [
        ("set_type", "/Var", "Sphere"),
        ("set", "/Var.radius", {"double": 2.0}),
    ]),
]


def plain(value):
    if isinstance(value, float):
        return value
    if isinstance(value, (list, tuple)):
        return [plain(v) for v in value]
    if value is None:
        return None
    return str(value) if not isinstance(value, (int, bool)) else value


def call_value(value):
    ((_, v),) = value.items()
    return v


def accept(result):
    return result is None or bool(result)


def run(stage, call):
    """Makes `call`, returning whether OpenUSD accepted it."""
    kind, path = call[0], call[1]
    try:
        if kind == "define":
            return bool(stage.DefinePrim(path, call[2]))
        prim_path = Sdf.Path(path).GetPrimPath()
        prim = stage.GetPrimAtPath(prim_path)
        name = Sdf.Path(path).name
        if kind == "set_type":
            return accept(prim.SetTypeName(call[2]))
        if kind == "clear_type":
            return accept(prim.ClearTypeName())
        if kind == "apply":
            return accept(prim.AddAppliedSchema(call[2]))
        if kind == "unapply":
            return accept(prim.RemoveAppliedSchema(call[2]))
        if kind == "set":
            return accept(prim.GetAttribute(name).Set(call_value(call[2])))
        if kind == "sample":
            return accept(prim.GetAttribute(name).Set(call_value(call[3]), call[2]))
        if kind == "add_target":
            return accept(prim.GetRelationship(name).AddTarget(call[2]))
    except Exception:
        return False
    raise ValueError(call)


def read_back(stage, call):
    """What OpenUSD reads back of the prim and property `call` edits."""
    path = Sdf.Path(call[1])
    prim = stage.GetPrimAtPath(path.GetPrimPath())
    record = {
        "type_name": str(prim.GetTypeName()) if prim else None,
        "applied": [str(s) for s in prim.GetAppliedSchemas()] if prim else [],
    }
    if call[0] in ("set", "sample"):
        attr = prim.GetAttribute(path.name)
        record["attribute"] = None if not attr else {
            "type": str(attr.GetTypeName()),
            "variability": "uniform" if attr.GetVariability() == Sdf.VariabilityUniform
            else "varying",
            "custom": attr.IsCustom(),
            "value": plain(attr.Get(call[2] if call[0] == "sample" else
                                    Usd.TimeCode.Default())),
        }
    if call[0] == "add_target":
        rel = prim.GetRelationship(path.name)
        record["relationship"] = None if not rel else {
            "custom": rel.IsCustom(),
            "targets": [str(t) for t in rel.GetTargets()],
        }
    return record


def edit_target(stage, out, target):
    if target.startswith("ref:"):
        prim = stage.GetPrimAtPath(target[4:])
        node = prim.GetPrimIndex().rootNode.children[0]
        return Usd.EditTarget(Sdf.Layer.Find(os.path.join(out, "asset.usda")), node)
    if target.startswith("variant:"):
        prim = stage.GetPrimAtPath(target[8:])
        return prim.GetVariantSets().GetVariantSet("shape").GetVariantEditTarget()
    return Usd.EditTarget(Sdf.Layer.Find(os.path.join(out, target)))


def reload_layers(out):
    for name in LAYERS:
        layer = Sdf.Layer.Find(os.path.join(out, name))
        if layer:
            layer.Reload(force=True)


def texts(out):
    return {name: Sdf.Layer.FindOrOpen(os.path.join(out, name)).ExportToString()
            for name in LAYERS}


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    reload_layers(out)
    before = texts(out)
    cases = []
    for name, target, calls in CASES:
        reload_layers(out)
        stage = Usd.Stage.Open(os.path.join(out, "scene.usda"))
        stage.SetEditTarget(edit_target(stage, out, target))
        records = []
        for call in calls:
            accepted = run(stage, call)
            record = {"call": list(call), "accepted": accepted, "texts": texts(out)}
            record.update(read_back(stage, call))
            records.append(record)
        cases.append({"name": name, "target": target, "calls": records})
    reload_layers(out)
    _, minor, patch = Usd.GetVersion()
    oracle = {"openusd_version": f"{minor}.{patch}", "before": before, "cases": cases}
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
