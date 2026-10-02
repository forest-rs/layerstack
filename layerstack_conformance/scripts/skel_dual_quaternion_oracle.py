# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin DQS kernels and bound deformation to OpenUSD 26.8."""
import json
import pathlib
from pxr import Usd, UsdGeom, UsdSkel, Gf, Vt
assert Usd.GetVersion() == (0, 26, 8)
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
def rotate(axis, degrees, translation=(0,0,0)):
    matrix = Gf.Matrix4d(1).SetRotate(Gf.Rotation(Gf.Vec3d(*axis), degrees))
    matrix.SetRow(3, Gf.Vec4d(*translation, 1))
    return matrix
def affine(scale, shear, angle, translation):
    matrix = Gf.Matrix4d(1).SetScale(Gf.Vec3d(*scale))
    matrix[0,1] = shear
    return matrix * rotate((0,0,1), angle, translation)
def rows(matrix):
    return [list(r) for r in matrix]
identity = Gf.Matrix4d(1)
bind = affine((1.2,0.7,2),0.25,0,(1.123456789,2,3))
points = [(1,0,0), (0,2,0), (-0.2,0.3,0.9)]
normals = [(1,1,0), (0,1,1), (1,0,1)]
wrap = [rotate((0,0,1), -140), identity, rotate((0,0,1), 140)]
mixed = [affine((2,0.5,1),0.3,30,(1,2,3)), affine((-1,2,0.7),-0.2,-80,(-2,0,1)), rotate((1,1,0),120,(3,1,-2))]
singular = Gf.Matrix4d(1).SetScale(Gf.Vec3d(0,0,0))
cases = [
    ("twist",identity,[identity,rotate((0,0,1),120)],[0.5,0.5]),
    ("hemisphere_max_pivot",bind,wrap,[0.2,0.3,0.5]),
    ("hemisphere_first_tie",bind,wrap,[0.4,0.4,0.4]),
    ("scale_shear_reflection",bind,mixed,[0.2,0.3,0.5]),
    ("negative_weights",bind,mixed,[-0.5,1.25,0]),
    ("zero_weights_rigid",identity,wrap,[0,0,0]),
    ("zero_weights_scaled",identity,mixed,[0,0,0]),
    ("nonunit_rigid",identity,[identity],[2]),
    ("nonunit_scaled",identity,[affine((2,3,4),0,0,(1,2,3))],[2]),
    ("singular_factor_fallback",bind,[singular,rotate((0,0,1),45,(2,3,4))],[0.4,0.6]),
    ("all_singular",identity,[singular],[1]),
    ("near_singular",bind,[affine((1e-8,1,1),0.2,30,(0,1,2)),mixed[0]],[0.7,0.3]),
]
result = {"kernels": []}
for name, geom_bind, joints, weights in cases:
    indices = list(range(len(joints)))
    ids = Vt.IntArray(indices * len(points))
    ws = Vt.FloatArray(weights * len(points))
    deformed = Vt.Vec3fArray(points)
    assert UsdSkel.SkinPoints("dualQuaternion",geom_bind,Vt.Matrix4dArray(joints),ids,ws,len(joints),deformed)
    row = {"name": name,"bind":rows(geom_bind),"joints":[rows(m) for m in joints],"indices":indices,"weights":list(Vt.FloatArray(weights)),"source_points":points,"points":[list(p) for p in deformed]}
    row["rigid"] = rows(UsdSkel.SkinTransform("dualQuaternion",geom_bind,Vt.Matrix4dArray(joints),Vt.IntArray(indices),Vt.FloatArray(weights)))
    if all(abs(m.GetDeterminant3()) > 0 for m in joints):
        ns = Vt.Vec3fArray(normals)
        normal_matrices = Vt.Matrix3dArray([m.ExtractRotationMatrix().GetInverse().GetTranspose() for m in joints])
        normal_bind = geom_bind.ExtractRotationMatrix().GetInverse().GetTranspose()
        assert UsdSkel.SkinNormals("dualQuaternion",normal_bind,normal_matrices,ids,ws,len(joints),ns)
        row["source_normals"] = normals
        row["normals"] = [list(n) for n in ns]
    result["kernels"].append(row)
