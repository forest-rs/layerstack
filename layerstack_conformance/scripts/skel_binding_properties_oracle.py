# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""The properties `UsdSkelSkinningQuery` names for each skinning input
(`GetJointIndicesPrimvar`, `GetJointWeightsPrimvar`,
`GetGeomBindTransformAttr`, `GetSkinningMethodAttr`, `GetBlendShapesAttr`,
`GetBlendShapeTargetsRel`), through `UsdSkelCache` with instance proxies.
Inside the two instances C++ names the prototype's property
(`/__Prototype_1/...`), except `skinningMethod`, named at the proxy."""
import json
import sys
from pathlib import Path
from pxr import Usd, UsdGeom, UsdSkel

here = Path(__file__).resolve().parents[1] / "fixtures" / "skel_binding_properties"
stage = Usd.Stage.Open(str(here / "scene.usda"))
cache = UsdSkel.Cache()
root = UsdSkel.Root(stage.GetPrimAtPath("/World"))
cache.Populate(root, Usd.TraverseInstanceProxies())


def path(obj):
    if isinstance(obj, UsdGeom.Primvar):
        obj = obj.GetAttr()
    return str(obj.GetPath()) if obj.IsValid() else None


targets = []
for binding in cache.ComputeSkelBindings(root, Usd.TraverseInstanceProxies()):
    for query in binding.GetSkinningTargets():
        targets.append({
            "geometry": str(query.GetPrim().GetPath()),
            "joint_indices": path(query.GetJointIndicesPrimvar()),
            "joint_weights": path(query.GetJointWeightsPrimvar()),
            "geom_bind_transform": path(query.GetGeomBindTransformAttr()),
            "skinning_method": path(query.GetSkinningMethodAttr()),
            "blend_shapes": path(query.GetBlendShapesAttr()),
            "blend_shape_targets": path(query.GetBlendShapeTargetsRel()),
        })
targets.sort(key=lambda t: t["geometry"])
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "targets": targets}
text = json.dumps(result, indent=2) + "\n"
dest = here / "oracle.json"
if "--check" in sys.argv:
    assert dest.read_text() == text, "skel binding properties oracle differs"
else:
    dest.write_text(text)
