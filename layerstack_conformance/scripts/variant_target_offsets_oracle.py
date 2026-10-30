# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records which time a sample lands at through edit targets of a variant
in a sublayer with a layer offset, one call per case.

Usage: variant_target_offsets_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/variant_target_offsets` by
default, the layers below and `oracle.json`, which
`tests/conformance/variant_target_offsets.rs` replays against layerstack.

The root layer sublayers `sub.usda` with `(offset = 10)`; `/Robot`, a
prim of the root, selects the variant `{finish=painted}`, whose branch
`sub.usda` authors. Each case opens the stage afresh, points it at one
edit target in `sub.usda` and makes one call, `/Robot.spin`'s
`UsdAttribute::Set(70, 15)` at stage time 15. It records whether the call
was accepted, the spec path the target maps `/Robot.spin` to, the times of
that spec's samples in `sub.usda`, and the stage's composed sample times:

- `local_direct_variant`: `UsdEditTarget::ForLocalDirectVariant(sub,
  /Robot{finish=painted})`, whose map function has no time offset, so the
  sample lands at layer time 15 (read at stage time 25).
- `local_direct_variant_unselected`: the same for `{finish=chrome}`, which
  is not selected.
- `variant_node`: `UsdEditTarget(sub, node)` for the prim index's variant
  node, whose map function carries the sublayer's offset: layer time 5.
- `local_layer`: `UsdStage::GetEditTargetForLocalLayer(sub)`, outside the
  variant: layer time 5.

`--check` rewrites everything into a temporary directory and fails unless
the layers and `oracle.json` match the checked-in fixture.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "variant_target_offsets"))

LAYERS = {
    "scene.usda": '''#usda 1.0
(
    subLayers = [
        @sub.usda@ (offset = 10)
    ]
)

def "Robot" (
    variants = {
        string finish = "painted"
    }
    prepend variantSets = "finish"
)
{
    double spin
}
''',
    "sub.usda": '''#usda 1.0

over "Robot"
{
    variantSet "finish" = {
        "chrome" {

        }
        "painted" {

        }
    }
}
''',
}

CASES = [
    ("local_direct_variant", "/Robot{finish=painted}"),
    ("local_direct_variant_unselected", "/Robot{finish=chrome}"),
    ("variant_node", None),
    ("local_layer", None),
]


def reload_layers(directory):
    for name in LAYERS:
        layer = Sdf.Layer.Find(os.path.join(directory, name))
        if layer:
            layer.Reload(force=True)


def edit_target(stage, sub, name, variant):
    if variant is not None:
        return Usd.EditTarget.ForLocalDirectVariant(sub, Sdf.Path(variant))
    if name == "local_layer":
        return stage.GetEditTargetForLocalLayer(sub)
    stack = [stage.GetPrimAtPath("/Robot").GetPrimIndex().rootNode]
    while stack:
        node = stack.pop(0)
        if str(node.arcType).lower().endswith("variant"):
            return Usd.EditTarget(sub, node)
        stack.extend(node.children)
    raise LookupError("no variant node")


def write(out):
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    cases = []
    for name, variant in CASES:
        reload_layers(out)
        stage = Usd.Stage.Open(os.path.join(out, "scene.usda"))
        sub = Sdf.Layer.Find(os.path.join(out, "sub.usda"))
        target = edit_target(stage, sub, name, variant)
        stage.SetEditTarget(target)
        attr = stage.GetPrimAtPath("/Robot").GetAttribute("spin")
        try:
            accepted = bool(attr.Set(70.0, 15.0))
        except Exception:
            accepted = False
        spec_path = target.MapToSpecPath(attr.GetPath())
        spec = sub.GetPropertyAtPath(spec_path)
        cases.append({
            "name": name,
            "variant": variant,
            "accepted": accepted,
            "spec_path": str(spec_path),
            "layer_times": sorted(sub.ListTimeSamplesForPath(spec_path)) if spec else [],
            "stage_times": list(attr.GetTimeSamples()),
        })
    reload_layers(out)
    _, minor, patch = Usd.GetVersion()
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump({"openusd_version": f"{minor}.{patch}", "cases": cases}, f, indent=1, sort_keys=True)
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
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name), shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
