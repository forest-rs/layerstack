# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD authors for sublayer and relocates edits, call by call.

Usage: layer_field_edits_oracle.py [FIXTURES [OUT]]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Each case opens `fixtures/layer_field_edits/scene.usda` afresh (it
sublayers `s1.usda`; `s2.usda` and `s3.usda` are loaded through
references, so they can be sublayered too) and edits the root layer's
`subLayers`, `subLayerOffsets` and `layerRelocates` through `SdfLayer`. For
each call it records whether OpenUSD accepted it. After the last call it
records the root layer's sublayer paths and offsets, its relocates, and
every prim the stage then composes (`TraverseAll`).

`tests/conformance/layer_field_edits.rs` makes the same edits through layerstack's
`Transaction` and must agree with every record.

Writes `fixtures/layer_field_edits/oracle.json` by default.
"""
import json
import os
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "layer_field_edits"))


def call(layer, step):
    kind = step["call"]
    if kind == "insert_sublayer":
        layer.subLayerPaths.insert(step["index"], step["asset"])
    elif kind == "remove_sublayer":
        if not 0 <= step["index"] < len(layer.subLayerPaths):
            raise IndexError(step["index"])
        del layer.subLayerPaths[step["index"]]
    elif kind == "set_sublayer_offset":
        if not 0 <= step["index"] < len(layer.subLayerPaths):
            raise IndexError(step["index"])
        layer.subLayerOffsets[step["index"]] = Sdf.LayerOffset(step["offset"], step["scale"])
    elif kind == "set_sublayers":
        layer.subLayerPaths = step["value"]
    elif kind == "set_relocates":
        layer.relocates = [
            (Sdf.Path(source), Sdf.Path(target) if target else Sdf.Path())
            for source, target in step["value"]
        ]
    elif kind == "clear_relocates":
        layer.ClearRelocates()
    else:
        raise ValueError(kind)


CASES = [
    ("sublayer_insert_and_remove", [
        {"call": "insert_sublayer", "asset": "s2.usda", "index": 0},
        {"call": "insert_sublayer", "asset": "s3.usda", "index": 2},
        {"call": "remove_sublayer", "index": 1},
    ]),
    ("sublayer_offsets", [
        {"call": "insert_sublayer", "asset": "s2.usda", "index": 1},
        {"call": "set_sublayer_offset", "index": 1, "offset": 10.0, "scale": 2.0},
        {"call": "set_sublayer_offset", "index": 0, "offset": -3.0, "scale": 1.0},
        {"call": "remove_sublayer", "index": 0},
    ]),
    ("sublayer_rejections", [
        {"call": "insert_sublayer", "asset": "s1.usda", "index": 0},
        {"call": "insert_sublayer", "asset": "", "index": 0},
        {"call": "remove_sublayer", "index": 4},
        {"call": "set_sublayer_offset", "index": 3, "offset": 1.0, "scale": 1.0},
        {"call": "set_sublayers", "value": ["s2.usda", "s2.usda"]},
    ]),
    ("sublayer_replace", [
        {"call": "set_sublayers", "value": ["s3.usda", "s2.usda"]},
        {"call": "set_sublayers", "value": []},
    ]),
    ("sublayer_self_cycle", [
        {"call": "insert_sublayer", "asset": "scene.usda", "index": 0},
    ]),
    ("relocates_move_and_remove", [
        {"call": "set_relocates", "value": [["/A/B", "/A/Moved"]]},
        {"call": "set_relocates", "value": [["/A/B", "/A/Moved"], ["/A/D", ""]]},
    ]),
    ("relocates_clear", [
        {"call": "set_relocates", "value": [["/A/B", "/A/Moved"]]},
        {"call": "clear_relocates"},
    ]),
    ("relocates_invalid_entries", [
        {"call": "set_relocates", "value": [["/A", "/X"]]},
        {"call": "set_relocates", "value": [["/A/B", "/A/B"]]},
        {"call": "set_relocates", "value": [["/A/B", "/A/B/C/E"]]},
        {"call": "set_relocates", "value": [["/A/B/C", "/A/B"]]},
        {"call": "set_relocates", "value": [["/A/B", "/A/X"], ["/A/B", "/A/Y"]]},
    ]),
]


def main():
    fixtures = sys.argv[1] if len(sys.argv) > 1 else FIXTURES
    out_path = sys.argv[2] if len(sys.argv) > 2 else os.path.join(fixtures, "oracle.json")
    cases = []
    for name, steps in CASES:
        for loaded in list(Sdf.Layer.GetLoadedLayers()):
            if not loaded.anonymous:
                loaded.Reload(force=True)
        stage = Usd.Stage.Open(os.path.join(fixtures, "scene.usda"))
        root = stage.GetRootLayer()
        accepted = []
        for step in steps:
            try:
                call(root, step)
                accepted.append(True)
            except Exception:
                accepted.append(False)
        cases.append({
            "name": name,
            "steps": steps,
            "accepted": accepted,
            "sublayers": [str(p) for p in root.subLayerPaths],
            "offsets": [[o.offset, o.scale] for o in root.subLayerOffsets],
            "relocates": [
                [str(s), "" if t.isEmpty else str(t)] for s, t in root.relocates
            ],
            "prims": [str(p.GetPath()) for p in stage.TraverseAll()],
        })
        for loaded in list(Sdf.Layer.GetLoadedLayers()):
            if not loaded.anonymous:
                loaded.Reload(force=True)

    _, minor, patch = Usd.GetVersion()
    with open(out_path, "w") as f:
        json.dump({"openusd_version": f"{minor}.{patch}", "cases": cases}, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
