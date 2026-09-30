# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Replay the C++ oracle tool built from render_spec_oracle.cpp.

Build the adjacent C++ source against OpenUSD 26.8's headers and libraries,
then run this script with --binary PATH. The computation is not exposed by
OpenUSD's Python bindings; this script only serializes the C++ result.
"""
import argparse
import json
import pathlib
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", required=True, type=pathlib.Path)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "render_spec"
output = subprocess.check_output([str(args.binary.resolve()), str(here / "scene.usda")], text=True)
result = json.loads(output)
text = json.dumps(result, indent=2, sort_keys=True) + "\n"
path = here / "oracle.json"
if args.check:
    assert json.loads(path.read_text()) == result, "render-spec oracle differs from OpenUSD"
else:
    path.write_text(text)
