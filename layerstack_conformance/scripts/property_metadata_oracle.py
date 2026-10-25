# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records which property metadata fields OpenUSD's `SetMetadata` accepts.

Usage: property_metadata_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

On a new stage, a `Mesh` gets an attribute `a` (`float`), a non-custom
relationship `r`, its schema attribute `doubleSided` (unauthored) and no
property `nope`. Each call authors one field on one of them, call by call,
recording whether `UsdObject::SetMetadata` accepts it and the value
`GetMetadata` reads back. Values are tagged with the Rust value they
stand for; Python converts each to the field's registered type.

`tests/conformance/property_metadata.rs` replays every call through
`PrimEdit::write_property_metadata`.
"""
import argparse
import json
import pathlib

from pxr import Sdf, Tf, Usd

OUT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "property_metadata" / "oracle.json"

FIELDS = [
    ("documentation", "string", "x"),
    ("documentation", "int", 5),
    ("displayName", "string", "Name"),
    ("hidden", "bool", True),
    ("customData", "dictionary", {"a": 1}),
    ("comment", "string", "note"),
    ("weight", "float", 0.5),
    ("bindMaterialAs", "token", "strongerThanDescendants"),
    ("connectability", "token", "interfaceOnly"),
    ("elementSize", "int", 2),
    ("interpolation", "token", "vertex"),
    ("allowedTokens", "token[]", ["a", "b"]),
    ("colorSpace", "token", "raw"),
    ("noLoadHint", "bool", True),
    ("unknownKey", "int", 1),
    ("typeName", "token", "int"),
    ("custom", "bool", True),
    ("default", "double", 1.0),
]


def plain(value):
    if isinstance(value, (list, tuple)) or type(value).__name__.endswith("Array"):
        return [plain(v) for v in value]
    if isinstance(value, dict):
        return {k: plain(v) for k, v in value.items()}
    if isinstance(value, (bool, int, float, str)) or value is None:
        return value
    return str(value)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stage = Usd.Stage.CreateInMemory()
    prim = stage.DefinePrim("/P", "Mesh")
    prim.CreateAttribute("a", Sdf.ValueTypeNames.Float)
    prim.CreateRelationship("r", custom=False)
    calls = []
    targets = [("a", key, tag, value) for key, tag, value in FIELDS]
    targets += [("r", key, tag, value) for key, tag, value in FIELDS]
    targets += [("doubleSided", "documentation", "string", "schema"),
                ("nope", "documentation", "string", "missing")]
    for name, key, tag, value in targets:
        prop = prim.GetProperty(name) if name != "nope" else prim.GetAttribute(name)
        try:
            accepted = bool(prop.SetMetadata(key, value))
        except (Tf.ErrorException, ValueError):
            accepted = False
        calls.append({
            "property": name,
            "key": key,
            "type": tag,
            "value": value,
            "accepted": accepted,
            "read": plain(prop.GetMetadata(key)) if accepted else None,
        })
    spec = stage.GetRootLayer().GetPropertyAtPath("/P.doubleSided")
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "calls": calls,
        "schema_spec": {
            "exists": spec is not None,
            "custom": spec.custom,
            "variability": str(spec.variability),
            "type_name": str(spec.typeName),
        },
    }
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.check:
        assert OUT.read_text() == text, "oracle.json is out of date"
    else:
        OUT.write_text(text)


main()
