# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record native authored layer-stack flatten output (usd-core 26.8)."""
import argparse
from pathlib import Path
from pxr import Usd, UsdUtils

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
fixture = Path(__file__).resolve().parent.parent / "fixtures" / "layer_stack_flatten"
stage = Usd.Stage.Open(str(fixture / "root.usda"))
flat = UsdUtils.FlattenLayerStack(stage)
text = flat.ExportToString().replace(str(fixture), "__FIXTURE__")
# Normalize native formatting's trailing whitespace in this fixed fixture.
text = "\n".join(line.rstrip() for line in text.splitlines()).rstrip() + "\n"
snapshot = fixture / "native_flattened.usda"
if args.check:
    if snapshot.read_text() != text:
        raise SystemExit(f"native snapshot differs: {snapshot}")
else:
    snapshot.write_text(text)
