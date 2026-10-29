# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD reports for instance proxies' composition, on a
stage and on a stage referencing it, before and after each of a run of
edits.

Usage: instance_proxies_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/instance_proxies` by
default, the layers below (`scene.usda`, whose instances reference its
own `/Proto` and `asset.usda`'s `/Asset`, and `dependent.usda`, whose
`/Use` references `scene.usda`'s `/Group`) and `oracle.json`, which
`tests/conformance/instance_proxies.rs` replays against layerstack.

Both stages are opened once and kept open while `STEPS` edit
`scene.usda` through the scene stage, one after another: moves and
deletes through `UsdNamespaceEditor`, the rest as `Sdf` edits. After each
step, every prim of each stage (`TraverseInstanceProxies` of every prim,
prototypes left out) is recorded: whether it is an instance or an instance
proxy, `UsdPrim::GetPrimStack`, each property's
`UsdProperty::GetPropertyStack`, each attribute's value, and the nodes of
`UsdPrim::GetPrimIndex` in strength order (arc, layer stack, site, and
whether the node is inert). For an instance proxy it also records its
instance (the nearest instance at or above it) and the instance whose
prim index the prototype was composed from (`source`,
`UsdPrim::_GetSourcePrimIndex` of the prototype). Each stage is then
opened afresh, and `fresh_differs` lists the prims whose prim index the
open stage reports otherwise than the new one.

What the records show, with the 26.11 code that does it:

- An instance proxy's `GetPrimIndex` is the prim index of its prim in the
  prototype, which is the source instance's (`Usd_PrimData::GetPrimIndex`,
  `pxr/usd/usd/primData.cpp`): its root node names the source instance,
  not the proxy's own, and a nested proxy reports the nested prototype's
  source, through whatever arcs that source was reached by.
- Which instance is the source is the first one registered, and another
  is assigned when the source goes away
  (`Usd_InstanceCache::_CreateOrUpdatePrototypeForInstances`,
  `_RemoveInstances`, `pxr/usd/usd/instanceCache.cpp`, which notes the
  source "may change from run to run"): after edits the open stage keeps
  sources a new stage does not pick (`fresh_differs`).
- The nodes that name the instance are not instanceable, and beneath an
  instance they are inert (`Pcp_DisableNonInstanceableNodesVisitor`,
  `pxr/usd/pcp/primIndex.cpp`): prim stacks, property stacks and values
  do not depend on which instance is the source, and an open stage and a
  new one agree on them.

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
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "instance_proxies"))

LAYERS = {
    "asset.usda": '''#usda 1.0

def "Asset"
{
    def "Leaf"
    {
        double size = 1

        def "Tip"
        {
        }
    }
}
''',
    "scene.usda": '''#usda 1.0

# The prototype source: references the asset, and holds a nested instance
# of it.
def "Proto" (
    prepend references = @asset.usda@</Asset>
)
{
    def "Part"
    {
        double size = 2
    }

    def "Inner" (
        instanceable = true
        prepend references = @asset.usda@</Asset>
    )
    {
    }
}

# Instances sharing a prototype.
def "Group"
{
    def "I1" (
        instanceable = true
        prepend references = </Proto>
    )
    {
    }

    def "I2" (
        instanceable = true
        prepend references = </Proto>
    )
    {
    }
}

# An instance of the asset alone, sharing the nested instances' prototype.
def "Solo" (
    instanceable = true
    prepend references = @asset.usda@</Asset>
)
{
}
''',
    "dependent.usda": '''#usda 1.0

# The scene's instances through a reference into its layer stack.
def "Use" (
    prepend references = @scene.usda@</Group>
)
{
}
''',
}

STAGES = ["scene.usda", "dependent.usda"]

# (name, edit, path, argument), applied in order to `scene.usda`.
STEPS = [
    ("rename_instance", "move", "/Group/I1", "/Group/Moved"),
    ("author_prototype_value", "size", "/Proto/Leaf", 3.0),
    ("add_first_instance", "instance", "/Group/First", "/Proto"),
    ("uninstance", "instanceable", "/Group/I2", False),
    ("deactivate_nested_source", "active", "/Proto/Inner", False),
    ("delete_instance", "delete", "/Group/Moved", None),
]

ARCS = {
    "ArcTypeRoot": "root",
    "ArcTypeInherit": "inherit",
    "ArcTypeVariant": "variant",
    "ArcTypeRelocate": "relocate",
    "ArcTypeReference": "reference",
    "ArcTypePayload": "payload",
    "ArcTypeSpecialize": "specialize",
}


def layer_name(layer):
    return os.path.basename(layer.identifier)


def graph_record(index):
    if not index.IsValid():
        return []
    out = []

    def walk(node):
        out.append({
            "arc": ARCS[str(node.arcType).split(".")[-1]],
            "layer": layer_name(node.layerStack.identifier.rootLayer),
            "site": str(node.path),
            "inert": node.isInert,
        })
        for child in node.children:
            walk(child)

    walk(index.rootNode)
    return out


def specs_record(specs):
    return [[layer_name(spec.layer), str(spec.path)] for spec in specs]


def instance_of(prim):
    while not prim.IsInstance():
        prim = prim.GetParent()
    return prim


def stage_record(stage):
    out = {}
    predicate = Usd.TraverseInstanceProxies(Usd.PrimAllPrimsPredicate)
    for prim in Usd.PrimRange.Stage(stage, predicate):
        record = {
            "kind": "proxy" if prim.IsInstanceProxy() else
                    "instance" if prim.IsInstance() else "prim",
            "prim_stack": specs_record(prim.GetPrimStack()),
            "properties": {
                prop.GetName(): specs_record(prop.GetPropertyStack())
                for prop in prim.GetProperties()
            },
            "values": {attr.GetName(): attr.Get() for attr in prim.GetAttributes()},
            "index": graph_record(prim.GetPrimIndex()),
        }
        if prim.IsInstanceProxy():
            instance = instance_of(prim.GetParent())
            record["instance"] = str(instance.GetPath())
            source = instance.GetPrototype()._GetSourcePrimIndex()
            record["source"] = str(source.rootNode.path)
        out[str(prim.GetPath())] = record
    return out


def edit(stage, layer, kind, path, argument):
    if kind in ("move", "delete"):
        editor = Usd.NamespaceEditor(stage)
        if kind == "move":
            assert editor.MovePrimAtPath(Sdf.Path(path), Sdf.Path(argument))
        else:
            assert editor.DeletePrimAtPath(Sdf.Path(path))
        assert editor.CanApplyEdits()
        assert editor.ApplyEdits()
        return
    with Sdf.ChangeBlock():
        if kind == "size":
            spec = Sdf.CreatePrimInLayer(layer, path)
            attr = Sdf.AttributeSpec(spec, "size", Sdf.ValueTypeNames.Double)
            attr.default = argument
        elif kind == "instance":
            spec = Sdf.CreatePrimInLayer(layer, path)
            spec.specifier = Sdf.SpecifierDef
            spec.instanceable = True
            spec.referenceList.Prepend(Sdf.Reference(primPath=argument))
            parent = spec.nameParent
            parent.primOrder = [spec.name] + [c.name for c in parent.nameChildren
                                              if c.name != spec.name]
        elif kind == "instanceable":
            layer.GetPrimAtPath(path).instanceable = argument
        elif kind == "active":
            layer.GetPrimAtPath(path).active = argument
        else:
            raise SystemExit(f"no edit {kind}")


def differing_indexes(open_record, fresh_record):
    """The prims whose prim index an open stage reports otherwise than a new
    one, after checking that they agree on everything else."""
    assert sorted(open_record) == sorted(fresh_record)
    differs = []
    for path, record in open_record.items():
        fresh = fresh_record[path]
        for key in ("kind", "prim_stack", "properties", "values"):
            assert record[key] == fresh[key], (path, key)
        if record["index"] != fresh["index"]:
            differs.append(path)
    return differs


def reload_layers():
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)


def records(directory, stages):
    out = {}
    for name, stage in zip(STAGES, stages):
        record = stage_record(stage)
        fresh = Usd.Stage.Open(os.path.join(directory, name))
        out[name] = {
            "prims": record,
            "fresh_differs": differing_indexes(record, stage_record(fresh)),
        }
    return out


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    reload_layers()
    stages = [Usd.Stage.Open(os.path.join(out, name)) for name in STAGES]
    for stage in stages:
        errors = stage.GetCompositionErrors()
        if errors:
            raise SystemExit(f"unexpected composition errors: {errors}")
    layer = stages[0].GetRootLayer()
    steps = [{"name": "before", "stages": records(out, stages)}]
    for name, kind, path, argument in STEPS:
        edit(stages[0], layer, kind, path, argument)
        steps.append({
            "name": name,
            "edit": kind,
            "path": path,
            "argument": argument,
            "stages": records(out, stages),
        })
    _, minor, patch = Usd.GetVersion()
    oracle = {"openusd_version": f"{minor}.{patch}", "steps": steps}
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")
    reload_layers()


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
