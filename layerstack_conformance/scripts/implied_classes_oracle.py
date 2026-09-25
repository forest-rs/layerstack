# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's strength order for classes implied into stronger layer stacks.

Usage: implied_classes_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/implied_classes` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/implied_classes.rs` replays against layerstack.

A class arc authored inside a reference or payload target is implied into
every stronger layer stack on the way to the root: the class path, mapped
across each arc (a path outside the arc's target maps to itself), is
inherited again beneath the node of that layer stack, with the authored
class node as its origin (AOUSD Core §10.4.2.4; `_EvalImpliedClasses` in
`pxr/usd/pcp/primIndex.cpp`). The implied class ranks with that layer
stack's node, so a class opinion in a stronger layer stack outranks every
opinion of the reference or payload that implies it.

For every composed prim the vectors record its prim stack, repeats
included, and for every attribute its resolved default.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "implied_classes"))

LAYERS = {
    "root": '''#usda 1.0

# `bark.usda /Trunk` inherits `/_class_Bark` two references down; the class
# is implied into `stand.usda` and into this layer, and each implied class
# outranks the reference that implies it.
def "Grove" (
    references = @./stand.usda@</Stand>
)
{
}

# Local opinions on the implied class outrank every referenced opinion.
over "_class_Bark"
{
    int height = 0
    int width = 0
}

# `meadow.usda /Meadow` inherits `/_class_Grass` through a payload.
def "Field" (
    payload = @./meadow.usda@</Meadow>
)
{
}

over "_class_Grass"
{
    int blade = 0
}

# `rock.usda /Boulder` inherits `/_class_Boulder`, whose `Face` inherits the
# nested class `/_class_Boulder/_class_Face`. That class is implied across
# the ancestral inherit as `/Boulder/_class_Face`, then across the reference
# as `/Cliff/_class_Face`, where this layer overrides it.
def "Cliff" (
    references = @./rock.usda@</Boulder>
)
{
    over "_class_Face"
    {
        int grain = 0
    }
}
''',
    "stand": '''#usda 1.0

def "Stand" (
    references = @./bark.usda@</Trunk>
)
{
    int height = 1
}

over "_class_Bark"
{
    int width = 1
    int ring = 1
}
''',
    "bark": '''#usda 1.0

def "Trunk" (
    inherits = </_class_Bark>
)
{
    int ring = 2
    int knot = 2
}

class "_class_Bark"
{
    int height = 2
    int width = 2
    int ring = 2
    int knot = 2
    int moss = 2
}
''',
    "meadow": '''#usda 1.0

def "Meadow" (
    inherits = </_class_Grass>
)
{
    int blade = 1
}

class "_class_Grass"
{
    int blade = 2
    int root = 2
}
''',
    "rock": '''#usda 1.0

def "Boulder" (
    inherits = </_class_Boulder>
)
{
    def "Face"
    {
        int grain = 1
    }

    over "_class_Face"
    {
        int grain = 2
        int lichen = 2
    }
}

class "_class_Boulder"
{
    class "_class_Face"
    {
        int grain = 3
        int lichen = 3
        int crack = 3
    }

    over "Face" (
        inherits = </_class_Boulder/_class_Face>
    )
    {
        int crack = 4
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
        "generator": "layerstack_conformance/scripts/implied_classes_oracle.py",
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
