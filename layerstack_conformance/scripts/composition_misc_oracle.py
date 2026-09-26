# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD composes for instance descendants, child order and
path expressions.

Usage: composition_misc_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/composition_misc` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/composition_misc.rs` replays against layerstack.

- `/Grove` is an instance of `/Seedling`. Its local `over "Leaf"` authors a
  reference, an inherit, a variant set and a value: beneath an instance the
  local node is inert, so none of them contribute, and `/Grove/Leaf` holds
  only the prototype's opinions (AOUSD Core §11.3.3; `_ConvertNodeForChild`
  in `pxr/usd/pcp/primIndex.cpp`).
- `/Row` authors children and `reorder nameChildren` in `rows.usda` and
  `root.usda`: each layer appends its new children, then reorders the names
  gathered so far (`PcpComposeSiteChildNames` in
  `pxr/usd/pcp/composeSite.cpp`). Children before the first name a reorder
  lists stay in front (`SdfApplyListOrdering` in `pxr/usd/sdf/listOp.cpp`).
- `/Kiln`, and `/Studio`, which references a copy of it, author explicit
  inherits, specializes, references and payloads on the prim and on its
  selected variant branch, and on a child and on its spec in that branch.
  Each node composes the list ops of its own sites
  (`PcpComposeSiteInherits` and the like in
  `pxr/usd/pcp/composeSite.cpp`), so no node's explicit list replaces
  another's.
- `/Loom/Frame`, and `/Mill/Frame` through a reference to a copy, author
  the same reference, inherit, specializes and payload on the prim and on
  its spec in its parent's selected branch: each arc is followed once from
  each of the two nodes.
- `/Press`'s selected `grip` branch adds a reference, an inherit and a
  payload in `press.usda`, and deletes them in `root.usda`: the branch is
  one node, whose list ops chain across the layer stack, deletes
  included, so none remains there, while the same reference `/Press`
  itself adds stays.
- Path expressions: a stronger expression's `%_` splices in the next weaker
  one (`SdfPathExpression::ComposeOver`); a relative expression is anchored
  at the prim that authors it, and every expression is mapped through the
  arcs to the stage namespace, where a pattern outside an arc's domain
  drops out (`PcpMapFunction::MapSourceToTarget` in
  `pxr/usd/pcp/mapFunction.cpp`).

