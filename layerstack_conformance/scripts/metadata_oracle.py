# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Plugin metadata semantics from the pinned OpenUSD 26.8 runtime."""
import json
import sys
from pathlib import Path
from pxr import Sdf, Usd, UsdGeom, UsdShade, UsdPhysics

assert Usd.GetVersion() == (0, 26, 8)
weak = Sdf.Layer.CreateAnonymous()
root = Sdf.Layer.CreateAnonymous()
root.subLayerPaths = [weak.identifier]
stage = Usd.Stage.Open(root)
mesh = UsdGeom.Mesh.Define(stage, "/Mesh")
attribute = mesh.CreateNormalsAttr()
relationship = mesh.GetPrim().CreateRelationship("binding")
keys = ["metersPerUnit", "upAxis", "kilogramsPerUnit", "renderSettingsPrimPath"]
result = {"version": "26.8", "stage_defaults": {key: stage.GetMetadata(key) for key in keys},
    "property_defaults": {key: attribute.GetMetadata(key) for key in ["interpolation", "elementSize", "unauthoredValuesIndex"]}}
weak.pseudoRoot.SetInfo("metersPerUnit", 10.0)
result["sublayer_does_not_override"] = stage.GetMetadata("metersPerUnit")
stage.SetMetadata("metersPerUnit", 0.5)
result["authored_stage"] = stage.GetMetadata("metersPerUnit")
stage.SetEditTarget(weak)
attribute.SetMetadata("interpolation", "vertex")
attribute.SetMetadata("sdrMetadata", {"weak": "yes", "nested": {"left": 1}})
mesh.GetPrim().SetMetadata("inactiveIds", Sdf.Int64ListOp.CreateExplicit([1, 2]))
stage.SetEditTarget(root)
attribute.SetMetadata("sdrMetadata", {"strong": "yes", "nested": {"right": 2}})
mesh.GetPrim().SetMetadata("inactiveIds", Sdf.Int64ListOp.Create(deletedItems=[1], appendedItems=[3]))
relationship.SetMetadata("renderType", "terminal")
result["interpolation"] = attribute.GetMetadata("interpolation")
result["dictionary"] = attribute.GetMetadata("sdrMetadata")
result["inactive_ids"] = list(mesh.GetPrim().GetMetadata("inactiveIds").GetAppliedItems())
result["relationship_render_type"] = relationship.GetMetadata("renderType")
Path(sys.argv[1]).write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
