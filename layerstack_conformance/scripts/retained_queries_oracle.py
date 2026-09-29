# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Public OpenUSD notices and computed results through an edit sequence."""
import json
import sys
from pathlib import Path
from pxr import Gf, Tf, Usd, UsdGeom, UsdShade

fixtures = Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(fixtures / "retained_queries.usda"))
notices = []
def notice(n, sender):
    notices.append({
        "resynced": [str(p) for p in n.GetResyncedPaths()],
        "info": {str(p): sorted(n.GetChangedFields(p)) for p in n.GetChangedInfoOnlyPaths()},
    })
registration = Tf.Notice.Register(Usd.Notice.ObjectsChanged, notice, stage)
records = []
translate = stage.GetAttributeAtPath("/World.xformOp:translate")
size = stage.GetAttributeAtPath("/World/Shape.size")
port = UsdShade.Input(stage.GetAttributeAtPath("/Material/Surface.inputs:roughness"))
def record(name, time):
    xf = UsdGeom.XformCache(time)
    bounds = UsdGeom.BBoxCache(time, [UsdGeom.Tokens.default_])
    matrix = xf.GetLocalToWorldTransform(stage.GetPrimAtPath("/World/Shape"))
    box = bounds.ComputeWorldBound(stage.GetPrimAtPath("/World")).ComputeAlignedRange()
    providers = port.GetValueProducingAttributes()
    records.append({"name": name, "matrix": [list(row) for row in matrix],
                    "min": list(box.GetMin()), "max": list(box.GetMax()),
                    "providers": [str(p.GetPath()) for p in providers],
                    "values": [p.Get(time) for p in providers], "notices": list(notices)})
    notices.clear()
record("initial", Usd.TimeCode.Default())
stage.GetAttributeAtPath("/World.inputs:unrelated").Set(2.0)
record("unrelated", Usd.TimeCode.Default())
translate.Set(Gf.Vec3d(4, 0, 0))
record("translated", Usd.TimeCode.Default())
translate.Set(Gf.Vec3d(0, 0, 0))
record("undo_translate", Usd.TimeCode.Default())
record("time_1", Usd.TimeCode(1))
size.Set(4.0)
record("size_4", Usd.TimeCode(1))
size.Set(2.0)
record("undo_size", Usd.TimeCode(1))
port.GetAttr().SetConnections(["/Material.inputs:b"])
record("provider_b", Usd.TimeCode.Default())
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "records": records}
dest = fixtures / "retained_queries.json"
text = json.dumps(result, indent=2) + "\n"
if "--check" in sys.argv:
    assert dest.read_text() == text, "retained query oracle differs"
else:
    dest.write_text(text)
print(f"{len(records)} edit/time records agree with committed oracle" if "--check" in sys.argv else f"wrote {len(records)} records")
