# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD anchors asset paths when it flattens a stage read
from files, and how it resolves the asset paths of stage metadata.

Usage: flatten_anchoring_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/flatten_anchoring` by
default, the layers below, the empty files they name, and `oracle.json`,
which `tests/conformance/flatten_anchoring.rs` replays against layerstack.

The scene: `root.usda` authors asset paths relative to itself, search
paths found beside it and found nowhere, an absolute path, an empty one
and expressions, in `customLayerData` and on `/World`, which references
`assets/asset.usda</Asset>`; the asset authors its own, in values,
arrays, time samples and `customData`, among them a search path found
beside the root but not beside the asset. The root's sublayer
`sub/sub.usda` authors layer metadata, which the stage ignores, and
asset paths on `/World`. Two session layers stand in other directories:
`sess/session.usda` authors `customLayerData` whose keys partly override
the root's, an expression variable the root's expressions then read,
`upAxis`, `endTimeCode` and an asset path on `/World`; and
`rate/rate.usda` authors `timeCodesPerSecond`. A third,
`stack/session.usda`, has a sublayer, `stack/weak/weak.usda`, which joins
the session layer stack: its asset paths anchor to it, and its layer
metadata is ignored. A fourth, `pkg/session.usda`, defines
`/World/Packaged`, referencing a member of the package `pkg/pkg.usdz` and
the package itself, whose layers author search paths found beside the
member, beside the package's root layer, beside the package file and
nowhere: OpenUSD looks inside the package first
(`SdfComputeAssetPathRelativeToLayer` in `pxr/usd/sdf/layerUtils.cpp`).

OpenUSD's flatten anchors every asset path to the layer that authors it
(`SdfAnchorAssetPaths` through `SdfComputeAssetPathRelativeToLayer`),
creating the identifier its default resolver creates
(`ArDefaultResolver::_CreateIdentifier` in
`pxr/usd/ar/defaultResolver.cpp`): a search path is anchored only when the
anchored file exists, and kept otherwise. It copies only the stage
metadata the root or session layer authors (`_CopyPrim` on the
pseudo-root, `GetAllAuthoredMetadata`). `UsdStage::GetMetadata` resolves
each asset path of the stage metadata against the layer authoring it,
evaluating an expression with the root layer stack's variables first
(`_AssetPathToStage` in `pxr/usd/usd/stage.cpp`), and the flatten writes
what it resolves.

It records, for the stage opened on `root.usda` alone (`root`), with
`sess/session.usda` as its session layer (`session`), with
`rate/rate.usda` (`rate`), with `stack/session.usda`
(`session_sublayers`) and with `pkg/session.usda` (`package`):

- `flatten`: `UsdStage::Flatten`'s `ExportToString`, without its `doc`;
- `metadata`: for each stage metadata field, `HasAuthoredMetadata` and
  `GetMetadata`, every asset path as its `authoredPath` and
  `resolvedPath`;
- `default_prim`: `GetDefaultPrim`'s path.

The fixture directory is spelled `<dir>`. `--check` rewrites everything
into a temporary directory and fails unless it matches the checked-in
fixture.
"""
import argparse
import filecmp
import json
import os
import struct
import tempfile
import zipfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "flatten_anchoring"))

LAYERS = {
    "root.usda": """#usda 1.0
(
    customLayerData = {
        dictionary inner = {
            asset deep = @./deep.png@
        }
        string notAsset = "./root.png"
        asset rootEmptyExpr = @`"${EMPTY}"`@
        asset rootExpr = @`"./${NAME}.png"`@
        asset rootMissing = @./gone.png@
        asset rootSearch = @textures/beside.png@
        asset rootSearchMissing = @textures/nowhere.png@
        asset rootTex = @./root.png@
        asset[] rootList = [@./root.png@, @textures/beside.png@]
    }
    defaultPrim = "World"
    expressionVariables = {
        string EMPTY = ""
        string NAME = "root"
    }
    upAxis = "Y"
    subLayers = [
        @./sub/sub.usda@
    ]
)

def Xform "World" (
    references = @./assets/asset.usda@</Asset>
)
{
    asset absolute = @/elsewhere/abs.png@
    asset beside = @textures/beside.png@
    asset empty = @@
    asset nowhere = @textures/nowhere.png@
    asset own = @./root.png@
    asset spelled = @textures/../textures/beside.png@
}
""",
    "sub/sub.usda": """#usda 1.0
(
    customLayerData = {
        asset subTex = @./sub.png@
    }
    framesPerSecond = 12
    startTimeCode = 3
)

over "World"
{
    asset fromSub = @../root.png@
    asset subBeside = @near.png@
}
""",
    "assets/asset.usda": """#usda 1.0

def Xform "Asset" (
    customData = {
        asset cd = @./cd.png@
        asset cdSearch = @maps/m.png@
    }
)
{
    asset[] arr = [@./tex.png@, @maps/m.png@, @maps/none.png@]
    asset inAsset = @maps/m.png@
    asset rootOnly = @textures/beside.png@
    asset sampled.timeSamples = {
        1: @maps/m.png@,
        2: @./tex.png@,
    }
    asset tex = @./tex.png@
}
""",
    "sess/session.usda": """#usda 1.0
(
    customLayerData = {
        dictionary inner = {
            asset deepSession = @./deep.png@
        }
        asset rootTex = @./sess.png@
        asset sessExpr = @`"./${NAME}.png"`@
        asset sessSearch = @stex/s.png@
        asset sessTex = @./sess.png@
    }
    endTimeCode = 10
    expressionVariables = {
        string NAME = "sess"
    }
    upAxis = "Z"
)

over "World"
{
    asset sessAttr = @./sess.png@
}
""",
    "rate/rate.usda": """#usda 1.0
(
    timeCodesPerSecond = 24
)
""",
    "stack/session.usda": """#usda 1.0
(
    subLayers = [
        @./weak/weak.usda@
    ]
)

over "World"
{
    asset stackSession = @./s.png@
}
""",
    "stack/weak/weak.usda": """#usda 1.0
(
    customLayerData = {
        asset weakTex = @./w.png@
    }
    upAxis = "Z"
)

over "World"
{
    asset weakMissing = @tex/none.png@
    asset weakOwn = @./w.png@
    asset weakSearch = @tex/w.png@
    asset weakUp = @../s.png@
}
""",
    "pkg/session.usda": """#usda 1.0

over "World"
{
    def "Packaged" (
        references = [
            @./pkg.usdz[sub/inner.usda]@</Inner>,
            @./pkg.usdz@</Root>
        ]
    )
    {
    }
}
""",
}
# Packages, each its members in package order, the root layer first.
PACKAGES = {
    "pkg/pkg.usdz": [
        ("root.usda", """#usda 1.0

def Xform "Root"
{
    asset rootBesidePackage = @near.png@
    asset rootOwn = @./own.png@
    asset rootSearch = @tex/a.png@
}
"""),
        ("sub/inner.usda", """#usda 1.0

def Xform "Inner"
{
    asset atPackageRoot = @tex/a.png@
    asset besideMember = @near.png@
    asset besidePackage = @outside.png@
    asset inBoth = @both.png@
    asset nowhere = @nowhere.png@
    asset own = @./own.png@
    asset up = @../tex/a.png@
}
"""),
        ("sub/near.png", ""),
        ("tex/a.png", ""),
        ("both.png", ""),
    ],
}
# Files the paths name, so that OpenUSD finds them; the others are missing.
FILES = [
    "root.png", "deep.png", "textures/beside.png", "sub/near.png",
    "assets/cd.png", "assets/tex.png", "assets/maps/m.png",
    "sess/sess.png", "sess/deep.png", "sess/stex/s.png",
    "stack/s.png", "stack/weak/w.png", "stack/weak/tex/w.png",
    "pkg/outside.png", "pkg/both.png", "pkg/near.png",
]
FIELDS = [
    "customLayerData", "defaultPrim", "upAxis", "startTimeCode", "endTimeCode",
    "framesPerSecond", "timeCodesPerSecond",
]
SESSIONS = {
    "root": None,
    "session": "sess/session.usda",
    "rate": "rate/rate.usda",
    "session_sublayers": "stack/session.usda",
    "package": "pkg/session.usda",
}


def encode(value, directory):
    if isinstance(value, Sdf.AssetPath):
        resolved = value.resolvedPath
        if resolved.startswith(directory + "/"):
            resolved = "<dir>/" + resolved[len(directory) + 1:]
        return {
            "authored": value.authoredPath,
            "evaluated": value.evaluatedPath,
            "resolved": resolved,
        }
    if isinstance(value, dict):
        return {"dictionary": {k: encode(v, directory) for k, v in sorted(value.items())}}
    if isinstance(value, (bool, int, float, str)):
        return value
    return [encode(item, directory) for item in value]


def strip_doc(text):
    lines = text.split("\n")
    at = next((i for i, line in enumerate(lines) if line.lstrip().startswith("doc = ")), None)
    if at is None:
        return text
    end = at
    while not lines[end].endswith('"""') or end == at and lines[end].count('"""') < 2:
        end += 1
    lines = lines[:at] + lines[end + 1:]
    if lines[1:3] == ["(", ")"]:
        lines = lines[:1] + lines[3:]
    return "\n".join(lines)


def record(directory, session):
    root = Sdf.Layer.FindOrOpen(os.path.join(directory, "root.usda"))
    if session is None:
        stage = Usd.Stage.Open(root)
    else:
        stage = Usd.Stage.Open(root, Sdf.Layer.FindOrOpen(os.path.join(directory, session)))
    errors = stage.GetCompositionErrors()
    if errors:
        raise SystemExit(f"unexpected composition errors: {errors}")
    metadata = {}
    for field in FIELDS:
        metadata[field] = {
            "authored": stage.HasAuthoredMetadata(field),
            "value": encode(stage.GetMetadata(field), directory),
        }
    flattened = stage.Flatten().ExportToString()
    return {
        "flatten": strip_doc(flattened).replace(directory + "/", "<dir>/"),
        "metadata": metadata,
        "default_prim": str(stage.GetDefaultPrim().GetPath()),
    }


def package(members, path):
    """Packages the members as `usdzip` lays them out: stored, with every
    member's data 64-byte aligned by a `0x1986` extra field."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_STORED) as z:
        for name, text in members:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            pad = (-(z.fp.tell() + 30 + len(name.encode()) + 4)) % 64
            info.extra = struct.pack("<HH", 0x1986, pad) + bytes(pad)
            z.writestr(info, text.encode())


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
        open(path, "w").close()
    for name, members in PACKAGES.items():
        package(members, os.path.join(out, name))
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)
    _, minor, patch = Usd.GetVersion()
    oracle = {"openusd": f"{minor}.{patch}"}
    for name, session in SESSIONS.items():
        oracle[name] = record(out, session)
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
        for name in list(LAYERS) + FILES + list(PACKAGES) + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
