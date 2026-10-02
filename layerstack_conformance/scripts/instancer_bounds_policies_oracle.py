# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin independent prototype and caller bounds policies to OpenUSD 26.8."""
import json
import pathlib
from pxr import Usd, UsdGeom

assert Usd.GetVersion() == (0, 26, 8)
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(fixtures / "instancer_bounds_policies.usda"))
rows = []
for ignore in (False, True):
    for hints in (False, True):
        cache = UsdGeom.BBoxCache(Usd.TimeCode.Default(), ["default"], hints, ignore)
        rows.append({"ignore": ignore, "hints": hints, "bounds": [
            [list(r.GetMin()), list(r.GetMax())]
            for r in (cache.ComputeWorldBound(stage.GetPrimAtPath(p)).ComputeAlignedRange()
                      for p in ("/Query/I", "/Query"))]})
print(json.dumps(rows, indent=2))
