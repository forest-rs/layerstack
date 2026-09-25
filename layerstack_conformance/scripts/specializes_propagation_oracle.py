# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD propagates specializes nodes to the root of a prim index.

Usage: specializes_propagation_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/specializes_propagation` by
default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/specializes_propagation.rs` replays against layerstack.

OpenUSD adds a specializes arc where it is authored as an inert placeholder,
then propagates a copy of that node, with the arcs of the specialized prim
beneath it, to the root of the prim index, with the placeholder as its
origin. Propagated specializes nodes rank after every other child of the
root, and among themselves by where their origins sit. A specializes
authored inside a reference or payload target is also implied into each
stronger layer stack, like an inherit, with the class hierarchy beneath it
(AOUSD Core §10.4.1, §10.4.2.4; `_EvalImpliedSpecializes` and
`_EvalImpliedClasses` in `pxr/usd/pcp/primIndex.cpp`,
`PcpCompareSiblingNodeStrength` in `pxr/usd/pcp/strengthOrdering.cpp`).

For every composed prim the vectors record its prim stack, repeats
included, and for every attribute its resolved default.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "specializes_propagation"))

LAYERS = {
    "root": '''#usda 1.0

# A specializes inside the first reference's target ranks after the second
# reference: `base.usda /Base` outranks `column.usda /_Stone`.
def "Pillar" (
    references = [@./column.usda@</Column>, @./base.usda@</Base>]
)
{
}

# `tile.usda /Tile` specializes `/_Tint`, which inherits `/_Glaze`. The
# specializes is implied into this layer stack as `root.usda /_Tint`, with
# the inherit beneath it: both outrank the referenced classes, and both are
# weaker than `tile.usda /Tile`.
def "Tile" (
    references = @./tile.usda@</Tile>
)
{
}

over "_Tint"
{
    int shade = 0
}

over "_Glaze"
{
    int gloss = 0
}

# A prim that inherits and specializes the same class, directly and
# through a reference whose target specializes it too.
def "Leaf" (
    inherits = </_Green>
    specializes = </_Green>
)
{
}

def "Frond" (
    inherits = </_Green>
    references = @./leaf.usda@</Leaf>
)
{
}

class "_Green"
{
    int hue = 0
}

# A specializes authored in the selected branch of a variant set, and one
# authored in a referenced target's selected branch.
def "Lamp" (
    variants = {
        string glow = "bright"
    }
    prepend variantSets = "glow"
    references = @./lamp.usda@</Lamp>
)
{
    variantSet "glow" = {
        "bright" (
            specializes = </_Warm>
        ) {
        }
        "dim" (
            specializes = </_Cool>
        ) {
        }
    }
}

class "_Warm"
{
    int heat = 0
}

class "_Cool"
{
    int heat = 9
}

over "_Soft"
{
    int glare = 0
}

# `gem.usda /Gem` specializes `/_Cut`, which references `facet.usda
# /Facet`, which specializes `/_Facet`: a chain of two specializes across
# three layer stacks, each implied into the stronger ones.
def "Gem" (
    references = @./gem.usda@</Gem>
)
{
}

over "_Cut"
{
    int sheen = 0
}

over "_Facet"
{
    int edge = 0
}
''',
    "column": '''#usda 1.0

def "Column" (
    specializes = </_Stone>
)
{
}

class "_Stone"
{
    int hue = 1
    int grain = 1
}
''',
    "base": '''#usda 1.0

def "Base"
{
    int hue = 2
}
''',
    "tile": '''#usda 1.0

def "Tile" (
    specializes = </_Tint>
)
{
    int shade = 1
}

class "_Tint" (
    inherits = </_Glaze>
)
{
    int shade = 2
    int gloss = 2
}

class "_Glaze"
{
    int gloss = 3
    int grit = 3
}
''',
    "leaf": '''#usda 1.0

def "Leaf" (
    specializes = </_Green>
)
{
}

class "_Green"
{
    int hue = 1
    int vein = 1
}
''',
    "lamp": '''#usda 1.0

def "Lamp" (
    variants = {
        string shade = "soft"
    }
    prepend variantSets = "shade"
)
{
    variantSet "shade" = {
        "soft" (
            specializes = </_Soft>
        ) {
            int heat = 1
        }
    }
}

class "_Soft"
{
    int glare = 1
    int heat = 2
}
''',
    "gem": '''#usda 1.0

def "Gem" (
    specializes = </_Cut>
)
{
}

class "_Cut" (
    references = @./facet.usda@</Facet>
)
{
    int sheen = 1
}

over "_Facet"
{
    int edge = 1
    int point = 1
}
''',
    "facet": '''#usda 1.0

def "Facet" (
    specializes = </_Facet>
)
{
    int sheen = 2
}

class "_Facet"
{
    int edge = 2
    int point = 2
    int shard = 2
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
    for prim in stage.TraverseAll():
        prims.append({
            "path": str(prim.GetPath()),
            "prim_stack": [[layer_name(spec.layer.identifier), str(spec.path)]
                           for spec in prim.GetPrimStack()],
        })
        for attr in prim.GetAttributes():
            values[str(attr.GetPath())] = attr.Get()
    if stage.GetCompositionErrors():
        sys.exit(f"unexpected composition errors: {stage.GetCompositionErrors()}")
    return {"prims": prims, "values": values}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/specializes_propagation_oracle.py",
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
