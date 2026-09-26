# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD composes asset paths authored as variable expressions.

Usage: expression_variables_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/expression_variables` by
default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/expression_variables.rs` replays against layerstack.

A sublayer, reference or payload asset path may be a variable expression
(`SdfVariableExpression`), evaluated with the `expressionVariables` of the
layer stack that authors it. A layer stack reached through a reference or
payload composes its root layer's variables beneath those of the layer
stack that reaches it (`PcpExpressionVariables::Compute`), so a variable
set by the referencing layer stack wins, and one set only by a layer stack
in between passes through. An expression that evaluates to nothing drops
the arc silently; one that fails to evaluate drops it with a
`PcpErrorVariableExpressionError`. Each layer's arcs are evaluated and
anchored to that layer before list ops compose them, so a stronger
`delete` of an expression removes the arc a weaker layer adds when both
evaluate to the same asset, and not when they anchor in different
directories; literal and expression arcs, and any spelling of one asset
(`granite.usda`, `./granite.usda`, `deep/../granite.usda`), compare by
the anchored asset. A variant selection is evaluated only where composition
reads it.

For every composed prim the vectors record its prim stack, for every
attribute its resolved default, and every composition error as its
`PcpErrorType` name and the path of the prim whose index found it (empty
for an error of a layer stack).
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "expression_variables"))

