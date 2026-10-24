# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD adds and removes relationship targets and attribute
connections one at a time, call by call, and how a layer stack composes
them.

Usage: target_list_edits_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/target_list_edits` by
default, the layers below as authored and `oracle.json`, which
`tests/conformance/target_list_edits.rs` replays against layerstack.

`UsdRelationship::AddTarget` and `UsdAttribute::AddConnection` put the
item with `Usd_InsertListItem` (`pxr/usd/usd/valueUtils.h`): at the front
or back of the prepended or appended items as the `UsdListPosition` says,
or of the explicit items when the list op is explicit, moving an item the
chosen list already holds. `RemoveTarget` and `RemoveConnection` call
`SdfListEditorProxy::Remove` (`pxr/usd/sdf/listEditorProxy.h`): an explicit
list drops the item; otherwise it leaves the prepended, appended and added
items and joins the deleted ones. Each call first creates the property
spec at the edit target as `_CreateSpec` does (`relationship.cpp`,
`attribute.cpp`): from the schema definition, else stamped from the
strongest authored spec, else, for a relationship only, as a `custom`
relationship; an attribute nothing declares fails. No case connects an
attribute nothing declares: layerstack's one `add_target` for both kinds
makes a relationship of a property nothing declares.

The oracle records:

- `edits`: authoring calls on `edits.usda` (sublayering `edits_sub.usda`;
  `/Rock` has a `size` variant set and `/Vehicle` references `asset.usda`'s
  `/Model`), call by call: whether OpenUSD accepted each call (`false` when
  it returned false or raised a coding error) and the property spec the
  edit target addresses after it (its kind, `custom`, an attribute's type
  name and its targets or connections list op), or `null` when there is
  none. The calls are `UsdRelationship::AddTarget` / `RemoveTarget`,
  `UsdAttribute::AddConnection` / `RemoveConnection`, and the
  `CollectionAPI` and `LightAPI` relationships through their schema's
  `Create*Rel()`, which layerstack's generated handles make as
  `add_to_<rel>` / `remove_from_<rel>`;
- `scopes`: one sublayer per scope. `scopes.usda` defines the sphere light
  `/World/Key` and sublayers `scope_strong.usda`, `scope_mid.usda` and
  `scope_weak.usda`, strongest first. The two weaker layers append to the
  light-link collection's `includes` and `excludes` and to the light's
  `light:filters`, and the strongest deletes, each through its own edit
  target, call by call as `edits` records them; then the composed
  `GetTargets()` of each relationship.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd, UsdLux

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "target_list_edits"))

EDITS = '''#usda 1.0
(
    subLayers = [
        @edits_sub.usda@
    ]
)

def "Lamp"
{
    rel lights
    rel explicit = [
        </A>,
        </B>,
    ]
    delete rel deleted = </A>
    prepend rel deleted = </B>
    custom rel declared
    color3f inputs:color
    color3f inputs:tint.connect = </Source.outputs:a>
}

def "Rock" (
    prepend variantSets = "size"
    variants = {
        string size = "big"
    }
)
{
    variantSet "size" = {
        "big" {
            rel lights
        }
    }

    def "Pebble"
    {
        double radius = 1
    }
}

def "Vehicle" (
    prepend references = @asset.usda@</Model>
)
{
}

def SphereLight "Key"
{
}
'''

EDITS_SUB = '''#usda 1.0

over "Terrain"
{
}
'''

ASSET = '''#usda 1.0

def "Model"
{
    rel parts

    def "Part"
    {
    }
}
'''

SCOPES = '''#usda 1.0
(
    subLayers = [
        @scope_strong.usda@,
        @scope_mid.usda@,
        @scope_weak.usda@
    ]
)

def Xform "World"
{
    def SphereLight "Key"
    {
    }
}
'''

EMPTY = '''#usda 1.0
'''

LAYERS = {
    "edits.usda": EDITS,
    "edits_sub.usda": EDITS_SUB,
    "asset.usda": ASSET,
    "scopes.usda": SCOPES,
    "scope_strong.usda": EMPTY,
    "scope_mid.usda": EMPTY,
    "scope_weak.usda": EMPTY,
}

POSITIONS = {
    "front_of_prepend": Usd.ListPositionFrontOfPrependList,
    "back_of_prepend": Usd.ListPositionBackOfPrependList,
    "front_of_append": Usd.ListPositionFrontOfAppendList,
    "back_of_append": Usd.ListPositionBackOfAppendList,
}

# The schema relationships the generated handles edit: the step's `call`
# names the handle's method, and `rel` the C++ `Create*Rel` call.
SCHEMA_RELS = {
    "includes": ("collection", "CreateIncludesRel", "collection:lightLink:includes"),
    "excludes": ("collection", "CreateExcludesRel", "collection:lightLink:excludes"),
    "filters": ("light", "CreateFiltersRel", "light:filters"),
}


def add(prop, target, position="back_of_prepend"):
    return {"call": "add_target", "property": prop, "target": target, "position": position}


def remove(prop, target):
    return {"call": "remove_target", "property": prop, "target": target}


def add_connection(prop, source, position="back_of_prepend"):
    return {"call": "add_connection", "property": prop, "target": source,
            "position": position}


def remove_connection(prop, source):
    return {"call": "remove_connection", "property": prop, "target": source}


def add_to(rel, target, position="back_of_append"):
    return {"call": f"add_to_{rel}", "property": SCHEMA_RELS[rel][2], "target": target,
            "position": position}


def remove_from(rel, target):
    return {"call": f"remove_from_{rel}", "property": SCHEMA_RELS[rel][2], "target": target}


ROOT = {"kind": "root"}
SUB = {"kind": "sublayer"}

# (name, edit target, prim, calls)
EDIT_CASES = [
    ("add_positions", ROOT, "/Lamp", [
        add("lights", "/A", "back_of_prepend"),
        add("lights", "/B", "front_of_prepend"),
        add("lights", "/C", "back_of_append"),
        add("lights", "/A", "back_of_append"),
        add("lights", "/A", "back_of_append"),
        add("lights", "/B", "back_of_prepend"),
        add("lights", "/D", "front_of_append"),
        add("lights", "/A", "back_of_prepend"),
    ]),
    ("add_explicit", ROOT, "/Lamp", [
        add("explicit", "/C", "front_of_append"),
        add("explicit", "/A", "back_of_append"),
        add("explicit", "/B", "front_of_prepend"),
        add("explicit", "/D", "back_of_prepend"),
    ]),
    ("add_and_remove_deleted", ROOT, "/Lamp", [
        add("deleted", "/A", "back_of_prepend"),
        add("deleted", "/B", "back_of_append"),
        remove("deleted", "/B"),
        remove("deleted", "/A"),
        remove("deleted", "/A"),
        remove("deleted", "/C"),
    ]),
    ("remove_explicit", ROOT, "/Lamp", [
        remove("explicit", "/A"),
        remove("explicit", "/Z"),
        remove("explicit", "/B"),
        add("explicit", "/C", "back_of_append"),
    ]),
    ("property_targets", ROOT, "/Lamp", [
        add("lights", "/Rock/Pebble.radius", "back_of_append"),
        add("lights", "/Rock/Pebble", "back_of_append"),
        remove("lights", "/Rock/Pebble.radius"),
    ]),
    ("remove_creates_spec", SUB, "/Lamp", [
        remove("lights", "/A"),
        add("lights", "/B", "back_of_append"),
    ]),
    ("add_undeclared", ROOT, "/Lamp", [
        add("extra", "/A"),
    ]),
    ("add_undeclared_in_sublayer", SUB, "/Lamp", [
        remove("extra", "/A"),
    ]),
    ("add_declared_custom", SUB, "/Lamp", [
        add("declared", "/A", "back_of_append"),
    ]),
    ("connections", ROOT, "/Lamp", [
        add_connection("inputs:color", "/Source.outputs:a"),
        add_connection("inputs:color", "/Source.outputs:b", "back_of_append"),
        remove_connection("inputs:color", "/Source.outputs:a"),
        remove_connection("inputs:color", "/Other.outputs:c"),
    ]),
    ("connections_explicit", ROOT, "/Lamp", [
        add_connection("inputs:tint", "/Source.outputs:b", "front_of_append"),
        remove_connection("inputs:tint", "/Source.outputs:a"),
    ]),
    ("connection_creates_spec", SUB, "/Lamp", [
        add_connection("inputs:color", "/Source.outputs:a", "back_of_append"),
        remove_connection("inputs:tint", "/Source.outputs:a"),
    ]),
    ("through_variant", {"kind": "variant", "prim": "/Rock"}, "/Rock", [
        add("lights", "/Rock/Pebble", "back_of_append"),
        remove("lights", "/Lamp"),
    ]),
    ("through_reference", {"kind": "reference", "prim": "/Vehicle"}, "/Vehicle", [
        add("parts", "/Vehicle/Part", "back_of_append"),
        remove("parts", "/Vehicle"),
        add("parts", "/Lamp", "back_of_append"),
        remove("parts", "/Lamp"),
    ]),
    ("collection_includes", ROOT, "/Key", [
        add_to("includes", "/Rock", "back_of_append"),
        add_to("includes", "/Vehicle", "back_of_append"),
        add_to("includes", "/Rock", "back_of_append"),
        remove_from("includes", "/Lamp"),
        remove_from("includes", "/Vehicle"),
    ]),
    ("collection_excludes", SUB, "/Key", [
        add_to("excludes", "/Rock/Pebble", "front_of_prepend"),
        remove_from("excludes", "/Rock"),
    ]),
    ("light_filters", SUB, "/Key", [
        add_to("filters", "/Lamp", "back_of_append"),
        remove_from("filters", "/Lamp"),
        add_to("filters", "/Rock", "front_of_append"),
    ]),
]

KEY = "/World/Key"

# (layer, call): the weaker scopes append, the strongest deletes and puts
# one target in front.
SCOPE_STEPS = [
    ("scope_weak.usda", add_to("includes", "/World/Rocks/A")),
    ("scope_weak.usda", add_to("includes", "/World/Rocks/B")),
    ("scope_weak.usda", add_to("excludes", "/World/Rocks/A/Moss")),
    ("scope_weak.usda", add_to("filters", "/World/Filters/Haze")),
    ("scope_mid.usda", add_to("includes", "/World/Plants/Fern")),
    ("scope_mid.usda", add_to("includes", "/World/Rocks/A")),
    ("scope_mid.usda", add_to("filters", "/World/Filters/Gobo")),
    ("scope_strong.usda", remove_from("includes", "/World/Rocks/B")),
    ("scope_strong.usda", add_to("includes", "/World/Terrain", "front_of_prepend")),
    ("scope_strong.usda", remove_from("excludes", "/World/Rocks/A/Moss")),
    ("scope_strong.usda", remove_from("filters", "/World/Filters/Haze")),
]


def list_op(op):
    return {
        "explicit": [str(p) for p in op.explicitItems] if op.isExplicit else None,
        "prepend": [str(p) for p in op.prependedItems],
        "append": [str(p) for p in op.appendedItems],
        "delete": [str(p) for p in op.deletedItems],
        "add": [str(p) for p in op.addedItems],
        "reorder": [str(p) for p in op.orderedItems],
    }


def spec_record(spec):
    """A property spec: its kind, `custom`, an attribute's type name, and
    its targets or connections list op (`null` when it authors none)."""
    if spec is None:
        return None
    if isinstance(spec, Sdf.RelationshipSpec):
        field, kind, type_name = "targetPaths", "relationship", None
    else:
        field, kind, type_name = "connectionPaths", "attribute", str(spec.typeName)
    return {
        "kind": kind,
        "custom": spec.custom,
        "type": type_name,
        "targets": list_op(spec.GetInfo(field)) if spec.HasInfo(field) else None,
    }


def node_of(prim, arc_name):
    stack = [prim.GetPrimIndex().rootNode]
    while stack:
        node = stack.pop(0)
        if str(node.arcType).lower().endswith(arc_name):
            return node
        stack.extend(node.children)
    raise LookupError(f"{prim.GetPath()} has no {arc_name} node")


def edit_target(stage, target, out):
    kind = target["kind"]
    if kind == "root":
        return Usd.EditTarget(stage.GetRootLayer())
    if kind == "sublayer":
        return Usd.EditTarget(Sdf.Layer.Find(os.path.join(out, "edits_sub.usda")))
    node = node_of(stage.GetPrimAtPath(target["prim"]), kind)
    return Usd.EditTarget(node.layerStack.layerTree.layer, node)


def schema_rel(prim, call):
    rel = call.split("_", 2)[-1]
    holder, create, _ = SCHEMA_RELS[rel]
    if holder == "collection":
        schema = Usd.CollectionAPI(prim, "lightLink")
    else:
        schema = UsdLux.LightAPI(prim)
    return getattr(schema, create)()


def call(prim, step):
    kind = step["call"]
    target = Sdf.Path(step["target"])
    position = POSITIONS[step.get("position", "back_of_prepend")]
    if kind == "add_target":
        return prim.GetRelationship(step["property"]).AddTarget(target, position)
    if kind == "remove_target":
        return prim.GetRelationship(step["property"]).RemoveTarget(target)
    if kind == "add_connection":
        return prim.GetAttribute(step["property"]).AddConnection(target, position)
    if kind == "remove_connection":
        return prim.GetAttribute(step["property"]).RemoveConnection(target)
    if kind.startswith("add_to_"):
        return schema_rel(prim, kind).AddTarget(target, position)
    if kind.startswith("remove_from_"):
        return schema_rel(prim, kind).RemoveTarget(target)
    raise ValueError(kind)


def accepted(prim, step):
    try:
        return call(prim, step) is not False
    except Exception:
        return False


def reload_layers():
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)


def addressed(target, prim_path, prop):
    path = target.MapToSpecPath(Sdf.Path(prim_path)).AppendProperty(prop)
    return spec_record(target.GetLayer().GetPropertyAtPath(path))


def edits_record(out):
    cases = []
    for name, target_desc, prim_path, steps in EDIT_CASES:
        reload_layers()
        stage = Usd.Stage.Open(os.path.join(out, "edits.usda"))
        target = edit_target(stage, target_desc, out)
        stage.SetEditTarget(target)
        prim = stage.GetPrimAtPath(prim_path)
        results, after = [], []
        for step in steps:
            results.append(accepted(prim, step))
            after.append(addressed(target, prim_path, step["property"]))
        cases.append({
            "name": name,
            "target": target_desc,
            "prim": prim_path,
            "steps": steps,
            "accepted": results,
            "after": after,
        })
    reload_layers()
    return cases


def scopes_record(out):
    reload_layers()
    stage = Usd.Stage.Open(os.path.join(out, "scopes.usda"))
    prim = stage.GetPrimAtPath(KEY)
    steps = []
    for layer_name, step in SCOPE_STEPS:
        target = Usd.EditTarget(Sdf.Layer.Find(os.path.join(out, layer_name)))
        stage.SetEditTarget(target)
        ok = accepted(prim, step)
        steps.append(dict(step, layer=layer_name, accepted=ok,
                          after=addressed(target, KEY, step["property"])))
    composed = {
        rel: [str(p) for p in prim.GetRelationship(SCHEMA_RELS[rel][2]).GetTargets()]
        for rel in SCHEMA_RELS
    }
    reload_layers()
    return {"prim": KEY, "steps": steps, "composed": composed}


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "edits": edits_record(out),
        "scopes": scopes_record(out),
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
        for name in list(LAYERS) + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
