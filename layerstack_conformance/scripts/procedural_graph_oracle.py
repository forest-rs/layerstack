# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Reopen the procedural_graph example's authored world with native OpenUSD.

Usage: python procedural_graph_oracle.py path/to/procedural_world.usda
The build graph is application behavior; these checks verify the USD geometry
and placements produced after its resource-driven rebuild.
"""
import sys
from pxr import Usd, UsdGeom

stage = Usd.Stage.Open(sys.argv[1])
assert stage, sys.argv[1]
terrain = UsdGeom.Mesh(stage.GetPrimAtPath("/Assets/Terrain"))
assert terrain
assert list(terrain.GetFaceVertexCountsAttr().Get()) == [4]
assert list(terrain.GetFaceVertexIndicesAttr().Get()) == [0, 1, 2, 3]
assert terrain.GetSubdivisionSchemeAttr().Get() == "none"
assert [tuple(p) for p in terrain.GetPointsAttr().Get()] == [
    (0, 2, 0), (1, 2, 0), (1, 2, 1), (0, 2, 1)
]
scatter = UsdGeom.PointInstancer(stage.GetPrimAtPath("/World/Scatter"))
assert scatter
assert [str(p) for p in scatter.GetPrototypesRel().GetTargets()] == ["/Assets/Terrain"]
assert list(scatter.GetProtoIndicesAttr().Get()) == [0, 0, 0, 0]
positions = [(0, 2, 0), (3, 2, 0), (3, 2, 3), (0, 2, 3)]
assert [tuple(p) for p in scatter.GetPositionsAttr().Get()] == positions
transforms = scatter.ComputeInstanceTransformsAtTime(
    Usd.TimeCode.Default(), Usd.TimeCode.Default()
)
assert [tuple(m.ExtractTranslation()) for m in transforms] == positions
print("Procedural terrain and scatter agree with native OpenUSD")
