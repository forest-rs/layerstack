# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Regenerate authored-extent BBoxCache evidence with the C++ implementation."""
import json
import math
import sys
from pathlib import Path
from pxr import Gf, Sdf, Usd, UsdGeom

DEST = Path(__file__).resolve().parents[1] / "fixtures" / "bounds"
stage = Usd.Stage.CreateInMemory()

def xform(path, angle=0, shift=(0, 0, 0), kind=None):
    obj = UsdGeom.Xform.Define(stage, path)
    obj.AddTranslateOp().Set(Gf.Vec3d(*shift))
    obj.AddRotateZOp().Set(angle)
    if kind:
        Usd.ModelAPI(obj).SetKind(kind)
    return obj

def mesh(path, purpose=None, invisible=False):
    obj = UsdGeom.Mesh.Define(stage, path)
    obj.CreateExtentAttr([(-1, -2, -3), (2, 3, 4)])
    if purpose:
        obj.CreatePurposeAttr(purpose)
    if invisible:
        obj.CreateVisibilityAttr("invisible")
    return obj

xform("/World", 23, (3, 4, 5), "assembly")
xform("/World/Model", 31, (2, -3, 1), "component")
xform("/World/Model/Tilt", 17, (1, 2, 3))
mesh("/World/Model/Tilt/A")
mesh("/World/Model/Tilt/Render", "render")
xform("/World/Model/Reset", 13, (8, 0, 0)).SetResetXformStack(True)
mesh("/World/Model/Reset/B")
xform("/World/Model/Hidden", 12).CreateVisibilityAttr("invisible")
mesh("/World/Model/Hidden/Leaf")
mesh("/World/Model/HiddenMesh", invisible=True)
mesh("/World/Model/HiddenMesh/Pruned", "proxy")
xform("/World/Model/Proxy").CreatePurposeAttr("proxy")
mesh("/World/Model/Proxy/C")
xform("/World/Model/Sub", 44, (1, 3, 0), "subcomponent")
mesh("/World/Model/Sub/D", "guide")
xform("/World/Unmodelled", 37)
mesh("/World/Unmodelled/E")
hint = xform("/World/Hint", 11, kind="component")
hint.GetPrim().CreateAttribute("extentsHint", Sdf.ValueTypeNames.Float3Array).Set([
    (-10, -1, -1), (10, 1, 1), (-20, -2, -2), (20, 2, 2),
    (1, 1, 1), (-1, -1, -1), (-40, -4, -4), (40, 4, 4)])
mesh("/World/Hint/Child")
stage.DefinePrim("/World/Unknown", "FutureSchema")
mesh("/World/Unknown/Child")
stage.DefinePrim("/World/Typed", "Material")
mesh("/World/Typed/Child")
xform("/Isolated")
stage.DefinePrim("/Isolated/Typed", "Material")
mesh("/Isolated/Typed/Child").CreateExtentAttr([(10, 20, 30), (40, 50, 60)])
class_mesh = mesh("/World/Class")
class_mesh.GetPrim().SetSpecifier(Sdf.SpecifierClass)
# An authored extent is authoritative even with descendants.
mesh("/World/Boundable")
mesh("/World/Boundable/Child").CreateExtentAttr([(-100, -100, -100), (100, 100, 100)])
xform("/Prototype")
mesh("/Prototype/Leaf")
for name, purpose in (("I1", "render"), ("I2", "proxy")):
    inst = stage.DefinePrim("/World/" + name)
    inst.GetReferences().AddInternalReference("/Prototype")
    inst.SetInstanceable(True)
    UsdGeom.Imageable(inst).CreatePurposeAttr(purpose)
# Def descendants inherit undefined/abstract flags from ancestors. Direct
# queries of a leaf's own authored extent nevertheless return that extent.
for name, specifier in (("Undefined", Sdf.SpecifierOver), ("Abstract", Sdf.SpecifierClass)):
    ancestor = xform("/" + name)
    xform("/" + name + "/Middle")
    mesh("/" + name + "/Middle/Leaf")
    ancestor.GetPrim().SetSpecifier(specifier)
animated = UsdGeom.Mesh.Get(stage, "/World/Model/Tilt/A")
animated.GetExtentAttr().Set([(-2,-3,-4),(3,4,5)], 0.0)
animated.GetExtentAttr().Set([(-4,-5,-6),(5,6,7)], 2.0)
# Temporal dependencies include ancestors, reset stacks, excluded visibility,
# and hints that are blocked at one time but become authoritative later.
stage.GetPrimAtPath("/World").GetAttribute("xformOp:rotateZ").Set(23, 0.0)
stage.GetPrimAtPath("/World").GetAttribute("xformOp:rotateZ").Set(61, 2.0)
hidden = stage.GetPrimAtPath("/World/Model/Hidden").GetAttribute("visibility")
hidden.Set("invisible", 0.0)
hidden.Set("inherited", 2.0)
hints_attr = hint.GetPrim().GetAttribute("extentsHint")
hints_attr.Set(Sdf.ValueBlock(), 0.0)
hints_attr.Set([(-30, -3, -3), (30, 3, 3)], 2.0)
DEST.mkdir(parents=True, exist_ok=True)
scene_text = stage.GetRootLayer().ExportToString().rstrip() + "\n"
if "--check" in sys.argv:
    assert (DEST / "scene.usda").read_text() == scene_text, "bounds scene differs"
else:
    (DEST / "scene.usda").write_text(scene_text)

