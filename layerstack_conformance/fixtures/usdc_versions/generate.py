# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Generates the native USDC crate-version fixtures and their oracle values.

The `.usdc` files in this directory are written by OpenUSD itself, not by
Layerstack, so they are independent evidence for the crate reader in
`layerstack_usdc`. `expected.json` records what the same OpenUSD computes
for every fixture (`UsdAttribute.Get` at default and at sample times), plus
the generator provenance. It also records OpenUSD's values for the AOUSD
supplemental spec's `gen_splines.usdc`, which is read but not written here.
`layerstack_conformance/tests/usdc_versions.rs` decodes the fixtures,
composes them through `Stage`, and compares.

Pinned oracle: `usd-core` 26.8 from PyPI (OpenUSD v26.08, which writes crate
0.15.0). The script refuses to run under any other OpenUSD version so the
fixtures and the JSON can only be regenerated against that pin:

    python3 -m venv venv && venv/bin/pip install usd-core==26.8
    venv/bin/python layerstack_conformance/fixtures/usdc_versions/generate.py

Each `version_0_N.usdc` is written by a child process with
`USD_WRITE_NEW_USDC_FILES_AS_VERSION=0.N.0`, because OpenUSD reads that
setting once per process and otherwise writes the lowest version a file's
content needs (`pxr/usd/sdf/crateFile.cpp:413`). Regenerating with the
pinned version reproduces every file byte for byte.
"""

import json
import math
import os
import subprocess
import sys

from pxr import Gf, Sdf, Ts, Usd, Vt

PINNED_USD_VERSION = (0, 26, 8)
HERE = os.path.dirname(os.path.abspath(__file__))

# Query times used for every time-varying attribute.
SPLINE_TIMES = [-4.0, 0.0, 1.0, 2.5, 5.0, 7.5, 10.0, 12.5, 20.0]
# Includes every query time of `testUsdAttributeArrayEdits.cpp`.
SAMPLE_TIMES = [0.0, 1.0, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 7.0, 9.0, 10.0]


# ---------------------------------------------------------------------------
# Authoring helpers
# ---------------------------------------------------------------------------


def define(stage, path, type_name=""):
    return stage.DefinePrim(path, type_name)


def author_ordinary(stage):
    """Content every crate version from 0.8 on can represent."""
    prim = define(stage, "/Ordinary")
    prim.CreateAttribute("count", Sdf.ValueTypeNames.Int).Set(7)
    prim.CreateAttribute("weight", Sdf.ValueTypeNames.Double).Set(2.5)
    prim.CreateAttribute("mode", Sdf.ValueTypeNames.Token).Set("fast")
    prim.CreateAttribute("label", Sdf.ValueTypeNames.String).Set("hello")
    prim.CreateAttribute("offset", Sdf.ValueTypeNames.Float3).Set(
        Gf.Vec3f(1.0, 2.0, 3.0)
    )
    prim.CreateAttribute("samples", Sdf.ValueTypeNames.FloatArray).Set(
        Vt.FloatArray([1.5, 2.5, -3.25])
    )
    # Long enough (>= 16 elements) that OpenUSD compresses it.
    prim.CreateAttribute("ints", Sdf.ValueTypeNames.IntArray).Set(
        Vt.IntArray(list(range(0, 40, 2)))
    )
    animated = prim.CreateAttribute("animated", Sdf.ValueTypeNames.Double)
    animated.Set(1.0, 0.0)
    animated.Set(11.0, 10.0)


def knot(time, value, interp, **kwargs):
    return Ts.Knot(
        "double", time=time, value=value, nextInterp=interp, **kwargs
    )


def spline_v1():
    """A spline in Ts binary format 1 (crate 0.12)."""
    spline = Ts.Spline("double")
    spline.SetKnot(knot(0.0, 0.0, Ts.InterpLinear))
    spline.SetKnot(
        knot(
            5.0,
            10.0,
            Ts.InterpCurve,
            preTanWidth=1.0,
            preTanSlope=1.0,
            postTanWidth=2.0,
            postTanSlope=-1.0,
        )
    )
    # Per-knot custom data is stored after the spline blob in the crate.
    spline.SetKnot(
        knot(
            10.0,
            4.0,
            Ts.InterpHeld,
            preTanWidth=1.5,
            preTanSlope=0.5,
            customData={"note": "settle", "weight": 0.5},
        )
    )
    spline.SetPreExtrapolation(Ts.Extrapolation(Ts.ExtrapHeld))
    sloped = Ts.Extrapolation(Ts.ExtrapSloped)
    sloped.slope = 0.25
    spline.SetPostExtrapolation(sloped)
    return spline


def spline_v2():
    """A spline in Ts binary format 2 (crate 0.13): tangent algorithms."""
    spline = Ts.Spline("double")
    spline.SetKnot(
        knot(
            0.0,
            0.0,
            Ts.InterpCurve,
            postTanAlgorithm=Ts.TangentAlgorithmAutoEase,
        )
    )
    spline.SetKnot(
        knot(
            5.0,
            10.0,
            Ts.InterpCurve,
            preTanAlgorithm=Ts.TangentAlgorithmAutoEase,
            postTanAlgorithm=Ts.TangentAlgorithmAutoEase,
        )
    )
    spline.SetKnot(
        knot(
            10.0,
            2.0,
            Ts.InterpLinear,
            preTanWidth=1.0,
            preTanSlope=0.0,
            preTanAlgorithm=Ts.TangentAlgorithmCustom,
        )
    )
    spline.SetPostExtrapolation(Ts.Extrapolation(Ts.ExtrapLinear))
    return spline


def spline_v3_loop_boundary():
    """Ts binary format 3 (crate 0.15): looping with `loopBoundaryTime`."""
    spline = Ts.Spline("double")
    spline.SetKnot(knot(0.0, 0.0, Ts.InterpLinear))
    spline.SetKnot(knot(4.0, 8.0, Ts.InterpLinear))
    loop = Ts.Extrapolation(Ts.ExtrapLoopRepeat)
    loop.loopBoundaryTime = 2.0
    spline.SetPostExtrapolation(loop)
    return spline


def spline_v3_time_valued():
    """Ts binary format 3 (crate 0.15): a GfTimeCode-valued spline."""
    spline = Ts.Spline("timecode")
    spline.SetKnot(Ts.Knot("timecode", time=0.0, value=Sdf.TimeCode(0.0),
                           nextInterp=Ts.InterpLinear))
    spline.SetKnot(Ts.Knot("timecode", time=10.0, value=Sdf.TimeCode(20.0),
                           nextInterp=Ts.InterpLinear))
    return spline


def author_spline(stage, path, name, spline, type_name=Sdf.ValueTypeNames.Double):
    prim = stage.GetPrimAtPath(path) or define(stage, path)
    attr = prim.CreateAttribute(name, type_name)
    attr.SetSpline(spline)


def write_version_fixture(name):
    """Runs in a child process with `USD_WRITE_NEW_USDC_FILES_AS_VERSION` set."""
    path = os.path.join(HERE, name + ".usdc")
    stage = Usd.Stage.CreateNew(path)
    author_ordinary(stage)
    if name == "version_0_12":
        author_spline(stage, "/Animated", "curve", spline_v1())
    elif name == "version_0_13":
        author_spline(stage, "/Animated", "curve", spline_v2())
    elif name == "version_0_15":
        author_spline(stage, "/Animated", "curve", spline_v1())
    elif name == "spline_loop_boundary":
        author_spline(stage, "/Animated", "curve", spline_v3_loop_boundary())
    elif name == "spline_time_valued":
        author_spline(
            stage,
            "/Animated",
            "curve",
            spline_v3_time_valued(),
            Sdf.ValueTypeNames.TimeCode,
        )
    stage.GetRootLayer().Save()


# ---------------------------------------------------------------------------
# Array edits (crate 0.14)
# ---------------------------------------------------------------------------


def edit(builder_type, fn):
    builder = builder_type()
    fn(builder)
    return builder.FinalizeAndReset()


def int_edit(fn):
    return edit(Vt.IntArrayEditBuilder, fn)


def set_default(layer, prim_path, name, type_name, value):
    prim = layer.GetPrimAtPath(prim_path)
    if not prim:
        prim = Sdf.CreatePrimInLayer(layer, prim_path)
        prim.specifier = Sdf.SpecifierDef
    attr = prim.attributes.get(name)
    if not attr:
        attr = Sdf.AttributeSpec(prim, name, type_name)
    if value is not None:
        attr.default = value
    return attr


def set_samples(layer, prim_path, name, type_name, samples):
    attr = set_default(layer, prim_path, name, type_name, None)
    for time, value in samples:
        layer.SetTimeSample(attr.path, time, value)


# Operation vectors over the weaker dense array `OPS_BASE`. Each builder
# call is one instruction; the name describes it.
OPS_BASE = Vt.IntArray([10, 20, 30, 40, 50])
OPS = [
    ("insertLiteral", lambda b: b.Insert(99, 2)),
    ("insertNegative", lambda b: b.Insert(77, -1)),
    ("insertRef", lambda b: b.InsertRef(0, 3)),
    ("prependLiteral", lambda b: b.Prepend(5)),
    ("prependRef", lambda b: b.PrependRef(-1)),
    ("appendLiteral", lambda b: b.Append(60)),
    ("appendRef", lambda b: b.AppendRef(1)),
    ("eraseRef", lambda b: b.EraseRef(1).EraseRef(-1)),
    ("writeLiteral", lambda b: b.Write(7, 0).Write(8, -2)),
    ("writeRef", lambda b: b.WriteRef(-1, 0)),
    ("outOfBounds", lambda b: b.Write(5, 100).EraseRef(50).Insert(1, 9)),
    ("minSize", lambda b: b.MinSize(8)),
    ("minSizeFill", lambda b: b.MinSize(8, fill=7)),
    ("minSizeNoop", lambda b: b.MinSize(2, fill=7)),
    ("resizeShrink", lambda b: b.SetSize(3)),
    ("resizeGrow", lambda b: b.SetSize(7)),
    ("resizeFill", lambda b: b.SetSize(7, fill=-1)),
    ("maxSize", lambda b: b.MaxSize(2)),
    # testVtArrayEdit.py `mixAndTrim`.
    (
        "mixAndTrim",
        lambda b: b.WriteRef(-1, 2).WriteRef(0, 4).EraseRef(-1).EraseRef(0),
    ),
    # Repeated identical ops share one op-and-count header in the encoding.
    ("repeated", lambda b: b.Append(1).Append(2).Append(3).Prepend(0)),
]


def author_array_edits():
    """Writes `array_edits_weak.usdc` and `array_edits_strong.usdc`.

    The strong layer sublayers the weak one, so the pair reproduces the
    session-over-root setup of `pxr/usd/usd/testenv/testUsdAttributeArrayEdits.cpp`.
    """
    weak = Sdf.Layer.CreateNew(os.path.join(HERE, "array_edits_weak.usdc"))
    strong = Sdf.Layer.CreateNew(os.path.join(HERE, "array_edits_strong.usdc"))
    strong.subLayerPaths.append("./array_edits_weak.usdc")

    int_array = Sdf.ValueTypeNames.IntArray
    float_array = Sdf.ValueTypeNames.FloatArray

    zero_nine = int_edit(lambda b: b.Prepend(0).Append(9))
    three_three = int_edit(lambda b: b.Prepend(3).Append(3))
    six_seven = int_edit(lambda b: b.Prepend(6).Append(7))
    minus_one = int_edit(lambda b: b.Prepend(-1).Append(-1))
    minus_five = int_edit(lambda b: b.Prepend(-5).Append(-5))
    minus_nine = int_edit(lambda b: b.Prepend(-9).Append(-9))
    dense = Vt.IntArray([3, 2, 1])

    # TestBasics, stage 1: a sparse default over a dense default.
    set_default(weak, "/Basics", "attr", int_array, dense)
    set_default(strong, "/Basics", "attr", int_array, zero_nine)

    # TestBasics, stage 2: strong samples hide the strong default.
    set_default(weak, "/BasicsSamples", "attr", int_array, dense)
    set_default(strong, "/BasicsSamples", "attr", int_array, zero_nine)
    set_samples(strong, "/BasicsSamples", "attr", int_array,
                [(3.0, three_three), (6.0, six_seven)])

    # TestBasics, stage 3: sparse samples over sparse samples.
    set_default(weak, "/BasicsBothSamples", "attr", int_array, dense)
    set_samples(weak, "/BasicsBothSamples", "attr", int_array,
                [(1.0, minus_one), (5.0, minus_five), (9.0, minus_nine)])
    set_default(strong, "/BasicsBothSamples", "attr", int_array, zero_nine)
    set_samples(strong, "/BasicsBothSamples", "attr", int_array,
                [(3.0, three_three), (6.0, six_seven)])

    # TestInterpolation: interpolation of composed sparse samples.
    size4 = edit(Vt.FloatArrayEditBuilder, lambda b: b.SetSize(4))
    eight1 = edit(Vt.FloatArrayEditBuilder, lambda b: b.Write(8.0, 1))
    cheer = Vt.FloatArray([2.0, 4.0, 6.0, 8.0])
    set_samples(weak, "/InterpRoot", "attr", float_array,
                [(1.0, size4), (3.0, cheer)])
    set_samples(weak, "/Interp", "attr", float_array,
                [(1.0, size4), (3.0, cheer)])
    set_samples(strong, "/Interp", "attr", float_array, [(2.0, eight1)])

    # One attribute per operation over a dense weaker default.
    for name, fn in OPS:
        set_default(weak, "/Ops", name, int_array, OPS_BASE)
        set_default(strong, "/Ops", name, int_array, int_edit(fn))
    set_default(weak, "/Ops", "identity", int_array, OPS_BASE)
    set_default(strong, "/Ops", "identity", int_array, Vt.IntArrayEdit())

    # Literal element types other than int.
    types = [
        ("floats", Sdf.ValueTypeNames.FloatArray, Vt.FloatArray([1.5, 2.5]),
         edit(Vt.FloatArrayEditBuilder, lambda b: b.Append(3.5).Write(-1.0, 0))),
        ("doubles", Sdf.ValueTypeNames.DoubleArray, Vt.DoubleArray([0.125]),
         edit(Vt.DoubleArrayEditBuilder, lambda b: b.Prepend(1e10))),
        ("int64s", Sdf.ValueTypeNames.Int64Array, Vt.Int64Array([1]),
         edit(Vt.Int64ArrayEditBuilder, lambda b: b.Append(1 << 40))),
        ("tokens", Sdf.ValueTypeNames.TokenArray, Vt.TokenArray(["a", "b"]),
         edit(Vt.TokenArrayEditBuilder, lambda b: b.Insert("mid", 1))),
        ("strings", Sdf.ValueTypeNames.StringArray, Vt.StringArray(["x"]),
         edit(Vt.StringArrayEditBuilder, lambda b: b.Append("y").Prepend("w"))),
        ("points", Sdf.ValueTypeNames.Float3Array,
         Vt.Vec3fArray([Gf.Vec3f(0, 0, 0), Gf.Vec3f(1, 1, 1)]),
         edit(Vt.Vec3fArrayEditBuilder,
              lambda b: b.Write(Gf.Vec3f(4, 5, 6), 1).MinSize(3))),
    ]
    for name, type_name, base, sparse in types:
        set_default(weak, "/Types", name, type_name, base)
        set_default(strong, "/Types", name, type_name, sparse)

    # Edits over blocks: a blocked default, and a blocked weaker sample.
    append_nine = int_edit(lambda b: b.Append(9))
    set_default(weak, "/Block", "overDefault", int_array, Sdf.ValueBlock())
    set_default(strong, "/Block", "overDefault", int_array, append_nine)
    set_samples(weak, "/Block", "overSample", int_array,
                [(1.0, Vt.IntArray([1, 2, 3])), (4.0, Sdf.ValueBlock())])
    set_samples(strong, "/Block", "overSample", int_array,
                [(2.0, append_nine)])

    # An edit with no weaker opinion composes over the empty array.
    set_default(strong, "/NoWeaker", "attr", int_array,
                int_edit(lambda b: b.Prepend(5).MinSize(3)))

    weak.Save()
    strong.Save()


# ---------------------------------------------------------------------------
# Sublayers (any version; written at OpenUSD's default, 0.8)
# ---------------------------------------------------------------------------


def author_sublayers():
    """Writes `sublayers_root.usdc` over `sublayers_weak.usdc`.

    The root sublayers the weak layer with an offset and scale, so the pair
    checks `subLayers`, `subLayerOffsets` and time mapping through them.
    """
    weak = Sdf.Layer.CreateNew(os.path.join(HERE, "sublayers_weak.usdc"))
    root = Sdf.Layer.CreateNew(os.path.join(HERE, "sublayers_root.usdc"))
    root.subLayerPaths.append("./sublayers_weak.usdc")
    root.subLayerOffsets[0] = Sdf.LayerOffset(10.0, 2.0)

    double = Sdf.ValueTypeNames.Double
    set_default(weak, "/Prim", "weakOnly", double, 1.5)
    set_default(weak, "/Prim", "overridden", double, 2.5)
    set_default(root, "/Prim", "overridden", double, 3.5)
    set_samples(weak, "/Prim", "animated", double, [(0.0, 0.0), (5.0, 10.0)])
    weak.Save()
    root.Save()


# ---------------------------------------------------------------------------
# Oracle values
# ---------------------------------------------------------------------------


def to_json(value):
    if value is None:
        return None
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, str)):
        return value
    if isinstance(value, float):
        return value if math.isfinite(value) else repr(value)
    if isinstance(value, Sdf.TimeCode):
        return float(value)
    if hasattr(value, "__len__") and not isinstance(value, str):
        return [to_json(v) for v in value]
    return str(value)


def crate_version(path):
    with open(path, "rb") as f:
        header = f.read(11)
    return "%d.%d.%d" % (header[8], header[9], header[10])


def attribute_record(attr, times):
    record = {"default": to_json(attr.Get())}
    if times:
        record["times"] = [[t, to_json(attr.Get(t))] for t in times]
    return record


def expected_for(path):
    stage = Usd.Stage.Open(path)
    prims = {}
    for prim in stage.TraverseAll():
        attrs = {}
        for attr in prim.GetAttributes():
            times = None
            if attr.GetNumTimeSamples() > 0:
                times = sorted(set(SAMPLE_TIMES + [
                    t + d for t in attr.GetTimeSamples() for d in (-0.5, 0.0, 0.5)
                ] + [20.0]))
            elif attr.HasSpline():
                times = SPLINE_TIMES
            attrs[attr.GetName()] = attribute_record(attr, times)
        if attrs:
            prims[str(prim.GetPath())] = attrs
    return {"crate_version": crate_version(path), "prims": prims}


VERSION_FIXTURES = {
    "version_0_12": "0.12.0",
    "version_0_13": "0.13.0",
    "version_0_14": "0.14.0",
    "version_0_15": "0.15.0",
    "spline_loop_boundary": "0.8.0",
    "spline_time_valued": "0.8.0",
}


LAYER_FIXTURES = [
    "array_edits_weak",
    "array_edits_strong",
    "sublayers_weak",
    "sublayers_root",
]
SUPPLEMENTAL_FIXTURES = ["gen_splines.usdc"]


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--one":
        write_version_fixture(sys.argv[2])
        return

    if Usd.GetVersion() != PINNED_USD_VERSION:
        sys.exit("expected OpenUSD %s, found %s" % (PINNED_USD_VERSION, Usd.GetVersion()))

    for name in list(VERSION_FIXTURES) + LAYER_FIXTURES:
        path = os.path.join(HERE, name + ".usdc")
        if os.path.exists(path):
            os.remove(path)

    for name, version in VERSION_FIXTURES.items():
        env = dict(os.environ, USD_WRITE_NEW_USDC_FILES_AS_VERSION=version)
        subprocess.run([sys.executable, __file__, "--one", name], env=env, check=True)
    author_array_edits()
    author_sublayers()

    fixtures = {}
    for name in list(VERSION_FIXTURES) + LAYER_FIXTURES:
        fixtures[name + ".usdc"] = expected_for(os.path.join(HERE, name + ".usdc"))

    # Existing files read but not written here: the AOUSD supplemental
    # spec's spline fixture, to check the reader against OpenUSD's reading.
    supplemental = {}
    binary = os.path.join(
        HERE, "..", "..", "..", "core-spec-supplemental-release_dec2025",
        "file_formats", "tests", "assets", "binary",
    )
    for name in SUPPLEMENTAL_FIXTURES:
        supplemental[name] = expected_for(os.path.join(binary, name))

    expected = {
        "generator": {
            "script": "layerstack_conformance/fixtures/usdc_versions/generate.py",
            "openusd": "%d.%d.%d" % Usd.GetVersion(),
            "package": "usd-core==26.8 (PyPI)",
            "python": sys.version.split()[0],
        },
        "fixtures": fixtures,
        "supplemental": supplemental,
    }
    with open(os.path.join(HERE, "expected.json"), "w") as f:
        json.dump(expected, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
