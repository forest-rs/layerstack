# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Pin `GfFrustum` picking and intersection: pick rays through window and
world points, narrowed frusta, point intersection,
`SetPerspective`/`GetPerspective`, and conformance of scaled, sheared and
reflected placements, for perspective and orthographic frusta.

Cases are seeded random plus edge cases: window corners, points on the
frustum boundary, picking regions reaching past the window, world points
behind the eye, triangles spanning the whole frustum, degenerate triangles.

`--check` requires `fixtures/frustum.json` to be what OpenUSD computes here,
numbers to within 1e-12 of their magnitude (at least 1), everything else
exactly. A frustum keeps its rotation as a `GfRotation` (axis and angle via
`acos`), so placed values carry the platform's trigonometry rounding;
`tests/conformance/frustum.rs` compares layerstack with the file to 1e-9.
"""
import json
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


def vec(scale):
    return [round(rng.uniform(-scale, scale), 3) for _ in range(3)]


def matrix(m):
    return [list(row) for row in m]


def ray(r):
    return {"origin": list(r.startPoint), "direction": list(r.direction)}


def window(f):
    w = f.GetWindow()
    return [w.GetMin()[0], w.GetMin()[1], w.GetMax()[0], w.GetMax()[1]]


def placements():
    rotated = Gf.Matrix4d().SetRotate(Gf.Rotation(Gf.Vec3d(0.3, 1.0, 0.2), 35.0))
    rotated.SetTranslateOnly(Gf.Vec3d(2.0, -1.0, 4.0))
    skewed = Gf.Matrix4d(Gf.Vec4d(-2.0, 1.0, 0.5, 1.0)) * rotated
    skewed[1][0] = 0.4
    return [Gf.Matrix4d(1.0), rotated, skewed]


def frusta():
    """Perspective and orthographic frusta at each placement. The unplaced
    windows have power-of-two proportions, so their planes and boundary
    distances round to exactly zero with or without fused multiply-adds."""
    out = []
    for i, xf in enumerate(placements()):
        for projection in (Gf.Frustum.Perspective, Gf.Frustum.Orthographic):
            if projection == Gf.Frustum.Perspective:
                win = [(-0.5, -0.25, 0.5, 0.25), (-0.3, -0.2, 0.5, 0.4), (-0.6, -0.3, 0.2, 0.5)][i]
                near_far = [(1.0, 100.0), (0.5, 50.0), (2.0, 30.0)][i]
            else:
                win = [(-2.0, -1.0, 2.0, 1.0), (-1.0, -2.0, 3.0, 1.0), (-3.0, -1.5, 1.0, 2.5)][i]
                near_far = [(1.0, 20.0), (0.5, 15.0), (-1.0, 12.0)][i]
            f = Gf.Frustum(xf, Gf.Range2d(Gf.Vec2d(*win[:2]), Gf.Vec2d(*win[2:])),
                           Gf.Range1d(*near_far), projection)
            out.append((xf, list(win), list(near_far), projection, f))
    return out


def record():
    rows = []
    for xf, win, near_far, projection, f in frusta():
        corners = [list(p) for p in f.ComputeCorners()]
        window_points = [[-1.0, -1.0], [1.0, 1.0], [-1.0, 1.0], [1.0, -1.0], [0.0, 0.0]]
        window_points += [[round(rng.uniform(-1, 1), 3), round(rng.uniform(-1, 1), 3)] for _ in range(5)]
        view_inverse = f.ComputeViewInverse()

        def eye_point(x, y, depth):
            """A world point at eye depth `depth` behind window point (x, y)."""
            scale = depth if projection == Gf.Frustum.Perspective else 1.0
            return list(view_inverse.Transform(Gf.Vec3d(x * scale, y * scale, -depth)))

        n, far = near_far
        mid = 0.5 * (n + far)
        inside = [eye_point(rng.uniform(win[0], win[2]), rng.uniform(win[1], win[3]), rng.uniform(n, far))
                  for _ in range(4)]
        outside = [eye_point(rng.uniform(-6, 6), rng.uniform(-6, 6), rng.uniform(n - 5, far + 5))
                   for _ in range(6)]
        # Placed boundary points classify by rounding that differs between
        # platforms (fused multiply-adds), in OpenUSD itself; only the
        # unplaced frusta pin them.
        boundary = corners + [eye_point(0.5 * (win[0] + win[2]), 0.5 * (win[1] + win[3]), n),
                              eye_point(win[0], 0.5 * (win[1] + win[3]), mid),
                              eye_point(0.5 * (win[0] + win[2]), win[3], mid)]
        behind = eye_point(0.0, 0.0, -1.0) if projection == Gf.Frustum.Perspective else eye_point(0.0, 0.0, n - 3)
        if xf != Gf.Matrix4d(1.0):
            # Just inside and just outside the placed corners instead.
            def nudge(v, k):
                return v + (1e-6 if k % 2 else -1e-6) * max(1.0, abs(v))
            boundary = [eye_point(nudge(win[2 * (k % 2)], k), nudge(win[1 + 2 * (k // 2 % 2)], k // 3),
                                  nudge((n, far)[k // 4 % 2], k // 5))
                        for k in range(len(boundary))]
        points = inside + outside + boundary + [behind]
        segments = [[points[i], points[j]] for i, j in ((0, 4), (4, 5), (5, 6), (6, 7), (8, 9),
                                                        (10, 11), (12, 13), (14, 15), (9, 21))]
        segments.append([eye_point(win[0] - 1, 0.0, mid), eye_point(win[2] + 1, 0.0, mid)])
        segments.append([eye_point(win[0] - 1, win[3] + 1, mid), eye_point(win[2] + 1, win[3] + 1, mid)])
        segments.append([points[10], points[11]])
        cx, cy = 0.5 * (win[0] + win[2]), 0.5 * (win[1] + win[3])
        big = 50.0
        triangles = [[points[0], points[4], points[5]], [points[4], points[5], points[6]],
                     [points[6], points[7], points[8]], [points[9], points[10], points[11]],
                     # spans the whole frustum between near and far
                     [eye_point(cx - big, cy - big, mid), eye_point(cx + big, cy - big, mid), eye_point(cx, cy + big, mid)],
                     # spans it, tilted through near and far
                     [eye_point(cx - big, cy - big, n - 0.5), eye_point(cx + big, cy - big, n - 0.5),
                      eye_point(cx, cy + big, far + 5)],
                     # degenerate: collinear across the frustum
                     [eye_point(win[0] - 1, 0.0, mid), eye_point(cx, 0.0, mid), eye_point(win[2] + 1, 0.0, mid)],
                     # degenerate: collinear outside
                     [eye_point(win[0] - 1, win[3] + 1, mid), eye_point(cx, win[3] + 1, mid),
                      eye_point(win[2] + 1, win[3] + 1, mid)],
                     [points[10], points[11], points[12]]]
        through = inside[:2] + outside[:3] + [corners[5]]
        regions = [([0.0, 0.0], [0.1, 0.1]), ([0.9, -0.9], [0.5, 0.5]), ([-1.0, 1.0], [0.0, 0.0]),
                   ([3.0, 0.0], [0.2, 0.2]), ([0.25, 0.5], [2.0, 2.0])]
        around = [(inside[0], [0.05, 0.05]), (inside[1], [0.3, 0.1]), (outside[0], [0.2, 0.2]),
                  (behind, [0.1, 0.1]), (list(f.GetPosition()), [0.1, 0.1])]
        row = {
            "camera_to_world": matrix(xf), "window": win, "clipping_range": near_far,
            "projection": "perspective" if projection == Gf.Frustum.Perspective else "orthographic",
            "view": matrix(f.ComputeViewMatrix()), "projection_matrix": matrix(f.ComputeProjectionMatrix()),
            "corners": corners,
            "picks": [{"window_point": p, **ray(f.ComputePickRay(Gf.Vec2d(*p)))} for p in window_points],
            "picks_through": [{"point": p, **ray(f.ComputePickRay(Gf.Vec3d(*p)))} for p in through],
            "narrowed": [{"window_point": p, "size": s,
                          "window": window(f.ComputeNarrowedFrustum(Gf.Vec2d(*p), Gf.Vec2d(*s)))}
                         for p, s in regions],
            "narrowed_around": [{"point": p, "size": s,
                                 "window": window(f.ComputeNarrowedFrustum(Gf.Vec3d(*p), Gf.Vec2d(*s)))
                                 if f.ComputeViewMatrix().Transform(Gf.Vec3d(*p))[2] < 0 else None}
                                for p, s in around],
            "points": [{"point": p, "inside": f.Intersects(Gf.Vec3d(*p))} for p in points],
        }
        if projection == Gf.Frustum.Perspective:
            row["perspective"] = {axis: list(f.GetPerspective(axis == "vertical"))
                                  for axis in ("vertical", "horizontal")}
        else:
            row["perspective"] = None
        rows.append(row)
    perspectives = []
    for xf in placements():
        for fov, vertical, aspect, near_far in ((45.0, True, 1.6, (0.1, 1000.0)), (90.0, False, 0.75, (1.0, 10.0)),
                                                (30.0, True, 0.0, (2.0, 20.0))):
            f = Gf.Frustum()
            f.SetPositionAndRotationFromMatrix(xf)
            f.SetPerspective(fov, vertical, aspect, *near_far)
            perspectives.append({"camera_to_world": matrix(xf), "field_of_view": fov, "vertical": vertical,
                                 "aspect_ratio": aspect, "clipping_range": list(near_far), "window": window(f),
                                 "view": matrix(f.ComputeViewMatrix())})
    return {"version": ".".join(map(str, Usd.GetVersion()[1:])), "frusta": rows, "perspectives": perspectives}


report = record()
output = "{\n" + ",\n".join(
    f'  "{key}": ' + (json.dumps(value) if key == "version" else
                      "[\n" + ",\n".join("    " + json.dumps(row, sort_keys=True) for row in value) + "\n  ]")
    for key, value in report.items()) + "\n}\n"
path = HERE / "frustum.json"
if "--check" in sys.argv:
    differences = close(json.loads(path.read_text()), json.loads(output))
    for where, pinned, computed in differences:
        print(f"{where}: pinned {pinned!r}, OpenUSD computes {computed!r}", file=sys.stderr)
    assert not differences, "frustum oracle differs from OpenUSD"
else:
    path.write_text(output)
