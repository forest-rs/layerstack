# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records the `UsdNotice::ObjectsChanged` notices OpenUSD sends for prim
spec additions, removals and child reorders, call by call.

Usage: prim_resync_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Opens `layerstack_conformance/fixtures/prim_resync/scene.usda` (a root
layer over `weak.usda`, with `/World` holding `/World/Rock` and
`/World/Lamp`, `/Sky`, and the instanceable `/Robot` and `/Drone` referencing
`asset.usda`), takes the fixed sequence of edits in `STEPS`, and records,
after each, every notice it sent: `GetResyncedPaths`,
`GetChangedInfoOnlyPaths` and `GetChangedFields` of each of them, and the
child names of the prims the sequence edits.
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
WATCHED = ["/", "/World", "/Robot"]


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
]

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


records = []
for name, call, step in STEPS:
    step()
    children = {}
    for path in WATCHED:
        prim = stage.GetPrimAtPath(path)
        if prim:
            children[path] = [c.GetName() for c in prim.GetAllChildren()]
    robot = stage.GetPrimAtPath("/Robot")
    if robot and robot.GetPrototype():
        children["prototype"] = [c.GetName() for c in robot.GetPrototype().GetAllChildren()]
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
