# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record a camera's exposure inputs and `ComputeLinearExposureScale`.

`--check` requires `fixtures/camera_exposure.json` to be exactly what
usd-core computes. `tests/conformance/camera.rs` compares layerstack's
`ComputedCamera::exposure` with it.
"""
import argparse
import json
import pathlib

from pxr import Usd, UsdGeom

parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(here / "camera_exposure.usda"))
rows = []
for path in ["/Defaults", "/Metered", "/Animated", "/Blocked"]:
    camera = UsdGeom.Camera(stage.GetPrimAtPath(path))
    for time in [None, 0, 5, 10]:
        tc = Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)
        rows.append({
            "path": path,
            "time": time,
            "exposure": camera.GetExposureAttr().Get(tc),
            "f_stop": camera.GetExposureFStopAttr().Get(tc),
            "iso": camera.GetExposureIsoAttr().Get(tc),
            "responsivity": camera.GetExposureResponsivityAttr().Get(tc),
            "exposure_time": camera.GetExposureTimeAttr().Get(tc),
            "linear_exposure_scale": camera.ComputeLinearExposureScale(tc),
        })
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows}
output = here / "camera_exposure.json"
if args.check:
    assert json.loads(output.read_text()) == result, "camera exposure oracle differs from OpenUSD"
else:
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
