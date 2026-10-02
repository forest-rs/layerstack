# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record collision policy with OpenUSD 26.8; run from the repository root."""
import json
from pathlib import Path
from pxr import Usd, UsdPhysics
fixtures = Path("layerstack_conformance/fixtures")
assert Usd.GetVersion() == (0, 26, 8)
stage = Usd.Stage.Open(str(fixtures / "collision_groups.usda"))
table = UsdPhysics.CollisionGroup.ComputeCollisionGroupTable(stage)
groups = table.GetGroups()
result = {"groups": list(map(str, groups)), "enabled": [[table.IsCollisionEnabled(a, b) for b in groups] for a in groups]}
(fixtures / "collision_groups.json").write_text(json.dumps(result, indent=2) + "\n")
