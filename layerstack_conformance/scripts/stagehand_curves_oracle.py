# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record what OpenUSD answers about the curves and points of a draw list.

Usage: stagehand_curves_oracle.py [--check] [OUT_DIR]

Authors `fixtures/stagehand_curves/curves.usda` (`upAxis = "Y"`) through
the `UsdGeom` schema API, exports it, reopens the exported file and records
in `oracle.json`, at the default time:

- under `basisCurves`, for one `BasisCurves` prim under `/Curves` per
  `curveVertexCounts` set of every type (`linear`, `cubic`), basis
  (`bezier`, `bspline`, `catmullRom`) and wrap (`nonperiodic`, `periodic`,
  `pinned`), and for prims leaving `type`, `basis` or `wrap` unauthored:
  `type`, `basis` and `wrap` as `Get` returns them (fallbacks included),
  `curveVertexCounts`, `ComputeSegmentCounts`, `ComputeUniformDataSize`,
  `ComputeVaryingDataSize` and `ComputeVertexDataSize`, and
  `ComputeInterpolationForSize(n)` for every `n` from 0 to two past the
  vertex size ("" when no interpolation fits). Its `widths` and
  `GetWidthsInterpolation` when authored, `UsdGeomCurves::ComputeExtent`
  of its points and widths (an empty widths array when unauthored) and
  `UsdGeomBoundable::ComputeExtentFromPlugins`;
- under `points`, for each `Points` prim under `/Points`: its points,
  widths, `GetWidthsInterpolation` and ids, `UsdGeomPoints::ComputeExtent`
  of its points and widths (an empty widths array when unauthored) and
  `ComputeExtentFromPlugins`;
- under `nurbsCurves`, the rational quarter circle `/Nurbs/quarter`: its
  `order`, `knots`, `ranges`, `pointWeights`, points and widths read back,
  and `ComputeExtentFromPlugins`;
- under `hermiteCurves`, `/Hermite/a`: its curve vertex counts, points,
  tangents and widths read back, and `ComputeExtentFromPlugins`.

An extent is `null` when OpenUSD computes none.

`_ComputeVaryingDataSize` (`pxr/usd/usdGeom/basisCurves.cpp`) sizes a
cubic nonperiodic or pinned curve as
`std::max<size_t>((count - 4) / vstep, 0) + 2`, where `count - 4` is an
`int` converted to `size_t`: a curve of fewer than four vertices wraps
around and the batch's varying size is meaningless (the comment beside it
intends 2 per such curve). For those prims `varying` is `null` and
`interpolationForSize` keeps only the sizes OpenUSD decides before
comparing with the varying size (`constant` for 1, `uniform` for the curve
count). Segment counts use C++'s truncating integer division.

Points are exact binary fractions and widths multiples of 1/4 (but the
NURBS curve's 0.1 and its weight `sqrt(0.5)`), so every value is the same
on every platform. `--check` rewrites everything into a temporary
directory and fails unless it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import math
import os
import tempfile

from pxr import Gf, Sdf, Usd, UsdGeom, Vt

assert Usd.GetVersion() == (0, 26, 11)

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "stagehand_curves"))
FILES = ["curves.usda", "oracle.json"]
DEFAULT = Usd.TimeCode.Default()

TYPES = ["linear", "cubic"]
BASES = ["bezier", "bspline", "catmullRom"]
WRAPS = ["nonperiodic", "periodic", "pinned"]
INTERPOLATIONS = ["constant", "uniform", "varying", "vertex"]

# The examples of `basisCurves.h`'s "deriving per curve segment and varying
# primvar data counts" table, keyed by (type, basis, wrap); `None` matches
# any basis.
HEADER_EXAMPLES = {
    ("linear", None, "nonperiodic"): [2, 3, 2, 5],
    ("cubic", "bezier", "nonperiodic"): [4, 7, 10, 4, 7],
    ("cubic", "bspline", "nonperiodic"): [5, 4, 6, 7],
    ("cubic", "bezier", "periodic"): [6, 9, 6],
    ("linear", None, "periodic"): [3, 7],
}


