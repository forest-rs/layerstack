# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Composed standard shader definitions from the OpenUSD 26.8 wheel."""
import json
import sys
from pathlib import Path
from pxr import Sdf, Usd, Gf, Plug

assert Usd.GetVersion() == (0, 26, 8)
plugin = Plug.Registry().GetPluginWithName("usdShaders")
source = Path(plugin.resourcePath) / "shaders/shaderDefs.usda"
stage = Usd.Stage.Open(str(source))

def value(v):
    if v is None or isinstance(v, (str, int, float, bool)):
        return v
    if isinstance(v, Sdf.AssetPath):
        return v.path
    return [value(item) for item in v]

nodes = {}
for prim in stage.Traverse():
    if not prim.IsDefined() or prim.IsAbstract():
        continue
    identifier = prim.GetAttribute("info:id").Get()
    assert identifier
    nodes[identifier] = {attr.GetName(): {"type": str(attr.GetTypeName()), "default": value(attr.Get())}
        for attr in prim.GetAttributes() if attr.GetName().startswith(("inputs:", "outputs:"))}
Path(sys.argv[1]).write_text(json.dumps({"version": "26.8", "nodes": nodes}, indent=2, sort_keys=True) + "\n")
