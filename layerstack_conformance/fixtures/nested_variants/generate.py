# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Generates the nested-variant fixtures and their oracle results.

Each case is one layer whose prim `/P` nests variant sets two or three
levels deep inside its own branches. Different outer branches reuse the
same inner branch names, and every innermost branch authors a child `C` with
one composition arc, an attribute, and a child prim, all naming the outer
branches that enclose it. Only the branch whose every enclosing selection is
selected may contribute (AOUSD Core §7.3.6, §10.3.2.5).

Every case is written as `<case>.usda` and as `<case>.usdc` by OpenUSD
itself. `expected.json` records what the same OpenUSD composes for `/P/C`:
whether it exists, its child names, and the default of each attribute.
`layerstack_conformance/tests/variant_selection.rs` composes the fixtures and
compares.

Pinned oracle: `usd-core` 26.8 from PyPI (OpenUSD v26.08):

    python3 -m venv venv && venv/bin/pip install usd-core==26.8
    venv/bin/python layerstack_conformance/fixtures/nested_variants/generate.py
"""

import itertools
import json
import os

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))

# `(field, target)` for each arc kind; every target is a prim of the layer.
ARCS = {
    "references": "references",
    "payload": "payload",
    "inherits": "inherits",
    "specializes": "specializes",
}

# The nesting of `/P`'s variant sets: each level's set name and branches.
# The two-level shape is the review reproduction: both `outer` branches
# author the same `inner=i` branch. The three-level shape also reuses the
# `mid` branch names under both `outer` branches.
SHAPES = {
    "two": [("outer", ["a", "b"]), ("inner", ["i"])],
    "three": [("outer", ["a", "b"]), ("mid", ["m", "n"]), ("inner", ["i"])],
}


def target_name(path):
    return "T_" + "_".join(path)


def branch_body(levels, path, arc, indent):
    """USDA for the branches below `path` (the branches chosen so far)."""
    pad = " " * indent
    if len(path) == len(levels):
        tag = "_".join(path)
        return (
            f'{pad}def "C" (\n'
            f"{pad}    {ARCS[arc]} = </{target_name(path)}>\n"
            f"{pad})\n"
            f"{pad}{{\n"
            f'{pad}    string from = "{tag}"\n'
            f'{pad}    def "G_{tag}"\n'
            f"{pad}    {{\n"
            f"{pad}    }}\n"
            f"{pad}}}\n"
        )
    name, branches = levels[len(path)]
    out = f'{pad}variantSet "{name}" = {{\n'
    for branch in branches:
        nested = ""
        if len(path) + 1 < len(levels):
            nested = f' (\n{pad}        prepend variantSets = "{levels[len(path) + 1][0]}"\n{pad}    )'
        out += f'{pad}    "{branch}"{nested} {{\n'
        out += branch_body(levels, path + [branch], arc, indent + 8)
        out += f"{pad}    }}\n"
    out += f"{pad}}}\n"
    return out


def case_source(levels, arc, selection):
    out = "#usda 1.0\n"
    for path in itertools.product(*(branches for _, branches in levels)):
        tag = "_".join(path)
        out += f'def "{target_name(path)}"\n{{\n    int x_{tag} = 1\n}}\n'
    selections = "\n".join(
        f'        string {name} = "{choice}"'
        for (name, _), choice in zip(levels, selection)
    )
    out += (
        'def "P" (\n'
        f'    prepend variantSets = "{levels[0][0]}"\n'
        "    variants = {\n"
        f"{selections}\n"
        "    }\n"
        ")\n"
        "{\n"
    )
    out += branch_body(levels, [], arc, 4)
    out += "}\n"
    return out


def oracle(layer):
    stage = Usd.Stage.Open(layer)
    prim = stage.GetPrimAtPath("/P/C")
    if not prim:
        return {"exists": False}
    return {
        "exists": True,
        "children": [child.GetName() for child in prim.GetAllChildren()],
        "attributes": {
            attr.GetName(): attr.Get()
            for attr in prim.GetAttributes()
            if attr.Get() is not None
        },
    }


def main():
    assert Usd.GetVersion() == (0, 26, 8), Usd.GetVersion()
    expected = {}
    for shape, levels in SHAPES.items():
        choices = [branches for _, branches in levels]
        for arc in ARCS:
            for selection in itertools.product(*choices):
                case = f"{shape}_{arc}_{'_'.join(selection)}"
                source = case_source(levels, arc, selection)
                usda = os.path.join(HERE, f"{case}.usda")
                with open(usda, "w") as f:
                    f.write(source)
                layer = Sdf.Layer.FindOrOpen(usda)
                layer.Export(os.path.join(HERE, f"{case}.usdc"))
                expected[case] = oracle(layer)
    expected = {
        "generator": "usd-core 26.8 (OpenUSD v26.08)",
        "cases": expected,
    }
    with open(os.path.join(HERE, "expected.json"), "w") as f:
        json.dump(expected, f, indent=2, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
