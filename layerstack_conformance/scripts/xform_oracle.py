# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's transforms, visibility and purpose for every prim.

Usage: xform_oracle.py [FIXTURES [OUT]]
       xform_oracle.py --check [FIXTURES]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Opens `fixtures/xform/scene.usda` (which references `asset.usda`) and
writes `fixtures/xform/oracle.json` by default. For every prim, at the
default time and at several time codes with linear and with held
interpolation (`linear@1.5`, `held@1.5`), it records:

- for an `Xformable`: `UsdGeomXformCache.GetLocalTransformation` (the
  matrix and whether it resets the transform stack);
- `UsdGeomXformCache.GetLocalToWorldTransform`, and for an `Imageable`
  that `ComputeLocalToWorldTransform` and `ComputeParentToWorldTransform`
  agree with the cache;
- for an `Imageable`: `ComputeVisibility`, `ComputeEffectiveVisibility`
  for each purpose, and `ComputePurposeInfo` (at the default time only,
  as purpose is uniform), with the prim whose authored purpose it is.

Matrices are lists of rows. `tests/xform.rs` compares everything.

With `--check` it writes nothing and fails unless the committed
`oracle.json` is what OpenUSD computes here: the same records, with
matrices equal to within 1e-12 of their scale, as the platform's
trigonometry may round differently in the last place.
"""
import json
import os
import sys

from pxr import Usd, UsdGeom

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "xform"))
TIMES = [0.0, 1.5, 2.0, 3.0]
PURPOSES = ["default", "render", "proxy", "guide"]


def matrix(m):
    return [[m[i][j] for j in range(4)] for i in range(4)]


def sample(stage, prim, time):
    out = {}
    cache = UsdGeom.XformCache(time)
    if prim.IsA(UsdGeom.Xformable):
        local, resets = cache.GetLocalTransformation(prim)
        out["local"] = matrix(local)
        out["resets_xform_stack"] = resets
    out["world"] = matrix(cache.GetLocalToWorldTransform(prim))
    if prim.IsA(UsdGeom.Imageable):
        imageable = UsdGeom.Imageable(prim)
        assert imageable.ComputeLocalToWorldTransform(time) == cache.GetLocalToWorldTransform(prim)
        assert imageable.ComputeParentToWorldTransform(time) == cache.GetParentToWorldTransform(prim)
        out["visibility"] = str(imageable.ComputeVisibility(time))
        out["effective_visibility"] = {
            purpose: str(imageable.ComputeEffectiveVisibility(purpose, time)) for purpose in PURPOSES
        }
    return out


def close(a, b):
    """Whether two records agree, matrices to within 1e-12 of their scale."""
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(close(a[k], b[k]) for k in a)
    if isinstance(a, list) and isinstance(b, list) and a and isinstance(a[0], list):
        if len(a) != len(b):
            return False
        scale = max([1.0] + [abs(v) for row in b for v in row])
        return all(
            abs(x - y) <= 1e-12 * scale for ra, rb in zip(a, b) for x, y in zip(ra, rb)
        )
    return a == b


def authored_on(prim):
    """The prim whose authored purpose `ComputePurposeInfo` inherits: the
    nearest `Imageable` prim, this one or an ancestor, with an authored,
    non-empty `purpose` (`_ComputeAuthoredPurpose` in
    `pxr/usd/usdGeom/imageable.cpp`); `None` for a fallback."""
    while prim:
        if prim.IsA(UsdGeom.Imageable):
            attr = UsdGeom.Imageable(prim).GetPurposeAttr()
            if attr.HasAuthoredValue() and attr.Get():
                return str(prim.GetPath())
        prim = prim.GetParent()
    return None


def main():
    args = sys.argv[1:]
    check = bool(args) and args[0] == "--check"
    if check:
        args = args[1:]
    fixtures = args[0] if args else FIXTURES
    out_path = args[1] if len(args) > 1 else os.path.join(fixtures, "oracle.json")
    stage = Usd.Stage.Open(os.path.join(fixtures, "scene.usda"))
    prims = {}
    for prim in stage.Traverse():
        record = {"samples": {}}
        stage.SetInterpolationType(Usd.InterpolationTypeLinear)
        record["samples"]["default"] = sample(stage, prim, Usd.TimeCode.Default())
        for interpolation, kind in [
            ("linear", Usd.InterpolationTypeLinear),
            ("held", Usd.InterpolationTypeHeld),
        ]:
            stage.SetInterpolationType(kind)
            for time in TIMES:
                record["samples"][f"{interpolation}@{time}"] = sample(stage, prim, Usd.TimeCode(time))
        stage.SetInterpolationType(Usd.InterpolationTypeLinear)
        if prim.IsA(UsdGeom.Imageable):
            info = UsdGeom.Imageable(prim).ComputePurposeInfo()
            record["purpose"] = {
                "purpose": str(info.purpose),
                "inheritable": info.isInheritable,
                "authored_on": authored_on(prim),
            }
        prims[str(prim.GetPath())] = record

    _, minor, patch = Usd.GetVersion()
    if check:
        with open(os.path.join(fixtures, "oracle.json")) as f:
            committed = json.load(f)
        fresh = {"openusd_version": f"{minor}.{patch}", "prims": prims}
        if not close(committed, json.loads(json.dumps(fresh))):
            sys.exit("oracle.json is not what OpenUSD computes; rerun xform_oracle.py")
        return
    # One line per prim and time keeps the file small and its diffs
    # readable.
    def compact(value):
        return json.dumps(value, sort_keys=True, separators=(",", ":"))

    lines = ["{", f' "openusd_version": {json.dumps(f"{minor}.{patch}")},', ' "prims": {']
    for i, (path, record) in enumerate(sorted(prims.items())):
        lines.append(f"  {json.dumps(path)}: {{")
        if "purpose" in record:
            lines.append(f'   "purpose": {compact(record["purpose"])},')
        lines.append('   "samples": {')
        samples = sorted(record["samples"].items())
        for j, (key, value) in enumerate(samples):
            comma = "," if j + 1 < len(samples) else ""
            lines.append(f"    {json.dumps(key)}: {compact(value)}{comma}")
        lines.append("   }")
        lines.append("  }" + ("," if i + 1 < len(prims) else ""))
    lines.append(" }")
    lines.append("}")
    with open(out_path, "w") as f:
        f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
