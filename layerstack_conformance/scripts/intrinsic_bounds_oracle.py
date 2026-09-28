# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Intrinsic geometry extent evidence from OpenUSD's boundable API."""
import json
import sys
from pathlib import Path
from pxr import Sdf, Usd, UsdGeom

DEST = Path(__file__).resolve().parents[1] / "fixtures" / "intrinsic_bounds"
stage = Usd.Stage.CreateInMemory()
for schema in (UsdGeom.Cube, UsdGeom.Sphere, UsdGeom.Cylinder, UsdGeom.Cone, UsdGeom.Capsule):
    name = schema.__name__
    schema.Define(stage, "/" + name + "Default")
    for axis in ("X", "Y", "Z", "invalid") if hasattr(schema, "GetAxisAttr") else ("Z",):
        obj = schema.Define(stage, "/" + name + axis)
        if hasattr(obj, "CreateSizeAttr"):
            obj.CreateSizeAttr(7.25)
            attr = obj.GetSizeAttr()
        else:
            obj.CreateRadiusAttr(2.25)
            attr = obj.GetRadiusAttr()
        attr.Set(1.25, 0)
        attr.Set(3.75, 2)
        if hasattr(obj, "CreateHeightAttr"):
            obj.CreateHeightAttr(5.25)
            obj.CreateAxisAttr(axis)
    negative = schema.Define(stage, "/" + name + "Negative")
    if hasattr(negative, "CreateSizeAttr"):
        negative.CreateSizeAttr(-2)
    else:
        negative.CreateRadiusAttr(-2)
for name, value in (("Authored", [(-8, -9, -10), (8, 9, 10)]), ("Malformed", [(1, 2, 3)]), ("Blocked", Sdf.ValueBlock())):
    obj = UsdGeom.Cube.Define(stage, "/Cube" + name)
    obj.CreateSizeAttr(12)
    obj.CreateExtentAttr(value)
obj = UsdGeom.Cube.Define(stage, "/CubeChangingExtent")
obj.CreateSizeAttr(12)
obj.CreateExtentAttr().Set(Sdf.ValueBlock(), 0)
obj.GetExtentAttr().Set([(-2, -3, -4), (2, 3, 4)], 2)
obj = UsdGeom.Cylinder.Define(stage, "/CylinderChangingAxis")
obj.CreateHeightAttr(8)
obj.CreateAxisAttr().Set("X", 0)
obj.GetAxisAttr().Set("Y", 2)
for name, points in (("Mesh", [(-3, 2, -1), (4, -5, 6), (1, 2, 3)]), ("EmptyMesh", [])):
    obj = UsdGeom.Mesh.Define(stage, "/" + name)
    obj.CreatePointsAttr(points)
mesh = UsdGeom.Mesh.Define(stage, "/AnimatedMesh")
mesh.CreatePointsAttr([(-1, -2, -3), (1, 2, 3)])
mesh.GetPointsAttr().Set([(-2, -3, -4), (2, 3, 4)], 0)
mesh.GetPointsAttr().Set([(-4, -5, -6), (4, 5, 6)], 2)
UsdGeom.Mesh.Define(stage, "/MissingPoints")
# Exercise the existing default-time-block-hides-fallback divergence too.
UsdGeom.Sphere.Define(stage, "/BlockedRadius").CreateRadiusAttr(Sdf.ValueBlock())
records = []
for time in (None, 0.0, 1.0, 2.0, None):
    for prim in stage.Traverse():
        extent = UsdGeom.Boundable(prim).ComputeExtent(Usd.TimeCode.Default() if time is None else Usd.TimeCode(time))
        bounds = None
        if extent is not None and len(extent) == 2:
            empty = any(extent[0][i] > extent[1][i] for i in range(3))
            bounds = {"empty": empty}
            if not empty:
                bounds.update(min=list(extent[0]), max=list(extent[1]))
        record = {"path": str(prim.GetPath()), "time": time, "bounds": bounds}
        if str(prim.GetPath()) == "/BlockedRadius" and time is None:
            # docs/generic-sparse-composition.md: AOUSD resolves the fallback;
            # C++ 26.8 instead has no value at default time. Preserve its result.
            assert bounds is None
            record["divergence"] = "default-time-block-hides-fallback"
        if str(prim.GetPath()) == "/CubeChangingExtent" and time in (0.0, 1.0):
            # Existing sampled-block-drops-fallback divergence: C++ has no
            # extent value and computes from size; AOUSD resolves extent's
            # schema fallback, so the fallback itself supplies the bound.
            assert bounds == {"empty": False, "min": [-6.0] * 3, "max": [6.0] * 3}
            record["divergence"] = "sampled-block-drops-fallback"
        records.append(record)
outputs = {"scene.usda": stage.GetRootLayer().ExportToString().rstrip() + "\n",
           "oracle.json": json.dumps({"openusd_version": ".".join(map(str, Usd.GetVersion()[1:]))})[:-1]
           + ',"records":[\n' + ',\n'.join(json.dumps(record, separators=(",", ":")) for record in records) + '\n]}\n' }
DEST.mkdir(parents=True, exist_ok=True)
for name, data in outputs.items():
    if "--check" in sys.argv:
        assert (DEST / name).read_text() == data, name + " differs"
    else:
        (DEST / name).write_text(data)
