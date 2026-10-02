# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin primary camera frusta, including scale/shear/reflection conformance."""
import json
import pathlib
import sys
from pxr import Gf, Usd, UsdGeom

HERE = pathlib.Path(__file__).resolve().parents[1] / "fixtures"

def matrix(m):
    return [list(row) for row in m]

def record():
    stage = Usd.Stage.Open(str(HERE / "camera.usda"))
    rows = []
    for interpolation in ("linear", "held"):
        stage.SetInterpolationType(Usd.InterpolationTypeLinear if interpolation == "linear" else Usd.InterpolationTypeHeld)
        for prim in stage.Traverse():
            if not prim.IsA(UsdGeom.Camera):
                continue
            for time in (None, 0, 1, 2):
                tc = Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)
                camera = UsdGeom.Camera(prim).GetCamera(tc)
                frustum = camera.frustum
                inverse = frustum.ComputeViewInverse()
                l, b = frustum.window.GetMin()
                r, t = frustum.window.GetMax()
                n, f = frustum.nearFar.GetMin(), frustum.nearFar.GetMax()
                depth = (n + f) / 2
                scale = depth if camera.projection == Gf.Camera.Perspective else 1
                center = Gf.Vec3d((l+r)*scale/2, (b+t)*scale/2, -depth)
                outside = Gf.Vec3d((r+2*(r-l))*scale, center[1], -depth)
                before = Gf.Vec3d(0, 0, -n+1)
                after = Gf.Vec3d(0, 0, -f-1)
                points = [inverse.Transform(p) for p in (center, outside, before, after)]
                boxes = []
                for point in (center, outside):
                    translation = Gf.Matrix4d().SetTranslate(point)
                    rotation = Gf.Matrix4d().SetRotate(Gf.Rotation(Gf.Vec3d(0,1,0),17))
                    m = rotation * translation * inverse
                    extent = Gf.Range3d(Gf.Vec3d(-0.05), Gf.Vec3d(0.05))
                    box = Gf.BBox3d(extent, m)
                    boxes.append({"min": list(extent.GetMin()), "max": list(extent.GetMax()),
                                  "matrix": matrix(m), "intersects": frustum.Intersects(box)})
                schema = UsdGeom.Camera(prim)
                rows.append({"path": str(prim.GetPath()), "time": time, "interpolation": interpolation,
                             "transform": matrix(camera.transform), "view": matrix(frustum.ComputeViewMatrix()),
                             "projection": matrix(frustum.ComputeProjectionMatrix()), "corners": [list(p) for p in frustum.ComputeCorners()],
                             "window": [l,b,r,t], "clipping_range": [n,f],
                             "points": [{"point": list(p), "inside": frustum.Intersects(p)} for p in points], "boxes": boxes,
                             "aperture": [camera.horizontalAperture, camera.verticalAperture],
                             "aperture_offset": [camera.horizontalApertureOffset, camera.verticalApertureOffset],
                             "focal_length": camera.focalLength, "f_stop": camera.fStop, "focus_distance": camera.focusDistance,
                             "clipping_planes": [list(p) for p in camera.clippingPlanes],
                             "shutter": [schema.GetShutterOpenAttr().Get(tc),schema.GetShutterCloseAttr().Get(tc)]})
    return {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows}

report = record()
output = '{\n  "version": ' + json.dumps(report["version"]) + ',\n  "rows": [\n'
output += ",\n".join("    " + json.dumps(row, sort_keys=True) for row in report["rows"])
output += "\n  ]\n}\n"
path = HERE / "camera.json"
if "--check" in sys.argv:
    assert path.read_text() == output, "camera oracle differs from OpenUSD"
else:
    path.write_text(output)
