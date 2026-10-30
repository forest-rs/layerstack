# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the `UsdNotice::ObjectsChanged` notices OpenUSD sends for
property spec additions, removals and renames, call by call.

Usage: property_resync_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Opens `layerstack_conformance/fixtures/property_resync/scene.usda` (a
root layer over `weak.usda`, with `/Lamp` and the instanceable `/Robot`
referencing `asset.usda`), takes the fixed sequence of edits in `STEPS`,
and records, after each, every notice it sent: `GetResyncedPaths`,
`GetChangedInfoOnlyPaths` and `GetChangedFields` of each of them, and the
composed property names of the prims the sequence edits.
`tests/conformance/property_resync.rs` replays the edits against
layerstack.

What it shows, with the 26.11 code that does it: a property spec added
or removed, with or without fields beyond the required ones, resyncs the
property's path, not its prim's. `UsdStage::_HandleLayersDidChange`
(`pxr/usd/usd/stage.cpp`) adds the path of every `SdfChangeList` entry
at a property path with `didAddProperty`,
`didAddPropertyWithOnlyRequiredFields`, `didRemoveProperty` or
`didRemovePropertyWithOnlyRequiredFields` to its resyncs, and
`PcpChanges::DidChange` (`pxr/usd/pcp/changes.cpp`) records it as a
change of that property's spec stack
(`Pcp_EntryRequiresPropertySpecsChange`). A prim spec created in the same
change block resyncs the prim instead, which covers its properties. A
rename's entry has `didRename` alone, so `RenameProperty` resyncs
neither path: the new one is info-only, with no fields, and the old one
is not reported.

