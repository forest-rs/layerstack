# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the `UsdNotice::ObjectsChanged` notices OpenUSD sends for prim
spec additions, removals and child reorders, and for the arcs, metadata
and layers above them, call by call.

Usage: prim_resync_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Opens `layerstack_conformance/fixtures/prim_resync/scene.usda` (a root
layer over `weak.usda`, with `/World` holding `/World/Rock` and
`/World/Lamp`, `/Sky`, the instanceable `/Robot` and `/Drone` and `/Thing`
referencing `asset.usda` (an `over` of `/Asset` over `asset_base.usda`),
`/Switch` with a variant set, `/Rover`, and `/Echo` inheriting
`/World/Lamp`; `weak.usda` defines `/Moon`), with an anonymous session
layer, takes the fixed sequence of edits in `STEPS`, and records, after
each, every notice it sent: `GetResyncedPaths`,
`GetChangedInfoOnlyPaths` and `GetChangedFields` of each of them, and the
child names of the prims the sequence edits, which must be those a stage
opened afresh on the same layers composes.
`tests/conformance/prim_resync.rs` replays the edits against layerstack.

What it shows, with the 26.11 code that does it: a prim spec added or
removed resyncs the prim's path and reports nothing of its parent.
The parent's spec gains or loses a `primChildren` entry, but
`SdfLayer::_PrimPushChild` (`pxr/usd/sdf/layer.cpp`) records no field
change for it ("child fields are essentially an implementation detail"),
and `Sdf_ChangeManager::DidChangeField` (`pxr/usd/sdf/changeManager.cpp`)
turns a `primChildren` change into a reorder only when the names are the
same (`_IsOrderChangeOnly`). Reordering children (`primOrder`) sets
`didReorderChildren` on the parent's entry, and
`UsdStage::_HandleLayersDidChange` (`pxr/usd/usd/stage.cpp`) recomposes,
so resyncs, the parent. A prim renamed or reparented with
`UsdNamespaceEditor` resyncs its old and new paths, and neither parent.
An `over` added in another layer for a prim
already composed (an inert spec, `didAddInertPrim`) changes that prim's
spec stack alone: it is info-only, with no fields, whether or not a child
spec comes with it. A spec authored beneath an instance root composes nothing:
the instance's descendants come from its prototype (`UsdStage::DefinePrim`
refuses the instance proxy path, so the spec is authored in the layer).
Its notice varies run to run, and is recorded as none (see `stable`).
One added in the asset the instances
reference resyncs the prototype's prim.

An inert `over` added or removed for a prim that composes either way, in
the root layer, the weaker layer, the session layer or the referenced
asset, is a spec stack change (`PcpChanges::DidChangeSpecs`) of that prim
and of each prim composing from it, which `UsdStage` reports info-only;
with an attribute spec in it the attribute is resynced, and with
`documentation` that field is reported. `kind` and `specifier`, like
`active` and `typeName`, are fields `_HandleLayersDidChange` recomposes
for, so they resync the prim; `documentation` does not. A field of the
root layer is an info change of `/`; a sublayer inserted, removed, muted
or unmuted resyncs the prims and properties its specs add or take away.
An `over` added to a weaker layer can change its parent's composed child
order, which the edited stage does not recompose (`STALE`).

