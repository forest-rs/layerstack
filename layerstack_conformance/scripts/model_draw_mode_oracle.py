# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's resolved model draw modes.

Usage: model_draw_mode_oracle.py [--check]

Needs OpenUSD through Python `pxr` (`pip install usd-core==26.11`).

Opens `fixtures/model_draw_mode/scene.usda` and records, for every prim,
`UsdGeomModelAPI(prim).ComputeModelDrawMode(parent)` called once for each
`parent` of `PARENTS` (the empty token, which walks the ancestors, and
explicit parents), and whether the prim is a model (`UsdPrim::IsModel`).

Writes `oracle.json` beside the scene; `tests/conformance/model_draw_mode.rs`
compares `layerstack_schemas`. With `--check` it writes nothing and fails
unless the committed `oracle.json` agrees.
"""
import json
import os
import sys

from pxr import Usd, UsdGeom

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "model_draw_mode"))

PARENTS = ["", "bounds", "inherited", "default"]


def record(scene):
    stage = Usd.Stage.Open(scene)
    prims = {}
    for prim in stage.Traverse():
        api = UsdGeom.ModelAPI(prim)
        prims[str(prim.GetPath())] = {
            "model": prim.IsModel(),
            "modes": {parent: str(api.ComputeModelDrawMode(parent)) for parent in PARENTS},
        }
    return {"prims": prims}


def main():
    check = sys.argv[1:] == ["--check"]
    result = record(os.path.join(FIXTURES, "scene.usda"))
    _, minor, patch = Usd.GetVersion()
    result["openusd_version"] = f"{minor}.{patch}"
    out = os.path.join(FIXTURES, "oracle.json")
    if check:
        with open(out) as f:
            if json.load(f) != json.loads(json.dumps(result)):
                sys.exit("oracle.json is not what OpenUSD computes; rerun model_draw_mode_oracle.py")
        return
    with open(out, "w") as f:
        json.dump(result, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
