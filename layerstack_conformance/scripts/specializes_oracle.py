# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD places specializes arcs in strength order.

Usage: specializes_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/specializes_placement` by
default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/specializes_placement.rs` replays against layerstack.

Specializes are the weakest arcs of the whole prim index: OpenUSD propagates
every specializes node to the root of the graph and ranks it after every
other arc, including arcs of other references and payloads (AOUSD Core
§10.4.1; `pxr/usd/pcp/primIndex.cpp`, `_EvalImpliedSpecializes`). Each case
below reaches a specializes arc through another arc and authors a competing
opinion in an arc that is weaker than that outer arc but not a specializes.

For every composed prim the vectors record its prim stack, and for every
attribute its resolved default.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "specializes_placement"))

LAYERS = {
    "root": '''#usda 1.0

# The minimized case: a specializes reached through a reference is weaker
# than a payload of the referencing prim.
def "Minimal" (
    references = </Ref>
    payload = </Payload>
)
{
}

over "Ref" (
    specializes = </Class>
)
{
}

class "Class"
{
    int x = 1
}

over "Payload"
{
    int x = 2
}

# The same through a reference to another layer stack.
def "External" (
    references = @./ref.usda@</Ref>
    payload = @./payload.usda@</Payload>
)
{
}

# Through two levels of references.
def "Nested" (
    references = @./outer.usda@</Outer>
    payload = @./payload.usda@</Payload>
)
{
}

# Through a reference authored in a selected variant: the specialized class
# is weaker than a payload of the prim.
def "InVariant" (
    payload = </Payload>
    variants = {
        string v = "a"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "a" (
            prepend references = </Ref>
        ) {
        }
    }
}

# A specializes authored inside a specialized class is weaker than the
# whole class, and its implied copy under the instance ranks before it.
def "Chain" (
    specializes = </Base>
)
{
    over "Impl"
    {
        int y = 2
    }
}

class "Base"
{
    class "Impl"
    {
        int y = 1
    }

    def "Child" (
        specializes = </Base/Impl>
    )
    {
    }
}

# Sibling specializes: everything the stronger sibling brings in,
# including the class it specializes in turn, outranks the weaker sibling.
class "SiblingsC"
{
    int x = 3
}

class "SiblingsA" (
    specializes = </SiblingsC>
)
{
}

class "SiblingsB"
{
    int x = 2
}

def "Siblings" (
    specializes = [</SiblingsA>, </SiblingsB>]
)
{
}

# The same siblings in the other order.
def "SiblingsReversed" (
    specializes = [</SiblingsB>, </SiblingsA>]
)
{
}

# A deeper chain beside a sibling: A -> C -> D, then B.
class "DeepD"
{
    int x = 4
}

class "DeepC" (
    specializes = </DeepD>
)
{
}

class "DeepA" (
    specializes = </DeepC>
)
{
}

class "DeepB"
{
    int x = 2
}

def "Deep" (
    specializes = [</DeepA>, </DeepB>]
)
{
}

# A nested chain under both siblings: A -> C and B -> D.
class "BothC"
{
    int x = 3
}

class "BothD"
{
    int x = 5
}

class "BothA" (
    specializes = </BothC>
)
{
}

class "BothB" (
    specializes = </BothD>
)
{
}

def "Both" (
    specializes = [</BothA>, </BothB>]
)
{
}
''',
    "ref": '''#usda 1.0

over "Ref" (
    specializes = </Class>
)
{
}

class "Class"
{
    int x = 1
}
''',
    "outer": '''#usda 1.0

over "Outer" (
    references = @./ref.usda@</Ref>
)
{
}
''',
    "payload": '''#usda 1.0

over "Payload"
{
    int x = 2
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
        "generator": "layerstack_conformance/scripts/specializes_oracle.py",
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
