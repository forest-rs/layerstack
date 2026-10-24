# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records which authoring calls OpenUSD refuses on instance proxies.

Usage: proxy_edits_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/proxy_edits` by default, the
layers below as OpenUSD exports them and `oracle.json`, which
`tests/proxy_edits.rs` replays against layerstack's `SchemaEdit` routes.

Every authoring call of `UsdStage` creates or clears a prim or property spec
through `_CreatePrimSpecForEditing`, `_CreatePropertySpecForEditing` or
`_ClearMetadata`, which first call `UsdStage::_ValidateEditPrim`
(`pxr/usd/usd/stage.cpp`); `DefinePrim` calls `_ValidateEditPrimAtPath`.
Both raise a coding error and author nothing for a prim in a prototype or
an instance proxy, when the edit target is a layer of the local layer stack
that maps the prim's path to itself. Through another target the edit is
allowed.

Two native instances reference `asset.usda`: `/Instance`, a mesh with a
mesh child and a material child, and `/Looks`, a material with a shader
child. The meshes have only the properties whose names sort before those
the routes author, and the UI hint route writes one metadata field: OpenUSD
exports a prim's properties and metadata by name, and layerstack's USDA
save in the order they were authored. Each case starts from fresh copies of `scene.usda` (the root layer)
and `asset.usda`, and makes one route's calls on one prim:

- `root`: the instance itself, through the root layer;
- `proxy`: its child, an instance proxy, through the root layer;
- `prototype`: that child in the instance's prototype (`/__Prototype_N`),
  found through the instance (`GetPrototype`): OpenUSD numbers prototypes
  in no fixed order, so the case records the instance (`prototype_of`)
  and the path below its prototype, never the number;
- `new`: a path beneath the instance that no prim has;
- `nonlocal`: the proxy through the instance's reference node, whose map
  takes `/Instance/Child` to `/Asset/Child` in `asset.usda`.

For each case the oracle records whether the calls raised an error
(`raised`) and the edit target layer's exported text after them (`text`).
A call that raised must leave the text as it was. `before` is each layer's
text before any call.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Gf, Pcp, Sdf, Usd, UsdGeom, UsdLux, UsdShade, Vt

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "proxy_edits"))

SCENE = '''#usda 1.0

def "Instance" (
    instanceable = true
    prepend references = @asset.usda@</Asset>
)
{
}

def "Looks" (
    instanceable = true
    prepend references = @asset.usda@</Looks>
)
{
}
'''

ASSET = '''#usda 1.0

def Mesh "Asset"
{
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]

    def Mesh "Child"
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
    }

    def Material "Mat"
    {
    }
}

def Material "Looks"
{
    def Shader "Surface"
    {
        uniform token info:id = "UsdPreviewSurface"
    }
}
'''

LAYERS = {"scene.usda": SCENE, "asset.usda": ASSET}

# Each family is an instance and the child its proxy cases edit.
FAMILIES = {"geometry": ("/Instance", "Child"), "shading": ("/Looks", "Surface")}


def r_define(stage, prim, root):
    stage.DefinePrim(root.edited, "Scope")


def r_apply_single(stage, prim, root):
    UsdLux.ShapingAPI.Apply(prim)


def r_apply_multiple(stage, prim, root):
    Usd.CollectionAPI.Apply(prim, "set")


def r_apply_by_name(stage, prim, root):
    if not prim.ApplyAPI("MotionAPI"):
        raise RuntimeError("ApplyAPI failed")


def r_attribute_default(stage, prim, root):
    UsdGeom.Imageable(prim).CreatePurposeAttr().Set(UsdGeom.Tokens.proxy)


def r_attribute_sample(stage, prim, root):
    UsdGeom.Imageable(prim).CreateVisibilityAttr().Set(UsdGeom.Tokens.invisible, 5.0)


def r_targets(stage, prim, root):
    UsdGeom.Imageable(prim).CreateProxyPrimRel().SetTargets([Sdf.Path(root.path)])


def r_add_target(stage, prim, root):
    prim.CreateRelationship("links", True).AddTarget(Sdf.Path(root.path))


def r_property_metadata(stage, prim, root):
    UsdGeom.Imageable(prim).GetPurposeAttr().SetMetadata("documentation", "Tagged")


def r_primvar(stage, prim, root):
    UsdGeom.PrimvarsAPI(prim).CreatePrimvar("tint", Sdf.ValueTypeNames.Float).Set(0.5)


def r_subset(stage, prim, root):
    subset = UsdGeom.Subset.CreateGeomSubset(
        UsdGeom.Imageable(prim), "part", UsdGeom.Tokens.face, Vt.IntArray([0]), "family")
    if not subset:
        raise RuntimeError("no subset")


def r_binding(stage, prim, root):
    material = UsdShade.Material(stage.GetPrimAtPath(root.path + "/Mat"))
    UsdShade.MaterialBindingAPI.Apply(prim).Bind(material)


def r_xform_op(stage, prim, root):
    UsdGeom.Xformable(prim).AddTranslateOp(UsdGeom.XformOp.PrecisionDouble).Set(
        Gf.Vec3d(1, 2, 3))


def r_make_invisible(stage, prim, root):
    UsdGeom.Imageable(prim).MakeInvisible()


def r_ui_hints(stage, prim, root):
    # The `uiHints` dictionary alone, which `UsdUI.ObjectHints` writes with
    # the legacy `displayName` field.
    UsdGeom.Imageable(prim).GetPurposeAttr().SetMetadataByDictKey(
        "uiHints", "displayName", "Shown")


def r_port(stage, prim, root):
    UsdShade.ConnectableAPI(prim).CreateInput("roughness", Sdf.ValueTypeNames.Float).Set(0.25)


# (route, family); `tests/proxy_edits.rs` has a route of each name.
ROUTES = [
    ("define", "geometry", r_define),
    ("apply_single", "geometry", r_apply_single),
    ("apply_multiple", "geometry", r_apply_multiple),
    ("apply_by_name", "geometry", r_apply_by_name),
    ("attribute_default", "geometry", r_attribute_default),
    ("attribute_sample", "geometry", r_attribute_sample),
    ("targets", "geometry", r_targets),
    ("add_target", "geometry", r_add_target),
    ("property_metadata", "geometry", r_property_metadata),
    ("primvar", "geometry", r_primvar),
    ("subset", "geometry", r_subset),
    ("binding", "geometry", r_binding),
    ("xform_op", "geometry", r_xform_op),
    ("make_invisible", "geometry", r_make_invisible),
    ("ui_hints", "geometry", r_ui_hints),
    ("port", "shading", r_port),
]

SITUATIONS = ["root", "proxy", "prototype", "new", "nonlocal"]


class Root:
    """The family's instance's path, and the path the case edits."""

    def __init__(self, path, edited):
        self.path = path
        self.edited = edited


def reload_layers(out):
    for name in LAYERS:
        layer = Sdf.Layer.Find(os.path.join(out, name))
        if layer:
            layer.Reload(force=True)


def open_stage(out):
    reload_layers(out)
    return Usd.Stage.Open(os.path.join(out, "scene.usda"))


def case_path(stage, family, situation):
    """The path the case edits on `stage`, and how the oracle records it:
    a prototype prim relative to the prototype of its instance."""
    root, child = FAMILIES[family]
    if situation == "root":
        return root, {"path": root}
    if situation in ("proxy", "nonlocal"):
        return f"{root}/{child}", {"path": f"{root}/{child}"}
    if situation == "prototype":
        prototype = stage.GetPrimAtPath(root).GetPrototype()
        assert prototype, f"{root} is an instance"
        return f"{prototype.GetPath()}/{child}", {"prototype_of": root, "path": child}
    return f"{root}/New", {"path": f"{root}/New"}


def run_case(out, route, family, situation):
    stage = open_stage(out)
    root_path = FAMILIES[family][0]
    path, recorded = case_path(stage, family, situation)
    if situation == "nonlocal":
        target_name = "asset.usda"
        target_layer = Sdf.Layer.Find(os.path.join(out, target_name))
        node = stage.GetPrimAtPath(root_path).GetPrimIndex().rootNode.children[0]
        assert node.arcType == Pcp.ArcTypeReference
        stage.SetEditTarget(Usd.EditTarget(target_layer, node))
    else:
        target_name = "scene.usda"
        target_layer = Sdf.Layer.Find(os.path.join(out, target_name))
        stage.SetEditTarget(Usd.EditTarget(target_layer))
    before = target_layer.ExportToString()
    prim = stage.GetPrimAtPath(path)
    try:
        dict((name, fn) for name, _, fn in ROUTES)[route](stage, prim, Root(root_path, path))
        raised = False
    except Exception:
        raised = True
    text = target_layer.ExportToString()
    if raised and text != before:
        raise SystemExit(f"{route} {situation}: OpenUSD raised after authoring")
    return {"route": route, "family": family, "situation": situation, **recorded,
            "target": target_name, "raised": raised, "text": text}


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    reload_layers(out)
    before = {name: Sdf.Layer.FindOrOpen(os.path.join(out, name)).ExportToString()
              for name in LAYERS}
    cases = [run_case(out, route, family, situation)
             for route, family, _ in ROUTES for situation in SITUATIONS]
    reload_layers(out)
    _, minor, patch = Usd.GetVersion()
    oracle = {"openusd_version": f"{minor}.{patch}", "before": before, "cases": cases}
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
