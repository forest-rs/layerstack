# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Composed local transform time queries, using OpenUSD's public API."""
import json
import sys
from pathlib import Path
from pxr import Gf, Sdf, Ts, Usd, UsdGeom

stage = Usd.Stage.CreateInMemory()
source = UsdGeom.Xform.Define(stage, "/Source")
op = source.AddTranslateOp()
op.Set(Gf.Vec3d(1), 0)
op.Set(Gf.Vec3d(2), 2)
for name in ("Offset", "Masked", "Blocked"):
    prim = stage.DefinePrim("/" + name)
    prim.GetReferences().AddInternalReference("/Source", Sdf.LayerOffset(5, 2))
    if name != "Offset":
        attr = prim.CreateAttribute("xformOp:translate", Sdf.ValueTypeNames.Double3)
        attr.Set(Gf.Vec3d(7) if name == "Masked" else Sdf.ValueBlock())
animated = UsdGeom.Xform.Define(stage, "/Animated")
t = animated.AddTranslateOp()
t.Set(Gf.Vec3d(1), 0)
t.Set(Gf.Vec3d(2), 2)
r = animated.AddRotateXYZOp()
r.Set(Gf.Vec3f(1), 1)
r.Set(Gf.Vec3f(2), 3)
single = UsdGeom.Xform.Define(stage, "/Single").AddTranslateOp()
single.Set(Gf.Vec3d(1))
single.Set(Gf.Vec3d(2), 4)
UsdGeom.Xform.Define(stage, "/Static").AddTranslateOp().Set(Gf.Vec3d(3))
reset = UsdGeom.Xform.Define(stage, "/Reset")
t = reset.AddTranslateOp()
t.Set(Gf.Vec3d(1), 0)
t.Set(Gf.Vec3d(2), 2)
reset.AddScaleOp().Set(Gf.Vec3f(1))
reset.GetXformOpOrderAttr().Set(["xformOp:translate", "!resetXformStack!", "xformOp:scale"])
inverse = UsdGeom.Xform.Define(stage, "/Inverse")
t = inverse.AddTranslateOp(opSuffix="pivot")
t.Set(Gf.Vec3d(1), 0)
t.Set(Gf.Vec3d(2), 2)
inverse.AddTranslateOp(opSuffix="pivot", isInverseOp=True)
spline_op = UsdGeom.Xform.Define(stage, "/Spline").AddRotateZOp(UsdGeom.XformOp.PrecisionDouble)
spline = Ts.Spline("double")
for time, value in ((0, 0), (2, 30)):
    spline.SetKnot(Ts.Knot(typeName="double", time=time, value=value, nextInterp=Ts.InterpLinear))
spline_op.GetAttr().SetSpline(spline)
records = []
for prim in stage.Traverse():
    xform = UsdGeom.Xformable(prim)
    records.append({"path": str(prim.GetPath()), "varying": xform.TransformMightBeTimeVarying(), "times": xform.GetTimeSamples(), "interval": xform.GetTimeSamplesInInterval(Gf.Interval(1, 5))})
outputs = {"scene.usda": stage.GetRootLayer().ExportToString().rstrip() + "\n", "oracle.json": json.dumps({"version": ".".join(map(str, Usd.GetVersion()[1:])), "records": records}, indent=2) + "\n"}
dest = Path(__file__).resolve().parents[1] / "fixtures" / "transform_queries"
dest.mkdir(parents=True, exist_ok=True)
for name, text in outputs.items():
    if "--check" in sys.argv:
        assert (dest / name).read_text() == text, name
    else:
        (dest / name).write_text(text)
