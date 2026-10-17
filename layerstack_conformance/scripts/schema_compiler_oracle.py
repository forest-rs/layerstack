# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's schema registry reads from the schema compiler's
fixture plugins; run with usd-core 26.8.

Usage: schema_compiler_oracle.py [--check]

`fixtures/schema_compiler/lab` and `fixtures/schema_compiler/bench` each
hold a `schema.usda` and the `generatedSchema.usda` and `plugInfo.json`
`layerstack_schemagen compile` writes for it, and
`fixtures/schema_compiler/installed/<plugin>/resources` what
`layerstack_schemagen compile --install` writes, from OpenUSD's template.
This registers the installed plugins with usd-core, reading their
`plugInfo.json` as strict JSON, and records, for each schema they declare, its kind, family
and version, the typed schemas it `IsA`, its prim definitions (a typed
schema's own; a typeless prim applying an applied schema, for a
multiple-apply schema once per recorded instance name), each with its
applied schemas and every property's kind, type, variability, fallback and
`allowedTokens`, and for an applied schema where it can only be applied
(per instance name), which probed instance names it allows and what it is
auto-applied to. It writes `fixtures/schema_compiler/oracle.json`, which
`tests/schema_compiler.rs` replays against layerstack's registry built from
the same files; with `--check` it writes nothing and fails on a difference.
"""
import argparse
import json
import math
from pathlib import Path

from pxr import Gf, Plug, Sdf, Tf, Usd

FIXTURE = Path(__file__).resolve().parent.parent / "fixtures" / "schema_compiler"
PLUGINS = ["lab", "bench"]
INSTANCES = ["red", "fixture"]
INSTANCE_PROBES = ["red", "green", "blue", "fixture", "outer:inner", "gain", "1st"]


def encode(value):
    """A JSON form of a value: vectors and arrays as lists, non-finite floats
    as strings."""
    if value is None:
        return None
    if isinstance(value, float) and not math.isfinite(value):
        return str(value)
    if isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, Sdf.AssetPath):
        return value.path
    if isinstance(value, (Gf.Quatd, Gf.Quatf, Gf.Quath)):
        return [float(value.GetReal())] + [float(x) for x in value.GetImaginary()]
    return [encode(v) for v in value]


def definition(prim_definition):
    properties = {}
    for name in prim_definition.GetPropertyNames():
        prop = prim_definition.GetPropertyDefinition(name)
        entry = {"kind": "attribute" if prop.IsAttribute() else "relationship"}
        if prop.IsAttribute():
            attr = prim_definition.GetAttributeDefinition(name)
            entry["type_name"] = str(attr.GetTypeName())
            entry["variability"] = (
                "uniform" if attr.GetVariability() == Sdf.VariabilityUniform else "varying")
            entry["fallback"] = encode(attr.GetFallbackValue())
            tokens = prim_definition.GetPropertyMetadata(name, "allowedTokens")
            entry["allowed_tokens"] = [str(t) for t in tokens] if tokens else []
        properties[str(name)] = entry
    return {
        "applied_schemas": [str(s) for s in prim_definition.GetAppliedAPISchemas()],
        "properties": properties,
    }


def resources(plugin):
    return FIXTURE / "installed" / plugin / "resources"


def plug_info_types(plugin):
    types = {}
    for entry in json.loads((resources(plugin) / "plugInfo.json").read_text())["Plugins"]:
        types.update(entry["Info"].get("Types", {}))
    return types


def record(plugin, info, registry, auto_apply):
    identifier = info["schemaIdentifier"]
    schema_info = Usd.SchemaRegistry.FindSchemaInfo(identifier)
    kind = str(schema_info.kind).split(".")[-1]
    ancestry = []
    for ancestor in schema_info.type.GetAllAncestorTypes():
        ancestor_info = Usd.SchemaRegistry.FindSchemaInfo(ancestor)
        if ancestor_info and Usd.SchemaRegistry.IsTyped(ancestor):
            ancestry.append(str(ancestor_info.identifier))
    entry = {
        "name": identifier,
        "plugin": plugin,
        "kind": kind,
        "family": str(schema_info.family),
        "version": schema_info.version,
        "is_a": sorted(ancestry),
        "definitions": [],
    }
    if kind == "ConcreteTyped":
        entry["definitions"].append({
            "type": identifier, "applied": [],
            **definition(registry.FindConcretePrimDefinition(identifier)),
        })
    elif kind == "AbstractTyped":
        entry["definitions"].append({
            "type": identifier, "applied": [],
            **definition(registry.FindAbstractPrimDefinition(identifier)),
        })
    elif kind == "SingleApplyAPI":
        entry["definitions"].append({
            "type": "", "applied": [identifier],
            **definition(registry.BuildComposedPrimDefinition("", [identifier])),
        })
    elif kind == "MultipleApplyAPI":
        for instance in INSTANCES:
            applied = f"{identifier}:{instance}"
            entry["definitions"].append({
                "type": "", "applied": [applied],
                **definition(registry.BuildComposedPrimDefinition("", [applied])),
            })
    if kind in ("SingleApplyAPI", "MultipleApplyAPI"):
        instances = [""] + (INSTANCES if kind == "MultipleApplyAPI" else [])
        entry["can_only_apply_to"] = [
            [instance, [str(t) for t in
                        Usd.SchemaRegistry.GetAPISchemaCanOnlyApplyToTypeNames(
                            identifier, instance)]]
            for instance in instances
        ]
        entry["auto_apply_to"] = sorted(str(t) for t in auto_apply.get(identifier, []))
    if kind == "MultipleApplyAPI":
        entry["allowed_instance_names"] = [
            [instance, bool(Usd.SchemaRegistry.IsAllowedAPISchemaInstanceName(
                identifier, instance))]
            for instance in INSTANCE_PROBES
        ]
    return entry


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    registered = Plug.Registry().RegisterPlugins([str(resources(p)) for p in PLUGINS])
    names = sorted(p.name for p in registered)
    if names != ["labBenches", "labInstruments"]:
        raise SystemExit(f"registered {names}")
    registry = Usd.SchemaRegistry()
    auto_apply = Usd.SchemaRegistry.GetAutoApplyAPISchemas()
    schemas = [
        record(plugin, info, registry, auto_apply)
        for plugin in PLUGINS
        for info in plug_info_types(plugin).values()
    ]
    schemas.sort(key=lambda s: s["name"])
    text = json.dumps({
        "openusd_version": ".".join(str(v) for v in Usd.GetVersion()),
        "instances": INSTANCES,
        "schemas": schemas,
    }, indent=1, sort_keys=True) + "\n"
    snapshot = FIXTURE / "oracle.json"
    if args.check:
        if snapshot.read_text() != text:
            raise SystemExit(f"OpenUSD's registry differs from {snapshot}")
    else:
        snapshot.write_text(text)
        print(f"wrote {snapshot}: {len(schemas)} schemas")


if __name__ == "__main__":
    main()
