# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD composes variant sets nested in other branches.

Usage: variant_specs_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/variant_specs` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/variant_specs.rs` replays against layerstack.

Every variant is a spec of its own, addressed by its variant-qualified path
(`/P{a=x}`, `/P{a=x}{b=y}`), and a variant set may be a child of a prim spec
or of a variant spec (AOUSD Core §7.3.6; `SdfVariantSetSpec` under
`SdfPrimSpec` or `SdfVariantSpec` in `pxr/usd/sdf`). So a set nested in a
branch is available only under that branch, a set reusing an enclosing
set's name is a set of its own, and a set of one name nested under two
branches is two sets with their own contents. A nested branch composes as a
variant node beneath the node of the branch enclosing it, ranked by its
set's position in the `variantSets` of that branch, so two branches may
order the same nested sets differently (`_AddVariantArc` in
`pxr/usd/pcp/primIndex.cpp`), in the layer stack as the referencing
context's expression variables gather it.

For every composed prim the oracle records its prim stack, repeats
included, its variant selections, and for every attribute its resolved
default.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "variant_specs"))

LAYERS = {
    "root": '''#usda 1.0
(
    expressionVariables = {
        string SEAM = "seam_pleat.usda"
    }
)

# Directly nested sets reusing a name: `finish=rough` nests `grain`, whose
# `coarse` branch nests another `finish` set. Each of the three branches is
# a variant spec with its own opinions and child; `depth`, authored at
# every level, resolves from the outermost branch.
def "Stone" (
    prepend variantSets = "finish"
    variants = {
        string finish = "rough"
        string grain = "coarse"
    }
)
{
    variantSet "finish" = {
        "rough" (
            prepend variantSets = "grain"
        ) {
            int depth = 1
            int outer = 1
            def "Chip"
            {
            }
            variantSet "grain" = {
                "coarse" (
                    prepend variantSets = "finish"
                ) {
                    int depth = 2
                    int middle = 2
                    def "Flake"
                    {
                    }
                    variantSet "finish" = {
                        "rough" {
                            int depth = 3
                            int inner = 3
                            def "Dust"
                            {
                            }
                        }
                        "smooth" {
                            int inner = 4
                            def "Sheen"
                            {
                            }
                        }
                    }
                }
                "fine" {
                    int middle = 5
                    def "Powder"
                    {
                    }
                }
            }
        }
        "smooth" {
            int outer = 6
            def "Polish"
            {
            }
        }
    }
}

# One set name, `canopy`, nested under both `season` branches with
# different contents. `/Oak` selects the winter one, `/Elm` the summer one.
class "_class_Tree" (
    prepend variantSets = "season"
)
{
    variantSet "season" = {
        "summer" (
            prepend variantSets = "canopy"
        ) {
            variantSet "canopy" = {
                "full" {
                    int leaves = 10
                    def "Leaves"
                    {
                    }
                }
                "sparse" {
                    int leaves = 3
                }
            }
        }
        "winter" (
            prepend variantSets = "canopy"
        ) {
            variantSet "canopy" = {
                "full" {
                    int leaves = 0
                    int snow = 1
                    def "Snow"
                    {
                    }
                }
                "sparse" {
                    int snow = 2
                }
            }
        }
    }
}

def "Oak" (
    inherits = </_class_Tree>
    variants = {
        string canopy = "full"
        string season = "winter"
    }
)
{
}

def "Elm" (
    inherits = </_class_Tree>
    variants = {
        string canopy = "full"
        string season = "summer"
    }
)
{
}

# Three levels, each selected by the branch enclosing it: the prim selects
# `flow`, the `fast` branch selects `depth`, the `deep` branch selects
# `bed`. Unselected alternatives author other values at each level.
def "River" (
    prepend variantSets = "flow"
    variants = {
        string flow = "fast"
    }
)
{
    variantSet "flow" = {
        "fast" (
            variants = {
                string depth = "deep"
            }
            prepend variantSets = "depth"
        ) {
            int speed = 9
            variantSet "depth" = {
                "deep" (
                    variants = {
                        string bed = "rocky"
                    }
                    prepend variantSets = "bed"
                ) {
                    int level = 2
                    variantSet "bed" = {
                        "rocky" {
                            int grit = 7
                            def "Boulder"
                            {
                                int size = 3
                            }
                        }
                        "sandy" {
                            int grit = 1
                            def "Dune"
                            {
                            }
                        }
                    }
                }
                "shallow" {
                    int level = 1
                    def "Ford"
                    {
                    }
                }
            }
        }
        "slow" {
            int speed = 1
            def "Pool"
            {
            }
        }
    }
}

# Nested sets rank by the `variantSets` of the branch declaring them:
# `plain` declares `[warp, weft]`, `twill` declares `[weft, warp]`, so
# `/Plain` takes `warp`'s opinion and `/Twill` `weft`'s.
class "_class_Weave" (
    prepend variantSets = "pattern"
)
{
    variantSet "pattern" = {
        "plain" (
            prepend variantSets = ["warp", "weft"]
        ) {
            variantSet "warp" = {
                "on" {
                    int thread = 1
                }
            }
            variantSet "weft" = {
                "on" {
                    int thread = 2
                }
            }
        }
        "twill" (
            prepend variantSets = ["weft", "warp"]
        ) {
            variantSet "warp" = {
                "on" {
                    int thread = 3
                }
            }
            variantSet "weft" = {
                "on" {
                    int thread = 4
                }
            }
        }
    }
}

def "Plain" (
    inherits = </_class_Weave>
    variants = {
        string pattern = "plain"
        string warp = "on"
        string weft = "on"
    }
)
{
}

def "Twill" (
    inherits = </_class_Weave>
    variants = {
        string pattern = "twill"
        string warp = "on"
        string weft = "on"
    }
)
{
}

# Three nested sets in three orders: each prim's value and prim stack
# follow the order of its own branch.
class "_class_Knot" (
    prepend variantSets = "tie"
)
{
    variantSet "tie" = {
        "loop" (
            prepend variantSets = ["hue", "tone", "grain"]
        ) {
            variantSet "hue" = {
                "on" {
                    int strand = 1
                }
            }
            variantSet "tone" = {
                "on" {
                    int strand = 2
                }
            }
            variantSet "grain" = {
                "on" {
                    int strand = 3
                }
            }
        }
        "hitch" (
            prepend variantSets = ["grain", "hue", "tone"]
        ) {
            variantSet "hue" = {
                "on" {
                    int strand = 4
                }
            }
            variantSet "tone" = {
                "on" {
                    int strand = 5
                }
            }
            variantSet "grain" = {
                "on" {
                    int strand = 6
                }
            }
        }
        "bend" (
            prepend variantSets = ["tone", "grain", "hue"]
        ) {
            variantSet "hue" = {
                "on" {
                    int strand = 7
                }
            }
            variantSet "tone" = {
                "on" {
                    int strand = 8
                }
            }
            variantSet "grain" = {
                "on" {
                    int strand = 9
                }
            }
        }
    }
}

def "Loop" (
    inherits = </_class_Knot>
    variants = {
        string grain = "on"
        string hue = "on"
        string tie = "loop"
        string tone = "on"
    }
)
{
}

def "Hitch" (
    inherits = </_class_Knot>
    variants = {
        string grain = "on"
        string hue = "on"
        string tie = "hitch"
        string tone = "on"
    }
)
{
}

def "Bend" (
    inherits = </_class_Knot>
    variants = {
        string grain = "on"
        string hue = "on"
        string tie = "bend"
        string tone = "on"
    }
)
{
}

# Nested sets inside a referenced asset: `/Grove` keeps the asset's outer
# selection and overrides the nested one; `/Copse` takes both from the
# asset. The asset's same-named inner sets differ by outer branch.
def "Grove" (
    references = @./model.usda@</Model>
    variants = {
        string shade = "dark"
    }
)
{
}

def "Copse" (
    references = @./model.usda@</Model>
)
{
}

# The referenced `seam.usda` sublayers the layer its `SEAM` variable names:
# `seam_hem.usda` by default, `seam_pleat.usda` as this layer overrides it.
# The two author the same nested sets in opposite orders, so the nested
# sets rank by the sublayer the referencing context selects.
def "Hem" (
    references = @./seam.usda@</Seam>
)
{
}
''',
    "seam": '''#usda 1.0
(
    expressionVariables = {
        string SEAM = "seam_hem.usda"
    }
    subLayers = [
        @`"./${SEAM}"`@
    ]
)
''',
    "seam_hem": '''#usda 1.0

def "Seam" (
    variants = {
        string cut = "x"
        string hem = "on"
        string pleat = "on"
    }
    prepend variantSets = "cut"
)
{
    variantSet "cut" = {
        "x" (
            prepend variantSets = ["hem", "pleat"]
        ) {
            variantSet "hem" = {
                "on" {
                    int stitch = 1
                }
            }
            variantSet "pleat" = {
                "on" {
                    int stitch = 2
                }
            }
        }
    }
}
''',
    "seam_pleat": '''#usda 1.0

def "Seam" (
    variants = {
        string cut = "x"
        string hem = "on"
        string pleat = "on"
    }
    prepend variantSets = "cut"
)
{
    variantSet "cut" = {
        "x" (
            prepend variantSets = ["pleat", "hem"]
        ) {
            variantSet "hem" = {
                "on" {
                    int stitch = 1
                }
            }
            variantSet "pleat" = {
                "on" {
                    int stitch = 2
                }
            }
        }
    }
}
''',
    "model": '''#usda 1.0

def "Model" (
    prepend variantSets = "light"
    variants = {
        string light = "dim"
        string shade = "pale"
    }
)
{
    variantSet "light" = {
        "dim" (
            prepend variantSets = "shade"
        ) {
            int glow = 1
            variantSet "shade" = {
                "pale" {
                    int tone = 1
                    def "Mist"
                    {
                    }
                }
                "dark" {
                    int tone = 2
                    def "Shadow"
                    {
                    }
                }
            }
        }
        "bright" (
            prepend variantSets = "shade"
        ) {
            int glow = 5
            variantSet "shade" = {
                "pale" {
                    int tone = 3
                    def "Haze"
                    {
                    }
                }
                "dark" {
                    int tone = 4
                }
            }
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


def compose(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    prims = []
    values = {}
    for prim in stage.TraverseAll():
        prims.append({
            "path": str(prim.GetPath()),
            "prim_stack": [[layer_name(spec.layer.identifier), str(spec.path)]
                           for spec in prim.GetPrimStack()],
            "selections": dict(sorted(
                prim.GetVariantSets().GetAllVariantSelections().items())),
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
        "generator": "layerstack_conformance/scripts/variant_specs_oracle.py",
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
