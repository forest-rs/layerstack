# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record schema behavior with the matching OpenUSD wheel; run from repo root."""
from pathlib import Path
import json
from pxr import Usd, UsdGeom

fixtures = Path("layerstack_conformance/fixtures")
version = ".".join(map(str, Usd.GetVersion()))
assert version == "0.26.8", version
stage = Usd.Stage.Open(str(fixtures / "primvar_behavior.usda"))
api = UsdGeom.PrimvarsAPI(stage.GetPrimAtPath("/Root/Group/Mesh"))
result = {"version": version}
for name in ["packed", "scalar", "animated", "blockedIndices", "empty"]:
    var = api.GetPrimvar(name)
    value = var.ComputeFlattened()
    if value is not None and not isinstance(value, (float, int, str)):
        value = list(value)
    result[name] = {"interpolation": str(var.GetInterpolation()),
                    "elementSize": var.GetElementSize(),
                    "indexed": var.IsIndexed(), "default": value}
result["animated"]["at1"] = list(api.GetPrimvar("animated").ComputeFlattened(1))
result["inheritedNames"] = sorted(str(v.GetBaseName()) for v in api.FindPrimvarsWithInheritance())
(fixtures / "primvar_behavior.json").write_text(json.dumps(result, indent=2) + "\n")
