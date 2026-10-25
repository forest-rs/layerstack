# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD authors and reads point-instance ID masks.

Usage: point_instancer_ids_oracle.py [--check]

Needs OpenUSD through Python `pxr` (`pip install usd-core==26.11`).

Opens `fixtures/point_instancer_ids/scene.usda` and makes each call in
`CALLS`, in order, on one stage with OpenUSD's own API:
`UsdGeomPointInstancer::DeactivateIds`, `ActivateIds`, `ActivateAllIds`,
`InvisIds`, `VisIds` and `VisAllIds` (at the default time or a time code).
After each call it records whether OpenUSD accepted it, the root layer's
`inactiveIds` list operation (every bucket), the root layer's
`invisibleIds` default and time samples, and `ComputeMaskAtTime` at the
default time and at the times in `MASK_TIMES`.

`masks` records `ComputeMaskAtTime` on the unedited scene for each entry of
`MASKS`, with and without an explicit `ids` array. The Python bindings do
not expose the `ids` overload, so an explicit array is authored as the
prim's `ids` (on a scratch copy of the scene) before the ordinary call: the
overload and the authored-`ids` path run the same loop over the array, and
neither reads `protoIndices`.

Writes `oracle.json` beside the scene; `tests/conformance/point_instancer_ids.rs`
replays the same calls through `layerstack_schemas`. With `--check` it
writes nothing and fails unless the committed `oracle.json` agrees.
"""
import json
import os
import sys

from pxr import Sdf, Usd, UsdGeom, Vt

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "point_instancer_ids"))

# Each call: `deactivate`/`activate` (prim, ids), `activate_all` (prim),
# `invis`/`vis` (prim, ids, time or None), `vis_all` (prim, time or None).
CALLS = [
    ["deactivate", "/Legacy", [40, 10, 40]],
    ["activate", "/Legacy", [30, 90, 30]],
    ["deactivate", "/Legacy", [50]],
    ["activate", "/Legacy", []],
    ["deactivate", "/Legacy", [99, 60, 99, 50]],
    ["deactivate", "/Explicit", [20, 10]],
    ["activate", "/Explicit", [30, 30]],
    ["deactivate", "/Explicit", []],
    ["activate_all", "/Legacy"],
    ["deactivate", "/Legacy", [20]],
    ["deactivate", "/Sparse", [2, 4]],
    ["activate", "/Sparse", [3, 1]],
    ["deactivate", "/Sparse", [5, 1, 5]],
    ["activate", "/Sparse", [0]],
    ["vis", "/Sampled", [99], None],
    ["invis", "/Sampled", [3, 8, 8], None],
    ["vis", "/Sampled", [7], None],
    ["invis", "/Sampled", [20, 40, 20], None],
    ["invis", "/Sampled", [6], 1.0],
    ["invis", "/Sampled", [9, 9], 1.5],
    ["vis", "/Sampled", [5], 2.0],
    ["vis", "/Sampled", [1], 2.0],
    ["invis", "/Sampled", [1], 0.5],
    ["vis_all", "/Sampled", 3.0],
    ["invis", "/Sampled", [8], 4.0],
    ["vis_all", "/Sampled", None],
    ["vis_all", "/Unauthored", None],
    ["vis_all", "/Unauthored", 2.0],
    ["vis", "/Unauthored", [1], None],
    ["invis", "/Unauthored", [2, 3], 2.0],
    ["vis_all", "/Unauthored", 1.0],
    ["activate_all", "/Unauthored"],
]

MASK_TIMES = [0.5, 1.0, 1.5, 2.0, 3.0, 4.0]

# Each mask: prim, time or None, explicit ids or None.
MASKS = [
    ["/Legacy", None, None],
    ["/Legacy", None, [10, 20, 30, 77, 30]],
    ["/Legacy", None, []],
    ["/Legacy", None, [40, 50]],
    ["/Explicit", None, [30, 10, 20]],
    ["/Sparse", None, None],
    ["/Sparse", None, [0, 1, 2, 3, 4, 4]],
    ["/Sampled", 1.5, None],
    ["/Sampled", 1.5, [5, 6, 7]],
    ["/Sampled", None, [7, 3, 3]],
    ["/Unauthored", None, None],
    ["/Unauthored", None, [1, 2]],
]


def code(time):
    return Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)


def list_op(layer, path):
    spec = layer.GetPrimAtPath(path)
    if not spec or "inactiveIds" not in spec.ListInfoKeys():
        return None
    op = spec.GetInfo("inactiveIds")
    return {
        "explicit": list(op.explicitItems) if op.isExplicit else None,
        "prepend": list(op.prependedItems),
        "append": list(op.appendedItems),
        "delete": list(op.deletedItems),
        "add": list(op.addedItems),
        "reorder": list(op.orderedItems),
    }


def invisible(layer, path):
    attr = layer.GetAttributeAtPath(path + ".invisibleIds")
    if not attr:
        return None
    return {
        "default": list(attr.default) if attr.HasDefaultValue() else None,
        "samples": {
            repr(float(t)): list(layer.QueryTimeSample(attr.path, t))
            for t in layer.ListTimeSamplesForPath(attr.path)
        },
    }


def masks(instancer):
    out = {"default": list(instancer.ComputeMaskAtTime(Usd.TimeCode.Default()))}
    for time in MASK_TIMES:
        out[repr(time)] = list(instancer.ComputeMaskAtTime(Usd.TimeCode(time)))
    return out


def call(stage, entry):
    kind, path = entry[0], entry[1]
    instancer = UsdGeom.PointInstancer(stage.GetPrimAtPath(path))
    if kind == "deactivate":
        accepted = instancer.DeactivateIds(Vt.Int64Array(entry[2]))
    elif kind == "activate":
        accepted = instancer.ActivateIds(Vt.Int64Array(entry[2]))
    elif kind == "activate_all":
        accepted = instancer.ActivateAllIds()
    elif kind == "invis":
        accepted = instancer.InvisIds(Vt.Int64Array(entry[2]), code(entry[3]))
    elif kind == "vis":
        accepted = instancer.VisIds(Vt.Int64Array(entry[2]), code(entry[3]))
    elif kind == "vis_all":
        accepted = instancer.VisAllIds(code(entry[2]))
    else:
        raise SystemExit(f"unknown call {kind}")
    layer = stage.GetRootLayer()
    return {
        "call": entry,
        "accepted": bool(accepted),
        "inactive": list_op(layer, path),
        "invisible": invisible(layer, path),
        "masks": masks(instancer),
    }


def record(scene):
    stage = Usd.Stage.Open(scene)
    initial = []
    for entry in MASKS:
        path, time, ids = entry
        if ids is None:
            instancer = UsdGeom.PointInstancer(stage.GetPrimAtPath(path))
        else:
            scratch = Sdf.Layer.CreateAnonymous(".usda")
            scratch.TransferContent(stage.GetRootLayer())
            scratch_stage = Usd.Stage.Open(scratch)
            instancer = UsdGeom.PointInstancer(scratch_stage.GetPrimAtPath(path))
            attr = instancer.CreateIdsAttr()
            attr.Clear()
            attr.Set(Vt.Int64Array(ids))
        mask = instancer.ComputeMaskAtTime(code(time))
        initial.append({"query": entry, "mask": list(mask)})
    layer = Sdf.Layer.CreateAnonymous(".usda")
    layer.TransferContent(Sdf.Layer.FindOrOpen(scene))
    edited = Usd.Stage.Open(layer)
    return initial, [call(edited, entry) for entry in CALLS]


def main():
    check = sys.argv[1:] == ["--check"]
    initial, calls = record(os.path.join(FIXTURES, "scene.usda"))
    _, minor, patch = Usd.GetVersion()
    result = {"openusd_version": f"{minor}.{patch}", "masks": initial, "calls": calls}
    out = os.path.join(FIXTURES, "oracle.json")
    if check:
        with open(out) as f:
            if json.load(f) != json.loads(json.dumps(result)):
                sys.exit("oracle.json is not what OpenUSD computes; rerun point_instancer_ids_oracle.py")
        return
    with open(out, "w") as f:
        json.dump(result, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
