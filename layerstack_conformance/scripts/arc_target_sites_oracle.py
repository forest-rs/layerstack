# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's prim stacks where each arc target's sites appear once.

Usage: arc_target_sites_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/arc_target_sites` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/arc_target_sites.rs` replays against layerstack.

OpenUSD builds an arc's target subgraph once (`_AddArc` in
`pxr/usd/pcp/primIndex.cpp`), so a site appears once per arc path in a prim
stack: the selected branch of a referenced prim, an asset reached through
references nested in another asset, and a class a prim inherits directly
that a referenced prim implies again (`_IsRedundantSite`). A class reached
twice is one site at its strongest registration (`skipDuplicateNodes`):
inherited directly and through another class's specializes, in either list
order, specialized directly, and the same shape through a reference.

For every composed prim the vectors record its prim stack, repeats
included, and for every attribute its resolved default.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "arc_target_sites"))

LAYERS = {
    "root": '''#usda 1.0

# A referenced prim with a selected variant: the branch is one site.
def "Trunk" (
    references = @./asset.usda@</Tree>
)
{
}

# The same asset reached through references nested in another asset.
def "Grove" (
    references = @./outer.usda@</Grove>
)
{
}

# A class inherited directly, and again by the referenced prim, which
# implies it back into this layer stack: the class is one site.
class "Class"
{
    int size = 1
}

def "Bush" (
    inherits = </Class>
    references = @./shrub.usda@</Shrub>
)
{
}

# A class inherited directly and reached again, more weakly, through the
# specializes of another inherited class: the direct inherit is the
# registration that stays, whichever of the two is listed first.
def "Pine" (
    inherits = [</Needle>, </Bark>]
)
{
}

def "Fir" (
    inherits = [</Bark>, </Needle>]
)
{
}

class "Needle" (
    specializes = </Resin>
)
{
}

class "Resin" (
    specializes = </Bark>
)
{
    int x = 3
}

class "Bark"
{
    int x = 4
}

# The same class specialized directly, and reached through the
# specializes of an inherited class.
def "Larch" (
    inherits = </Needle>
    specializes = </Bark>
)
{
}

# The first shape again, inside a referenced layer stack.
def "Spruce" (
    references = @./classes.usda@</Sapling>
)
{
}
''',
    "asset": '''#usda 1.0

def "Tree" (
    variants = {
        string season = "summer"
    }
    prepend variantSets = "season"
)
{
    int height = 1
    variantSet "season" = {
        "summer" {
            int leaves = 2
        }
    }
}
''',
    "outer": '''#usda 1.0

def "Grove"
{
    def "Oak" (
        references = @./asset.usda@</Tree>
    )
    {
    }
    def "Elm" (
        references = @./asset.usda@</Tree>
    )
    {
    }
}
''',
    "classes": '''#usda 1.0

def "Sapling" (
    inherits = [</Twig>, </Knot>]
)
{
}

class "Twig" (
    specializes = </Sap>
)
{
}

class "Sap" (
    specializes = </Knot>
)
{
    int x = 6
}

class "Knot"
{
    int x = 7
}
''',
    "shrub": '''#usda 1.0

def "Shrub" (
    inherits = </Class>
)
{
}

class "Class"
{
    int size = 2
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
        "generator": "layerstack_conformance/scripts/arc_target_sites_oracle.py",
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
