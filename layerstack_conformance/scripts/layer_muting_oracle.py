# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's layer muting by identifier and its namespace edits
around muted layers, call by call.

Usage: layer_muting_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/layer_muting` by default,
the layers below and `oracle.json`, which
`tests/conformance/layer_muting.rs` replays against layerstack.

`calls` is one stage on `calls/scene.usda` taking a fixed sequence of
`UsdStage::MuteLayer`, `UnmuteLayer`, `MuteAndUnmuteLayers` and
`IsLayerMuted` calls, and two edits that add references to layers muted
before anything opened them. After each call it records what the call
returned or raised, `GetMutedLayers`, the `UsdNotice::LayerMutingChanged`
the call sent (`null` for none), the composed property names of the
prims, the `PcpErrorMutedAssetPath` errors, and which of the fixture's
layers are open. Identifiers are relative to the fixture directory.
What it shows, with the 26.11 code that does it: a muted identifier is
anchored to the root layer (`Pcp_MutedLayers::_GetCanonicalLayerId`,
`pxr/usd/pcp/layerStackRegistry.cpp`), may name a layer not opened yet,
and a later arc to it, anchored to the layer authoring it, composes
muted without opening it; an unmute request repeating a mute request's
spelling is dropped, and the root cannot be muted
(`PcpCache::RequestLayerMuting`, `pxr/usd/pcp/cache.cpp`).

`namespace` opens `namespace/scene.usda` afresh per case, mutes the
case's layers, then queues one `UsdNamespaceEditor` move or delete
through the root layer and applies it, recording whether it could and
each layer's `ExportToString` afterwards. A muted layer is never edited:
its specs stay where they are, even over the moved prim's new path.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixture.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Tf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "layer_muting"))

CALL_LAYERS = {
    "calls/scene.usda": '''#usda 1.0
(
    subLayers = [
        @a.usda@,
        @b.usda@
    ]
)

def "Ball" (
    prepend references = @parts/ball.usda@</Ball>
)
{
}

def "Lamp"
{
}
''',
    "calls/a.usda": '''#usda 1.0

over "Ball"
{
    double a = 1
}
''',
    "calls/b.usda": '''#usda 1.0

over "Ball"
{
    double b = 1
}
''',
    "calls/lamp.usda": '''#usda 1.0

def "Lamp"
{
    double glow = 1
}
''',
    "calls/parts/ball.usda": '''#usda 1.0

def "Ball"
{
    double points = 1
}
''',
    "calls/parts/robot.usda": '''#usda 1.0

def "Robot" (
    prepend references = @./ball.usda@</Ball>
)
{
}
''',
}

NAMESPACE_LAYERS = {
    "namespace/scene.usda": '''#usda 1.0
(
    subLayers = [
        @sub.usda@,
        @other.usda@
    ]
)

def "World"
{
    def "Lamp"
    {
        float intensity = 1

        def "Bulb"
        {
        }
    }

    def "Shelf" (
        prepend references = @asset.usda@</Asset>
    )
    {
    }

    def "Switch"
    {
        rel lamp = </World/Lamp>
    }
}
''',
    "namespace/sub.usda": '''#usda 1.0

over "World"
{
    over "Lamp"
    {
        float intensity = 2
        rel target = </World/Lamp/Bulb>
    }

    over "Switch"
    {
        rel other = </World/Lamp>
    }

    def "OnlySub"
    {
    }

    over "Light"
    {
        float glow = 3
    }
}
''',
    "namespace/other.usda": '''#usda 1.0

over "World"
{
    over "Unrelated"
    {
    }
}
''',
    "namespace/asset.usda": '''#usda 1.0

def "Asset"
{
    def "Front" (
        prepend references = @scene.usda@</World/Lamp>
    )
    {
        over "Bulb"
        {
        }
    }
}
''',
}

LAYERS = {**CALL_LAYERS, **NAMESPACE_LAYERS}

# Each call: [method, *arguments]. `reference` adds a reference to the
# prim, defining it first; `mute_and_unmute` takes two lists.
CALLS = [
    ["is_muted", "lamp.usda"],
    ["mute", "lamp.usda"],
    ["is_muted", "./lamp.usda"],
    ["is_muted", "parts/../lamp.usda"],
    ["reference", "/Lamp", "./lamp.usda", "/Lamp"],
    ["unmute", "./lamp.usda"],
    ["mute", "parts/ball.usda"],
    ["reference", "/Robot", "parts/robot.usda", "/Robot"],
    ["is_muted", "parts/./ball.usda"],
    ["mute_and_unmute", ["a.usda"], ["a.usda"]],
    ["mute_and_unmute", ["b.usda"], ["./b.usda"]],
    ["mute_and_unmute", ["b.usda", "./b.usda"], []],
    ["mute_and_unmute", ["a.usda"], ["b.usda", "b.usda"]],
    ["mute_and_unmute", [], ["a.usda", "parts/ball.usda"]],
    ["mute", "scene.usda"],
    ["mute_and_unmute", ["./scene.usda", "b.usda"], []],
    ["unmute", "scene.usda"],
    ["mute", ""],
    ["unmute", "never.usda"],
    ["is_muted", "never.usda"],
]

PRIMS = ["/Ball", "/Lamp", "/Robot"]

NAMESPACE_CASES = [
    # [name, muted, path, destination (None deletes)]
    ["muted sublayer, move", ["sub.usda"], "/World/Lamp", "/World/Light2"],
    ["muted sublayer, move onto its over", ["sub.usda"], "/World/Lamp", "/World/Light"],
    ["muted sublayer, delete", ["sub.usda"], "/World/Lamp", None],
    ["muted sublayer, its own prim", ["sub.usda"], "/World/OnlySub", "/World/Moved"],
    ["muted asset, move", ["asset.usda"], "/World/Lamp", "/World/Light2"],
    ["muted asset, delete", ["asset.usda"], "/World/Lamp", None],
    ["muted asset, move its referencing prim", ["asset.usda"], "/World/Shelf", "/World/Rack"],
    ["unrelated muted layer, move", ["other.usda"], "/World/Lamp", "/World/Light2"],
    ["unrelated muted layer, delete", ["other.usda"], "/World/Lamp", None],
]


def reload_layers():
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)


def relative(directory, identifier):
    prefix = directory + os.sep
    return identifier[len(prefix):].replace(os.sep, "/") if identifier.startswith(prefix) else identifier


def calls_record(out):
    directory = os.path.join(out, "calls")
    reload_layers()
    stage = Usd.Stage.Open(os.path.join(directory, "scene.usda"))
    heard = []

    def muting(notice, sender):
        heard.append([[relative(directory, i) for i in notice.GetMutedLayers()],
                      [relative(directory, i) for i in notice.GetUnmutedLayers()]])

    key = Tf.Notice.Register(Usd.Notice.LayerMutingChanged, muting, stage)
    steps = []
    for call in CALLS:
        heard.clear()
        method = call[0]
        result = None
        try:
            if method == "is_muted":
                result = stage.IsLayerMuted(call[1])
            elif method == "mute":
                stage.MuteLayer(call[1])
            elif method == "unmute":
                stage.UnmuteLayer(call[1])
            elif method == "mute_and_unmute":
                stage.MuteAndUnmuteLayers(call[1], call[2])
            elif method == "reference":
                prim = stage.DefinePrim(call[1])
                assert prim.GetReferences().AddReference(call[2], Sdf.Path(call[3]))
            raised = False
        except Tf.ErrorException:
            raised = True
        errors = muted_errors(stage, directory)
        steps.append({
            "call": call,
            "result": result,
            "raised": raised,
            "muted": [relative(directory, i) for i in stage.GetMutedLayers()],
            "heard": heard[0] if heard else None,
            "properties": {
                path: sorted(p.GetName() for p in stage.GetPrimAtPath(path).GetProperties())
                if stage.GetPrimAtPath(path) else None
                for path in PRIMS
            },
            "muted_assets": errors,
            "open": sorted(name[len("calls/"):] for name in CALL_LAYERS
                           if Sdf.Layer.Find(os.path.join(out, name)) is not None),
        })
    del key
    return steps


def muted_errors(stage, directory):
    """`[prim, muted asset, introducing layer]` of each muted-asset error,
    sorted, read from the errors' text: Python does not wrap their type."""
    rows = []
    for error in stage.GetCompositionErrors():
        text = str(error)
        if " was muted for " not in text:
            continue
        asset = text.split("@")[1]
        introducing = text.split("introduced by @")[1].split("@")[0]
        prim = text.split("@<")[-1].split(">")[0]
        rows.append([prim, relative(directory, asset), relative(directory, introducing)])
    return sorted(rows)


def namespace_record(out):
    directory = os.path.join(out, "namespace")
    names = [name[len("namespace/"):] for name in NAMESPACE_LAYERS]
    cases = []
    for name, muted, path, destination in NAMESPACE_CASES:
        reload_layers()
        stage = Usd.Stage.Open(os.path.join(directory, "scene.usda"))
        held = {n: Sdf.Layer.FindOrOpen(os.path.join(directory, n)) for n in names}
        for layer in muted:
            stage.MuteLayer(layer)
        editor = Usd.NamespaceEditor(stage)
        if destination is None:
            assert editor.DeletePrimAtPath(Sdf.Path(path))
        else:
            assert editor.MovePrimAtPath(Sdf.Path(path), Sdf.Path(destination))
        can = editor.CanApplyEdits()
        applied = bool(can) and editor.ApplyEdits()
        cases.append({
            "name": name,
            "muted": muted,
            "path": path,
            "destination": destination,
            "can_apply": bool(can),
            "why_not": None if can else can.whyNot,
            "applied": applied,
            "layers": {n: held[n].ExportToString() for n in names},
            "muted_assets": muted_errors(stage, directory),
        })
    reload_layers()
    return cases


def write(out):
    for name, text in LAYERS.items():
        path = os.path.join(out, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "calls": calls_record(out),
        "namespace": namespace_record(out),
    }
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        tmp = os.path.realpath(tmp)
        write(tmp)
        for name in list(LAYERS) + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
