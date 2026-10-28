# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""How C++ reads an unknown `info:implementationSource` and an empty
`connectability`: `UsdShadeNodeDefAPI::GetImplementationSource` falls back
to `id` (with a warning), so `ComputeSurfaceSource` still finds the
`UsdPreviewSurface`; `UsdShadeInput::GetConnectability` reads an empty token
as `full`, so `CanConnect` accepts it."""
import json
import sys
from pathlib import Path
from pxr import Usd, UsdShade

here = Path(__file__).resolve().parents[1] / "fixtures" / "shading_defaults"
stage = Usd.Stage.Open(str(here / "scene.usda"))
nodes = []
for path in ["/Unknown/Surface", "/Unknown/Texture", "/Plain/Surface", "/Asset/Surface", "/Untyped"]:
    node_def = UsdShade.NodeDefAPI(stage.GetPrimAtPath(path))
    authored = node_def.GetImplementationSourceAttr().Get()
    nodes.append({
        "path": path,
        "authored": None if authored is None else str(authored),
        "effective": str(node_def.GetImplementationSource()),
        "shader_id": node_def.GetShaderId(),
    })
surfaces = []
for path in ["/Unknown", "/Plain", "/Asset"]:
    source, _, _ = UsdShade.Material(stage.GetPrimAtPath(path)).ComputeSurfaceSource()
    surfaces.append({"material": path, "surface": str(source.GetPath()) if source else None})
connections = []
for destination, source in [
    ("/Mat/A.inputs:full", "/Mat/B.outputs:out"),
    ("/Mat/A.inputs:empty", "/Mat/B.outputs:out"),
    ("/Mat/A.inputs:empty", "/Mat.inputs:matIn"),
    ("/Mat/A.inputs:empty", "/Mat.inputs:matEmpty"),
    ("/Mat/A.inputs:bogus", "/Mat/B.outputs:out"),
    ("/Mat/A.inputs:iface", "/Mat.inputs:matIface"),
    ("/Mat/A.inputs:iface", "/Mat.inputs:matEmpty"),
    ("/Mat/A.inputs:iface", "/Mat.inputs:matIn"),
]:
    connections.append({
        "destination": destination,
        "source": source,
        "can_connect": UsdShade.ConnectableAPI.CanConnect(
            UsdShade.Input(stage.GetAttributeAtPath(destination)),
            stage.GetAttributeAtPath(source)),
        "connectability": str(UsdShade.Input(stage.GetAttributeAtPath(destination)).GetConnectability()),
    })
result = {
    "version": ".".join(map(str, Usd.GetVersion()[1:])),
    "nodes": nodes,
    "surfaces": surfaces,
    "connections": connections,
}
text = json.dumps(result, indent=2) + "\n"
dest = here / "oracle.json"
if "--check" in sys.argv:
    assert dest.read_text() == text, "shading defaults oracle differs"
else:
    dest.write_text(text)
