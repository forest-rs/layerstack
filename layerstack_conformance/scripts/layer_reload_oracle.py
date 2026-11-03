# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD reloads, composes and announces when a stage or
one of its layers is reloaded after the files change.

Usage: layer_reload_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/layer_reload` by default,
the layers below and `oracle.json`, which
`tests/conformance/layer_reload.rs` replays against layerstack.

Each case copies the layers into a directory of its own, sets the default
search path to that directory's `first/` and `second/`, opens a stage on
`root.usda`, and takes its steps: rewrite or create a file (a rewrite also
moves the file's modification time a second past the old one), edit a
layer in memory, mute or unmute a layer, and reload the stage
(`UsdStage::Reload`) or one layer (`SdfLayer::Reload`, with or without
force). After each step it records the composed values of the attributes
the layers author (`null` for none), the stage's prims, each file layer
the stage uses with whether it is dirty and where it resolves (relative to
the case's directory), the `UsdNotice::ObjectsChanged` paths the step
sent, whether OpenUSD raised an error, and what a layer reload returned.

What it shows, with the 26.11 code that does it:

- `UsdStage::Reload` (`pxr/usd/usd/stage.cpp`) refreshes the resolver
  context, has `PcpCache::Reload` (`pxr/usd/pcp/cache.cpp`) retry every
  sublayer and asset that failed to open, and reloads every layer the
  stage uses except its session layers in one change block, so a reload
  sends at most one notice, and none when nothing changed.
- `SdfLayer::_Reload` (`pxr/usd/sdf/layer.cpp`) re-resolves the layer's
  identifier first, then skips a clean layer whose resolved path and
  modification time are unchanged unless forced. A dirty layer always
  reloads and loses its edits; a search-path asset moves to the directory
  that now holds it. Reading the file replaces the layer's data with
  change entries for what differs, plus a `didReloadContent` entry at the
  layer's `/` (`SdfChangeList::DidReloadLayerContent`), which the stage
  hears as an info change at `/` for a layer of the root layer stack and
  at the referencing prim (`/R`, `/Q`) for a layer reached through a
  reference.
- A layer that fails to parse keeps its old content while the others
  reload in the same call; `SdfLayer::ReloadLayers` warns and the parse
  error is raised after the stage has processed the rest.
- A file that appears where an arc failed to open resyncs the arc's prim.
- `SdfLayer::_Reload` re-resolves the root layer's identifier too: a root
  opened by a search-path identifier moves to the directory that now holds
  it, on a stage reload as on a reload of the layer alone.
- A muted layer is not one the stage uses, so a stage reload skips it, and
  the stage held its only use, so it was freed; unmuting opens the file
  anew and composes what was written while it was muted.

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

from pxr import Ar, Sdf, Tf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "layer_reload"))

ROOT = '''#usda 1.0
(
    subLayers = [
        @sub.usda@
    ]
)

def "A"
{
    double x = 1
}

def "R" (
    prepend references = @ref.usda@</R>
)
{
}

def "M" (
    prepend references = @missing.usda@</M>
)
{
}

def "Q" (
    prepend references = @asset.usda@</Q>
)
{
}
'''

SUB = '''#usda 1.0

def "B"
{
    double y = 2
}

def "S"
{
    double z = 3
}
'''

REF = '''#usda 1.0

def "R"
{
    double r = 6
}
'''

# `missing.usda` does not exist until a case creates it, and `asset.usda`
# is a search-path asset found only in `second/` until a case writes one
# into `first/`, the search path's first directory.
LAYERS = {
    "root.usda": ROOT,
    "sub.usda": SUB,
    "ref.usda": REF,
    "other.usda": REF.replace("r = 6", "r = 9"),
    "extra.usda": '''#usda 1.0

over "A"
{
    double e = 7
}
''',
    "second/asset.usda": '''#usda 1.0

def "Q"
{
    double q = 1
}
''',
    # A root layer opened by a search-path identifier, found only in
    # `second/` until a case writes one into `first/`.
    "second/search_root.usda": '''#usda 1.0

def "Z"
{
    double z = 1
}
''',
}

# The search path each case sets before it opens its stage, relative to
# the case's directory.
SEARCH_PATH = ["first", "second"]

# What a case writes over (or into) a file, by key.
REWRITES = {
    "sub_value": SUB.replace("y = 2", "y = 20"),
    "sub_added": SUB + '''
def "T"
{
    double w = 8
}
''',
    "sub_removed": '''#usda 1.0

def "B"
{
    double y = 2
}
''',
    "sub_renamed": SUB.replace('def "S"', 'def "S2"'),
    "sub_changed": '''#usda 1.0

def "B"
{
    double y = 20
}

def "S"
{
    double z = 3
}

def "T"
{
    double w = 8
}
''',
    "sub_broken": '#usda 1.0\n\ndef "B"\n{\n    double y = \n',
    "root_sublayer_added": ROOT.replace(
        "        @sub.usda@\n", "        @sub.usda@,\n        @extra.usda@\n"),
    "root_sublayer_removed": ROOT.replace(
        "(\n    subLayers = [\n        @sub.usda@\n    ]\n)\n", ""),
    "root_reference_target": ROOT.replace("@ref.usda@</R>", "@other.usda@</R>"),
    "ref_value": REF.replace("r = 6", "r = 60"),
    "missing_created": '''#usda 1.0

def "M"
{
    double m = 5
}
''',
    "search_root_first": '''#usda 1.0

def "Z"
{
    double z = 2
}

def "Y"
{
}
''',
    "asset_first": '''#usda 1.0

def "Q"
{
    double q = 2
}
''',
}

# The attributes recorded after each step.
ATTRIBUTES = ["/A.x", "/A.e", "/B.y", "/S.z", "/S2.z", "/T.w", "/R.r", "/M.m", "/Q.q", "/Z.z"]

# Each case: its name, the steps it takes after opening the stage, and
# optionally the identifier it opens the stage on (`root.usda` in the
# case's directory otherwise).
CASES = [
    ("unchanged", [["reload"]]),
    ("value", [["rewrite", "sub.usda", "sub_value"], ["reload"]]),
    ("prim_added", [["rewrite", "sub.usda", "sub_added"], ["reload"]]),
    ("prim_removed", [["rewrite", "sub.usda", "sub_removed"], ["reload"]]),
    ("prim_renamed", [["rewrite", "sub.usda", "sub_renamed"], ["reload"]]),
    ("sublayer_added", [["rewrite", "root.usda", "root_sublayer_added"], ["reload"]]),
    ("sublayer_removed", [["rewrite", "root.usda", "root_sublayer_removed"], ["reload"]]),
    ("reference_target", [["rewrite", "root.usda", "root_reference_target"], ["reload"]]),
    ("referenced_layer_value", [["rewrite", "ref.usda", "ref_value"], ["reload"]]),
    ("missing_appears", [["create", "missing.usda", "missing_created"], ["reload"]]),
    ("dirty", [["set", "root.usda", "/A.x", 100.0], ["reload"]]),
    ("dirty_layer_reload", [["set", "root.usda", "/A.x", 100.0], ["layer_reload", "root.usda"]]),
    ("dirty_layer_reload_force", [["set", "root.usda", "/A.x", 100.0],
                                  ["layer_reload_force", "root.usda"]]),
    ("clean_layer_reload", [["layer_reload", "sub.usda"], ["layer_reload_force", "sub.usda"]]),
    ("changed_layer_reload", [["rewrite", "sub.usda", "sub_value"],
                              ["layer_reload", "sub.usda"]]),
    ("parse_failure", [["rewrite", "sub.usda", "sub_broken"], ["rewrite", "ref.usda", "ref_value"],
                       ["reload"], ["rewrite", "sub.usda", "sub_value"], ["reload"]]),
    ("search_path", [["create_search", "first", "asset.usda", "asset_first"], ["reload"]]),
    ("search_path_layer_reload", [["create_search", "first", "asset.usda", "asset_first"],
                                  ["layer_reload", "asset.usda"]]),
    ("muted", [["mute", "sub.usda"], ["rewrite", "sub.usda", "sub_changed"], ["reload"],
               ["unmute", "sub.usda"]]),
    ("search_path_root", [["create_search", "first", "search_root.usda", "search_root_first"],
                          ["reload"]], "search_root.usda"),
    ("search_path_root_layer_reload",
     [["create_search", "first", "search_root.usda", "search_root_first"],
      ["layer_reload", "search_root.usda"]], "search_root.usda"),
]


class Listener:
    """The paths of the `ObjectsChanged` notices a stage sends."""

    def __init__(self, stage):
        self.heard = []
        self.key = Tf.Notice.Register(Usd.Notice.ObjectsChanged, self.on, stage)

    def on(self, notice, sender):
        self.heard.append({
            "resynced": sorted(str(p) for p in notice.GetResyncedPaths()),
            "info": sorted(str(p) for p in notice.GetChangedInfoOnlyPaths()),
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


def layer_name(directory, layer):
    """A layer's identifier, relative to the case's directory when it is a
    file path there; a search-path identifier stays as it is."""
    identifier = layer.identifier
    if os.path.isabs(identifier):
        return os.path.relpath(identifier, directory)
    return identifier


def used_layers(stage, directory):
    """Each file layer the stage uses: whether it is dirty and where its
    identifier resolves, relative to the case's directory."""
    gc.collect()
    layers = {}
    for layer in stage.GetUsedLayers():
        if layer.anonymous:
            continue
        layers[layer_name(directory, layer)] = {
            "dirty": layer.dirty,
            "resolved": os.path.relpath(layer.resolvedPath.GetPathString(), directory),
        }
    return layers


def find_layer(stage, directory, name):
    for layer in stage.GetUsedLayers():
        if not layer.anonymous and layer_name(directory, layer) == name:
            return layer
    return Sdf.Layer.Find(os.path.join(directory, name))


def write_file(path, text):
    """Writes `text` at `path` and, when it replaces a file, moves the
    modification time a second past the old one, so that a coarse file
    system clock cannot hide the change from OpenUSD's timestamp check."""
    before = os.stat(path).st_mtime_ns if os.path.exists(path) else None
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)
    if before is not None:
        after = max(os.stat(path).st_mtime_ns, before + 1_000_000_000)
        os.utime(path, ns=(after, after))


def take_step(stage, directory, step):
    """Takes `step`; returns what a layer reload returned (else `None`)
    and whether OpenUSD raised an error."""
    kind = step[0]
    returned = None
    try:
        if kind == "rewrite" or kind == "create":
            write_file(os.path.join(directory, step[1]), REWRITES[step[2]])
        elif kind == "create_search":
            write_file(os.path.join(directory, step[1], step[2]), REWRITES[step[3]])
        elif kind == "reload":
            stage.Reload()
        elif kind == "layer_reload":
            returned = find_layer(stage, directory, step[1]).Reload()
        elif kind == "layer_reload_force":
            returned = find_layer(stage, directory, step[1]).Reload(True)
        elif kind == "set":
            layer = find_layer(stage, directory, step[1])
            layer.GetAttributeAtPath(step[2]).default = step[3]
            del layer
        elif kind == "mute":
            stage.MuteLayer(os.path.join(directory, step[1]))
        elif kind == "unmute":
            stage.UnmuteLayer(os.path.join(directory, step[1]))
        else:
            raise ValueError(kind)
    except Tf.ErrorException:
        return returned, True
    return returned, False


def record(stage, directory, step, listener, returned=None, raised=False):
    entry = {
        "step": step,
        "values": {path: value(stage, path) for path in ATTRIBUTES},
        "prims": sorted(str(prim.GetPath()) for prim in stage.TraverseAll()),
        "layers": used_layers(stage, directory),
        "heard": listener.take(),
        "raised": raised,
    }
    if step[0] in ("layer_reload", "layer_reload_force"):
        entry["returned"] = returned
    return entry


def case_record(out, name, steps, root=None):
    directory = os.path.join(out, "work", name)
    shutil.copytree(os.path.join(out, "layers"), directory)
    directory = os.path.realpath(directory)
    # The default search path is process-wide; each case sets its own
    # before it opens its stage.
    Ar.DefaultResolver.SetDefaultSearchPath(
        [os.path.join(directory, d) for d in SEARCH_PATH])
    # A search-path identifier is looked for in the working directory
    # first; the oracle's is not the case's.
    stage = Usd.Stage.Open(root or os.path.join(directory, "root.usda"))
    listener = Listener(stage)
    recorded = [record(stage, directory, ["open"], listener)]
    for step in steps:
        returned, raised = take_step(stage, directory, step)
        recorded.append(record(stage, directory, step, listener, returned, raised))
    del listener
    del stage
    gc.collect()
    Ar.DefaultResolver.SetDefaultSearchPath([])
    recorded_case = {"name": name, "steps": recorded}
    if root:
        recorded_case["root"] = root
    return recorded_case


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
        "search_path": SEARCH_PATH,
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
