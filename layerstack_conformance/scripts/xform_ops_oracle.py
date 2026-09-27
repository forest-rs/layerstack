# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD reads, and authors, transform ops.

Usage: xform_ops_oracle.py [FIXTURES [OUT]]
       xform_ops_oracle.py --check [FIXTURES]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Opens `fixtures/xform_ops/scene.usda`, which `tests/xform_ops.rs` authors
through `layerstack_schemas`' op API, and writes `oracle.json` beside it:

- for every prim, `GetOrderedXformOps` (each op's name, type, precision and
  inversion), `GetResetXformStack`, and `GetLocalTransformation` at the
  default time and at time 5 (linear interpolation);
- the contract of each authoring call the op API has: for each call in
  `CALLS`, made alone on a fresh copy of the scene with OpenUSD's own API
  (`AddXformOp`, `GetXformOp` then `UsdGeomXformOp::Set` at the default
  time or a time code, `ClearXformOpOrder`, `SetResetXformStack`), whether
  OpenUSD accepts it (a coding error rejects it), and the prim as the root
  layer then authors it: `xformOpOrder`, and every `xformOp:*` attribute's
  type, default and time samples. For an added op, also the precision it
  gets.

Values are JSON: vectors, quaternions (`[i, j, k, r]`) and matrices (rows)
as lists. With `--check` it writes nothing and fails unless the committed
`oracle.json` agrees, floats to within 1e-12 of their scale.
"""
import json
import os
import sys

from pxr import Gf, Sdf, Tf, Usd, UsdGeom

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "xform_ops"))

MATRIX = [[0.0, 0.0, 1.0, 0.0], [0.0, 1.0, 0.0, 0.0], [-1.0, 0.0, 0.0, 0.0], [1.0, 2.0, 3.0, 1.0]]

# Each call: `add` (prim, op type, precision, suffix, inverse), `set`
# (prim, op type, suffix, inverse, value, time or None), `clear` (prim) or
# `reset` (prim, on).
CALLS = [
    ["add", "/Pivoted", "translate", "double", "", False],
    ["add", "/Pivoted", "translate", "double", "pivot", True],
    ["add", "/Pivoted", "translate", "half", "other", False],
    ["add", "/Pivoted", "rotateXYZ", "double", "", True],
    ["add", "/Oriented", "rotateX", "float", "tilt", False],
    ["add", "/Oriented", "rotateX", "float", "tilt", True],
    ["add", "/Matrix", "transform", "float", "second", False],
    ["add", "/Matrix", "scaleZ", "float", "", True],
    ["add", "/Reset", "scale", "float", "", False],
    ["add", "/Cleared", "translate", "float", "", False],
    ["set", "/Pivoted", "translate", "", False, [5.0, 6.0, 7.0], None],
    ["set", "/Pivoted", "translate", "pivot", True, [9.0, 9.0, 9.0], None],
    ["set", "/Pivoted", "translate", "pivot", True, [9.0, 9.0, 9.0], 5.0],
    ["set", "/Pivoted", "translate", "pivot", False, [0.25, 0.5, 0.75], 5.0],
    ["set", "/Pivoted", "rotateXYZ", "", False, [1.5, 2.5, 3.5], 5.0],
    ["set", "/Pivoted", "scale", "", False, [0.5, 1.5, 2.5], None],
    ["set", "/Pivoted", "translate", "", False, 2.0, None],
    ["set", "/Oriented", "orient", "", False, [0.0, 0.0, 0.70710678, 0.70710678], None],
    ["set", "/Oriented", "rotateX", "tilt", False, 45.0, 5.0],
    ["set", "/Matrix", "transform", "", False, MATRIX, None],
    ["set", "/Matrix", "scaleZ", "", True, 3.0, None],
    ["set", "/Matrix", "transform", "", False, [1.0, 2.0, 3.0], None],
    ["set", "/Pivoted", "translate", "absent", False, [1.0, 1.0, 1.0], None],
    ["clear", "/Pivoted"],
    ["clear", "/Cleared"],
    ["reset", "/Pivoted", True],
    ["reset", "/Reset", True],
    ["reset", "/Reset", False],
    ["reset", "/Unreset", False],
]

PRECISIONS = {
    "double": UsdGeom.XformOp.PrecisionDouble,
    "float": UsdGeom.XformOp.PrecisionFloat,
    "half": UsdGeom.XformOp.PrecisionHalf,
}


def precision_name(precision):
    return {value: name for name, value in PRECISIONS.items()}[precision]


def op_type(token):
    return getattr(UsdGeom.XformOp, "Type" + token[0].upper() + token[1:])


def matrix(m):
    return [[m[i][j] for j in range(4)] for i in range(4)]


def value(v):
    """`v` as JSON."""
    if isinstance(v, (Gf.Quatd, Gf.Quatf, Gf.Quath)):
        return [float(x) for x in v.GetImaginary()] + [float(v.GetReal())]
    if isinstance(v, (Gf.Matrix4d,)):
        return matrix(v)
    if isinstance(v, (bool, str)):
        return v
    if isinstance(v, (int, float)):
        return float(v)
    if hasattr(v, "__len__"):
        return [value(x) for x in v]
    return float(v)


def python_value(v, type_name):
    """A JSON call value in the attribute's own value type, as OpenUSD's API
    takes it: the op API converts a value to its op's precision the same
    way. A value of another shape keeps its natural type."""
    if isinstance(v, float):
        return v
    if isinstance(v[0], list):
        return Gf.Matrix4d(*[x for row in v for x in row])
    if len(v) == 4:
        quat, vec = {
            "quatf": (Gf.Quatf, Gf.Vec3f),
            "quath": (Gf.Quath, Gf.Vec3h),
        }.get(type_name, (Gf.Quatd, Gf.Vec3d))
        return quat(v[3], vec(v[0], v[1], v[2]))
    vec = {"float3": Gf.Vec3f, "half3": Gf.Vec3h}.get(type_name, Gf.Vec3d)
    return vec(*v)


def authored(layer, prim_path):
    """The prim's `xformOpOrder` and `xformOp:*` attributes in `layer`."""
    spec = layer.GetPrimAtPath(prim_path)
    out = {"order": None, "attributes": {}}
    if not spec:
        return out
    for attr in spec.attributes:
        if attr.name == "xformOpOrder":
            out["order"] = [str(t) for t in attr.default] if attr.HasDefaultValue() else None
        elif attr.name.startswith("xformOp:"):
            samples = {}
            for time in layer.ListTimeSamplesForPath(attr.path):
                samples[repr(float(time))] = value(layer.QueryTimeSample(attr.path, time))
            out["attributes"][attr.name] = {
                "type": str(attr.typeName),
                "default": value(attr.default) if attr.HasDefaultValue() else None,
                "samples": samples,
            }
    return out


