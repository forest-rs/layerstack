# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""`UsdShadeConnectableAPI::ConnectToSource` (Replace, Prepend, Append) and
single-source `DisconnectSource`, call by call, authored in a root layer
over weak.usda. A source output that does not exist yet is created with the
consumer's type, not custom; prepend and append edit the root layer's list
op, so the weak layer's connections stay. After each call: whether C++
accepts it, the root layer's specs of the consumer and source, and the
consumer's composed connections."""
import json
import sys
from pathlib import Path
from pxr import Sdf, Usd, UsdShade

here = Path(__file__).resolve().parents[1] / "fixtures" / "shade_connections"
weak = Sdf.Layer.FindOrOpen(str(here / "weak.usda"))
root = Sdf.Layer.CreateAnonymous()
root.subLayerPaths = [weak.identifier]
stage = Usd.Stage.Open(root)
stage.SetEditTarget(root)
Mod = UsdShade.ConnectionModification
Type = UsdShade.AttributeType


def spec(path):
    attr = root.GetAttributeAtPath(path)
    if not attr:
        return None
    entry = {"type": str(attr.typeName), "custom": attr.custom}
    if attr.HasInfo("connectionPaths"):
        op = attr.GetInfo("connectionPaths")
        entry["connections"] = {
            "explicit": op.isExplicit,
            "explicit_items": [str(p) for p in op.explicitItems],
            "prepended": [str(p) for p in op.prependedItems],
            "appended": [str(p) for p in op.appendedItems],
            "deleted": [str(p) for p in op.deletedItems],
        }
    return entry


calls = [
    ("connect", "/Mat/A.inputs:x", "/Mat/B", "outputs:out", "replace"),
    ("connect", "/Mat/A.inputs:x", "/Mat/C", "outputs:out", "prepend"),
    ("connect", "/Mat/A.inputs:x", "/Mat/D", "outputs:out", "append"),
    ("connect", "/Mat/A.inputs:z", "/Mat", "inputs:z", "replace"),
    ("disconnect", "/Mat/A.inputs:x", "/Mat/C", "outputs:out", None),
    ("connect", "/Mat/A.inputs:w", "/Mat/B", "outputs:out", "prepend"),
    ("disconnect", "/Mat/A.inputs:w", "/Mat/E", "outputs:out", None),
    ("connect", "/Mat/A.inputs:y", "/Mat/E", "outputs:out", "append"),
    ("connect", "/Mat/A.inputs:y", "/Mat/R", "outputs:out", "replace"),
    ("connect", "/Mat/A.inputs:y", "/Mat/Missing", "outputs:out", "replace"),
]
modes = {"replace": Mod.Replace, "prepend": Mod.Prepend, "append": Mod.Append}
records = []
for call, consumer, prim, name, mode in calls:
    attr = stage.GetAttributeAtPath(consumer)
    source_path = prim + "." + name
    try:
        if call == "connect":
            kind, base = name.split(":", 1)
            info = UsdShade.ConnectionSourceInfo(
                UsdShade.ConnectableAPI(stage.GetPrimAtPath(prim)), base,
                Type.Output if kind == "outputs" else Type.Input)
            accepted = bool(UsdShade.ConnectableAPI.ConnectToSource(attr, info, modes[mode]))
        else:
            accepted = bool(UsdShade.ConnectableAPI.DisconnectSource(
                attr, stage.GetAttributeAtPath(source_path)))
    except Exception:
        accepted = False
    records.append({
        "call": call,
        "consumer": consumer,
        "prim": prim,
        "name": name,
        "mode": mode,
        "accepted": accepted,
        "consumer_spec": spec(consumer),
        "source_spec": spec(source_path),
        "composed": [str(p) for p in attr.GetConnections()],
    })
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "calls": records}
text = json.dumps(result, indent=2) + "\n"
dest = here / "oracle.json"
if "--check" in sys.argv:
    assert dest.read_text() == text, "shading connections oracle differs"
else:
    dest.write_text(text)
