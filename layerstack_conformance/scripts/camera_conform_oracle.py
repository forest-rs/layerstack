# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record `CameraUtilConformWindow` on every camera of `fixtures/camera.usda`.

`cameraUtil` is an imaging library the Python wheel leaves out, so the call
is made by `camera_conform_oracle.cpp`, built against the wheel's
`libusd_ms` (and the `libpython` it needs) with
`pxr/imaging/cameraUtil/conformWindow.cpp` (unchanged since 2019) compiled
beside it, OpenUSD's headers and a `pxr/pxr.h` generated for 26.11 on the
include path; run this script with `--binary PATH`. This
script reads each camera with `UsdGeomCamera::GetCamera` (usd-core), hands
its `GfCamera` fields to the tool for every policy and target aspect, and
pins the conformed apertures, window and view and projection matrices in
`fixtures/camera_conform.json`. `--check` requires the file to be what the
tool computes, numbers to within 1e-12 of the largest magnitude in their
field. `tests/conformance/camera.rs` compares layerstack with the file.
"""
import argparse
import json
import math
import pathlib
import subprocess
import sys

from pxr import Gf, Usd, UsdGeom


def leaves(value):
    """The leaves of a nested list."""
    if isinstance(value, list):
        for item in value:
            yield from leaves(item)
    else:
        yield value


def close(pinned, computed, where="", scale=None):
    """The places where `computed` differs from `pinned`: numbers by more
    than 1e-12 of the largest magnitude (at least 1) of their list."""
    if isinstance(pinned, dict) and isinstance(computed, dict):
        if pinned.keys() != computed.keys():
            return [(where, sorted(pinned), sorted(computed))]
        return [d for key in pinned for d in close(pinned[key], computed[key], f"{where}.{key}")]
    if isinstance(pinned, list) and isinstance(computed, list):
        if len(pinned) != len(computed):
            return [(where, pinned, computed)]
        numbers = list(leaves(pinned))
        if scale is None and all(isinstance(v, (int, float)) for v in numbers):
            scale = max([1.0] + [abs(v) for v in numbers])
        return [d for i, (p, c) in enumerate(zip(pinned, computed))
                for d in close(p, c, f"{where}[{i}]", scale)]
    if isinstance(pinned, float) or isinstance(computed, float):
        bound = 1e-12 * (scale if scale is not None else max(1.0, abs(pinned)))
        if math.isfinite(pinned) and abs(pinned - computed) <= bound:
            return []
    elif pinned == computed:
        return []
    return [(where, pinned, computed)]

parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
parser.add_argument("--binary", required=True, type=pathlib.Path)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()

HERE = pathlib.Path(__file__).resolve().parents[1] / "fixtures"
POLICIES = ["matchVertically", "matchHorizontally", "fit", "crop", "dontConform"]
ASPECTS = [0.5, 1.0, 1.5, 16 / 9, 2.0]

stage = Usd.Stage.Open(str(HERE / "camera.usda"))
cases = []
lines = []
for path, time in [("/Defaults", None), ("/Rig/Perspective", 0), ("/Rig/Perspective", 2),
                   ("/Rig/Orthographic", None), ("/Rig/Mirrored", None), ("/ZeroFocal", None)]:
    tc = Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)
    camera = UsdGeom.Camera(stage.GetPrimAtPath(path)).GetCamera(tc)
    projection = 1 if camera.projection == Gf.Camera.Orthographic else 0
    fields = [v for row in camera.transform for v in row]
    fields += [camera.horizontalAperture, camera.verticalAperture,
               camera.horizontalApertureOffset, camera.verticalApertureOffset,
               camera.focalLength, camera.clippingRange.min, camera.clippingRange.max]
    for policy, name in enumerate(POLICIES):
        for aspect in ASPECTS:
            cases.append({"path": path, "time": time, "policy": name, "aspect": aspect})
            lines.append(f"{projection} " + " ".join(repr(float(v)) for v in fields) + f" {policy} {aspect!r}")
output = subprocess.run([str(args.binary.resolve())], input="\n".join(lines) + "\n",
                        capture_output=True, text=True, check=True).stdout
rows = [dict(case, **json.loads(line)) for case, line in zip(cases, output.splitlines())]
assert len(rows) == len(cases)
report = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows}
text = '{\n  "version": ' + json.dumps(report["version"]) + ',\n  "rows": [\n'
text += ",\n".join("    " + json.dumps(row, sort_keys=True) for row in rows)
text += "\n  ]\n}\n"
target = HERE / "camera_conform.json"
if args.check:
    differences = close(json.loads(target.read_text()), json.loads(text))
    for where, pinned, computed in differences:
        print(f"{where}: pinned {pinned!r}, OpenUSD computes {computed!r}", file=sys.stderr)
    assert not differences, "camera conform oracle differs from OpenUSD"
else:
    target.write_text(text)
