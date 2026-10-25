# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's common transform API and `MakeMatrixXform`, call by call.

Usage: common_xform_api_oracle.py [--check]

Needs OpenUSD through Python `pxr` (`pip install usd-core==26.11`).

Opens `fixtures/common_xform_api/scene.usda` with `weak.usda` as its
sublayer and records:

- `reads`: for every prim, whether `UsdGeomXformCommonAPI` is compatible
  with its op stack, and `GetXformVectors` and
  `GetXformVectorsByAccumulation` at the default time and at time 2;
- `calls`: each call of `CALLS` made alone, on every prim, on a fresh copy
  of the two layers: `SetXformVectors`, `SetTranslate`, `SetRotate`,
  `SetScale` and `SetPivot` of `UsdGeomXformCommonAPI`, and
  `UsdGeomXformable::MakeMatrixXform` (then `Set` of `MATRIX` on the op it
  returns) with the root or the weak layer as edit target. For each: what
  the call returned (`null` when it raised a coding error, which the
  bindings turn into `Tf.ErrorException`) and what the edit target layer
  then authors for the prim: `xformOpOrder` and every `xformOp:*`
  attribute's type, default and time samples;
- `rotation_orders`: `ConvertRotationOrderToOpType` and
  `GetRotationTransform` for each order, and `CanConvertOpTypeToRotationOrder`
  for every op type.

Writes `oracle.json` beside the scene; `tests/conformance/common_xform_api.rs` replays
the calls through `layerstack_schemas`. With `--check` it writes nothing and
fails unless the committed `oracle.json` agrees, floats to within 1e-9 of
their scale.
"""
import json
import os
import sys

from pxr import Gf, Sdf, Tf, Usd, UsdGeom

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "common_xform_api"))

API = UsdGeom.XformCommonAPI
ORDERS = ["XYZ", "XZY", "YXZ", "YZX", "ZXY", "ZYX"]
MATRIX = Gf.Matrix4d(0, 1, 0, 0, -1, 0, 0, 0, 0, 0, 1, 0, 4, 5, 6, 1)

# Each call: a name and its arguments. Times are `None` for the default.
CALLS = [
    ["vectors", [1.0, 2.0, 3.0], [10.0, 20.0, 30.0], [2.0, 3.0, 4.0], [0.5, 1.0, 1.5], "XYZ", None],
    ["vectors", [-1.0, 4.0, 2.0], [30.0, 10.0, 20.0], [3.0, 2.0, 1.0], [1.0, 0.5, 0.0], "ZYX", 2.0],
    ["translate", [4.0, 5.0, 6.0], None],
    ["translate", [7.0, 8.0, 9.0], 2.0],
    ["rotate", [1.0, 2.0, 3.0], "XYZ", None],
    ["rotate", [1.0, 2.0, 3.0], "ZYX", 2.0],
    ["scale", [7.0, 8.0, 9.0], None],
    ["pivot", [0.25, 0.5, 0.75], None],
    ["make_matrix", "root"],
    ["make_matrix", "weak"],
]


def code(time):
    return Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)


def order(name):
    return getattr(API, "RotationOrder" + name)


def order_name(value):
    return {order(name): name for name in ORDERS}[value]


def value(v):
    """`v` as JSON."""
    if isinstance(v, (Gf.Quatd, Gf.Quatf, Gf.Quath)):
        return [float(x) for x in v.GetImaginary()] + [float(v.GetReal())]
    if isinstance(v, Gf.Matrix4d):
        return [[v[i][j] for j in range(4)] for i in range(4)]
    if isinstance(v, (bool, str)):
        return v
    if isinstance(v, (int, float)):
        return float(v)
    if hasattr(v, "__len__"):
        return [value(x) for x in v]
    return float(v)


def authored(layer, path):
    """The prim's `xformOpOrder` and `xformOp:*` attributes in `layer`."""
    spec = layer.GetPrimAtPath(path)
    out = {"order": None, "attributes": {}}
    if not spec:
        return out
    for attr in spec.attributes:
        if attr.name == "xformOpOrder":
            out["order"] = [str(t) for t in attr.default] if attr.HasDefaultValue() else None
        elif attr.name.startswith("xformOp:"):
            out["attributes"][attr.name] = {
                "type": str(attr.typeName),
                "default": value(attr.default) if attr.HasDefaultValue() else None,
                "samples": {
                    repr(float(t)): value(layer.QueryTimeSample(attr.path, t))
                    for t in layer.ListTimeSamplesForPath(attr.path)
                },
            }
    return out


