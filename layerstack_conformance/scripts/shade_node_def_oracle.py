# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""`UsdShadeNodeDefAPI` source queries and setters, call by call.

Reads: `GetImplementationSource`, `GetShaderId`, `GetSourceAsset`,
`GetSourceAssetSubIdentifier`, `GetSourceCode` and `GetSourceTypes` on
scene.usda, including the universal fallback and an attribute that exists
without a value. Authoring: each setter on a fresh in-memory stage's root
layer, recording whether C++ accepts it and every attribute spec of the
prim afterwards; `SetShaderId` writes `info:implementationSource` sparsely
and corrects an authored `sourceAsset`, which `CreateIdAttr().Set` does not.
"""
import json
import sys
from pathlib import Path
from pxr import Sdf, Usd, UsdLux, UsdShade

here = Path(__file__).resolve().parents[1] / "fixtures" / "shade_node_def"


def text(value):
    if value is None:
        return None
    if isinstance(value, Sdf.AssetPath):
        return value.path
    return str(value)


def reads(stage, paths):
    result = []
    for path in paths:
        node_def = UsdShade.NodeDefAPI(stage.GetPrimAtPath(path))
        row = {
            "path": path,
            "effective": str(node_def.GetImplementationSource()),
            "shader_id": text(node_def.GetShaderId()),
            "source_types": [str(t) for t in node_def.GetSourceTypes()],
        }
        for source_type in ["", "osl", "mdl", "glslfx", "ri"]:
            row["source_asset:" + source_type] = text(node_def.GetSourceAsset(source_type))
            row["subidentifier:" + source_type] = text(
                node_def.GetSourceAssetSubIdentifier(source_type))
            row["source_code:" + source_type] = text(node_def.GetSourceCode(source_type))
        result.append(row)
    return result


scene = Usd.Stage.Open(str(here / "scene.usda"))
scene_reads = reads(scene, ["/Asset", "/Code", "/Unknown", "/Plain", "/Light"])

stage = Usd.Stage.CreateInMemory()
layer = stage.GetRootLayer()
UsdShade.Material.Define(stage, "/Mat")
for name in ["S", "S3", "S4", "S5", "S6", "S7"]:
    UsdShade.Shader.Define(stage, "/Mat/" + name)
stage.DefinePrim("/Mat/U")
UsdLux.PluginLight.Define(stage, "/Light")
for name in ["S3", "S7"]:
    UsdShade.Shader.Get(stage, "/Mat/" + name).GetImplementationSourceAttr().Set("sourceAsset")


def specs(prim_path):
    prim = layer.GetPrimAtPath(prim_path)
    result = {}
    for attr in prim.attributes if prim else []:
        result[attr.name] = {
            "type": str(attr.typeName),
            "custom": attr.custom,
            "uniform": attr.variability == Sdf.VariabilityUniform,
            "default": text(attr.default) if attr.HasDefaultValue() else None,
        }
    return result


def node_def(path):
    return UsdShade.NodeDefAPI(stage.GetPrimAtPath(path))


calls = [
    ("set_shader_id", "/Mat/S", "UsdPreviewSurface", "",
     lambda: node_def("/Mat/S").SetShaderId("UsdPreviewSurface")),
    ("set_shader_id", "/Mat/U", "UsdPreviewSurface", "",
     lambda: node_def("/Mat/U").SetShaderId("UsdPreviewSurface")),
    ("set_shader_id", "/Mat/S3", "UsdPreviewSurface", "",
     lambda: node_def("/Mat/S3").SetShaderId("UsdPreviewSurface")),
    ("set_id", "/Mat/S7", "UsdPreviewSurface", "",
     lambda: node_def("/Mat/S7").CreateIdAttr().Set("UsdPreviewSurface")),
    ("set_source_asset", "/Mat/S4", "a.osl", "osl",
     lambda: node_def("/Mat/S4").SetSourceAsset(Sdf.AssetPath("a.osl"), "osl")),
    ("set_source_asset", "/Mat/S4", "u.osl", "",
     lambda: node_def("/Mat/S4").SetSourceAsset(Sdf.AssetPath("u.osl"))),
    ("set_source_code", "/Mat/S5", "void main(){}", "glslfx",
     lambda: node_def("/Mat/S5").SetSourceCode("void main(){}", "glslfx")),
    ("set_source_asset_subidentifier", "/Mat/S6", "Sub", "mdl",
     lambda: node_def("/Mat/S6").SetSourceAssetSubIdentifier("Sub", "mdl")),
    ("set_shader_id", "/Light", "CustomLight", "",
     lambda: UsdLux.PluginLight(stage.GetPrimAtPath("/Light")).GetNodeDefAPI().SetShaderId("CustomLight")),
]
records = []
for call, prim, value, source_type, run in calls:
    try:
        accepted = bool(run())
    except Exception:
        accepted = False
    records.append({"call": call, "prim": prim, "value": value, "source_type": source_type,
                    "accepted": accepted, "specs": specs(prim)})
edit_reads = reads(stage, ["/Mat/S", "/Mat/S3", "/Mat/S4", "/Mat/S5", "/Mat/S6", "/Mat/S7", "/Light"])
result = {
    "version": ".".join(map(str, Usd.GetVersion()[1:])),
    "scene_reads": scene_reads,
    "calls": records,
    "edit_reads": edit_reads,
}
text_out = json.dumps(result, indent=2) + "\n"
dest = here / "oracle.json"
if "--check" in sys.argv:
    assert dest.read_text() == text_out, "node def oracle differs"
else:
    dest.write_text(text_out)