def count_sets(curve_type, basis, wrap):
    """The `curveVertexCounts` sets of one combination: typical counts,
    counts below the basis minimum or not divisible by its vstep, and the
    header's example when it has one."""
    if curve_type == "linear":
        sets = [[2, 3, 5], [1, 4]]
    elif basis == "bezier":
        sets = [[4, 7, 10], [2, 3, 5, 8]]
    else:
        sets = [[5, 4, 6, 7], [2, 3, 5]]
    example = (HEADER_EXAMPLES.get((curve_type, basis, wrap))
               or HEADER_EXAMPLES.get((curve_type, None, wrap)))
    if example and example not in sets:
        sets.append(example)
    return sets


def point(i):
    return Gf.Vec3f(i, (i * i) % 5, 0.5 * i)


def width(i):
    return 0.25 * (1 + i % 4)


def cases():
    """(name, type, basis, wrap, counts, widths interpolation); a `None`
    type, basis or wrap is left unauthored, a widths interpolation of `None`
    authors no widths and "" authors widths with no interpolation."""
    found = []
    index = 0
    for curve_type in TYPES:
        for basis in BASES:
            for wrap in WRAPS:
                for at, counts in enumerate(count_sets(curve_type, basis, wrap)):
                    # The first set of each combination authors widths, its
                    # interpolation cycling through the four.
                    interpolation = INTERPOLATIONS[index % 4] if at == 0 else None
                    name = f"{curve_type}_{basis}_{wrap}_{'abc'[at]}"
                    found.append((name, curve_type, basis, wrap, counts, interpolation))
                index += 1
    found += [
        ("unauthored_type", None, "bezier", "nonperiodic", [4, 7], ""),
        ("unauthored_basis", "cubic", None, "nonperiodic", [4, 7, 10], "vertex"),
        ("unauthored_wrap", "cubic", "bspline", None, [5, 4], "varying"),
    ]
    return found


def wraps_varying(curves):
    """Whether C++'s varying size wraps around: a cubic nonperiodic or
    pinned curve of fewer than four vertices."""
    counts = curves.GetCurveVertexCountsAttr().Get(DEFAULT)
    return (curves.GetTypeAttr().Get(DEFAULT) == "cubic"
            and curves.GetWrapAttr().Get(DEFAULT) != "periodic"
            and any(count < 4 for count in counts))


