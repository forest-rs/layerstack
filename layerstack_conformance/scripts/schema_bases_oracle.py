# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's schema registry makes of typed schemas that declare
more than one base; run with usd-core 26.11.

Usage: schema_bases_oracle.py [--check]

`fixtures/schema_bases` is a hand-written codeless schema plugin: its
`plugInfo.json` gives some typed schemas two `bases` (usdGenSchema always
writes one), one a type that is no schema and one a type nothing declares,
and one an order its bases contradict; `generatedSchema.usda` holds each
class, some without their bases' properties. This registers the plugin and
records, for each type the plugin declares, whether the registry knows it as
a schema, the fixture's typed schemas it `IsA`, its ancestors that are typed
schemas in `TfType::GetAllAncestorTypes` (C3) order, or that the order is
inconsistent, its prim definition's properties and applied schemas, and
whether each API schema can be applied to it; and the errors registering
the plugin raises. It writes `fixtures/schema_bases/oracle.json`, which
`tests/conformance/schema_bases.rs` replays against layerstack's registry built from
the same files; with `--check` it writes nothing and fails on a difference.
"""
import argparse
import json
from pathlib import Path

from pxr import Plug, Tf, Usd

FIXTURE = Path(__file__).resolve().parent.parent / "fixtures" / "schema_bases"
API_SCHEMAS = ["OnlyAPI", "TagAPI"]


def errors_of(call):
    """The messages of the errors `call` raises, without their source
    locations, sorted."""
    try:
        call()
    except Tf.ErrorException as error:
        messages = []
        for line in str(error).strip().splitlines():
            message = line.split(" : ", 1)[-1].strip()
            if message.startswith("'") and message.endswith("'"):
                message = message[1:-1]
            messages.append(message)
        return sorted(messages)
    return []


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    registered = Plug.Registry().RegisterPlugins([str(FIXTURE)])
    if [p.name for p in registered] != ["LayerstackSchemaBases"]:
        raise SystemExit(f"registered {[p.name for p in registered]}")
    errors = errors_of(Usd.SchemaRegistry)
    registry = Usd.SchemaRegistry()
    plug_info = json.loads((FIXTURE / "plugInfo.json").read_text())
    declared = plug_info["Plugins"][0]["Info"]["Types"]
    typed = sorted(
        info["schemaIdentifier"] for info in declared.values()
        if info.get("schemaKind", "").endswith("Typed")) + ["Typed"]
    stage = Usd.Stage.CreateInMemory()
    types = {}
    for cpp_name, info in sorted(declared.items()):
        identifier = info.get("schemaIdentifier")
        if not identifier or not info["schemaKind"].endswith("Typed"):
            continue
        schema_info = Usd.SchemaRegistry.FindSchemaInfo(identifier)
        entry = {"schema": schema_info is not None}
        prim = stage.DefinePrim(f"/{identifier}", identifier)
        entry["is_a"] = [name for name in typed if prim.IsA(name)]
        if schema_info:
            ancestors = []
            entry["ancestor_errors"] = errors_of(
                lambda: ancestors.extend(schema_info.type.GetAllAncestorTypes()))
            if not entry["ancestor_errors"]:
                entry["ancestors"] = [
                    str(found.identifier) for found in map(
                        Usd.SchemaRegistry.FindSchemaInfo, ancestors)
                    if found and Usd.SchemaRegistry.IsTyped(found.type)]
            definition = (registry.FindConcretePrimDefinition(identifier)
                          or registry.FindAbstractPrimDefinition(identifier))
            entry["properties"] = sorted(str(n) for n in definition.GetPropertyNames())
            entry["applied_schemas"] = [str(s) for s in definition.GetAppliedAPISchemas()]
        entry["can_apply"] = {api: bool(prim.CanApplyAPI(api)) for api in API_SCHEMAS}
        types[identifier] = entry
    text = json.dumps({
        "openusd_version": ".".join(str(v) for v in Usd.GetVersion()),
        "registry_errors": errors,
        "types": types,
    }, indent=1, sort_keys=True) + "\n"
    snapshot = FIXTURE / "oracle.json"
    if args.check:
        if snapshot.read_text() != text:
            raise SystemExit(f"OpenUSD's registry differs from {snapshot}")
    else:
        snapshot.write_text(text)
        print(f"wrote {snapshot}: {len(types)} types")


if __name__ == "__main__":
    main()
