# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's value type registry, the type names attributes declare.

Usage: value_types_oracle.py [--check] [OUT]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes `layerstack_conformance/fixtures/value_types/oracle.json` by default,
which `tests/conformance/value_types.rs` checks layerstack's value type table
(`layerstack::ValueType`) against.

For every name `Sdf.ValueTypeNames` registers (`Sdf_ValueTypeRegistry`,
filled by `_RegisterStandardTypes` in `pxr/usd/sdf/schema.cpp`), the oracle
records whether it is an array type, the name of its scalar and of its array
form (empty when it has none), its role, the C++ type its values hold
(`SdfValueTypeName::GetCPPTypeName`, which unlike the `TfType` name is the
same on every platform), and its aliases, the other names
`Sdf.ValueTypeNames.Find` takes for it.

`--check` rewrites the oracle into a temporary file and fails unless it
matches the checked-in one.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
ORACLE = os.path.normpath(os.path.join(HERE, "..", "fixtures", "value_types", "oracle.json"))


def records():
    types = {}
    for attr in dir(Sdf.ValueTypeNames):
        value = getattr(Sdf.ValueTypeNames, attr)
        if isinstance(value, Sdf.ValueTypeName):
            types[str(value)] = value
    out = []
    for name in sorted(types):
        ty = types[name]
        out.append({
            "name": name,
            "is_array": ty.isArray,
            "scalar": str(ty.scalarType),
            "array": str(ty.arrayType),
            "role": str(ty.role),
            "type": ty.cppTypeName,
            "aliases": sorted(alias for alias in ty.aliasesAsStrings if alias != name),
        })
    return out


def write(path):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    _, minor, patch = Usd.GetVersion()
    with open(path, "w") as f:
        json.dump({"openusd_version": f"{minor}.{patch}", "types": records()}, f, indent=1,
                  sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=ORACLE)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "oracle.json")
        write(path)
        if not filecmp.cmp(path, ORACLE, shallow=False):
            raise SystemExit(f"{ORACLE} is stale")


if __name__ == "__main__":
    main()
