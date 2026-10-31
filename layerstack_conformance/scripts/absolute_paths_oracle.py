# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records, call by call, how OpenUSD makes an asset path authored in a
layer absolute: `SdfLayer::ComputeAbsolutePath`.

Usage: absolute_paths_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/absolute_paths` by default,
the files below, the package `pkg.usdz` and `oracle.json`, which
`tests/conformance/absolute_paths.rs` replays with
`StageDocument::layer_locations` (`anchor_asset_path`).

Each of `CASES` is made absolute from four layers (`ANCHORS`): a plain
layer, `scene/scene.usda`, beside which `beside.usda` exists; a layer in
a package, `pkg.usdz[sub/inner.usda]`, beside which `tex/b.usda` exists
in the package and at whose root `top.usda` does; the package itself,
whose root layer is `root.usda`; and an anonymous layer. The cases
include `./` and `../` paths, search paths found and not found, an
absolute path, a path that does not normalize to itself, and format
arguments (`:SDF_FORMAT_ARGS:`), which OpenUSD splits off before
anchoring and appends after it, outside the package brackets
(`SdfComputeAssetPathRelativeToLayer`, `pxr/usd/sdf/layerUtils.cpp`).

The fixture directory is spelled `<dir>`. `--check` rewrites everything
into a temporary directory and fails unless it matches the checked-in
fixture.
"""
import argparse
import filecmp
import io
import json
import os
import struct
import tempfile
import zipfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "absolute_paths"))

FILES = {
    "scene/scene.usda": """#usda 1.0

def "Pkg" (
    references = @../pkg.usdz@
)
{
}

def "Inner" (
    references = @../pkg.usdz[sub/inner.usda]@
)
{
}
""",
    "scene/beside.usda": "#usda 1.0\n",
    "scene/textures/wood.png": "",
    "shared.usda": "#usda 1.0\n",
}
# The package's members, its root layer first.
PACKAGE = [
    ("root.usda", '#usda 1.0\n(\n    defaultPrim = "Root"\n)\ndef "Root"\n{\n}\n'),
    ("sub/inner.usda", '#usda 1.0\n(\n    defaultPrim = "Inner"\n)\ndef "Inner"\n{\n}\n'),
    ("sub/tex/b.usda", "#usda 1.0\n"),
    ("top.usda", "#usda 1.0\n"),
]
CASES = [
    "./textures/wood.png",
    "../shared.usda",
    "beside.usda",
    "missing.usda",
    "/abs/x.usda",
    "./a/../b.usda",
    "foo.usda:SDF_FORMAT_ARGS:a=b",
    "./textures/wood.png:SDF_FORMAT_ARGS:a=b",
    "../shared.usda:SDF_FORMAT_ARGS:b=2&a=1",
    "tex/b.usda",
    "tex/b.usda:SDF_FORMAT_ARGS:a=b",
    "./tex/b.usda",
    "top.usda",
    "nowhere.usda",
    "../top.usda",
    "sub/inner.usda",
    "./sub/inner.usda",
    "wood.png",
    "./wood.png",
    "../x.usda",
]
# Each anchor: the layer, by its path in the fixture (`None` for an
# anonymous layer).
ANCHORS = {
    "scene": "scene/scene.usda",
    "inner": "pkg.usdz[sub/inner.usda]",
    "package": "pkg.usdz",
    "anonymous": None,
}


def package_bytes(members):
    """The members packaged as `usdzip` lays them out: stored, with every
    member's data 64-byte aligned by a `0x1986` extra field."""
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_STORED) as z:
        for name, data in members:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            pad = (-(z.fp.tell() + 30 + len(name.encode()) + 4)) % 64
            info.extra = struct.pack("<HH", 0x1986, pad) + bytes(pad)
            z.writestr(info, data.encode())
    return buffer.getvalue()


def write(out):
    out = os.path.realpath(out)
    for name, text in FILES.items():
        path = os.path.join(out, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
    with open(os.path.join(out, "pkg.usdz"), "wb") as f:
        f.write(package_bytes(PACKAGE))
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)
    _, minor, patch = Usd.GetVersion()
    oracle = {"openusd": f"{minor}.{patch}", "cases": CASES, "anchors": {}}
    for name, at in ANCHORS.items():
        if at is None:
            layer = Sdf.Layer.CreateAnonymous("other.usda")
        else:
            layer = Sdf.Layer.FindOrOpen(os.path.join(out, at))
        if not layer:
            raise SystemExit(f"cannot open {at}")
        oracle["anchors"][name] = [
            layer.ComputeAbsolutePath(case).replace(out + "/", "<dir>/") for case in CASES
        ]
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
        for name in list(FILES) + ["pkg.usdz", "oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