def author(path):
    stage = Usd.Stage.CreateInMemory()
    UsdGeom.SetStageUpAxis(stage, UsdGeom.Tokens.y)
    UsdGeom.Scope.Define(stage, "/Curves")
    for name, curve_type, basis, wrap, counts, interpolation in cases():
        curves = UsdGeom.BasisCurves.Define(stage, f"/Curves/{name}")
        if curve_type is not None:
            curves.CreateTypeAttr(curve_type)
        if basis is not None:
            curves.CreateBasisAttr(basis)
        if wrap is not None:
            curves.CreateWrapAttr(wrap)
        curves.CreateCurveVertexCountsAttr(Vt.IntArray(counts))
        curves.CreatePointsAttr(Vt.Vec3fArray([point(i) for i in range(sum(counts))]))
        if interpolation is None:
            continue
        size = {
            "": sum(counts),
            "constant": 1,
            "uniform": len(counts),
            "varying": curves.ComputeVaryingDataSize(DEFAULT),
            "vertex": sum(counts),
        }[interpolation]
        assert not (interpolation == "varying" and wraps_varying(curves))
        curves.CreateWidthsAttr(Vt.FloatArray([width(i) for i in range(size)]))
        if interpolation:
            curves.SetWidthsInterpolation(interpolation)

    UsdGeom.Scope.Define(stage, "/Points")
    sparks = UsdGeom.Points.Define(stage, "/Points/sparks")
    sparks.CreatePointsAttr(Vt.Vec3fArray([point(i) for i in range(5)]))
    sparks.CreateWidthsAttr(Vt.FloatArray([width(i) for i in range(5)]))
    sparks.SetWidthsInterpolation(UsdGeom.Tokens.vertex)
    sparks.CreateIdsAttr(Vt.Int64Array([10, 20, 30, 40, 2**40]))
    bare = UsdGeom.Points.Define(stage, "/Points/bare")
    bare.CreatePointsAttr(Vt.Vec3fArray([point(i) for i in range(1, 4)]))
    constant = UsdGeom.Points.Define(stage, "/Points/constant")
    constant.CreatePointsAttr(Vt.Vec3fArray([point(i) for i in range(3)]))
    constant.CreateWidthsAttr(Vt.FloatArray([0.5]))
    constant.SetWidthsInterpolation(UsdGeom.Tokens.constant)

    UsdGeom.Scope.Define(stage, "/Nurbs")
    quarter = UsdGeom.NurbsCurves.Define(stage, "/Nurbs/quarter")
    quarter.CreateCurveVertexCountsAttr(Vt.IntArray([3]))
    quarter.CreateOrderAttr(Vt.IntArray([3]))
    quarter.CreateKnotsAttr(Vt.DoubleArray([0, 0, 0, 1, 1, 1]))
    quarter.CreateRangesAttr(Vt.Vec2dArray([Gf.Vec2d(0, 1)]))
    quarter.CreatePointsAttr(Vt.Vec3fArray([Gf.Vec3f(1, 0, 0), Gf.Vec3f(1, 1, 0),
                                            Gf.Vec3f(0, 1, 0)]))
    quarter.CreatePointWeightsAttr(Vt.DoubleArray([1, math.sqrt(0.5), 1]))
    quarter.CreateWidthsAttr(Vt.FloatArray([0.1]))
    quarter.SetWidthsInterpolation(UsdGeom.Tokens.constant)

    UsdGeom.Scope.Define(stage, "/Hermite")
    hermite = UsdGeom.HermiteCurves.Define(stage, "/Hermite/a")
    hermite.CreateCurveVertexCountsAttr(Vt.IntArray([2, 3]))
    hermite.CreatePointsAttr(Vt.Vec3fArray([point(i) for i in range(5)]))
    hermite.CreateTangentsAttr(Vt.Vec3fArray([Gf.Vec3f(1, 0, 0), Gf.Vec3f(0, 1, 0),
                                              Gf.Vec3f(0.5, 0, -0.5), Gf.Vec3f(-1, 2, 0),
                                              Gf.Vec3f(0, 0, 4)]))
    hermite.CreateWidthsAttr(Vt.FloatArray([0.5]))
    hermite.SetWidthsInterpolation(UsdGeom.Tokens.constant)

    with open(path, "w") as f:
        f.write(stage.GetRootLayer().ExportToString())


def floats(values):
    return [float(v) for v in values]


def vectors(values):
    return None if values is None else [floats(v) for v in values]


def extent(value):
    """An extent as two corners, or `None` when OpenUSD computes none."""
    return None if not value else vectors(value)


def plugin_extent(prim):
    return extent(UsdGeom.Boundable.ComputeExtentFromPlugins(UsdGeom.Boundable(prim), DEFAULT))


def widths_of(schema):
    widths = schema.GetWidthsAttr().Get(DEFAULT)
    return None if widths is None else floats(widths)


def basis_curves_row(prim):
    curves = UsdGeom.BasisCurves(prim)
    counts = list(curves.GetCurveVertexCountsAttr().Get(DEFAULT))
    uniform = curves.ComputeUniformDataSize(DEFAULT)
    vertex = curves.ComputeVertexDataSize(DEFAULT)
    wrapped = wraps_varying(curves)
    sizes = {}
    for n in range(vertex + 3):
        # With a wrapped varying size only the sizes decided before it is
        # compared are meaningful.
        if wrapped and n not in (1, uniform):
            continue
        sizes[str(n)] = str(curves.ComputeInterpolationForSize(n, DEFAULT))
    points = curves.GetPointsAttr().Get(DEFAULT)
    widths = curves.GetWidthsAttr().Get(DEFAULT)
    return {
        "path": str(prim.GetPath()),
        "type": str(curves.GetTypeAttr().Get(DEFAULT)),
        "basis": str(curves.GetBasisAttr().Get(DEFAULT)),
        "wrap": str(curves.GetWrapAttr().Get(DEFAULT)),
        "authored": {name: curves.GetPrim().GetAttribute(name).HasAuthoredValue()
                     for name in ["type", "basis", "wrap"]},
        "curveVertexCounts": counts,
        "segmentCounts": list(curves.ComputeSegmentCounts(DEFAULT)),
        "uniform": uniform,
        "varying": None if wrapped else curves.ComputeVaryingDataSize(DEFAULT),
        "vertex": vertex,
        "interpolationForSize": sizes,
        "widths": None if widths is None else floats(widths),
        "widthsInterpolation": None if widths is None else str(curves.GetWidthsInterpolation()),
        "extent": extent(UsdGeom.Curves.ComputeExtent(
            points, widths if widths is not None else Vt.FloatArray())),
        "extentFromPlugins": plugin_extent(prim),
    }


