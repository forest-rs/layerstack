# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Writes a character-sized skinning scene and times OpenUSD's bake of it.

Usage: skel_bake_cpp_timing.py SCENE.usda [POINTS JOINTS FRAMES [RUNS]]

Needs OpenUSD through Python `pxr` (`pip install usd-core==26.8`). Writes
SCENE.usda (default 50000 points, 100 joints, 200 frames): a `SkelRoot`
with a skeleton of JOINTS joints (each joint's parent is joint
`(i - 1) // 2`), an animation rotating every joint about X at every frame,
and a grid mesh of POINTS points with vertex normals and four joint
influences per point. It then bakes a fresh copy of the scene RUNS times
(default 5) with `UsdSkel.BakeSkinning(root)` into the root layer, and
prints the median and minimum wall-clock time. Opening the stage is not
timed.

`examples/skel_bake.rs` loads the same file and times `SkinningBake`.
OpenUSD parallelizes the bake with its work dispatcher; set
`PXR_WORK_THREAD_LIMIT=1` to time one thread.
"""
import math
import statistics
import sys
import time

from pxr import Gf, Sdf, Usd, UsdGeom, UsdSkel, Vt


def joint_names(count):
    names = ["J0"]
    for i in range(1, count):
        names.append(f"{names[(i - 1) // 2]}/J{i}")
    return names


def write(path, points, joints, frames):
    stage = Usd.Stage.CreateNew(path)
    stage.SetStartTimeCode(0)
    stage.SetEndTimeCode(frames - 1)
    stage.SetTimeCodesPerSecond(24)
    stage.SetFramesPerSecond(24)
    root = UsdSkel.Root.Define(stage, "/Character")
    UsdSkel.BindingAPI.Apply(root.GetPrim()).CreateSkeletonRel().SetTargets(["/Character/Skel"])
    skel = UsdSkel.Skeleton.Define(stage, "/Character/Skel")
    UsdSkel.BindingAPI.Apply(skel.GetPrim()).CreateAnimationSourceRel().SetTargets(
        ["/Character/Skel/Anim"])
    names = joint_names(joints)
    skel.CreateJointsAttr(names)
    depth = [0] * joints
    for i in range(1, joints):
        depth[i] = depth[(i - 1) // 2] + 1
    local = [Gf.Matrix4d(1).SetTranslate(Gf.Vec3d(0, 0 if i == 0 else 0.5, 0)) for i in range(joints)]
    bind = [Gf.Matrix4d(1).SetTranslate(Gf.Vec3d(0, 0.5 * depth[i], 0)) for i in range(joints)]
    skel.CreateRestTransformsAttr(Vt.Matrix4dArray(local))
    skel.CreateBindTransformsAttr(Vt.Matrix4dArray(bind))
    anim = UsdSkel.Animation.Define(stage, "/Character/Skel/Anim")
    anim.CreateJointsAttr(names)
    anim.CreateTranslationsAttr(Vt.Vec3fArray([Gf.Vec3f(0, 0 if i == 0 else 0.5, 0) for i in range(joints)]))
    anim.CreateScalesAttr(Vt.Vec3hArray([Gf.Vec3h(1, 1, 1)] * joints))
    rotations = anim.CreateRotationsAttr()
    for frame in range(frames):
        quats = []
        for i in range(joints):
            half = math.radians(10.0 * math.sin(frame * 0.1 + i)) / 2.0
            quats.append(Gf.Quatf(math.cos(half), math.sin(half), 0, 0))
        rotations.Set(Vt.QuatfArray(quats), frame)
    mesh = UsdGeom.Mesh.Define(stage, "/Character/Body")
    columns = 250
    rows = points // columns
    count = rows * columns
    mesh.CreatePointsAttr(Vt.Vec3fArray(
        [Gf.Vec3f((p % columns) * 0.01, (p // columns) * 0.02, 0) for p in range(count)]))
    mesh.CreateNormalsAttr(Vt.Vec3fArray([Gf.Vec3f(0, 0, 1)] * count))
    mesh.SetNormalsInterpolation(UsdGeom.Tokens.vertex)
    counts = []
    corners = []
    for r in range(rows - 1):
        for c in range(columns - 1):
            p = r * columns + c
            counts.append(4)
            corners.extend([p, p + 1, p + 1 + columns, p + columns])
    mesh.CreateFaceVertexCountsAttr(Vt.IntArray(counts))
    mesh.CreateFaceVertexIndicesAttr(Vt.IntArray(corners))
    binding = UsdSkel.BindingAPI.Apply(mesh.GetPrim())
    indices = []
    weights = []
    for p in range(count):
        k = p % 97
        indices.extend([(k + j) % joints for j in range(4)])
        weights.extend([0.4, 0.3, 0.2, 0.1])
    binding.CreateJointIndicesPrimvar(False, 4).Set(Vt.IntArray(indices))
    binding.CreateJointWeightsPrimvar(False, 4).Set(Vt.FloatArray(weights))
    stage.GetRootLayer().Save()


def main(argv):
    if len(argv) < 2:
        raise SystemExit(__doc__)
    path = argv[1]
    points, joints, frames = (int(a) for a in argv[2:5]) if len(argv) >= 5 else (50000, 100, 200)
    runs = int(argv[5]) if len(argv) >= 6 else 5
    write(path, points, joints, frames)
    timings = []
    for _ in range(runs):
        layer = Sdf.Layer.FindOrOpen(path)
        layer.Reload(force=True)
        stage = Usd.Stage.Open(layer)
        start = time.perf_counter()
        assert UsdSkel.BakeSkinning(UsdSkel.Root(stage.GetPrimAtPath("/Character")))
        timings.append(time.perf_counter() - start)
        del stage
        layer.Reload(force=True)
    print(f"openusd {'.'.join(map(str, Usd.GetVersion()))} bake points={points} joints={joints} "
          f"frames={frames} median_ms={statistics.median(timings) * 1000:.1f} "
          f"min_ms={min(timings) * 1000:.1f}")


main(sys.argv)
