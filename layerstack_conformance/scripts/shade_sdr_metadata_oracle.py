# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's `sdrMetadata` API on shaders, inputs and outputs.

Usage: shade_sdr_metadata_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

Reads: on `fixtures/shade_sdr_metadata/scene.usda`, for each shader and
each of its inputs and outputs, `GetSdrMetadata` (every value stringified
as `TfStringify` spells it: numbers, booleans, assets, halves, time
codes, vectors, quaternions, matrices, path expressions, arrays and nested
dictionaries), `HasSdrMetadata`, and `GetSdrMetadataByKey` and
`HasSdrMetadataByKey` for a set of keys: nested, missing, the empty key,
and keys with empty parts, which name entries named `""`. The same for
`listops.usdc`, which this script writes: list ops in `sdrMetadata`, which
only the binary format holds (`--check` does not rewrite it).

Authoring: on a new stage with the shader `/Look/S` and its input
`inputs:x` and output `outputs:out`, it calls `SetSdrMetadataByKey`
(flat, nested, and with empty key parts), `SetSdrMetadata`,
`ClearSdrMetadataByKey` (nested, leaving an empty dictionary, missing,
with empty key parts, and the empty key, which clears the field) and
`ClearSdrMetadata` one at a time, recording the root layer's
`sdrMetadata` of the prim and of each property afterwards. The setters'
empty key is left out: C++ replaces the whole dictionary with a string.

`tests/conformance/shade_sdr_metadata.rs` compares everything.
"""
import argparse
import json
import pathlib

from pxr import Sdf, Usd, UsdShade

HERE = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "shade_sdr_metadata"
KEYS = ["role", "nested", "nested:a", "nested:n", "nested:missing", "missing", "", "page:name",
        "label", "renderType", "arr", "a", "a:", "x", "x:", "x::", "x::y", ":", ":b", "tok"]


def record(api):
    return {
        "map": dict(sorted(api.GetSdrMetadata().items())),
        "has": api.HasSdrMetadata(),
        "by_key": {key: api.GetSdrMetadataByKey(key) for key in KEYS},
        "has_by_key": {key: api.HasSdrMetadataByKey(key) for key in KEYS},
    }


def write_list_ops():
    layer = Sdf.Layer.CreateNew(str(HERE / "listops.usdc"))
    spec = Sdf.CreatePrimInLayer(layer, "/Ops")
    spec.specifier = Sdf.SpecifierDef
    spec.typeName = "Shader"
    tokens = Sdf.TokenListOp()
    tokens.prependedItems = ["a", "b"]
    tokens.deletedItems = ["c"]
    strings = Sdf.StringListOp()
    strings.appendedItems = ["x y"]
    spec.SetInfo("sdrMetadata", {
        "tok": tokens,
        "int": Sdf.IntListOp.CreateExplicit([1, 2]),
        "empty": Sdf.StringListOp.CreateExplicit([]),
        "str": strings,
        "u64": Sdf.UInt64ListOp.Create(prependedItems=[4]),
        "i64": Sdf.Int64ListOp.Create(appendedItems=[3]),
        "uint": Sdf.UIntListOp.Create(deletedItems=[5]),
    })
    layer.Save()


def reads():
    found = {}
    stage = Usd.Stage.Open(str(HERE / "listops.usdc"))
    found["/Ops"] = record(UsdShade.Shader.Get(stage, "/Ops"))
    stage = Usd.Stage.Open(str(HERE / "scene.usda"))
    for path in ["/Look/Surface", "/Look/Typed", "/Look/Bare", "/Look/Empty"]:
        shader = UsdShade.Shader.Get(stage, path)
        found[path] = record(shader)
        for port in shader.GetInputs() + shader.GetOutputs():
            found[str(port.GetAttr().GetPath())] = record(port)
    return found


def dictionaries(layer):
    found = {}
    prim = layer.GetPrimAtPath("/Look/S")
    if prim.HasInfo("sdrMetadata"):
        found[str(prim.path)] = prim.GetInfo("sdrMetadata")
    for prop in prim.properties:
        if prop.HasInfo("sdrMetadata"):
            found[str(prop.path)] = prop.GetInfo("sdrMetadata")
    return found


def calls():
    stage = Usd.Stage.CreateInMemory()
    layer = stage.GetRootLayer()
    UsdShade.Material.Define(stage, "/Look")
    shader = UsdShade.Shader.Define(stage, "/Look/S")
    tint = shader.CreateInput("x", Sdf.ValueTypeNames.Float)
    out = shader.CreateOutput("out", Sdf.ValueTypeNames.Token)
    steps = [
        ("shader set role", lambda: shader.SetSdrMetadataByKey("role", "surface")),
        ("shader set nested", lambda: shader.SetSdrMetadataByKey("page:name", "Base")),
        ("shader set map", lambda: shader.SetSdrMetadata({"role": "texture", "page:open": "1"})),
        ("shader clear nested", lambda: shader.ClearSdrMetadataByKey("page:name")),
        ("shader clear last nested", lambda: shader.ClearSdrMetadataByKey("page:open")),
        ("shader clear missing", lambda: shader.ClearSdrMetadataByKey("missing")),
        ("shader set a:", lambda: shader.SetSdrMetadataByKey("a:", "e")),
        ("shader set x::y", lambda: shader.SetSdrMetadataByKey("x::y", "v")),
        ("shader set :b", lambda: shader.SetSdrMetadataByKey(":b", "w")),
        ("shader set map with empty parts",
         lambda: shader.SetSdrMetadata({":": "z", "page:": "p"})),
        ("shader clear x::y", lambda: shader.ClearSdrMetadataByKey("x::y")),
        ("shader clear :", lambda: shader.ClearSdrMetadataByKey(":")),
        ("shader clear a:", lambda: shader.ClearSdrMetadataByKey("a:")),
        ("input set label", lambda: tint.SetSdrMetadataByKey("label", "Tint")),
        ("input set map", lambda: tint.SetSdrMetadata({"help": "A tint", "widget:kind": "color"})),
        ("output set", lambda: out.SetSdrMetadataByKey("renderType", "terminal")),
        ("input clear widget", lambda: tint.ClearSdrMetadataByKey("widget:kind")),
        ("output clear", lambda: out.ClearSdrMetadata()),
        ("shader clear", lambda: shader.ClearSdrMetadata()),
        ("input clear label", lambda: tint.ClearSdrMetadataByKey("label")),
        ("input clear help", lambda: tint.ClearSdrMetadataByKey("help")),
        ("input set :k", lambda: tint.SetSdrMetadataByKey(":k", "1")),
        ("shader clear empty key", lambda: shader.ClearSdrMetadataByKey("")),
        ("input clear empty key", lambda: tint.ClearSdrMetadataByKey("")),
    ]
    log = []
    for label, step in steps:
        step()
        log.append({"call": label, "dictionaries": dictionaries(layer)})
    return log


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if not args.check:
        write_list_ops()
    result = {
        "openusd_version": ".".join(map(str, Usd.GetVersion()[1:])),
        "reads": reads(),
        "calls": calls(),
    }
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    output = HERE / "oracle.json"
    if args.check:
        assert output.read_text() == text, "oracle.json is out of date"
    else:
        output.write_text(text)


main()