For every composed prim, instance proxies included, the vectors record its
prim stack, repeats included, and its children in order; for every
attribute, its resolved default and its value at a few numeric times
(numbers as numbers, path expressions as their text, `null` for no value).
"""
import json
import os
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "composition_misc"))

LAYERS = {
    "root": '''#usda 1.0
(
    subLayers = [
        @./rows.usda@,
        @./press.usda@
    ]
)

def "Seedling"
{
    def "Leaf" (
        inherits = </_class_Leaf>
    )
    {
        double size = 1
    }
}

class "_class_Leaf"
{
    double tint = 1
}

def "Stray"
{
    def "Leaf"
    {
        double size = 9
        double vein = 9

        def "Bud"
        {
        }
    }
}

# Nothing authored beneath the instance contributes: not the value, not
# the reference, inherit or variant set of `Leaf`.
def "Grove" (
    instanceable = true
    references = </Seedling>
)
{
    over "Leaf" (
        references = </Stray/Leaf>
        inherits = </Stray/Leaf>
        variants = {
            string season = "dry"
        }
        prepend variantSets = "season"
    )
    {
        double size = 7
        variantSet "season" = {
            "dry" {
                double moisture = 0
            }
        }
    }
}

# `rows.usda` lists `a b c` and reorders them `c b a`; this layer adds `d`
# and reorders `a d`, which leaves `c b` in front.
over "Row"
{
    def "d"
    {
    }
    reorder nameChildren = ["a", "d"]
}

# `d b` moves `d` before `b`, and `b` carries `c` along; `a` stays in front.
def "Column"
{
    def "a"
    {
    }
    def "b"
    {
    }
    def "c"
    {
    }
    def "d"
    {
    }
    reorder nameChildren = ["d", "b"]
}

# `%_` splices in the weaker expression from `rows.usda`.
over "Sets"
{
    uniform pathExpression heroes = "/Stray/L* %_"
    uniform pathExpression lone = "/Stray %_"
    uniform pathExpression both = "%_ - /Stray/Leaf/Bud"

    # `%_` reaching a block: no value at the default time, and nothing
    # spliced in at a numeric time.
    uniform pathExpression blocked = "/Stray %_"

    # `%_` over weaker time samples, and time samples over a weaker default.
    uniform pathExpression sampled = "/Stray %_"
    uniform pathExpression timed.timeSamples = {
        0: "/Stray/Leaf %_",
        10: "/Stray/Leaf/Bud %_",
    }
}

# Expressions authored in `part.usda` are anchored at their prim and mapped
# into `/Part`; paths outside the referenced prim drop out.
def "Part" (
    references = @./part.usda@</Gear>
)
{
    uniform pathExpression chain = "%_ Axle"
    uniform pathExpression plus = "%_ + Local"
}

# An internal reference and an inherit also map the root namespace to itself.
def "Copy" (
    references = </Local>
)
{
}

def "Local"
{
    uniform pathExpression mixed = "/Local/Pin /Stray Pin ../Stray"
    uniform pathExpression globs = "/Local/P[a-z]n /S* //S* %../Local:parts %:parts"
}

def "Heir" (
    inherits = </_class_Heir>
)
{
}

class "_class_Heir"
{
    uniform pathExpression mixed = "/_class_Heir/Pin /Stray Pin"
}

# Each node composes its arcs from its own sites: `/Kiln` and its
# selected `heat` branch author explicit inherits, specializes, references
# and payloads, as do `/Kiln/Shelf` and its spec in that branch, and none
# replaces another's. `/Studio` references the same prim in `kiln.usda`.
def "Kiln" (
    inherits = </_class_Fire>
    specializes = </_base_Kiln>
    references = @./clay.usda@</Clay>
    payload = @./clay.usda@</Glaze>
    variantSets = "heat"
    variants = {
        string heat = "high"
    }
)
{
    def "Shelf" (
        inherits = </_class_Rack>
        references = @./clay.usda@</Board>
    )
    {
    }

    variantSet "heat" = {
        "high" (
            inherits = </_class_Smoke>
            specializes = </_base_Vent>
            references = @./clay.usda@</Slip>
            payload = @./clay.usda@</Ash>
        ) {
            over "Shelf" (
                inherits = </_class_Tray>
                references = @./clay.usda@</Rim>
            )
            {
            }
        }
    }
}

class "_class_Fire"
{
    double fire = 1
}

class "_class_Smoke"
{
    double smoke = 1
}

class "_base_Kiln"
{
    double base = 1
}

class "_base_Vent"
{
    double vent = 1
}

class "_class_Rack"
{
    double rack = 1
}

class "_class_Tray"
{
    double tray = 1
}

def "Studio" (
    references = @./kiln.usda@</Kiln>
)
{
}

# `/Loom/Frame` and its spec in `/Loom`'s selected `weave` branch each
# author the same reference, inherit, specializes and payload: each site
# is a node of its own, so each arc is followed once from each.
def "Loom" (
    variantSets = "weave"
    variants = {
        string weave = "tight"
    }
)
{
    def "Frame" (
        references = @./clay.usda@</Clay>
        inherits = </_class_Loom>
        specializes = </_base_Loom>
        payload = @./clay.usda@</Glaze>
    )
    {
    }

    variantSet "weave" = {
        "tight" {
            over "Frame" (
                references = @./clay.usda@</Clay>
                inherits = </_class_Loom>
                specializes = </_base_Loom>
                payload = @./clay.usda@</Glaze>
            )
            {
            }
        }
    }
}

class "_class_Loom"
{
    double loom = 1
}

class "_base_Loom"
{
    double warp = 1
}

def "Mill" (
    references = @./kiln.usda@</Loom>
)
{
}

# `/Press`'s `firm` branch adds a reference, an inherit and a payload in
# `press.usda`, a sublayer; the stronger `root.usda` deletes each of them
# in the same branch. The branch is one node, whose list ops chain across
# both layers, so none of the three arcs remains there. `/Press` itself,
# another node, adds the same reference, which the branch's deletion
# leaves alone.
over "Press"
{
    variantSet "grip" = {
        "firm" (
            delete references = @./clay.usda@</Slip>
            delete inherits = </_class_Press>
            delete payload = @./clay.usda@</Ash>
        ) {
        }
    }
}
''',
    "rows": '''#usda 1.0

def "Row"
{
    def "a"
    {
    }
    def "b"
    {
    }
    def "c"
    {
    }
    reorder nameChildren = ["c", "b", "a"]
}

def "Sets"
{
    uniform pathExpression heroes = "/Stray/Leaf //Bud"
    uniform pathExpression both = "/Stray// %_"
    uniform pathExpression blocked = None
    uniform pathExpression sampled.timeSamples = {
        0: "/Stray/Leaf",
        10: "/Stray/Leaf/Bud",
    }
    uniform pathExpression timed = "/Stray"
}
''',
    "kiln": '''#usda 1.0

def "Kiln" (
    inherits = </_class_Fire>
    specializes = </_base_Kiln>
    references = @./clay.usda@</Clay>
    payload = @./clay.usda@</Glaze>
    variantSets = "heat"
    variants = {
        string heat = "high"
    }
)
{
    def "Shelf" (
        inherits = </_class_Rack>
        references = @./clay.usda@</Board>
    )
    {
    }

    variantSet "heat" = {
        "high" (
            inherits = </_class_Smoke>
            specializes = </_base_Vent>
            references = @./clay.usda@</Slip>
            payload = @./clay.usda@</Ash>
        ) {
            over "Shelf" (
                inherits = </_class_Tray>
                references = @./clay.usda@</Rim>
            )
            {
            }
        }
    }
}

class "_class_Fire"
{
    double fire = 1
}

class "_class_Smoke"
{
    double smoke = 1
}

class "_base_Kiln"
{
    double base = 1
}

class "_base_Vent"
{
    double vent = 1
}

class "_class_Rack"
{
    double rack = 1
}

class "_class_Tray"
{
    double tray = 1
}

def "Loom" (
    variantSets = "weave"
    variants = {
        string weave = "tight"
    }
)
{
    def "Frame" (
        references = @./clay.usda@</Clay>
        inherits = </_class_Loom>
        specializes = </_base_Loom>
        payload = @./clay.usda@</Glaze>
    )
    {
    }

    variantSet "weave" = {
        "tight" {
            over "Frame" (
                references = @./clay.usda@</Clay>
                inherits = </_class_Loom>
                specializes = </_base_Loom>
                payload = @./clay.usda@</Glaze>
            )
            {
            }
        }
    }
}

class "_class_Loom"
{
    double loom = 1
}

class "_base_Loom"
{
    double warp = 1
}
''',
    "clay": '''#usda 1.0

def "Clay"
{
    double clay = 1
}

def "Glaze"
{
    double glaze = 1
}

def "Slip"
{
    double slip = 1
}

def "Ash"
{
    double ash = 1
}

def "Board"
{
    double board = 1
}

def "Rim"
{
    double rim = 1
}
''',
    "press": '''#usda 1.0

def "Press" (
    prepend references = @./clay.usda@</Slip>
    variantSets = "grip"
    variants = {
        string grip = "firm"
    }
)
{
    variantSet "grip" = {
        "firm" (
            prepend references = @./clay.usda@</Slip>
            prepend inherits = </_class_Press>
            prepend payload = @./clay.usda@</Ash>
        ) {
        }
    }
}

class "_class_Press"
{
    double press = 1
}
''',
    "part": '''#usda 1.0

def "Gear"
{
    uniform pathExpression all = ".//"
    uniform pathExpression inside = "/Gear/Tooth //Tooth"
    uniform pathExpression outside = "/Elsewhere"
    uniform pathExpression mixed = "/Gear/Tooth /Elsewhere ../Elsewhere"
    uniform pathExpression ops = "~/Gear/Tooth & (Tooth - /Gear/Hub)"
    uniform pathExpression chain = "/Gear/Tooth %_"
    uniform pathExpression self = "."
    uniform pathExpression plus = "Child"

    # Character classes keep their ranges; a root glob lies outside the
    # reference and drops out, a leading stretch stays; a relative
    # reference maps like a path, a named one stays.
    uniform pathExpression globs = "/Gear/T[a-z]* Tooth[!x] - /Gear/Hub[0-9] /S* //S*"
    uniform pathExpression refs = "%../Gear:parts %:parts %..:parts"

    def "Tooth"
    {
        uniform pathExpression near = "Tip .. ../Hub"
    }
}
''',
}


# Numeric times the values are also resolved at.
TIMES = [0.0, 5.0, 10.0]


def layer_name(identifier):
    """The layer's file name without directory."""
    return os.path.basename(identifier)