def points_row(prim):
    schema = UsdGeom.Points(prim)
    points = schema.GetPointsAttr().Get(DEFAULT)
    widths = schema.GetWidthsAttr().Get(DEFAULT)
    ids = schema.GetIdsAttr().Get(DEFAULT)
    return {
        "path": str(prim.GetPath()),
        "points": vectors(points),
        "widths": None if widths is None else floats(widths),
        "widthsInterpolation": None if widths is None else str(schema.GetWidthsInterpolation()),
        "ids": None if ids is None else [int(i) for i in ids],
        "extent": extent(UsdGeom.Points.ComputeExtent(
            points, widths if widths is not None else Vt.FloatArray())),
        "extentFromPlugins": plugin_extent(prim),
    }


def nurbs_row(prim):
    schema = UsdGeom.NurbsCurves(prim)
    return {
        "path": str(prim.GetPath()),
        "curveVertexCounts": list(schema.GetCurveVertexCountsAttr().Get(DEFAULT)),
        "order": list(schema.GetOrderAttr().Get(DEFAULT)),
        "knots": floats(schema.GetKnotsAttr().Get(DEFAULT)),
        "ranges": vectors(schema.GetRangesAttr().Get(DEFAULT)),
        "pointWeights": floats(schema.GetPointWeightsAttr().Get(DEFAULT)),
        "points": vectors(schema.GetPointsAttr().Get(DEFAULT)),
        "widths": widths_of(schema),
        "widthsInterpolation": str(schema.GetWidthsInterpolation()),
        "extentFromPlugins": plugin_extent(prim),
    }


def hermite_row(prim):
    schema = UsdGeom.HermiteCurves(prim)
    return {
        "path": str(prim.GetPath()),
        "curveVertexCounts": list(schema.GetCurveVertexCountsAttr().Get(DEFAULT)),
        "points": vectors(schema.GetPointsAttr().Get(DEFAULT)),
        "tangents": vectors(schema.GetTangentsAttr().Get(DEFAULT)),
        "widths": widths_of(schema),
        "widthsInterpolation": str(schema.GetWidthsInterpolation()),
        "extentFromPlugins": plugin_extent(prim),
    }


def write(out):
    os.makedirs(out, exist_ok=True)
    path = os.path.join(out, "curves.usda")
    author(path)
    layer = Sdf.Layer.Find(path)
    if layer:
        layer.Reload(force=True)
    stage = Usd.Stage.Open(path)
    rows = {"basisCurves": [], "points": [], "nurbsCurves": [], "hermiteCurves": []}
    for prim in stage.Traverse():
        if prim.IsA(UsdGeom.BasisCurves):
            rows["basisCurves"].append(basis_curves_row(prim))
        elif prim.IsA(UsdGeom.Points):
            rows["points"].append(points_row(prim))
        elif prim.IsA(UsdGeom.NurbsCurves):
            rows["nurbsCurves"].append(nurbs_row(prim))
        elif prim.IsA(UsdGeom.HermiteCurves):
            rows["hermiteCurves"].append(hermite_row(prim))
    for kind in rows.values():
        kind.sort(key=lambda row: row["path"])
    result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), **rows}
    with open(os.path.join(out, "oracle.json"), "w") as f:
        f.write(json.dumps(result, indent=1, sort_keys=True) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        for name in FILES:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
