# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD's `UsdStage::DefinePrim`, `OverridePrim` and
`CreateClassPrim` author, call by call.

Usage: define_prim_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/define_prim` by default, the
layers below as OpenUSD exports them and `oracle.json`, which
`tests/conformance/define_prim.rs` replays against layerstack's `SchemaEdit::define_prim`,
`override_prim` and `create_class_prim`.

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

`OverridePrim` makes the same checks, returns an existing prim without
authoring, and otherwise creates the spec at the edit target
(`SdfCreatePrimInLayer`, keeping a spec already there). `CreateClassPrim`
refuses a target outside the local layer stack that maps paths to
themselves and a defined prim that is not a class; for a prim that is
missing or not abstract (neither it nor an ancestor a class) it defines it
as `_DefinePrim` does and then sets its specifier to `class`.

Every case starts from fresh copies of `scene.usda` (the root layer) and
`weak.usda`, its sublayer, and runs its steps in order. A step is a list
of calls, which layerstack makes in one `SchemaEdit`; OpenUSD makes them
one after another. For each call the oracle records whether OpenUSD raised
an error (`raised`), whether the returned prim is valid (`valid`) and
whether it is defined (`defined`). After each step it records the edit target layer's exported
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

OVER = "over"
CLASS = "class"

# (name, edit target layer, steps); a step is a list of calls: (path, type)
# for `DefinePrim`, an empty type being a typeless `DefinePrim(path)`;
# (OVER, path) for `OverridePrim` and (CLASS, path) for `CreateClassPrim`.
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
    # `OverridePrim`.
    ("override_new", "scene.usda", [[(OVER, "/Fresh/Leaf")]]),
    ("override_def_in_target", "scene.usda", [[(OVER, "/Prototype")]]),
    ("override_def_in_sublayer", "scene.usda", [[(OVER, "/Typed")]]),
    ("override_over", "scene.usda", [[(OVER, "/Over")]]),
    ("override_child_of_def", "scene.usda", [[(OVER, "/Prototype/Child")]]),
    ("override_class", "scene.usda", [[(OVER, "/Class")]]),
    ("override_below_inactive", "scene.usda", [[(OVER, "/Inactive/Child")]]),
    ("override_instance_root", "scene.usda", [[(OVER, "/Instance")]]),
    ("override_instance_proxy", "scene.usda", [[(OVER, "/Instance/Child")]]),
    ("override_beneath_instance", "scene.usda", [[(OVER, "/Instance/New")]]),
    ("override_in_prototype", "scene.usda", [[(OVER, "/__Prototype_1/Child")]]),
    ("override_pseudo_root", "scene.usda", [[(OVER, "/")]]),
    ("override_sublayer_target", "weak.usda", [[(OVER, "/Over/Deep")]]),
    ("override_then_define", "scene.usda", [[(OVER, "/Mix"), ("/Mix", "Xform"), (OVER, "/Mix")]]),
    ("define_then_override", "scene.usda", [[("/Mix", "Scope"), (OVER, "/Mix/Kid"), (OVER, "/Mix")]]),
    # `CreateClassPrim`.
    ("class_new", "scene.usda", [[(CLASS, "/Base")]]),
    ("class_existing", "scene.usda", [[(CLASS, "/Class")]]),
    ("class_on_def", "scene.usda", [[(CLASS, "/Prototype")]]),
    ("class_on_sublayer_def", "scene.usda", [[(CLASS, "/Typed")]]),
    ("class_on_over", "scene.usda", [[(CLASS, "/Over")]]),
    ("class_on_typed_over", "scene.usda", [[(CLASS, "/TypedOver")]]),
    ("class_in_class", "scene.usda", [[(CLASS, "/Class/Member")]]),
    ("class_on_over_in_class", "scene.usda", [[(OVER, "/Class/Inner"), (CLASS, "/Class/Inner")]]),
    ("class_nested_new", "scene.usda", [[(CLASS, "/Group/Base")]]),
    ("class_over_ancestors", "scene.usda", [[(CLASS, "/G/H/Base")]]),
    ("class_below_inactive", "scene.usda", [[(CLASS, "/Inactive/Base")]]),
    ("class_beneath_instance", "scene.usda", [[(CLASS, "/Instance/New")]]),
    ("class_pseudo_root", "scene.usda", [[(CLASS, "/")]]),
    ("class_sublayer_target", "weak.usda", [[(CLASS, "/Over/Shared")]]),
    ("class_then_define", "scene.usda", [[(CLASS, "/Base"), ("/Base", "Xform"), (CLASS, "/Base")]]),
    ("class_then_define_typeless", "scene.usda", [[(CLASS, "/Base"), ("/Base", "")]]),
    ("class_call_by_call", "scene.usda", [
        [(CLASS, "/Base")],
        [(CLASS, "/Base")],
        [(CLASS, "/Base/Kid")],
        [("/Base/Kid", "Xform")],
    ]),
]


def call(stage, op, path, type_name):
    """Makes the call, returning (raised, valid, defined)."""
    try:
        if op == OVER:
            prim = stage.OverridePrim(path)
        elif op == CLASS:
            prim = stage.CreateClassPrim(path)
        elif type_name:
            prim = stage.DefinePrim(path, type_name)
        else:
            prim = stage.DefinePrim(path)
    except Exception:
        return True, False, False
    return False, bool(prim), bool(prim) and prim.IsDefined()


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
            for entry in step:
                op, path, type_name = (
                    (entry[0], entry[1], "") if entry[0] in (OVER, CLASS) else ("def",) + entry)
                raised, valid, defined = call(stage, op, path, type_name)
                calls.append({"op": op, "path": path, "type": type_name, "raised": raised,
                              "valid": valid, "defined": defined})
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