records = []
for hints in (False, True):
    for ignore in (False, True):
        for purposes in (["default"], ["render"], ["proxy"], ["guide"], ["default", "render", "proxy", "guide"]):
            cache = UsdGeom.BBoxCache(Usd.TimeCode.Default(), purposes, hints, ignore)
            for prim in [stage.GetPseudoRoot()] + list(stage.TraverseAll()):
                bounds = {}
                for name, method in (("world", cache.ComputeWorldBound), ("local", cache.ComputeLocalBound), ("untransformed", cache.ComputeUntransformedBound)):
                    box = method(prim)
                    extent = box.GetRange()
                    bounds[name] = {"min": list(extent.GetMin()), "max": list(extent.GetMax()), "matrix": [list(row) for row in box.GetMatrix()]}
                records.append({"path": str(prim.GetPath()), "hints": hints, "ignore": ignore, "purposes": purposes, "bounds": bounds})
for hints in (False, True):
    for time in (0.0, 1.0, 2.0, 0.0, None):
        cache = UsdGeom.BBoxCache(Usd.TimeCode.Default() if time is None else Usd.TimeCode(time), ["default"], hints)
        # OpenUSD 26.8 _Resolve swaps _ctmCache before computing the initial
        # component inverse, which then uses time zero for a cold non-component
        # query. Populate through /World so this oracle measures the intended
        # component-space result, independent of that cold-query defect.
        # Hidden is tested below with visibility bypassed so traversal reaches it.
        cache.ComputeWorldBound(stage.GetPrimAtPath("/World"))
        for path in ("/World", "/World/Model", "/World/Model/Tilt", "/World/Model/Tilt/A", "/World/Model/Reset", "/World/Hint", "/World/Unmodelled"):
            prim = stage.GetPrimAtPath(path)
            bounds = {}
            for name, method in (("world", cache.ComputeWorldBound), ("local", cache.ComputeLocalBound), ("untransformed", cache.ComputeUntransformedBound)):
                box = method(prim)
                bounds[name] = {"min": list(box.GetRange().GetMin()), "max": list(box.GetRange().GetMax()), "matrix": [list(row) for row in box.GetMatrix()]}
            records.append({"path": path, "time": time, "hints": hints, "ignore": False, "purposes": ["default"], "bounds": bounds})
for time in (0.0, 1.0, 2.0):
    cache = UsdGeom.BBoxCache(Usd.TimeCode(time), ["default"], False, True)
    cache.ComputeWorldBound(stage.GetPrimAtPath("/World/Model"))
    prim = stage.GetPrimAtPath("/World/Model/Hidden")
    bounds = {}
    for name, method in (("world", cache.ComputeWorldBound), ("local", cache.ComputeLocalBound), ("untransformed", cache.ComputeUntransformedBound)):
        box = method(prim)
        bounds[name] = {"min": list(box.GetRange().GetMin()), "max": list(box.GetRange().GetMax()), "matrix": [list(row) for row in box.GetMatrix()]}
    records.append({"path": str(prim.GetPath()), "time": time, "hints": False, "ignore": True, "purposes": ["default"], "bounds": bounds})
for time in (0.0, 1.0, 2.0):
    cache = UsdGeom.BBoxCache(Usd.TimeCode(time), ["default"], False, True)
    cache.ComputeWorldBound(stage.GetPrimAtPath("/World"))
    xforms = UsdGeom.XformCache(Usd.TimeCode(time))
    for path, target in (("/World/Model/Tilt/A", "/World/Model"), ("/World/Model/Reset/B", "/World/Model"), ("/World/Model/Reset/B", "/World/Model/Reset"), ("/World/Model/Tilt", "/World/Model/Tilt"), ("/World/Model/Tilt/A", "/World/Hint"), ("/World/Model/Tilt/A", "/")):
        prim, ancestor = stage.GetPrimAtPath(path), stage.GetPrimAtPath(target)
        box = cache.ComputeRelativeBound(prim, ancestor)
        # OpenUSD 26.8's Python wrapper leaves resetXformStack uninitialized
        # when prim == ancestor: the C++ loop never writes it. Do not serialize
        # that undefined result. Rust tests the identity/false contract directly;
        # the relative-bound comparison below still covers the self case.
        transform = {}
        if prim != ancestor:
            matrix, reset = xforms.ComputeRelativeTransform(prim, ancestor)
            transform = {"transform": {"matrix": [list(row) for row in matrix], "reset": reset}}
        records.append({"path": path, "relative_to": target, "time": time, "hints": False, "ignore": True, "purposes": ["default"], "bounds": {"relative": {"min": list(box.GetRange().GetMin()), "max": list(box.GetRange().GetMax()), "matrix": [list(row) for row in box.GetMatrix()]}}, **transform})
# One record per line keeps oracle diffs local without redundant indentation.
header = json.dumps({"openusd_version": ".".join(map(str, Usd.GetVersion()[1:]))})[:-1]
output = header + ',"records":[\n' + ',\n'.join(json.dumps(record, separators=(",", ":")) for record in records) + '\n]}\n'
def compare(expected, actual, path=""):
    if isinstance(expected, dict):
        assert expected.keys() == actual.keys(), path
        for key in expected:
            compare(expected[key], actual[key], path + "/" + key)
    elif isinstance(expected, list):
        assert len(expected) == len(actual), path
        for index, (left, right) in enumerate(zip(expected, actual)):
            compare(left, right, path + "/" + str(index))
    elif isinstance(expected, float):
        assert math.isclose(expected, actual, rel_tol=1e-12, abs_tol=1e-12), (path, expected, actual)
    else:
        assert expected == actual, (path, expected, actual)
if "--check" in sys.argv:
    compare(json.loads((DEST / "oracle.json").read_text()), json.loads(output))
else:
    (DEST / "oracle.json").write_text(output)
