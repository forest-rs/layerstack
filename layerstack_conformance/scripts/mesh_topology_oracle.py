# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's mesh topology reads and validation, call by call.

Usage: mesh_topology_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

Two parts:

- `validate`: `UsdGeom.Mesh.ValidateTopology(faceVertexIndices,
  faceVertexCounts, numPoints)` on hand-written cases, one call each,
  recording whether it accepts them and the reason it gives.
- `meshes`: for each mesh of `fixtures/mesh_topology/scene.usda`, at the
  default time and at times 1 and 2, the values of `faceVertexCounts`,
  `faceVertexIndices`, `holeIndices`, `orientation` and
  `subdivisionScheme` (`Get`, fallbacks included), the number of `points`,
  and `ValidateTopology` on them.

`tests/conformance/mesh_topology.rs` replays both through
`validate_mesh_topology` and `Mesh::topology` and compares everything.
"""
import argparse
import json
import pathlib

from pxr import Usd, UsdGeom, Vt

FIXTURES = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "mesh_topology"
OUT = FIXTURES / "oracle.json"

# (label, faceVertexIndices, faceVertexCounts, numPoints)
CASES = [
    ("empty", [], [], 0),
    ("one triangle", [0, 1, 2], [3], 3),
    ("quad and triangle", [0, 1, 2, 3, 0, 2, 4], [4, 3], 5),
    ("index sum too large", [0, 1, 2], [4], 3),
    ("index sum too small", [0, 1, 2, 0], [3], 3),
    ("index at the point count", [0, 1, 3], [3], 3),
    ("negative index", [0, -1, 2], [3], 3),
    ("degenerate faces are counted", [0, 1, 0], [2, 1], 2),
    ("zero-vertex face", [0, 1, 2], [0, 3], 3),
    ("indices without points", [0, 1, 2], [3], 0),
    # OpenUSD sums the counts as `int` and has no check of their sign, so
    # counts that sum to the index count pass whatever their values.
    ("negative counts summing to the index count", [0, 1, 2], [4, -1], 3),
    ("negative counts summing to zero", [], [3, -3], 0),
    ("negative count alone", [], [-3], 0),
]


def validate(indices, counts, points):
    valid, reason = UsdGeom.Mesh.ValidateTopology(
        Vt.IntArray(indices), Vt.IntArray(counts), points)
    return {"valid": bool(valid), "reason": reason or None}


def ints(attr, time):
    value = attr.Get(time)
    return None if value is None else [int(v) for v in value]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    cases = []
    for label, indices, counts, points in CASES:
        cases.append({
            "case": label,
            "face_vertex_indices": indices,
            "face_vertex_counts": counts,
            "points": points,
            **validate(indices, counts, points),
        })
    stage = Usd.Stage.Open(str(FIXTURES / "scene.usda"))
    meshes = []
    for prim in stage.Traverse():
        mesh = UsdGeom.Mesh(prim)
        for label, time in [("default", Usd.TimeCode.Default()), ("1", 1.0), ("2", 2.0)]:
            counts = ints(mesh.GetFaceVertexCountsAttr(), time)
            indices = ints(mesh.GetFaceVertexIndicesAttr(), time)
            holes = ints(mesh.GetHoleIndicesAttr(), time)
            points = mesh.GetPointsAttr().Get(time)
            count = 0 if points is None else len(points)
            meshes.append({
                "mesh": str(prim.GetPath()),
                "time": label,
                "face_vertex_counts": counts,
                "face_vertex_indices": indices,
                "hole_indices": holes,
                "orientation": str(mesh.GetOrientationAttr().Get(time)),
                "subdivision_scheme": str(mesh.GetSubdivisionSchemeAttr().Get(time)),
                "points": count,
                **validate(indices or [], counts or [], count),
            })
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "validate": cases,
        "meshes": meshes,
    }
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.check:
        assert OUT.read_text() == text, "oracle.json is out of date"
    else:
        OUT.write_text(text)


main()
