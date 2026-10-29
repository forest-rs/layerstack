# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD maps the target paths inside relational attribute
and mapper targets through composition arcs.

Usage: relational_arcs_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/relational_arcs` by default,
the layers below as authored and `oracle.json`, which
`tests/conformance/relational_arcs.rs` replays against layerstack.

A relationship may target a relational attribute (`</A.b[/C].d>`) or a
mapper (`</A.b.mapper[/C]>`), and an attribute may connect to a relational
attribute. Composing such a path across an arc maps the bracketed paths as
well as the owning prim's: `Pcp_TranslatePath`
(`pxr/usd/pcp/pathTranslation.cpp`) maps the whole path with
`PcpMapFunction::MapSourceToTarget`, which maps only its prim part
(`_Map` in `pxr/usd/pcp/mapFunction.cpp` replaces the prefix with
`fixTargetPaths = false`), then maps each path
`SdfPath::GetAllTargetPathsRecursively` finds in the result, innermost
property element first, and puts each mapped one in with
`SdfPath::ReplacePrefix(target, mapped)`, which fixes target paths. A path
any of whose bracketed paths does not map does not map: the target is
dropped (`PcpErrorInvalidExternalTargetPath`, `_PathTranslateCallback` in
`pxr/usd/pcp/targetIndex.cpp`).

`root.usda` composes the asset `asset.usda` through each arc kind: an
external reference, a payload, a reference nested in another asset
(`outer.usda`), an internal reference, inherits and specializes of a
class, a variant of the referenced prim, a reference relocated by the
root layer's `relocates`, a class the asset's own prim inherits, and
instanceable references. Each authors
relational attribute targets, mapper targets and connections, nested
brackets, relative paths (and a bracket `SdfPath::MakeAbsolutePath` leaves
relative, which maps only where the arc maps every path outside its
pairs to itself), variant selections in a bracket, deleted targets, and
paths that do not map: a bracketed path, an
owning prim or a nested bracketed path outside the arc's namespace, and a
path an inherit maps back to the class (`PcpMapFunction`'s bijection
check). `/A/B` references the asset's `/A`, whose target `</A.b[/A].d>`
shows that `SdfPath::ReplacePrefix` replaces the mapped prefix in the
owning prim's path too.

The oracle records:

- `targets`: for every composed property, instance proxies included, the
  targets of a relationship (`UsdRelationship::GetTargets`) or the
  connections of an attribute that authors some
  (`UsdAttribute::GetConnections`);
- `flattened`: for every property spec of `UsdStage::Flatten`'s layer
  that authors targets or connections, its explicit list.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import sys
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "relational_arcs"))

ASSET = '''#usda 1.0

def "Model" (
    prepend variantSets = "v"
    variants = {
        string v = "a"
    }
)
{
    float c.connect = </Model.b[/Model/C].d>
    rel m = </Model.b.mapper[/Model/C]>
    rel n = </Model.b[/Model/C.d[/Model/E].f].g>
    rel r = </Model.b[/Model/C].d>
    rel t = </Model.b[/Model/C].d.mapper[/Model/E]>
    rel relative = [<.b[C].d>, <C.b[../E].d>, <.b.mapper[C/F]>]
    rel outside = [</Model.b[/Other].d>, </Model.b[/Model/C].d>]
    rel outsideOwner = [</Other.b[/Model/C].d>, </Model/E>]
    rel outsideMapper = [</Model.b.mapper[/Other]>, </Model.b.mapper[/Model/E]>]
    rel outsideNested = [</Model.b[/Model/C.d[/Other].f].g>, </Model.b[/Model/C.d[/Model/E].f].g>]
    float outsideConnect.connect = [</Model.b[/Other].d>, </Model/C.x>]
    rel relativeBracket = [<.b[C].d.mapper[E]>, </Model/E>]
    rel variantBracket = [</Model.b[/Model/C{w=x}].d>, </Model.b[/Model{v=a}C].d>]
    delete rel deleted = [</Model.b[/Other].d>, </Model.b[/Model/C].d>]
    variantSet "v" = {
        "a" {
            rel inVariant = </Model.b[/Model/C].d>
            float variantConnect.connect = </Model/C.b[/Model/E].d>
        }
    }

    def "C"
    {
        def "F"
        {
        }
    }

    def "E"
    {
    }
}

def "Other"
{
}

def "A"
{
    rel r = </A.b[/A].d>
    rel s = </A/X.b[/A/X].d>
    rel t = </A.b[/A/X].d>

    def "X"
    {
    }
}

def "Classy" (
    inherits = </_Class>
)
{
}

class "_Class"
{
    rel r = </_Class.b[/_Class/C].d>
    rel kept = </_Class.b[/Elsewhere].d>
    rel back = [</_Class.b[/Classy/C].d>, </_Class/C>]

    def "C"
    {
    }
}
'''

OUTER = '''#usda 1.0

def "Outer"
{
    rel r = </Outer.b[/Outer/Inner/C].d>
    rel m = </Outer/Inner.b.mapper[/Outer/Inner/C]>
    rel outside = [</Outer.b[/Model/C].d>, </Outer/Inner>]

    def "Inner" (
        prepend references = @./asset.usda@</Model>
    )
    {
    }
}
'''

ROOT = '''#usda 1.0
(
    relocates = {
        </Reloc/C>: </Reloc/D>
    }
)

def "Ref" (
    prepend references = @./asset.usda@</Model>
)
{
}

def "Pay" (
    prepend payload = @./asset.usda@</Model>
)
{
}

def "Nested" (
    prepend references = @./outer.usda@</Outer>
)
{
}

def "Reloc" (
    prepend references = @./asset.usda@</Model>
)
{
    rel local = </Reloc.b[/Reloc/D].d>
}

def "Local"
{
    rel r = </Local.b[/Local/C].d>
    rel m = </Local.b.mapper[/Local/C]>
    float c.connect = </Local.b[/Local/C].d>
    rel outside = [</Local.b[/Ref/C].d>, </Local/C>]

    def "C"
    {
    }
}

def "Internal" (
    prepend references = </Local>
)
{
}

class "Class"
{
    rel r = </Class.b[/Class/C].d>
    rel m = </Class.b.mapper[/Class/C]>
    float c.connect = </Class.b[/Class/C].d>
    rel kept = </Class.b[/Ref/C].d>
    rel back = [</Class.b[/Inst/C].d>, </Class/C>]
    rel relativeBracket = <.b[C].d.mapper[E]>

    def "C"
    {
    }
}

def "Inst" (
    prepend inherits = </Class>
)
{
}

def "Spec" (
    prepend specializes = </Class>
)
{
}

def "Classy" (
    prepend references = @./asset.usda@</Classy>
)
{
}

def "A"
{
    def "B" (
        prepend references = @./asset.usda@</A>
    )
    {
    }
}

def "Strong" (
    prepend references = @./asset.usda@</Model>
)
{
    delete rel r = </Strong.b[/Strong/C].d>
    prepend rel m = </Strong.b.mapper[/Strong/E]>
}

def "Instances"
{
    def "I1" (
        instanceable = true
        prepend references = @./asset.usda@</Model>
    )
    {
    }

    def "I2" (
        instanceable = true
        prepend references = @./asset.usda@</Model>
    )
    {
    }
}
'''

LAYERS = {"asset.usda": ASSET, "outer.usda": OUTER, "root.usda": ROOT}


def composed_targets(stage):
    """Every composed property's targets or connections, by path."""
    out = {}
    predicate = Usd.TraverseInstanceProxies(Usd.PrimAllPrimsPredicate)
    for prim in Usd.PrimRange.Stage(stage, predicate):
        for rel in prim.GetRelationships():
            out[str(rel.GetPath())] = [str(p) for p in rel.GetTargets()]
        for attr in prim.GetAttributes():
            if attr.HasAuthoredConnections():
                out[str(attr.GetPath())] = [str(p) for p in attr.GetConnections()]
    return out


def flattened_targets(layer):
    """Every property spec's explicit targets or connections in `layer`."""
    out = {}

    def visit(path):
        spec = layer.GetObjectAtPath(path)
        if isinstance(spec, Sdf.RelationshipSpec):
            items = spec.targetPathList
        elif isinstance(spec, Sdf.AttributeSpec):
            items = spec.connectionPathList
        else:
            return
        if items.isExplicit or items.explicitItems:
            out[str(path)] = [str(p) for p in items.explicitItems]

    layer.Traverse(Sdf.Path.absoluteRootPath, visit)
    return dict(sorted(out.items()))


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    stage = Usd.Stage.Open(os.path.join(out, "root.usda"))
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "targets": composed_targets(stage),
        "flattened": flattened_targets(stage.Flatten()),
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
        write(tmp)
        names = sorted(LAYERS) + ["oracle.json"]
        _, mismatch, errors = filecmp.cmpfiles(tmp, FIXTURES, names, shallow=False)
        if mismatch or errors:
            sys.exit(f"stale fixtures in {FIXTURES}: {mismatch + errors}")


if __name__ == "__main__":
    main()
