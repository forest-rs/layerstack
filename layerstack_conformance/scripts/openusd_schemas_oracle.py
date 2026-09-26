# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's schema registry for the domains layerstack_schemas ships.

Usage: openusd_schemas_oracle.py [OUT_JSON]

Needs usd-core 26.8 through Python `pxr` (`pip install usd-core==26.8`), the
release `layerstack_schemas` is generated from. Writes
`layerstack_conformance/fixtures/openusd_schemas/registry.json` by default,
which `tests/openusd_schemas.rs` replays against the registry
`layerstack_schemas::openusd` builds.

For every schema the domains' `plugInfo.json` files declare, the vectors
record its kind, family and version, the typed schemas it `IsA` (its
ancestry, itself included), and its prim definition: for a concrete typed
schema `FindConcretePrimDefinition`, for an abstract one
`FindAbstractPrimDefinition`, for a single-apply schema the definition
`BuildComposedPrimDefinition` composes for a typeless prim applying it, and
for a multiple-apply schema those of the instances `fixture` and
`outer:inner`. Each definition records its applied schemas in order and
every property's kind, type name, variability and fallback.
"""
import json
import os
import re
import sys

from pxr import Gf, Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "openusd_schemas", "registry.json"))

# The plugins `layerstack_schemas` generates, as in layerstack_schemagen's
# `DOMAINS`.
DOMAINS = ["usd", "usdGeom", "usdShade", "usdLux"]

# The instance names multiple-apply schemas are recorded with: a plain one,
# and one containing `:` (AOUSD Core 13.3.2).
INSTANCES = ["fixture", "outer:inner"]


def encode(value):
    """A JSON form of a value: quaternions as [real, i, j, k], matrices as
    their rows flattened, arrays and vectors as lists."""
    if value is None:
        return None
    if isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, Sdf.AssetPath):
        return value.path
    if isinstance(value, Sdf.PathExpression):
        return value.GetText()
    if isinstance(value, Sdf.TimeCode):
        return value.GetValue()
    if isinstance(value, (Gf.Quatd, Gf.Quatf, Gf.Quath)):
        return [float(value.GetReal())] + [float(x) for x in value.GetImaginary()]
    if isinstance(value, (Gf.Matrix2d, Gf.Matrix3d, Gf.Matrix4d)):
        return [float(x) for row in value for x in row]
    try:
        return [encode(v) for v in value]
    except TypeError:
        raise TypeError(f"cannot encode {value!r}")


def definition(prim_definition):
    properties = {}
    for name in prim_definition.GetPropertyNames():
        prop = prim_definition.GetPropertyDefinition(name)
        entry = {
            "kind": "attribute" if prop.IsAttribute() else "relationship",
        }
        if prop.IsAttribute():
            attr = prim_definition.GetAttributeDefinition(name)
            entry["type_name"] = str(attr.GetTypeName())
            entry["variability"] = (
                "uniform" if attr.GetVariability() == Sdf.VariabilityUniform else "varying")
            entry["fallback"] = encode(attr.GetFallbackValue())
        properties[str(name)] = entry
    return {
        "applied_schemas": [str(s) for s in prim_definition.GetAppliedAPISchemas()],
        "properties": properties,
    }


def plug_info_types(pxr_dir, plugin):
    path = os.path.join(pxr_dir, "pluginfo", plugin, "resources", "plugInfo.json")
    text = "\n".join(
        line for line in open(path).read().splitlines()
        if not line.lstrip().startswith("#"))
    types = {}
    for entry in json.loads(text)["Plugins"]:
        types.update(entry["Info"].get("Types", {}))
    return types


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    import pxr
    pxr_dir = os.path.dirname(pxr.__file__)
    registry = Usd.SchemaRegistry()
    schemas = []
    for plugin in DOMAINS:
        for info in plug_info_types(pxr_dir, plugin).values():
            identifier = info.get("schemaIdentifier")
            if not identifier:
                continue
            schema_info = Usd.SchemaRegistry.FindSchemaInfo(identifier)
            kind = str(schema_info.kind).split(".")[-1]
            ancestry = []
            for ancestor in schema_info.type.GetAllAncestorTypes():
                ancestor_info = Usd.SchemaRegistry.FindSchemaInfo(ancestor)
                if ancestor_info and Usd.SchemaRegistry.IsTyped(ancestor):
                    ancestry.append(str(ancestor_info.identifier))
            record = {
                "name": identifier,
                "plugin": plugin,
                "kind": kind,
                "family": str(schema_info.family),
                "version": schema_info.version,
                "is_a": sorted(ancestry),
                "definitions": [],
            }
            if kind == "ConcreteTyped":
                record["definitions"].append({
                    "type": identifier,
                    "applied": [],
                    **definition(registry.FindConcretePrimDefinition(identifier)),
                })
            elif kind == "AbstractTyped":
                record["definitions"].append({
                    "type": identifier,
                    "applied": [],
                    **definition(registry.FindAbstractPrimDefinition(identifier)),
                })
            elif kind == "SingleApplyAPI":
                record["definitions"].append({
                    "type": "",
                    "applied": [identifier],
                    **definition(registry.BuildComposedPrimDefinition("", [identifier])),
                })
            elif kind == "MultipleApplyAPI":
                for instance in INSTANCES:
                    applied = f"{identifier}:{instance}"
                    record["definitions"].append({
                        "type": "",
                        "applied": [applied],
                        **definition(registry.BuildComposedPrimDefinition("", [applied])),
                    })
            schemas.append(record)
    schemas.sort(key=lambda s: s["name"])
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump({
            "openusd_version": ".".join(str(v) for v in Usd.GetVersion()),
            "domains": DOMAINS,
            "instances": INSTANCES,
            "schemas": schemas,
        }, f, indent=1, sort_keys=True)
        f.write("\n")
    print(f"wrote {out}: {len(schemas)} schemas")


if __name__ == "__main__":
    main()
