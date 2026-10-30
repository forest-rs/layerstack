# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's skinning kernels with interleaved influences.

Usage: skel_interleaved_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

Calls the `TfSpan<const GfVec2f> influences` overloads of
`UsdSkel.SkinPoints` / `SkinPointsLBS`, `SkinNormals` / `SkinNormalsLBS`
and `SkinTransform` / `SkinTransformLBS`, with both skinning methods, on
fixed joint matrices, points and normals. The cases cover fractional and
negative-fractional indices (C++ truncates each to an int), a unit
single-joint transform, and refusals: an influence count that does not
match the points, an index past the last joint and a negative index.
Python answers the bind transform for a refused transform, which is how
this script tells one. Normal kernels take inverse-transpose matrices, which this script derives
from the point matrices as the skinning query does. Python does not reach
`SkinFaceVaryingNormals`.

`tests/conformance/skel_interleaved.rs` replays every case through the
skinning functions with `InterleavedInfluences`, comparing within 1e-5.
"""
import argparse
import json
import math
import pathlib

from pxr import Gf, Usd, UsdSkel, Vt

OUT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "skel_interleaved" / "oracle.json"


def matrix(rotate_axis, angle, translate, scale):
    m = Gf.Matrix4d().SetScale(Gf.Vec3d(*scale))
    m = m * Gf.Matrix4d().SetRotate(Gf.Rotation(Gf.Vec3d(*rotate_axis), angle))
    m.SetTranslateOnly(Gf.Vec3d(*translate))
    return m


BIND = matrix((0, 0, 1), 10, (0.5, 0, 0), (1, 1, 1))
JOINTS = [
    matrix((0, 1, 0), 30, (1, 0, 0), (1, 1, 1)),
    matrix((1, 0, 0), 45, (0, 2, 0), (1, 1, 1)),
    matrix((0, 0, 1), 90, (0, 0, 3), (2, 2, 2)),
]
POINTS = [(0, 0, 0), (1, 2, 3), (-1, 0.5, 2)]
NORMALS = [(0, 0, 1), (0, 1, 0), (0.6, 0.8, 0)]
CASES = [
    {"kernel": "points", "influences": [[0, 0.5], [1.9, 0.5], [2, 1], [0, 0], [1, 0.25], [2, 0.75]],
     "per_point": 2},
    {"kernel": "points", "influences": [[-0.5, 1], [1, 1], [2.99, 0.5]], "per_point": 1},
    {"kernel": "points", "influences": [[0, 1], [1, 1]], "per_point": 1},
    {"kernel": "points", "influences": [[0, 1], [3.2, 1], [1, 1]], "per_point": 1},
    {"kernel": "points", "influences": [[0, 1], [-1, 1], [1, 1]], "per_point": 1},
    {"kernel": "normals", "influences": [[0, 0.5], [1.9, 0.5], [2, 1], [0, 0], [1, 0.25], [2, 0.75]],
     "per_point": 2},
    {"kernel": "normals", "influences": [[0, 1], [1, 1]], "per_point": 1},
    {"kernel": "transform", "influences": [[0, 0.25], [2.5, 0.75]]},
    {"kernel": "transform", "influences": [[1.5, 1]]},
    {"kernel": "transform", "influences": [[0, 0.5], [7, 0.5]]},
]


def rows(m, n):
    return [[m[i][j] for j in range(n)] for i in range(n)]


def normal_matrix(m):
    return m.ExtractRotationMatrix().GetInverse().GetTranspose()


def run(case, method):
    influences = Vt.Vec2fArray([Gf.Vec2f(*pair) for pair in case["influences"]])
    lbs = method == "classicLinear"
    result = {"method": method}
    result.update(case)
    if case["kernel"] == "points":
        points = Vt.Vec3fArray(POINTS)
        joints = Vt.Matrix4dArray(JOINTS)
        if lbs:
            ok = UsdSkel.SkinPointsLBS(BIND, joints, influences, case["per_point"], points)
        else:
            ok = UsdSkel.SkinPoints(method, BIND, joints, influences, case["per_point"], points)
        result["output"] = [list(p) for p in points]
    elif case["kernel"] == "normals":
        normals = Vt.Vec3fArray(NORMALS)
        joints = Vt.Matrix3dArray([normal_matrix(m) for m in JOINTS])
        bind = normal_matrix(BIND)
        if lbs:
            ok = UsdSkel.SkinNormalsLBS(bind, joints, influences, case["per_point"], normals)
        else:
            ok = UsdSkel.SkinNormals(method, bind, joints, influences, case["per_point"], normals)
        result["output"] = [list(n) for n in normals]
    else:
        joints = Vt.Matrix4dArray(JOINTS)
        if lbs:
            xform = UsdSkel.SkinTransformLBS(BIND, joints, influences)
        else:
            xform = UsdSkel.SkinTransform(method, BIND, joints, influences)
        # The wrapper answers the bind transform when C++ fails; no case's
        # skinned transform is the bind transform itself.
        ok = xform != BIND
        result["output"] = rows(xform, 4) if ok else None
    result["ok"] = bool(ok)
    if not result["ok"]:
        result["output"] = None
    return result


def agree(a, b):
    if isinstance(a, dict):
        return isinstance(b, dict) and a.keys() == b.keys() and all(agree(a[k], b[k]) for k in a)
    if isinstance(a, list):
        return isinstance(b, list) and len(a) == len(b) and all(agree(x, y) for x, y in zip(a, b))
    if isinstance(a, float) or isinstance(b, float):
        return math.isclose(a, b, rel_tol=1e-6, abs_tol=1e-6)
    return a == b


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "bind": rows(BIND, 4),
        "joints": [rows(m, 4) for m in JOINTS],
        "points": [list(map(float, p)) for p in POINTS],
        "normals": [list(map(float, n)) for n in NORMALS],
        "cases": [run(case, method) for case in CASES
                  for method in ("classicLinear", "dualQuaternion")],
    }
    if args.check:
        assert agree(json.loads(OUT.read_text()), result), "oracle.json is out of date"
    else:
        OUT.write_text(json.dumps(result, indent=1) + "\n")


main()
