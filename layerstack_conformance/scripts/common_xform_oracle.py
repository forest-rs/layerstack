# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Common TRS/pivot authoring through OpenUSD's public API."""
import json
import sys
from pathlib import Path
from pxr import Gf, Usd, UsdGeom

stage = Usd.Stage.CreateInMemory()
records = []
cases = [(name, name, "empty") for name in ("XYZ", "XZY", "YXZ", "YZX", "ZXY", "ZYX")]
cases += [("Partial", "XYZ", "scale"), ("Reset", "XYZ", "reset"), ("Suffix", "XYZ", "suffix"), ("Mismatch", "ZYX", "suffix"), ("Unpaired", "XYZ", "pivot"), ("WrongOrder", "XYZ", "wrong_order")]
for name, order, setup in cases:
    obj = UsdGeom.Xform.Define(stage, "/" + name)
    if setup == "scale":
        obj.AddScaleOp().Set(Gf.Vec3f(1))
    elif setup == "reset":
        obj.SetResetXformStack(True)
    elif setup == "suffix":
        obj.AddRotateXYZOp(opSuffix="spin").Set(Gf.Vec3f(0))
    elif setup == "pivot":
        obj.AddTranslateOp(opSuffix="pivot").Set(Gf.Vec3d(0))
    elif setup == "wrong_order":
        obj.AddScaleOp()
        obj.AddTranslateOp()
    api = UsdGeom.XformCommonAPI(obj)
    rotation_order = getattr(UsdGeom.XformCommonAPI, "RotationOrder" + order)
    try:
        accepted = api.SetXformVectors(Gf.Vec3d(1, 2, 3), Gf.Vec3f(10, 20, 30), Gf.Vec3f(2, 3, 4), Gf.Vec3f(.5, 1, 1.5), rotation_order, Usd.TimeCode.Default())
    except Exception:
        if name != "Mismatch":
            raise
        accepted = False
    assert accepted == (setup not in ("pivot", "wrong_order") and name != "Mismatch"), name
    record = {"name": name, "rotation_order": order, "setup": setup, "accepted": accepted}
    if accepted:
        assert api.SetXformVectors(Gf.Vec3d(-1, 4, 2), Gf.Vec3f(30, 10, 20), Gf.Vec3f(3, 2, 1), Gf.Vec3f(1, .5, 0), rotation_order, Usd.TimeCode(2))
        record["order"] = list(obj.GetXformOpOrderAttr().Get())
        record["matrices"] = [[[float(v) for v in row] for row in obj.GetLocalTransformation(time)] for time in (Usd.TimeCode.Default(), Usd.TimeCode(2))]
    records.append(record)
text = json.dumps({"version": ".".join(map(str, Usd.GetVersion()[1:])), "records": records}, indent=2) + "\n"
dest = Path(__file__).resolve().parents[1] / "fixtures" / "common_xform.json"
if "--check" in sys.argv:
    expected = json.loads(dest.read_text())
    actual = json.loads(text)
    for left, right in zip(expected["records"], actual["records"]):
        for key in ("name", "rotation_order", "setup", "accepted", "order"):
            assert left.get(key) == right.get(key), (left["name"], key)
        for a, b in zip(left.get("matrices", []), right.get("matrices", [])):
            assert all(abs(x-y) <= 1e-12 * max(1, abs(y)) for ar, br in zip(a,b) for x,y in zip(ar,br)), left["name"]
    assert expected["version"] == actual["version"] and len(expected["records"]) == len(actual["records"])
else:
    dest.write_text(text)
