# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the arc path of every source in OpenUSD's prim index graphs.

Usage: prim_index_graph_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/prim_index_graph` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: for every prim OpenUSD composes from `root.usda`, each
  source of its prim stack, strongest first, with the arc path of the node
  that provides it, which `tests/prim_index_graph.rs` replays against
  layerstack.

An arc path lists `[arc, site]` from the root node down to the node: the arc
that reaches each node (`Local` for the root) and the path the node reads in
its layer stack. A variant branch is a node beneath the node whose site
hosts the variant set, and arcs authored inside the branch, and specs of
descendants authored inside it, sit beneath that variant node
(`pxr/usd/pcp/primIndex.cpp`, `_EvalNodeVariantSets`, `_AddArc`).
"""
import json
import os
import sys

from pxr import Pcp, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "prim_index_graph"))

LAYERS = {
    "root": '''#usda 1.0
(
    subLayers = [
        @./sub.usda@
    ]
)

# A variant set nested inside a branch of another: `/Nested{v=x}{b=y}` is a
# node beneath `/Nested{v=x}`.
def "Nested" (
    variants = {
        string v = "x"
        string b = "y"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "x" (
            prepend variantSets = "b"
        ) {
            int outer = 1
            variantSet "b" = {
                "y" {
                    int inner = 2
                }
            }
        }
    }
}

# A reference and a payload authored on a selected branch sit beneath the
# branch's node.
def "InVariant" (
    variants = {
        string v = "x"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "x" (
            prepend references = </Target>
            prepend payload = @./asset.usda@</Leaf>
        ) {
        }
    }
}

# A reference authored on a child spec inside an ancestor's branch sits
# beneath the ancestral variant node `/Outer{v=x}Child`.
def "Outer" (
    variants = {
        string v = "x"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "x" {
            def "Child" (
                prepend references = @./asset.usda@</Leaf>
            )
            {
            }
        }
    }
}

def "Target"
{
    int target = 1
}

# A class whose selected branch authors a child: `/ByInherits/Child` and
# `/BySpecializes/Child` read `/Class{v=x}Child` through the class's variant
# node.
class "Class" (
    variants = {
        string v = "x"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "x" {
            def "Child"
            {
                int x = 1
            }
        }
    }
}

def "ByInherits" (
    inherits = </Class>
)
{
}

def "BySpecializes" (
    specializes = </Class>
)
{
}

# The same branch child, and arcs authored inside branches, reached through a
# reference and a payload to another layer stack.
def "ByReference" (
    references = @./asset.usda@</Model>
)
{
}

def "ByPayload" (
    payload = @./asset.usda@</Model>
)
{
}
''',
    "sub": '''#usda 1.0

# An internal reference authored on a branch in a sublayer targets the root
# layer stack, and still sits beneath the branch's node.
def "Anchored" (
    variants = {
        string v = "x"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "x" (
            prepend references = </Target>
        ) {
        }
    }
}
''',
    "asset": '''#usda 1.0

def "Model" (
    variants = {
        string v = "x"
        string b = "y"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "x" (
            prepend references = </Part>
            prepend variantSets = "b"
        ) {
            int outer = 1
            variantSet "b" = {
                "y" {
                    int inner = 2
                }
            }
            def "Child"
            {
                int x = 1
            }
            def "Linked" (
                prepend references = </Part>
            )
            {
            }
        }
    }
}

def "Part"
{
    int part = 1
}

def "Leaf"
{
    int leaf = 1
}
''',
}

ARCS = {
    Pcp.ArcTypeRoot: "Local",
    Pcp.ArcTypeInherit: "Inherits",
    Pcp.ArcTypeVariant: "Variants",
    Pcp.ArcTypeRelocate: "Relocates",
    Pcp.ArcTypeReference: "References",
    Pcp.ArcTypePayload: "Payloads",
    Pcp.ArcTypeSpecialize: "Specializes",
}


def layer_name(layer):
    """The layer's file name without directory."""
    return os.path.basename(layer.identifier)


def write_layers(directory):
    os.makedirs(directory, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(directory, f"{name}.usda"), "w") as f:
            f.write(text)


def arc_path(node):
    """`[arc, site]` pairs from the root node down to `node`."""
    path = []
    while node:
        path.append([ARCS[node.arcType], str(node.path)])
        node = node.parent
    return list(reversed(path))


def sources(node, out):
    """Appends the sources of `node` and its descendants, strongest first,
    as `GetPrimStack` lists them."""
    if node.hasSpecs and not node.isInert and not node.isCulled:
        for layer in node.layerStack.layers:
            if layer.GetPrimAtPath(node.path):
                out.append({
                    "layer": layer_name(layer),
                    "site": str(node.path),
                    "arc_path": arc_path(node),
                })
    for child in node.children:
        sources(child, out)


def compose(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    prims = []
    for prim in stage.TraverseAll():
        index = prim.GetPrimIndex()
        found = []
        sources(index.rootNode, found)
        stack = [[layer_name(spec.layer), str(spec.path)]
                 for spec in prim.GetPrimStack()]
        if stack != [[s["layer"], s["site"]] for s in found]:
            sys.exit(f"{prim.GetPath()}: graph walk {found} is not the prim stack {stack}")
        prims.append({"path": str(prim.GetPath()), "sources": found})
    if stage.GetCompositionErrors():
        sys.exit(f"unexpected composition errors: {stage.GetCompositionErrors()}")
    return {"prims": prims}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/prim_index_graph_oracle.py",
        "openusd_version": version,
        "root": "root.usda",
        **compose(out_dir),
    }
    out_path = os.path.join(out_dir, "oracle.json")
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print(f"wrote {len(doc['prims'])} prims from OpenUSD {version} to {out_path}")


if __name__ == "__main__":
    main()
