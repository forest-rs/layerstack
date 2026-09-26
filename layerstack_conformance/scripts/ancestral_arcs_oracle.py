# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's prim stacks for arcs to subroot targets.

Usage: ancestral_arcs_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/ancestral_arcs` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/ancestral_arcs.rs` replays against layerstack.

An arc to a subroot target `/T/B` reads the target's prim index, which
starts from its parent's: the arcs and variant selections authored on `/T`
reach `/T/B` with its name appended to their sites, beneath the arc's node
(AOUSD Core §10.2, §10.4; OpenUSD `_AddArc` with `includeAncestralOpinions`
and `_BuildInitialPrimIndexFromAncestor` in `pxr/usd/pcp/primIndex.cpp`).

For every composed prim the vectors record its prim stack, repeats
included, for every attribute its resolved default, and for every
relationship and connected attribute its target paths. For an attribute
with time samples they record, at each composed sample time, at the
midpoints between them and one unit beyond either end, its value with
linear interpolation.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "ancestral_arcs"))

LAYERS = {
    "root": '''#usda 1.0

# `grove.usda /Grove` references `/Forest`, so `/Grove/Tree` reaches
# `forest.usda /Forest/Tree` and its children.
def "Orchard" (
    references = @./grove.usda@</Grove/Tree>
)
{
}

# Two levels below the ancestor that authors the reference.
def "Twig" (
    references = @./grove.usda@</Grove/Tree/Branch>
)
{
}

# `field.usda /Field` selects `season = "summer"`; its branch holds
# opinions for `Patch` and references `flowers.usda /Flowers`. The
# selection authored here is for `/Meadow`'s own variant set, not for
# `/Field`'s, which the reference does not map.
def "Meadow" (
    references = @./field.usda@</Field/Patch>
    variants = {
        string season = "winter"
    }
)
{
}

# `cliff.usda /Cliff` inherits `/_class_Cliff`, so `/Cliff/Ledge` inherits
# `/_class_Cliff/Ledge`, a class implied into this layer stack as well.
def "Outcrop" (
    references = @./cliff.usda@</Cliff/Ledge>
)
{
}

over "_class_Cliff"
{
    over "Ledge"
    {
        int moss = 0
    }
}

# A payload to a subroot target whose parent references a sibling prim of
# its own layer.
def "Crater" (
    payload = @./basin.usda@</Basin/Pool>
)
{
}

# Layer offsets compose across every arc: `/Clock` references
# `dial.usda /Dial/Hand` with offset 10 and scale 2, and `/Dial` references
# `gears.usda /Gears` with offset 3 and scale 4, so a sample at time t in
# `gears.usda` lands at 10 + 2 * (3 + 4 * t); `spin` comes from a sublayer
# of `gears.usda` with its own offset and scale. `/Stopwatch` reaches the
# same prims through payloads.
def "Clock" (
    prepend references = @./dial.usda@</Dial/Hand> (offset = 10; scale = 2)
)
{
}

def "Stopwatch" (
    prepend payload = @./dial.usda@</Timer/Hand> (offset = 6; scale = 3)
)
{
}

# An internal reference to `/Zeta/Stone`, which only `/Zeta`'s reference to
# `/Quarry` provides, and which comes after `/Alpha` in namespace order.
# `/Quarry/Stone` inherits `/Quarry/_class_Rock`, outside the subroot
# target: the class maps across `/Zeta`'s reference as authored, to
# `/Zeta/_class_Rock`, whose opinion wins.
def "Alpha" (
    references = </Zeta/Stone>
)
{
}

def "Zeta" (
    references = </Quarry>
)
{
    over "_class_Rock"
    {
        int grain = 0
    }
}

def "Quarry"
{
    def "Stone" (
        inherits = </Quarry/_class_Rock>
    )
    {
        int weight = 5

        def "Chip"
        {
            int size = 5
        }
    }

    class "_class_Rock"
    {
        int grain = 5
        int hardness = 5
    }
}

# `/Hut/Loft` references `loft.usda /Hut`, a target with the same name as
# the stage's `/Hut`. Its relationships, those of the class it inherits,
# and those of the class its `Stool` specializes and of the prim its
# `Shelf` references internally, map once, into `/Hut/Loft`.
def "Hut"
{
    def "Loft" (
        references = @./loft.usda@</Hut>
    )
    {
    }
}
''',
    "loft": '''#usda 1.0

# Every target path below maps once, into `/Hut/Loft`'s namespace: through
# the reference alone, or first through the arc that brings its opinion
# (a class it inherits, a specialized class, an internal reference), then
# through the reference. `stray` lies beneath `Shelf`, the internal
# reference's destination, which that reference does not map back:
# `Shelf` drops it, `_proto_Jar` keeps it.
def "Hut" (
    prepend inherits = </_class_Beam>
)
{
    rel door = </Hut/Wick>

    def "Wick"
    {
        int flame = 1
    }

    def "Shelf" (
        references = </Hut/_proto_Jar>
    )
    {
    }

    def "Stool" (
        specializes = </Hut/_base_Stool>
    )
    {
    }

    class "_proto_Jar"
    {
        rel lid = </Hut/_proto_Jar/Cap>
        rel wick = </Hut/Wick>
        rel stray = </Hut/Shelf/Cap>

        def "Cap"
        {
        }
    }

    class "_base_Stool"
    {
        rel seat = </Hut/_base_Stool/Cushion>
        rel wick = </Hut/Wick>

        def "Cushion"
        {
        }
    }
}

class "_class_Beam"
{
    rel beam = </_class_Beam/Ray>

    def "Ray"
    {
        int width = 1
    }
}
''',
    "grove": '''#usda 1.0

def "Grove" (
    references = @./forest.usda@</Forest>
)
{
    def "Tree"
    {
        int width = 1
    }
}
''',
    "forest": '''#usda 1.0

def "Forest"
{
    def "Tree"
    {
        int width = 2
        int height = 2

        def "Branch"
        {
            int leaves = 2
        }
    }
}
''',
    "field": '''#usda 1.0

def "Field" (
    variants = {
        string season = "summer"
    }
    prepend variantSets = "season"
)
{
    variantSet "season" = {
        "summer" (
            references = @./flowers.usda@</Flowers>
        ) {
            over "Patch"
            {
                int bloom = 1
            }
        }
        "winter" {
            over "Patch"
            {
                int bloom = 0
                int frost = 0
            }
        }
    }

    def "Patch"
    {
        int grass = 1
    }
}
''',
    "flowers": '''#usda 1.0

def "Flowers"
{
    def "Patch"
    {
        int bloom = 2
        int petals = 2
    }
}
''',
    "cliff": '''#usda 1.0

def "Cliff" (
    inherits = </_class_Cliff>
)
{
    def "Ledge"
    {
        int grain = 1
    }
}

class "_class_Cliff"
{
    over "Ledge"
    {
        int grain = 3
        int moss = 3
    }
}
''',
    "dial": '''#usda 1.0

def "Dial" (
    prepend references = @./gears.usda@</Gears> (offset = 3; scale = 4)
)
{
    def "Hand"
    {
    }
}

def "Timer" (
    prepend payload = @./gears.usda@</Gears> (offset = -2; scale = 0.5)
)
{
    def "Hand"
    {
    }
}
''',
    "gears": '''#usda 1.0
(
    subLayers = [
        @./gears_sub.usda@ (offset = 1; scale = 2)
    ]
)

def "Gears"
{
    def "Hand"
    {
        double angle.timeSamples = {
            0: 0,
            2: 2,
        }
    }
}
''',
    "gears_sub": '''#usda 1.0

over "Gears"
{
    over "Hand"
    {
        double spin.timeSamples = {
            0: 0,
            4: 4,
        }
    }
}
''',
    "basin": '''#usda 1.0

def "Basin" (
    references = </Lake>
)
{
}

def "Lake"
{
    def "Pool"
    {
        int depth = 4
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


def compose(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    prims = []
    values = {}
    samples = {}
    targets = {}
    for prim in stage.TraverseAll():
        for rel in prim.GetRelationships():
            targets[str(rel.GetPath())] = [str(t) for t in rel.GetTargets()]
        for attr in prim.GetAttributes():
            if attr.HasAuthoredConnections():
                targets[str(attr.GetPath())] = [str(t) for t in attr.GetConnections()]
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
    if stage.GetCompositionErrors():
        sys.exit(f"unexpected composition errors: {stage.GetCompositionErrors()}")
    return {"prims": prims, "values": values, "samples": samples, "targets": targets}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/ancestral_arcs_oracle.py",
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
