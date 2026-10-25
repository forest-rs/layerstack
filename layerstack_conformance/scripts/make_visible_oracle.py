# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD's `UsdGeomImageable::MakeVisible` and
`MakeInvisible` author, call by call.

Usage: make_visible_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/make_visible` by default,
the layers below as authored and `oracle.json`, which
`tests/conformance/make_visible.rs` replays against layerstack's `ImageableEdit`.

`MakeVisible` (`pxr/usd/usdGeom/imageable.cpp`) sets the prim's own
`visibility` to `inherited` if it is `invisible` at the time code, then
walks its ancestors from the root prim down: each `Imageable` ancestor
that is `invisible` there is set to `inherited`, and once one was, every
`Imageable` ancestor below it on the way has its other children
(`GetAllChildren`, inactive and undefined ones included) that are
`Imageable` set to `invisible`, so only the prim is revealed. A
non-`Imageable` ancestor is skipped. `MakeInvisible` sets `invisible`
unless the prim already is. Each value is set through
`CreateVisibilityAttr`, as a default or as a time sample at the time code.

`scene.usda`, the root layer and edit target, sublayers `hierarchy.usda`,
which holds the hierarchy. Every case starts from fresh copies of both
and runs its steps in order; a step is a list of calls, which layerstack
makes in one `SchemaEdit` and OpenUSD one after another (`define` is
`UsdStage::DefinePrim`). After each step the oracle records the root
layer's exported text and, for each call, `ComputeVisibility` of its prim
at its time. `before` is the root layer's text before any step.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd, UsdGeom

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "make_visible"))

SCENE = '''#usda 1.0
(
    subLayers = [
        @hierarchy.usda@
    ]
)

'''

HIERARCHY = '''#usda 1.0

def Xform "World"
{
    token visibility = "invisible"

    def Xform "A"
    {
        token visibility = "invisible"

        def Mesh "Target"
        {
            token visibility = "invisible"
        }

        def Mesh "Sibling"
        {
        }

        def "Untyped"
        {
        }

        def Mesh "Off" (
            active = false
        )
        {
        }

        over Mesh "Undefined"
        {
        }
    }

    def Xform "B"
    {
        def Mesh "Leaf"
        {
        }

        def Mesh "Sibling"
        {
        }
    }

    def "Group"
    {
        def Mesh "Deep"
        {
        }

        def Mesh "Beside"
        {
        }
    }
}

def Xform "Lit"
{
    def Mesh "Shown"
    {
    }

    def Mesh "Other"
    {
    }
}

def Xform "Anim"
{
    token visibility.timeSamples = {
        0: "inherited",
        10: "invisible",
    }

    def Mesh "Kid"
    {
    }

    def Mesh "Other"
    {
    }
}
'''

LAYERS = {"scene.usda": SCENE, "hierarchy.usda": HIERARCHY}


def visible(path, time=None):
    return {"call": "make_visible", "path": path, "time": time}


def invisible(path, time=None):
    return {"call": "make_invisible", "path": path, "time": time}


def define(path, type_name):
    return {"call": "define", "path": path, "type": type_name}


# (name, steps); a step is a list of calls.
CASES = [
    ("nested_invisible_ancestors", [[visible("/World/A/Target")]]),
    ("visible_parent_below_hidden", [[visible("/World/B/Leaf")]]),
    ("non_imageable_ancestor", [[visible("/World/Group/Deep")]]),
    ("already_visible", [[visible("/Lit/Shown")]]),
    ("invisible_then_visible", [
        [invisible("/Lit/Shown")],
        [invisible("/Lit/Shown")],
        [visible("/Lit/Shown")],
    ]),
    ("invisible_at_time", [[invisible("/Lit/Shown", 5)], [invisible("/Lit/Shown", 5)]]),
    ("animated_ancestor", [
        [visible("/Anim/Kid", 5)],
        [visible("/Anim/Kid", 10)],
        [visible("/Anim/Kid")],
    ]),
    ("hidden_parent", [[invisible("/Lit")], [visible("/Lit/Shown")]]),
    ("one_edit", [[invisible("/Lit"), visible("/Lit/Shown")]]),
    ("defined_in_the_edit", [[
        define("/Lit/Added", "Mesh"),
        invisible("/Lit"),
        visible("/Lit/Shown"),
    ]]),
]


def time_code(time):
    return Usd.TimeCode.Default() if time is None else Usd.TimeCode(time)


def call(stage, step):
    if step["call"] == "define":
        stage.DefinePrim(step["path"], step["type"])
        return
    imageable = UsdGeom.Imageable(stage.GetPrimAtPath(step["path"]))
    if step["call"] == "make_visible":
        imageable.MakeVisible(time_code(step["time"]))
    else:
        imageable.MakeInvisible(time_code(step["time"]))


def computed(stage, step):
    if step["call"] == "define":
        return None
    imageable = UsdGeom.Imageable(stage.GetPrimAtPath(step["path"]))
    return str(imageable.ComputeVisibility(time_code(step["time"])))


def reload_layers(out):
    for name in LAYERS:
        layer = Sdf.Layer.Find(os.path.join(out, name))
        if layer:
            layer.Reload(force=True)


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    reload_layers(out)
    root_path = os.path.join(out, "scene.usda")
    before = Sdf.Layer.FindOrOpen(root_path).ExportToString()
    cases = []
    for name, steps in CASES:
        reload_layers(out)
        stage = Usd.Stage.Open(root_path)
        root = stage.GetRootLayer()
        records = []
        for step in steps:
            for one in step:
                call(stage, one)
            records.append({
                "calls": [dict(one, visibility=computed(stage, one)) for one in step],
                "text": root.ExportToString(),
            })
        cases.append({"name": name, "steps": records})
    reload_layers(out)
    _, minor, patch = Usd.GetVersion()
    oracle = {"openusd_version": f"{minor}.{patch}", "before": before, "cases": cases}
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
        write(tmp)
        for name in list(LAYERS) + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
