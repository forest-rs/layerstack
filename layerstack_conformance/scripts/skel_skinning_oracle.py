# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin inherited/indexed influences and CPU LBS to OpenUSD 26.8."""
import json
import pathlib
from pxr import Usd, UsdSkel, UsdGeom

assert Usd.GetVersion() == (0, 26, 8)
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(fixtures / "skel_skinning.usda"))
cache = UsdSkel.Cache()
cache.Populate(UsdSkel.Root(stage.GetPrimAtPath("/Rig")), Usd.PrimDefaultPredicate)
skel = cache.GetSkelQuery(UsdSkel.Skeleton(stage.GetPrimAtPath("/Rig/Skeleton")))
rows = []
for code in (None, 1, 2, 3):
    time = Usd.TimeCode.Default() if code is None else Usd.TimeCode(code)
    for path in ("/Rig/Geometry/Mesh", "/Rig/Rigid"):
        prim = stage.GetPrimAtPath(path)
        query = cache.GetSkinningQuery(prim)
        points = UsdGeom.PointBased(prim).GetPointsAttr().Get(time)
        assert query.ComputeSkinnedPoints(skel.ComputeSkinningTransforms(time), points, time)
        rows.append({"path": path, "time": code, "points": [list(p) for p in points]})
print(json.dumps(rows, indent=2))
