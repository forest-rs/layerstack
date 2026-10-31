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
  instancer's arrays, its `ids` entry when the instancer authors `ids`
  (`invisibleIds` naming ids, not positions), and the light's transform relative to its prototype
  (`UsdGeomXformCache.ComputeRelativeTransform`) times the instance's
  times the instancer's `ComputeLocalToWorldTransform`, and the
  instancer's visibility;
- under `draws`, for each gprim inside a `PointInstancer`'s prototype,
  once per instance of that prototype `ComputeInstanceTransformsAtTime`
  keeps, in the instancer's order: the instance's position, its `ids`
  entry (or its position when `ids` is unauthored), and the gprim's
  transform relative to its prototype times the instance's times the
  instancer's. Two instancers sample `protoIndices`, `ids`, `positions`
  and `invisibleIds` (`ComputeMaskAtTime`), so their instances differ
  at each time: none at the default time, where they have no values.

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
draws = []
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
        if indices is None:
            # Nothing authored at this time: no instances.
            continue
        ids = instancer.GetIdsAttr().Get(tc)
        mask = instancer.ComputeMaskAtTime(tc)
        placed = instancer.ComputeInstanceTransformsAtTime(tc, tc)
        world = UsdGeom.Xformable(prim).ComputeLocalToWorldTransform(tc)
        visibility = str(UsdGeom.Imageable(prim).ComputeVisibility(tc))
        kept = [i for i in range(len(indices)) if not mask or mask[i]]
        assert len(kept) == len(placed)
        for at, index in enumerate(kept):
            prototype = stage.GetPrimAtPath(prototypes[indices[index]])
            for inner in Usd.PrimRange(prototype):
                if inner.IsA(UsdGeom.Gprim):
                    relative, _ = cache.ComputeRelativeTransform(inner, prototype)
                    draws.append({
                        "time": time,
                        "path": str(inner.GetPath()),
                        "instancer": str(prim.GetPath()),
                        "index": index,
                        "id": index if ids is None else int(ids[index]),
                        "world": matrix(relative * placed[at] * world),
                    })
                if not inner.HasAPI(UsdLux.LightAPI):
                    continue
                relative, _ = cache.ComputeRelativeTransform(inner, prototype)
                rows.append({
                    "time": time,
                    "path": str(inner.GetPath()),
                    "instance": {
                        "instancer": str(prim.GetPath()),
                        "index": index,
                        "id": None if ids is None else int(ids[index]),
                    },
                    "proxy": False,
                    "type": str(inner.GetTypeName()),
                    "camera": False,
                    "world": matrix(relative * placed[at] * world),
                    "visibility": visibility,
                })
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows, "draws": draws}
output = here / "oracle.json"
if args.check:
    assert json.loads(output.read_text()) == result, "stagehand instanced lights oracle differs from OpenUSD"
else:
    output.write_text(json.dumps(result, indent=1, sort_keys=True) + "\n")
