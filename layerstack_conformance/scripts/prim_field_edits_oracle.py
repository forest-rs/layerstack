# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's prim authoring calls author, call by call.

Usage: prim_field_edits_oracle.py [FIXTURES [OUT]]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

Each case opens `fixtures/prim_field_edits/scene.usda` afresh (it sublayers
`sub.usda`, and `/R` references `asset.usda`'s `/Asset`), points the stage
at one edit target, and makes a sequence of `UsdPrim`, `UsdReferences`,
`UsdPayloads`, `UsdInherits` and `UsdSpecializes` calls on one prim. For
each call it records whether OpenUSD accepted it (`false` when the call
returns false or raises a coding error). After the last call it records
the spec the target addresses (`MapToSpecPath` of the prim) in the
target's layer, as normalized fields; `null` when there is none.

`tests/conformance/prim_field_edits.rs` makes the same calls through layerstack's
`Transaction` and must agree with every record.

Writes `fixtures/prim_field_edits/oracle.json` by default.
"""
import json
import os
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "prim_field_edits"))

POSITIONS = {
    "front_of_prepend": Usd.ListPositionFrontOfPrependList,
    "back_of_prepend": Usd.ListPositionBackOfPrependList,
    "front_of_append": Usd.ListPositionFrontOfAppendList,
    "back_of_append": Usd.ListPositionBackOfAppendList,
}


def ref(step, payload=False):
    offset = Sdf.LayerOffset(step.get("offset", 0.0), step.get("scale", 1.0))
    asset = step.get("asset", "")
    if payload:
        return Sdf.Payload(asset, step["path"], offset)
    return Sdf.Reference(asset, step["path"], offset)


def call(stage, prim, step):
    kind = step["call"]
    position = POSITIONS[step.get("position", "front_of_prepend")]
    value = step.get("value")
    if kind == "add_reference":
        return prim.GetReferences().AddReference(ref(step), position)
    if kind == "remove_reference":
        return prim.GetReferences().RemoveReference(ref(step))
    if kind == "set_references":
        if value is None:
            return prim.GetReferences().ClearReferences()
        return prim.GetReferences().SetReferences([ref(item) for item in value])
    if kind == "add_payload":
        return prim.GetPayloads().AddPayload(ref(step, payload=True), position)
    if kind == "remove_payload":
        return prim.GetPayloads().RemovePayload(ref(step, payload=True))
    if kind == "set_payloads":
        if value is None:
            return prim.GetPayloads().ClearPayloads()
        return prim.GetPayloads().SetPayloads([ref(item, payload=True) for item in value])
    if kind == "add_inherit":
        return prim.GetInherits().AddInherit(Sdf.Path(step["path"]), position)
    if kind == "remove_inherit":
        return prim.GetInherits().RemoveInherit(Sdf.Path(step["path"]))
    if kind == "set_inherits":
        if value is None:
            return prim.GetInherits().ClearInherits()
        return prim.GetInherits().SetInherits([Sdf.Path(p) for p in value])
    if kind == "add_specialize":
        return prim.GetSpecializes().AddSpecialize(Sdf.Path(step["path"]), position)
    if kind == "remove_specialize":
        return prim.GetSpecializes().RemoveSpecialize(Sdf.Path(step["path"]))
    if kind == "set_specializes":
        if value is None:
            return prim.GetSpecializes().ClearSpecializes()
        return prim.GetSpecializes().SetSpecializes([Sdf.Path(p) for p in value])
    if kind == "set_active":
        return prim.ClearActive() if value is None else prim.SetActive(value)
    if kind == "set_instanceable":
        return prim.ClearInstanceable() if value is None else prim.SetInstanceable(value)
    if kind == "set_type_name":
        return prim.ClearTypeName() if value is None else prim.SetTypeName(value)
    if kind == "set_specifier":
        return prim.SetSpecifier(getattr(Sdf, "Specifier" + value.capitalize()))
    if kind == "set_prim_order":
        return prim.ClearChildrenReorder() if value is None else prim.SetChildrenReorder(value)
    if kind == "set_property_order":
        return prim.ClearPropertyOrder() if value is None else prim.SetPropertyOrder(value)
    if kind == "add_applied_schema":
        return prim.AddAppliedSchema(step["name"])
    if kind == "remove_applied_schema":
        return prim.RemoveAppliedSchema(step["name"])
    raise ValueError(kind)


def list_op(op, item):
    out = {
        "explicit": [item(x) for x in op.explicitItems] if op.isExplicit else None,
        "prepend": [item(x) for x in op.prependedItems],
        "append": [item(x) for x in op.appendedItems],
        "delete": [item(x) for x in op.deletedItems],
    }
    return out


def arc(item):
    offset = item.layerOffset
    return {
        "asset": item.assetPath,
        "path": str(item.primPath),
        "offset": offset.offset,
        "scale": offset.scale,
    }


FIELDS = (
    "specifier", "typeName", "active", "instanceable", "primOrder", "propertyOrder",
    "references", "payload", "inheritPaths", "specializes", "apiSchemas",
)


def spec_fields(spec):
    """The fields this oracle covers that `spec` authors."""
    if spec is None:
        return None
    out = {}
    keys = set(spec.ListInfoKeys())
    for key in FIELDS:
        if key not in keys:
            continue
        value = spec.GetInfo(key)
        if key == "specifier":
            out[key] = str(value).replace("Sdf.Specifier", "").lower()
        elif key in ("references", "payload"):
            out[key] = list_op(value, arc)
        elif key in ("inheritPaths", "specializes", "apiSchemas"):
            out[key] = list_op(value, str)
        elif key in ("primOrder", "propertyOrder"):
            out[key] = [str(x) for x in value]
        elif key == "typeName":
            out[key] = str(value)
        else:
            out[key] = bool(value)
    return out


def node_of(prim, arc_name):
    stack = [prim.GetPrimIndex().rootNode]
    while stack:
        node = stack.pop(0)
        if str(node.arcType).lower().endswith(arc_name):
            return node
        stack.extend(node.children)
    raise LookupError(f"{prim.GetPath()} has no {arc_name} node")


def edit_target(stage, target):
    kind = target["kind"]
    root = stage.GetRootLayer()
    if kind == "root":
        return Usd.EditTarget(root)
    if kind == "sublayer":
        return Usd.EditTarget(Sdf.Layer.Find(os.path.join(os.path.dirname(root.realPath), "sub.usda")))
    node = node_of(stage.GetPrimAtPath(target["prim"]), kind)
    return Usd.EditTarget(node.layerStack.layerTree.layer, node)


# (name, edit target, prim, calls)
CASES = [
    ("references_positions", {"kind": "root"}, "/A", [
        {"call": "add_reference", "path": "/B"},
        {"call": "add_reference", "path": "/C", "position": "back_of_prepend"},
        {"call": "add_reference", "path": "/D", "position": "front_of_append"},
        {"call": "add_reference", "path": "/E", "position": "front_of_append"},
        {"call": "add_reference", "path": "/B", "position": "back_of_append"},
    ]),
    ("references_move_within_list", {"kind": "root"}, "/A", [
        {"call": "add_reference", "path": "/B"},
        {"call": "add_reference", "path": "/C"},
        {"call": "add_reference", "path": "/B"},
        {"call": "add_reference", "path": "/D", "position": "back_of_append"},
        {"call": "add_reference", "path": "/E", "position": "back_of_append"},
        {"call": "add_reference", "path": "/E", "position": "front_of_append"},
    ]),
    ("references_explicit", {"kind": "root"}, "/A", [
        {"call": "set_references", "value": [{"path": "/B"}, {"path": "/C"}]},
        {"call": "add_reference", "path": "/D", "position": "front_of_prepend"},
        {"call": "add_reference", "path": "/E", "position": "back_of_append"},
        {"call": "add_reference", "path": "/E", "position": "front_of_prepend"},
        {"call": "remove_reference", "path": "/B"},
    ]),
    ("references_remove", {"kind": "root"}, "/A", [
        {"call": "add_reference", "path": "/B"},
        {"call": "add_reference", "path": "/C", "position": "back_of_append"},
        {"call": "remove_reference", "path": "/B"},
        {"call": "remove_reference", "path": "/C"},
        {"call": "remove_reference", "path": "/D"},
        {"call": "remove_reference", "path": "/D"},
        {"call": "add_reference", "path": "/B"},
    ]),
    ("references_clear", {"kind": "root"}, "/A", [
        {"call": "add_reference", "path": "/B"},
        {"call": "set_references", "value": None},
    ]),
    ("references_explicit_empty", {"kind": "root"}, "/A", [
        {"call": "set_references", "value": []},
    ]),
    ("references_offsets_and_assets", {"kind": "root"}, "/A", [
        {"call": "add_reference", "path": "/B"},
        {"call": "add_reference", "path": "/B", "offset": 5.0},
        {"call": "add_reference", "path": "/Other", "asset": "asset.usda", "position": "back_of_prepend"},
        {"call": "add_reference", "path": "", "asset": "asset.usda", "offset": 2.0, "scale": 0.5},
    ]),
    ("references_rejected_paths", {"kind": "root"}, "/A", [
        {"call": "add_reference", "path": "/"},
        {"call": "add_payload", "path": "/"},
    ]),
    ("payloads", {"kind": "root"}, "/A", [
        {"call": "add_payload", "path": "/B"},
        {"call": "add_payload", "path": "/C", "position": "back_of_append"},
        {"call": "remove_payload", "path": "/B"},
        {"call": "add_payload", "path": "/Other", "asset": "asset.usda"},
    ]),
    ("payloads_explicit", {"kind": "root"}, "/A", [
        {"call": "set_payloads", "value": [{"path": "/B"}]},
        {"call": "add_payload", "path": "/C", "position": "front_of_append"},
        {"call": "set_payloads", "value": None},
    ]),
    ("inherits", {"kind": "root"}, "/A", [
        {"call": "add_inherit", "path": "/Cls"},
        {"call": "add_inherit", "path": "/B", "position": "back_of_append"},
        {"call": "add_inherit", "path": "/C", "position": "front_of_append"},
        {"call": "remove_inherit", "path": "/Cls"},
        {"call": "add_inherit", "path": "/Cls", "position": "back_of_prepend"},
        {"call": "add_inherit", "path": "/"},
    ]),
    ("inherits_explicit", {"kind": "root"}, "/A", [
        {"call": "set_inherits", "value": ["/Cls", "/B"]},
        {"call": "add_inherit", "path": "/C", "position": "back_of_append"},
        {"call": "remove_inherit", "path": "/Cls"},
        {"call": "set_inherits", "value": None},
        {"call": "remove_inherit", "path": "/B"},
    ]),
    ("specializes", {"kind": "root"}, "/A", [
        {"call": "add_specialize", "path": "/B"},
        {"call": "add_specialize", "path": "/C", "position": "back_of_append"},
        {"call": "remove_specialize", "path": "/B"},
        {"call": "set_specializes", "value": ["/D"]},
        {"call": "add_specialize", "path": "/"},
    ]),
    ("scalar_fields", {"kind": "root"}, "/A", [
        {"call": "set_active", "value": False},
        {"call": "set_instanceable", "value": True},
        {"call": "set_type_name", "value": "Xform"},
        {"call": "set_specifier", "value": "class"},
        {"call": "set_prim_order", "value": ["y", "x"]},
        {"call": "set_property_order", "value": ["b", "a"]},
    ]),
    ("scalar_fields_cleared", {"kind": "root"}, "/A", [
        {"call": "set_active", "value": False},
        {"call": "set_instanceable", "value": True},
        {"call": "set_type_name", "value": "Xform"},
        {"call": "set_prim_order", "value": ["y", "x"]},
        {"call": "set_property_order", "value": ["b", "a"]},
        {"call": "set_active", "value": None},
        {"call": "set_instanceable", "value": None},
        {"call": "set_type_name", "value": None},
        {"call": "set_prim_order", "value": None},
        {"call": "set_property_order", "value": None},
        {"call": "set_specifier", "value": "over"},
        {"call": "set_specifier", "value": "def"},
    ]),
    ("empty_orders", {"kind": "root"}, "/A", [
        {"call": "set_prim_order", "value": []},
        {"call": "set_property_order", "value": []},
    ]),
    ("pseudo_root", {"kind": "root"}, "/", [
        {"call": "set_active", "value": False},
        {"call": "set_instanceable", "value": True},
        {"call": "set_type_name", "value": "Xform"},
    ]),
    ("applied_schemas", {"kind": "root"}, "/A", [
        {"call": "add_applied_schema", "name": "FooAPI"},
        {"call": "add_applied_schema", "name": "BarAPI"},
        {"call": "add_applied_schema", "name": "FooAPI"},
        {"call": "remove_applied_schema", "name": "FooAPI"},
        {"call": "remove_applied_schema", "name": "BazAPI"},
        {"call": "add_applied_schema", "name": "FooAPI"},
    ]),
    ("sublayer_creates_over", {"kind": "sublayer"}, "/A", [
        {"call": "set_active", "value": False},
        {"call": "add_inherit", "path": "/Cls"},
    ]),
    ("sublayer_clear_keeps_spec", {"kind": "sublayer"}, "/B", [
        {"call": "set_active", "value": None},
        {"call": "set_references", "value": None},
    ]),
    ("sublayer_clear_creates_nothing", {"kind": "sublayer"}, "/C", [
        {"call": "set_active", "value": None},
        {"call": "set_type_name", "value": None},
        {"call": "set_references", "value": None},
        {"call": "remove_reference", "path": "/B"},
    ]),
    ("variant_branch", {"kind": "variant", "prim": "/V"}, "/V", [
        {"call": "add_reference", "path": "/B"},
        {"call": "add_inherit", "path": "/Cls"},
        {"call": "set_active", "value": False},
        {"call": "set_instanceable", "value": True},
        {"call": "set_property_order", "value": ["b", "a"]},
    ]),
    ("reference_target_maps_paths", {"kind": "reference", "prim": "/R"}, "/R", [
        {"call": "add_inherit", "path": "/R/Child"},
        {"call": "add_specialize", "path": "/R/Child"},
        {"call": "add_reference", "path": "/R/Child", "position": "back_of_prepend"},
        {"call": "add_reference", "path": "/Other", "asset": "asset.usda", "position": "back_of_prepend"},
        {"call": "set_active", "value": False},
        {"call": "set_type_name", "value": "Scope"},
    ]),
    ("reference_target_rejects_unmapped", {"kind": "reference", "prim": "/R"}, "/R", [
        {"call": "add_inherit", "path": "/Cls"},
        {"call": "add_reference", "path": "/B"},
        {"call": "set_inherits", "value": ["/Cls"]},
        {"call": "remove_inherit", "path": "/Cls"},
    ]),
]


def main():
    fixtures = sys.argv[1] if len(sys.argv) > 1 else FIXTURES
    out_path = sys.argv[2] if len(sys.argv) > 2 else os.path.join(fixtures, "oracle.json")
    cases = []
    for name, target_desc, prim_path, steps in CASES:
        for layer in list(Sdf.Layer.GetLoadedLayers()):
            if not layer.anonymous:
                layer.Reload(force=True)
        stage = Usd.Stage.Open(os.path.join(fixtures, "scene.usda"))
        target = edit_target(stage, target_desc)
        stage.SetEditTarget(target)
        prim = stage.GetPrimAtPath(prim_path)
        accepted = []
        for step in steps:
            # The order setters return nothing; the others return false
            # when they reject the call.
            try:
                accepted.append(call(stage, prim, step) is not False)
            except Exception:
                accepted.append(False)
        spec_path = target.MapToSpecPath(Sdf.Path(prim_path))
        layer = target.GetLayer()
        spec = None if spec_path.isEmpty else layer.GetObjectAtPath(spec_path)
        if isinstance(spec, Sdf.VariantSpec):
            spec = spec.primSpec
        cases.append({
            "name": name,
            "target": target_desc,
            "prim": prim_path,
            "steps": steps,
            "accepted": accepted,
            "spec_path": None if spec_path.isEmpty else str(spec_path),
            "spec": spec_fields(spec),
        })
        for layer in list(Sdf.Layer.GetLoadedLayers()):
            if not layer.anonymous:
                layer.Reload(force=True)

    _, minor, patch = Usd.GetVersion()
    with open(out_path, "w") as f:
        json.dump({"openusd_version": f"{minor}.{patch}", "cases": cases}, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
