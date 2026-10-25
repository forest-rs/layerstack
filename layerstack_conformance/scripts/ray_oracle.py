# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin `GfRay` intersection tests: triangles, planes, ranges, oriented boxes,
spheres, cylinders and cones, from seeded random cases plus edge cases
(degenerate triangles, rays parallel to planes and slabs, rays starting
inside solids, solids behind the ray).

`--check` requires `fixtures/ray.json` to be what OpenUSD computes here,
numbers to within 1e-12 of their magnitude (at least 1), everything else
exactly. `tests/conformance/ray.rs` compares layerstack with the file.
"""
import json
import math
import pathlib
import random
import sys
from pxr import Gf, Usd

HERE = pathlib.Path(__file__).resolve().parents[1] / "fixtures"


def close(pinned, computed, where=""):
    if isinstance(pinned, dict) and isinstance(computed, dict):
        if pinned.keys() != computed.keys():
            return [(where, sorted(pinned), sorted(computed))]
        return [d for k in pinned for d in close(pinned[k], computed[k], f"{where}.{k}")]
    if isinstance(pinned, list) and isinstance(computed, list) and len(pinned) == len(computed):
        return [d for i, (p, c) in enumerate(zip(pinned, computed)) for d in close(p, c, f"{where}[{i}]")]
    if isinstance(pinned, float) and isinstance(computed, float):
        if abs(pinned - computed) <= 1e-12 * max(1.0, abs(pinned)):
            return []
    elif type(pinned) is type(computed) and pinned == computed:
        return []
    return [(where, pinned, computed)]


rng = random.Random(2608)


def vec(scale=4.0):
    return [round(rng.uniform(-scale, scale), 3) for _ in range(3)]


def matrix(m):
    return [list(row) for row in m]


def interval(result):
    return [result[1], result[2]] if result[0] else None


def rays():
    """Rays aimed near the origin, plus axis-aligned ones."""
    out = []
    for _ in range(24):
        origin = vec(6.0)
        target = vec(0.8)
        out.append((origin, [t - o for t, o in zip(target, origin)]))
    out += [([0.25, 0.25, 5.0], [0.0, 0.0, -1.0]),
            ([0.25, 0.25, 0.0], [1.0, 0.0, 0.0]),
            ([0.5, 0.5, 0.5], [0.0, 1.0, 0.0]),
            ([3.0, 0.5, 0.5], [1.0, 0.0, 0.0])]
    return out


def record():
    triangles, planes, ranges, bounds, spheres, cylinders, cones = [], [], [], [], [], [], []
    shapes = [[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
              [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 2.0, 0.0]],  # collinear
              [[0.5, 0.5, 0.5], [0.5, 0.5, 0.5], [0.5, 0.5, 0.5]],  # a point
              [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
              [[0.0, -1.0, -1.0], [0.0, 1.0, -1.0], [0.0, 0.0, 1.0]]]
    for i, (origin, direction) in enumerate(rays()):
        ray = Gf.Ray(Gf.Vec3d(*origin), Gf.Vec3d(*direction))
        tri = shapes[i % len(shapes)] if i % 3 else [vec(1.5), vec(1.5), vec(1.5)]
        hit, distance, bary, front = ray.Intersect(*(Gf.Vec3d(*p) for p in tri))
        triangles.append({"origin": origin, "direction": direction, "points": tri,
                          "hit": {"distance": distance, "barycentric": list(bary), "front_facing": front} if hit else None})
        for equation in ([0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 2.0, -2.0], [1.0, 0.0, 0.0, 0.0],
                         [0.0, 0.0, 0.0, 1.0], vec(1.0) + [rng.uniform(-1, 1)]):
            hit, distance, front = ray.Intersect(Gf.Plane(Gf.Vec4d(*equation)))
            planes.append({"origin": origin, "direction": direction, "equation": equation,
                           "hit": {"distance": distance, "front_facing": front} if hit else None})
        lo = vec(1.0)
        hi = [v + abs(rng.uniform(0.1, 2.0)) for v in lo]
        if i % 7 == 0:
            lo, hi = [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]
        box = Gf.Range3d(Gf.Vec3d(*lo), Gf.Vec3d(*hi))
        ranges.append({"origin": origin, "direction": direction, "min": lo, "max": hi,
                       "hit": interval(ray.Intersect(box))})
        m = Gf.Matrix4d().SetRotate(Gf.Rotation(Gf.Vec3d(*vec(1.0)), rng.uniform(0, 360)))
        m = m * Gf.Matrix4d().SetScale(Gf.Vec3d(1.0, rng.uniform(0.5, 2.0), 1.0))
        m.SetTranslateOnly(Gf.Vec3d(*vec(1.0)))
        if i % 9 == 4:
            m = Gf.Matrix4d(Gf.Vec4d(0.0, 1.0, 1.0, 1.0))  # degenerate: identity inverse
        bounds.append({"origin": origin, "direction": direction, "min": lo, "max": hi,
                       "matrix": matrix(m), "hit": interval(ray.Intersect(Gf.BBox3d(box, m)))})
        center, radius = vec(1.0), rng.uniform(0.2, 2.0)
        spheres.append({"origin": origin, "direction": direction, "center": center, "radius": radius,
                        "hit": interval(ray.Intersect(Gf.Vec3d(*center), radius))})
        axis = vec(1.0)
        cylinders.append({"origin": origin, "direction": direction, "base": center, "axis": axis,
                          "radius": radius, "hit": interval(ray.Intersect(Gf.Vec3d(*center), Gf.Vec3d(*axis), radius))})
        height = rng.uniform(0.5, 3.0)
        cones.append({"origin": origin, "direction": direction, "base": center, "axis": axis,
                      "radius": radius, "height": height,
                      "hit": interval(ray.Intersect(Gf.Vec3d(*center), Gf.Vec3d(*axis), radius, height))})
    return {"version": ".".join(map(str, Usd.GetVersion()[1:])), "triangles": triangles,
            "planes": planes, "ranges": ranges, "bounds": bounds, "spheres": spheres,
            "cylinders": cylinders, "cones": cones}


report = record()
output = "{\n" + ",\n".join(
    f'  "{key}": ' + (json.dumps(value) if key == "version" else
                      "[\n" + ",\n".join("    " + json.dumps(row, sort_keys=True) for row in value) + "\n  ]")
    for key, value in report.items()) + "\n}\n"
path = HERE / "ray.json"
if "--check" in sys.argv:
    differences = close(json.loads(path.read_text()), json.loads(output))
    for where, pinned, computed in differences:
        print(f"{where}: pinned {pinned!r}, OpenUSD computes {computed!r}", file=sys.stderr)
    assert not differences, "ray oracle differs from OpenUSD"
else:
    path.write_text(output)
