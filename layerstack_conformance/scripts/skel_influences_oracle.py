# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's joint influence utilities, call by call.

Usage: skel_influences_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

Runs `UsdSkel.NormalizeWeights`, `UsdSkel.SortInfluences`,
`UsdSkel.ResizeInfluences`, `UsdSkel.ExpandConstantInfluencesToVarying` and
`UsdSkel.InterleaveInfluences` on each case below and records whether the
call was accepted and the arrays it left. Then, for each mesh of
`fixtures/skel_influences/scene.usda`, records the skinning query's
`IsRigidlyDeformed`, `GetNumInfluencesPerComponent`,
`ComputeJointInfluences` and `ComputeVaryingJointInfluences` for several
point counts (`null` where C++ fails). With `--check`, compares a fresh run
with `fixtures/skel_influences/oracle.json`: acceptance and integers
exactly, weights within 1e-6.

`tests/conformance/skel_influences.rs` replays every case.
"""
import argparse
import json
import math
import pathlib

from pxr import Usd, UsdSkel, Vt

OUTPUT = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "skel_influences" / "oracle.json"

CASES = [
    {"op": "normalize", "weights": [1, 3, 0, 0, -2, 2], "per_component": 2},
    {"op": "normalize", "weights": [0.2, 0.2, 0.2, 1e-9, 1e-9, 0], "per_component": 3},
    {"op": "normalize", "weights": [0.5, 0.25, 0.125], "per_component": 1},
    {"op": "normalize", "weights": [1, 1, 1], "per_component": 2},
    {"op": "normalize", "weights": [1, 1], "per_component": 0},
    {"op": "normalize", "weights": [], "per_component": 4},
    {"op": "normalize", "weights": [0.01, 0.02, 1, 1], "per_component": 2, "epsilon": 0.05},
    {"op": "sort", "indices": [1, 2, 3, 4, 7, 5, 6, 8], "weights": [0.1, 0.9, 0.5, 0.5, 0.2, 0.2, 0.6, 0], "per_component": 4},
    {"op": "sort", "indices": [3, 1, 2], "weights": [0.2, 0.3, 0.5], "per_component": 1},
    {"op": "sort", "indices": [3, 1, 2, 0], "weights": [-1, 2, 0, 0], "per_component": 2},
    {"op": "sort", "indices": [3, 1], "weights": [0.5, 0.5, 0], "per_component": 1},
    {"op": "sort", "indices": [3, 1, 2], "weights": [0.5, 0.5, 0], "per_component": 2},
    {"op": "sort", "indices": [3, 1], "weights": [0.5, 0.5], "per_component": 0},
    {"op": "resize_indices", "indices": [1, 2, 3, 4, 5, 6], "from": 3, "to": 4},
    {"op": "resize_indices", "indices": [1, 2, 3, 4, 5, 6], "from": 3, "to": 1},
    {"op": "resize_indices", "indices": [1, 2, 3, 4, 5, 6], "from": 2, "to": 2},
    {"op": "resize_indices", "indices": [1, 2, 3, 4, 5], "from": 2, "to": 2},
    {"op": "resize_indices", "indices": [1, 2, 3, 4, 5], "from": 2, "to": 3},
    {"op": "resize_indices", "indices": [1, 2, 3], "from": 3, "to": 0},
    {"op": "resize_indices", "indices": [1, 2, 3], "from": 0, "to": 1},
    {"op": "resize_indices", "indices": [], "from": 2, "to": 5},
    {"op": "resize_weights", "weights": [0.5, 0.3, 0.2, 0.1, 0.1, 0.8], "from": 3, "to": 2},
    {"op": "resize_weights", "weights": [0.5, 0.3, 0.2, 0.1, 0.1, 0.8], "from": 3, "to": 5},
    {"op": "resize_weights", "weights": [0.5, 0.5, 0, 0], "from": 2, "to": 1},
    {"op": "resize_weights", "weights": [0, 1, 1, 0], "from": 2, "to": 1},
    {"op": "resize_weights", "weights": [1, 1, 1], "from": 2, "to": 1},
    {"op": "resize_weights", "weights": [1, 1, 1], "from": 3, "to": 0},
    {"op": "resize_weights", "weights": [], "from": 3, "to": 0},
    {"op": "resize_weights", "weights": [1, 1, 1], "from": 0, "to": 2},
    {"op": "expand_indices", "indices": [2, 1], "size": 3},
    {"op": "expand_indices", "indices": [2, 1], "size": 1},
    {"op": "expand_indices", "indices": [2, 1], "size": 0},
    {"op": "expand_indices", "indices": [], "size": 4},
    {"op": "expand_weights", "weights": [0.25, 0.75], "size": 2},
    {"op": "expand_weights", "weights": [0.25, 0.75], "size": 0},
    {"op": "interleave", "indices": [0, 3, 16777217], "weights": [0.5, 0.25, 0.25]},
    {"op": "interleave", "indices": [], "weights": []},
    {"op": "interleave", "indices": [1, 2], "weights": [1]},
]
SCENE = OUTPUT.parent / "scene.usda"
MESHES = ["Vertex", "Rigid", "Indexed", "RigidTooMany", "Mismatched", "Ragged"]
POINT_COUNTS = [0, 2, 3]


def run(case):
    op = case["op"]
    n = case.get("per_component")
    result = dict(case)
    if op == "normalize":
        weights = Vt.FloatArray(case["weights"])
        if "epsilon" in case:
            ok = UsdSkel.NormalizeWeights(weights, n, case["epsilon"])
        else:
            ok = UsdSkel.NormalizeWeights(weights, n)
        result["result_weights"] = [float(w) for w in weights]
    elif op == "sort":
        indices = Vt.IntArray(case["indices"])
        weights = Vt.FloatArray(case["weights"])
        ok = UsdSkel.SortInfluences(indices, weights, n)
        result["result_indices"] = list(indices)
        result["result_weights"] = [float(w) for w in weights]
    elif op in ("expand_indices", "expand_weights"):
        array = (Vt.IntArray(case["indices"]) if op == "expand_indices"
                 else Vt.FloatArray(case["weights"]))
        ok = UsdSkel.ExpandConstantInfluencesToVarying(array, case["size"])
        if op == "expand_indices":
            result["result_indices"] = list(array)
        else:
            result["result_weights"] = [float(w) for w in array]
    elif op == "interleave":
        out = Vt.Vec2fArray(len(case["indices"]))
        ok = UsdSkel.InterleaveInfluences(
            Vt.IntArray(case["indices"]), Vt.FloatArray(case["weights"]), out)
        result["result_interleaved"] = [[float(v[0]), float(v[1])] for v in out]
    elif op == "resize_indices":
        indices = Vt.IntArray(case["indices"])
        ok = UsdSkel.ResizeInfluences(indices, case["from"], case["to"])
        result["result_indices"] = list(indices)
    else:
        weights = Vt.FloatArray(case["weights"])
        ok = UsdSkel.ResizeInfluences(weights, case["from"], case["to"])
        result["result_weights"] = [float(w) for w in weights]
    result["ok"] = bool(ok)
    return result


def arrays(found):
    if found is None:
        return None
    indices, weights = found
    return {"indices": list(indices), "weights": [float(w) for w in weights]}


def queries():
    stage = Usd.Stage.Open(str(SCENE))
    cache = UsdSkel.Cache()
    cache.Populate(UsdSkel.Root(stage.GetPrimAtPath("/Root")), Usd.TraverseInstanceProxies())
    found = {}
    for name in MESHES:
        query = cache.GetSkinningQuery(stage.GetPrimAtPath("/Root/" + name))
        found[name] = {
            "rigid": query.IsRigidlyDeformed(),
            "per_component": query.GetNumInfluencesPerComponent(),
            "influences": arrays(query.ComputeJointInfluences()),
            "varying": {str(n): arrays(query.ComputeVaryingJointInfluences(n))
                        for n in POINT_COUNTS},
        }
    return found


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
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "cases": [run(case) for case in CASES],
        "queries": queries(),
    }
    if args.check:
        assert agree(json.loads(OUTPUT.read_text()), result), "oracle.json is out of date"
    else:
        OUTPUT.write_text(json.dumps(result, indent=1) + "\n")


main()
