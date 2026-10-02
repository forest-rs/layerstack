# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin traversal-root binding scope to OpenUSD 26.8."""
import json
import pathlib
from pxr import Usd, UsdSkel, UsdGeom

assert Usd.GetVersion() == (0, 26, 8)
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(fixtures / "skel_binding_scope.usda"))
rows = []
for name in ("AboveOnly", "SkeletonOnly", "InfluencesOnly", "Local"):
    path = "/Outside/" + name
    root = UsdSkel.Root(stage.GetPrimAtPath(path))
    cache = UsdSkel.Cache()
    assert cache.Populate(root, Usd.PrimDefaultPredicate)
    bindings = cache.ComputeSkelBindings(root, Usd.PrimDefaultPredicate)
    geometries = []
    for binding in bindings:
        skel = cache.GetSkelQuery(binding.GetSkeleton())
        for query in binding.GetSkinningTargets():
            # LayerStack prepares deformation queries only for influenced geometry.
            if not query.HasJointInfluences():
                continue
            prim = query.GetPrim()
            points = UsdGeom.PointBased(prim).GetPointsAttr().Get()
            assert query.ComputeSkinnedPoints(skel.ComputeSkinningTransforms(), points)
            geometries.append({"path": str(prim.GetPath()), "points": [list(p) for p in points]})
    rows.append({"root": path, "geometries": geometries})
print(json.dumps(rows, indent=2))