def open_stage(fixtures):
    root = Sdf.Layer.CreateAnonymous(".usda")
    root.TransferContent(Sdf.Layer.FindOrOpen(os.path.join(fixtures, "scene.usda")))
    weak = Sdf.Layer.CreateAnonymous(".usda")
    weak.TransferContent(Sdf.Layer.FindOrOpen(os.path.join(fixtures, "weak.usda")))
    root.subLayerPaths.append(weak.identifier)
    return Usd.Stage.Open(root), root, weak


def make(stage, path, entry):
    prim = stage.GetPrimAtPath(path)
    api = API(prim)
    kind = entry[0]
    if kind == "vectors":
        _, t, r, s, p, o, time = entry
        return api.SetXformVectors(Gf.Vec3d(*t), Gf.Vec3f(*r), Gf.Vec3f(*s), Gf.Vec3f(*p), order(o), code(time))
    if kind == "translate":
        return api.SetTranslate(Gf.Vec3d(*entry[1]), code(entry[2]))
    if kind == "rotate":
        return api.SetRotate(Gf.Vec3f(*entry[1]), order(entry[2]), code(entry[3]))
    if kind == "scale":
        return api.SetScale(Gf.Vec3f(*entry[1]), code(entry[2]))
    if kind == "pivot":
        return api.SetPivot(Gf.Vec3f(*entry[1]), code(entry[2]))
    if kind == "make_matrix":
        op = UsdGeom.Xformable(prim).MakeMatrixXform()
        return bool(op) and op.Set(MATRIX)
    raise SystemExit(f"unknown call {kind}")


def call(fixtures, path, entry):
    stage, root, weak = open_stage(fixtures)
    layer = weak if entry[:2] == ["make_matrix", "weak"] else root
    stage.SetEditTarget(Usd.EditTarget(layer))
    try:
        result = bool(make(stage, path, entry))
    except Tf.ErrorException:
        result = None
    return {"prim": path, "call": entry, "result": result, "after": authored(layer, path)}


def vectors(reading):
    t, r, s, p, o = reading
    return {
        "translation": value(t),
        "rotation": value(r),
        "scale": value(s),
        "pivot": value(p),
        "rotation_order": order_name(o),
    }


def reads(stage):
    out = {}
    for prim in stage.Traverse():
        api = API(prim)
        entry = {"compatible": bool(api)}
        for key, time in [("default", None), ("2", 2.0)]:
            entry["vectors_" + key] = vectors(api.GetXformVectors(code(time)))
            entry["accumulated_" + key] = vectors(api.GetXformVectorsByAccumulation(code(time)))
        out[str(prim.GetPath())] = entry
    return out


def rotation_orders():
    types = [
        "TypeTranslate", "TypeScale", "TypeRotateX", "TypeRotateY", "TypeRotateZ",
        "TypeRotateXYZ", "TypeRotateXZY", "TypeRotateYXZ", "TypeRotateYZX",
        "TypeRotateZXY", "TypeRotateZYX", "TypeOrient", "TypeTransform",
        "TypeTranslateX", "TypeScaleX",
    ]
    return {
        "op_types": {
            name: str(UsdGeom.XformOp.GetOpTypeToken(API.ConvertRotationOrderToOpType(order(name))))
            for name in ORDERS
        },
        "transforms": {
            name: value(API.GetRotationTransform(Gf.Vec3f(10, 20, 30), order(name))) for name in ORDERS
        },
        "convertible": {
            str(UsdGeom.XformOp.GetOpTypeToken(getattr(UsdGeom.XformOp, t))): API.CanConvertOpTypeToRotationOrder(getattr(UsdGeom.XformOp, t))
            for t in types
        },
    }


def record(fixtures):
    stage, _, _ = open_stage(fixtures)
    paths = [str(p.GetPath()) for p in stage.Traverse()]
    return {
        "reads": reads(stage),
        "calls": [call(fixtures, path, entry) for path in paths for entry in CALLS],
        "rotation_orders": rotation_orders(),
        "matrix": value(MATRIX),
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
    result = record(FIXTURES)
    _, minor, patch = Usd.GetVersion()
    result["openusd_version"] = f"{minor}.{patch}"
    out = os.path.join(FIXTURES, "oracle.json")
    if check:
        with open(out) as f:
            if not close(json.load(f), json.loads(json.dumps(result))):
                sys.exit("oracle.json is not what OpenUSD computes; rerun common_xform_api_oracle.py")
        return
    with open(out, "w") as f:
        json.dump(result, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
