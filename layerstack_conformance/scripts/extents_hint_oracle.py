# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's model extents hints.

Usage: extents_hint_oracle.py [--check]

Needs OpenUSD through Python `pxr` (`pip install usd-core==26.11`).

Opens `fixtures/extents_hint/scene.usda` and records:

- `computed`: for every prim, `UsdGeomModelAPI::ComputeExtentsHint` with a
  `UsdGeomBBoxCache` at the default time and at time 1, ignoring and using
  extents hints: one `float3` per entry, two per purpose in
  `UsdGeomImageable::GetOrderedPurposeTokens` order;
- `authored`: `GetExtentsHint` at the default time for every prim;
- `sets`: each call of `SETS` made alone on a fresh copy of the scene:
  `SetExtentsHint` with that many `float3` entries at the default time or a
  time code. For each: what it returned and what the root layer then
  authors for `extentsHint` (type, custom, default and time samples).

Writes `oracle.json` beside the scene; `tests/conformance/extents_hint.rs` compares
`layerstack_schemas`. With `--check` it writes nothing and fails unless the
committed `oracle.json` agrees, floats to within 1e-9 of their scale.
"""
import json
import os
import sys

from pxr import Gf, Sdf, Tf, Usd, UsdGeom, Vt

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "extents_hint"))

# Each: prim, entry count, time (None is the default).
SETS = [
    ["/Bare", 2, None],
    ["/Bare", 4, 1.0],
    ["/Asset", 8, None],
    ["/Hinted", 6, None],
    ["/Hinted", 0, None],
    ["/Hinted", 3, None],
    ["/Bare", 10, None],
]


def code(time):
    return Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)


def vectors(values):
    return [[float(x) for x in v] for v in values]


def entries(count):
    return [Gf.Vec3f(i, -i, 0.5 * i) for i in range(count)]


def record(scene):
    stage = Usd.Stage.Open(scene)
    computed = {}
    authored = {}
    for prim in stage.Traverse():
        api = UsdGeom.ModelAPI(prim)
        out = {}
        for key, time in [("default", None), ("1", 1.0)]:
            for hints in [False, True]:
                cache = UsdGeom.BBoxCache(code(time), [UsdGeom.Tokens.default_], useExtentsHint=hints)
                try:
                    out[f"{key}_{'hints' if hints else 'plain'}"] = vectors(api.ComputeExtentsHint(cache))
                except Tf.ErrorException:
                    out[f"{key}_{'hints' if hints else 'plain'}"] = None
        computed[str(prim.GetPath())] = out
        authored[str(prim.GetPath())] = vectors(api.GetExtentsHint(Usd.TimeCode.Default()) or [])
    sets = []
    for path, count, time in SETS:
        layer = Sdf.Layer.CreateAnonymous(".usda")
        layer.TransferContent(Sdf.Layer.FindOrOpen(scene))
        copy = Usd.Stage.Open(layer)
        api = UsdGeom.ModelAPI(copy.GetPrimAtPath(path))
        try:
            result = bool(api.SetExtentsHint(Vt.Vec3fArray(entries(count)), code(time)))
        except Tf.ErrorException:
            result = None
        attr = layer.GetAttributeAtPath(path + ".extentsHint")
        after = None
        if attr:
            after = {
                "type": str(attr.typeName),
                "custom": attr.custom,
                "default": vectors(attr.default) if attr.HasDefaultValue() else None,
                "samples": {
                    repr(float(t)): vectors(layer.QueryTimeSample(attr.path, t))
                    for t in layer.ListTimeSamplesForPath(attr.path)
                },
            }
        sets.append({"set": [path, count, time], "result": result, "after": after})
    return {"computed": computed, "authored": authored, "sets": sets}


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
    result = record(os.path.join(FIXTURES, "scene.usda"))
    _, minor, patch = Usd.GetVersion()
    result["openusd_version"] = f"{minor}.{patch}"
    out = os.path.join(FIXTURES, "oracle.json")
    if check:
        with open(out) as f:
            if not close(json.load(f), json.loads(json.dumps(result))):
                sys.exit("oracle.json is not what OpenUSD computes; rerun extents_hint_oracle.py")
        return
    with open(out, "w") as f:
        json.dump(result, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
