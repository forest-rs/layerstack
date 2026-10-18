# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD does when a scene's dependencies are missing.

Usage: missing_assets_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/missing_assets` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from them, which
  `tests/missing_assets.rs` replays against layerstack's `StageDocument`.

`scene.usda` names assets that do not exist: a sublayer, a reference, a
reference beside a valid sibling reference, a payload, a reference inside
the variant its prim selects, and a reference inside a referenced layer.
`broken.usda` references a layer that exists but does not parse, beside a
valid sibling. `missing.usda` is not written at all: opening it as the
root fails.

Each case is a `Usd.Stage.Open`, with every payload loaded or none. It
records whether the stage opened; each composition error, by its
`PcpErrorType` name, the composed prim (`rootSite`), the arc, the asset's
file name, and the layer and spec that introduce the arc, read from the
error's message (the Python wrappers expose only `errorType` and
`rootSite`); and every prim `TraverseAll` reaches, with its prim stack and
its attributes' default values.

The `reloaded` case writes the missing assets (`APPEARING`) beside the
others and calls `UsdStage::Reload`, which retries every invalid asset and
sublayer (`PcpCache::Reload`). `loaded_later` opens with no payloads
loaded, writes them, and calls `UsdStage::Load` on the payload's prim.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import re
import shutil
import tempfile

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "missing_assets"))

LAYERS = {
    "scene.usda": '''#usda 1.0
(
    subLayers = [
        @./absent_layer.usda@,
        @./terrain.usda@
    ]
)

# A missing reference: the arc contributes nothing, the local opinion stays.
def "Rock" (
    references = @./absent_rock.usda@</Rock>
)
{
    int size = 1
}

# A missing reference beside a valid one: the valid sibling still loads.
def "Pair" (
    references = [
        @./absent_rock.usda@</Rock>,
        @./plant.usda@</Plant>
    ]
)
{
}

# A missing payload, reported only while the payload is loaded.
def "Seed" (
    payload = @./absent_seed.usda@</Seed>
)
{
    int count = 2
}

# A missing reference inside the variant the prim selects.
def "Moss" (
    variants = {
        string look = "mossy"
    }
    prepend variantSets = "look"
)
{
    variantSet "look" = {
        "bare" {
        }
        "mossy" (
            references = @./absent_moss.usda@</Moss>
        )
        {
            int tufts = 3
        }
    }
}

# A missing reference inside a referenced layer.
def "Bed" (
    references = @./bed.usda@</Bed>
)
{
}
''',
    "terrain.usda": '''#usda 1.0

def "Hill"
{
    int height = 4
}
''',
    "plant.usda": '''#usda 1.0

def "Plant"
{
    int leaves = 5
}
''',
    "bed.usda": '''#usda 1.0

def "Bed" (
    references = @./absent_soil.usda@</Soil>
)
{
    int width = 6
}
''',
    "broken.usda": '''#usda 1.0

# A dependency that exists but does not parse, beside a valid one.
def "Cairn" (
    references = [
        @./malformed.usda@</Stone>,
        @./plant.usda@</Plant>
    ]
)
{
}
''',
    "malformed.usda": '''#usda 1.0

def "Stone"
{
    int weight =
}
''',
}

# The missing assets, as the `reloaded` and `loaded_later` cases write them.
APPEARING = {
    "absent_layer.usda": '''#usda 1.0

def "Valley"
{
    int depth = 7
}
''',
    "absent_rock.usda": '''#usda 1.0

def "Rock"
{
    int size = 8
    int grain = 9
}
''',
    "absent_seed.usda": '''#usda 1.0

def "Seed"
{
    int sprouts = 10
}
''',
    "absent_moss.usda": '''#usda 1.0

def "Moss"
{
    int spores = 11
}
''',
    "absent_soil.usda": '''#usda 1.0

def "Soil"
{
    int depth = 12
}
''',
}

ASSET_MESSAGE = re.compile(
    r"Could not open asset @(?P<asset>[^@]*)@ for (?P<arc>reference|payload) "
    r"introduced by @(?P<layer>[^@]*)@<(?P<site>[^>]*)>")
SUBLAYER_MESSAGE = re.compile(
    r"Could not load sublayer @(?P<asset>[^@]*)@ of layer @(?P<layer>[^@]*)@")


def error_record(error):
    kind = str(error.errorType).split("_", 1)[1]
    text = str(error)
    record = {"type": kind}
    if kind == "InvalidAssetPath":
        match = ASSET_MESSAGE.match(text)
        record.update({
            "prim": str(error.rootSite.path),
            "arc": match["arc"],
            "asset": os.path.basename(match["asset"]),
            "introducing_layer": os.path.basename(match["layer"]),
            "introducing_spec": match["site"],
        })
    elif kind == "InvalidSublayerPath":
        match = SUBLAYER_MESSAGE.match(text)
        record.update({
            "asset": os.path.basename(match["asset"]),
            "layer": os.path.basename(match["layer"]),
        })
    else:
        raise ValueError(text)
    return record


def stage_record(stage):
    errors = sorted((error_record(e) for e in stage.GetCompositionErrors()),
                    key=lambda e: json.dumps(e, sort_keys=True))
    prims = {}
    for prim in stage.TraverseAll():
        prims[str(prim.GetPath())] = {
            "stack": [[os.path.basename(spec.layer.identifier), str(spec.path)]
                      for spec in prim.GetPrimStack()],
            "attributes": {a.GetName(): a.Get() for a in prim.GetAttributes()},
        }
    return {"opened": True, "errors": errors, "prims": prims}


def open_record(path, load):
    try:
        stage = Usd.Stage.Open(path, load)
    except Exception:  # pxr raises Tf.ErrorException
        stage = None
    if not stage:
        return {"opened": False}
    return stage_record(stage)


def write_layers(out, layers):
    for name, text in layers.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)


def cases(out):
    load_all, load_none = Usd.Stage.LoadAll, Usd.Stage.LoadNone
    records = {
        "load_all": open_record(os.path.join(out, "scene.usda"), load_all),
        "load_none": open_record(os.path.join(out, "scene.usda"), load_none),
        "broken": open_record(os.path.join(out, "broken.usda"), load_all),
        "missing_root": open_record(os.path.join(out, "missing.usda"), load_all),
    }
    # The assets appear after opening, each case in its own copy so the
    # fixture keeps them missing and the cases share no layers.
    for case, load, then in [("reloaded", load_all, lambda s: s.Reload()),
                             ("loaded_later", load_none, lambda s: s.Load("/Seed"))]:
        with tempfile.TemporaryDirectory() as tmp:
            for name in LAYERS:
                shutil.copy(os.path.join(out, name), tmp)
            stage = Usd.Stage.Open(os.path.join(tmp, "scene.usda"), load)
            write_layers(tmp, APPEARING)
            then(stage)
            records[case] = stage_record(stage)
    return records


def write(out):
    os.makedirs(out, exist_ok=True)
    write_layers(out, LAYERS)
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "appearing": APPEARING,
        "cases": cases(out),
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
        for name in list(LAYERS) + ["oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
