# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD composes and announces when an edit changes which
children a prim has, or their order, with inactive children among them:
`primOrder` (`reorder nameChildren`) moving or naming inactive children,
an inactive child added or removed, a child deactivated or activated, and
the same through a reference and inside a variant.

Usage: inactive_children_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/inactive_children` by
default, the layers below and `oracle.json`, which
`tests/conformance/inactive_children.rs` replays against layerstack.

Each case copies the layers into a directory of its own, opens a stage on
`root.usda` and takes its steps, each an edit of one prim spec of one
layer. After each step it records, for each prim of `WATCHED`, the names
of `GetAllChildren`, `GetChildren` and
`GetFilteredChildren(Usd.PrimAllPrimsPredicate)`, and the
`UsdNotice::ObjectsChanged` paths the step sent, with `GetChangedFields`
of every path. Every record is checked against a stage opened afresh on
the same layers, so that it records a composition OpenUSD itself agrees
with.

What it shows, with the 26.11 code that does it: `primOrder` is a field
`UsdStage::_HandleLayersDidChange` (`pxr/usd/usd/stage.cpp`) treats as a
child reorder (`didReorderChildren`), so authoring or clearing it
resyncs the parent, whether the children it moves are active or not, and
`GetAllChildren` lists the inactive ones where the order puts them while
`GetChildren` leaves them out. `active` is a field
`_HandleLayersDidChange` recomposes for, so it resyncs the prim, and a
prim spec added or removed resyncs the prim (`didAddNonInertPrim`,
`didRemoveNonInertPrim`), active or not, and reports nothing of its
parent. A child added or removed in the referenced asset, or inside the
selected variant, resyncs the prim composing it the same way. `documentation`, which
composes nothing, changes a prim info-only, with inactive children or
inactive itself.

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
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "inactive_children"))

LAYERS = {
    "root.usda": '''#usda 1.0
(
    subLayers = [
        @weak.usda@
    ]
)

def "P"
{
    def "A"
    {
    }

    def "B" (
        active = false
    )
    {
        def "Inner"
        {
        }
    }

    def "C"
    {
    }

    def "D" (
        active = false
    )
    {
    }
}

def "R" (
    prepend references = @asset.usda@</Asset>
)
{
    over "Y" (
        active = false
    )
    {
    }
}

def "V" (
    variants = {
        string look = "one"
    }
    prepend variantSets = "look"
)
{
    variantSet "look" = {
        "one" {
            def "M"
            {
            }

            def "N" (
                active = false
            )
            {
            }

            def "O"
            {
            }

        }
        "two" {
            def "N"
            {
            }

            def "M" (
                active = false
            )
            {
            }

        }
    }
}
''',
    "weak.usda": '''#usda 1.0

over "P"
{
    def "W" (
        active = false
    )
    {
    }
}
''',
    "asset.usda": '''#usda 1.0

def "Asset"
{
    def "X"
    {
    }

    def "Y"
    {
    }

    def "Z" (
        active = false
    )
    {
    }
}
''',
}

# The prims whose children are recorded after each step.
WATCHED = ["/P", "/P/B", "/P/C", "/R", "/V"]

# Each case: its name and its steps. A step edits the prim spec at a path
# (a variant path, `/V{look=one}N`, for a spec inside a variant) of the
# layer named by its file:
#
# - `["active", layer, path, value]` sets `active`, or clears it for `null`,
#   adding an `over` for the prim first, in the same change block, when the
#   layer has no spec for it;
# - `["order", layer, path, names]` sets `primOrder`, or clears it for `null`;
# - `["def", layer, path, active]` adds a `def` spec, with `active` unless
#   it is `null`, in one change block;
# - `["remove", layer, path]` removes the spec and everything beneath it;
# - `["select", layer, path, set, variant]` sets a variant selection;
# - `["doc", layer, path, text]` sets `documentation`, adding an `over` for
#   the prim first when the layer has no spec for it.
CASES = [
    # `D`, the last child, is inactive: moving it first changes
    # `GetAllChildren` alone; then an order naming only inactive children,
    # one naming active and inactive children, and none.
    ("order_moves_inactive", [
        ["order", "root.usda", "/P", ["D", "A", "B", "C"]],
        ["order", "root.usda", "/P", ["D", "B"]],
        ["order", "root.usda", "/P", ["C", "B", "A"]],
        ["order", "root.usda", "/P", None],
    ]),
    # `W` comes from the weaker layer, inactive, and is ordered first.
    ("order_moves_weak_inactive", [
        ["order", "root.usda", "/P", ["A", "W"]],
        ["order", "weak.usda", "/P", ["W", "A"]],
        ["order", "root.usda", "/P", None],
    ]),
    ("add_remove_inactive", [
        ["def", "root.usda", "/P/E", False],
        ["remove", "root.usda", "/P/E"],
        ["def", "weak.usda", "/P/F", False],
        ["remove", "weak.usda", "/P/W"],
        ["remove", "weak.usda", "/P/F"],
        ["remove", "root.usda", "/P/D"],
    ]),
    ("deactivate_activate", [
        ["active", "root.usda", "/P/C", False],
        ["active", "root.usda", "/P/C", True],
        ["active", "root.usda", "/P/B", True],
        ["active", "root.usda", "/P/B", None],
        ["active", "weak.usda", "/P/A", False],
        ["active", "root.usda", "/P/A", True],
        ["active", "root.usda", "/P/A", None],
    ]),
    # An order authored while children are inactive, which then become
    # active, and the reverse.
    ("order_then_activate", [
        ["active", "root.usda", "/P/C", False],
        ["order", "root.usda", "/P", ["C", "D", "A"]],
        ["active", "root.usda", "/P/C", None],
        ["active", "root.usda", "/P/D", True],
        ["active", "root.usda", "/P/A", False],
        ["order", "root.usda", "/P", ["B", "A", "D"]],
    ]),
    # `/R` references `/Asset`, which has an inactive `Z`; `/R` deactivates
    # `Y`.
    ("through_reference", [
        ["order", "root.usda", "/R", ["Z", "Y", "X"]],
        ["order", "asset.usda", "/Asset", ["Y", "Z"]],
        ["def", "asset.usda", "/Asset/Q", False],
        ["active", "asset.usda", "/Asset/X", False],
        ["active", "root.usda", "/R/Z", True],
        ["remove", "asset.usda", "/Asset/Q"],
        ["order", "root.usda", "/R", None],
    ]),
    # `/V` selects `one`, with an inactive `N` between `M` and `O`; `two`
    # lists `N` first and holds an inactive `M`.
    ("in_variant", [
        ["order", "root.usda", "/V{look=one}", ["O", "N", "M"]],
        ["def", "root.usda", "/V{look=one}L", False],
        ["active", "root.usda", "/V{look=one}N", True],
        ["active", "root.usda", "/V{look=one}M", False],
        ["select", "root.usda", "/V", "look", "two"],
        ["order", "root.usda", "/V{look=two}", ["M", "N"]],
        ["select", "root.usda", "/V", "look", "one"],
        ["remove", "root.usda", "/V{look=one}L"],
        ["order", "root.usda", "/V{look=one}", None],
    ]),
    # Metadata that composes nothing, on prims with inactive children,
    # changes them info-only.
    ("metadata_beside_inactive", [
        ["doc", "root.usda", "/P", "parent"],
        ["doc", "weak.usda", "/P", "weaker"],
        ["doc", "asset.usda", "/Asset", "asset"],
        ["doc", "root.usda", "/V{look=one}", "variant"],
        ["doc", "root.usda", "/P/B", "inactive"],
    ]),
]


class Listener:
    """The paths of the `ObjectsChanged` notices a stage sends, with the
    changed fields of each."""

    def __init__(self, stage):
        self.heard = []
        self.key = Tf.Notice.Register(Usd.Notice.ObjectsChanged, self.on, stage)

    def on(self, notice, sender):
        resynced = sorted(str(p) for p in notice.GetResyncedPaths())
        info = sorted(str(p) for p in notice.GetChangedInfoOnlyPaths())
        self.heard.append({
            "resynced": resynced,
            "info": info,
            "fields": {p: sorted(notice.GetChangedFields(p)) for p in resynced + info},
        })

    def take(self):
        heard, self.heard = self.heard, []
        return heard


def take_step(directory, step):
    kind = step[0]
    layer = Sdf.Layer.Find(os.path.join(directory, step[1]))
    assert layer, step
    path = Sdf.Path(step[2])
    if kind == "active":
        with Sdf.ChangeBlock():
            spec = layer.GetPrimAtPath(path) or Sdf.CreatePrimInLayer(layer, path)
            if step[3] is None:
                spec.ClearInfo("active")
            else:
                spec.active = step[3]
    elif kind == "order":
        spec = layer.GetPrimAtPath(path)
        assert spec, step
        if step[3] is None:
            spec.ClearInfo("primOrder")
        else:
            spec.nameChildrenOrder = step[3]
    elif kind == "def":
        with Sdf.ChangeBlock():
            spec = Sdf.CreatePrimInLayer(layer, path)
            spec.specifier = Sdf.SpecifierDef
            if step[3] is not None:
                spec.active = step[3]
    elif kind == "remove":
        spec = layer.GetPrimAtPath(path)
        assert spec, step
        parent = layer.GetPrimAtPath(path.GetParentPath())
        del parent.nameChildren[spec.name]
    elif kind == "doc":
        with Sdf.ChangeBlock():
            spec = layer.GetPrimAtPath(path) or Sdf.CreatePrimInLayer(layer, path)
            spec.documentation = step[3]
    elif kind == "select":
        spec = layer.GetPrimAtPath(path)
        assert spec, step
        spec.variantSelections[step[3]] = step[4]
    else:
        raise ValueError(kind)


def children(stage):
    found = {}
    for path in WATCHED:
        prim = stage.GetPrimAtPath(path)
        if not prim:
            continue
        found[path] = {
            "all": [c.GetName() for c in prim.GetAllChildren()],
            "active": [c.GetName() for c in prim.GetChildren()],
            "filtered": [c.GetName() for c in
                         prim.GetFilteredChildren(Usd.PrimAllPrimsPredicate)],
        }
    return found


def record(stage, directory, step, listener):
    composed = children(stage)
    fresh = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    # The layers are open and edited in memory: the fresh stage reads them.
    assert composed == children(fresh), f"{step}: {composed} != fresh {children(fresh)}"
    return {"step": step, "children": composed, "heard": listener.take()}


def case_record(out, name, steps):
    directory = os.path.join(out, "work", name)
    shutil.copytree(os.path.join(out, "layers"), directory)
    directory = os.path.realpath(directory)
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    listener = Listener(stage)
    recorded = [record(stage, directory, ["open"], listener)]
    for step in steps:
        take_step(directory, step)
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
    oracle = {"openusd_version": f"{minor}.{patch}", "cases": cases}
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
