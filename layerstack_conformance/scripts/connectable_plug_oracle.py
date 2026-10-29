#!/usr/bin/env python3
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's connectable behaviors that schema plugin metadata configures.

Usage: connectable_plug_oracle.py [--check]

`fixtures/connectable_plug/plugin` is a codeless schema plugin whose
`plugInfo.json` gives typed and applied schemas
`providesUsdShadeConnectableAPIBehavior`, `isUsdShadeContainer` and
`requiresUsdShadeEncapsulation` (and leaves them out, sets them false, or
to values that are not booleans), on schemas of their own, derived from
one another and from `Shader`, and on auto-applied and built-in API
schemas. OpenUSD reads one set of plugins per process, so a child process
records the oracle with `PXR_PLUGINPATH_NAME` naming the plugin.

For every prim of `scene.usda` the oracle records whether
`UsdShadeConnectableAPI` is compatible with it (it has a behavior),
`IsContainer` and `RequiresEncapsulation`. For each top-level prim it
records `CanConnect` between every pair of its ports, those of its child
`Inner` (and `Leaf`), and those of `/Outside`, a shader outside every
container, as `[destination, source, allowed]`, and the value-producing
attributes of inputs connected through a container plugin metadata
configures, and through a prim it does not
(`UsdShadeUtils::GetValueProducingAttributes`, with and without
`shaderOutputsOnly`).
`tests/conformance/connectable_plug.rs` replays it against layerstack.

`--check` regenerates the oracle and fails when it differs from the
committed one.
"""

import argparse
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURE = os.path.join(ROOT, "fixtures", "connectable_plug")
PLUGIN = os.path.join(FIXTURE, "plugin")
SCENE = os.path.join(FIXTURE, "scene.usda")
ORACLE = os.path.join(FIXTURE, "oracle.json")


def record():
    from pxr import Usd, UsdShade

    stage = Usd.Stage.Open(SCENE)
    prims = []
    for prim in stage.Traverse():
        api = UsdShade.ConnectableAPI(prim)
        prims.append({
            "path": str(prim.GetPath()),
            "connectable": bool(api),
            "is_container": api.IsContainer(),
            "requires_encapsulation": api.RequiresEncapsulation(),
        })
    outside = stage.GetPrimAtPath("/Outside")
    connections = []
    seen = set()
    for top in stage.GetPseudoRoot().GetChildren():
        if top == outside:
            continue
        group = [top] + list(top.GetChildren()) + [outside]
        ports = [
            attr for prim in group for attr in prim.GetAttributes()
            if UsdShade.Input.IsInput(attr) or UsdShade.Output.IsOutput(attr)
        ]
        for destination in ports:
            for source in ports:
                key = (str(destination.GetPath()), str(source.GetPath()))
                if key in seen:
                    continue
                seen.add(key)
                if UsdShade.Input.IsInput(destination):
                    allowed = UsdShade.Input(destination).CanConnect(source)
                else:
                    allowed = UsdShade.Output(destination).CanConnect(source)
                connections.append([key[0], key[1], bool(allowed)])
    # Value-producing attributes through a container that plugin metadata
    # configures, and through a prim that is none.
    providers = []
    for path in ["/Chain/Inner.inputs:x", "/AppliedChain/Inner.inputs:x",
                 "/Stop/Inner.inputs:x"]:
        source = UsdShade.Input(stage.GetAttributeAtPath(path))
        for outputs_only in [False, True]:
            found = UsdShade.Utils.GetValueProducingAttributes(source, outputs_only)
            providers.append([path, outputs_only, [str(a.GetPath()) for a in found]])
    return {
        "openusd_version": ".".join(str(v) for v in Usd.GetVersion()),
        "prims": prims,
        "connections": connections,
        "providers": providers,
    }


def write(oracle):
    """The oracle as committed: one prim and one connection per line."""
    lines = ['{', f'  "openusd_version": {json.dumps(oracle["openusd_version"])},',
             '  "prims": [']
    lines += [f'    {json.dumps(p, sort_keys=True)},' for p in oracle["prims"]]
    lines[-1] = lines[-1].rstrip(",")
    lines += ['  ],', '  "connections": [']
    lines += [f'    {json.dumps(c)},' for c in oracle["connections"]]
    lines[-1] = lines[-1].rstrip(",")
    lines += ['  ],', '  "providers": [']
    lines += [f'    {json.dumps(p)},' for p in oracle["providers"]]
    lines[-1] = lines[-1].rstrip(",")
    lines += ['  ]', '}']
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--record", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.record:
        print(json.dumps(record()))
        return
    env = dict(os.environ)
    env["PXR_PLUGINPATH_NAME"] = PLUGIN
    result = subprocess.run(
        [sys.executable, os.path.abspath(__file__), "--record"],
        env=env, check=True, capture_output=True, text=True)
    if result.stderr.strip():
        sys.stderr.write(result.stderr)
    text = write(json.loads(result.stdout))
    if args.check:
        with open(ORACLE) as f:
            if f.read() != text:
                raise SystemExit(f"{ORACLE} differs from OpenUSD's; regenerate it")
        print("connectable_plug oracle matches")
    else:
        with open(ORACLE, "w") as f:
            f.write(text)


if __name__ == "__main__":
    main()
