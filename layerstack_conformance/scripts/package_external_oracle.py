# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD writes for an asset a package does not copy, and
how a path written into a package resolves outside it.

Usage: package_external_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/package_external` by
default, the files below and `oracle.json`, which
`tests/conformance/package_external.rs` replays against layerstack. The
fixture directory stands for a host's root, and is the current directory
while OpenUSD resolves, as a host resolves a path relative to its root.

`packaged`: `set/rock.usda` authors an asset beside it, one outside its
directory, and a relative, a search and an absolute path to files that do
not exist. OpenUSD's packaging has no way to leave an asset outside the
package: `UsdUtilsCreateNewUsdzPackage` copies the one outside the root
layer's directory in as `1/outside.png`, and so does
`UsdUtilsLocalizeAsset` when its processing function names that asset by
its absolute path (`_ProcessAssetPath` and `_AddDependenciesToPackage` in
`pxr/usd/usdUtils/assetLocalizationPackage.cpp`). The only paths it keeps
as authored are relative ones it cannot resolve, and it rewrites the
absolute one it cannot resolve to a member it does not write
(`0/gone.png`). Each records the call's result, the files written and the
asset values of the written root layer.

`anchored`: `pkg.usdz`, at the root, and `out/pkg.usdz`, below it, each
hold a `root.usda` authoring asset paths to files outside the package.
For each, `SdfComputeAssetPathRelativeToLayer` from the packaged layer and
what the resolver resolves that to: a path starting with `./` or `../`
always names a member (`pkg.usdz[set/albedo.png]`, which is not there),
and a search path that names no member is anchored beside the package
file when a file is there (`out/set/near.png`), and otherwise kept and
resolved from the root (`pxr/usd/sdf/layerUtils.cpp`,
`ArDefaultResolver::_CreateIdentifier`). So a path relative to the host's
root must be written into a package as a search path to resolve outside
it. The fixture directory is spelled `<dir>`.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixture.
"""
import argparse
import filecmp
import json
import os
import struct
import sys
import tempfile
import zipfile

from pxr import Ar, Sdf, Usd, UsdUtils

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "package_external"))

ROCK = """#usda 1.0

def "Rock"
{
    asset albedo = @./albedo.png@
    asset missingAbsolute = @/package_external_missing/gone.png@
    asset missingRelative = @./gone.png@
    asset missingSearch = @gone.png@
    asset outside = @../far/outside.png@
}
"""

PROBE = """#usda 1.0

def "Probe"
{
    asset besidePackage = @set/near.png@
    asset layerRelative = @./set/albedo.png@
    asset nowhere = @set/nowhere.png@
    asset parentRelative = @../set/albedo.png@
    asset searchPath = @set/albedo.png@
}
"""

LAYERS = {"set/rock.usda": ROCK}
FILES = ["set/albedo.png", "far/outside.png", "out/set/near.png"]
PACKAGES = ["pkg.usdz", "out/pkg.usdz"]


def package(members, path):
    """Packages the members as `usdzip` lays them out: stored, with every
    member's data 64-byte aligned by a `0x1986` extra field."""
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_STORED) as z:
        for name, text in members:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            pad = (-(z.fp.tell() + 30 + len(name.encode()) + 4)) % 64
            info.extra = struct.pack("<HH", 0x1986, pad) + bytes(pad)
            z.writestr(info, text.encode())


def asset_values(layer):
    """The asset values `layer` authors on `/Rock`, by attribute."""
    prim = layer.GetPrimAtPath("/Rock")
    return {a.name: a.default.path for a in prim.attributes}


def packaged(directory):
    root = os.path.join(directory, "set", "rock.usda")
    with tempfile.TemporaryDirectory() as work:
        usdz = os.path.join(work, "rock.usdz")
        created = UsdUtils.CreateNewUsdzPackage(Sdf.AssetPath(root), usdz)
        with zipfile.ZipFile(usdz) as z:
            members = z.namelist()
        layer = Sdf.Layer.FindOrOpen(usdz)
        usdz_values = asset_values(layer)

        outside = os.path.join(directory, "far", "outside.png")

        def keep_outside(layer, info):
            if info.assetPath.endswith("outside.png"):
                return UsdUtils.DependencyInfo(outside)
            return info

        localized = os.path.join(work, "localized")
        result = UsdUtils.LocalizeAsset(Sdf.AssetPath(root), localized,
                                        processingFunc=keep_outside)
        files = sorted(
            os.path.relpath(os.path.join(d, f), localized).replace(os.sep, "/")
            for d, _, names in os.walk(localized) for f in names)
        localized_values = asset_values(Sdf.Layer.FindOrOpen(
            os.path.join(localized, "rock.usda")))
    return {
        "create_new_usdz_package": {
            "result": created, "members": members, "values": usdz_values},
        "localize_asset_keeping_outside": {
            "result": result, "files": files, "values": localized_values},
    }


def anchored(directory):
    resolver = Ar.GetResolver()
    found = {}
    for name in PACKAGES:
        layer = Sdf.Layer.FindOrOpen(os.path.join(directory, name) + "[root.usda]")
        prim = layer.GetPrimAtPath("/Probe")
        found[name] = {}
        for attribute in prim.attributes:
            authored = attribute.default.path
            identifier = Sdf.ComputeAssetPathRelativeToLayer(layer, authored)
            resolved = str(resolver.Resolve(identifier))
            found[name][attribute.name] = {
                "authored": authored,
                "anchored": identifier.replace(directory + "/", "<dir>/"),
                "resolved": resolved.replace(directory + "/", "<dir>/"),
            }
    return found


def write(out):
    out = os.path.realpath(out)
    for name, text in LAYERS.items():
        path = os.path.join(out, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
    for name in FILES:
        path = os.path.join(out, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        # Not empty: OpenUSD's zip writer cannot map an empty file.
        with open(path, "w") as f:
            f.write(name + "\n")
    for name in PACKAGES:
        package([("root.usda", PROBE)], os.path.join(out, name))
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)
    previous = os.getcwd()
    os.chdir(out)
    try:
        _, minor, patch = Usd.GetVersion()
        oracle = {
            "openusd": f"{minor}.{patch}",
            "packaged": packaged(out),
            "anchored": anchored(out),
        }
    finally:
        os.chdir(previous)
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
        for name in list(LAYERS) + FILES + PACKAGES + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
