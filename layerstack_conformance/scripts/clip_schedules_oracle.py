# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records, call by call, which clips OpenUSD schedules, opens and reads,
and when it calls a clipped attribute time-varying.

Usage: clip_schedules_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/clip_schedules` by default,
the layers below and `oracle.json`, which
`tests/conformance/clip_schedules.rs` replays against layerstack, loading
what each clip query plan demands.

`root.usda` sublayers `weak.usda`, which gives every clipped attribute a
weaker default of 42, and defines one prim per clip set:

- `Multi`: two clips, the second unparsable (`broken.usda`);
- `SingleSame`, `SingleTwo`: one clip, with one sample and with two;
- `RepeatOne`: one clip activated twice;
- `DupActive`: two activations at one time, which OpenUSD rejects
  (`_ValidateClipFields`, `pxr/usd/usd/clipSet.cpp:246-346`);
- `TripleTimes`: three `times` entries at one stage time, rejected too;
- `DoubleTimes`: a jump discontinuity inside `times`;
- `FirstJump`: a jump discontinuity that starts `times`, which OpenUSD
  drops (`clipSet.cpp:460-485`);
- `UnschedBroken`: an unscheduled clip that does not parse;
- `UnschedNoManifest`, `UnschedBrokenNoManifest`: an unscheduled clip
  without a manifest: OpenUSD generates the manifest from the scheduled
  clips alone (`clipSet.cpp:416-430`, `:537`), so `extra`, which only the
  unscheduled `clipExtra.usda` declares, reads the weaker 42;
- `FracIndex`: a fractional clip index, `(0, 0.5)`, which OpenUSD
  truncates to clip 0 (`clipSet.cpp:418`);
- `UnsortedActive`: activations authored out of order;
- `PropPrimPath`, `RelPrimPath`: a `primPath` naming a property or
  relative, which OpenUSD rejects.

It records, in order on one stage:

- `open`: the clip and manifest layers loaded once the stage is open
  (`Sdf.Layer.GetLoadedLayers`), the manifests OpenUSD generates
  included;
- `vary`: for each attribute, before any value is read,
  `UsdAttribute::ValueMightBeTimeVarying` and the layers loaded after it;
- `get`: for each attribute, `UsdAttribute::Get` at each time of
  `TIMES` (`"error"` where the call raises, as a read that needs a clip
  that does not parse does) and the layers loaded after the attribute's
  reads;
- `samples`: `UsdAttribute::GetTimeSamples` once everything is read.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixture.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "clip_schedules"))

TIMES = [0, 5, 9.5, 10, 15, 25]


def clip_set(paths, active, manifest=True, extra=""):
    lines = [
        "            asset[] assetPaths = [%s]" % ", ".join("@./%s@" % p for p in paths),
    ]
    if manifest:
        lines.append("            asset manifestAssetPath = @./manifest.usda@")
    lines.append('            string primPath = "/Model"')
    lines.append("            double2[] active = %s" % active)
    if extra:
        lines.append("            " + extra)
    return "\n".join(lines)


# Each case: prim name, its clip set's fields.
CASES = [
    ("Multi", clip_set(["clipA.usda", "broken.usda"], "[(0, 0), (10, 1)]")),
    ("SingleSame", clip_set(["clipZero.usda"], "[(0, 0)]")),
    ("SingleTwo", clip_set(["clipFive.usda"], "[(0, 0)]")),
    ("RepeatOne", clip_set(["clipZero.usda"], "[(0, 0), (10, 0)]")),
    ("DupActive", clip_set(["clipA.usda", "clipB.usda"], "[(0, 0), (0, 1)]")),
    ("TripleTimes", clip_set(
        ["clipLin.usda"], "[(0, 0)]",
        extra="double2[] times = [(0, 0), (10, 10), (10, 0), (10, 5), (20, 10)]")),
    ("DoubleTimes", clip_set(
        ["clipLin.usda"], "[(0, 0)]",
        extra="double2[] times = [(0, 0), (10, 10), (10, 0), (20, 10)]")),
    ("FirstJump", clip_set(
        ["clipLin.usda"], "[(0, 0)]",
        extra="double2[] times = [(10, 10), (10, 0), (20, 10)]")),
    ("UnschedBroken", clip_set(["clipA.usda", "broken.usda"], "[(0, 0)]")),
    ("UnschedNoManifest", clip_set(["clipA.usda", "clipExtra.usda"], "[(0, 0)]", manifest=False)),
    ("UnschedBrokenNoManifest", clip_set(["clipA.usda", "broken.usda"], "[(0, 0)]", manifest=False)),
    ("FracIndex", clip_set(["clipA.usda", "clipB.usda"], "[(0, 0.5), (10, 1)]")),
    ("UnsortedActive", clip_set(["clipA.usda", "clipB.usda"], "[(10, 1), (0, 0)]")),
    ("PropPrimPath", clip_set(["clipA.usda"], "[(0, 0)]").replace('"/Model"', '"/Model.size"')),
    ("RelPrimPath", clip_set(["clipA.usda"], "[(0, 0)]").replace('"/Model"', '"Model"')),
]
# The attributes read, in order.
ATTRIBUTES = ["/%s.size" % name for name, _ in CASES] + ["/UnschedNoManifest.extra"]


def model(body):
    return '#usda 1.0\n\ndef "Model"\n{\n%s}\n' % "".join("    %s\n" % line for line in body)


LAYERS = {
    "root.usda": "#usda 1.0\n(\n    subLayers = [@./weak.usda@]\n)\n" + "".join(
        '\ndef "%s" (\n    clips = {\n        dictionary default = {\n%s\n        }\n    }\n)\n{\n}\n'
        % (name, fields)
        for name, fields in CASES
    ),
    "weak.usda": "#usda 1.0\n" + "".join(
        '\nover "%s"\n{\n    float size = 42\n%s}\n'
        % (name, "    float extra = 42\n" if name == "UnschedNoManifest" else "")
        for name, _ in CASES
    ),
    "manifest.usda": model(["float size"]),
    "clipA.usda": model(["float size.timeSamples = { 0: 1, 5: 2 }"]),
    "clipB.usda": model(["float size.timeSamples = { 10: 3 }"]),
    "clipExtra.usda": model([
        "float size.timeSamples = { 0: 8 }",
        "float extra.timeSamples = { 0: 9, 5: 10 }",
    ]),
    "clipFive.usda": model(["float size.timeSamples = { 5: 7 }"]),
    "clipZero.usda": model(["float size.timeSamples = { 0: 7 }"]),
    "clipLin.usda": model(["float size.timeSamples = { 0: 0, 10: 10, 20: 20 }"]),
    "broken.usda": "#usda 1.0\ndef !!!\n",
}
# The layers whose loading the oracle tracks.
TRACKED = sorted(name for name in LAYERS if name not in ("root.usda", "weak.usda"))


def loaded(directory):
    """The tracked layers loaded now, by file name."""
    names = set()
    for layer in Sdf.Layer.GetLoadedLayers():
        path = layer.realPath
        if path and os.path.dirname(path) == directory and os.path.basename(path) in TRACKED:
            names.add(os.path.basename(path))
    return sorted(names)


def number(value):
    return None if value is None else float(value)


def record(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    oracle = {"open": loaded(directory), "vary": [], "get": [], "samples": []}
    for path in ATTRIBUTES:
        attribute = stage.GetAttributeAtPath(path)
        oracle["vary"].append({
            "attribute": path,
            "might_vary": attribute.ValueMightBeTimeVarying(),
            "loaded": loaded(directory),
        })
    for path in ATTRIBUTES:
        attribute = stage.GetAttributeAtPath(path)
        values = []
        for time in TIMES:
            try:
                values.append(number(attribute.Get(time)))
            except Exception:
                values.append("error")
        oracle["get"].append({"attribute": path, "values": values, "loaded": loaded(directory)})
    for path in ATTRIBUTES:
        attribute = stage.GetAttributeAtPath(path)
        oracle["samples"].append({"attribute": path, "times": list(attribute.GetTimeSamples())})
    return oracle


def write(out):
    out = os.path.realpath(out)
    os.makedirs(out, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
    for layer in list(Sdf.Layer.GetLoadedLayers()):
        if not layer.anonymous:
            layer.Reload(force=True)
    _, minor, patch = Usd.GetVersion()
    oracle = {"openusd": f"{minor}.{patch}", "times": TIMES}
    oracle.update(record(out))
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