source = (fixtures / "skel_normals.usda").read_text().replace("rel skel:skeleton = </Rig/Skeleton>", "rel skel:skeleton = </Rig/Skeleton>\n token primvars:skel:skinningMethod = \"dualQuaternion\"")
(fixtures / "skel_dual_quaternion.usda").write_text(source)
stage = Usd.Stage.Open(str(fixtures / "skel_dual_quaternion.usda"))
cache = UsdSkel.Cache()
cache.Populate(UsdSkel.Root(stage.GetPrimAtPath("/Rig")),Usd.PrimDefaultPredicate)
skel = cache.GetSkelQuery(UsdSkel.Skeleton(stage.GetPrimAtPath("/Rig/Skeleton")))
result["frames"] = []
for code in (None,1,2,3):
    time = Usd.TimeCode.Default() if code is None else Usd.TimeCode(code)
    transforms = skel.ComputeSkinningTransforms(time)
    for path in ("/Rig/Geometry/Vertex","/Rig/Geometry/Corners","/Rig/Rigid"):
        prim = stage.GetPrimAtPath(path)
        query = cache.GetSkinningQuery(prim)
        deformed = UsdGeom.PointBased(prim).GetPointsAttr().Get(time)
        assert query.ComputeSkinnedPoints(transforms,deformed,time)
        ns = UsdGeom.PointBased(prim).GetNormalsAttr().Get(time)
        ids, ws = query.ComputeVaryingJointInfluences(2,time)
        if path.endswith("Corners"):
            # The face-varying normal wrapper is absent; use its explicit
            # corner-to-point influence expansion with the C++ normal kernel.
            corners = UsdGeom.Mesh(prim).GetFaceVertexIndicesAttr().Get(time)
            ids = Vt.IntArray([ids[p*2+i] for p in corners for i in range(2)])
            ws = Vt.FloatArray([ws[p*2+i] for p in corners for i in range(2)])
        matrices = Vt.Matrix3dArray([m.ExtractRotationMatrix().GetInverse().GetTranspose() for m in transforms])
        normal_bind = query.GetGeomBindTransform(time).ExtractRotationMatrix().GetInverse().GetTranspose()
        assert UsdSkel.SkinNormals("dualQuaternion",normal_bind,matrices,ids,ws,2,ns)
        row = {"path":path,"time":code,"points":[list(p) for p in deformed],"normals":[list(n) for n in ns]}
        if path.endswith("Rigid"):
            row["rigid"] = rows(query.ComputeSkinnedTransform(transforms,time))
        result["frames"].append(row)
# Verify the complete blend-shape-before-DQS ordering as well.
source = (fixtures / "skel_blend_shapes.usda").read_text().replace("rel skel:skeleton = </Rig/Skeleton>", "rel skel:skeleton = </Rig/Skeleton>\n token primvars:skel:skinningMethod = \"dualQuaternion\"")
stage = Usd.Stage.CreateInMemory()
stage.GetRootLayer().ImportFromString(source)
cache = UsdSkel.Cache()
cache.Populate(UsdSkel.Root(stage.GetPrimAtPath("/Rig")),Usd.PrimDefaultPredicate)
skel = cache.GetSkelQuery(UsdSkel.Skeleton(stage.GetPrimAtPath("/Rig/Skeleton")))
prim = stage.GetPrimAtPath("/Rig/Geometry/Mesh")
skin = cache.GetSkinningQuery(prim)
blend = UsdSkel.BlendShapeQuery(UsdSkel.BindingAPI(prim))
indices = blend.ComputeBlendShapePointIndices()
offsets = blend.ComputeSubShapePointOffsets()
result["blends"] = []
for code in (None,1,2,3):
    time = Usd.TimeCode.Default() if code is None else Usd.TimeCode(code)
    weights = skin.GetBlendShapeMapper().Remap(skel.GetAnimQuery().ComputeBlendShapeWeights(time))
    subweights, shapes, subs = blend.ComputeSubShapeWeights(weights)
    points = UsdGeom.PointBased(prim).GetPointsAttr().Get(time)
    assert blend.ComputeDeformedPoints(subweights,shapes,subs,indices,offsets,points)
    assert skin.ComputeSkinnedPoints(skel.ComputeSkinningTransforms(time),points,time)
    result["blends"].append({"time":code,"points":[list(p) for p in points]})
print(json.dumps(result,indent=2))
