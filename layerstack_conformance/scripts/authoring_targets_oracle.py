# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's resolve targets for writes to sites no spec is at yet.

Usage: authoring_targets_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/authoring_targets` by default:

- `root.usda`, `asset.usda`, `other.usda`: the layers below, as authored;
- `oracle.json`: for each case, an edit target at a reference node of a
  prim, a property of a descendant, whether the descendant's prim index
  holds a node at the target's site, and the value a resolve target
  "stronger than the edit target" finds (`UsdPrim::
  MakeResolveTargetStrongerThanEditTarget`), which `tests/
  authoring_targets.rs` replays against `Stage::inspect_property_authoring`.

Composition culls a node whose site no layer of its stack has a spec at,
so a referenced asset with no spec where a write would go has no node in
the prim's index. OpenUSD places the write in the prim's index composed
without culling (`UsdPrim::_MakeResolveTargetFromEditTarget`,
`ComputeExpandedPrimIndex`): the write is weaker than what is stronger
than the reference, and stronger than weaker references.
"""
import json
import sys
from pathlib import Path

from pxr import Sdf, Usd

assert Usd.GetVersion() == (0, 26, 11)
out = Path(sys.argv[1]) if len(sys.argv) > 1 else (
    Path(__file__).resolve().parent.parent / "fixtures" / "authoring_targets")
out.mkdir(parents=True, exist_ok=True)
LAYERS = {
    "asset.usda": """#usda 1.0
def "Asset"
{
    def "Child"
    {
        double x = 5
    }
    def "Spot2"
    {
        double x = 3
    }
}
""",
    "other.usda": """#usda 1.0
def "Other"
{
    def "Spot"
    {
        double x = 7
    }
}
""",
    "root.usda": """#usda 1.0
def "Model" (
    references = @./asset.usda@</Asset>
)
{
    def "Local"
    {
        double x = 1
    }
    over "Child"
    {
    }
    def "Bare"
    {
    }
    def "Deep"
    {
        def "Leaf"
        {
            double x = 2
        }
    }
}

def "Pair" (
    references = [@./asset.usda@</Asset>, @./other.usda@</Other>]
)
{
    def "Spot"
    {
    }
    def "Spot2"
    {
    }
}

def "Inst" (
    instanceable = true
    references = @./asset.usda@</Asset>
)
{
}
""",
}
for name, text in LAYERS.items():
    (out / name).write_text(text)

stage = Usd.Stage.Open(str(out / "root.usda"))
CASES = [
    ("/Model", "/Asset", "/Model/Child.x"),
    ("/Model", "/Asset", "/Model/Local.x"),
    ("/Model", "/Asset", "/Model/Bare.x"),
    ("/Model", "/Asset", "/Model/Deep.x"),
    ("/Model", "/Asset", "/Model/Deep/Leaf.x"),
    ("/Pair", "/Asset", "/Pair/Spot.x"),
    ("/Pair", "/Other", "/Pair/Spot2.x"),
    ("/Inst", "/Asset", "/Inst/Child.x"),
]


def nodes(node):
    yield node
    for child in node.children:
        yield from nodes(child)


cases = []
for holder, source, prop in CASES:
    holder_prim = stage.GetPrimAtPath(holder)
    node = next(n for n in holder_prim.GetPrimIndex().rootNode.children if str(n.path) == source)
    layer = node.layerStack.layers[0]
    target = Usd.EditTarget(layer, node)
    attr_path = Sdf.Path(prop)
    prim = stage.GetPrimAtPath(attr_path.GetPrimPath())
    mapped = target.MapToSpecPath(attr_path.GetPrimPath())
    present = any(n.path == mapped and n.layerStack == node.layerStack
                  for n in nodes(prim.GetPrimIndex().rootNode))
    resolve = prim.MakeResolveTargetStrongerThanEditTarget(target)
    query = Usd.AttributeQuery(prim.GetAttribute(attr_path.name), resolve)
    cases.append({
        "holder": holder,
        "source": source,
        "layer": Path(layer.realPath).name,
        "property": prop,
        "site": str(mapped),
        "culled": not present,
        "stronger_value": query.Get() if query.HasAuthoredValue() else None,
    })
(out / "oracle.json").write_text(json.dumps(
    {"openusd_version": "26.11", "root": "root.usda", "cases": cases}, indent=1) + "\n")
