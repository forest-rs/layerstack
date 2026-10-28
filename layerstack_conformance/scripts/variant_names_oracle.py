# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""`UsdVariantSet::GetVariantNames` for each prim and set in scene.usda:
the variants every node of the prim index defines for the set, through
references and inside selected variants, sorted and each once; a selection
naming no variant, a set with no variants, and a variant set no
`variantSets` list names."""
import json
import sys
from pathlib import Path
from pxr import Usd

here = Path(__file__).resolve().parents[1] / "fixtures" / "variant_names"
stage = Usd.Stage.Open(str(here / "scene.usda"))
rows = []
for path in ["/Base", "/Shape", "/Other", "/Unlisted"]:
    prim = stage.GetPrimAtPath(path)
    for name in ["look", "size", "finish", "hidden", "none"]:
        rows.append({
            "prim": path,
            "set": name,
            "names": prim.GetVariantSet(name).GetVariantNames(),
        })
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "rows": rows}
text = json.dumps(result, indent=2) + "\n"
dest = here / "oracle.json"
if "--check" in sys.argv:
    assert dest.read_text() == text, "variant names oracle differs"
else:
    dest.write_text(text)
