# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record light list traversal and authored cache behavior with OpenUSD 26.8."""
import argparse
import json
import pathlib
from pxr import Usd, UsdLux
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "lights"
stage = Usd.Stage.Open(str(here / "scene.usda"))
rows = []
for root in ["/", "/World", "/World/Cached", "/World/Continue", "/World/HaltedLight", "/World/Instance", "/World/Undefining"]:
    api = UsdLux.LightListAPI(stage.GetPrimAtPath(root))
    for name, mode in [("ignore", UsdLux.LightListAPI.ComputeModeIgnoreCache),
                       ("consult", UsdLux.LightListAPI.ComputeModeConsultModelHierarchyCache)]:
        rows.append({"root": root, "mode": name, "targets": sorted(map(str, api.ComputeLightList(mode)))})
api = UsdLux.LightListAPI(stage.GetPrimAtPath("/World/Ignored"))
api.StoreLightList({stage.GetPrimAtPath("/World/Ignored/Child").GetPath(),
                   stage.GetPrimAtPath("/Outside").GetPath()})
stored = list(map(str, api.GetLightListRel().GetTargets()))
behavior = str(api.GetLightListCacheBehaviorAttr().Get())
api.InvalidateLightList()
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows,
          "stored": stored, "behavior": behavior,
          "invalidated": str(api.GetLightListCacheBehaviorAttr().Get()),
          "retained": list(map(str, api.GetLightListRel().GetTargets()))}
output = here / "oracle.json"
if args.check:
    assert json.loads(output.read_text()) == result
else:
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
