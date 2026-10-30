# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record what OpenUSD answers about lights and cameras inside instances.

For `fixtures/stagehand_instanced_lights/scene.usda` at the default time and
at times 0 and 10, the lights and cameras a draw list lists:

- every prim `Usd.PrimRange` walks with instance proxies
  (`Usd.TraverseInstanceProxies`) that is a `Camera` or has
  `UsdLux.LightAPI` (built-in lights, `MeshLightAPI` meshes) or is a
  `GeometryLight`, a point instancer's subtree pruned: its path (an instance proxy's inside a native
  instance), whether it is an instance proxy, its typed schema,
  `ComputeLocalToWorldTransform` and `ComputeVisibility`;
- for each light inside a `PointInstancer`'s prototype, once per instance
  `ComputeInstanceTransformsAtTime` keeps (the mask applied, the
  prototype's transform included): the instance's position in the
  instancer's arrays, and the light's transform relative to its prototype
  (`UsdGeomXformCache.ComputeRelativeTransform`) times the instance's
  times the instancer's `ComputeLocalToWorldTransform`, and the
  instancer's visibility.

The scene uses translations and scales only, so every product is exact
on every platform. `--check` requires `oracle.json` to be exactly what
usd-core computes. `layerstack_stagehand`'s `instanced_lights_match_openusd`
test compares its `LightItem`s and `CameraItem`s with it, as does the
TypeScript draw list's test.
"""
import argparse
import json
import pathlib

from pxr import Gf, Usd, UsdGeom, UsdLux

parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "stagehand_instanced_lights"
stage = Usd.Stage.Open(str(here / "scene.usda"))


def matrix(m):
    return [list(row) for row in m]


rows = []
for time in [None, 0.0, 10.0]:
    tc = Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)
    walk = iter(Usd.PrimRange(stage.GetPseudoRoot(), Usd.TraverseInstanceProxies()))
    for prim in walk:
        # A point instancer's prototypes are listed per instance below, not
        # where they are defined.
        if prim.IsA(UsdGeom.PointInstancer):
            walk.PruneChildren()
            continue
        camera = prim.IsA(UsdGeom.Camera)
        light = prim.HasAPI(UsdLux.LightAPI) or prim.IsA(UsdLux.GeometryLight)
        if not (camera or light):
            continue
        rows.append({
            "time": time,
            "path": str(prim.GetPath()),
            "instance": None,
            "proxy": prim.IsInstanceProxy(),
            "type": str(prim.GetTypeName()),
            "camera": camera,
            "world": matrix(UsdGeom.Xformable(prim).ComputeLocalToWorldTransform(tc)),
            "visibility": str(UsdGeom.Imageable(prim).ComputeVisibility(tc)),
        })
    cache = UsdGeom.XformCache(tc)
    for prim in stage.Traverse():
        instancer = UsdGeom.PointInstancer(prim)
        if not instancer:
            continue
        prototypes = instancer.GetPrototypesRel().GetTargets()
        indices = instancer.GetProtoIndicesAttr().Get(tc)
        mask = instancer.ComputeMaskAtTime(tc)
        placed = instancer.ComputeInstanceTransformsAtTime(tc, tc)
        world = UsdGeom.Xformable(prim).ComputeLocalToWorldTransform(tc)
        visibility = str(UsdGeom.Imageable(prim).ComputeVisibility(tc))
        kept = [i for i in range(len(indices)) if not mask or mask[i]]
        assert len(kept) == len(placed)
        for at, index in enumerate(kept):
            prototype = stage.GetPrimAtPath(prototypes[indices[index]])
            for inner in Usd.PrimRange(prototype):
                if not inner.HasAPI(UsdLux.LightAPI):
                    continue
                relative, _ = cache.ComputeRelativeTransform(inner, prototype)
                rows.append({
                    "time": time,
                    "path": str(inner.GetPath()),
                    "instance": {"instancer": str(prim.GetPath()), "index": index},
                    "proxy": False,
                    "type": str(inner.GetTypeName()),
                    "camera": False,
                    "world": matrix(relative * placed[at] * world),
                    "visibility": visibility,
                })
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows}
output = here / "oracle.json"
if args.check:
    assert json.loads(output.read_text()) == result, "stagehand instanced lights oracle differs from OpenUSD"
else:
    output.write_text(json.dumps(result, indent=1, sort_keys=True) + "\n")