`--check` regenerates the oracle and fails unless it matches the
checked-in `oracle.json`.
"""
import json
import sys
from pathlib import Path

from pxr import Sdf, Tf, Usd

FIXTURES = Path(__file__).resolve().parents[1] / "fixtures" / "prim_resync"
stage = Usd.Stage.Open(str(FIXTURES / "scene.usda"))
root = stage.GetRootLayer()
session = stage.GetSessionLayer()
weak = Sdf.Layer.FindOrOpen(str(FIXTURES / "weak.usda"))
asset = Sdf.Layer.FindOrOpen(str(FIXTURES / "asset.usda"))
# Sublayers the sequence inserts: one empty, one defining a root prim.
empty = Sdf.Layer.CreateAnonymous("empty.usda")
comet = Sdf.Layer.CreateAnonymous("comet.usda")
Sdf.CreatePrimInLayer(comet, "/Comet").specifier = Sdf.SpecifierDef

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
WATCHED = ["/", "/World", "/Robot", "/Thing", "/Switch", "/Rover"]


def define_child():
    stage.DefinePrim("/World/Pebble", "Sphere")


def define_with_ancestors():
    stage.DefinePrim("/World/Moss/Patch", "Mesh")


def override_prim():
    stage.OverridePrim("/World/Ghost")


def create_class():
    stage.CreateClassPrim("/_Base")


def define_root():
    stage.DefinePrim("/Ground", "Xform")


def remove_prim():
    stage.RemovePrim("/World/Pebble")


def remove_with_children():
    stage.RemovePrim("/World/Moss")


def remove_root():
    stage.RemovePrim("/Ground")


def reorder_children():
    stage.GetPrimAtPath("/World").SetChildrenReorder(["Lamp", "Rock"])


def clear_reorder():
    stage.GetPrimAtPath("/World").ClearChildrenReorder()


def rename_prim():
    editor = Usd.NamespaceEditor(stage)
    assert editor.RenamePrim(stage.GetPrimAtPath("/World/Ghost"), "Spirit")
    assert editor.ApplyEdits()


def reparent_prim():
    editor = Usd.NamespaceEditor(stage)
    assert editor.ReparentPrim(stage.GetPrimAtPath("/World/Spirit"),
                               stage.GetPseudoRoot())
    assert editor.ApplyEdits()


def create_property():
    stage.GetPrimAtPath("/World/Rock").CreateAttribute(
        "roughness", Sdf.ValueTypeNames.Float)


def define_in_sublayer():
    with Usd.EditContext(stage, weak):
        stage.DefinePrim("/World/Shrub", "Xform")


def define_in_sublayer_with_new_over():
    with Usd.EditContext(stage, weak):
        stage.DefinePrim("/Sky/Cloud", "Xform")


def override_in_sublayer():
    # `UsdStage::OverridePrim` authors nothing for a prim that exists.
    Sdf.CreatePrimInLayer(weak, "/World/Lamp")


def define_under_instance():
    # `UsdStage::DefinePrim` refuses: `/Robot/Extra` would be an instance
    # proxy. The spec is authored in the layer instead.
    try:
        stage.DefinePrim("/Robot/Extra", "Sphere")
        raise AssertionError("authoring an instance proxy succeeded")
    except Tf.ErrorException:
        pass
    with Sdf.ChangeBlock():
        prim = Sdf.CreatePrimInLayer(root, "/Robot/Extra")
        prim.specifier = Sdf.SpecifierDef
        prim.typeName = "Sphere"


def define_in_prototype_source():
    with Sdf.ChangeBlock():
        prim = Sdf.CreatePrimInLayer(asset, "/Asset/Antenna")
        prim.specifier = Sdf.SpecifierDef
        prim.typeName = "Cylinder"


def remove_from_prototype_source():
    prim = asset.GetPrimAtPath("/Asset")
    del prim.nameChildren["Antenna"]


def override_in_root():
    # `/Moon` is defined in the weaker layer alone.
    Sdf.CreatePrimInLayer(root, "/Moon")


def remove_override_in_root():
    del root.pseudoRoot.nameChildren["Moon"]


def remove_override_in_sublayer():
    del weak.GetPrimAtPath("/World").nameChildren["Lamp"]


def override_reference_child():
    # `/Thing/Ball` composes from the asset `/Thing` references.
    Sdf.CreatePrimInLayer(root, "/Thing/Ball")


def override_in_asset():
    # The asset's root layer has an `over` for `/Asset`; `Ball` is defined
    # in its sublayer. `/Thing`, `/Robot` and `/Drone` reference it.
    Sdf.CreatePrimInLayer(asset, "/Asset/Ball")


def remove_override_in_asset():
    del asset.GetPrimAtPath("/Asset").nameChildren["Ball"]


def override_with_attribute():
    with Sdf.ChangeBlock():
        prim = Sdf.CreatePrimInLayer(root, "/Moon")
        Sdf.AttributeSpec(prim, "weight", Sdf.ValueTypeNames.Float)


def override_with_metadata():
    with Sdf.ChangeBlock():
        prim = Sdf.CreatePrimInLayer(session, "/Sky")
        prim.documentation = "clear"


def override_in_session():
    Sdf.CreatePrimInLayer(session, "/World/Lamp")


def override_instance_root():
    Sdf.CreatePrimInLayer(weak, "/Robot")


def remove_override_instance_root():
    del weak.pseudoRoot.nameChildren["Robot"]


def select_variant():
    stage.GetPrimAtPath("/Switch").GetVariantSet("look").SetVariantSelection("night")


def clear_variant():
    stage.GetPrimAtPath("/Switch").GetVariantSet("look").ClearVariantSelection()


def add_reference():
    stage.GetPrimAtPath("/Rover").GetReferences().AddReference("./asset.usda", "/Asset")


def clear_references():
    stage.GetPrimAtPath("/Rover").GetReferences().ClearReferences()


def set_kind():
    stage.GetPrimAtPath("/World/Rock").SetMetadata("kind", "component")


def set_documentation():
    stage.GetPrimAtPath("/World/Rock").SetMetadata("documentation", "granite")


def deactivate():
    stage.GetPrimAtPath("/World/Lamp").SetActive(False)


def activate():
    stage.GetPrimAtPath("/World/Lamp").SetActive(True)


def set_type_name():
    stage.GetPrimAtPath("/World/Lamp").SetTypeName("Scope")


def set_layer_documentation():
    root.documentation = "a scene"


def insert_empty_sublayer():
    root.subLayerPaths.append(empty.identifier)


def remove_empty_sublayer():
    root.subLayerPaths.remove(empty.identifier)


def insert_prim_sublayer():
    root.subLayerPaths.append(comet.identifier)


def remove_prim_sublayer():
    root.subLayerPaths.remove(comet.identifier)


def set_specifier():
    stage.GetPrimAtPath("/Rover").SetSpecifier(Sdf.SpecifierClass)


def override_reorders_children():
    # The weaker layer lists `World`, `Moon` and `Sky`; the root layer
    # lists `Thing` after `Robot` and `Drone`. Child names compose weakest
    # layer first, so an `over` for `Thing` in the weaker layer moves it
    # before them; the edited stage keeps the old order (`STALE`).
    Sdf.CreatePrimInLayer(weak, "/Thing")


def rename_target():
    # `/World/Lamp.aim` targets `/World/Robot`: the editor repairs it.
    editor = Usd.NamespaceEditor(stage)
    assert editor.RenamePrim(stage.GetPrimAtPath("/World/Robot"), "Rover")
    assert editor.ApplyEdits()
    assert root.GetRelationshipAtPath("/World/Lamp.aim").targetPathList.explicitItems == ["/World/Rover"]


def mute_sublayer():
    stage.MuteLayer(weak.identifier)


def unmute_sublayer():
    stage.UnmuteLayer(weak.identifier)


STEPS = [
    ("define_child", "`UsdStage::DefinePrim` of a child of `/World`", define_child),
    ("define_with_ancestors", "`UsdStage::DefinePrim` of `/World/Moss/Patch`, creating `/World/Moss` too", define_with_ancestors),
    ("override_prim", "`UsdStage::OverridePrim` of a new child", override_prim),
    ("create_class", "`UsdStage::CreateClassPrim` at the root", create_class),
    ("define_root", "`UsdStage::DefinePrim` of a root prim", define_root),
    ("remove_prim", "`UsdStage::RemovePrim` of a leaf", remove_prim),
    ("remove_with_children", "`UsdStage::RemovePrim` of a prim with a child", remove_with_children),
    ("remove_root", "`UsdStage::RemovePrim` of a root prim", remove_root),
    ("reorder_children", "`UsdPrim::SetChildrenReorder` authors `primOrder`", reorder_children),
    ("clear_reorder", "`UsdPrim::ClearChildrenReorder`", clear_reorder),
    ("rename_prim", "`UsdNamespaceEditor::RenamePrim` of a child of `/World`", rename_prim),
    ("reparent_prim", "`UsdNamespaceEditor::ReparentPrim` of it to the pseudo-root", reparent_prim),
    ("create_property", "`UsdPrim::CreateAttribute`", create_property),
    ("define_in_sublayer", "`UsdStage::DefinePrim` through the weaker layer", define_in_sublayer),
    ("define_in_sublayer_with_new_over", "`UsdStage::DefinePrim` of `/Sky/Cloud` through the weaker layer, which has no `/Sky` spec: an `over` is added for it", define_in_sublayer_with_new_over),
    ("override_in_sublayer", "`SdfCreatePrimInLayer` adds an `over` for `/World/Lamp` to the weaker layer, which has no spec for it", override_in_sublayer),
    ("define_under_instance", "a `def` prim spec beneath the instance root `/Robot` (`UsdStage::DefinePrim` refuses an instance proxy path)", define_under_instance),
    ("define_in_prototype_source", "a `def` prim spec added in the asset the instances reference", define_in_prototype_source),
    ("remove_from_prototype_source", "that spec removed", remove_from_prototype_source),
    ("override_in_root", "`SdfCreatePrimInLayer` adds an `over` for `/Moon`, defined in the weaker layer alone, to the root layer", override_in_root),
    ("remove_override_in_root", "that `over` removed", remove_override_in_root),
    ("remove_override_in_sublayer", "the weaker layer's `over` for `/World/Lamp` removed", remove_override_in_sublayer),
    ("override_reference_child", "an `over` for `/Thing/Ball`, which `/Thing` references, in the root layer", override_reference_child),
    ("override_in_asset", "an `over` for `/Asset/Ball` in the referenced asset's root layer; its sublayer defines `Ball`", override_in_asset),
    ("remove_override_in_asset", "that `over` removed", remove_override_in_asset),
    ("override_with_attribute", "one `SdfChangeBlock` adds an `over` for `/Moon` to the root layer and an attribute spec to it", override_with_attribute),
    ("override_with_metadata", "one `SdfChangeBlock` adds an `over` for `/Sky` to the session layer and sets its `documentation`", override_with_metadata),
    ("override_in_session", "an `over` for `/World/Lamp` in the session layer, which has no `/World` spec", override_in_session),
    ("override_instance_root", "an `over` for the instance root `/Robot` in the weaker layer", override_instance_root),
    ("remove_override_instance_root", "that `over` removed", remove_override_instance_root),
    ("select_variant", "`UsdVariantSet::SetVariantSelection`", select_variant),
    ("clear_variant", "`UsdVariantSet::ClearVariantSelection`", clear_variant),
    ("add_reference", "`UsdReferences::AddReference`", add_reference),
    ("clear_references", "`UsdReferences::ClearReferences`", clear_references),
    ("set_kind", "`UsdObject::SetMetadata` of `kind`", set_kind),
    ("set_documentation", "`UsdObject::SetMetadata` of `documentation`", set_documentation),
    ("deactivate", "`UsdPrim::SetActive(false)`", deactivate),
    ("activate", "`UsdPrim::SetActive(true)`", activate),
    ("set_type_name", "`UsdPrim::SetTypeName`", set_type_name),
    ("set_specifier", "`UsdPrim::SetSpecifier` makes `/Rover` a `class`", set_specifier),
    ("set_layer_documentation", "`SdfLayer::SetDocumentation` on the root layer", set_layer_documentation),
    ("insert_empty_sublayer", "an empty sublayer appended to the root layer's", insert_empty_sublayer),
    ("remove_empty_sublayer", "that sublayer removed", remove_empty_sublayer),
    ("insert_prim_sublayer", "a sublayer defining `/Comet` appended", insert_prim_sublayer),
    ("remove_prim_sublayer", "that sublayer removed", remove_prim_sublayer),
    ("mute_sublayer", "`UsdStage::MuteLayer` of the weaker layer", mute_sublayer),
    ("unmute_sublayer", "`UsdStage::UnmuteLayer` of it", unmute_sublayer),
    ("rename_target", "`UsdNamespaceEditor::RenamePrim` of `/World/Robot`, which `/World/Lamp.aim` targets, to `Rover`", rename_target),
    ("override_reorders_children", "an `over` for `/Thing` in the weaker layer, which changes the composed order of the root prims", override_reorders_children),
]

# Steps after which the edited stage composes children a fresh stage on
# the same layers does not: an inert `over` in a weaker layer
# reorders its parent's children, and OpenUSD neither recomposes nor
# reports the parent. Their records hold the fresh stage's children.
STALE = {"override_reorders_children"}

def stable(name, sent):
    """The notices of step `name`, without one whose sending varies run
    to run: a spec beneath an instance root resyncs the prototype's path
    when the prototype was built from that instance, and sends nothing
    when from another, and which it is varies (C++ OpenUSD nondeterminism;
    the instance a prototype is built from is not ordered). Either way no
    prim is composed there, which the record's child names show."""
    if name != "define_under_instance":
        return list(sent)
    for n in sent:
        assert n["resynced"] == ["/__Prototype_1/Extra"] and not n["info"], n
    assert not stage.GetPrimAtPath("/__Prototype_1/Extra")
    return []


