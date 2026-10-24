# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records, layer by layer, how OpenUSD reads each authored relationship
target, connection and collection path, and which name nothing.

Usage: layer_dangling_targets_oracle.py [--check] [OUT]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes `layerstack_conformance/fixtures/layer_dangling_targets/oracle.json`
by default, which `tests/conformance/layer_dangling_targets.rs` checks layerstack's
`Stage::authored_targets` and `LayerDanglingReport` against.

The fixture's `scene.usda` is opened with every payload loaded, then
`/World/Shed` unloaded (`UsdStage::Unload`). Every prim of
`Traverse(TraverseInstanceProxies(PrimAllPrimsPredicate))` is walked, with
its prim index (`ComputeExpandedPrimIndex` for an instance proxy, whose
index otherwise names the prototype). For each authored property
(`GetAuthoredPropertyNames`), the property specs of every node that can
contribute specs (`PcpNodeRef::CanContributeSpecs`, nodes in strength
order) and every layer of its layer stack, strongest first, are the
property's stack; each spec's `targetPathList` or `connectionPathList`
authors paths in its explicit, prepended, appended and added items. Each
path is read through its node's `mapToRoot.MapSourceToTarget`:

- `replaced` when a stronger spec of the stack authors an explicit list,
  which replaces this one (`SdfListOp::ApplyOperations`);
- `dropped` when the map gives no path: OpenUSD warns that it is outside
  the arc and ignores it;
- otherwise the mapped path, checked as `dangling_targets_oracle.py` checks
  a composed one (`GetObjectAtPath`, `IsActive`, `IsLoaded`), an include of
  a collection path by an applied collection for that collection (`HasAPI`),
  with whether the composed list (`GetTargets`, `GetConnections`) holds it.

Every property spec with such paths in a used layer (`GetUsedLayers`, the
session layer left out) that no read names is recorded as unread, with the
number of specs per layer.

`--check` rewrites the oracle into a temporary file and fails unless it
matches the checked-in one.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE = os.path.normpath(os.path.join(HERE, "..", "fixtures", "layer_dangling_targets"))
ORACLE = os.path.join(FIXTURE, "oracle.json")
UNLOADED = "/World/Shed"


def kind_of(stage, path):
    """Why `path` names nothing usable, or `None` when it resolves."""
    obj = stage.GetObjectAtPath(path)
    if obj:
        prim = obj if path.IsPrimPath() else obj.GetPrim()
        return None if prim.IsActive() else "inactive"
    prim_path = path.GetPrimPath()
    at = prim_path if path.IsPropertyPath() else prim_path.GetParentPath()
    while not at.isEmpty:
        prim = stage.GetPrimAtPath(at)
        if prim:
            if not prim.IsActive():
                return "inactive"
            if not prim.IsLoaded():
                return "unloaded"
            if at == prim_path:
                return "no_property"
            break
        at = at.GetParentPath()
    if not stage.GetPopulationMask().Includes(prim_path):
        return "masked"
    return "no_prim"


def problem_of(stage, prim, name, mapped):
    """Why the mapped path of `prim`'s property `name` names nothing."""
    kind = kind_of(stage, mapped)
    owner = name[len("collection:"):-len(":includes")] if (
        name.startswith("collection:") and name.endswith(":includes")) else None
    if (owner and prim.HasAPI(Usd.CollectionAPI, owner)
            and Usd.CollectionAPI.IsCollectionAPIPath(mapped)
            and kind in (None, "no_property")):
        included = mapped.name[len("collection:"):]
        if not stage.GetPrimAtPath(mapped.GetPrimPath()).HasAPI(Usd.CollectionAPI, included):
            return "no_collection"
    return kind


def name_of(layer):
    return os.path.basename(layer.realPath)


def list_op(spec):
    if isinstance(spec, Sdf.RelationshipSpec):
        return spec.targetPathList
    return spec.connectionPathList


def inserted(op):
    lists = []
    if op.isExplicit:
        lists.append(("explicit", op.explicitItems))
    lists += [("prepended", op.prependedItems), ("appended", op.appendedItems),
              ("added", op.addedItems)]
    return [(position, item) for position, items in lists for item in items]


def nodes_of(node):
    yield node
    for child in node.children:
        yield from nodes_of(child)


def composed_list(prim, name):
    prop = prim.GetProperty(name)
    if isinstance(prop, Usd.Relationship):
        return prop.GetTargets()
    if isinstance(prop, Usd.Attribute):
        return prop.GetConnections()
    return []


def record():
    stage = Usd.Stage.Open(os.path.join(FIXTURE, "scene.usda"), Usd.Stage.LoadAll)
    stage.Unload(Sdf.Path(UNLOADED))
    reads = []
    read_specs = set()
    for prim in stage.Traverse(Usd.TraverseInstanceProxies(Usd.PrimAllPrimsPredicate)):
        index = prim.ComputeExpandedPrimIndex() if prim.IsInstanceProxy() else prim.GetPrimIndex()
        nodes = [node for node in nodes_of(index.rootNode) if node.CanContributeSpecs()]
        for name in prim.GetAuthoredPropertyNames():
            source = str(prim.GetPath().AppendProperty(name))
            composed = None
            replaced = False
            for node in nodes:
                for layer in node.layerStack.layers:
                    spec_path = node.path.AppendProperty(name)
                    spec = layer.GetPropertyAtPath(spec_path)
                    if spec is None:
                        continue
                    op = list_op(spec)
                    items = inserted(op)
                    if items:
                        read_specs.add((name_of(layer), str(spec_path)))
                    for position, item in items:
                        entry = [name_of(layer), str(spec_path), position, str(item), source]
                        if replaced:
                            reads.append(entry + ["replaced", None, False])
                            continue
                        mapped = node.mapToRoot.MapSourceToTarget(item)
                        if mapped.isEmpty:
                            reads.append(entry + ["dropped", "dropped", False])
                            continue
                        if composed is None:
                            composed = composed_list(prim, name)
                        reads.append(entry + [str(mapped), problem_of(stage, prim, name, mapped),
                                              mapped in composed])
                    if op.isExplicit:
                        replaced = True
    layers = {}
    unread = []
    for layer in stage.GetUsedLayers():
        if layer.anonymous:
            continue
        specs = []

        def visit(path, layer=layer, specs=specs):
            spec = layer.GetObjectAtPath(path)
            if isinstance(spec, (Sdf.RelationshipSpec, Sdf.AttributeSpec)) and inserted(
                    list_op(spec)):
                specs.append(str(path))

        layer.Traverse(Sdf.Path.absoluteRootPath, visit)
        layers[name_of(layer)] = len(specs)
        unread += [[name_of(layer), spec] for spec in specs
                   if (name_of(layer), spec) not in read_specs]
    _, minor, patch = Usd.GetVersion()
    return {
        "openusd_version": f"{minor}.{patch}",
        "unloaded": UNLOADED,
        "layers": layers,
        "reads": sorted(reads, key=lambda r: [str(x) for x in r]),
        "unread": sorted(unread),
    }


def write(path):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(record(), f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=ORACLE)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "oracle.json")
        write(path)
        if not filecmp.cmp(path, ORACLE, shallow=False):
            raise SystemExit(f"{ORACLE} is stale")


if __name__ == "__main__":
    main()
