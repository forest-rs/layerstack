# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD reads, writes and composes the child and property
orders a variant authors.

Usage: variant_orders_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/variant_orders` by default:

- `*.usda`: the layers below, as authored;
- `root.usdc`: `root.usda` as OpenUSD writes it in the crate format;
- `oracle.json`, which `tests/conformance/variant_orders.rs` replays
  against layerstack.

A variant's prim spec is an `SdfPrimSpec` like any other
(`SdfVariantSpec::GetPrimSpec`), so it holds `primOrder`
(`reorder nameChildren`) and `propertyOrder` (`reorder properties`) as a
prim spec does, a nested variant's and a variant's descendants' too.
Composition reads `primOrder` at every site of a prim index, variant sites
included, weakest site first, and reorders the names composed so far after
each (`PcpComposeSiteChildNames`, `pxr/usd/pcp/composeSite.cpp`, from
`_ComposePrimChildNamesAtNode`, `pxr/usd/pcp/primIndex.cpp`); property
names are sorted and then the strongest `propertyOrder`, a variant's
included, moves its names to the front (`UsdPrim::_GetPropertyNames`,
`pxr/usd/usd/prim.cpp`).

The oracle records:

- `specs`: for each layer, every prim spec and variant prim spec, with the
  orders it authors (`nameChildrenOrder`, `propertyOrder`, `null` when it
  authors none);
- `text`: each layer's `ExportToString`; `root.usdc` reads back to the
  same text;
- `composed`: for every prim of the stage on `root.usda`, its children
  (`GetAllChildrenNames`), property names (`GetPropertyNames`) and the
  composed `primOrder` and `propertyOrder` (`GetMetadata`,
  `GetPropertyOrder`, the strongest opinion of each);
- `switched`: the same after `/S` selects its other variant, authored on
  the root layer;
- `flatten`: every prim spec `UsdStage::Flatten` writes, with its children
  and orders;
- `layer_stack_flatten`: every prim and variant prim spec
  `UsdUtils.FlattenLayerStack` writes, with its children and orders
  (`_FlattenSpec`, `pxr/usd/usd/flattenUtils.cpp`, composes a variant's
  children with its `primOrder` as a prim's);
- `edits`: calls made through an edit target into `/E`'s variant `v=x`,
  and into the variant `n=y` nested in it: `UsdPrim::SetChildrenReorder`,
  `ClearChildrenReorder`, `SetPropertyOrder` and `ClearPropertyOrder`, and
  `SdfPrimSpec::nameChildrenOrder` on the variant's prim spec. Each case
  records the variant's prim spec orders, `/E`'s composed children and
  property names, and the root layer's `ExportToString` after its last
  call.

`--check` rewrites everything into a temporary directory and fails unless
the layers and `oracle.json` match the checked-in fixture and the
checked-in `root.usdc` reads back as `root.usda`.
"""
import argparse
import filecmp
import json
import os
import sys
import tempfile

from pxr import Sdf, Usd, UsdUtils

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "variant_orders"))

LAYERS = {
    "root.usda": '''#usda 1.0
(
    subLayers = [
        @weak.usda@
    ]
)

# Orders at every level: the prim, its variant, the variant nested in it,
# a child of the variant, and a variant of that child. The weak sublayer
# authors the same variant with an order of its own.
def "P" (
    variants = {
        string v = "x"
    }
    prepend variantSets = "v"
)
{
    reorder nameChildren = ["A", "B"]
    int z = 0
    def "A"
    {
    }

    def "B"
    {
    }
    variantSet "v" = {
        "x" (
            variants = {
                string n = "y"
            }
            prepend variantSets = "n"
        ) {
            reorder properties = ["a1", "a0"]
            reorder nameChildren = ["C1", "C0"]
            int a0 = 0
            int a1 = 1

            def "C0" (
                variants = {
                    string w = "z"
                }
                prepend variantSets = "w"
            )
            {
                reorder nameChildren = ["G1", "G0"]
                def "G0"
                {
                }

                def "G1"
                {
                }
                variantSet "w" = {
                    "z" {
                        reorder properties = ["k", "j"]
                        reorder nameChildren = ["K", "G0", "J"]
                        int j = 0
                        int k = 1

                        def "J"
                        {
                        }

                        def "K"
                        {
                        }

                    }
                }
            }

            def "C1"
            {
            }
            variantSet "n" = {
                "y" {
                    reorder properties = ["b1", "a0"]
                    reorder nameChildren = ["N1", "B", "N0"]
                    int b0 = 0
                    int b1 = 1

                    def "N0"
                    {
                    }

                    def "N1"
                    {
                    }

                }
            }

        }
    }
}

# The variant comes through a reference: the asset's variant orders the
# referencing prim's children beneath its own local ones.
def "Q" (
    prepend references = @asset.usda@</Asset>
)
{
    def "L"
    {
    }
}

# Two variants ordering the same children differently, for a switch.
def "S" (
    variants = {
        string look = "a"
    }
    prepend variantSets = "look"
)
{
    def "X"
    {
    }

    def "Y"
    {
    }

    def "Z"
    {
    }
    variantSet "look" = {
        "a" {
            reorder nameChildren = ["Z", "X"]

        }
        "b" {
            reorder properties = ["q", "p"]
            reorder nameChildren = ["Y", "Z", "X"]
            int p = 0
            int q = 1

        }
    }
}

# The target of the edits.
def "E" (
    variants = {
        string v = "x"
    }
    prepend variantSets = "v"
)
{
    def "A"
    {
    }
    variantSet "v" = {
        "x" (
            variants = {
                string n = "y"
            }
            prepend variantSets = "n"
        ) {
            int a = 1
            int b = 2

            def "B"
            {
            }

            def "C"
            {
            }
            variantSet "n" = {
                "y" {
                    int c = 3

                    def "D"
                    {
                    }

                }
            }

        }
    }
}
''',
    "weak.usda": '''#usda 1.0

over "P"
{
    variantSet "v" = {
        "x" {
            reorder properties = ["w", "a0"]
            reorder nameChildren = ["W", "C1"]
            int w = 0

            def "W"
            {
            }

        }
    }
}
''',
    "asset.usda": '''#usda 1.0

def "Asset" (
    variants = {
        string v = "x"
    }
    prepend variantSets = "v"
)
{
    def "R0"
    {
    }

    def "R1"
    {
    }
    variantSet "v" = {
        "x" {
            reorder properties = ["t", "s"]
            reorder nameChildren = ["L", "R1", "V", "R0"]
            int s = 0
            int t = 1

            def "V"
            {
            }

        }
    }
}
''',
}

# The selection `tests/conformance/variant_orders.rs` switches through
# `LiveStage`, authored on the root layer.
SWITCH = ("/S", "look", "b")

# (name, variant chain of `/E` the edit target addresses, calls). A call is
# `[method, value]`; `sdf_name_children_order` assigns the variant prim
# spec's `nameChildrenOrder` directly.
EDITS = [
    ("set_prim_order", [("v", "x")], [
        ["set_prim_order", ["C", "B", "A"]],
    ]),
    ("set_and_clear_prim_order", [("v", "x")], [
        ["set_prim_order", ["C", "B"]],
        ["set_prim_order", None],
    ]),
    ("empty_prim_order", [("v", "x")], [
        ["set_prim_order", []],
    ]),
    ("set_property_order", [("v", "x")], [
        ["set_property_order", ["b", "a"]],
    ]),
    ("set_both_orders", [("v", "x")], [
        ["set_property_order", ["b", "a"]],
        ["set_prim_order", ["C", "A", "B"]],
        ["set_property_order", None],
    ]),
    ("nested_variant", [("v", "x"), ("n", "y")], [
        ["set_prim_order", ["D", "C", "B", "A"]],
        ["set_property_order", ["c", "b"]],
    ]),
    ("sdf_name_children_order", [("v", "x")], [
        ["sdf_name_children_order", ["C", "B"]],
    ]),
]


def specs_record_of(spec):
    """The orders `spec` authors, `None` for each it does not."""
    children = spec.nameChildrenOrder if spec.HasInfo("primOrder") else None
    properties = spec.propertyOrder if spec.HasInfo("propertyOrder") else None
    return {
        "primOrder": None if children is None else [str(n) for n in children],
        "propertyOrder": None if properties is None else [str(n) for n in properties],
    }


def specs_record(layer):
    """Every prim spec and variant prim spec of `layer`, with its orders."""
    out = {}

    def visit(path):
        if not (path.IsPrimPath() or path.IsPrimVariantSelectionPath()):
            return
        spec = layer.GetPrimAtPath(path)
        if spec is not None:
            out[str(path)] = specs_record_of(spec)

    layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    return dict(sorted(out.items()))


def composed_record(stage):
    out = {}
    for prim in stage.TraverseAll():
        prim_order = prim.GetMetadata("primOrder")
        out[str(prim.GetPath())] = {
            "children": [str(n) for n in prim.GetAllChildrenNames()],
            "properties": [str(n) for n in prim.GetPropertyNames()],
            "primOrder": None if prim_order is None else [str(n) for n in prim_order],
            "propertyOrder": [str(n) for n in prim.GetPropertyOrder()],
        }
    if stage.GetCompositionErrors():
        sys.exit(f"unexpected composition errors: {stage.GetCompositionErrors()}")
    return out


def flattened_record(layer):
    """Every prim spec and variant prim spec of a flattened layer, with its
    children in order and its orders."""
    out = {}

    def visit(path):
        if not (path.IsPrimPath() or path.IsPrimVariantSelectionPath()):
            return
        spec = layer.GetPrimAtPath(path)
        if spec is None:
            return
        record = specs_record_of(spec)
        record["children"] = [c.name for c in spec.nameChildren]
        out[str(path)] = record

    layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    return dict(sorted(out.items()))


def edits_record(directory):
    cases = []
    for name, chain, calls in EDITS:
        for layer in list(Sdf.Layer.GetLoadedLayers()):
            if not layer.anonymous:
                layer.Reload(force=True)
        stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
        prim = stage.GetPrimAtPath("/E")
        root = stage.GetRootLayer()
        path = Sdf.Path("/E")
        for set_name, variant in chain:
            path = path.AppendVariantSelection(set_name, variant)
        # The variant's node in `/E`'s prim index, as
        # `UsdVariantSet::GetVariantEditTarget` makes for the outer one.
        target = Usd.EditTarget.ForLocalDirectVariant(root, path)
        stage.SetEditTarget(target)
        accepted = []
        for method, value in calls:
            try:
                if method == "set_prim_order":
                    result = (prim.ClearChildrenReorder() if value is None
                              else prim.SetChildrenReorder(value))
                elif method == "set_property_order":
                    result = (prim.ClearPropertyOrder() if value is None
                              else prim.SetPropertyOrder(value))
                elif method == "sdf_name_children_order":
                    root.GetPrimAtPath(path).nameChildrenOrder = value
                    result = None
                else:
                    raise ValueError(method)
                accepted.append(result is not False)
            except Exception:
                accepted.append(False)
        cases.append({
            "name": name,
            "variant": str(path),
            "calls": calls,
            "accepted": accepted,
            "spec": specs_record_of(root.GetPrimAtPath(path)),
            "composed": composed_record(stage)["/E"],
            "text": root.ExportToString(),
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
    layers = {name: Sdf.Layer.OpenAsAnonymous(os.path.join(out, name)) for name in LAYERS}
    layers["root.usda"].Export(os.path.join(out, "root.usdc"))
    stage = Usd.Stage.Open(os.path.join(out, "root.usda"))
    composed = composed_record(stage)
    flatten = flattened_record(stage.Flatten(addSourceFileComment=False))
    layer_stack_flatten = flattened_record(UsdUtils.FlattenLayerStack(stage))
    prim, set_name, variant = SWITCH
    stage.SetEditTarget(stage.GetRootLayer())
    stage.GetPrimAtPath(prim).GetVariantSet(set_name).SetVariantSelection(variant)
    switched = composed_record(stage)
    stage = None
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "specs": {name: specs_record(layer) for name, layer in layers.items()},
        "text": {name: layer.ExportToString() for name, layer in layers.items()},
        "composed": composed,
        "switched": {"selection": list(SWITCH), "composed": switched},
        "flatten": flatten,
        "layer_stack_flatten": layer_stack_flatten,
        "edits": edits_record(out),
    }
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def usdc_reads_as_usda(directory):
    usdc = Sdf.Layer.OpenAsAnonymous(os.path.join(directory, "root.usdc"))
    usda = Sdf.Layer.OpenAsAnonymous(os.path.join(directory, "root.usda"))
    return usdc.ExportToString() == usda.ExportToString()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        if not usdc_reads_as_usda(args.out):
            raise SystemExit("root.usdc does not read as root.usda")
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        for name in list(LAYERS) + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")
    if not usdc_reads_as_usda(FIXTURES):
        raise SystemExit("the checked-in root.usdc does not read as root.usda")


if __name__ == "__main__":
    main()
