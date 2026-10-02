# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin built-in point and curve extents to OpenUSD 26.8."""
import json
import pathlib
from pxr import Usd, UsdGeom

assert Usd.GetVersion() == (0, 26, 8)
fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(fixtures / "point_curve_bounds.usda"))
rows = []
for code in (None, 1, 2, 3):
    time = Usd.TimeCode.Default() if code is None else Usd.TimeCode(code)
    for prim in stage.Traverse():
        extent = UsdGeom.Boundable.ComputeExtentFromPlugins(UsdGeom.Boundable(prim), time)
        rows.append({"path": str(prim.GetPath()), "time": code,
                     "extent": [list(v) for v in extent] if extent else None})
print(json.dumps(rows, indent=2))
