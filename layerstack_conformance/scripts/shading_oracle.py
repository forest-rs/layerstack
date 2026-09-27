# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record direct connections and terminal tracing through OpenUSD's own APIs."""
import json
import pathlib
import sys
from pxr import Usd, UsdShade

HERE = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "shading"
CONTEXTS = ["", "ri", "missing", "bad", "empty"]

def record():
    stage = Usd.Stage.Open(str(HERE / "scene.usda"))
    connections = {}
    terminals = []
    for prim in stage.Traverse():
        for attr in prim.GetAttributes():
            if not attr.GetName().startswith(("inputs:", "outputs:")):
                continue
            sources, invalid = UsdShade.ConnectableAPI.GetConnectedSources(attr)
            endpoints = [str(s.source.GetPrim().GetPath().AppendProperty(
                ("inputs:" if s.sourceType == UsdShade.AttributeType.Input else "outputs:") + s.sourceName)) for s in sources]
            port = UsdShade.Input(attr) if attr.GetName().startswith("inputs:") else UsdShade.Output(attr)
            connections[str(attr.GetPath())] = {
                "sources": endpoints, "invalid": [str(p) for p in invalid],
                "terminals": [str(a.GetPath()) for a in UsdShade.Utils.GetValueProducingAttributes(port, True)],
            }
        material = UsdShade.Material(prim)
        if material:
            for contexts in CONTEXTS:
                for terminal in ("Surface", "Displacement", "Volume"):
                    shader, name, kind = getattr(material, "Compute" + terminal + "Source")(contexts)
                    terminals.append({"material": str(prim.GetPath()), "context": contexts,
                                      "terminal": terminal.lower(),
                                      "selected": str(shader.GetPrim().GetPath().AppendProperty("outputs:" + name)) if shader else None})
    return {"version": ".".join(map(str, Usd.GetVersion()[1:])), "connections": connections, "materials": terminals}

output = json.dumps(record(), indent=2, sort_keys=True) + "\n"
path = HERE / "oracle.json"
if "--check" in sys.argv:
    assert path.read_text() == output, "shading oracle differs from OpenUSD"
else:
    path.write_text(output)
