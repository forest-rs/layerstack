# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the composition errors OpenUSD reports, and what it drops for them.

Usage: composition_errors_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/composition_errors` by
default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from each entry layer, which
  `tests/composition_errors.rs` replays against layerstack.

Each composition is a `Pcp.Cache` in USD mode, the mode `UsdStage` uses,
with the entry's variant fallbacks. For every prim it records the prim
stack, and for every property its stack, its kind, its target or
connection paths and its resolved default. Errors are recorded as their
`PcpErrorType` name and the composed prim whose prim index, property index
or target index reports them: `PcpPrimIndex.localErrors` for arcs, and the
errors `Pcp.BuildPrimPropertyIndex` and
`ComputeRelationshipTargetPaths` / `ComputeAttributeConnectionPaths` return.

The `invalid_*` layers are not composed: the text parser rejects each of
them, and the oracle records the message. `variant_connection.usda` is the
one relative path inside a variant branch the parser accepts.
"""
import json
import os
import re
import sys

from pxr import Pcp, Sdf, Tf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "composition_errors"))

LAYERS = {
    "root": '''#usda 1.0

# An internal payload list with one target that names no prim: `/Stone`
# reports `UnresolvedPrimPath` and keeps `/Pebble`.
def "Stone" (
    payload = [</Pebble>, </Missing>]
)
{
}

def "Pebble"
{
    int weight = 1
}

# An external reference to a prim the asset does not define.
def "Brook" (
    references = @./stream.usda@</Nowhere>
)
{
}

# `stream.usda /Spring` references `/Absent` internally: the error is
# reported on `/River`, the prim whose composition reaches the arc.
def "River" (
    references = @./stream.usda@</Spring>
)
{
}

# `/Roost/Egg` exists only through `/Roost`'s reference to `/Nest`, and
# reaching it from `/Nest/Egg/Shell` would reference `/Nest/Egg`, an
# ancestor: the cycle is skipped, so the arc brings in no specs.
def "Nest"
{
    def "Egg"
    {
        def "Shell" (
            references = </Roost/Egg>
        )
        {
        }
    }
}

def "Roost" (
    references = </Nest>
)
{
}

# A subroot reference to a prim that exists: no error.
def "Delta" (
    references = @./stream.usda@</Stream/Bend>
)
{
}

# An attribute and a relationship with one name: the strongest spec
# defines the kind, and the conflicting spec is dropped and reported.
def "Lantern" (
    references = @./lamp.usda@</Lamp>
)
{
    int glow = 1
    rel wick = </Pebble>
}

# Targets outside the referenced prim cannot be mapped to `/Yard` and are
# dropped; targets inside it are mapped.
def "Yard" (
    references = @./garden.usda@</Garden>
)
{
}

# A stronger explicit target list replaces the referenced one, so the
# referenced path that cannot be mapped reports nothing.
def "Hedge" (
    references = @./garden.usda@</Plot>
)
{
    rel border = </Pebble>
}

# The same, with both opinions in this layer: the explicit empty list on
# `/Tracing` replaces the target `/Pattern` authors, which cannot be mapped.
def "Pattern"
{
    rel outline = </Tracing/Edge>
}

def "Tracing" (
    references = </Pattern>
)
{
    rel outline = []
}

# The explicit list that wins authors the path that cannot be mapped, so it
# is reported, although a stronger opinion edits the list.
def "Stencil"
{
    rel outline = [</Sketch/Edge>]
}

def "Sketch" (
    references = </Stencil>
)
{
    prepend rel outline = </Pebble>
}

# A class in a referenced layer stack that relocates each instance's
# `Parts/Part` to `/Kit/Parts/<side>`: the connection to the instance's own
# relocated part would not map back, and the one to the other instance's
# part targets an instance of the class.
def "Bench" (
    references = @./kit.usda@</Kit>
)
{
}

# An internal reference maps every path outside `/Tools` to itself, except
# a path under `/Shed`, which would not map back.
def "Shed" (
    references = </Tools>
)
{
}

def "Tools"
{
    rel owner = [</Pebble>, </Tools/Saw>, </Shed/Door>]

    def "Saw"
    {
    }
}

# A class maps every other path to itself, but a connection to the
# inheriting prim would not map back, so `/Orchard` drops it.
class "_class_Tree"
{
    int height
    int height.connect = </Orchard.age>
}

def "Orchard" (
    inherits = </_class_Tree>
)
{
    int age = 3
}

# A connection authored in a class may not target an instance of that
# class: `/Grove/Oak` cannot map it back, and `/Grove/Elm` reports it as
# targeting an instance.
def "Grove"
{
    class "Kind"
    {
        int x
        int x.connect = </Grove/Oak.y>
    }

    def "Oak" (
        inherits = </Grove/Kind>
    )
    {
        int y = 1
    }

    def "Elm" (
        inherits = </Grove/Kind>
    )
    {
        int y = 2
    }
}
''',
    "stream": '''#usda 1.0

def "Stream"
{
    def "Bend"
    {
        int depth = 1
    }
}

def "Spring" (
    references = </Absent>
)
{
    int flow = 2
}
''',
    "kit": '''#usda 1.0
(
    relocates = {
        </Kit/Left/Parts/Part>: </Kit/Parts/Left>,
        </Kit/Right/Parts/Part>: </Kit/Parts/Right>
    }
)

def "Kit"
{
    class "Side"
    {
        def "Gauge"
        {
            int level = 1
            add int level.connect = [
                </Kit/Parts/Right.value>,
                </Kit/Parts/Left.value>,
            ]
        }

        def "Parts"
        {
            def "Part"
            {
            }
        }
    }

    def "Left" (
        inherits = </Kit/Side>
    )
    {
    }

    def "Right" (
        inherits = </Kit/Side>
    )
    {
    }

    def "Parts"
    {
    }
}
''',
    "lamp": '''#usda 1.0

def "Lamp"
{
    rel glow = </Lamp>
    int wick = 4
    int flame = 5
}
''',
    "garden": '''#usda 1.0

def "Garden"
{
    int water
    int water.connect = [</Outside.level>, </Garden.soil>]
    int soil = 1
    rel fence = [</Outside>, </Garden/Bed>]

    def "Bed"
    {
    }
}

def "Plot"
{
    rel border = </Outside>
}

def "Outside"
{
    int level = 2
}
''',
    "fallbacks": '''#usda 1.0

# No selection is authored: the first fallback that names a variant of the
# set, `cube`, is selected.
def "Block" (
    variantSets = ["shape", "size"]
)
{
    variantSet "shape" = {
        "cube" {
            int sides = 6
        }
        "sphere" {
            int sides = 0
        }
    }
    variantSet "size" = {
        "small" {
            int scale = 1
        }
    }
}

# An authored selection wins over the fallbacks.
def "Ball" (
    variants = {
        string shape = "sphere"
    }
    variantSets = ["shape"]
)
{
    variantSet "shape" = {
        "cube" {
            int sides = 6
        }
        "sphere" {
            int sides = 0
        }
    }
}

# A selection authored in a referenced layer stack also wins.
def "Crate" (
    references = @./crate.usda@</Crate>
)
{
}

# A referenced prim without a selection takes the fallback.
def "Box" (
    references = @./crate.usda@</Plain>
)
{
}

# The branch a fallback selects composes its arcs and child prims.
def "Tower" (
    variantSets = ["shape"]
)
{
    variantSet "shape" = {
        "cube" (
            references = @./crate.usda@</Lid>
        ) {
            def "Flag"
            {
                int height = 3
            }
        }
        "sphere" {
        }
    }
}

# A selection authored in the branch a fallback selects outranks the
# fallback of a set declared after it.
def "Mast" (
    variantSets = ["shape", "size"]
)
{
    variantSet "shape" = {
        "cube" (
            variants = {
                string size = "small"
            }
        ) {
        }
        "sphere" {
        }
    }
    variantSet "size" = {
        "small" {
            int scale = 1
        }
        "large" {
            int scale = 2
        }
    }
}

# The same sets declared the other way around: `size` falls back to
# `large` before the `shape=cube` branch is selected, so that branch's
# selection of `size` comes too late and `size=large` is kept.
def "Boom" (
    variantSets = ["size", "shape"]
)
{
    variantSet "shape" = {
        "cube" (
            variants = {
                string size = "small"
            }
        ) {
        }
        "sphere" {
        }
    }
    variantSet "size" = {
        "small" {
            int scale = 1
        }
        "large" {
            int scale = 2
        }
    }
}

# The branch a fallback selects declares a set of its own, which falls
# back too; its branch composes a child prim.
def "Hull" (
    variantSets = ["shape"]
)
{
    variantSet "shape" = {
        "cube" (
            variantSets = ["color"]
        ) {
            variantSet "color" = {
                "red" {
                    int hue = 1

                    def "Flag"
                    {
                        int height = 4
                    }
                }
                "blue" {
                    int hue = 2

                    def "Pennant"
                    {
                    }
                }
            }
        }
        "sphere" {
        }
    }
}
''',
    "crate": '''#usda 1.0

def "Crate" (
    variants = {
        string shape = "sphere"
    }
    variantSets = ["shape"]
)
{
    variantSet "shape" = {
        "cube" {
            int sides = 6
        }
        "sphere" {
            int sides = 0
        }
    }
}

def "Lid"
{
    int hinge = 2
}

def "Plain" (
    variantSets = ["shape"]
)
{
    variantSet "shape" = {
        "cube" {
            int sides = 6
        }
        "sphere" {
            int sides = 0
        }
    }
}
''',
    "selection": '''#usda 1.0

# Variant sets are evaluated in `variantSets` order, each from the
# strongest selection among the sites composed so far. `size` is
# evaluated after `shape=cube` is selected by the weaker reference, and
# that branch, a variant arc of `/Mast`, is stronger than the reference:
# its `size=small` wins over the referenced `size=large`.
def "Mast" (
    references = @./selection_ref.usda@</Mast>
    variantSets = ["shape", "size"]
)
{
    variantSet "shape" = {
        "cube" (
            variants = {
                string size = "small"
            }
        ) {
        }
    }
    variantSet "size" = {
        "small" {
            int scale = 1
        }
        "large" {
            int scale = 2
        }
    }
}

# The same sets declared the other way around: `size` is evaluated
# first, before any branch is selected, and takes the referenced
# `size=large`.
def "Boom" (
    references = @./selection_ref.usda@</Mast>
    variantSets = ["size", "shape"]
)
{
    variantSet "shape" = {
        "cube" (
            variants = {
                string size = "small"
            }
        ) {
        }
    }
    variantSet "size" = {
        "small" {
            int scale = 1
        }
        "large" {
            int scale = 2
        }
    }
}

# The reference selects `shape=cube` and `color`, a set the `cube` branch
# declares: nothing stronger selects `color`, so the referenced `blue`
# holds.
def "Hull" (
    references = @./selection_ref.usda@</Hull>
    variantSets = ["shape"]
)
{
    variantSet "shape" = {
        "cube" (
            variantSets = ["color"]
        ) {
            variantSet "color" = {
                "red" {
                    int hue = 1
                }
                "blue" {
                    int hue = 2
                }
            }
        }
    }
}

# The same, with the `cube` branch selecting `color=red` itself, over the
# weaker reference.
def "Keel" (
    references = @./selection_ref.usda@</Hull>
    variantSets = ["shape"]
)
{
    variantSet "shape" = {
        "cube" (
            variants = {
                string color = "red"
            }
            variantSets = ["color"]
        ) {
            variantSet "color" = {
                "red" {
                    int hue = 1
                }
                "blue" {
                    int hue = 2
                }
            }
        }
    }
}

# A chain: the referenced `shape=cube` selects a branch that selects
# `size=small`, whose branch selects `color=red`; each wins over the
# reference's selection for the next set.
def "Spar" (
    references = @./selection_ref.usda@</Spar>
    variantSets = ["shape", "size", "color"]
)
{
    variantSet "shape" = {
        "cube" (
            variants = {
                string size = "small"
            }
        ) {
        }
    }
    variantSet "size" = {
        "small" (
            variants = {
                string color = "red"
            }
        ) {
            int scale = 1
        }
        "large" {
            int scale = 2
        }
    }
    variantSet "color" = {
        "red" {
            int hue = 1
        }
        "blue" {
            int hue = 2
        }
    }
}

# `/Mast` referenced: its sets are evaluated the same way at the
# referenced node.
def "Deck" (
    references = </Mast>
)
{
}
''',
    "selection_ref": '''#usda 1.0

def "Mast" (
    variants = {
        string shape = "cube"
        string size = "large"
    }
)
{
}

def "Hull" (
    variants = {
        string color = "blue"
        string shape = "cube"
    }
)
{
}

def "Spar" (
    variants = {
        string color = "blue"
        string shape = "cube"
        string size = "large"
    }
)
{
}

def "Sized" (
    variants = {
        string size = "large"
    }
)
{
}

def "Shaped" (
    variants = {
        string shape = "cube"
    }
)
{
}
''',
    "selection_fallbacks": '''#usda 1.0

# No selection of `shape` is authored, so its fallback waits until every
# authored selection is evaluated: `size` takes the referenced `large`
# first, and the `size=small` of the fallback branch `shape=cube` comes too
# late.
def "Mast" (
    references = @./selection_ref.usda@</Sized>
    variantSets = ["shape", "size"]
)
{
    variantSet "shape" = {
        "cube" (
            variants = {
                string size = "small"
            }
        ) {
        }
    }
    variantSet "size" = {
        "small" {
            int scale = 1
        }
        "large" {
            int scale = 2
        }
    }
}

# The reference selects `shape=cube`, whose branch selects `size=small`:
# an authored selection, so the fallback `size=large` is not used.
def "Boom" (
    references = @./selection_ref.usda@</Shaped>
    variantSets = ["shape", "size"]
)
{
    variantSet "shape" = {
        "cube" (
            variants = {
                string size = "small"
            }
        ) {
        }
        "sphere" {
        }
    }
    variantSet "size" = {
        "small" {
            int scale = 1
        }
        "large" {
            int scale = 2
        }
    }
}
''',
    "invalid_inherit": '''#usda 1.0

def "Hill" (
    variantSets = ["v"]
)
{
    variantSet "v" = {
        "x" {
            def "Rock"
            {
            }
        }
    }
}

def "Slope" (
    inherits = </Hill{v=x}Rock>
)
{
}
''',
    "invalid_specializes": '''#usda 1.0

def "Slope" (
    specializes = </Hill{v=x}Rock>
)
{
}
''',
    "invalid_reference": '''#usda 1.0

def "Slope" (
    references = </Hill{v=x}Rock>
)
{
}
''',
    "invalid_payload": '''#usda 1.0

def "Slope" (
    payload = @./stream.usda@</Stream{v=x}Bend>
)
{
}
''',
    "invalid_relocates": '''#usda 1.0
(
    relocates = {
        </Hill{v=x}Rock>: </Hill/Stone>
    }
)
''',
    "invalid_target": '''#usda 1.0

def "Hill" (
    variantSets = ["v"]
)
{
    variantSet "v" = {
        "x" {
            def "Rock"
            {
                rel next = <../Stone>
            }
        }
    }
}
''',
    "invalid_connection": '''#usda 1.0

def "Hill"
{
    int x
    int x.connect = </Hill{v=x}Rock.y>
}
''',
    "variant_connection": '''#usda 1.0

# A relative connection inside a variant branch is anchored at the prim
# without its variant selection: `/Hill/Rock.y`.
def "Hill" (
    variants = {
        string v = "x"
    }
    variantSets = ["v"]
)
{
    variantSet "v" = {
        "x" {
            def "Rock"
            {
                int x
                int x.connect = <.y>
                int y = 1
            }
        }
    }
}
''',
}

# Entry layers composed, with the variant fallbacks each is composed with.
ENTRIES = {
    "root.usda": {},
    "fallbacks.usda": {
        "shape": ["cone", "cube"],
        "size": ["large", "small"],
        "color": ["red"],
    },
    "variant_connection.usda": {},
    "selection.usda": {},
    "selection_fallbacks.usda": {
        "shape": ["cube"],
        "size": ["large", "small"],
    },
}


def layer_name(identifier):
    """The layer's file name without directory."""
    return os.path.basename(identifier)


def write_layers(directory):
    os.makedirs(directory, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(directory, f"{name}.usda"), "w") as f:
            f.write(text)


def error_kind(err):
    """The `PcpErrorType` name of a `Pcp.Error*`."""
    kind = type(err).__name__
    return kind[len("Error"):] if kind.startswith("Error") else kind


def compose(directory, entry, fallbacks):
    root = Sdf.Layer.FindOrOpen(os.path.join(directory, entry))
    cache = Pcp.Cache(Pcp.LayerStackIdentifier(root), usd=True)
    cache.SetVariantFallbacks(fallbacks)
    Usd.Stage.SetGlobalVariantFallbacks(fallbacks)
    stage = Usd.Stage.Open(root, Usd.Stage.LoadAll)
    prims = []
    errors = set()

    def record(path, errs):
        for err in errs:
            errors.add((error_kind(err), str(path)))

    def walk(path):
        index, errs = cache.ComputePrimIndex(path)
        if index.hasAnyPayloads and not cache.IsPayloadIncluded(path):
            cache.RequestPayloads([path], [])
            index, errs = cache.ComputePrimIndex(path)
        record(path, errs)
        if path != Sdf.Path.absoluteRootPath:
            prims.append(prim_record(path, index))
        names, _prohibited = index.ComputePrimChildNames()
        for name in names:
            walk(path.AppendChild(name))

    def prim_record(path, index):
        properties = []
        for name in sorted(index.ComputePrimPropertyNames()):
            property_path = path.AppendProperty(name)
            property_index, errs = Pcp.BuildPrimPropertyIndex(
                property_path, cache, index)
            record(path, errs)
            specs = list(property_index.propertyStack)
            is_rel = bool(specs) and isinstance(specs[0], Sdf.RelationshipSpec)
            if is_rel:
                result = cache.ComputeRelationshipTargetPaths(
                    property_path, False, None, False)
            else:
                result = cache.ComputeAttributeConnectionPaths(
                    property_path, False, None, False)
            record(path, result[2])
            entry = {
                "name": name,
                "kind": "relationship" if is_rel else "attribute",
                "stack": [[layer_name(s.layer.identifier), str(s.path)]
                          for s in specs],
                "targets": [str(p) for p in result[0]],
            }
            attribute = stage.GetAttributeAtPath(property_path)
            if not is_rel and attribute and attribute.HasAuthoredValue():
                entry["value"] = attribute.Get()
            properties.append(entry)
        return {
            "path": str(path),
            "prim_stack": [[layer_name(s.layer.identifier), str(s.path)]
                           for s in index.primStack],
            "properties": properties,
        }

    walk(Sdf.Path.absoluteRootPath)
    Usd.Stage.SetGlobalVariantFallbacks({})
    return {
        "root": entry,
        "fallbacks": fallbacks,
        "prims": prims,
        "errors": [list(e) for e in sorted(errors)],
    }


def rejection(directory, name):
    """The text parser's message for a layer it rejects."""
    try:
        Sdf.Layer.FindOrOpen(os.path.join(directory, name))
    except Tf.ErrorException as e:
        match = re.search(
            r"(\w+(?: \w+)* paths cannot contain variant selections"
            r"|'[^']*' is not a valid [a-z ]+ path)", str(e))
        return match.group(1) if match else str(e)
    sys.exit(f"{name} was expected to be rejected")


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/composition_errors_oracle.py",
        "openusd_version": version,
        "compositions": [compose(out_dir, entry, fallbacks)
                         for entry, fallbacks in ENTRIES.items()],
        "rejected": {
            f"{name}.usda": rejection(out_dir, f"{name}.usda")
            for name in LAYERS if name.startswith("invalid_")
        },
    }
    out_path = os.path.join(out_dir, "oracle.json")
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=1, ensure_ascii=False)
        f.write("\n")
    count = sum(len(c["prims"]) for c in doc["compositions"])
    print(f"wrote {count} prims from OpenUSD {version} to {out_path}")


if __name__ == "__main__":
    main()
