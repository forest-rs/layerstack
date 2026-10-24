# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD finds and packages UDIM texture tiles.

Usage: udim_tiles_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/udim_tiles` by default:

- the layers and tile files below, as authored; each tile's content is its
  own relative path, so a packaged tile shows which file it came from;
- `oracle.json`, which `tests/udim_tiles.rs` replays against layerstack.

`scene.usda` names UDIM sets in asset values: a search path and a `./`
path to one set (tiles 1001, 1002, 1011), a set with gaps and with files
just outside the 1001-1100 range, a `..` segment before the marker, a set
without tiles, a bare `<UDIM>`, a tile in a directory name, time samples,
an array, and a variable expression. It references `models/model.usda`,
whose patterns anchor to that layer. `package.usda` holds the cases
`UsdUtilsCreateNewUsdzPackage` packages completely.

Recorded: `UsdShadeUdimUtils::IsUdimIdentifier` and `ReplaceUdimPattern`
for a list of spellings; `ResolveUdimTilePaths` and `ResolveUdimPath` for
every authored pattern, relative to its layer; `UsdUtilsComputeAllDependencies`
of `scene.usda`, with each unresolved path traced back to the property
authoring it; and, after packaging `package.usda`, the package's members
and, for each asset value of each packaged layer, its authored spelling and
the tiles `ResolveUdimTilePaths` finds for it inside the package, with
their contents.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import tempfile
import zipfile

from pxr import Sdf, Usd, UsdShade, UsdUtils

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "udim_tiles"))

LAYERS = {
    "scene.usda": '''#usda 1.0
(
    expressionVariables = {
        string NAME = "expr"
    }
)

def "Root" (
    references = @./models/model.usda@</Model>
)
{
    asset search = @textures/color.<UDIM>.png@
    asset dotted = @./textures/color.<UDIM>.png@
    asset gap = @./textures/gap.<UDIM>.png@
    asset parent = @./textures/../shared/stone.<UDIM>.png@
    asset missing = @./textures/none.<UDIM>.png@
    asset bare = @<UDIM>@
    asset folder = @./tiles/<UDIM>/color.png@
    asset sampled.timeSamples = {
        1: @./textures/a.<UDIM>.png@,
        2: @./textures/b.<UDIM>.png@,
    }
    asset[] array = [@./textures/arr.<UDIM>.png@, @./textures/color.<UDIM>.png@]
    asset expression = @`"./textures/${NAME}.<UDIM>.png"`@
}
''',
    "package.usda": '''#usda 1.0

def "Root" (
    references = @./models/model.usda@</Model>
)
{
    asset search = @textures/color.<UDIM>.png@
    asset dotted = @./textures/color.<UDIM>.png@
    asset gap = @./textures/gap.<UDIM>.png@
    asset parent = @./textures/../shared/stone.<UDIM>.png@
    asset sampled.timeSamples = {
        1: @./textures/a.<UDIM>.png@,
        2: @./textures/b.<UDIM>.png@,
    }
    asset[] array = [@./textures/arr.<UDIM>.png@, @./textures/color.<UDIM>.png@]
}
''',
    "models/model.usda": '''#usda 1.0

def "Model"
{
    asset bark = @./tex/bark.<UDIM>.png@
    asset stone = @../shared/stone.<UDIM>.png@
}
''',
}

TILES = [
    "textures/color.1001.png",
    "textures/color.1002.png",
    "textures/color.1011.png",
    "textures/gap.1000.png",
    "textures/gap.1001.png",
    "textures/gap.1003.png",
    "textures/gap.1100.png",
    "textures/gap.1101.png",
    "shared/stone.1002.png",
    "models/tex/bark.1001.png",
    "models/tex/bark.1005.png",
    "textures/a.1001.png",
    "textures/a.1002.png",
    "textures/b.1001.png",
    "textures/arr.1001.png",
    "textures/arr.1002.png",
    "textures/expr.1001.png",
    "textures/expr.1004.png",
    "tiles/1001/color.png",
    "tiles/1002/color.png",
]

SPELLINGS = [
    "textures/color.<UDIM>.png",
    "<UDIM>",
    "a<UDIM>",
    "<UDIM>b",
    "x.<UDIM>.<UDIM>.png",
    "<udim>.png",
    "./tiles/<UDIM>/color.png",
    '`"./textures/${NAME}.<UDIM>.png"`',
    "color.png",
]


def write_files(out):
    for name, text in LAYERS.items():
        path = os.path.join(out, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
    for name in TILES:
        path = os.path.join(out, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(name + "\n")


def asset_values(layer):
    """(property path, value) for every authored asset value, in path order."""
    values = []

    def visit(spec):
        for prop in spec.properties:
            path = str(prop.path)
            default = prop.default if hasattr(prop, "default") else None
            if isinstance(default, Sdf.AssetPath):
                values.append((path, default.path))
            elif default is not None and hasattr(default, "__iter__") and not isinstance(default, str):
                values.extend((path, v.path) for v in default if isinstance(v, Sdf.AssetPath))
            for time in sorted(layer.ListTimeSamplesForPath(prop.path)):
                sample = layer.QueryTimeSample(prop.path, time)
                if isinstance(sample, Sdf.AssetPath):
                    values.append((path, sample.path))
        for child in spec.nameChildren:
            visit(child)

    for prim in layer.rootPrims:
        visit(prim)
    return values


def tiles_record(root):
    records = []
    for name in ["scene.usda", "models/model.usda"]:
        layer = Sdf.Layer.FindOrOpen(os.path.join(root, name))
        for prop, value in asset_values(layer):
            resolved = UsdShade.UdimUtils.ResolveUdimPath(value, layer)
            records.append({
                "layer": name,
                "property": prop,
                "pattern": value,
                "resolved": os.path.relpath(resolved, root) if resolved else "",
                "tiles": [[os.path.relpath(str(p), root), tile]
                          for p, tile in UsdShade.UdimUtils.ResolveUdimTilePaths(value, layer)],
            })
    return records


def dependencies_record(root):
    scene = os.path.join(root, "scene.usda")
    layers, assets, unresolved = UsdUtils.ComputeAllDependencies(scene)
    unresolved = set(str(u) for u in unresolved)
    layer = Sdf.Layer.FindOrOpen(scene)
    variables = layer.expressionVariables
    traced = set()
    for prop, value in asset_values(layer):
        if Sdf.VariableExpression.IsExpression(value):
            value = Sdf.VariableExpression(value).Evaluate(variables).value
        if Sdf.ComputeAssetPathRelativeToLayer(layer, value) in unresolved:
            traced.add(prop)
    return {
        "layers": sorted(os.path.relpath(l.realPath, root) for l in layers),
        "assets": sorted(os.path.relpath(str(a), root) for a in assets),
        "unresolved": len(unresolved),
        "unresolved_properties": sorted(traced),
    }


def package_record(root):
    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, "out.usdz")
        if not UsdUtils.CreateNewUsdzPackage(os.path.join(root, "package.usda"), out):
            raise SystemExit("OpenUSD failed to package package.usda")
        with zipfile.ZipFile(out) as archive:
            members = archive.namelist()
            contents = {name: archive.read(name).decode() for name in members
                        if not name.endswith(".usda") and not name.endswith(".usdc")}
        layers = {}
        for member in members:
            if not (member.endswith(".usda") or member.endswith(".usdc")):
                continue
            layer = Sdf.Layer.FindOrOpen(f"{out}[{member}]")
            values = []
            for prop, value in asset_values(layer):
                tiles = UsdShade.UdimUtils.ResolveUdimTilePaths(value, layer)
                values.append({
                    "property": prop,
                    "value": value,
                    "tiles": {tile: contents[str(path).split("[", 1)[1][:-1]]
                              for path, tile in tiles},
                })
            # The root is the first member; others keep their source name.
            layers["root" if member == members[0] else member] = values
        return {"members": members, "layers": layers}


def write(out):
    os.makedirs(out, exist_ok=True)
    write_files(out)
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "spellings": [{
            "path": path,
            "is_udim": UsdShade.UdimUtils.IsUdimIdentifier(path),
            "tile_1011": UsdShade.UdimUtils.ReplaceUdimPattern(path, "1011"),
        } for path in SPELLINGS],
        "tiles": tiles_record(out),
        "dependencies": dependencies_record(out),
        "package": package_record(out),
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
        for name in list(LAYERS) + TILES + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
