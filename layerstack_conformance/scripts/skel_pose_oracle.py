# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin sparse skeletal pose evaluation and inherited bindings to OpenUSD 26.8."""
import json
import pathlib
from pxr import Usd, UsdSkel

assert Usd.GetVersion() == (0, 26, 8)
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(fixtures / "skel_pose.usda"))
cache = UsdSkel.Cache()
query = cache.GetSkelQuery(UsdSkel.Skeleton(stage.GetPrimAtPath("/Rig/Skeleton")))
def matrices(values):
    return [[list(row) for row in matrix] for matrix in values]
rows = []
for code in (None, 1, 2, 3):
    time = Usd.TimeCode.Default() if code is None else Usd.TimeCode(code)
    rows.append({"time": code, "local": matrices(query.ComputeJointLocalTransforms(time)),
                 "skeleton": matrices(query.ComputeJointSkelTransforms(time)),
                 "skinning": matrices(query.ComputeSkinningTransforms(time))})
bindings = []
for path in ("/Rig/Inherited", "/Rig/Unbound/Child"):
    binding = UsdSkel.BindingAPI(stage.GetPrimAtPath(path))
    skel = binding.GetInheritedSkeleton()
    anim = binding.GetInheritedAnimationSource()
    bindings.append({"path": path, "skeleton": str(skel.GetPath()) if skel else None,
                     "animation": str(anim.GetPath()) if anim else None})
print(json.dumps({"poses": rows, "bindings": bindings}, indent=2))