`--check` regenerates the oracle and fails unless it matches the
checked-in `oracle.json`.
"""
import json
import sys
from pathlib import Path

from pxr import Sdf, Tf, Usd, UsdGeom

FIXTURES = Path(__file__).resolve().parents[1] / "fixtures" / "property_resync"
stage = Usd.Stage.Open(str(FIXTURES / "scene.usda"))
root = stage.GetRootLayer()
weak = Sdf.Layer.FindOrOpen(str(FIXTURES / "weak.usda"))
asset = Sdf.Layer.FindOrOpen(str(FIXTURES / "asset.usda"))

notices = []


def notice(n, sender):
    resynced = [str(p) for p in n.GetResyncedPaths()]
    info = [str(p) for p in n.GetChangedInfoOnlyPaths()]
    notices.append({
        "resynced": resynced,
        "info": info,
        "fields": {p: sorted(n.GetChangedFields(p)) for p in resynced + info},
    })


registration = Tf.Notice.Register(Usd.Notice.ObjectsChanged, notice, stage)
WATCHED = ["/Rock", "/Lamp", "/Robot", "/Pebble"]


def rock():
    return stage.GetPrimAtPath("/Rock")


def set_visibility():
    UsdGeom.Imageable(rock()).GetVisibilityAttr().Set(UsdGeom.Tokens.invisible)


def change_visibility():
    UsdGeom.Imageable(rock()).GetVisibilityAttr().Set(UsdGeom.Tokens.inherited)


def remove_visibility():
    rock().RemoveProperty("visibility")


def create_required_only():
    rock().CreateAttribute("roughness", Sdf.ValueTypeNames.Float)


def set_required_default():
    rock().GetAttribute("roughness").Set(0.5)


def create_relationship():
    rock().CreateRelationship("proxyPrim")


def add_relationship_target():
    rock().GetRelationship("proxyPrim").AddTarget("/Looks")


def remove_relationship():
    rock().RemoveProperty("proxyPrim")


def set_stronger():
    rock().GetAttribute("size").Set(2.0)


def remove_stronger():
    rock().RemoveProperty("size")


def remove_last():
    with Usd.EditContext(stage, weak):
        rock().RemoveProperty("size")


def create_in_sublayer():
    with Usd.EditContext(stage, weak):
        rock().CreateAttribute("weight", Sdf.ValueTypeNames.Double).Set(3.0)


def create_with_new_over():
    with Usd.EditContext(stage, weak):
        stage.GetPrimAtPath("/Lamp").CreateAttribute(
            "weight", Sdf.ValueTypeNames.Double).Set(1.0)


def create_in_reference():
    prim = asset.GetPrimAtPath("/Asset")
    attr = Sdf.AttributeSpec(prim, "intensity", Sdf.ValueTypeNames.Float)
    attr.default = 2.0


def change_in_reference():
    asset.GetAttributeAtPath("/Asset.intensity").default = 4.0


def create_in_prototype_source():
    prim = asset.GetPrimAtPath("/Asset/Ball")
    attr = Sdf.AttributeSpec(prim, "radius", Sdf.ValueTypeNames.Double)
    attr.default = 2.0


def remove_in_reference():
    prim = asset.GetPrimAtPath("/Asset")
    prim.RemoveProperty(prim.properties["intensity"])


def add_rotate_op():
    UsdGeom.Xformable(rock()).AddRotateZOp().Set(45.0)


def change_translate():
    rock().GetAttribute("xformOp:translate").Set((4.0, 5.0, 6.0))


def rename_property():
    editor = Usd.NamespaceEditor(stage)
    assert editor.RenameProperty(rock().GetAttribute("roughness"), "gloss")
    assert editor.ApplyEdits()


def create_prim_and_property():
    with Sdf.ChangeBlock():
        prim = Sdf.CreatePrimInLayer(root, "/Pebble")
        prim.specifier = Sdf.SpecifierDef
        attr = Sdf.AttributeSpec(prim, "size", Sdf.ValueTypeNames.Double)
        attr.default = 1.0


def create_with_default_in_block():
    with Sdf.ChangeBlock():
        attr = Sdf.AttributeSpec(root.GetPrimAtPath("/Rock"), "lift", Sdf.ValueTypeNames.Double)
        attr.default = 0.5


def set_metadata():
    rock().GetAttribute("gloss").SetMetadata("documentation", "polish")


STEPS = [
    ("set_visibility", "`UsdAttribute::Set` creates `/Rock.visibility`, which no layer has, with a default", set_visibility),
    ("change_visibility", "`UsdAttribute::Set` on the spec it created", change_visibility),
    ("remove_visibility", "`UsdPrim::RemoveProperty` removes the only spec", remove_visibility),
    ("create_required_only", "`UsdPrim::CreateAttribute` authors only the required fields", create_required_only),
    ("set_required_default", "`UsdAttribute::Set` adds a default to it", set_required_default),
    ("create_relationship", "`UsdPrim::CreateRelationship`", create_relationship),
    ("add_relationship_target", "`UsdRelationship::AddTarget`", add_relationship_target),
    ("remove_relationship", "`UsdPrim::RemoveProperty` on the relationship", remove_relationship),
    ("set_stronger", "`UsdAttribute::Set` adds a root spec over the weaker layer's `size`", set_stronger),
    ("remove_stronger", "`UsdPrim::RemoveProperty` removes the root spec; the weaker one stays", remove_stronger),
    ("remove_last", "`UsdPrim::RemoveProperty` through the weaker layer removes the last spec", remove_last),
    ("create_in_sublayer", "`CreateAttribute` and `Set` through the weaker layer", create_in_sublayer),
    ("create_with_new_over", "`CreateAttribute` and `Set` through the weaker layer, which has no `/Lamp` spec", create_with_new_over),
    ("create_in_reference", "`SdfAttributeSpec` with a default in the referenced layer", create_in_reference),
    ("change_in_reference", "a new default on that spec", change_in_reference),
    ("create_in_prototype_source", "`SdfAttributeSpec` under the prim instances share", create_in_prototype_source),
    ("remove_in_reference", "`SdfPrimSpec::RemoveProperty` in the referenced layer", remove_in_reference),
    ("add_rotate_op", "`UsdGeomXformable::AddRotateZOp` and `Set`", add_rotate_op),
    ("change_translate", "`UsdAttribute::Set` on the existing translate", change_translate),
    ("rename_property", "`UsdNamespaceEditor::RenameProperty`", rename_property),
    ("create_prim_and_property", "one `SdfChangeBlock` creates `/Pebble` and its property", create_prim_and_property),
    ("create_with_default_in_block", "one `SdfChangeBlock` creates `/Rock.lift` and sets its default", create_with_default_in_block),
    ("set_metadata", "`UsdObject::SetMetadata` on a property", set_metadata),
]

records = []
for name, call, step in STEPS:
    step()
    properties = {}
    for path in WATCHED:
        prim = stage.GetPrimAtPath(path)
        if prim:
            properties[path] = [str(p) for p in prim.GetPropertyNames()]
    records.append({"name": name, "call": call, "notices": list(notices),
                    "properties": properties})
    notices.clear()

result = {"openusd_version": ".".join(map(str, Usd.GetVersion())), "records": records}
dest = FIXTURES / "oracle.json"
text = json.dumps(result, indent=2) + "\n"
if "--check" in sys.argv:
    assert dest.read_text() == text, "property resync oracle differs"
    print(f"{len(records)} property resync records agree with the committed oracle")
else:
    dest.write_text(text)
    print(f"wrote {len(records)} records")