LAYERS = {
    "root": '''#usda 1.0
(
    expressionVariables = {
        string ROCK = "granite"
        string LAYER = "strata"
        bool DEEP = true
        string SEASON = "summer"
    }
    subLayers = [
        # The sublayer path comes from a variable.
        @`"./${LAYER}.usda"`@,
        # An expression that evaluates to nothing is skipped silently.
        @`""`@,
        # A sublayer in another directory, whose relative asset paths
        # anchor there.
        @./deep/vault.usda@
    ]
)

# The reference path comes from a variable.
def "Outcrop" (
    references = @`"./${ROCK}.usda"`@
)
{
}

# The payload path comes from a function of a variable.
def "Deposit" (
    payload = @`if(${DEEP}, "./basalt.usda", "./granite.usda")`@
)
{
}

# `cliff.usda` sets ROCK to shale for its own reference; this layer stack
# sets it to granite, which wins. STONE, set only there, keeps its value.
def "Ridge" (
    references = @./cliff.usda@
)
{
}

# A reference to a child of `/Cliff`, which only `/Cliff`'s own reference
# brings: that ancestral arc is evaluated as for `/Ridge`.
def "Ledge" (
    references = @./cliff.usda@</Cliff/Crystal>
)
{
}

# `bed.usda`, two references down, reads ROCK from this layer stack and
# DEPTH from `gorge.usda`, the layer stack in between.
def "Canyon" (
    references = @./gorge.usda@
)
{
}

# Substituted into a string, a variable that is not set leaves its name:
# `./MISSING.usda` does not resolve.
def "Hollow" (
    references = @`"./${MISSING}.usda"`@
)
{
}

# On its own, a variable that is not set is an error; the arc is dropped.
def "Void" (
    references = @`${MISSING}`@
)
{
}

# An expression that does not parse is an error; the arc is dropped, and
# the next one still composes.
def "Rubble" (
    references = [@`"./${ROCK"`@, @./basalt.usda@]
)
{
}

# An expression that evaluates to no value drops the arc silently.
def "Plain" (
    references = @`if(${DEEP}, None, "./granite.usda")`@
)
{
}

# A value of another type than string is an error.
def "Marsh" (
    payload = @`${DEEP}`@
)
{
}

# The variant selection comes from a variable.
def "Meadow" (
    variants = {
        string season = "`${SEASON}`"
    }
    variantSets = "season"
)
{
    variantSet "season" = {
        "summer" {
            int bloom = 3
        }
        "winter" {
            int bloom = 0
        }
    }
}

# `glade.usda` sets SEASON to winter for its own selection; this layer
# stack's summer wins.
def "Clearing" (
    references = @./glade.usda@
)
{
}

# A selection that fails to evaluate is an error, and the weaker selection
# `strata.usda` authors applies.
over "Thicket" (
    variants = {
        string season = "`${NOPE}`"
    }
)
{
}

def "Cavern" (
    delete references = @`"./${ROCK}.usda"`@
)
{
}

def "Grotto" (
    prepend references = @`"./${ROCK}.usda"`@
)
{
}

# List ops compose the arcs each layer evaluates and anchors: this layer's
# delete removes the expression reference and payload `strata.usda` adds,
# which evaluate to the same assets.
def "Quarry" (
    delete references = @`"./${ROCK}.usda"`@
)
{
}

def "Mine" (
    delete payload = @`if(${DEEP}, "./basalt.usda", "./granite.usda")`@
)
{
}

# A literal delete removes the arc `strata.usda` authors as an expression
# that evaluates to the same asset and prim, and an expression delete
# removes the arc it authors literally.
def "Pit" (
    delete references = @./granite.usda@</Granite>
)
{
}

def "Shaft" (
    delete payload = @./basalt.usda@</Basalt>
)
{
}

def "Seam" (
    delete references = @`"./${ROCK}.usda"`@</Granite>
)
{
}

def "Lode" (
    delete payload = @`if(${DEEP}, "./basalt.usda", "./granite.usda")`@</Basalt>
)
{
}

# List ops compare anchored asset paths, so spellings of one asset match:
# these deletes remove the arcs `strata.usda` and `deep/vault.usda` add.
def "Flint" (
    delete references = @granite.usda@</Granite>
)
{
}

def "Slate" (
    delete payload = @deep/../basalt.usda@</Basalt>
)
{
}

def "Marble" (
    delete references = @`"deep/../${ROCK}.usda"`@</Granite>
)
{
}

def "Gneiss" (
    delete references = @./deep/granite.usda@</Chip>
)
{
}

# `deep/vault.usda`'s `./granite.usda` anchors to `deep/granite.usda`, another
# asset than this layer's: the delete does not remove it.
def "Quartz" (
    delete references = @./granite.usda@</Chip>
)
{
}

# Only the selections composition reads are evaluated: the `soil`
# selection of the selected `summer` branch is an error; that of the
# unselected `winter` branch is never read.
def "Orchard" (
    variants = {
        string season = "summer"
    }
    variantSets = ["season", "soil"]
)
{
    variantSet "season" = {
        "summer" (
            variants = {
                string soil = "`${CLAY`"
            }
        ) {
        }
        "winter" (
            variants = {
                string soil = "`${SAND`"
            }
        ) {
        }
    }
    variantSet "soil" = {
        "clay" {
            int grain = 1
        }
        "sand" {
            int grain = 2
        }
    }
}
''',
    "deep/vault": '''#usda 1.0

# The same expression as `root.usda`'s evaluates to `./granite.usda` here
# too, but anchors in this directory: another asset, which the root
# layer's delete does not remove.
over "Cavern" (
    prepend references = @`"./${ROCK}.usda"`@
)
{
}

# Both layers add the expression; each anchors it in its own directory, so
# the two arcs differ and both compose.
over "Grotto" (
    prepend references = @`"./${ROCK}.usda"`@
)
{
}

over "Gneiss" (
    prepend references = @./granite.usda@</Chip>
)
{
}

over "Quartz" (
    prepend references = @./granite.usda@</Chip>
)
{
}
''',
    "deep/granite": '''#usda 1.0
(
    defaultPrim = "Chip"
)

def "Chip"
{
    int hardness = 2
}
''',
    "strata": '''#usda 1.0

over "Quarry" (
    prepend references = @`"./${ROCK}.usda"`@
)
{
}

over "Mine" (
    prepend payload = @`if(${DEEP}, "./basalt.usda", "./granite.usda")`@
)
{
}

over "Pit" (
    prepend references = @`"./${ROCK}.usda"`@</Granite>
)
{
}

over "Shaft" (
    prepend payload = @`if(${DEEP}, "./basalt.usda", "./granite.usda")`@</Basalt>
)
{
}

over "Seam" (
    prepend references = @./granite.usda@</Granite>
)
{
}

over "Lode" (
    prepend payload = @./basalt.usda@</Basalt>
)
{
}

over "Flint" (
    prepend references = @./granite.usda@</Granite>
)
{
}

over "Slate" (
    prepend payload = @./basalt.usda@</Basalt>
)
{
}

over "Marble" (
    prepend references = @./granite.usda@</Granite>
)
{
}

over "Outcrop"
{
    int layered = 1
}

def "Thicket" (
    variants = {
        string season = "winter"
    }
    variantSets = "season"
)
{
    variantSet "season" = {
        "summer" {
            int bloom = 3
        }
        "winter" {
            int bloom = 0
        }
    }
}
''',
    "glade": '''#usda 1.0
(
    defaultPrim = "Glade"
    expressionVariables = {
        string SEASON = "winter"
    }
)

def "Glade" (
    variants = {
        string season = "`'${SEASON}'`"
    }
    variantSets = "season"
)
{
    variantSet "season" = {
        "summer" {
            int bloom = 3
        }
        "winter" {
            int bloom = 0
        }
    }
}
''',
    "granite": '''#usda 1.0
(
    defaultPrim = "Granite"
)

def "Granite"
{
    int hardness = 7

    def "Crystal"
    {
        int facets = 6
    }
}
''',
    "basalt": '''#usda 1.0
(
    defaultPrim = "Basalt"
)

def "Basalt"
{
    int hardness = 6
}
''',
    "shale": '''#usda 1.0
(
    defaultPrim = "Shale"
)

def "Shale"
{
    int hardness = 3
}
''',
    "cliff": '''#usda 1.0
(
    defaultPrim = "Cliff"
    expressionVariables = {
        string ROCK = "shale"
        string STONE = "shale"
    }
)

def "Cliff" (
    references = [@`"./${ROCK}.usda"`@, @`"./${STONE}.usda"`@]
)
{
}
''',
    "gorge": '''#usda 1.0
(
    defaultPrim = "Gorge"
    expressionVariables = {
        string DEPTH = "basalt"
    }
)

def "Gorge" (
    references = @./bed.usda@
)
{
}
''',
    "bed": '''#usda 1.0
(
    defaultPrim = "Bed"
    expressionVariables = {
        string ROCK = "shale"
        string DEPTH = "shale"
    }
)

def "Bed" (
    references = [@`"./${ROCK}.usda"`@, @`"./${DEPTH}.usda"`@]
)
{
}
''',
}


