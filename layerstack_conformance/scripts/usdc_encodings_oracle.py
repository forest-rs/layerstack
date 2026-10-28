# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records, call by call, what OpenUSD's USDC writer does with values the
USDC writer of layerstack once refused, and what it reads back.

Usage: usdc_encodings_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/usdc_encodings` by default,
each case's USDA source (`<case>.usda`), the crate file OpenUSD exports
for the layer it reads from that file (`<case>.usdc`, when the export
succeeds) and `oracle.json`, which `tests/conformance/usdc_encodings.rs`
replays with `read_usda`, `save_usdc` and `read_usdc`.

For each case the oracle records three calls: `Sdf.Layer.FindOrOpen` of
the source, `SdfLayer::Export` to a `.usdc`, and `Sdf.Layer.OpenAsAnonymous`
of that file followed by `ExportToString`, which reads every value. A
call that posts errors is recorded as refused, with each error's
commentary (`Tf.ErrorException` lists them); one that does not is
recorded as accepted, the export with the file's length and 64-bit
FNV-1a hash and the read with whether the layer it reads exports the
same text as the source layer (`same_text`) and that text.

- `unregistered_list_ops`: list-op metadata no schema registers, which
  the text parser keeps as an `SdfUnregisteredValue` holding an
  `SdfUnregisteredValueListOp` of each item's text
  (`textParserHelpers.cpp`), on the layer, a prim, an attribute and a
  relationship, one of each list-op kind, twice the same, beside an
  unregistered dictionary. The crate stores it as such
  (`crateDataTypes.h`, `UnregisteredValueListOp`; `crateFile.cpp`,
  `Write(SdfUnregisteredValue)` through `_RecursiveWrite`).

`--check` regenerates every file into a temporary directory and fails
unless each matches the checked-in fixture.
"""
import argparse
import difflib
import filecmp
import json
import os
import re
import sys
import tempfile

from pxr import Sdf, Tf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
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
}


def fnv1a64(data):
    h = 0xcbf29ce484222325
    for byte in data:
        h ^= byte
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


def errors(exc):
    """The commentary of each error `exc` lists, without the function,
    file and line OpenUSD names before it."""
    found = []
    for line in str(exc).splitlines():
        match = re.match(r"\s*Error in '[^']*' at line \d+ in file \S+ : '(.*)'$", line)
        if match:
            found.append(match.group(1))
    return found


def call(fn):
    """Runs `fn`, returning its result and the errors it posted."""
    try:
        return fn(), []
    except Tf.ErrorException as exc:
        return None, errors(exc)


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
        ok, posted = call(lambda: layer.Export(written))
        export = {"accepted": bool(ok) and not posted, "errors": posted}
        with open(written, "rb") as f:
            data = f.read()
        if export["accepted"]:
            export["bytes"] = len(data)
            export["fnv1a64"] = fnv1a64(data)
            with open(os.path.join(out, f"{name}.usdc"), "wb") as f:
                f.write(data)
        record["export_usdc"] = export
        # Crate values are read when asked for: the text asks for all.
        reread_text, posted = call(lambda: Sdf.Layer.OpenAsAnonymous(written).ExportToString())
    read = {"accepted": not posted, "errors": posted}
    if read["accepted"]:
        read["same_text"] = reread_text == layer.ExportToString()
        read["text"] = reread_text
    record["open_usdc"] = read
    return record


def write(out):
    os.makedirs(out, exist_ok=True)
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "cases": {name: case(out, name, text) for name, text in CASES.items()},
    }
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
        fresh_names = sorted(os.listdir(tmp))
        committed_names = sorted(os.listdir(FIXTURES))
        if fresh_names != committed_names:
            raise SystemExit(f"{FIXTURES} holds {committed_names}, not {fresh_names}")
        for name in fresh_names:
            fresh = os.path.join(tmp, name)
            committed = os.path.join(FIXTURES, name)
            if filecmp.cmp(fresh, committed, shallow=False):
                continue
            if not name.endswith(".usdc"):
                with open(committed) as a, open(fresh) as b:
                    diff = difflib.unified_diff(a.readlines(), b.readlines(),
                                                "committed", "regenerated", n=2)
                    sys.stdout.writelines(list(diff)[:200])
            raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
