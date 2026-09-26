# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD composes from layers inside USDZ packages.

Usage: usdz_packages_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/usdz_packages` by default:

- `<case>/<member>`: the members of each package, `.usda` as authored and
  `.usdc` exported from the text below by OpenUSD;
- `<case>_outside/<file>`: the layers beside a package, outside it, which
  paths naming no member resolve to;
- `oracle.json`: for each case, its members in package order (the root
  layer first) and what OpenUSD composes from the package, which
  `tests/usdz_packages.rs` replays against layerstack after packaging the
  same members in the same order.

Every layer a package member loads, directly or beneath another member,
contributes to the stage (AOUSD Core §16.4, §9.7):

- `reference`, `payload`: the root layer references or payloads a member.
- `sublayers`: a sublayer of a sublayer references a member, three layers
  below the root.
- `mixed`: `.usda` and `.usdc` members reference and payload each other.
- `repeated`: several prims and layers reference the same member, which
  loads once.
- `cycle`: two members reference each other, without a composition cycle.
- `external`: members and layers beside the package reference each other's
  layers, interleaved: a search path naming no member resolves beside the
  package, and a layer found there loads its own references there.

For every composed prim the vectors record its prim stack, as package
member paths (and paths relative to the package's directory for layers
outside it), and its children; for every attribute, its resolved default
(`null` for no value); and the number of composition errors.
"""
import json
import os
import shutil
import struct
import sys
import tempfile
import zipfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(
    os.path.join(HERE, "..", "fixtures", "usdz_packages"))

# Each case is a package: its members in package order, the root first. A
# `.usdc` member is authored as text here and exported as crate.
CASES = {
    "reference": [
        ("root.usda", '''#usda 1.0

def "World" (
    references = @asset.usda@</Asset>
)
{
}
'''),
        ("asset.usda", '''#usda 1.0

def "Asset"
{
    double value = 7
}
'''),
    ],
    "payload": [
        ("root.usda", '''#usda 1.0

def "World" (
    payload = @asset.usda@</Asset>
)
{
}
'''),
        ("asset.usda", '''#usda 1.0

def "Asset"
{
    double value = 7
}
'''),
    ],
    "sublayers": [
        ("root.usda", '''#usda 1.0
(
    subLayers = [
        @middle.usda@
    ]
)

over "Shared"
{
    double top = 1
}
'''),
        ("middle.usda", '''#usda 1.0
(
    subLayers = [
        @bottom.usda@
    ]
)

def "Shared"
{
    double middle = 2
}
'''),
        ("bottom.usda", '''#usda 1.0

def "Shared"
{
    double middle = 20
    double bottom = 3
}

def "Deep" (
    references = @leaf.usda@</Leaf>
)
{
}
'''),
        ("leaf.usda", '''#usda 1.0

def "Leaf"
{
    double depth = 4
}
'''),
    ],
    "mixed": [
        ("root.usda", '''#usda 1.0
(
    subLayers = [
        @base.usdc@
    ]
)

def "World" (
    references = @shape.usdc@</Shape>
)
{
}
'''),
        ("base.usdc", '''#usda 1.0

def "Ground"
{
    double level = 1
}
'''),
        ("shape.usdc", '''#usda 1.0

def "Shape" (
    references = @detail.usda@</Detail>
)
{
    double size = 2
}
'''),
        ("detail.usda", '''#usda 1.0

def "Detail" (
    payload = @finish.usdc@</Finish>
)
{
    double grain = 3
}
'''),
        ("finish.usdc", '''#usda 1.0

def "Finish"
{
    double gloss = 4
}
'''),
    ],
    "repeated": [
        ("root.usda", '''#usda 1.0

def "First" (
    references = @asset.usda@</Asset>
)
{
}

def "Second" (
    references = @asset.usda@</Asset>
)
{
}

def "Third" (
    references = @part.usda@</Part>
)
{
}
'''),
        ("part.usda", '''#usda 1.0

def "Part" (
    references = [
        @asset.usda@</Asset>,
        @./asset.usda@</Other>
    ]
)
{
}
'''),
        ("asset.usda", '''#usda 1.0

def "Asset"
{
    double value = 7
}

def "Other"
{
    double other = 8
}
'''),
    ],
    "cycle": [
        ("root.usda", '''#usda 1.0

def "World" (
    references = @ping.usda@</Ping>
)
{
}
'''),
        ("ping.usda", '''#usda 1.0

def "Ping" (
    references = @pong.usda@</Pong>
)
{
    double ping = 1
}

def "Echo"
{
    double echo = 3
}
'''),
        ("pong.usda", '''#usda 1.0

def "Pong" (
    references = @ping.usda@</Echo>
)
{
    double pong = 2
}
'''),
    ],
    "external": [
        ("root.usda", '''#usda 1.0

def "World" (
    references = [
        @member.usda@</Member>,
        @outside.usda@</Outside>
    ]
)
{
}

def "Again" (
    payload = @outside.usda@</Outside>
)
{
}
'''),
        ("member.usda", '''#usda 1.0

def "Member" (
    references = [
        @further.usda@</Further>,
        @second.usda@</Second>
    ]
)
{
    double member = 1
}
'''),
        ("second.usda", '''#usda 1.0

def "Second"
{
    double second = 2
}
'''),
    ],
}


# Layers beside a package, outside it, by case.
OUTSIDE = {
    "external": [
        ("outside.usda", '''#usda 1.0

def "Outside" (
    references = @further.usda@</Further>
)
{
    double outside = 3
}
'''),
        ("further.usda", '''#usda 1.0

def "Further"
{
    double further = 4
}
'''),
    ],
}


def write_members(directory, members):
    """Writes each member under `directory`, exporting `.usdc` members."""
    for name, text in members:
        path = os.path.join(directory, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        if name.endswith(".usdc"):
            layer = Sdf.Layer.CreateAnonymous(".usda")
            layer.ImportFromString(text)
            if not layer.Export(path):
                sys.exit(f"could not export {path}")
        else:
            with open(path, "w") as f:
                f.write(text)


def package(directory, members, out_path):
    """Packages the members as `usdzip` lays them out: stored, with every
    member's data 64-byte aligned by a `0x1986` extra field."""
    with zipfile.ZipFile(out_path, "w", compression=zipfile.ZIP_STORED) as z:
        for name, _ in members:
            with open(os.path.join(directory, name), "rb") as f:
                data = f.read()
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            pad = (-(z.fp.tell() + 30 + len(name.encode()) + 4)) % 64
            info.extra = struct.pack("<HH", 0x1986, pad) + bytes(pad)
            z.writestr(info, data)


def member_name(identifier, package_path, root):
    """The package member a layer identifier names: `pkg.usdz[a/b.usda]` is
    `a/b.usda`, and the package itself is its root layer. A layer outside
    the package is named by its path relative to the package's directory."""
    if identifier.endswith("]"):
        return identifier[identifier.index("[") + 1:-1]
    path = os.path.realpath(identifier)
    if path == os.path.realpath(package_path):
        return root
    return os.path.relpath(path, os.path.dirname(os.path.realpath(package_path)))


def json_value(value):
    if value is None or isinstance(value, float):
        return value
    sys.exit(f"unexpected value {value!r}")


def compose(package_path, root):
    stage = Usd.Stage.Open(package_path)
    predicate = Usd.PrimAllPrimsPredicate
    prims = []
    values = {}
    for prim in Usd.PrimRange.Stage(stage, predicate):
        prims.append({
            "path": str(prim.GetPath()),
            "prim_stack": [[member_name(spec.layer.identifier, package_path,
                                        root),
                            str(spec.path)]
                           for spec in prim.GetPrimStack()],
            "children": [child.GetName()
                         for child in prim.GetFilteredChildren(predicate)],
        })
        for attr in prim.GetAttributes():
            values[str(attr.GetPath())] = json_value(attr.Get())
    return {
        "prims": prims,
        "values": values,
        "composition_errors": len(stage.GetCompositionErrors()),
    }


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    version = ".".join(str(v) for v in Usd.GetVersion())
    cases = []
    scratch = tempfile.mkdtemp()
    try:
        for case, members in CASES.items():
            directory = os.path.join(out_dir, case)
            shutil.rmtree(directory, ignore_errors=True)
            write_members(directory, members)
            outside = OUTSIDE.get(case, [])
            beside = os.path.join(scratch, case)
            os.makedirs(beside)
            write_members(beside, outside)
            if outside:
                outside_dir = os.path.join(out_dir, f"{case}_outside")
                shutil.rmtree(outside_dir, ignore_errors=True)
                write_members(outside_dir, outside)
            package_path = os.path.join(beside, f"{case}.usdz")
            package(directory, members, package_path)
            root = members[0][0]
            cases.append({
                "name": case,
                "members": [name for name, _ in members],
                **compose(package_path, root),
            })
    finally:
        shutil.rmtree(scratch)
    doc = {
        "generator": "layerstack_conformance/scripts/usdz_packages_oracle.py",
        "openusd_version": version,
        "cases": cases,
    }
    out_path = os.path.join(out_dir, "oracle.json")
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print(f"wrote {len(cases)} packages from OpenUSD {version} to {out_path}")


if __name__ == "__main__":
    main()
