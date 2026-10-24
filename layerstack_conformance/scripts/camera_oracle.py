# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin primary camera frusta, including scale/shear/reflection conformance.

`--check` requires `fixtures/camera.json` to be what OpenUSD computes here,
numbers to within 1e-12 of the largest magnitude in their field (`close`),
everything else exactly. A frustum keeps its rotation as a `GfRotation`, an
axis and an angle taken with `acos` from the conformed transform and turned
back into a matrix with `GfSinCos` (`GfFrustum::SetPositionAndRotationFromMatrix`,
`pxr/base/gf/frustum.cpp`; `pxr/base/gf/rotation.cpp`), so its view matrix,
corners and the points and boxes placed through it carry the rounding of
the platform's trigonometry: usd-core 26.08 on x86-64 Linux and on arm64
macOS differ by up to 64 ulps, 5.3e-15 in a field of magnitude 16.
`tests/conformance/camera.rs` compares layerstack with the file to within 2e-10.
"""
import json
import math
import pathlib
import sys
from pxr import Gf, Usd, UsdGeom

HERE = pathlib.Path(__file__).resolve().parents[1] / "fixtures"


def leaves(value):
    """The numbers of a number or a nested list of numbers."""
    if isinstance(value, list):
        return [leaf for item in value for leaf in leaves(item)]
    return [value]


def is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def close(pinned, computed, where="", scale=None):
    """The places where `computed` differs from `pinned`: numbers by more
    than 1e-12 of `scale`, the largest magnitude (at least 1) of the field
    they belong to; anything else at all."""
    if isinstance(pinned, dict) and isinstance(computed, dict):
        if pinned.keys() != computed.keys():
            return [(where, sorted(pinned), sorted(computed))]
        return [d for key in pinned for d in close(pinned[key], computed[key], f"{where}.{key}")]
    if isinstance(pinned, list) and isinstance(computed, list):
        if len(pinned) != len(computed):
            return [(where, pinned, computed)]
        if scale is None and all(is_number(v) for v in leaves(pinned) + leaves(computed)):
            scale = max([1.0] + [abs(v) for v in leaves(pinned)])
        return [d for i, (p, c) in enumerate(zip(pinned, computed))
                for d in close(p, c, f"{where}[{i}]", scale)]
    if is_number(pinned) and is_number(computed):
        bound = 1e-12 * (scale if scale is not None else max(1.0, abs(pinned)))
        if math.isfinite(pinned) and abs(pinned - computed) <= bound:
            return []
    elif type(pinned) is type(computed) and pinned == computed:
        return []
    return [(where, pinned, computed)]

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
    differences = close(json.loads(path.read_text()), json.loads(output))
    for where, pinned, computed in differences:
        print(f"{where}: pinned {pinned!r}, OpenUSD computes {computed!r}", file=sys.stderr)
    assert not differences, "camera oracle differs from OpenUSD"
else:
    path.write_text(output)
