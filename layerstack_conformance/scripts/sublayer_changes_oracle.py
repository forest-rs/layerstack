# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD announces and composes when a layer's sublayers
change: a sublayer inserted, removed, reordered, retimed, muted or
unmuted, in the stage's own layer stack or in the layer stack a reference
reaches.

Usage: sublayer_changes_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/sublayer_changes` by
default, the layers below and `oracle.json`, which
`tests/conformance/sublayer_changes.rs` replays against layerstack.

Each case copies the layers into a directory of its own, opens a stage on
`root.usda` and takes its steps, each an edit of one layer's
`subLayerPaths` or `subLayerOffsets`, or a mute or unmute. After each
step it records the composed values of the attributes the layers author
(`null` for none; `/W.t` at time 5, the rest at the default time), the
stage's prims, its layer stack, and the `UsdNotice::ObjectsChanged` paths
the step sent, with `GetChangedFields` of the info-only ones.

What it shows, with the 26.11 code that does it:
`Sdf_ChangeManager::DidChangeField` (`pxr/usd/sdf/changeManager.cpp`)
records a `subLayers` change as the sublayers one list names and the
other does not, added or removed, and a reorder, which changes neither
set, as the removal and addition of every sublayer of the list.
`PcpChanges::DidChange` (`pxr/usd/pcp/changes.cpp`) hands each to
`_DidAddOrRemoveSublayer`, which, for a sublayer that is open (or opens,
when added) and not empty, processes the difference of the sublayer and an
empty layer as an edit of the layer stacks holding the edited layer: each
prim spec of the sublayer added or removed, and its own sublayers too. A
spec that is not inert resyncs the prims composing it; an inert one (an
`over` with no fields of its own) changes their spec stack alone, and
`UsdStage::_HandleLayersDidChange` (`pxr/usd/usd/stage.cpp`) reports the
prim info-only and the property specs it holds resynced. The `subLayers`
change itself is an info change of `/` for a layer of the stage's own
layer stack, and none for the layer a reference reaches, whose prims
(`/R`) change as the root layer stack's do. An empty sublayer changes no
spec (`_DidChangeSublayer`). A sublayer offset changed is a significant
change of the layer stack (`Pcp_EntryRequiresLayerStackOffsetsChange`),
which resyncs `/`. Muting or unmuting a sublayer adds or removes its specs
the same way. A sublayer that is muted is opened anyway when it is
inserted (`_LoadSublayerForChange` does not ask), so its prims are
resynced though none composes; removing a muted sublayer the stage no
longer holds open finds no layer and changes no spec. An added sublayer's
specs are processed in the layer stacks holding the edited layer alone
(`_SetLayerStacksUsingLayerOverride`), but a removed one's in every layer
stack holding the removed layer, so removing a sublayer that `/R` also
references resyncs `/R`.

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
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "sublayer_changes"))

LAYERS = {
    "root.usda": '''#usda 1.0
(
    subLayers = [
        @strong.usda@,
        @weak.usda@
    ]
)

def "A"
{
    double x = 1
}

def "R" (
    prepend references = @asset.usda@</Asset>
)
{
}

def "Other"
{
    double v = 1
}
''',
    "strong.usda": '''#usda 1.0

over "A"
{
    double y = 2
}

def "S"
{
    double s = 1
}
''',
    "weak.usda": '''#usda 1.0

over "A"
{
    double y = 3
}

def "W"
{
    double t.timeSamples = {
        0: 0,
        10: 10,
    }
    double w = 1
}
''',
    # Defines new prims.
    "prims.usda": '''#usda 1.0

def "N"
{
    double n = 1

    def "C"
    {
    }
}
''',
    # Overrides the values of prims that are there already.
    "values.usda": '''#usda 1.0

over "W"
{
    double w = 5
}
''',
    # Adds a reference to a prim that is there already.
    "arc.usda": '''#usda 1.0

over "Other" (
    prepend references = @asset.usda@</Asset>
)
{
}
''',
    # Has a sublayer of its own, which defines a prim.
    "nested.usda": '''#usda 1.0
(
    subLayers = [
        @prims.usda@
    ]
)

over "W"
{
    double w = 6
}
''',
    "empty.usda": '''#usda 1.0
''',
    "asset.usda": '''#usda 1.0

def "Asset"
{
    double a = 1
}
''',
    # Inserted beneath `asset.usda`, the layer `/R` references.
    "asset_extra.usda": '''#usda 1.0

over "Asset"
{
    double a = 4
    double b = 2

    def "Extra"
    {
    }
}
''',
}

# The attributes recorded after each step; `/W.t` is read at time 5.
ATTRIBUTES = ["/A.x", "/A.y", "/S.s", "/W.t", "/W.w", "/N.n", "/Other.v", "/Other.a",
              "/R.a", "/R.b", "/Asset.a"]
SAMPLED = {"/W.t": 5.0}

# Each case: its name and its steps. A step edits a layer's sublayers
# (`insert` at an index, `remove` an index, `reorder` to the order of the
# indices given, `offset` an index to an offset and scale), or mutes or
# unmutes a layer, by the name of its file.
CASES = [
    ("prims", [["insert", "root.usda", 1, "prims.usda"], ["remove", "root.usda", 1]]),
    ("values", [["insert", "root.usda", 1, "values.usda"], ["remove", "root.usda", 1]]),
    ("arc", [["insert", "root.usda", 0, "arc.usda"], ["remove", "root.usda", 0]]),
    ("nested", [["insert", "root.usda", 0, "nested.usda"], ["remove", "root.usda", 0]]),
    ("empty", [["insert", "root.usda", 2, "empty.usda"], ["remove", "root.usda", 2]]),
    ("remove_existing", [["remove", "root.usda", 1], ["remove", "root.usda", 0]]),
    ("reorder", [["reorder", "root.usda", [1, 0]]]),
    ("offset", [["offset", "root.usda", 1, 10.0, 1.0], ["offset", "root.usda", 1, 0.0, 2.0],
                ["offset", "root.usda", 1, 0.0, 1.0]]),
    ("within_sublayer", [["insert", "weak.usda", 0, "prims.usda"],
                         ["remove", "weak.usda", 0]]),
    ("referenced_stack", [["insert", "asset.usda", 0, "asset_extra.usda"],
                          ["remove", "asset.usda", 0]]),
    # The layer `/R` references, inserted as a sublayer too, then reordered.
    ("referenced_as_sublayer", [["insert", "root.usda", 0, "asset.usda"],
                                ["reorder", "root.usda", [1, 0, 2]],
                                ["remove", "root.usda", 1]]),
    ("muted_insert", [["mute", "prims.usda"], ["insert", "root.usda", 1, "prims.usda"],
                      ["unmute", "prims.usda"], ["mute", "prims.usda"],
                      ["remove", "root.usda", 1]]),
    ("muted_existing", [["mute", "weak.usda"], ["remove", "root.usda", 1],
                        ["unmute", "weak.usda"]]),
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
    time = SAMPLED.get(path)
    found = attribute.Get() if time is None else attribute.Get(time)
    return None if found is None else float(found)


def layer_stack(stage, directory):
    return [os.path.relpath(layer.realPath, directory)
            for layer in stage.GetLayerStack(includeSessionLayers=False)]


def take_step(stage, directory, step):
    kind = step[0]
    path = lambda name: os.path.join(directory, name)
    if kind == "mute":
        stage.MuteLayer(path(step[1]))
        return
    if kind == "unmute":
        stage.UnmuteLayer(path(step[1]))
        return
    layer = Sdf.Layer.Find(path(step[1]))
    assert layer, step
    if kind == "insert":
        layer.subLayerPaths.insert(step[2], step[3])
    elif kind == "remove":
        del layer.subLayerPaths[step[2]]
    elif kind == "reorder":
        paths = list(layer.subLayerPaths)
        offsets = list(layer.subLayerOffsets)
        with Sdf.ChangeBlock():
            layer.subLayerPaths = [paths[i] for i in step[2]]
            for at, i in enumerate(step[2]):
                layer.subLayerOffsets[at] = offsets[i]
    elif kind == "offset":
        layer.subLayerOffsets[step[2]] = Sdf.LayerOffset(step[3], step[4])
    else:
        raise ValueError(kind)


def record(stage, directory, step, listener):
    return {
        "step": step,
        "values": {path: value(stage, path) for path in ATTRIBUTES},
        "prims": sorted(str(prim.GetPath()) for prim in stage.TraverseAll()),
        "layer_stack": layer_stack(stage, directory),
        "heard": listener.take(),
    }


def case_record(out, name, steps):
    directory = os.path.join(out, "work", name)
    shutil.copytree(os.path.join(out, "layers"), directory)
    directory = os.path.realpath(directory)
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    listener = Listener(stage)
    recorded = [record(stage, directory, ["open"], listener)]
    for step in steps:
        take_step(stage, directory, step)
        recorded.append(record(stage, directory, step, listener))
    del listener
    del stage
    gc.collect()
    return {"name": name, "steps": recorded}


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
        "sampled": SAMPLED,
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