def write_layers(directory):
    os.makedirs(directory, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(directory, f"{name}.usda"), "w") as f:
            f.write(text)


def json_value(value):
    if value is None:
        return None
    if isinstance(value, Sdf.PathExpression):
        return value.GetText()
    if isinstance(value, float):
        return value
    sys.exit(f"unexpected value {value!r}")


def compose(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    predicate = Usd.TraverseInstanceProxies(Usd.PrimAllPrimsPredicate)
    prims = []
    values = {}
    values_at_time = {}
    for prim in Usd.PrimRange.Stage(stage, predicate):
        prims.append({
            "path": str(prim.GetPath()),
            "prim_stack": [[layer_name(spec.layer.identifier), str(spec.path)]
                           for spec in prim.GetPrimStack()],
            "children": [child.GetName()
                         for child in prim.GetFilteredChildren(predicate)],
        })
        for attr in prim.GetAttributes():
            values[str(attr.GetPath())] = json_value(attr.Get())
            for time in TIMES:
                values_at_time.setdefault(str(time), {})[str(attr.GetPath())] = \
                    json_value(attr.Get(time))
    if stage.GetCompositionErrors():
        sys.exit(f"unexpected composition errors: {stage.GetCompositionErrors()}")
    return {"prims": prims, "values": values, "values_at_time": values_at_time}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/composition_misc_oracle.py",
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
