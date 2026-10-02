# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin normal skinning and rigid-frame rounding to OpenUSD 26.8."""
import json
from pathlib import Path
from pxr import Usd, UsdGeom, UsdSkel, Vt
assert Usd.GetVersion() == (0, 26, 8)
fixtures = Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(fixtures / "skel_normals.usda"))
cache = UsdSkel.Cache()
cache.Populate(UsdSkel.Root(stage.GetPrimAtPath("/Rig")), Usd.PrimDefaultPredicate)
skel = cache.GetSkelQuery(UsdSkel.Skeleton(stage.GetPrimAtPath("/Rig/Skeleton")))
anim = cache.GetAnimQuery(stage.GetPrimAtPath("/Rig/Animation"))
rows = []
poses = []
for code in (None, 1, 2, 3):
    time = Usd.TimeCode.Default() if code is None else Usd.TimeCode(code)
    xforms = skel.ComputeSkinningTransforms(time)
    poses.append({"time": code, "local": [[list(r) for r in m] for m in anim.ComputeJointLocalTransforms(time)], "weights": list(anim.ComputeBlendShapeWeights(time))})
    for path in ("/Rig/Geometry/Vertex", "/Rig/Geometry/Corners", "/Rig/Rigid"):
        prim = stage.GetPrimAtPath(path)
        query = cache.GetSkinningQuery(prim)
        normals = UsdGeom.PointBased(prim).GetNormalsAttr().Get(time)
        # Python exposes the C++ normal kernel, but not the SkinningQuery
        # normal method. Supply its inverse-transpose matrices explicitly.
        ids, weights = query.ComputeVaryingJointInfluences(2, time)
        if path.endswith("Corners"):
            # SkinFaceVaryingNormals is also unwrapped: expand influences using
            # its corner-to-point mapping, then invoke the same C++ kernel.
            corners = UsdGeom.Mesh(prim).GetFaceVertexIndicesAttr().Get(time)
            ids = Vt.IntArray([ids[point * 2 + i] for point in corners for i in range(2)])
            weights = Vt.FloatArray([weights[point * 2 + i] for point in corners for i in range(2)])
        normal_xforms = Vt.Matrix3dArray([m.ExtractRotationMatrix().GetInverse().GetTranspose() for m in xforms])
        bind = query.GetGeomBindTransform(time).ExtractRotationMatrix().GetInverse().GetTranspose()
        assert UsdSkel.SkinNormals("classicLinear", bind, normal_xforms, ids, weights, 2, normals)
        row = {"path": path, "time": code, "normals": [list(n) for n in normals]}
        if path.endswith("Rigid"):
            row["transform"] = [list(r) for r in query.ComputeSkinnedTransform(xforms, time)]
        rows.append(row)
print(json.dumps({"frames": rows, "poses": poses, "joint_samples": anim.GetJointTransformTimeSamples(), "weight_samples": anim.GetBlendShapeWeightTimeSamples(), "joint_varying": anim.JointTransformsMightBeTimeVarying(), "weight_varying": anim.BlendShapeWeightsMightBeTimeVarying()}, indent=2))
