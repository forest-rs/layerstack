# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's point-instance transforms and extents.

Usage: point_instancer_transforms_oracle.py [--check]

Needs OpenUSD through Python `pxr` (`pip install usd-core==26.8`).

Opens `fixtures/point_instancer_transforms/scene.usda` and records, for
each case in `CASES`:

- `transforms`: `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`
  at (time, base time) for every `ProtoXformInclusion` and
  `MaskApplication`, each matrix as rows;
- `extent`: `ComputeExtentAtTime` at (time, base time).

For each batch in `BATCHES` (several times against one base time, in
order) it records `ComputeInstanceTransformsAtTimes` for every option pair
and `ComputeExtentAtTimes`. For each entry of `TRANSFORMED` it records
`UsdGeomBoundable::ComputeExtentFromPlugins` with a transform, which calls
`ComputeExtentAtTime(time, time, transform)`: the Python bindings expose
the transform overload only that way.

OpenUSD's Python bindings return an empty array when a computation fails,
so a refused computation is recorded as `[]`; one that raises a coding
error (`Tf.ErrorException`) is recorded as `null`. Writes `oracle.json` beside
the scene; `tests/point_instancer_transforms.rs` compares
`layerstack_schemas`' results. With `--check` it writes nothing and fails
unless the committed `oracle.json` agrees, floats to within 1e-9 of their
scale.
"""
import json
import os
import sys

from pxr import Gf, Tf, Usd, UsdGeom

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "point_instancer_transforms"))

# Each case: instancer, time, base time (None is the default time).
CASES = [
    ["/World/Motion", 1.0, 0.0],
    ["/World/Motion", 2.5, 0.0],
    ["/World/Motion", -1.0, 0.0],
    ["/World/Motion", 3.0, 2.0],
    ["/World/Motion", 4.0, 4.0],
    ["/World/Motion", 5.0, 4.0],
    ["/World/Motion", None, None],
    ["/World/Interpolated", 1.0, 0.0],
    ["/World/Interpolated", 2.5, 0.0],
    ["/World/Interpolated", 3.0, 0.0],
    ["/World/Interpolated", 0.5, 3.0],
    ["/World/Interpolated", None, None],
    ["/World/Dangling", None, None],
    ["/World/Dangling", 1.0, 1.0],
    ["/World/Empty", None, None],
]

# Each batch: instancer, times, base time.
BATCHES = [
    ["/World/Motion", [0.0, 1.0, 2.0, 6.0, 1.0], 0.0],
    ["/World/Motion", [3.0, 2.0], 2.0],
    ["/World/Interpolated", [1.0, 2.5, 3.0, 3.5], 0.0],
    ["/World/Interpolated", [3.0, 1.0], 0.0],
    ["/World/Dangling", [None, None], None],
    ["/World/Empty", [None], None],
    ["/World/Motion", [], 0.0],
]

# Each entry: instancer, time. The transform is `MATRIX`.
TRANSFORMED = [
    ["/World/Motion", 1.0],
    ["/World/Interpolated", 2.5],
    ["/World/Dangling", None],
]

MATRIX = Gf.Matrix4d().SetRotate(Gf.Rotation(Gf.Vec3d(1, 2, 3), 40)) * Gf.Matrix4d().SetTranslate(Gf.Vec3d(1, -2, 0.5))

OPTIONS = [
    ("include_mask", UsdGeom.PointInstancer.IncludeProtoXform, UsdGeom.PointInstancer.ApplyMask),
    ("include_all", UsdGeom.PointInstancer.IncludeProtoXform, UsdGeom.PointInstancer.IgnoreMask),
    ("exclude_mask", UsdGeom.PointInstancer.ExcludeProtoXform, UsdGeom.PointInstancer.ApplyMask),
    ("exclude_all", UsdGeom.PointInstancer.ExcludeProtoXform, UsdGeom.PointInstancer.IgnoreMask),
]


def code(time):
    return Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)


def matrices(values):
    return [[[m[r][c] for c in range(4)] for r in range(4)] for m in values]


def extent(values):
    return [[float(x) for x in v] for v in values]


def attempt(compute):
    """`compute()`, or `None` when OpenUSD raises a coding error."""
    try:
        return compute()
    except Tf.ErrorException:
        return None


def record(scene):
    stage = Usd.Stage.Open(scene)

    def instancer(path):
        return UsdGeom.PointInstancer(stage.GetPrimAtPath(path))

    cases = []
    for path, time, base in CASES:
        prim = instancer(path)
        cases.append({
            "case": [path, time, base],
            "transforms": {
                label: attempt(lambda: matrices(prim.ComputeInstanceTransformsAtTime(code(time), code(base), include, mask)))
                for label, include, mask in OPTIONS
            },
            "extent": attempt(lambda: extent(prim.ComputeExtentAtTime(code(time), code(base)))),
        })
    batches = []
    for path, times, base in BATCHES:
        prim = instancer(path)
        codes = [code(t) for t in times]
        batches.append({
            "batch": [path, times, base],
            "transforms": {
                label: attempt(lambda: [matrices(sample) for sample in prim.ComputeInstanceTransformsAtTimes(codes, code(base), include, mask)])
                for label, include, mask in OPTIONS
            },
            "extents": attempt(lambda: [extent(e) for e in prim.ComputeExtentAtTimes(codes, code(base))]),
        })
    transformed = []
    for path, time in TRANSFORMED:
        prim = instancer(path)
        result = attempt(lambda: extent(UsdGeom.Boundable.ComputeExtentFromPlugins(prim, code(time), MATRIX) or []))
        transformed.append({"entry": [path, time], "extent": result})
    return {
        "matrix": [[MATRIX[r][c] for c in range(4)] for r in range(4)],
        "cases": cases,
        "batches": batches,
        "transformed": transformed,
    }


def close(a, b):
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(close(a[k], b[k]) for k in a)
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(close(x, y) for x, y in zip(a, b))
    if isinstance(a, float) and isinstance(b, float):
        return abs(a - b) <= 1e-9 * max(1.0, abs(b))
    return a == b


def main():
    check = sys.argv[1:] == ["--check"]
    result = record(os.path.join(FIXTURES, "scene.usda"))
    _, minor, patch = Usd.GetVersion()
    result["openusd_version"] = f"{minor}.{patch}"
    out = os.path.join(FIXTURES, "oracle.json")
    if check:
        with open(out) as f:
            if not close(json.load(f), json.loads(json.dumps(result))):
                sys.exit("oracle.json is not what OpenUSD computes; rerun point_instancer_transforms_oracle.py")
        return
    with open(out, "w") as f:
        json.dump(result, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
