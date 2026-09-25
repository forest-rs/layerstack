# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records which layer stack OpenUSD composes internal arcs in.

Usage: internal_arcs_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/internal_arcs` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/internal_arcs.rs` replays against layerstack.

An internal reference or payload (no asset path) targets the layer stack
that contains the site authoring it, not the layer that authors it: the root
layer and every sublayer, with each sublayer's time offset, and the root
layer's `defaultPrim` for a `<>` target (AOUSD Core §10.3.2.1;
`pxr/usd/pcp/primIndex.cpp`, `_EvalRefOrPayloadArcs`). Every internal arc
below is authored in a sublayer, and a stronger layer of the same stack
authors a competing opinion at its target.

For every composed prim the vectors record its prim stack, for every
attribute with a default its resolved default, and for every time-sampled
attribute its value at `TIME`.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "internal_arcs"))

# Time at which time-sampled attributes are recorded. A layer with offset 10
# contributes its sample at 10 here, a layer without one its sample at 20.
TIME = 20

LAYERS = {
    "root": '''#usda 1.0
(
    defaultPrim = "Rock"
    subLayers = [
        @./sub.usda@ (offset = 10)
    ]
)

# An internal reference authored in a sublayer of a referenced asset reads
# the asset's whole layer stack.
def "InAsset" (
    references = @./asset.usda@</Tree>
)
{
}

# Internal references two asset layer stacks deep, and an internal payload
# beside them, each authored in its stack's sublayer.
def "Nested" (
    references = @./outer.usda@</Outer>
)
{
}

def "Rock"
{
    int mass = 7
}

over "Stone"
{
    int mass = 4
    int heat.timeSamples = {
        0: 0,
        10: 1,
        20: 2,
        30: 3,
    }
}

# The internal arc in `sub.usda` is the one deleted here.
over "Edited" (
    delete references = </Pebble>
)
{
}

over "Link"
{
    int links = 2
}

over "Anchor"
{
    int weight = 5
}
''',
    "sub": '''#usda 1.0
(
    defaultPrim = "Pebble"
)

def "Pebble"
{
    int mass = 1
}

def "Stone"
{
    int mass = 2
    int size = 6
    int glow.timeSamples = {
        0: 0,
        10: 1,
        20: 2,
        30: 3,
    }
}

# `<>` names the stack root layer's `defaultPrim`, not this layer's.
def "Holder" (
    payload = <>
)
{
}

# An internal payload reads every layer of the root layer stack.
def "Carrier" (
    payload = </Stone>
)
{
}

def "Edited" (
    prepend references = [</Pebble>, </Stone>]
)
{
}

# An internal payload inside the target of an internal reference.
def "Chain" (
    references = </Link>
)
{
}

def "Link" (
    payload = </Anchor>
)
{
    int links = 1
}

def "Anchor"
{
    int weight = 3
}
''',
    "asset": '''#usda 1.0
(
    subLayers = [
        @./asset_sub.usda@ (offset = 10)
    ]
)

over "Trunk"
{
    int height = 1
    int grow.timeSamples = {
        0: 0,
        10: 1,
        20: 2,
        30: 3,
    }
}
''',
    "asset_sub": '''#usda 1.0

def "Tree" (
    references = </Trunk>
)
{
}

def "Trunk"
{
    int height = 5
    int width = 3
    int bark.timeSamples = {
        0: 0,
        10: 1,
        20: 2,
        30: 3,
    }
}
''',
    "outer": '''#usda 1.0
(
    subLayers = [
        @./outer_sub.usda@
    ]
)

over "Shell"
{
    int depth = 1
}
''',
    "outer_sub": '''#usda 1.0

def "Outer" (
    references = @./inner.usda@</Inner>
    payload = </Shell>
)
{
}

def "Shell"
{
    int depth = 2
    int thickness = 4
}
''',
    "inner": '''#usda 1.0
(
    subLayers = [
        @./inner_sub.usda@
    ]
)

over "Core"
{
    int radius = 1
}
''',
    "inner_sub": '''#usda 1.0

def "Inner" (
    references = </Core>
)
{
}

def "Core"
{
    int radius = 2
    int density = 9
}
''',
}


def layer_name(identifier):
    """The layer's file name without directory."""
    return os.path.basename(identifier)


def write_layers(directory):
    os.makedirs(directory, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(directory, f"{name}.usda"), "w") as f:
            f.write(text)


def compose(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    stage.SetInterpolationType(Usd.InterpolationTypeHeld)
    prims = []
    values = {}
    samples = {}
    for prim in stage.TraverseAll():
        prims.append({
            "path": str(prim.GetPath()),
            "prim_stack": [[layer_name(spec.layer.identifier), str(spec.path)]
                           for spec in prim.GetPrimStack()],
        })
        for attr in prim.GetAttributes():
            if attr.GetNumTimeSamples():
                samples[str(attr.GetPath())] = attr.Get(TIME)
            else:
                values[str(attr.GetPath())] = attr.Get()
    if stage.GetCompositionErrors():
        sys.exit(f"unexpected composition errors: {stage.GetCompositionErrors()}")
    return {"prims": prims, "values": values, "time": TIME, "samples": samples}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/internal_arcs_oracle.py",
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