def call(scene, entry):
    layer = Sdf.Layer.CreateAnonymous(".usda")
    layer.TransferContent(Sdf.Layer.FindOrOpen(scene))
    stage = Usd.Stage.Open(layer)
    kind, prim_path = entry[0], entry[1]
    xformable = UsdGeom.Xformable(stage.GetPrimAtPath(prim_path))
    accepted = True
    record = {}
    try:
        if kind == "add":
            _, _, token, precision, suffix, inverse = entry
            op = xformable.AddXformOp(op_type(token), PRECISIONS[precision], suffix, inverse)
            accepted = bool(op)
            if accepted:
                record["op_precision"] = precision_name(op.GetPrecision())
        elif kind == "set":
            _, _, token, suffix, inverse, v, time = entry
            op = xformable.GetXformOp(op_type(token), suffix, inverse)
            if not op:
                accepted = False
            else:
                code = Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)
                type_name = str(op.GetAttr().GetTypeName())
                accepted = bool(op.Set(python_value(v, type_name), code))
        elif kind == "clear":
            accepted = bool(xformable.ClearXformOpOrder())
        elif kind == "reset":
            accepted = bool(xformable.SetResetXformStack(entry[2]))
    except Tf.ErrorException:
        # A coding error aborts the call. For `AddXformOp` a precision
        # mismatch is one, yet the op is added: the order then decides.
        accepted = False
    after = authored(layer, prim_path)
    if kind == "add" and not accepted:
        _, _, token, precision, suffix, inverse = entry
        name = f"xformOp:{token}" + (f":{suffix}" if suffix else "")
        if inverse:
            name = "!invert!" + name
        before = authored(Sdf.Layer.FindOrOpen(scene), prim_path)["order"] or []
        if name not in before and name in (after["order"] or []):
            accepted = True
            added = [o for o in xformable.GetOrderedXformOps() if o.GetOpName() == name]
            record["op_precision"] = precision_name(added[0].GetPrecision())
    record.update({"call": entry, "accepted": accepted, "after": after})
    return record


def record(scene):
    stage = Usd.Stage.Open(scene)
    prims = {}
    for prim in stage.Traverse():
        xformable = UsdGeom.Xformable(prim)
        local = {}
        for key, time in [("default", Usd.TimeCode.Default()), ("5", Usd.TimeCode(5.0))]:
            local[key] = matrix(xformable.GetLocalTransformation(time))
        prims[str(prim.GetPath())] = {
            "ops": [
                {
                    "name": str(op.GetOpName()),
                    "op_type": str(UsdGeom.XformOp.GetOpTypeToken(op.GetOpType())),
                    "precision": precision_name(op.GetPrecision()),
                    "inverse": op.IsInverseOp(),
                }
                for op in xformable.GetOrderedXformOps()
            ],
            "resets_xform_stack": xformable.GetResetXformStack(),
            "local": local,
        }
    return prims, [call(scene, entry) for entry in CALLS]


def close(a, b):
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(close(a[k], b[k]) for k in a)
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(close(x, y) for x, y in zip(a, b))
    if isinstance(a, float) and isinstance(b, float):
        return abs(a - b) <= 1e-12 * max(1.0, abs(b))
    return a == b


def main():
    args = sys.argv[1:]
    check = bool(args) and args[0] == "--check"
    if check:
        args = args[1:]
    fixtures = args[0] if args else FIXTURES
    out_path = args[1] if len(args) > 1 else os.path.join(fixtures, "oracle.json")
    prims, calls = record(os.path.join(fixtures, "scene.usda"))
    _, minor, patch = Usd.GetVersion()
    result = {"openusd_version": f"{minor}.{patch}", "prims": prims, "calls": calls}
    if check:
        with open(os.path.join(fixtures, "oracle.json")) as f:
            if not close(json.load(f), json.loads(json.dumps(result))):
                sys.exit("oracle.json is not what OpenUSD computes; rerun xform_ops_oracle.py")
        return
    with open(out_path, "w") as f:
        json.dump(result, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
