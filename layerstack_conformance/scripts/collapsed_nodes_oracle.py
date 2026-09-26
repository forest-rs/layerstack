# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's prim indexes for sites reached more than once.

Usage: collapsed_nodes_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/collapsed_nodes` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/collapsed_nodes.rs` replays against layerstack.

A prim index has one node per arc occurrence: a site reached through two
references, or listed twice with different layer offsets, is two nodes,
each with its own arc path, namespace mapping and layer offset. Only the
class-based arcs skip a site already in the graph (AOUSD Core §10.4;
OpenUSD `_AddArc` and `skipDuplicateNodes` in
`pxr/usd/pcp/primIndex.cpp`).

For every composed prim the vectors record its prim stack, repeats
included, and for every attribute its resolved default. For an attribute
with time samples they record its property stack, each spec with the
layer offset mapping it to the stage, and, at each composed sample time,
at the midpoints between them and one unit beyond either end, its value
with linear interpolation.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "collapsed_nodes"))

LAYERS = {
    "root": '''#usda 1.0

# A diamond: `left.usda /Left` and `right.usda /Right` both reference
# `source.usda /Source`, each with its own offset, so `/Junction` reaches
# `/Source` twice.
def "Junction" (
    references = [
        @./left.usda@</Left>,
        @./right.usda@</Right>
    ]
)
{
}

# The same payload target through two payloads.
def "Delta" (
    payload = [
        @./left.usda@</Left>,
        @./right.usda@</Right>
    ]
)
{
}

# The same target listed three times with different offsets: three nodes,
# the first strongest.
def "Echo" (
    references = [
        @./pulse.usda@</Pulse> (offset = 10),
        @./pulse.usda@</Pulse> (offset = 20; scale = 2),
        @./pulse.usda@</Pulse>
    ]
)
{
}

# `/Orbit/Moon`'s own payload to `moon.usda /Moon` is stronger than the
# one `/Orbit`'s payload reaches beneath it, through `system.usda
# /System/Moon`'s reference to `moon_ref.usda /Moon`: `depth` comes from
# `moon.usda`, not from `system.usda`.
def "Orbit" (
    payload = @./system.usda@</System>
)
{
    def "Moon" (
        payload = @./moon.usda@</Moon>
    )
    {
    }
}
''',
    "left": '''#usda 1.0

def "Left" (
    references = @./source.usda@</Source> (offset = 5)
)
{
    int left = 1
}
''',
    "right": '''#usda 1.0

def "Right" (
    references = @./source.usda@</Source> (offset = 7; scale = 0.5)
)
{
    int right = 1
    int width = 2
}
''',
    "source": '''#usda 1.0

def "Source"
{
    int width = 3
    double level.timeSamples = {
        0: 0,
        4: 8,
    }

    def "Tap"
    {
        int flow = 3
    }
}
''',
    "pulse": '''#usda 1.0

def "Pulse"
{
    double beat.timeSamples = {
        0: 0,
        2: 4,
    }

    def "Peak"
    {
        int height = 1
    }
}
''',
    "system": '''#usda 1.0

def "System"
{
    def "Moon" (
        references = @./moon_ref.usda@</Moon>
    )
    {
        over "Crater"
        {
            int depth = 1
        }
    }
}
''',
    "moon_ref": '''#usda 1.0

def "Moon" (
    payload = @./moon.usda@</Moon>
)
{
    over "Crater"
    {
    }
}
''',
    "moon": '''#usda 1.0

def "Moon"
{
    def "Crater"
    {
        int depth = 2
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


def strength_ordered_nodes(node):
    """The nodes beneath and including `node`, strongest first."""
    yield node
    for child in node.children:
        yield from strength_ordered_nodes(child)


def property_stack(prim, name):
    """Each spec of the property, strongest first, with the layer offset
    of its node, which maps its layer's times to the stage's."""
    stack = []
    for node in strength_ordered_nodes(prim.GetPrimIndex().rootNode):
        if node.isInert or not node.hasSpecs:
            continue
        path = node.path.AppendProperty(name)
        for layer in node.layerStack.layers:
            if not layer.GetPropertyAtPath(path):
                continue
            # These layer stacks have no sublayers, so the node's offset
            # is the spec's.
            offset = node.mapToRoot.timeOffset
            stack.append([layer_name(layer.identifier), str(path),
                          offset.offset, offset.scale])
    return stack


def compose(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    stage.Load()
    prims = []
    values = {}
    samples = {}
    stacks = {}
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
            stacks[str(attr.GetPath())] = property_stack(prim, attr.GetName())
    if stage.GetCompositionErrors():
        sys.exit(f"unexpected composition errors: {stage.GetCompositionErrors()}")
    return {"prims": prims, "values": values, "samples": samples,
            "property_stacks": stacks}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/collapsed_nodes_oracle.py",
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
