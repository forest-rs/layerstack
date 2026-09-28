# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Existing-port authoring, including layered disconnect versus clear."""
import json
import sys
from pathlib import Path
from pxr import Sdf, Usd, UsdShade

weak = Sdf.Layer.CreateAnonymous()
root = Sdf.Layer.CreateAnonymous()
root.subLayerPaths = [weak.identifier]
stage = Usd.Stage.Open(root)
stage.SetEditTarget(weak)
material = UsdShade.Material.Define(stage, "/Material")
shader = UsdShade.Shader.Define(stage, "/Material/Shader")
interface = material.CreateInput("roughness", Sdf.ValueTypeNames.Float)
interface.Set(0.25)
input_port = shader.CreateInput("roughness", Sdf.ValueTypeNames.Float)
input_port.Set(0.75)
output = shader.CreateOutput("result", Sdf.ValueTypeNames.Float)
output.GetAttr().Set(0.5)
input_port.ConnectToSource(interface)
stage.SetEditTarget(root)
records = []

def record(name):
    spec = root.GetPropertyAtPath(input_port.GetAttr().GetPath())
    records.append({
        "name": name,
        "connections": [str(p) for p in input_port.GetAttr().GetConnections()],
        "providers": [str(a.GetPath()) for a in input_port.GetValueProducingAttributes()],
        "local_connections": bool(spec and spec.HasInfo("connectionPaths")),
    })

record("inherited")
input_port.DisconnectSource()
record("disconnected")
input_port.ClearSources()
record("cleared")
input_port.SetConnectedSources([UsdShade.ConnectionSourceInfo(output), UsdShade.ConnectionSourceInfo(interface)])
record("replaced")
input_port.ClearSources()
record("cleared_again")
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "records": records}
dest = Path(__file__).resolve().parents[1] / "fixtures" / "shading_ports.json"
text = json.dumps(result, indent=2) + "\n"
if "--check" in sys.argv:
    assert dest.read_text() == text, "shading port oracle differs"
else:
    dest.write_text(text)
