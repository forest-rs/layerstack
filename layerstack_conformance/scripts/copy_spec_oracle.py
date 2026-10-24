# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's SdfCopySpec copies and how it remaps paths.

Usage: copy_spec_oracle.py [FIXTURES [OUT]]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Each case reloads `fixtures/copy_spec/src.usda` and `dst.usda` and makes
one `Sdf.CopySpec` call, recording whether OpenUSD accepted it and then
every spec of the destination layer: prim specs with their specifier, type
name, inherits, specializes and references, and their properties with
their default value, targets or connections. `scene.usda` only sublayers
both layers, so that `tests/conformance/copy_spec.rs` can load them into one store and
make the same copies with `Transaction::copy_spec`.

Writes `fixtures/copy_spec/oracle.json` by default.
"""
import json
import os
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "copy_spec"))

# (name, source layer, source path, destination layer, destination path)
CASES = [
    ("prim_replaces_destination", "src", "/A", "src", "/B"),
    ("prim_into_another_layer", "src", "/A", "dst", "/P/Q"),
    ("property", "src", "/A.y", "dst", "/P.y2"),
    ("relationship_targets_follow_the_prim", "src", "/A.look", "dst", "/P.look"),
    ("missing_source", "src", "/Nope", "dst", "/X"),
    ("prim_to_property", "src", "/Outside", "dst", "/P.x"),
    ("property_to_missing_prim", "src", "/A.y", "dst", "/Nope.y"),
]


def list_op(op, item=str):
    return {
        "explicit": [item(x) for x in op.explicitItems] if op.isExplicit else None,
        "prepend": [item(x) for x in op.prependedItems],
        "append": [item(x) for x in op.appendedItems],
        "delete": [item(x) for x in op.deletedItems],
    }


def dump(layer):
    """Every prim spec of `layer` (variant branches included) and its
    path-bearing fields."""
    out = {}

    def visit(path):
        if not (path.IsPrimPath() or path.IsPrimVariantSelectionPath()):
            return
        spec = layer.GetObjectAtPath(path)
        if isinstance(spec, Sdf.VariantSetSpec):
            return
        if isinstance(spec, Sdf.VariantSpec):
            spec = spec.primSpec
        if spec is None or path == Sdf.Path.absoluteRootPath:
            return
        record = {}
        if not path.IsPrimVariantSelectionPath():
            record["specifier"] = str(spec.specifier).replace("Sdf.Specifier", "").lower()
            if spec.typeName:
                record["typeName"] = spec.typeName
        for key in ("inheritPaths", "specializes"):
            if spec.HasInfo(key):
                record[key] = list_op(spec.GetInfo(key))
        if spec.HasInfo("references"):
            record["references"] = list_op(
                spec.GetInfo("references"), lambda r: [r.assetPath, str(r.primPath)]
            )
        properties = {}
        for prop in spec.properties:
            entry = {}
            if isinstance(prop, Sdf.AttributeSpec):
                value = prop.default
                if isinstance(value, Sdf.PathExpression):
                    entry["default"] = value.GetText()
                elif value is not None:
                    entry["default"] = value
                if prop.HasInfo("connectionPaths"):
                    entry["targets"] = list_op(prop.GetInfo("connectionPaths"))
            else:
                if prop.HasInfo("targetPaths"):
                    entry["targets"] = list_op(prop.GetInfo("targetPaths"))
            properties[prop.name] = entry
        if properties:
            record["properties"] = properties
        out[str(path)] = record

    layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    return out


def main():
    fixtures = sys.argv[1] if len(sys.argv) > 1 else FIXTURES
    out_path = sys.argv[2] if len(sys.argv) > 2 else os.path.join(fixtures, "oracle.json")
    layers = {name: Sdf.Layer.FindOrOpen(os.path.join(fixtures, name + ".usda")) for name in ("src", "dst")}
    cases = []
    for name, src, src_path, dst, dst_path in CASES:
        for layer in layers.values():
            layer.Reload(force=True)
        try:
            accepted = bool(Sdf.CopySpec(layers[src], src_path, layers[dst], dst_path))
        except Exception:
            accepted = False
        cases.append({
            "name": name,
            "from": [src, src_path],
            "to": [dst, dst_path],
            "accepted": accepted,
            "layer": dump(layers[dst]),
        })
    for layer in layers.values():
        layer.Reload(force=True)
    _, minor, patch = Usd.GetVersion()
    with open(out_path, "w") as f:
        json.dump({"openusd_version": f"{minor}.{patch}", "cases": cases}, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
