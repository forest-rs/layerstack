# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record ID targets, sample discovery and inheritance against OpenUSD 26.8."""
from pathlib import Path
import json
from pxr import Usd, UsdGeom
fixtures = Path("layerstack_conformance/fixtures")
assert Usd.GetVersion() == (0, 26, 8), Usd.GetVersion()
stage = Usd.Stage.Open(str(fixtures / "primvar_queries.usda"))
api = UsdGeom.PrimvarsAPI(stage.GetPrimAtPath("/Root/Group/Mesh"))
result = {"version": ".".join(map(str, Usd.GetVersion())), "idTargets": {}, "samples": {}, "inheritance": {}}
for name in ["id", "emptyId", "manyId", "ids", "oneId", "notString", "notRel"]:
    pv = api.GetPrimvar(name)
    value = pv.Get()
    if value is not None and not isinstance(value, (str, float)): value = list(value)
    result["idTargets"][name] = {"value": value, "isIdTarget": pv.IsIdTarget()}
for name in ["indexOnly", "both", "blockedIndices", "single"]:
    pv = api.GetPrimvar(name)
    result["samples"][name] = {"times": pv.GetTimeSamples(), "varying": pv.ValueMightBeTimeVarying(), "indexed": pv.IsIndexed()}
for path in ["/Root", "/Root/Group", "/Root/Group/Mesh", "/Root/Group/Mesh/Child", "/Root/Group/Sibling"]:
    a = UsdGeom.PrimvarsAPI(stage.GetPrimAtPath(path))
    def entries(values): return sorted([str(v.GetPrimvarName()), str(v.GetAttr().GetPrimPath())] for v in values)
    result["inheritance"][path] = {"inheritable": entries(a.FindInheritablePrimvars()), "all": entries(a.FindPrimvarsWithInheritance())}
(fixtures / "primvar_queries.json").write_text(json.dumps(result, indent=2) + "\n")
