# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record schema behavior with the matching OpenUSD wheel; run from repo root."""
from pathlib import Path
import json
from pxr import Usd, UsdGeom

fixtures = Path("layerstack_conformance/fixtures")
version = ".".join(map(str, Usd.GetVersion()))
assert version == "0.26.8", version
stage = Usd.Stage.Open(str(fixtures / "point_instancer_behavior.usda"))
instancer = UsdGeom.PointInstancer(stage.GetPrimAtPath("/World/Instances"))
result = {"version": version, "mask": list(instancer.ComputeMaskAtTime(Usd.TimeCode.Default()))}
for label, time, base, include, mask in [
    ("default", Usd.TimeCode.Default(), Usd.TimeCode.Default(), True, True),
    ("allDefault", Usd.TimeCode.Default(), Usd.TimeCode.Default(), True, False),
    ("motion", 2, 0, True, False), ("motionBase2", 2, 2, True, False),
    ("noPrototype", 2, 0, False, False),
]:
    matrices = instancer.ComputeInstanceTransformsAtTime(time, base,
        UsdGeom.PointInstancer.IncludeProtoXform if include else UsdGeom.PointInstancer.ExcludeProtoXform,
        UsdGeom.PointInstancer.ApplyMask if mask else UsdGeom.PointInstancer.IgnoreMask)
    result[label] = [[list(row) for row in matrix] for matrix in matrices]
for label, path in [("implicitMask", "/World/ImplicitIds")]:
    result[label] = list(UsdGeom.PointInstancer(stage.GetPrimAtPath(path)).ComputeMaskAtTime(Usd.TimeCode.Default()))
interpolated = UsdGeom.PointInstancer(stage.GetPrimAtPath("/World/Interpolated"))
result["interpolated"] = [[list(row) for row in matrix] for matrix in interpolated.ComputeInstanceTransformsAtTime(2, 0)]
for label, path, time, base in [
    ("compacted", "/World/Compacted", Usd.TimeCode.Default(), Usd.TimeCode.Default()),
    ("misaligned", "/World/Misaligned", 2, 0),
]:
    prim = UsdGeom.PointInstancer(stage.GetPrimAtPath(path))
    result[label] = [[list(row) for row in matrix] for matrix in prim.ComputeInstanceTransformsAtTime(time, base)]
for label, time in [("boundsDefault", Usd.TimeCode.Default()), ("boundsAt2", 2)]:
    cache = UsdGeom.BBoxCache(time, [UsdGeom.Tokens.default_])
    bounds = cache.ComputeWorldBound(instancer.GetPrim()).ComputeAlignedRange()
    result[label] = [list(bounds.GetMin()), list(bounds.GetMax())]
(fixtures / "point_instancer_behavior.json").write_text(json.dumps(result, indent=2) + "\n")
