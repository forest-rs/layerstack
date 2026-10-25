# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin SetFromCamera world/parent conversion and lens fields with OpenUSD 26.11."""
import json
import pathlib
import sys
from pxr import Usd, UsdGeom

fixtures = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
stage = Usd.Stage.Open(str(fixtures / "camera_writeback.usda"))
source = UsdGeom.Camera(stage.GetPrimAtPath("/SourceParent/Source"))
destination = UsdGeom.Camera(stage.GetPrimAtPath("/DestinationParent/Destination"))
destination.SetFromCamera(source.GetCamera(), Usd.TimeCode(2))
camera = destination.GetCamera(Usd.TimeCode(2))
record = {
    "version": ".".join(map(str, Usd.GetVersion()[1:])),
    "world": [list(row) for row in camera.transform],
    "local": [list(row) for row in destination.GetLocalTransformation(Usd.TimeCode(2))],
    "aperture": [camera.horizontalAperture, camera.verticalAperture],
    "offset": [camera.horizontalApertureOffset, camera.verticalApertureOffset],
    "clipping": [camera.clippingRange.min, camera.clippingRange.max],
    "planes": [list(row) for row in camera.clippingPlanes],
    "focal": camera.focalLength, "f_stop": camera.fStop, "focus": camera.focusDistance,
    "order": list(destination.GetXformOpOrderAttr().Get()),
}
text = json.dumps(record, indent=2, sort_keys=True) + "\n"
output = fixtures / "camera_writeback.json"
if "--check" in sys.argv:
    assert output.read_text() == text, "camera writeback fixture differs from OpenUSD"
else:
    output.write_text(text)
