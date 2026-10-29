# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD evaluates asset path values authored as variable
expressions, reading and flattening.

Usage: asset_expressions_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/asset_expressions` by
default, the layers below, the empty files they name, and `oracle.json`,
which `tests/conformance/asset_expressions.rs` replays against layerstack.

The scene: `scene.usda` sets `expressionVariables` and sublayers
`sub/sub.usda`, which sets variables of its own; `/Local` is authored in
`scene.usda`, `/Sub` in the sublayer, and `/Root` references
`ref/ref.usda</Ref>`, whose layer sets variables too; `/Clipped` reads
`tex` from the value clip `clips/clip.usda`, whose samples are
expressions, evaluated with the variables of the node that introduces
the clips and anchored to the clip layer. Each of those specs
authors the same asset values: defaults, time samples and arrays whose
paths are expressions (relative, search, absolute, conditional, empty,
`None`, of another type, naming an unset variable, malformed) beside
literal and empty paths, and asset paths in `customData`, nested
dictionaries and `assetInfo`.

OpenUSD evaluates and resolves each asset path as it reads it
(`_AssetPathToStage` in `pxr/usd/usd/stage.cpp`, applied to every
default, time sample, array element and dictionary entry through
`_FieldValueToStageXf`): `SdfResolveAssetPaths`
(`pxr/usd/sdf/assetPath.cpp`) evaluates an expression with
`EvaluateTyped<std::string>` and the variables of the layer stack of the
node that authors it (`PcpLayerStack::GetExpressionVariables`), sets the
evaluated path, and resolves `GetAssetPath()` (the evaluated path, or the
authored one when that is empty) relative to the authoring layer. A path
that fails to evaluate keeps no evaluated or resolved path, and the error
is a warning naming the layer and spec.

It records:

- `read`: on each prim, each attribute's `Get()` and `Get(t)` at each
  sample, `GetCustomData()` on the prim and each attribute, and
  `GetAssetInfo()`; every asset path as its `authoredPath`,
  `evaluatedPath` and `resolvedPath` (the fixture directory spelled
  `<dir>`), and the warnings each read issues (their text after `--`,
  without the stage's address).
- `flatten`: `UsdStage::Flatten`'s `ExportToString`, without its `doc`,
  where `SdfAnchorAssetPaths` writes an evaluated path anchored to its
  layer.
- `flatten_layer_stack`: `UsdUtils.FlattenLayerStack`'s text for the
  stage's layer stack, which evaluates the expressions of attribute values
  and arcs with the layer stack's own variables, writes one that evaluates
  to no path as `@@`, and keeps the asset paths of dictionaries as
  authored (`UsdFlattenLayerStackResolveAssetPathAdvanced` and
  `_FixAssetPaths` in `pxr/usd/usd/flattenUtils.cpp`);
- the warnings each flatten issues.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixture.
"""
import argparse
import filecmp
import json
import os
import re
import tempfile

from pxr import Sdf, Usd, UsdUtils

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "asset_expressions"))

ATTRIBUTES = """\
custom asset absolute = @`"/textures/${NAME}.png"`@
custom asset[] arraySamples.timeSamples = {
    1: [@`"./${NAME}.png"`@, @`${NUM}`@],
}
custom asset blank = @@
custom asset cond = @`if(${FLAG}, "./on.png", "./off.png")`@
custom asset empty = @`"${TEX}"`@
custom asset[] gap = [@@, @`"./${NAME}.png"`@]
custom asset[] list = [@`"${TEX}"`@, @`"./${NAME}.png"`@, @./plain.png@]
custom asset malformed = @`"./${NAME.png"`@
custom asset missing = @`"./${UNSET}.png"`@
custom asset named = @`"./${NAME}.png"`@ (
    customData = {
        asset a = @`"./${NAME}.png"`@
        dictionary n = {
            asset[] l = [@`"${TEX}"`@, @`"./${DIR}/${NAME}.png"`@]
        }
        asset p = @./plain.png@
    }
)
custom asset none = @`None`@
custom asset number = @`${NUM}`@
custom asset plain = @./plain.png@
custom asset samples.timeSamples = {
    1: @`"./${NAME}.png"`@,
    2: @`"${TEX}"`@,
    3: @`"./${UNSET}.png"`@,
}
custom asset search = @`"${NAME}.png"`@
"""


def spec(name, metadata, extra=""):
    body = ATTRIBUTES + extra
    lines = [f'def "{name}" (']
    lines += ["    " + line for line in metadata.splitlines()]
    lines += [")", "{"]
    lines += [("    " + line) if line else "" for line in body.splitlines()]
    lines += ["}"]
    return "\n".join(lines) + "\n"


PRIM_METADATA = """\
assetInfo = {
    asset identifier = @`"./${NAME}.usda"`@
}
customData = {
    asset a = @`"./${NAME}.png"`@
    dictionary n = {
        asset[] l = [@`"${TEX}"`@, @`"./${DIR}/${NAME}.png"`@]
    }
    asset p = @./plain.png@
}"""

LAYERS = {
    "scene.usda": """#usda 1.0
(
    expressionVariables = {
        string DIR = "maps"
        bool FLAG = 1
        string NAME = "wood"
        int NUM = 3
        string TEX = ""
    }
    subLayers = [
        @./sub/sub.usda@
    ]
)

""" + spec("Local", PRIM_METADATA) + """
def "Root" (
    references = @./ref/ref.usda@</Ref>
)
{
}

def "Clipped" (
    clips = {
        dictionary default = {
            double2[] active = [(0, 0)]
            asset[] assetPaths = [@./clips/clip.usda@]
            asset manifestAssetPath = @./clips/manifest.usda@
            string primPath = "/Model"
            double2[] times = [(0, 0), (2, 2)]
        }
    }
)
{
    custom asset tex
}
""",
    "clips/clip.usda": """#usda 1.0

def "Model"
{
    asset tex.timeSamples = {
        0: @`"./${NAME}.png"`@,
        1: @`"${UNSET}"`@,
        2: @`${NUM}`@,
    }
}
""",
    "clips/manifest.usda": """#usda 1.0

over "Model"
{
    asset tex
}
""",
    "sub/sub.usda": """#usda 1.0
(
    expressionVariables = {
        string NAME = "ignored"
    }
)

""" + spec("Sub", PRIM_METADATA),
    "ref/ref.usda": """#usda 1.0
(
    expressionVariables = {
        string NAME = "stone"
        string ONLY = "only"
    }
)

""" + spec("Ref", PRIM_METADATA, 'custom asset only = @`"./${ONLY}.png"`@\n'),
}

# Files the evaluated paths name, so that OpenUSD resolves them.
FILES = [
    "wood.png", "on.png", "plain.png", "maps/wood.png",
    "sub/wood.png", "sub/on.png", "sub/plain.png", "sub/maps/wood.png",
    "ref/wood.png", "ref/on.png", "ref/plain.png", "ref/maps/wood.png",
    "ref/only.png", "clips/wood.png",
]

PRIMS = ["/Clipped", "/Local", "/Root", "/Sub"]


class Warnings:
    """Captures the warnings OpenUSD writes to standard error."""

    def __init__(self, directory):
        self.directory = directory
        self.path = os.path.join(directory, ".warnings")

    def __enter__(self):
        self.saved = os.dup(2)
        self.file = open(self.path, "w+")
        os.dup2(self.file.fileno(), 2)
        return self

    def __exit__(self, *exc):
        os.dup2(self.saved, 2)
        os.close(self.saved)
        self.file.seek(0)
        text = self.file.read()
        self.file.close()
        os.remove(self.path)
        self.lines = []
        for line in text.splitlines():
            if " -- " not in line:
                continue
            message = line.split(" -- ", 1)[1]
            message = re.sub(r" <0x[0-9a-f]+>", "", message)
            message = message.replace(self.directory + "/", "")
            # A 64-bit integer's type is named as the compiler demangles
            # `int64_t` (`long long` on macOS, `long` on Linux); the
            # oracle names it `int` on every platform.
            message = re.sub(r"evaluated to '(long long|long)'", "evaluated to 'int'", message)
            self.lines.append(message)


def encode(value, directory):
    if value is None:
        return None
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


def read(directory, call):
    """`call()`'s value, encoded, and the warnings it issues."""
    with Warnings(directory) as warnings:
        value = call()
    return {"value": encode(value, directory), "warnings": warnings.lines}


def prim_record(prim, directory):
    attributes = {}
    for attr in prim.GetAttributes():
        times = attr.GetTimeSamples()
        attributes[attr.GetName()] = {
            "default": read(directory, attr.Get),
            "samples": [[t, read(directory, lambda t=t: attr.Get(t))] for t in times],
            "customData": read(directory, attr.GetCustomData),
        }
    return {
        "customData": read(directory, prim.GetCustomData),
        "assetInfo": read(directory, prim.GetAssetInfo),
        "attributes": attributes,
    }


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


def relative(text, directory):
    return text.replace(directory + "/", "<dir>/")


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
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)
    stage = Usd.Stage.Open(os.path.join(out, "scene.usda"))
    errors = stage.GetCompositionErrors()
    if errors:
        raise SystemExit(f"unexpected composition errors: {errors}")
    _, minor, patch = Usd.GetVersion()
    with Warnings(out) as flatten_warnings:
        flattened = stage.Flatten().ExportToString()
    with Warnings(out) as stack_warnings:
        stack = UsdUtils.FlattenLayerStack(stage).ExportToString()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "read": {path: prim_record(stage.GetPrimAtPath(path), out) for path in PRIMS},
        "flatten": relative(strip_doc(flattened), out),
        "flatten_warnings": flatten_warnings.lines,
        "flatten_layer_stack": relative(strip_doc(stack), out),
        "flatten_layer_stack_warnings": stack_warnings.lines,
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
        for name in list(LAYERS) + FILES + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
