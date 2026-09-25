# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD retimes arcs authored in offset sublayers.

Usage: layer_offsets_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/layer_offsets` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/layer_offsets.rs` replays against layerstack.

A reference or payload to another layer stack is read on the timeline of
the layer that authors it: its own offset composes beneath the offset of
that layer within its layer stack (AOUSD Core §12.3.2.1, §10.3.1.1;
OpenUSD `_EvalRefOrPayloadArcs` in `pxr/usd/pcp/primIndex.cpp`, which sets
the arc's offset to `sourceLayerStackOffset * layerOffset`). An internal
reference and a class arc read the same layer stack, whose layers keep
their own sublayer offsets, and take no offset from the layer that authors
them.

For every composed prim the vectors record its prim stack, and for every
attribute its resolved default. For an attribute with time samples they
record, at each composed sample time, at the midpoints between them and one
unit beyond either end, its value with linear interpolation, and the offset
(`[offset, scale]`) that maps the strongest time-sample opinion's layer
onto the stage: its node's map to the root composed with its layer's offset
in the node's layer stack.
"""
import json
import os
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "layer_offsets"))

LAYERS = {
    "root": '''#usda 1.0
(
    subLayers = [
        @./shot.usda@ (offset = 10),
        @./timing.usda@ (offset = 4; scale = 2)
    ]
)

# A reference authored in this layer, which has no offset of its own, to a
# prim whose layer stack authors a reference in a sublayer with an offset
# and scale: 2 + (5 + 2 * (1 + t)).
def "Station" (
    references = @./hub.usda@</Hub> (offset = 2)
)
{
}

# A reference to a subroot target whose parent's reference is authored in
# a sublayer with an offset and scale: the ancestral arc takes them too,
# 3 + 2 * (1 + t).
def "Probe" (
    references = @./system.usda@</System/Planet>
)
{
}
''',
    "shot": '''#usda 1.0

# A reference authored in a sublayer with offset 10: 10 + (5 + t), for the
# target and its namespace descendants.
def "Comet" (
    references = @./orbit.usda@</Orbit> (offset = 5)
)
{
}

# An internal reference authored in the same sublayer: its target is read
# in this layer stack, where `timing.usda` has offset 4 and scale 2, so
# this layer's offset does not apply.
def "Echo" (
    references = </Beacon>
)
{
}

# A class arc authored in the same sublayer reads `/_class_Flare` in this
# layer stack too.
def "Flare" (
    inherits = </_class_Flare>
)
{
}
''',
    "timing": '''#usda 1.0

# A payload authored in a sublayer with offset 4 and scale 2, with its own
# offset and scale: 4 + 2 * (1 + 3 * t).
def "Satellite" (
    payload = @./orbit.usda@</Orbit> (offset = 1; scale = 3)
)
{
}

def "Beacon"
{
    double phase.timeSamples = {
        0: 0,
        2: 20,
    }
}

class "_class_Flare"
{
    double phase.timeSamples = {
        0: 0,
        1: 5,
    }
}
''',
    "orbit": '''#usda 1.0

def "Orbit"
{
    int radius = 1
    double phase.timeSamples = {
        0: 0,
        10: 100,
    }

    def "Moon"
    {
        int radius = 2
        double phase.timeSamples = {
            0: 0,
            4: 8,
        }
    }
}
''',
    "hub": '''#usda 1.0
(
    subLayers = [
        @./hub_sub.usda@ (offset = 5; scale = 2)
    ]
)

def "Hub"
{
    int docks = 3
}
''',
    "hub_sub": '''#usda 1.0

# A nested reference authored in a sublayer of the referenced layer stack.
over "Hub" (
    references = @./orbit.usda@</Orbit> (offset = 1)
)
{
}
''',
    "system": '''#usda 1.0
(
    subLayers = [
        @./system_sub.usda@ (offset = 3; scale = 2)
    ]
)

def "System"
{
    def "Planet"
    {
        int rings = 0
    }
}
''',
    "system_sub": '''#usda 1.0

# The parent of `/Probe`'s target references `/Orbit`, so `/System/Planet`
# reaches `/Orbit/Planet` through this ancestral arc.
over "System" (
    references = @./orbit_system.usda@</Orbit> (offset = 1)
)
{
}
''',
    "orbit_system": '''#usda 1.0

def "Orbit"
{
    def "Planet"
    {
        double phase.timeSamples = {
            0: 0,
            6: 12,
        }
    }
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


def strongest_offset(attr):
    """The offset from the strongest time-sample opinion's layer to the stage."""
    info = attr.GetResolveInfo(Usd.TimeCode(0))
    node = info.GetNode()
    layer = next(spec.layer for spec in attr.GetPropertyStack()
                 if spec.HasInfo("timeSamples"))
    stack = node.layerStack
    in_stack = stack.layerOffsets[stack.layers.index(layer)]
    offset = node.mapToRoot.timeOffset * in_stack
    return [offset.offset, offset.scale]


def compose(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    prims = []
    values = {}
    samples = {}
    offsets = {}
    for prim in stage.TraverseAll():
        prims.append({
            "path": str(prim.GetPath()),
            "prim_stack": [[layer_name(spec.layer.identifier), str(spec.path)]
                           for spec in prim.GetPrimStack()],
        })
        for attr in prim.GetAttributes():
            times = attr.GetTimeSamples()
            if not times:
                values[str(attr.GetPath())] = attr.Get()
                continue
            probes = sorted(set(
                list(times)
                + [(a + b) / 2 for a, b in zip(times, times[1:])]
                + [times[0] - 1, times[-1] + 1]))
            samples[str(attr.GetPath())] = [[t, attr.Get(t)] for t in probes]
            offsets[str(attr.GetPath())] = strongest_offset(attr)
    if stage.GetCompositionErrors():
        sys.exit(f"unexpected composition errors: {stage.GetCompositionErrors()}")
    return {"prims": prims, "values": values, "samples": samples,
            "offsets": offsets}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/layer_offsets_oracle.py",
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
