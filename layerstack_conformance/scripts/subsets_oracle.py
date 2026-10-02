# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record subset validation and discovery using OpenUSD 26.8."""
import argparse
import json
import pathlib
from pxr import Usd, UsdGeom

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "subsets"
stage = Usd.Stage.Open(str(here / "scene.usda"))
rows = []
for prim in stage.GetPseudoRoot().GetChildren():
    geom = UsdGeom.Imageable(prim)
    name = prim.GetName()
    if name == "Sampled":
        element = "point"
    elif name.startswith("Edge"):
        element = "edge"
    elif name.startswith("Segment"):
        element = "segment"
    elif name == "Tet":
        element = "tetrahedron"
    else:
        element = "face"
    valid, reason = UsdGeom.Subset.ValidateFamily(geom, element, "materialBind")
    subsets = UsdGeom.Subset.GetGeomSubsets(geom, "", "materialBind")
    rows.append({
        "path": str(prim.GetPath()), "element": element,
        "valid": valid, "reason": reason,
        "subsets": [str(p.GetPath()) for p in subsets],
        "family_type": str(UsdGeom.Subset.GetFamilyType(geom, "materialBind")),
        "unassigned": list(UsdGeom.Subset.GetUnassignedIndices(
            geom, element, "materialBind", Usd.TimeCode(0))),
    })
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows}
output = here / "oracle.json"
if args.check:
    assert json.loads(output.read_text()) == result
else:
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
