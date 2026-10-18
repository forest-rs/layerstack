# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD reads, writes, composes and authors `variantSetNames`.

Usage: variant_set_names_oracle.py [--check] [OUT_DIR]
       variant_set_names_oracle.py --read FILE

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/variant_set_names` by default:

- `*.usda`: the layers below, as authored;
- `forms.usdc`: `forms.usda` as OpenUSD writes it in the crate format;
- `oracle.json`, which `tests/variant_set_names.rs` replays against
  layerstack.

A variant set composes only where a site lists it: `PcpComposeSiteVariantSets`
applies the `variantSetNames` list ops of the site's layers weakest first,
and `_EvalNodeVariantSets` evaluates the sets that leaves, in that order
(`pxr/usd/pcp/composeSite.cpp`, `primIndex.cpp`). A variant set spec no list
names contributes nothing; a listed set without a spec contributes no
opinions; each node composes its own site, so a stronger node's `delete`
does not remove a weaker node's listing; and a variant node composes the
list ops of its variant specs for the sets nested in the branch.

The oracle records:

- `compose`: for each prim of `root.usda` (sublayering `strong.usda` and
  `weak.usda`, referencing `asset.usda`), `UsdVariantSets::GetNames`, the
  selection `UsdVariantSet::GetVariantSelection` reports for each, and the
  resolved default of every attribute with a value;
- `flatten`: the `variantSetNames` of each prim spec of `flatten.usda`'s
  layer stack as `UsdUtils.FlattenLayerStack` writes it;
- `forms`: each spec's `variantSetNames` list op and set specs as OpenUSD
  reads `forms.usda` and `forms.usdc`, which exercise every list op form
  USDA can spell (OpenUSD's text parser takes no empty explicit
  `variantSets`, `[]` or `None`, and its writer leaves the value out);
- `edits`: authoring calls on `edits.usda` (sublayering `edits_sub.usda`;
  `/Vehicle` references `asset.usda`'s `/Model`), call by call: whether
  OpenUSD accepted each call (`false` when it raised a coding error or
  returned an invalid object) and the `variantSetNames` and variant sets
  of the spec the edit target addresses after it, then whether each probed
  spec exists. The calls are
  `UsdVariantSets::AddVariantSet` with each `UsdListPosition`,
  `UsdPrim::SetMetadata` / `ClearMetadata` of `variantSetNames`,
  `SdfCreatePrimInLayer` of a variant path, and `SdfPrimSpec::RemoveVariantSet`,
  which Python spells as erasing the set from the spec's `variantSets`
  proxy (`SdfVariantSetsProxy::erase`, which is all `RemoveVariantSet`
  does); a set the spec does not hold is erased as `RemoveChild` does, by
  doing nothing.

`--read FILE` prints the `forms` records of FILE, so a test can check how
OpenUSD reads a layer layerstack saved. `--check` rewrites everything into
a temporary directory and fails unless it matches the checked-in fixtures
(the checked-in `forms.usdc` must read as the `forms` records too).
"""
import argparse
import filecmp
import json
import os
import sys
import tempfile

from pxr import Sdf, Usd, UsdUtils

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "variant_set_names"))


def stack_sets(prim, names):
    """Variant sets `a`..`d` of a stack case prim, each with one variant
    `on` authoring `weight` (so the strongest evaluated set wins it) and a
    marker of its own."""
    body = []
    for weight, name in enumerate(names, start=1):
        body.append(f'''    variantSet "{name}" = {{
        "on" {{
            double weight = {weight}
            bool from_{name} = 1
        }}
    }}''')
    return f'''over "{prim}"
{{
''' + "\n".join(body) + "\n}\n"


SETS = ["a", "b", "c", "d"]
# Stack case prims: (prim, weak list op, strong list op, root list op).
STACK = [
    ("Prepends", 'prepend variantSets = ["a", "b"]', 'prepend variantSets = "c"',
     'append variantSets = "a"'),
    ("Explicit", 'prepend variantSets = "a"', 'variantSets = ["b", "c"]',
     'prepend variantSets = "d"'),
    ("Deletes", 'prepend variantSets = ["a", "b"]', None, 'delete variantSets = "a"'),
    ("Reorders", 'prepend variantSets = ["a", "b", "c"]', None,
     'reorder variantSets = ["c", "a"]'),
    ("Adds", 'prepend variantSets = "a"', 'add variantSets = ["b", "a"]', None),
    ("DeleteThenAppend", 'prepend variantSets = "a"', 'delete variantSets = "a"',
     'append variantSets = "a"'),
    ("AppendsAndPrepends", 'append variantSets = ["a", "b"]',
     'prepend variantSets = "b"', 'append variantSets = ["c", "a"]'),
]


def meta_block(lines):
    lines = [line for line in lines if line]
    if not lines:
        return ""
    return " (\n" + "".join(f"    {line}\n" for line in lines) + ")"


def stack_layer(which):
    out = ["#usda 1.0\n"]
    for prim, weak, strong, root in STACK:
        op = {"weak": weak, "strong": strong, "root": root}[which]
        if which == "weak":
            text = stack_sets(prim, SETS)
            head, rest = text.split("\n", 1)
            out.append(head + meta_block([op]) + "\n" + rest)
        elif op:
            out.append(f'over "{prim}"{meta_block([op])}\n{{\n}}\n')
    return "\n".join(out)


def root_layer():
    stack = []
    for prim, _, _, root in STACK:
        selections = "variants = {\n" + "".join(
            f'        string {name} = "on"\n' for name in SETS) + "    }"
        stack.append(f'def "{prim}"{meta_block([root, selections])}\n{{\n}}\n')
    return '''#usda 1.0
(
    subLayers = [
        @strong.usda@,
        @weak.usda@
    ]
)

# A variant set spec no list names: `look` is not evaluated, so `shine`
# has no opinion.
def "Unlisted" (
    variants = {
        string look = "wet"
    }
)
{
    variantSet "look" = {
        "wet" {
            double shine = 1
        }
    }
}

# A listed set without a spec: `size` is evaluated, and contributes no
# opinions.
def "ListedWithoutSpec" (
    prepend variantSets = "size"
    variants = {
        string size = "big"
    }
)
{
}

# The root deletes `look` in its own layer stack only: the referenced
# node lists it, and the root's selection is the strongest.
def "Referencing" (
    delete variantSets = "look"
    prepend references = @asset.usda@</Model>
    variants = {
        string look = "dry"
    }
)
{
}

# The root lists `look` and has no spec for it; the referenced node has a
# spec and no listing. Neither composes the branch.
def "ReferencingUnlisted" (
    prepend variantSets = "look"
    prepend references = @asset.usda@</Bare>
    variants = {
        string look = "wet"
    }
)
{
}

# A nested set composes where its branch lists it: `size` is listed in
# the `round` branch, `color` is not.
def "Nested" (
    prepend variantSets = "shape"
    variants = {
        string color = "red"
        string shape = "round"
        string size = "big"
    }
)
{
    variantSet "shape" = {
        "round" (
            prepend variantSets = "size"
        ) {
            double radius = 1
            variantSet "color" = {
                "red" {
                    double hue = 1
                }
            }
            variantSet "size" = {
                "big" {
                    double height = 2
                }
            }
        }
    }
}

# A branch's list ops compose across the layer stack: `weak.usda` lists
# `size` and `finish` in the `round` branch, and the root deletes
# `finish` there.
def "NestedStack" (
    variants = {
        string finish = "matte"
        string shape = "round"
        string size = "big"
    }
)
{
    variantSet "shape" = {
        "round" (
            delete variantSets = "finish"
        ) {
        }
    }
}

''' + "\n".join(stack)


WEAK_EXTRA = '''
over "NestedStack" (
    prepend variantSets = "shape"
)
{
    variantSet "shape" = {
        "round" (
            prepend variantSets = ["size", "finish"]
        ) {
            variantSet "finish" = {
                "matte" {
                    double gloss = 0.25
                }
            }
            variantSet "size" = {
                "big" {
                    double height = 3
                }
            }
        }
    }
}
'''

ASSET = '''#usda 1.0

def "Model" (
    prepend variantSets = "look"
    variants = {
        string look = "wet"
    }
)
{
    variantSet "look" = {
        "dry" {
            double shine = 0.5
        }
        "wet" {
            double shine = 1
        }
    }
}

def "Bare" (
    variants = {
        string look = "wet"
    }
)
{
    variantSet "look" = {
        "wet" {
            double shine = 1
        }
    }
}
'''

FLATTEN = '''#usda 1.0
(
    subLayers = [
        @flatten_sub.usda@
    ]
)

over "Prepends" (
    prepend variantSets = "c"
    append variantSets = "a"
)
{
}

over "Explicit" (
    prepend variantSets = "d"
)
{
}

over "Deletes" (
    delete variantSets = "a"
)
{
}

over "WeakExplicit" (
    delete variantSets = "b"
    append variantSets = "c"
)
{
}

over "StrongOnly" (
    append variantSets = "b"
)
{
}
'''

FLATTEN_SUB = '''#usda 1.0

def "Prepends" (
    prepend variantSets = ["a", "b"]
)
{
}

def "Explicit" (
    variantSets = ["b", "c"]
)
{
}

def "Deletes" (
    prepend variantSets = ["a", "b"]
)
{
}

def "WeakExplicit" (
    variantSets = ["a", "b"]
)
{
}

def "WeakOnly" (
    delete variantSets = "a"
    prepend variantSets = "b"
)
{
}
'''

FORMS = '''#usda 1.0

def "Explicit" (
    variantSets = ["a", "b"]
)
{
}

def "Prepended" (
    prepend variantSets = ["a", "b"]
)
{
}

def "Appended" (
    append variantSets = "a"
)
{
}

def "Deleted" (
    delete variantSets = ["a"]
)
{
}

def "Added" (
    add variantSets = ["a", "b"]
)
{
}

def "Reordered" (
    reorder variantSets = ["b", "a"]
)
{
}

def "Edits" (
    delete variantSets = "c"
    prepend variantSets = "a"
    append variantSets = "b"
    reorder variantSets = ["b", "a"]
)
{
}

def "Unlisted"
{
    variantSet "look" = {
        "wet" {
            double shine = 1
        }
    }
}

def "Branch" (
    prepend variantSets = "shape"
)
{
    variantSet "shape" = {
        "round" (
            append variantSets = "size"
        ) {
            variantSet "size" = {
                "big" {
                    double height = 1
                }
            }
        }
    }
}
'''

FORM_SPECS = [
    "/Explicit", "/Prepended", "/Appended", "/Deleted", "/Added",
    "/Reordered", "/Edits", "/Unlisted", "/Branch", "/Branch{shape=round}",
]

EDITS = '''#usda 1.0
(
    subLayers = [
        @edits_sub.usda@
    ]
)

def "Rock" (
    delete variantSets = "gone"
    prepend variantSets = "size"
    variants = {
        string size = "big"
    }
)
{
    variantSet "size" = {
        "big" {
            double height = 2
            def "Pebble"
            {
                double radius = 1
            }
        }
        "small" {
        }
    }
}

def "Plant" (
    variantSets = "leaf"
)
{
}

def "Light"
{
}

def "Vehicle" (
    prepend references = @asset.usda@</Model>
)
{
}
'''

EDITS_SUB = '''#usda 1.0

over "Terrain"
{
}
'''

LAYERS = {
    "root.usda": root_layer(),
    "strong.usda": stack_layer("strong"),
    "weak.usda": stack_layer("weak") + WEAK_EXTRA,
    "asset.usda": ASSET,
    "flatten.usda": FLATTEN,
    "flatten_sub.usda": FLATTEN_SUB,
    "forms.usda": FORMS,
    "edits.usda": EDITS,
    "edits_sub.usda": EDITS_SUB,
}

COMPOSE_PRIMS = ["/Unlisted", "/ListedWithoutSpec", "/Referencing", "/ReferencingUnlisted",
                 "/Nested", "/NestedStack"] + [f"/{prim}" for prim, _, _, _ in STACK]

FLATTEN_PRIMS = ["/Prepends", "/Explicit", "/Deletes", "/WeakExplicit", "/StrongOnly",
                 "/WeakOnly"]

POSITIONS = {
    "front_of_prepend": Usd.ListPositionFrontOfPrependList,
    "back_of_prepend": Usd.ListPositionBackOfPrependList,
    "front_of_append": Usd.ListPositionFrontOfAppendList,
    "back_of_append": Usd.ListPositionBackOfAppendList,
}

# (name, edit target, prim, calls, probed spec paths in the target layer)
EDIT_CASES = [
    ("add_positions", {"kind": "root"}, "/Light", [
        {"call": "add_variant_set", "name": "a", "position": "back_of_prepend"},
        {"call": "add_variant_set", "name": "b", "position": "front_of_prepend"},
        {"call": "add_variant_set", "name": "c", "position": "back_of_append"},
        {"call": "add_variant_set", "name": "a", "position": "back_of_append"},
        {"call": "add_variant_set", "name": "b", "position": "back_of_prepend"},
        {"call": "add_variant_set", "name": "d", "position": "front_of_append"},
    ], []),
    ("add_listed_and_deleted", {"kind": "root"}, "/Rock", [
        {"call": "add_variant_set", "name": "size", "position": "front_of_append"},
        {"call": "add_variant_set", "name": "gone", "position": "back_of_prepend"},
        {"call": "add_variant_set", "name": "size", "position": "front_of_prepend"},
    ], ["/Rock{size=big}", "/Rock{size=big}Pebble"]),
    ("add_explicit", {"kind": "root"}, "/Plant", [
        {"call": "add_variant_set", "name": "stem", "position": "front_of_append"},
        {"call": "add_variant_set", "name": "leaf", "position": "back_of_append"},
        {"call": "add_variant_set", "name": "stem", "position": "back_of_prepend"},
    ], []),
    ("add_invalid_names", {"kind": "root"}, "/Rock", [
        {"call": "add_variant_set", "name": "1bad"},
        {"call": "add_variant_set", "name": ""},
        {"call": "add_variant_set", "name": "a b"},
        {"call": "add_variant_set", "name": "ns:x"},
    ], []),
    ("add_pseudo_root", {"kind": "root"}, "/", [
        {"call": "add_variant_set", "name": "x"},
    ], []),
    ("add_creates_over", {"kind": "sublayer"}, "/Rock", [
        {"call": "add_variant_set", "name": "size"},
    ], []),
    ("add_in_variant", {"kind": "variant", "prim": "/Rock"}, "/Rock", [
        {"call": "add_variant_set", "name": "color"},
        {"call": "add_variant_set", "name": "finish", "position": "back_of_append"},
    ], []),
    ("add_through_reference", {"kind": "reference", "prim": "/Vehicle"}, "/Vehicle", [
        {"call": "add_variant_set", "name": "trim"},
        {"call": "add_variant_set", "name": "look", "position": "back_of_append"},
    ], []),
    ("set_names", {"kind": "root"}, "/Light", [
        {"call": "set_variant_set_names",
         "value": {"prepend": ["a", "b"], "append": ["c"], "delete": ["d"]}},
        {"call": "set_variant_set_names", "value": {"explicit": []}},
        {"call": "set_variant_set_names",
         "value": {"prepend": ["a", "b", "a"], "append": ["b", "c", "b"],
                   "delete": ["d", "d"]}},
        {"call": "set_variant_set_names", "value": {"explicit": ["c", "a", "c"]}},
        {"call": "set_variant_set_names", "value": {"add": ["z", "z"], "reorder": ["b", "a"]}},
    ], []),
    ("set_names_cleared", {"kind": "root"}, "/Rock", [
        {"call": "set_variant_set_names", "value": None},
        {"call": "set_variant_set_names", "value": None},
    ], ["/Rock{size=big}"]),
    ("set_names_not_identifiers", {"kind": "root"}, "/Light", [
        {"call": "set_variant_set_names", "value": {"prepend": ["1bad"]}},
    ], []),
    ("set_names_creates_over", {"kind": "sublayer"}, "/Plant", [
        {"call": "set_variant_set_names", "value": {"append": ["leaf"]}},
    ], []),
    ("set_names_in_variant", {"kind": "variant", "prim": "/Rock"}, "/Rock", [
        {"call": "set_variant_set_names", "value": {"prepend": ["color"]}},
    ], []),
    ("remove_set", {"kind": "root"}, "/Rock", [
        {"call": "remove_variant_set", "name": "size"},
        {"call": "remove_variant_set", "name": "size"},
        {"call": "remove_variant_set", "name": "missing"},
    ], ["/Rock{size=big}", "/Rock{size=small}", "/Rock{size=big}Pebble",
        "/Rock{size=big}Pebble.radius", "/Rock{size=big}.height"]),
    ("remove_nested_set", {"kind": "variant", "prim": "/Rock"}, "/Rock", [
        {"call": "create_prim_in_layer", "path": "/Rock{size=big}{color=red}Chip"},
        {"call": "remove_variant_set", "name": "color"},
    ], ["/Rock{size=big}", "/Rock{size=big}{color=red}", "/Rock{size=big}{color=red}Chip"]),
    ("remove_added_set", {"kind": "root"}, "/Light", [
        {"call": "add_variant_set", "name": "shape"},
        {"call": "remove_variant_set", "name": "shape"},
    ], []),
    ("create_variant", {"kind": "root"}, "/Light", [
        {"call": "create_prim_in_layer", "path": "/Light{shape=smooth}"},
        {"call": "create_prim_in_layer", "path": "/Light{shape=rough}"},
        {"call": "create_prim_in_layer", "path": "/Light{size=big}"},
        {"call": "create_prim_in_layer", "path": "/Light{shape=smooth}{color=red}"},
    ], ["/Light{shape=smooth}", "/Light{shape=rough}", "/Light{size=big}",
        "/Light{shape=smooth}{color=red}"]),
    ("create_variant_explicit", {"kind": "root"}, "/Plant", [
        {"call": "create_prim_in_layer", "path": "/Plant{trunk=tall}"},
    ], ["/Plant{trunk=tall}"]),
    ("create_variant_deleted", {"kind": "root"}, "/Rock", [
        {"call": "create_prim_in_layer", "path": "/Rock{gone=x}"},
        {"call": "create_prim_in_layer", "path": "/Rock{size=medium}"},
    ], ["/Rock{gone=x}", "/Rock{size=medium}"]),
]


def list_op(op):
    return {
        "explicit": list(op.explicitItems) if op.isExplicit else None,
        "prepend": list(op.prependedItems),
        "append": list(op.appendedItems),
        "delete": list(op.deletedItems),
        "add": list(op.addedItems),
        "reorder": list(op.orderedItems),
    }


def spec_record(spec):
    """The `variantSetNames` list op of a prim or variant spec (`null`
    when it authors none), and its variant sets with their variant names."""
    if spec is None:
        return None
    if isinstance(spec, Sdf.VariantSpec):
        names_spec = spec.primSpec
        sets = spec.variantSets
    else:
        names_spec = spec
        sets = spec.variantSets
    names = (list_op(names_spec.GetInfo("variantSetNames"))
             if names_spec.HasInfo("variantSetNames") else None)
    return {
        "names": names,
        "sets": {name: sorted(v.name for v in sets[name].variants.values())
                 for name in sorted(sets.keys())},
    }


def forms_record(path):
    layer = Sdf.Layer.OpenAsAnonymous(path)
    return {spec: spec_record(layer.GetObjectAtPath(spec)) for spec in FORM_SPECS}


def value(v):
    if isinstance(v, bool):
        return v
    if isinstance(v, (int, float)):
        return float(v)
    return str(v)


def compose_record(out):
    stage = Usd.Stage.Open(os.path.join(out, "root.usda"))
    record = {}
    for path in COMPOSE_PRIMS:
        prim = stage.GetPrimAtPath(path)
        names = prim.GetVariantSets().GetNames()
        record[path] = {
            "names": names,
            "selections": {name: prim.GetVariantSets().GetVariantSelection(name)
                           for name in names},
            "values": {attr.GetName(): value(attr.Get())
                       for attr in prim.GetAttributes() if attr.Get() is not None},
        }
    return record


def flatten_record(out):
    stage = Usd.Stage.Open(os.path.join(out, "flatten.usda"))
    flat = UsdUtils.FlattenLayerStack(stage)
    return {path: spec_record(flat.GetPrimAtPath(path)) for path in FLATTEN_PRIMS}


def node_of(prim, arc_name):
    stack = [prim.GetPrimIndex().rootNode]
    while stack:
        node = stack.pop(0)
        if str(node.arcType).lower().endswith(arc_name):
            return node
        stack.extend(node.children)
    raise LookupError(f"{prim.GetPath()} has no {arc_name} node")


def edit_target(stage, target, out):
    kind = target["kind"]
    if kind == "root":
        return Usd.EditTarget(stage.GetRootLayer())
    if kind == "sublayer":
        return Usd.EditTarget(Sdf.Layer.Find(os.path.join(out, "edits_sub.usda")))
    node = node_of(stage.GetPrimAtPath(target["prim"]), kind)
    return Usd.EditTarget(node.layerStack.layerTree.layer, node)


def names_op(value):
    op = Sdf.StringListOp()
    if "explicit" in value:
        op.explicitItems = value["explicit"]
    for key, attr in (("prepend", "prependedItems"), ("append", "appendedItems"),
                      ("delete", "deletedItems"), ("add", "addedItems"),
                      ("reorder", "orderedItems")):
        if key in value:
            setattr(op, attr, value[key])
    return op


def call(stage, target, prim, step):
    kind = step["call"]
    if kind == "add_variant_set":
        position = POSITIONS[step.get("position", "back_of_prepend")]
        return prim.GetVariantSets().AddVariantSet(step["name"], position).IsValid()
    if kind == "set_variant_set_names":
        if step["value"] is None:
            return prim.ClearMetadata("variantSetNames")
        return prim.SetMetadata("variantSetNames", names_op(step["value"]))
    if kind == "create_prim_in_layer":
        return bool(Sdf.CreatePrimInLayer(target.GetLayer(), Sdf.Path(step["path"])))
    if kind == "remove_variant_set":
        spec = target.GetLayer().GetObjectAtPath(target.MapToSpecPath(prim.GetPath()))
        if spec is None:
            return True
        if step["name"] in spec.variantSets:
            del spec.variantSets[step["name"]]
        return True
    raise ValueError(kind)


def edits_record(out):
    cases = []
    for name, target_desc, prim_path, steps, probes in EDIT_CASES:
        for layer in list(Sdf.Layer.GetLoadedLayers()):
            if not layer.anonymous:
                layer.Reload(force=True)
        stage = Usd.Stage.Open(os.path.join(out, "edits.usda"))
        target = edit_target(stage, target_desc, out)
        stage.SetEditTarget(target)
        prim = stage.GetPrimAtPath(prim_path)
        spec_path = target.MapToSpecPath(Sdf.Path(prim_path))
        layer = target.GetLayer()

        def addressed():
            if spec_path.isEmpty or prim_path == "/":
                return None
            return spec_record(layer.GetObjectAtPath(spec_path))

        accepted = []
        after = []
        for step in steps:
            try:
                accepted.append(call(stage, target, prim, step) is not False)
            except Exception:
                accepted.append(False)
            after.append(addressed())
        cases.append({
            "name": name,
            "target": target_desc,
            "prim": prim_path,
            "steps": steps,
            "accepted": accepted,
            "spec_path": None if spec_path.isEmpty else str(spec_path),
            "after": after,
            "probes": {path: layer.GetObjectAtPath(path) is not None for path in probes},
        })
        for layer in list(Sdf.Layer.GetLoadedLayers()):
            if not layer.anonymous:
                layer.Reload(force=True)
    return cases


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    Sdf.Layer.OpenAsAnonymous(os.path.join(out, "forms.usda")).Export(
        os.path.join(out, "forms.usdc"))
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "compose": compose_record(out),
        "flatten": flatten_record(out),
        "forms": forms_record(os.path.join(out, "forms.usda")),
        "forms_usdc": forms_record(os.path.join(out, "forms.usdc")),
        "edits": edits_record(out),
    }
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--read")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if args.read:
        json.dump(forms_record(args.read), sys.stdout, indent=1, sort_keys=True)
        sys.stdout.write("\n")
        return
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        for name in list(LAYERS) + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")
    with open(os.path.join(FIXTURES, "oracle.json")) as f:
        recorded = json.load(f)
    if forms_record(os.path.join(FIXTURES, "forms.usdc")) != recorded["forms"]:
        raise SystemExit("the checked-in forms.usdc does not read as forms.usda")


if __name__ == "__main__":
    main()
