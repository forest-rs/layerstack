# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Read the authored multi-producer example through OpenUSD's C++ bindings."""
import json
import sys
from pxr import Usd, UsdGeom, UsdShade

stage = Usd.Stage.Open(sys.argv[1])
time = Usd.TimeCode(13)
prim = stage.GetPrimAtPath('/World/Native_0/Geometry')
mesh = UsdGeom.Mesh(prim)
source = UsdGeom.Mesh(stage.GetPrimAtPath('/Assets/Tree/Geometry'))
instancer = UsdGeom.PointInstancer(stage.GetPrimAtPath('/World/Scatter'))
uv = UsdGeom.PrimvarsAPI(prim).GetPrimvar('st')
normals = UsdGeom.PrimvarsAPI(prim).GetPrimvar('normals')
mask = list(instancer.ComputeMaskAtTime(time))
ids = list(instancer.GetIdsAttr().Get(time))
transforms = instancer.ComputeInstanceTransformsAtTime(time, time)
bound = UsdGeom.BBoxCache(time, ['default', 'render', 'proxy']).ComputeWorldBound(instancer.GetPrim()).ComputeAlignedRange()
material, _ = UsdShade.MaterialBindingAPI(prim).ComputeBoundMaterial()
print(json.dumps({
    'nativeInstances': sum(1 for p in stage.Traverse() if p.IsInstance()),
    'isProxy': prim.IsInstanceProxy(),
    'points': [list(v) for v in mesh.GetPointsAttr().Get(time)],
    'counts': list(mesh.GetFaceVertexCountsAttr().Get()),
    'indices': list(mesh.GetFaceVertexIndicesAttr().Get()),
    'uvIndices': list(uv.GetIndices()),
    'uv': [list(v) for v in uv.ComputeFlattened(time)],
    'normalIndices': list(normals.GetIndices()),
    'normals': [list(v) for v in normals.ComputeFlattened(time)],
    'mask': mask,
    'ids': [v for i,v in enumerate(ids) if not mask or mask[i]],
    'transforms': [[list(row) for row in matrix] for matrix in transforms],
    'bounds': [list(bound.GetMin()), list(bound.GetMax())],
    'sourceHeight': source.GetPointsAttr().Get(time)[2][1],
    'material': str(material.GetPath()),
    'userTag': prim.GetAttribute('user:tag').Get(),
}))
