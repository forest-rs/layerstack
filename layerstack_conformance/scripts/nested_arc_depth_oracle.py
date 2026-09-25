# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's strength order for arcs nested two or more deep.

Usage: nested_arc_depth_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/nested_arc_depth` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/nested_arc_depth.rs` replays against layerstack.

OpenUSD ranks opinions by a strong-to-weak depth-first walk of the prim
index graph (`PcpCompareNodeStrength` in `pxr/usd/pcp/strengthOrdering.cpp`):
a node ranks above every node beneath it, and the nodes beneath an arc rank
before that arc's weaker siblings, however deep they are nested. A reference
inside a payload inside a reference therefore outranks a second reference
listed beside the first, and ranks after it when listed after it.

For every composed prim the vectors record its prim stack, repeats
included, and for every attribute its resolved default.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "nested_arc_depth"))

LAYERS = {
    "root": '''#usda 1.0

# A reference inside a payload inside the first reference outranks the
# second reference.
def "Boulder" (
    references = [@./quarry.usda@</Quarry>, @./pebble.usda@</Pebble>]
)
{
}

# Listed second, the same nest ranks after the direct reference.
def "Crag" (
    references = [@./pebble.usda@</Pebble>, @./quarry.usda@</Quarry>]
)
{
}
''',
    "quarry": '''#usda 1.0

def "Quarry" (
    payload = @./seam.usda@</Seam>
)
{
}
''',
    "seam": '''#usda 1.0

def "Seam" (
    references = @./vein.usda@</Vein>
)
{
    int depth = 2
}
''',
    "vein": '''#usda 1.0

def "Vein"
{
    int depth = 3
    int grain = 3
}
''',
    "pebble": '''#usda 1.0

def "Pebble"
{
    int grain = 1
    int depth = 1
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
        "generator": "layerstack_conformance/scripts/nested_arc_depth_oracle.py",
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
