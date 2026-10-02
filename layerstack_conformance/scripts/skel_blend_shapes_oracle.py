# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin dense/sparse inbetween deformation before LBS to OpenUSD 26.8."""
import json
import pathlib
from pxr import Usd, UsdGeom, UsdSkel, Vt, Gf

assert Usd.GetVersion() == (0, 26, 8)
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(fixtures / "skel_blend_shapes.usda"))
prim = stage.GetPrimAtPath("/Rig/Geometry/Mesh")
cache = UsdSkel.Cache()
cache.Populate(UsdSkel.Root(stage.GetPrimAtPath("/Rig")), Usd.PrimDefaultPredicate)
skel = cache.GetSkelQuery(UsdSkel.Skeleton(stage.GetPrimAtPath("/Rig/Skeleton")))
skin = cache.GetSkinningQuery(prim)
blend = UsdSkel.BlendShapeQuery(UsdSkel.BindingAPI(prim))
indices = blend.ComputeBlendShapePointIndices()
offsets = blend.ComputeSubShapePointOffsets()
# Normal-offset aggregation is not exposed by the Python wrapper. Use the
# query's exact subshape order and the same C++ deformation kernel for vectors.
normal_offsets = []
for i, point_offsets in enumerate(offsets):
    inbetween = blend.GetInbetween(i)
    if not point_offsets:
        normal_offsets.append(Vt.Vec3fArray())
    elif inbetween:
        normal_offsets.append(inbetween.GetNormalOffsets())
    else:
        normal_offsets.append(blend.GetBlendShape(blend.GetBlendShapeIndex(i)).GetNormalOffsetsAttr().Get())
def deform(weights):
    sub_weights, shape_indices, sub_indices = blend.ComputeSubShapeWeights(Vt.FloatArray(weights))
    points = UsdGeom.PointBased(prim).GetPointsAttr().Get()
    normals = Vt.Vec3fArray([Gf.Vec3f(0,0,1), Gf.Vec3f(0,0,1)])
    assert blend.ComputeDeformedPoints(sub_weights, shape_indices, sub_indices, indices, offsets, points)
    assert blend.ComputeDeformedPoints(sub_weights, shape_indices, sub_indices, indices, normal_offsets, normals)
    return points, normals
rows = []
for code in (None, 1, 2, 3):
    time = Usd.TimeCode.Default() if code is None else Usd.TimeCode(code)
    anim_weights = skel.GetAnimQuery().ComputeBlendShapeWeights(time)
    weights = skin.GetBlendShapeMapper().Remap(anim_weights)
    points, normals = deform(weights)
    local = [list(p) for p in points]
    assert skin.ComputeSkinnedPoints(skel.ComputeSkinningTransforms(time), points, time)
    rows.append({"time": code, "weights": list(weights), "local": local, "normals": [list(n) for n in normals], "skinned": [list(p) for p in points]})
extrapolation = []
for weight in (-0.25, 0, 0.5, 1, 1.25):
    points, normals = deform([weight,0])
    extrapolation.append({"weight": weight, "points": [list(p) for p in points]})
print(json.dumps({"frames": rows, "extrapolation": extrapolation}, indent=2))
