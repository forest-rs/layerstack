# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's payload load rules after sequences of rule edits.

Usage: load_rules_oracle.py [OUT]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes `layerstack_conformance/fixtures/load_rules/oracle.json` by default,
which `tests/load_rules.rs` replays against `PayloadLoadRules`.

Each case starts from all or no payloads loaded and applies a fixed,
pseudo-random sequence of `LoadWithDescendants`, `LoadWithoutDescendants`,
`Unload` and `AddRule` edits over a small namespace. After each edit it
records the literal rules and `GetEffectiveRuleForPath` of every path, and
the same once `Minimize` has run on a copy (`UsdStageLoadRules` in
`pxr/usd/usd/stageLoadRules.cpp`).
"""
import json
import random
import sys
from pathlib import Path

from pxr import Sdf, Usd

assert Usd.GetVersion() == (0, 26, 8)
Rules = Usd.StageLoadRules
NAMES = {Rules.AllRule: "all", Rules.OnlyRule: "only", Rules.NoneRule: "none"}
BY_NAME = {name: rule for rule, name in NAMES.items()}
PATHS = ["/", "/A", "/A/B", "/A/B/C", "/A/B/D", "/A/E", "/A/E/F", "/G", "/G/H", "/G/H/I"]


def state(rules):
    return {
        "rules": sorted([str(path), NAMES[rule]] for path, rule in rules.GetRules()),
        "effective": {path: NAMES[rules.GetEffectiveRuleForPath(Sdf.Path(path))] for path in PATHS},
    }


def apply(rules, op):
    kind, path = op[0], Sdf.Path(op[1])
    if kind == "load_with_descendants":
        rules.LoadWithDescendants(path)
    elif kind == "load_without_descendants":
        rules.LoadWithoutDescendants(path)
    elif kind == "unload":
        rules.Unload(path)
    else:
        rules.AddRule(path, BY_NAME[op[2]])


random.seed(26_08)
kinds = ["load_with_descendants", "load_without_descendants", "unload", "add_rule"]
cases = []
for start in ["all", "none"] * 6:
    rules = Rules.LoadAll() if start == "all" else Rules.LoadNone()
    steps = []
    for _ in range(12):
        kind = random.choice(kinds)
        op = [kind, random.choice(PATHS)]
        if kind == "add_rule":
            op.append(random.choice(["all", "only", "none"]))
        apply(rules, op)
        minimized = Rules(rules)
        minimized.Minimize()
        steps.append({"op": op, "after": state(rules), "minimized": state(minimized)})
    cases.append({"start": start, "steps": steps})

out = Path(sys.argv[1]) if len(sys.argv) > 1 else (
    Path(__file__).resolve().parent.parent / "fixtures" / "load_rules" / "oracle.json")
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(json.dumps({"openusd_version": "26.8", "paths": PATHS, "cases": cases}, separators=(",", ":")) + "\n")
