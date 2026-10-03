# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record intrinsic UsdLux extents from pinned OpenUSD, including float rounding."""
import argparse
import json
import pathlib
from pxr import Usd, UsdGeom, UsdLux
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
stage = Usd.Stage.CreateInMemory()
rows = []
for name in ["SphereLight", "CylinderLight", "DiskLight", "RectLight", "PortalLight"]:
    light = getattr(UsdLux, name).Define(stage, "/" + name)
    for param in ["radius", "length", "width", "height"]:
        attr = light.GetPrim().GetAttribute("inputs:" + param)
        if attr:
            attr.Set(1.234567, 1)
            attr.Set(3.456789, 3)
    for time in [None, 1, 2, 3]:
        code = Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)
        extent = UsdGeom.Boundable.ComputeExtentFromPlugins(UsdGeom.Boundable(light), code)
        rows.append({"schema": name, "time": time, "extent": [list(v) for v in extent]})
for name, param in [
    ("SphereLight", "radius"), ("DiskLight", "radius"),
    ("CylinderLight", "radius"), ("CylinderLight", "length"),
    ("RectLight", "width"), ("RectLight", "height"),
    ("PortalLight", "width"), ("PortalLight", "height"),
]:
    light = getattr(UsdLux, name).Define(stage, "/Blocked" + name + param)
    light.GetPrim().GetAttribute("inputs:" + param).Block()
    for time in [None, 1]:
        code = Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)
        extent = UsdGeom.Boundable.ComputeExtentFromPlugins(UsdGeom.Boundable(light), code)
        rows.append({"schema": name, "time": time, "blocked_dimension": param,
                     "extent": None if extent is None else [list(v) for v in extent]})
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows}
output = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "lux_extents.json"
if args.check:
    assert json.loads(output.read_text()) == result
else:
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
