# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record native metadata and ColorAPI precedence; run with usd-core 26.8."""
import json
from pathlib import Path
from pxr import Plug, Usd

fixture = Path(__file__).resolve().parent.parent / "fixtures" / "schema_metadata"
Plug.Registry().RegisterPlugins(str(fixture / "plugInfo.json"))
stage = Usd.Stage.Open(str(fixture / "scene.usda"))
rows = []
for prim in stage.Traverse():
    if prim.GetTypeName() != "MetadataColor":
        continue
    for attr in prim.GetAttributes():
        name = attr.GetName()
        definition = prim.GetPrimDefinition()
        rows.append({
            "prim": str(prim.GetPath()), "property": name,
            "metadata_color_space": attr.GetMetadata("colorSpace"),
            "effective_color_space": str(Usd.ColorSpaceAPI.ComputeColorSpaceName(attr, None)),
            "schema_color_space": definition.GetPropertyMetadata(name, "colorSpace"),
            "settings": attr.GetMetadata("settings"),
            "schema_settings": definition.GetPropertyMetadata(name, "settings"),
            "display_name": attr.GetMetadata("displayName"),
            "display_group": attr.GetMetadata("displayGroup"),
            "documentation": definition.GetPropertyMetadata(name, "documentation"),
            "custom_data": definition.GetPropertyMetadata(name, "customData"),
        })
(fixture / "oracle.json").write_text(json.dumps({"openusd_version":list(Usd.GetVersion()), "properties":rows},indent=2)+"\n")
