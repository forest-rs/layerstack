# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records which children OpenUSD composes around variant branches.

Usage: variant_children_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/variant_children` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/variant_children.rs` replays against layerstack.

A prim's children are the names its contributing specs author
(`PcpComposeSiteChildNames` in `pxr/usd/pcp/composeSite.cpp`, called for
each node of the prim index by `Pcp_ComposeChildNames` in
`pxr/usd/pcp/primIndex.cpp`). A selected variant branch is a node of the
index; an unselected one is not, so what it authors never names a child,
and never hides a child that a contributing spec authors either.

For every composed prim the vectors record its prim stack, repeats
included, and its ordered children; for every attribute, its resolved
default. `switched` records the same after selecting the other branch of
`/Scene` and `/Tree`, as the stage's root layer authors it.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "variant_children"))

LAYERS = {
    "root": '''#usda 1.0

# An `over` in the unselected branch names a child the base also defines;
# the selected branch is empty. The child stays.
def "Shape" (
    variants = {
        string look = "Plain"
    }
    prepend variantSets = "look"
)
{
    def "Geom"
    {
        int sides = 3
    }

    def "Frame"
    {
    }

    variantSet "look" = {
        "Plain" {
        }
        "Tinted" {
            over "Geom"
            {
                int sides = 4
            }
        }
    }
}

# The same, one level down: the unselected branch also overs a
# grandchild, which stays with its subtree.
def "Scene" (
    variants = {
        string look = "Plain"
    }
    prepend variantSets = "look"
)
{
    def "Geom"
    {
        def "Triangle"
        {
            int corners = 3

            def "Edge"
            {
            }
        }
    }

    variantSet "look" = {
        "Plain" {
        }
        "Tinted" {
            over "Geom"
            {
                over "Triangle"
                {
                    int corners = 4
                }
            }
        }
    }
}

# Children only an unselected branch authors disappear, at any depth.
def "Tree" (
    variants = {
        string season = "Summer"
    }
    prepend variantSets = "season"
)
{
    def "Trunk"
    {
        int rings = 12
    }

    variantSet "season" = {
        "Summer" {
            def "Leaf"
            {
            }
        }
        "Winter" {
            def "Snow"
            {
            }

            over "Trunk"
            {
                int rings = 13

                def "Icicle"
                {
                }
            }
        }
    }
}

# The base child comes through a reference; the variant is local.
def "Lamp" (
    variants = {
        string power = "Off"
    }
    prepend references = @./lamp.usda@</Lamp>
    prepend variantSets = "power"
)
{
    variantSet "power" = {
        "Off" {
        }
        "On" {
            over "Shade"
            {
                over "Bulb"
                {
                    int watts = 60
                }
            }
        }
    }
}

# The base child comes through a payload; the variant is local.
def "Lantern" (
    variants = {
        string power = "Off"
    }
    prepend payload = @./lamp.usda@</Lamp>
    prepend variantSets = "power"
)
{
    variantSet "power" = {
        "Off" {
        }
        "On" {
            over "Shade"
            {
                int height = 9
            }
        }
    }
}

# The variant lives in the referenced asset.
def "Boulder" (
    prepend references = @./boulder.usda@</Boulder>
)
{
}

# The unselected branch sits inside a selected one.
def "Pine" (
    variants = {
        string size = "Tall"
    }
    prepend variantSets = "size"
)
{
    def "Cone"
    {
        int scales = 5
    }

    variantSet "size" = {
        "Short" {
            over "Cone"
            {
                int scales = 1
            }
        }
        "Tall" (
            variants = {
                string age = "Young"
            }
            prepend variantSets = "age"
        ) {
            def "Crown"
            {
            }

            variantSet "age" = {
                "Old" {
                    over "Cone"
                    {
                        int scales = 9

                        def "Seed"
                        {
                        }
                    }

                    over "Crown"
                    {
                    }
                }
                "Young" {
                }
            }
        }
    }
}

# A child authored in the base and in the selected branch, beside
# children only one of the branches authors.
def "Pebble" (
    variants = {
        string grade = "Fine"
    }
    prepend variantSets = "grade"
)
{
    def "Chip"
    {
        int size = 1
    }

    variantSet "grade" = {
        "Coarse" {
            over "Chip"
            {
                int size = 3
            }

            def "Gravel"
            {
            }
        }
        "Fine" {
            def "Dust"
            {
            }

            over "Chip"
            {
                int size = 2
            }
        }
    }
}
''',
    "lamp": '''#usda 1.0

def "Lamp"
{
    def "Shade"
    {
        int height = 7

        def "Bulb"
        {
            int watts = 40
        }
    }

    def "Base"
    {
    }
}
''',
    "boulder": '''#usda 1.0

def "Boulder" (
    variants = {
        string weather = "Dry"
    }
    prepend variantSets = "weather"
)
{
    def "Moss"
    {
        int patches = 2

        def "Spore"
        {
        }
    }

    variantSet "weather" = {
        "Dry" {
        }
        "Wet" {
            over "Moss"
            {
                int patches = 8

                over "Spore"
                {
                }

                def "Drop"
                {
                }
            }

            def "Puddle"
            {
            }
        }
    }
}
''',
}


# The selections `tests/variant_children.rs` switches through `LiveStage`.
SWITCH = (("/Scene", "look", "Tinted"), ("/Tree", "season", "Winter"))


def layer_name(identifier):
    """The layer's file name without directory."""
    return os.path.basename(identifier)


def write_layers(directory):
    os.makedirs(directory, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(directory, f"{name}.usda"), "w") as f:
            f.write(text)


def compose(directory, switch=()):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    for path, variant_set, variant in switch:
        stage.GetPrimAtPath(path).GetVariantSet(variant_set).SetVariantSelection(variant)
    prims = []
    values = {}
    for prim in stage.TraverseAll():
        prims.append({
            "path": str(prim.GetPath()),
            "prim_stack": [[layer_name(spec.layer.identifier), str(spec.path)]
                           for spec in prim.GetPrimStack()],
            "children": [str(name) for name in prim.GetAllChildrenNames()],
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
        "generator": "layerstack_conformance/scripts/variant_children_oracle.py",
        "openusd_version": version,
        "root": "root.usda",
        **compose(out_dir),
        # After selecting the other branch of `/Scene` and `/Tree`.
        "switched": {
            "selections": [list(s) for s in SWITCH],
            **compose(out_dir, SWITCH),
        },
    }
    out_path = os.path.join(out_dir, "oracle.json")
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print(f"wrote {len(doc['prims'])} prims from OpenUSD {version} to {out_path}")


if __name__ == "__main__":
    main()