def children_of(composed):
    """The child names of the `WATCHED` prims of `composed`, and of the
    prototype of `/Robot`."""
    children = {}
    for path in WATCHED:
        prim = composed.GetPrimAtPath(path)
        if prim:
            children[path] = [c.GetName() for c in prim.GetAllChildren()]
    robot = composed.GetPrimAtPath("/Robot")
    if robot and robot.GetPrototype():
        children["prototype"] = [c.GetName() for c in robot.GetPrototype().GetAllChildren()]
    return children


records = []
for name, call, step in STEPS:
    step()
    children = children_of(stage)
    # Every record is checked against a stage composed afresh on the same
    # layers, so that it records a composition OpenUSD itself agrees with.
    fresh = Usd.Stage.Open(root, session)
    fresh.MuteAndUnmuteLayers(stage.GetMutedLayers(), [])
    fresh_children = children_of(fresh)
    if name in STALE:
        assert children != fresh_children, f"{name}: the stage composes the fresh order"
        children = fresh_children
    assert children == fresh_children, f"{name}: {children} != {fresh_children}"
    records.append({"name": name, "call": call, "notices": stable(name, notices),
                    "children": children})
    notices.clear()

result = {"openusd_version": ".".join(map(str, Usd.GetVersion())), "records": records}
dest = FIXTURES / "oracle.json"
text = json.dumps(result, indent=2) + "\n"
if "--check" in sys.argv:
    assert dest.read_text() == text, "prim resync oracle differs"
    print(f"{len(records)} prim resync records agree with the committed oracle")
else:
    dest.write_text(text)
    print(f"wrote {len(records)} records")
