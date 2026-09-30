# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record taxonomy and semantic label queries from OpenUSD's public API."""
import json
import pathlib
import sys
from pxr import Gf, Usd, UsdSemantics

HERE = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "labels"

def record():
    stage = Usd.Stage.Open(str(HERE / "scene.usda"))
    prims = [stage.GetPseudoRoot()] + list(stage.Traverse())
    queries = []
    times = [(None, None, True, True), (0, None, True, True), (2, None, True, True), (4, None, True, True),
             (0, 6, True, True), (2, 4, True, True), (3, 3, True, True), (1, 3, False, False),
             (None, 4, False, True), (4, None, True, False), (10, 17, True, True)]
    for taxonomy in ("kind", "role", "empty", "missing"):
        for index, (start, end, closed_start, closed_end) in enumerate(times):
            if index < 4:
                time = Usd.TimeCode.Default() if start is None else Usd.TimeCode(start)
                query = UsdSemantics.LabelsQuery(taxonomy, time)
                mode = "time"
            else:
                interval = Gf.Interval(float('-inf') if start is None else start,
                                       float('inf') if end is None else end, closed_start, closed_end)
                query = UsdSemantics.LabelsQuery(taxonomy, interval)
                mode = "interval"
            for prim in prims:
                direct = list(query.ComputeUniqueDirectLabels(prim))
                inherited = list(query.ComputeUniqueInheritedLabels(prim))
                queries.append({"path": str(prim.GetPath()), "taxonomy": taxonomy,
                                "mode": mode, "start": start, "end": end,
                                "closed_start": closed_start, "closed_end": closed_end,
                                "direct": direct, "inherited": inherited,
                                "has_direct": query.HasDirectLabel(prim, "object"),
                                "has_inherited": query.HasInheritedLabel(prim, "object")})
    taxonomies = [{"path": str(p.GetPath()), "direct": list(UsdSemantics.LabelsAPI.GetDirectTaxonomies(p)),
                   "inherited": list(UsdSemantics.LabelsAPI.ComputeInheritedTaxonomies(p))} for p in prims]
    return {"version": ".".join(map(str, Usd.GetVersion()[1:])), "queries": queries, "taxonomies": taxonomies}

output = json.dumps(record(), indent=2, sort_keys=True) + "\n"
path = HERE / "oracle.json"
if "--check" in sys.argv:
    assert path.read_text() == output, "labels oracle differs from OpenUSD"
else:
    path.write_text(output)
