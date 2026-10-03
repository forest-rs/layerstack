# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record callback-backed rigid-body mass evaluation with OpenUSD 26.8."""
import argparse
import json
import pathlib
from pxr import Usd, UsdPhysics, Gf
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "mass"
stage = Usd.Stage.Open(str(here / "scene.usda"))
rows = []
def quat(q):
    return list(q.GetImaginary()) + [q.GetReal()]
for prim in stage.GetPseudoRoot().GetChildren():
    if not prim.HasAPI(UsdPhysics.RigidBodyAPI):
        continue
    inputs = []
    def collider(shape):
        info = UsdPhysics.RigidBodyAPI.MassInformation()
        info.volume = shape.GetAttribute("test:volume").Get()
        info.inertia = Gf.Matrix3f(shape.GetAttribute("test:inertia").Get())
        info.centerOfMass = shape.GetAttribute("test:center").Get()
        info.localPos = shape.GetAttribute("test:position").Get()
        info.localRot = shape.GetAttribute("test:orientation").Get()
        inputs.append({"path": str(shape.GetPath()), "volume": info.volume,
                       "inertia": [list(row) for row in info.inertia],
                       "center": list(info.centerOfMass), "position": list(info.localPos),
                       "orientation": quat(info.localRot)})
        return info
    mass, inertia, com, axes = UsdPhysics.RigidBodyAPI(prim).ComputeMassProperties(collider)
    # C++ leaves the axes uninitialized without geometry or authored axes.
    has_axes = inputs or prim.GetAttribute("physics:principalAxes").Get() != Gf.Quatf(0)
    rows.append({"path": str(prim.GetPath()), "mass": mass, "inertia": list(inertia),
                 "center": list(com), "axes": quat(axes) if has_axes else None,
                 "inputs": inputs})
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows}
output = here / "oracle.json"
if args.check:
    assert json.loads(output.read_text()) == result
else:
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
