# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Asset-valued namespaced render settings keep their anchor: C++
`UsdAttribute::Get` of `asset ri:lut = @./lut.cube@`, authored in the
sublayer sub/settings.usda, resolves next to that sublayer, not the root
layer. `UsdRenderComputeNamespacedSettings` returns that value."""
import json
import sys
from pathlib import Path
from pxr import Usd

here = Path(__file__).resolve().parents[1] / "fixtures" / "render_assets"
stage = Usd.Stage.Open(str(here / "root.usda"))
attr = stage.GetAttributeAtPath("/Render/Settings.ri:lut")
value = attr.Get()
layer = attr.GetPropertyStack()[0].layer
result = {
    "version": ".".join(map(str, Usd.GetVersion()[1:])),
    "setting": "ri:lut",
    "authored_path": value.path,
    "resolved_path": str(Path(value.resolvedPath).relative_to(here)),
    "authoring_layer": str(Path(layer.realPath).relative_to(here)),
}
text = json.dumps(result, indent=2) + "\n"
dest = here / "oracle.json"
if "--check" in sys.argv:
    assert dest.read_text() == text, "render assets oracle differs"
else:
    dest.write_text(text)
