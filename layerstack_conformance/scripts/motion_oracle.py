# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record inherited motion settings using OpenUSD's public computations."""
import json
import pathlib
import sys
from pxr import Usd, UsdGeom

HERE = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "motion"

def record():
    stage = Usd.Stage.Open(str(HERE / "scene.usda"))
    results = []
    for interpolation in ("linear", "held"):
        stage.SetInterpolationType(Usd.InterpolationTypeLinear if interpolation == "linear" else Usd.InterpolationTypeHeld)
        for prim in stage.Traverse():
            for time in (None, 0, 1, 2, 3, 4):
                api = UsdGeom.MotionAPI(prim)
                code = Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)
                results.append({"path": str(prim.GetPath()), "time": time,
                                "interpolation": interpolation,
                                "blur": api.ComputeMotionBlurScale(code),
                                "count": api.ComputeNonlinearSampleCount(code),
                                "velocity": api.ComputeVelocityScale(code)})
    return {"version": ".".join(map(str, Usd.GetVersion()[1:])), "results": results}

output = json.dumps(record(), indent=2, sort_keys=True) + "\n"
path = HERE / "oracle.json"
if "--check" in sys.argv:
    assert path.read_text() == output, "motion oracle differs from OpenUSD"
else:
    path.write_text(output)
