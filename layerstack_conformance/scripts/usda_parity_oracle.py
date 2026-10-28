# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's text format reads and writes for small USDA
layers, each written to show one rule of its parser or writer.

Usage: usda_parity_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/usda_parity` by default,
`oracle.json`, which `tests/conformance/usda_parity.rs` replays against
layerstack's USDA reader and writer.

For each case of `CASES`, its source is written to a file and opened with
`Sdf.Layer.OpenAsAnonymous`. The oracle records whether OpenUSD reads it
(`reads`), and then `ExportToString` of the layer (`text`) and, for each
property path the case queries, the composed targets of a relationship
(`UsdRelationship::GetTargets`) or connections of an attribute
(`UsdAttribute::GetConnections`) on a stage opened on the layer
(`targets`). For a file OpenUSD refuses it records the first line of the
error (`error`) with the file's location removed.

The cases:

- `statements_*`: several list-op statements for one field, on a
  relationship, a connection, references, inherits, `variantSets` and
  metadata (`apiSchemas`). OpenUSD's parser sets each statement's list
  with `SdfListOp::SetItems` (`_SetListOpItemsWithError`,
  `pxr/usd/sdf/textParserHelpers.h`): a later statement of one kind
  replaces the earlier one's items, an explicit statement drops the edits
  and an edit statement drops the explicit list
  (`SdfListOp::_SetExplicit`, `pxr/usd/sdf/listOp.cpp`).

`--check` rewrites `oracle.json` into a temporary directory and fails
unless it matches the checked-in fixture.
"""
import argparse
import filecmp
import json
import os
import re
import tempfile

from pxr import Sdf, Tf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "usda_parity"))

# name -> (source, property paths whose composed targets are recorded)
CASES = {
    "statements_prepend_after_explicit_rel": ('''#usda 1.0

def "P"
{
    rel s = </A>
    prepend rel s = </B>
}
''', ["/P.s"]),
    "statements_repeated_prepend_rel": ('''#usda 1.0

def "P"
{
    prepend rel t = [</A>, </B>]
    prepend rel t = </C>
    append rel t = </D>
}
''', ["/P.t"]),
    "statements_explicit_after_prepend_rel": ('''#usda 1.0

def "P"
{
    prepend rel u = </A>
    rel u = []
}
''', ["/P.u"]),
    "statements_repeated_delete_rel": ('''#usda 1.0

def "P"
{
    delete rel r = </A>
    delete rel r = </B>
    append rel r = [</A>, </B>, </C>]
}
''', ["/P.r"]),
    "statements_explicit_after_add_connect": ('''#usda 1.0

def "P"
{
    add float c.connect = </X.y>
    float c.connect = None
}
''', ["/P.c"]),
    "statements_prepend_after_explicit_connect": ('''#usda 1.0

def "P"
{
    float c.connect = [</X.a>, </X.b>]
    prepend float c.connect = </X.c>
    append float c.connect = </X.d>
    append float c.connect = </X.e>
}
''', ["/P.c"]),
    "statements_explicit_after_edits_refs": ('''#usda 1.0

def "P" (
    delete references = @a.usda@
    add references = @b.usda@
    prepend references = @c.usda@
    references = @d.usda@
)
{
}
''', []),
    "statements_edits_after_explicit_refs": ('''#usda 1.0

def "P" (
    references = @d.usda@
    prepend references = @c.usda@
    prepend references = @e.usda@
    append references = @f.usda@
)
{
}
''', []),
    "statements_inherits_in_variant": ('''#usda 1.0

def "P" (
    variants = {
        string v = "one"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "one" (
            inherits = </A>
            prepend inherits = </B>
            prepend inherits = </C>
        ) {
        }
    }
}

class "A"
{
}

class "B"
{
}

class "C"
{
}
''', []),
    "statements_variant_sets": ('''#usda 1.0

def "P" (
    variantSets = ["a", "b"]
    append variantSets = "c"
)
{
}
''', []),
    "statements_explicit_after_prepend_meta": ('''#usda 1.0

def "P" (
    prepend apiSchemas = ["A"]
    apiSchemas = []
)
{
}
''', []),
    "statements_prepend_after_explicit_meta": ('''#usda 1.0

def "P" (
    apiSchemas = ["A"]
    prepend apiSchemas = ["B"]
)
{
}
''', []),
    "statements_repeated_append_meta": ('''#usda 1.0

def "P" (
    append apiSchemas = ["A", "B"]
    prepend apiSchemas = ["C"]
    append apiSchemas = ["D"]
)
{
}
''', []),
}


def error_line(error):
    """The first line of OpenUSD's message, without the file's location."""
    lines = [line.strip() for line in str(error).splitlines() if line.strip()]
    text = lines[0] if lines else str(error)
    text = re.sub(r"'[^']*/case\.usda:\d+:\d+: ", "'", text)
    return re.sub(r"Error in '[^']*' at line \d+ in file [^ ]* : ", "", text)


def record(directory, source, queries):
    path = os.path.join(directory, "case.usda")
    with open(path, "w") as f:
        f.write(source)
    try:
        layer = Sdf.Layer.OpenAsAnonymous(path)
    except Tf.ErrorException as e:
        return {"reads": False, "error": error_line(e)}
    if not layer:
        return {"reads": False, "error": "no layer"}
    out = {"reads": True, "text": layer.ExportToString()}
    if queries:
        stage = Usd.Stage.Open(layer)
        targets = {}
        for query in queries:
            prop = stage.GetPropertyAtPath(query)
            if isinstance(prop, Usd.Relationship):
                items = prop.GetTargets()
            else:
                items = prop.GetConnections()
            targets[query] = [str(p) for p in items]
        out["targets"] = targets
    return out


def write(out):
    os.makedirs(out, exist_ok=True)
    _, minor, patch = Usd.GetVersion()
    cases = {}
    with tempfile.TemporaryDirectory() as scratch:
        for name, (source, queries) in CASES.items():
            case = {"source": source}
            case.update(record(scratch, source, queries))
            cases[name] = case
    oracle = {"openusd_version": f"{minor}.{patch}", "cases": cases}
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        if not filecmp.cmp(os.path.join(tmp, "oracle.json"),
                           os.path.join(FIXTURES, "oracle.json"), shallow=False):
            raise SystemExit(f"oracle.json differs from {FIXTURES}")


if __name__ == "__main__":
    main()
