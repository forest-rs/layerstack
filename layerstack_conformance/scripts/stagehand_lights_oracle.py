# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record what OpenUSD answers about the lights of a draw list.

For each light of `fixtures/stagehand_lights/scene.usda` at the default
time and at times 0, 5 and 10: its inputs through the `UsdLux` schema API
(authored or fallback), the blackbody color of its color temperature
(`UsdLuxBlackbodyTemperatureAsRgb`), its `ComputeLocalToWorldTransform`
and `ComputeVisibility`, its `ShapingAPI` and `ShadowAPI` inputs when
applied, and its link collections' `includeRoot`, includes and excludes.
`--check` requires `oracle.json` to be exactly what usd-core computes.
`layerstack_stagehand`'s `light::oracle` test compares its `LightItem`s
with it, as does the TypeScript draw list's test.
"""
import argparse
import json
import pathlib

from pxr import Usd, UsdGeom, UsdLux

parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "stagehand_lights"
stage = Usd.Stage.Open(str(here / "scene.usda"))


def plain(value):
    """A JSON value: vectors as lists, assets as their authored path."""
    if value is None:
        return None
    if hasattr(value, "path"):
        return value.path
    if isinstance(value, (bool, int, float, str)):
        return value
    return [plain(v) for v in value]


SHAPES = {
    "DistantLight": ["angle"],
    "SphereLight": ["radius"],
    "RectLight": ["width", "height", "texture:file"],
    "DiskLight": ["radius"],
    "CylinderLight": ["radius", "length"],
    "DomeLight": ["texture:file", "texture:format"],
    "DomeLight_1": ["texture:file", "texture:format"],
    "PortalLight": ["width", "height"],
}
COMMON = ["color", "intensity", "exposure", "normalize", "enableColorTemperature",
          "colorTemperature", "diffuse", "specular"]
SHAPING = ["shaping:cone:angle", "shaping:cone:softness", "shaping:focus", "shaping:focusTint",
           "shaping:ies:file", "shaping:ies:angleScale", "shaping:ies:normalize"]
SHADOW = ["shadow:enable", "shadow:color", "shadow:distance", "shadow:falloff",
          "shadow:falloffGamma"]

rows = []
for prim in stage.Traverse():
    light = UsdLux.LightAPI(prim)
    if not prim.HasAPI(UsdLux.LightAPI):
        continue
    kind = prim.GetTypeName()
    for time in [None, 0, 5, 10]:
        tc = Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)
        inputs = {name: plain(light.GetInput(name).Get(tc)) for name in COMMON + SHAPES[kind]}
        if prim.HasAPI(UsdLux.ShapingAPI):
            shaping = UsdLux.ShapingAPI(prim)
            inputs.update({name: plain(shaping.GetInput(name).Get(tc)) for name in SHAPING})
        if prim.HasAPI(UsdLux.ShadowAPI):
            shadow = UsdLux.ShadowAPI(prim)
            inputs.update({name: plain(shadow.GetInput(name).Get(tc)) for name in SHADOW})
        attributes = {name: plain(prim.GetAttribute(name).Get(tc))
                      for name in ["treatAsPoint", "treatAsLine", "poleAxis"]
                      if prim.GetAttribute(name).IsValid()}
        links = {}
        for name, collection in [("lightLink", light.GetLightLinkCollectionAPI()),
                                 ("shadowLink", light.GetShadowLinkCollectionAPI())]:
            links[name] = {
                "includeRoot": collection.GetIncludeRootAttr().Get(),
                "includes": [str(p) for p in collection.GetIncludesRel().GetTargets()],
                "excludes": [str(p) for p in collection.GetExcludesRel().GetTargets()],
            }
        temperature = inputs["colorTemperature"]
        rows.append({
            "path": str(prim.GetPath()),
            "type": kind,
            "time": time,
            "inputs": inputs,
            "attributes": attributes,
            "blackbody": list(UsdLux.BlackbodyTemperatureAsRgb(temperature)),
            "world": [list(row) for row in UsdGeom.Xformable(prim).ComputeLocalToWorldTransform(tc)],
            "visibility": str(UsdGeom.Imageable(prim).ComputeVisibility(tc)),
            "shaping": prim.HasAPI(UsdLux.ShapingAPI),
            "shadow": prim.HasAPI(UsdLux.ShadowAPI),
            "links": links,
        })
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows}
output = here / "oracle.json"
if args.check:
    assert json.loads(output.read_text()) == result, "stagehand lights oracle differs from OpenUSD"
else:
    output.write_text(json.dumps(result, indent=1, sort_keys=True) + "\n")
