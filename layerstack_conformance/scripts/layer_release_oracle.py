# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records when OpenUSD frees a layer the stage stops using, and that it
reads the file again when the stage next asks for the layer.

Usage: layer_release_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/layer_release` by default,
the layers below and `oracle.json`, which
`tests/conformance/layer_release.rs` replays against layerstack.

Each case copies the layers into a directory of its own, opens a stage on
`root.usda`, and takes a fixed sequence of steps: drop the stage's last use
of a layer (unload `/P`'s payload, mute `sub.usda`, remove `extra.usda` from
the root's sublayers, clear `/R`'s reference), rewrite the layer's file,
then use it again (load, unmute, append the sublayer, add the reference).
The `_dirty` cases edit the layer in memory before the stage stops using
it, and leave its file as it is. After each step it records the composed
values of the attributes the layers author (`null` for none), whether each
layer is open (`SdfLayer::Find`, after a garbage collection) and whether
open layers are dirty, and the `UsdNotice::ObjectsChanged` paths the step
sent, with `GetChangedFields` of the info-only ones.

What it shows, with the 26.11 code that does it: a stage holds its layer
stacks through its prim indexes (`pxr/usd/pcp/primIndex_Graph.h`), a layer
stack holds its layers (`pxr/usd/pcp/layerStack.h`), and the registries
hold both weakly (`pxr/usd/pcp/layerStackRegistry.cpp`,
`pxr/usd/sdf/layerRegistry.h`). So once the stage stops using a layer it
is freed, with any unsaved edits, and using it again opens the file anew:
the rewritten values compose, and the edits of the `_dirty` cases are lost.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixture.
"""
import argparse
import filecmp
import gc
import json
import os
import shutil
import tempfile

from pxr import Sdf, Tf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "layer_release"))

LAYERS = {
    "root.usda": '''#usda 1.0
(
    subLayers = [
        @sub.usda@,
        @extra.usda@
    ]
)

def "A"
{
    double x = 1
}

def "P" (
    payload = @payload.usda@</P>
)
{
}

def "R" (
    prepend references = @ref.usda@</R>
)
{
}
''',
    "sub.usda": '''#usda 1.0

def "B"
{
    double y = 2
}

def "S"
{
    double z = 3
}
''',
    "extra.usda": '''#usda 1.0

over "A"
{
    double e = 7
}
''',
    "payload.usda": '''#usda 1.0
(
    subLayers = [
        @payload_sub.usda@
    ]
)

def "P"
{
    double p = 4
}
''',
    "payload_sub.usda": '''#usda 1.0

over "P"
{
    double q = 5
}
''',
    "ref.usda": '''#usda 1.0

def "R"
{
    double r = 6
}
''',
}

# What each case writes over a layer's file while the stage does not use it.
REWRITES = {
    "payload.usda": LAYERS["payload.usda"].replace("p = 4", "p = 44"),
    "payload_sub.usda": LAYERS["payload_sub.usda"].replace("q = 5", "q = 55"),
    "sub.usda": '''#usda 1.0

def "B"
{
    double y = 20
}

def "T"
{
    double w = 8
}
''',
    "extra.usda": LAYERS["extra.usda"].replace("e = 7", "e = 70"),
    "ref.usda": LAYERS["ref.usda"].replace("r = 6", "r = 60"),
}

# The attributes recorded after each step.
ATTRIBUTES = ["/A.x", "/A.e", "/B.y", "/S.z", "/T.w", "/P.p", "/P.q", "/R.r"]

# Each case: its name, the layer it edits in memory first (`None` for
# none) with the attribute and value, the step that drops the stage's use,
# the layers it rewrites, and the step that uses them again.
CASES = [
    ("unload", None, ["unload", "/P"], ["payload.usda", "payload_sub.usda"], ["load", "/P"]),
    ("unload_dirty", ("payload.usda", "/P.p", 40.0), ["unload", "/P"], [], ["load", "/P"]),
    ("mute", None, ["mute", "sub.usda"], ["sub.usda"], ["unmute", "sub.usda"]),
    ("mute_dirty", ("sub.usda", "/B.y", 300.0), ["mute", "sub.usda"], [], ["unmute", "sub.usda"]),
    ("sublayer", None, ["remove_sublayer", "extra.usda"], ["extra.usda"],
     ["append_sublayer", "extra.usda"]),
    ("sublayer_dirty", ("extra.usda", "/A.e", 700.0), ["remove_sublayer", "extra.usda"], [],
     ["append_sublayer", "extra.usda"]),
    ("reference", None, ["clear_references", "/R"], ["ref.usda"],
     ["add_reference", "/R", "ref.usda", "/R"]),
    ("reference_dirty", ("ref.usda", "/R.r", 600.0), ["clear_references", "/R"], [],
     ["add_reference", "/R", "ref.usda", "/R"]),
]


class Listener:
    """The paths of the `ObjectsChanged` notices a stage sends, with the
    changed fields of the info-only ones."""

    def __init__(self, stage):
        self.heard = []
        self.key = Tf.Notice.Register(Usd.Notice.ObjectsChanged, self.on, stage)

    def on(self, notice, sender):
        info = sorted(str(p) for p in notice.GetChangedInfoOnlyPaths())
        self.heard.append({
            "resynced": sorted(str(p) for p in notice.GetResyncedPaths()),
            "info": info,
            # `GetChangedFields` of each info-only path.
            "fields": {p: sorted(notice.GetChangedFields(p)) for p in info},
        })

    def take(self):
        heard, self.heard = self.heard, []
        return heard


def value(stage, path):
    attribute = stage.GetAttributeAtPath(path)
    if not attribute:
        return None
    found = attribute.Get()
    return None if found is None else float(found)


def layers_state(directory):
    """Each fixture layer: `freed`, `open` or `open dirty`."""
    gc.collect()
    state = {}
    for name in sorted(LAYERS):
        layer = Sdf.Layer.Find(os.path.join(directory, name))
        if layer is None:
            state[name] = "freed"
        else:
            state[name] = "open dirty" if layer.dirty else "open"
        del layer
    return state


def take_step(stage, directory, step):
    kind = step[0]
    if kind == "unload":
        stage.Unload(step[1])
    elif kind == "load":
        stage.Load(step[1])
    elif kind == "mute":
        stage.MuteLayer(os.path.join(directory, step[1]))
    elif kind == "unmute":
        stage.UnmuteLayer(os.path.join(directory, step[1]))
    elif kind == "remove_sublayer":
        paths = stage.GetRootLayer().subLayerPaths
        paths.remove(step[1])
    elif kind == "append_sublayer":
        stage.GetRootLayer().subLayerPaths.append(step[1])
    elif kind == "clear_references":
        stage.GetPrimAtPath(step[1]).GetReferences().ClearReferences()
    elif kind == "add_reference":
        stage.GetPrimAtPath(step[1]).GetReferences().AddReference(step[2], step[3])
    else:
        raise ValueError(kind)


def record(stage, directory, step, listener):
    return {
        "step": step,
        "values": {path: value(stage, path) for path in ATTRIBUTES},
        "layers": layers_state(directory),
        "heard": listener.take(),
    }


def case_record(out, name, dirty, drop, rewrites, use):
    directory = os.path.join(out, "work", name)
    shutil.copytree(os.path.join(out, "layers"), directory)
    directory = os.path.realpath(directory)
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    listener = Listener(stage)
    steps = [record(stage, directory, ["open"], listener)]
    if dirty is not None:
        layer_name, path, new = dirty
        layer = Sdf.Layer.Find(os.path.join(directory, layer_name))
        layer.GetAttributeAtPath(path).default = new
        del layer
        steps.append(record(stage, directory, ["edit", layer_name, path, new], listener))
    take_step(stage, directory, drop)
    steps.append(record(stage, directory, drop, listener))
    for layer_name in rewrites:
        with open(os.path.join(directory, layer_name), "w") as f:
            f.write(REWRITES[layer_name])
    if rewrites:
        steps.append(record(stage, directory, ["rewrite"] + rewrites, listener))
    take_step(stage, directory, use)
    steps.append(record(stage, directory, use, listener))
    del listener
    del stage
    gc.collect()
    return {"name": name, "steps": steps}


def write(out):
    for name, text in LAYERS.items():
        path = os.path.join(out, "layers", name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
    work = os.path.join(out, "work")
    os.makedirs(work, exist_ok=True)
    _, minor, patch = Usd.GetVersion()
    try:
        cases = [case_record(out, *case) for case in CASES]
    finally:
        shutil.rmtree(work)
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "rewrites": REWRITES,
        "cases": cases,
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
        for name in [os.path.join("layers", n) for n in LAYERS] + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
