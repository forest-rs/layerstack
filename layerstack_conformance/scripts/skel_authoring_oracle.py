# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's skel authoring helpers, call by call.

Usage: skel_authoring_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

On an in-memory stage, makes each call below in order and records whether
it was accepted and what the call's prim then authors in the root layer:
each property's type, `interpolation`, `elementSize`, default and time
samples. The calls are `UsdSkelBindingAPI::CreateJointIndicesPrimvar`,
`CreateJointWeightsPrimvar` and `SetRigidJointInfluence`, and
`UsdSkelAnimation::SetTransforms` (at the default time and at time codes).
It also records `UsdSkel.ComputeJointLocalTransforms` for one topology.

With `--check`, compares a fresh run with
`fixtures/skel_authoring/oracle.json`: acceptance, names and types
exactly, numbers within 1e-6.

`tests/conformance/skel_authoring.rs` replays every call.
"""
import argparse
import json
import math
import pathlib

from pxr import Gf, Sdf, Usd, UsdGeom, UsdSkel, Vt

OUTPUT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "skel_authoring" / "oracle.json"


def matrix(rows):
    return Gf.Matrix4d(*[v for row in rows for v in row])


def rows(m):
    return [[float(m[i][j]) for j in range(4)] for i in range(4)]


SHEAR = [[1, 0.5, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]]
ROTATE_SCALE = [[0, 2, 0, 0], [-1, 0, 0, 0], [0, 0, 0.5, 0], [1, 2, 3, 1]]
MIRROR = [[-1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0.25, 0, 1]]
TILT = [[1, 0, 0, 0], [0, 0.8660254, 0.5, 0], [0, -0.5, 0.8660254, 0], [0, 0, 0, 1]]
SINGULAR = [[1, 0, 0, 0], [0, 0, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]]

# Each call: (prim, operation, arguments).
CALLS = [
    ("/Rigid", "set_rigid_joint_influence", {"joint": 2, "weight": 0.25}),
    ("/Rigid", "set_rigid_joint_influence", {"joint": -1, "weight": 1.0}),
    ("/Rigid", "set_rigid_joint_influence", {"joint": 0, "weight": 1.0}),
    ("/Fresh", "set_rigid_joint_influence", {"joint": -3, "weight": 1.0}),
    ("/Skinned", "create_joint_indices_primvar", {"constant": False, "element_size": None}),
    ("/Skinned", "create_joint_indices_primvar", {"constant": True, "element_size": 3}),
    ("/Skinned", "create_joint_weights_primvar", {"constant": False, "element_size": 0}),
    ("/Skinned", "create_joint_weights_primvar", {"constant": False, "element_size": -1}),
    ("/Skinned", "create_joint_weights_primvar", {"constant": False, "element_size": 4}),
    ("/Typed", "create_joint_weights_primvar", {"constant": False, "element_size": 2}),
    ("/Anim", "set_transforms", {"time": None, "transforms": [ROTATE_SCALE, TILT, MIRROR]}),
    ("/Anim", "set_transforms", {"time": 1.0, "transforms": [SHEAR, TILT, ROTATE_SCALE]}),
    ("/Anim", "set_transforms", {"time": 2.0, "transforms": [TILT, SINGULAR, TILT]}),
    ("/Anim", "set_transforms", {"time": 3.0, "transforms": []}),
]

TOPOLOGY = ["A", "A/B", "A/B/C", "D"]
SKEL_SPACE = [ROTATE_SCALE, TILT, MIRROR, SHEAR]
ROOT_INVERSE = [[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, -5, 0, 1]]


def value(v):
    if v is None:
        return None
    if isinstance(v, (bool, int, str)):
        return v
    if isinstance(v, float):
        return v
    if isinstance(v, Gf.Quatf):
        return [float(x) for x in v.GetImaginary()] + [float(v.GetReal())]
    if hasattr(v, "__len__"):
        return [value(x) for x in v]
    return float(v)


def snapshot(layer, path):
    spec = layer.GetPrimAtPath(path)
    if spec is None:
        return {}
    out = {}
    for prop in spec.properties:
        if not isinstance(prop, Sdf.AttributeSpec):
            continue
        entry = {
            "type": str(prop.typeName),
            "interpolation": prop.GetInfo("interpolation") if prop.HasInfo("interpolation") else None,
            "elementSize": prop.GetInfo("elementSize") if prop.HasInfo("elementSize") else None,
            "default": value(prop.default) if prop.HasDefaultValue() else None,
            "timeSamples": {
                repr(float(t)): value(layer.QueryTimeSample(prop.path, t))
                for t in layer.ListTimeSamplesForPath(prop.path)
            },
        }
        out[prop.name] = entry
    return out


def call(stage, path, op, args):
    prim = stage.GetPrimAtPath(path)
    if op == "set_rigid_joint_influence":
        return UsdSkel.BindingAPI(prim).SetRigidJointInfluence(args["joint"], args["weight"])
    if op in ("create_joint_indices_primvar", "create_joint_weights_primvar"):
        binding = UsdSkel.BindingAPI(prim)
        create = (binding.CreateJointIndicesPrimvar if op == "create_joint_indices_primvar"
                  else binding.CreateJointWeightsPrimvar)
        size = args["element_size"]
        primvar = create(args["constant"]) if size is None else create(args["constant"], size)
        return bool(primvar) and primvar.GetTypeName() == (
            Sdf.ValueTypeNames.IntArray if op == "create_joint_indices_primvar"
            else Sdf.ValueTypeNames.FloatArray)
    if op == "set_transforms":
        time = Usd.TimeCode.Default() if args["time"] is None else Usd.TimeCode(args["time"])
        xforms = Vt.Matrix4dArray([matrix(m) for m in args["transforms"]])
        return UsdSkel.Animation(prim).SetTransforms(xforms, time)
    raise ValueError(op)


def run():
    stage = Usd.Stage.CreateInMemory()
    layer = stage.GetRootLayer()
    rigid = UsdGeom.Mesh.Define(stage, "/Rigid").GetPrim()
    UsdSkel.BindingAPI.Apply(rigid)
    UsdGeom.Mesh.Define(stage, "/Fresh")
    skinned = UsdGeom.Mesh.Define(stage, "/Skinned").GetPrim()
    UsdSkel.BindingAPI.Apply(skinned)
    typed = UsdGeom.Mesh.Define(stage, "/Typed").GetPrim()
    typed.CreateAttribute("primvars:skel:jointWeights", Sdf.ValueTypeNames.Double, False)
    UsdSkel.Animation.Define(stage, "/Anim")
    calls = []
    for path, op, args in CALLS:
        before = snapshot(layer, path)
        try:
            accepted = bool(call(stage, path, op, args))
        except Exception:
            accepted = False
        after = snapshot(layer, path)
        calls.append({
            "prim": path,
            "op": op,
            "args": args,
            "accepted": accepted,
            "changed_when_refused": (not accepted) and before != after,
            "prim_after": after,
        })
    topology = UsdSkel.Topology(TOPOLOGY)
    xforms = Vt.Matrix4dArray([matrix(m) for m in SKEL_SPACE])
    local = UsdSkel.ComputeJointLocalTransforms(topology, xforms)
    rooted = UsdSkel.ComputeJointLocalTransforms(topology, xforms, matrix(ROOT_INVERSE))
    return {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "calls": calls,
        "local_transforms": {
            "joints": TOPOLOGY,
            "skeleton_space": SKEL_SPACE,
            "root_inverse": ROOT_INVERSE,
            "local": [rows(m) for m in local],
            "rooted": [rows(m) for m in rooted],
        },
    }


def agree(a, b):
    if isinstance(a, dict):
        return isinstance(b, dict) and a.keys() == b.keys() and all(agree(a[k], b[k]) for k in a)
    if isinstance(a, list):
        return isinstance(b, list) and len(a) == len(b) and all(agree(x, y) for x, y in zip(a, b))
    if isinstance(a, float) or isinstance(b, float):
        return math.isclose(a, b, rel_tol=1e-6, abs_tol=1e-6)
    return a == b


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    result = run()
    if args.check:
        assert agree(json.loads(OUTPUT.read_text()), result), "oracle.json is out of date"
    else:
        OUTPUT.write_text(json.dumps(result, indent=1) + "\n")


main()
