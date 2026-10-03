# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record bounded UsdLux helpers from the pinned C++ Python bindings."""
import argparse
import json
import pathlib
from pxr import Usd, UsdLux
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
root = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "lux_helpers"
stage = Usd.Stage.Open(str(root / "scene.usda"))
light = UsdLux.LightAPI(stage.GetPrimAtPath("/Light"))
filter = UsdLux.LightFilter(stage.GetPrimAtPath("/Filter"))
contexts = [[], ["gpu"], ["absent", "empty", "gpu", "ray"], ["ray", "gpu"], ["absent"], ["", "gpu"]]
selections = [{"contexts": c, "light": str(light.GetShaderId(c)), "filter": str(filter.GetShaderId(c))} for c in contexts]
blocked_selections = []
for path, is_filter in [("/Blocked", False), ("/ContextBlocked", False), ("/BlockedFilter", True), ("/IncompatibleOverBlock", False), ("/IncompatibleOverToken", False)]:
    api = UsdLux.LightFilter(stage.GetPrimAtPath(path)) if is_filter else UsdLux.LightAPI(stage.GetPrimAtPath(path))
    for ordered_contexts in [[], ["gpu"], ["gpu", "ray"], ["", "ray"]]:
        blocked_selections.append({"path": path, "filter": is_filter, "contexts": ordered_contexts, "id": str(api.GetShaderId(ordered_contexts))})
links = []
for label, collection in [("light", light.GetLightLinkCollectionAPI()), ("shadow", light.GetShadowLinkCollectionAPI()), ("filter", filter.GetFilterLinkCollectionAPI())]:
    query = collection.ComputeMembershipQuery()
    links.append({"kind": label, "included": [str(p.GetPath()) for p in [stage.GetPrimAtPath("/Included"), stage.GetPrimAtPath("/Excluded"), stage.GetPrimAtPath("/Unshadowed")] if query.IsPathIncluded(p.GetPath())]})
dome = UsdLux.DomeLight(stage.GetPrimAtPath("/Dome"))
dome.OrientToStageUpAxis()
dome.OrientToStageUpAxis()
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])),
    "blackbody": [{"kelvin": k, "rgb": list(UsdLux.BlackbodyTemperatureAsRgb(k))} for k in [-1000, 0, 1000, 1100, 1300, 1499.75, 1500, 2000, 2500, 4000, 6500, 6750.25, 7000, 9000, 9999, 10000, 12000, 1e30]],
    "selections": selections, "blocked_selections": blocked_selections, "links": links,
    "dome_order": list(dome.GetXformOpOrderAttr().Get()),
    "dome_angle": dome.GetPrim().GetAttribute("xformOp:rotateX:orientToStageUpAxis").Get()}
output = root / "oracle.json"
if args.check:
    assert json.loads(output.read_text()) == result
else:
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
