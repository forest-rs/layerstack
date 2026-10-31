# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record what OpenUSD answers about light and shadow linking over draw items.

For `fixtures/stagehand_links/scene.usda`:

- `targets`: the paths a draw list matches link collections against, as
  UsdImaging does: every gprim `Usd.PrimRange` walks with instance proxies
  (`Usd.TraverseInstanceProxies`), by its instance proxy's path inside a
  native instance (`UsdImagingInstanceAdapter::GetInstanceCategories`
  queries the collection cache with the instance's stage path), and each
  `PointInstancer` by its own path, its prototypes' gprims left out
  (`UsdImagingDelegate::GetCategories` gives a point instancer's
  prototypes no categories of their own: its instances take the
  instancer's);
- `links`: for every prim with `UsdLux.LightAPI` (instance proxies
  included) and each of its `lightLink` and `shadowLink` collections,
  `Usd.CollectionAPI(prim, name).ComputeMembershipQuery()`: whether the
  query is trivial as `UsdImaging_CollectionCache` decides it (its rule map
  is exactly `/` with `expandPrims`: the light links everything and has no
  category), the first link whose query is equal to it (`==`: the cache
  shares one category id between equal queries), and `IsPathIncluded` for
  every target.

Membership has no time: `ComputeMembershipQuery` takes none, and
`includeRoot`, `expansionRule` and `membershipExpression` are uniform; the
rock's animated visibility changes nothing. `--check` requires
`oracle.json` to be exactly what usd-core computes.
`layerstack_stagehand`'s `links_match_openusd` test compares the draw
list's link categories and sets with it, as does the TypeScript draw
list's test.
"""
import argparse
import json
import pathlib

from pxr import Sdf, Usd, UsdGeom, UsdLux

parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "stagehand_links"
stage = Usd.Stage.Open(str(here / "scene.usda"))

targets = []
lights = []
walk = iter(Usd.PrimRange(stage.GetPseudoRoot(), Usd.TraverseInstanceProxies()))
for prim in walk:
    if prim.IsA(UsdGeom.PointInstancer):
        targets.append({"path": str(prim.GetPath()), "kind": "instancer"})
        walk.PruneChildren()
        continue
    if prim.IsA(UsdGeom.Gprim):
        kind = "proxy" if prim.IsInstanceProxy() else "gprim"
        targets.append({"path": str(prim.GetPath()), "kind": kind})
    if prim.HasAPI(UsdLux.LightAPI):
        lights.append(prim)

links = []
queries = []
for prim in lights:
    for name in ["lightLink", "shadowLink"]:
        query = Usd.CollectionAPI(prim, name).ComputeMembershipQuery()
        rules = query.GetAsPathExpansionRuleMap()
        trivial = len(rules) == 1 and list(rules.keys())[0] == Sdf.Path.absoluteRootPath and list(rules.values())[0] == Usd.Tokens.expandPrims
        shares = next(i for i, q in enumerate(queries + [query]) if q == query)
        queries.append(query)
        links.append({
            "light": str(prim.GetPath()),
            "proxy": prim.IsInstanceProxy(),
            "link": name,
            "trivial": trivial,
            "shares": shares,
            "included": [target["path"] for target in targets if query.IsPathIncluded(Sdf.Path(target["path"]))],
        })

result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "targets": targets, "links": links}
output = here / "oracle.json"
if args.check:
    assert json.loads(output.read_text()) == result, "stagehand links oracle differs from OpenUSD"
else:
    output.write_text(json.dumps(result, indent=1, sort_keys=True) + "\n")
