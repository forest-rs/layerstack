# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records, call by call, what OpenUSD's USDC writer does with values the
USDC writer of layerstack once refused or refuses, and what it reads back.

Usage: usdc_encodings_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/usdc_encodings` by default,
each case's USDA source (`<case>.usda`, but for the cases built below),
the crate file OpenUSD exports for the layer it reads from that file
(`<case>.usdc`, when the export succeeds) and `oracle.json`, which
`tests/conformance/usdc_encodings.rs` replays with `read_usda`,
`save_usdc` and `read_usdc`.

For each case the oracle records three calls: `Sdf.Layer.FindOrOpen` of
the source, `SdfLayer::Export` to a `.usdc`, and `Sdf.Layer.OpenAsAnonymous`
of that file followed by `ExportToString`, which reads every value. A
call that posts errors is recorded as refused, with each error's
commentary (`Tf.ErrorException` lists them); one that does not is
recorded as accepted, the read with whether the layer it reads exports
the same text as the source layer (`same_text`) and that text.

- `unregistered_list_ops`: list-op metadata no schema registers, which
  the text parser keeps as an `SdfUnregisteredValue` holding an
  `SdfUnregisteredValueListOp` of each item's text
  (`textParserHelpers.cpp`), on the layer, a prim, an attribute and a
  relationship, one of each list-op kind, twice the same, beside an
  unregistered dictionary. The crate stores it as such
  (`crateDataTypes.h`, `UnregisteredValueListOp`; `crateFile.cpp`,
  `Write(SdfUnregisteredValue)` through `_RecursiveWrite`).
- `relational_targets`: `usda_parity_oracle.py`'s
  `read_relational_targets`, relationship targets and an attribute
  connection that are relational attributes (`</A.b[/C].d>`) or mappers
  (`</A.b.mapper[/C]>`), some nested or anchored. The crate stores each
  element below the property as a path of its own (`/A.b[/C]` with token
  `[/C]`, then `.d`), adding a target path's bracketed path before its
  parent (`CrateFile::_AddPath`), and reads them back with
  `SdfPath::AppendElementToken`.
- `relationship_default`: a relationship's legacy `default`
  (`rel r.default = </A/B>`), an `SdfPath` value. The crate has no type
  for an `SdfPath` value: `CrateFile::_PackValue` posts "Attempted to
  pack unsupported type 'SdfPath'" and writes an invalid value, which the
  read refuses ("Attempted to unpack unsupported type").
- `anchored_brackets`: the targets of `usda_parity_oracle.py`'s
  `read_relational_relative_targets` that the parser anchors, brackets
  and all (`SdfPath::MakeAbsolutePath`), which save and read back.
- `relative_brackets`: the other two, which keep a relative bracketed
  path (`</X/Y.b[Z].d>`, and
  `</P/Q.b[C].d.mapper[/P/Q/E]>` from `<.b[C].d.mapper[E]>`). The export
  does not return: `CrateFile::_AddPath` adds a target path's bracketed
  path (`GetTargetPath`, here relative) and then its parents
  (`GetParentPath`: `Z`, `.`, `..`, `../..`, ...), which never reach the
  absolute root, until the stack overflows. The export runs in a child
  process, and the oracle records that it was killed (`crashed`).
- `display_unit`: an attribute's `displayUnit`, which the text parser
  holds as a `TfEnum` of its unit (`SdfGetUnitFromName`,
  `pxr/usd/sdf/types.cpp`). The crate has no type for a `TfEnum` either:
  `CrateFile::_PackValue` posts "Attempted to pack unsupported type
  'TfEnum'", and the read refuses the invalid value it writes.

Five cases have no USDA source, since the text parser cannot make them:
each is a layer `Sdf.CopySpec` copies from a USDA source, replacing
fields' values through its value callback (which sets them as they
are), or one built with `Sdf` calls. The oracle records its export, the
read of that file, the type `Usd.Stage` reads each replaced metadata
field as from it (`metadata_types`), and whether the text parser reads
the text that read exports (`open_text`).

- `mapper_connections`: attribute connections to mappers
  (`</A.b.mapper[/C]>`, `</A.b[/C].d.mapper[/E]>`) beside a relational
  attribute. `SdfSchemaBase::IsValidAttributeConnectionPath` keeps
  authoring from making them (`pxr/usd/sdf/schema.cpp`), and the text
  parser refuses them, but the crate stores and reads them as any path,
  and `ExportToString` writes them.
- `property_tail_targets`: connections and relationship targets of the
  other paths below a property, which neither `IsValidAttributeConnectionPath`
  nor `IsValidRelationshipTargetPath` admits: a target path
  (`</A.b[/C]>`), a relational attribute's target (`</A.b[/C].d[/E]>`), a
  mapper argument (`</A.b.mapper[/C].arg>`) and expressions
  (`</A.b.expression>`). The crate stores and reads them as any path,
  `ExportToString` writes them and the text parser refuses them.
- `unregistered_typed_list_ops`: typed list ops (`SdfIntListOp`,
  `SdfInt64ListOp`, `SdfUIntListOp`, `SdfUInt64ListOp`, `SdfTokenListOp`,
  `SdfStringListOp`) under metadata keys no schema registers, on a prim
  and an attribute. The crate stores each as its list-op type, not as an
  `SdfUnregisteredValue`; the text writes each as a list-op statement,
  which the text parser reads as an `SdfUnregisteredValueListOp`.
- `unspecified_prims`: prim specs with no specifier field, as
  `Sdf.CreatePrimInLayer` makes them. `SdfPrimSpec::GetSpecifier` falls
  back to `over`, which the text writes (`over "Q"`); the crate stores no
  field, so the conformance test finds the same file in `save_usdc`'s.
- `dictionary_list_ops`: dictionary entries holding list ops, which the
  crate stores and reads as any value. The text writes each as the list
  op's `TfStringify` text after an empty type name
  (` a = SdfIntListOp(Prepended Items: [1])`, `Sdf_FileIOUtility::_WriteDictionary`),
  which the text parser refuses.

`--check` regenerates every file into a temporary directory and fails
unless each matches the checked-in fixture; for a crate file, unless
OpenUSD reads the same text from both. The crate's bytes depend on how
OpenUSD was built (the Linux source build and the macOS wheel of 26.11.1
write files of the same length with different bytes), so the check
compares what OpenUSD reads from them; the Rust test compares
`save_usdc` with the checked-in file of each case with a USDA source.
"""
import argparse
import difflib
import filecmp
import json
import os
import re
import subprocess
import sys
import tempfile

from pxr import Sdf, Tf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.dont_write_bytecode = True
import usda_parity_oracle  # noqa: E402
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "usdc_encodings"))

CASES = {
    "unregistered_list_ops": """#usda 1.0
(
    prepend layerThing = ["x"]
)

def "A" (
    prepend foo = ["a", "b"]
    append bar = [1, 2.5]
    delete baz = ["c"]
    add qux = [hello]
    reorder quux = ["d", "e"]
)
{
    int x (
        prepend attrThing = ["m"]
    )
    rel r (
        append relThing = [3]
    )
}

def "B" (
    prepend foo = ["a", "b"]
    myDict = {
        int a = 1
    }
)
{
}
""",
    "relationship_default": """#usda 1.0

def "A"
{
    rel r
    rel r.default = </A/B>
    rel s = </A>
    rel s.default = </C.x>
}
""",
    "relational_targets": usda_parity_oracle.CASES["read_relational_targets"][0],
    "anchored_brackets": """#usda 1.0
def "P"
{
    def "Q"
    {
        rel a = <C.b[../E].d>
        rel c = <C.b[D.e[F].g].h>
        rel e = <../S.b[T].d>
        rel f = <C.b[/Abs].d.mapper[M]>
        rel g = <C.b[/Abs.e[F].g].h>
        float h.connect = <C.b[../E].d>
    }
}
""",
    "relative_brackets": """#usda 1.0
def "P"
{
    def "Q"
    {
        rel b = <.b[C].d.mapper[E]>
        rel d = </X/Y.b[Z].d>
    }
}
""",
    "display_unit": """#usda 1.0

def "P"
{
    double a (
        displayUnit = mm
    )
    float b (
        displayUnit = deg
    )
}
""",
}


def unspecified_prims():
    """Prim specs `Sdf.CreatePrimInLayer` makes, with no specifier field
    (`Sdf_UncheckedCreatePrimInLayer`), and a child whose specifier is
    set."""
    layer = Sdf.Layer.CreateAnonymous("unspecified_prims.usda")
    Sdf.CreatePrimInLayer(layer, "/Q/R")
    Sdf.CreatePrimInLayer(layer, "/Q/S").specifier = Sdf.SpecifierDef
    return layer


def dictionary_list_ops():
    """`customData` dictionaries holding list ops, on a prim (beside an
    `int`, and in a nested dictionary beside a string) and an attribute,
    as `SdfSpec::SetInfo` sets them."""
    layer = Sdf.Layer.CreateAnonymous("dictionary_list_ops.usda")
    layer.ImportFromString('#usda 1.0\ndef "P"\n{\n    float v\n}\n')
    layer.GetPrimAtPath("/P").SetInfo("customData", {
        "a": Sdf.IntListOp.Create(prependedItems=[1]),
        "c": Sdf.TokenListOp.Create(appendedItems=["t"]),
        "d": {"x": Sdf.StringListOp.CreateExplicit(["s"]), "y": "kept"},
        "e": 1,
    })
    layer.GetAttributeAtPath("/P.v").SetInfo("customData", {
        "ops": Sdf.Int64ListOp.Create(deletedItems=[2]),
    })
    return layer


# Layers the text parser cannot make: each copies `source` with
# `Sdf.CopySpec`, its value callback replacing the value of each
# `(path, field)` listed by a function of `pxr`'s `Sdf`, or is the layer
# `source` makes when it is a function.
BUILT = {
    "mapper_connections": ("""#usda 1.0
def "P"
{
    float c.connect = </X.y>
    float d.connect = </X.y>
    rel r = </X.y>
}
""", {
        ("/P.c", "connectionPaths"): lambda: Sdf.PathListOp.CreateExplicit(
            [Sdf.Path("/A.b.mapper[/C]")]),
        ("/P.d", "connectionPaths"): lambda: Sdf.PathListOp.Create(prependedItems=[
            Sdf.Path("/A.b[/C].d.mapper[/E]"), Sdf.Path("/A.b[/C].d")]),
    }),
    "property_tail_targets": ("""#usda 1.0
def "P"
{
    float c.connect = </X.y>
    float d.connect = </X.y>
    rel r = </X.y>
    rel s = </X.y>
}
""", {
        ("/P.c", "connectionPaths"): lambda: Sdf.PathListOp.CreateExplicit(
            [Sdf.Path("/A.b[/C]")]),
        ("/P.d", "connectionPaths"): lambda: Sdf.PathListOp.Create(appendedItems=[
            Sdf.Path("/A.b.mapper[/C].arg"), Sdf.Path("/A.b.expression")]),
        ("/P.r", "targetPaths"): lambda: Sdf.PathListOp.CreateExplicit([
            Sdf.Path("/A.b[/C]"), Sdf.Path("/A.b.mapper[/C].arg")]),
        ("/P.s", "targetPaths"): lambda: Sdf.PathListOp.Create(prependedItems=[
            Sdf.Path("/A.b[/C].d[/E]"), Sdf.Path("/A.b.expression"),
            Sdf.Path("/A.b[/C].d.expression")]),
    }),
    "unregistered_typed_list_ops": ("""#usda 1.0
def "P" (
    prepend a = [1]
    prepend b = [1]
    prepend c = [1]
    prepend d = [1]
    prepend e = [1]
    prepend f = [1]
)
{
    int x (
        prepend g = [1]
    )
}
""", {
        ("/P", "a"): lambda: Sdf.IntListOp.Create(prependedItems=[1, -2]),
        ("/P", "b"): lambda: Sdf.Int64ListOp.Create(appendedItems=[3]),
        ("/P", "c"): lambda: Sdf.UIntListOp.CreateExplicit([4]),
        ("/P", "d"): lambda: Sdf.UInt64ListOp.Create(deletedItems=[5]),
        ("/P", "e"): lambda: Sdf.TokenListOp.Create(prependedItems=["a", "b"]),
        ("/P", "f"): lambda: Sdf.StringListOp.Create(appendedItems=["s"]),
        ("/P.x", "g"): lambda: Sdf.TokenListOp.Create(prependedItems=["t"]),
    }),
    "unspecified_prims": (unspecified_prims, {}),
    "dictionary_list_ops": (dictionary_list_ops, {}),
}


def errors(exc):
    """The commentary of each error `exc` lists, without the function,
    file and line OpenUSD names before it; a parse error's commentary
    ends in a newline, which is dropped."""
    found = []
    pattern = r"Error in '[^']*' at line \d+ in file \S+ : '(.*?)\n?'(?=\n|$)"
    for match in re.finditer(pattern, str(exc), re.DOTALL):
        found.append(match.group(1))
    return found


def call(fn):
    """Runs `fn`, returning its result and the errors it posted."""
    try:
        return fn(), []
    except Tf.ErrorException as exc:
        return None, errors(exc) or [str(exc)]


def export_child(source, written):
    """Child process: opens `source`, exports it to `written` and prints
    the errors the export posted, as JSON."""
    layer = Sdf.Layer.FindOrOpen(source)
    ok, posted = call(lambda: layer.Export(written))
    print(json.dumps({"accepted": bool(ok) and not posted, "errors": posted}))


def export(source, written):
    """Exports the layer at `source` to `written` in a child process, so
    that an export that kills its process is recorded as `crashed`."""
    child = subprocess.run(
        [sys.executable, os.path.abspath(__file__), "--export-child", source, written],
        capture_output=True, text=True, timeout=600)
    if child.returncode < 0 or child.returncode > 128:
        return {"accepted": False, "crashed": True, "errors": []}
    if child.returncode != 0:
        raise SystemExit(f"export of {source} failed: {child.stderr}")
    return json.loads(child.stdout.splitlines()[-1])


def reread(record, written, source_text):
    """Records the read of the crate file at `written`."""
    reread_text, posted = call(lambda: Sdf.Layer.OpenAsAnonymous(written).ExportToString())
    read = {"accepted": not posted, "errors": posted}
    if read["accepted"]:
        read["same_text"] = reread_text == source_text
        read["text"] = reread_text
    record["open_usdc"] = read
    return reread_text if read["accepted"] else None


def keep(out, name, written, export_record):
    """Keeps the crate file at `written` as `<name>.usdc` in `out` when its
    export was accepted."""
    if export_record["accepted"]:
        with open(written, "rb") as f:
            data = f.read()
        with open(os.path.join(out, f"{name}.usdc"), "wb") as f:
            f.write(data)


def case(out, name, text):
    source = os.path.join(out, f"{name}.usda")
    with open(source, "w") as f:
        f.write(text)
    record = {}
    layer, posted = call(lambda: Sdf.Layer.FindOrOpen(source))
    record["open_usda"] = {"accepted": not posted and layer is not None, "errors": posted}
    if layer is None:
        return record
    with tempfile.TemporaryDirectory() as tmp:
        written = os.path.join(tmp, f"{name}.usdc")
        record["export_usdc"] = export(source, written)
        keep(out, name, written, record["export_usdc"])
        if not record["export_usdc"].get("crashed"):
            # Crate values are read when asked for: the text asks for all.
            reread(record, written, layer.ExportToString())
    return record


def metadata_types(written, fields):
    """The type of each replaced field that `Usd.Stage` reads as metadata
    from the crate file at `written`, keyed `<path> <field>`."""
    stage = Usd.Stage.Open(written)
    found = {}
    for path, field in fields:
        value = stage.GetObjectAtPath(path).GetAllMetadata().get(field)
        if value is not None:
            found[f"{path} {field}"] = type(value).__name__
    return found


def built(out, name, source, fields):
    """Records `name`: the layer `Sdf.CopySpec` copies from `source`, the
    value of each of `fields` replaced, or the layer `source` makes when
    it is a function."""
    if callable(source):
        layer = source()
    else:
        src = Sdf.Layer.CreateAnonymous(f"{name}_source.usda")
        src.ImportFromString(source)
        layer = Sdf.Layer.CreateAnonymous(f"{name}.usda")

        def value(spec_type, field, src_layer, src_path, in_src, dst_layer, dst_path, in_dst):
            make = fields.get((str(src_path), field))
            return (True, make()) if make else True

        for prim in src.rootPrims:
            Sdf.CopySpec(src, prim.path, layer, prim.path, value, lambda *_: True)
    record = {}
    with tempfile.TemporaryDirectory() as tmp:
        written = os.path.join(tmp, f"{name}.usdc")
        ok, posted = call(lambda: layer.Export(written))
        record["export_usdc"] = {"accepted": bool(ok) and not posted, "errors": posted}
        keep(out, name, written, record["export_usdc"])
        text = reread(record, written, layer.ExportToString())
        if text is not None:
            record["metadata_types"] = metadata_types(written, fields)
    if text is not None:
        _, posted = call(lambda: Sdf.Layer.CreateAnonymous("reread.usda").ImportFromString(text))
        record["open_text"] = {"accepted": not posted, "errors": posted}
    return record


def write(out):
    os.makedirs(out, exist_ok=True)
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "cases": {
            **{name: case(out, name, text) for name, text in CASES.items()},
            **{name: built(out, name, *spec) for name, spec in BUILT.items()},
        },
    }
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def read_text(path):
    """The text of the layer OpenUSD reads from the crate file at `path`."""
    return Sdf.Layer.OpenAsAnonymous(path).ExportToString()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--export-child", nargs=2, metavar=("SOURCE", "WRITTEN"),
                        help=argparse.SUPPRESS)
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if args.export_child:
        export_child(*args.export_child)
        return
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        fresh_names = sorted(os.listdir(tmp))
        committed_names = sorted(os.listdir(FIXTURES))
        if fresh_names != committed_names:
            raise SystemExit(f"{FIXTURES} holds {committed_names}, not {fresh_names}")
        for name in fresh_names:
            fresh = os.path.join(tmp, name)
            committed = os.path.join(FIXTURES, name)
            if filecmp.cmp(fresh, committed, shallow=False):
                continue
            if name.endswith(".usdc"):
                if read_text(fresh) == read_text(committed):
                    continue
            else:
                with open(committed) as a, open(fresh) as b:
                    diff = difflib.unified_diff(a.readlines(), b.readlines(),
                                                "committed", "regenerated", n=2)
                    sys.stdout.writelines(list(diff)[:200])
            raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