def layer_name(identifier):
    """The layer's file name without directory."""
    return os.path.basename(identifier)


def write_layers(directory):
    for name, text in LAYERS.items():
        path = os.path.join(directory, f"{name}.usda")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)


def compose(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    layer_stack = [layer_name(layer.identifier)
                   for layer in stage.GetLayerStack(includeSessionLayers=False)]
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
    errors = sorted({error_record(error) for error in stage.GetCompositionErrors()})
    return {
        "layer_stack": layer_stack,
        "prims": prims,
        "values": values,
        "errors": [list(error) for error in errors],
    }


def error_record(error):
    """`(kind, prim, context, expression, message)` for a composition error.

    `prim` is the path of the prim whose index found the error, empty when
    OpenUSD records none, as for every `PcpErrorVariableExpressionError`.
    The last three are read from such an error's text, `Error evaluating
    expression EXPR for CONTEXT at PATH in @LAYER@: MESSAGE`, and are empty
    for other errors.
    """
    kind = str(error.errorType).split("_", 1)[-1]
    prim = str(error.rootSite.path) if not error.rootSite.path.isEmpty else ""
    context = expression = message = ""
    text = str(error)
    prefix = "Error evaluating expression "
    if text.startswith(prefix):
        head, message = text[len(prefix):].split(": ", 1)
        expression, rest = head.split(" for ", 1)
        context = rest.split(" at ", 1)[0]
    return (kind, prim, context, expression, message)


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/expression_variables_oracle.py",
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
