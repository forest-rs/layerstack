# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Times OpenUSD's point-instancer computations on the `point_instancer`
bench's fixture.

Usage: point_instancer_cpp_timing.py [COUNT ...]

Needs OpenUSD through Python `pxr` (`pip install usd-core==26.8`). Builds,
for each COUNT (default 100000 and 1000000), the scene
`benches/point_instancer.rs` builds: two prototypes, `/World/Still` with
defaults and `/World/Moving` with samples at 0 and 24 and velocities,
accelerations and angular velocities, one in 100 IDs invisible and one in
1000 inactive, and `/World/Culled`, `/World/Still` with one in ten
inactive. It then prints the median of repeated wall-clock timings of
`ComputeMaskAtTime`, `ComputeInstanceTransformsAtTime`, a three-sample
`ComputeInstanceTransformsAtTimes` and `ComputeExtentAtTime`, the same
operations the bench names `compute_mask`, `compute_mask_tenth_inactive`,
`transforms_collected`,
`motion_batch_3_samples` and `compute_extent`.

OpenUSD parallelizes the per-instance loops with its work dispatcher; set
`PXR_WORK_THREAD_LIMIT=1` to time one thread. Python call overhead is a few
microseconds per call and is included.
"""
import math
import statistics
import sys
import time

from pxr import Gf, Sdf, Usd, UsdGeom, Vt


def about_z(degrees):
    half = math.radians(degrees) / 2.0
    return Gf.Quatf(math.cos(half), 0.0, 0.0, math.sin(half))


def fixture(n):
    stage = Usd.Stage.CreateInMemory()
    UsdGeom.Xform.Define(stage, "/World")
    stage.DefinePrim("/World/Protos")
    UsdGeom.Cube.Define(stage, "/World/Protos/Box").CreateSizeAttr(0.5)
    UsdGeom.Sphere.Define(stage, "/World/Protos/Ball").CreateRadiusAttr(0.25)
    indices = Vt.IntArray([i % 2 for i in range(n)])
    ids = Vt.Int64Array([i * 3 for i in range(n)])
    invisible = Vt.Int64Array([i * 3 for i in range(0, n, 100)])
    inactive = [i * 3 + 3 for i in range(0, n, 1000)]
    culled = [i * 3 + 3 for i in range(0, n, 10)]

    def positions(lift):
        return Vt.Vec3fArray([Gf.Vec3f(i % 1000, i // 1000, lift) for i in range(n)])

    rotations = Vt.QuatfArray([about_z(i % 360) for i in range(n)])
    scales = Vt.Vec3fArray([Gf.Vec3f(1.0 + (i % 5) * 0.1, 1.0, 1.0) for i in range(n)])
    velocities = Vt.Vec3fArray([Gf.Vec3f(0.0, 0.0, i % 7) for i in range(n)])
    spins = Vt.Vec3fArray([Gf.Vec3f(0.0, 0.0, i % 90) for i in range(n)])
    for path, still, hidden in [
        ("/World/Still", True, inactive),
        ("/World/Moving", False, inactive),
        ("/World/Culled", True, culled),
    ]:
        instancer = UsdGeom.PointInstancer.Define(stage, path)
        instancer.GetPrim().SetMetadata("inactiveIds", Sdf.Int64ListOp.CreateExplicit(hidden))
        instancer.CreatePrototypesRel().SetTargets(["/World/Protos/Box", "/World/Protos/Ball"])
        instancer.CreateProtoIndicesAttr(indices)
        instancer.CreateIdsAttr(ids)
        instancer.CreateInvisibleIdsAttr(invisible)
        instancer.CreateScalesAttr(scales)
        if still:
            instancer.CreatePositionsAttr(positions(0.0))
            instancer.CreateOrientationsfAttr(rotations)
        else:
            for t, lift in [(0.0, 0.0), (24.0, 1.0)]:
                instancer.CreatePositionsAttr().Set(positions(lift), t)
                instancer.CreateOrientationsfAttr().Set(rotations, t)
                instancer.CreateVelocitiesAttr().Set(velocities, t)
                instancer.CreateAccelerationsAttr().Set(velocities, t)
                instancer.CreateAngularVelocitiesAttr().Set(spins, t)
    return stage


def median_ms(compute, repeats=10):
    compute()
    samples = []
    for _ in range(repeats):
        start = time.perf_counter()
        compute()
        samples.append((time.perf_counter() - start) * 1000.0)
    return statistics.median(samples)


def main():
    counts = [int(a) for a in sys.argv[1:]] or [100000, 1000000]
    default = Usd.TimeCode.Default()
    shutter = [Usd.TimeCode(0.25), Usd.TimeCode(0.5), Usd.TimeCode(0.75)]
    for n in counts:
        stage = fixture(n)
        still = UsdGeom.PointInstancer(stage.GetPrimAtPath("/World/Still"))
        moving = UsdGeom.PointInstancer(stage.GetPrimAtPath("/World/Moving"))
        culled = UsdGeom.PointInstancer(stage.GetPrimAtPath("/World/Culled"))
        rows = [
            ("compute_mask", lambda: still.ComputeMaskAtTime(default)),
            ("compute_mask_tenth_inactive", lambda: culled.ComputeMaskAtTime(default)),
            ("transforms_collected", lambda: still.ComputeInstanceTransformsAtTime(default, default)),
            ("motion_batch_3_samples", lambda: moving.ComputeInstanceTransformsAtTimes(shutter, Usd.TimeCode(0.0))),
            ("compute_extent", lambda: still.ComputeExtentAtTime(default, default)),
        ]
        for name, compute in rows:
            print(f"{name}/{n}: {median_ms(compute):.3f} ms")


if __name__ == "__main__":
    main()
