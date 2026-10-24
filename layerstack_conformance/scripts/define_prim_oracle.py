# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD's `UsdStage::DefinePrim` authors, call by call.

Usage: define_prim_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/define_prim` by default, the
layers below as OpenUSD exports them and `oracle.json`, which
`tests/define_prim.rs` replays against layerstack's `SchemaEdit::define_prim`.

`UsdStage::DefinePrim` (`pxr/usd/usd/stage.cpp`) first refuses a path
beneath an instance or in a prototype (`_IsValidPathForCreatingPrim`,
`_ValidateEditPrim`), with a coding error and no authoring, when the edit
target is a local layer that maps the path to itself. `_DefinePrim` then
returns the pseudo-root for `/` without authoring, and otherwise defines
the parent first, recursively, with no type. At each prim it authors only
if no prim is on the stage there, the prim is not defined, or a type is
given that is not the prim's composed type name: it creates the spec at
the edit target (`SdfCreatePrimInLayer`, which makes missing ancestor specs
`over`s) and sets its specifier to `def` and its type name. A prim it
defined but cannot then find on the stage (beneath an inactive prim) is a
runtime error after the authoring.

Every case starts from fresh copies of `scene.usda` (the root layer) and
`weak.usda`, its sublayer, and runs its steps in order. A step is a list
of calls, which layerstack makes in one `SchemaEdit`; OpenUSD makes them
one after another. For each call the oracle records whether OpenUSD raised
an error (`raised`) and whether the returned prim is valid and defined
(`defined`). After each step it records the edit target layer's exported
text and the stage's default traversal. `before` is each layer's text
before any step.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "define_prim"))

SCENE = '''#usda 1.0
(
    subLayers = [
        @weak.usda@
    ]
)

over "Over"
{
}

over Xform "TypedOver"
{
}

def "Inactive" (
    active = false
)
{
}

over "G"
{
    over "H"
    {
    }
}

over "OverParent"
{
    def Xform "Kid"
    {
    }
}

class "Class"
{
}

def Xform "Instance" (
    instanceable = true
    prepend references = </Prototype>
)
{
}

def Xform "Prototype"
{
    def Xform "Child"
    {
    }
}
'''

WEAK = '''#usda 1.0

def Scope "Typed"
{
}

def Xform "Same"
{
}

def Scope "WeakParent"
{
}
'''

LAYERS = {"scene.usda": SCENE, "weak.usda": WEAK}

# (name, edit target layer, steps); a step is a list of (path, type) calls,
# an empty type being a typeless `DefinePrim(path)`.
CASES = [
    ("undefined_parent", "scene.usda", [[("/New/Leaf", "Xform")]]),
    ("over_in_target", "scene.usda", [[("/Over", "Xform")]]),
    ("typed_over_in_target", "scene.usda", [[("/TypedOver", "Xform")]]),
    ("same_type_in_sublayer", "scene.usda", [[("/Same", "Xform")]]),
    ("other_type_in_sublayer", "scene.usda", [[("/Typed", "Xform")]]),
    ("typeless_on_typed", "scene.usda", [[("/Typed", "")]]),
    ("typeless_new", "scene.usda", [[("/Plain", "")]]),
    ("under_sublayer_def", "scene.usda", [[("/WeakParent/Child", "Xform")]]),
    ("over_only_ancestors", "scene.usda", [[("/G/H/I", "Xform")]]),
    ("defined_child_of_over", "scene.usda", [[("/OverParent/Kid", "Xform")]]),
    ("under_class", "scene.usda", [[("/Class/Member", "Xform")]]),
    ("below_inactive", "scene.usda", [[("/Inactive/Child", "Xform")]]),
    ("instance_root", "scene.usda", [[("/Instance", "Scope")]]),
    ("instance_proxy", "scene.usda", [[("/Instance/Child", "Xform")]]),
    ("beneath_instance", "scene.usda", [[("/Instance/New", "Xform")]]),
    ("in_prototype", "scene.usda", [[("/__Prototype_1/Child", "Xform")]]),
    ("pseudo_root", "scene.usda", [[("/", "Xform")]]),
    ("sublayer_target", "weak.usda", [[("/Over/Deep", "Xform")]]),
    ("one_edit", "scene.usda", [[
        ("/Seq/A", "Xform"),
        ("/Seq/A/B", "Scope"),
        ("/Seq/A", "Xform"),
        ("/Seq/A", "Scope"),
        ("/Seq", ""),
    ]]),
    ("call_by_call", "scene.usda", [
        [("/Again", "Xform")],
        [("/Again", "Xform")],
        [("/Again", "Scope")],
        [("/Again/Child", "")],
    ]),
]


def define(stage, path, type_name):
    """Calls `DefinePrim`, returning (raised, defined)."""
    try:
        if type_name:
            prim = stage.DefinePrim(path, type_name)
        else:
            prim = stage.DefinePrim(path)
    except Exception:
        return True, False
    return False, bool(prim) and prim.IsDefined()


def traversal(stage):
    return [str(prim.GetPath()) for prim in stage.Traverse()]


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
    before = {name: Sdf.Layer.FindOrOpen(os.path.join(out, name)).ExportToString()
              for name in LAYERS}
    cases = []
    for name, target_name, steps in CASES:
        reload_layers(out)
        stage = Usd.Stage.Open(os.path.join(out, "scene.usda"))
        target = Sdf.Layer.Find(os.path.join(out, target_name))
        stage.SetEditTarget(Usd.EditTarget(target))
        records = []
        for step in steps:
            calls = []
            for path, type_name in step:
                raised, defined = define(stage, path, type_name)
                calls.append({"path": path, "type": type_name, "raised": raised,
                              "defined": defined})
            records.append({"calls": calls, "text": target.ExportToString(),
                            "traversal": traversal(stage)})
        cases.append({"name": name, "target": target_name, "steps": records})
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
