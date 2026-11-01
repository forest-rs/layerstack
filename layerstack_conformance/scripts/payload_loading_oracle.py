# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's stage answers, call by call, as payloads load.

Usage: payload_loading_oracle.py [OUT]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes `layerstack_conformance/fixtures/payload_loading/oracle.json` by
default, which `tests/conformance/payload_loading.rs` and the TypeScript
package's `test/loading.test.ts` replay.

Each case opens `scene.usda` with every payload loaded or none
(`UsdStage::Open(..., LoadAll | LoadNone)`), then makes the calls of its
steps: `UsdStage::Load`, `Unload`, `LoadAndUnload` and `SetLoadRules`.
After the open and after each call it records the call's result (whether
`Load` returns a valid prim), the paths the call refused, each with why
(`_IsValidForLoad` and `_IsValidForUnload` report each and skip it,
applying the rest: `inactive`, `not_on_stage` or `prototype`), the
literal rules (`GetLoadRules().GetRules()`), `GetLoadSet()`,
`FindLoadable()`, which of the payloads' layers the stage uses
(`GetUsedLayers()`), and for each path whether a prim is there,
`UsdPrim::IsLoaded` and `UsdPrim::HasAuthoredPayloads`.
"""
import json
import re
import sys
from pathlib import Path

from pxr import Sdf, Tf, Usd

assert Usd.GetVersion() == (0, 26, 11)
HERE = Path(__file__).resolve().parent.parent / "fixtures" / "payload_loading"
NAMES = {
    Usd.StageLoadRules.AllRule: "all",
    Usd.StageLoadRules.OnlyRule: "only",
    Usd.StageLoadRules.NoneRule: "none",
}
BY_NAME = {name: rule for rule, name in NAMES.items()}
POLICIES = {
    "withDescendants": Usd.LoadWithDescendants,
    "withoutDescendants": Usd.LoadWithoutDescendants,
}
PATHS = [
    "/",
    "/World",
    "/World/Rock",
    "/World/Tree",
    "/World/Tree/Local",
    "/World/Tree/Trunk",
    "/World/Tree/Branch",
    "/World/Off",
    "/World/Plain",
    "/World/Nope",
]
PAYLOAD_LAYERS = ["rock.usda", "tree.usda", "leaf.usda"]

CASES = [
    {
        "load": "none",
        "steps": [
            ["load", "/World/Tree", "withoutDescendants"],
            ["load", "/World/Tree/Branch", "withDescendants"],
            ["unload", "/World"],
            ["loadAndUnload", ["/World/Rock", "/World/Tree"], ["/World"], "withDescendants"],
            ["unload", "/World/Tree/Branch"],
            ["load", "/World/Nope", "withDescendants"],
            ["load", "/World/Off", "withDescendants"],
            ["load", "/World/Plain", "withDescendants"],
            ["setLoadRules", [["/", "all"]]],
            ["unload", "/World/Tree/Branch"],
            ["setLoadRules", [["/", "none"], ["/World/Rock", "all"]]],
            ["load", "/", "withDescendants"],
            [
                "loadAndUnload",
                ["/World/Off", "/World/Rock", "/Nowhere/Deep", "/__Prototype_1/Leaf"],
                ["/World/Tree", "/__Prototype_2"],
                "withDescendants",
            ],
            ["unload", "/__Prototype_1"],
            ["load", "/World/Off", "withDescendants"],
            ["loadAndUnload", ["/World/Off", "/World/Tree"], ["/World/Plain"], "withoutDescendants"],
        ],
    },
    {
        "load": "all",
        "steps": [
            ["unload", "/World/Tree"],
            ["load", "/World/Tree/Branch", "withDescendants"],
            ["load", "/World/Tree", "withoutDescendants"],
            ["unload", "/"],
            ["loadAndUnload", ["/World/Tree/Branch"], ["/World/Rock"], "withDescendants"],
            ["load", "/World/Off", "withDescendants"],
            ["loadAndUnload", ["/World/Rock", "/World/Off/Deep", "/Nowhere"], [], "withDescendants"],
        ],
    },
]


def state(stage):
    used = {Path(layer.realPath).name for layer in stage.GetUsedLayers() if layer.realPath}
    prims = {}
    for path in PATHS:
        prim = stage.GetPrimAtPath(path)
        prims[path] = (
            [True, prim.IsLoaded(), prim.HasAuthoredPayloads()] if prim else [False, False, False]
        )
    return {
        "rules": [[str(path), NAMES[rule]] for path, rule in stage.GetLoadRules().GetRules()],
        "loadSet": sorted(str(path) for path in stage.GetLoadSet()),
        "loadable": [str(path) for path in stage.FindLoadable()],
        "usedPayloadLayers": [name for name in PAYLOAD_LAYERS if name in used],
        "prims": prims,
    }


REFUSALS = [
    (re.compile(r"Attempt to load an inactive path <([^>]*)>"), "inactive"),
    (re.compile(r"Attempt to load a path <([^>]*)> which is not present in the stage"), "not_on_stage"),
    (re.compile(r"Attempted to load/unload a prototype path <([^>]*)>"), "prototype"),
]


def refusals(error):
    """The paths an error OpenUSD raised refuses, each with why."""
    found = []
    for pattern, code in REFUSALS:
        found.extend([path, code] for path in pattern.findall(str(error)))
    assert found, str(error)
    return sorted(found)


def call(stage, step):
    """Makes the call; returns its result and the paths it refused."""
    kind = step[0]
    refused = []
    try:
        if kind == "load":
            stage.Load(step[1], POLICIES[step[2]])
        elif kind == "unload":
            stage.Unload(step[1])
        elif kind == "loadAndUnload":
            stage.LoadAndUnload(step[1], step[2], POLICIES[step[3]])
        elif kind == "setLoadRules":
            rules = Usd.StageLoadRules.LoadNone()
            rules.SetRules([(Sdf.Path(path), BY_NAME[rule]) for path, rule in step[1]])
            stage.SetLoadRules(rules)
        else:
            raise ValueError(kind)
    except Tf.ErrorException as error:
        refused = refusals(error)
    # `Load` returns the prim at the path once the rest applied.
    result = ("prim" if stage.GetPrimAtPath(step[1]) else "invalid") if kind == "load" else None
    return result, refused


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "oracle.json"
    cases = []
    for case in CASES:
        load = Usd.Stage.LoadAll if case["load"] == "all" else Usd.Stage.LoadNone
        stage = Usd.Stage.Open(str(HERE / "scene.usda"), load=load)
        opened = state(stage)
        steps = []
        for step in case["steps"]:
            result, refused = call(stage, step)
            steps.append({"call": step, "result": result, "refused": refused, "after": state(stage)})
        cases.append({"load": case["load"], "opened": opened, "steps": steps})
    oracle = {
        "openusd_version": ".".join(str(part) for part in Usd.GetVersion()),
        "paths": PATHS,
        "cases": cases,
    }
    out.write_text(json.dumps(oracle, indent=1) + "\n")


if __name__ == "__main__":
    main()
