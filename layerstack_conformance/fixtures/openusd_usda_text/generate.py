# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Generates USDA text written by OpenUSD, each with the USDC of the same layer.

Each fixture is authored through `Sdf` and exported twice, as `NAME.usda` and
`NAME.usdc`. The text is what OpenUSD's own writer produces, and the crate
file carries the same layer in a form whose reader is checked independently
(`usdc_versions`, `usdc_binary`), so
`layerstack_conformance/tests/usda_openusd_text.rs` requires the USDA reader
to read each `.usda` as the USDC reader reads its `.usdc`.

- `strings`: string, token and dictionary-key values holding every
  character OpenUSD's writer escapes (`Sdf_FileIOUtility::Quote`): both
  quotes, backslash, control characters written as `\\n`, `\\t`, `\\r` and
  `\\xNN`, DEL, and multi-line text written triple-quoted.
- `array_edits`: sparse array edits (`VtArrayEdit`, USDA 1.2) as defaults
  and time samples, written `edit [op; op]`: every instruction, with
  literal and `[index]` operands, the `fill` forms of `minsize` and
  `resize`, tuple and string literals, and an empty edit.
- `asset_paths`: asset paths holding `@` (which OpenUSD writes between
  `@@@` delimiters), `//`, `#`, spaces and quotes, in attribute values, a
  sublayer and a reference.

A source fixture is the other way round: its `.usda` is text given here,
which OpenUSD reads but would not write in that form, and its `.usdc` is
OpenUSD's reading of it.

- `half_literals`: `half` literals that are not exact halves, rounded to
  nearest even through `float` (`GfHalf`), subnormal, overflowing, signed
  zero, infinite and NaN, in scalars, vectors, a quaternion, an array and
  time samples.
- `numeric_literals`: numbers given for `bool` (true when nonzero), for
  integer types (truncated toward zero) and for floating-point types
  (signed zero kept), in scalars, arrays, a vector and time samples.
- `numeric_rejected.txt`: attribute statements whose value OpenUSD rejects
  for the declared type; the script checks that it does.
- `unresolved_sublayer`: a layer whose first sublayer does not exist and
  whose second, `unresolved_sublayer_weak`, does. The script requires
  OpenUSD to report the first as `PcpErrorInvalidSublayerPath` and to keep
  the second, from the USDA and from the USDC.

Pinned oracle: `usd-core` 26.8 from PyPI (OpenUSD v26.08), as for the
`usdc_versions` fixtures:

    python3 -m venv venv && venv/bin/pip install usd-core==26.8
    venv/bin/python layerstack_conformance/fixtures/openusd_usda_text/generate.py
"""

import os
import sys

from pxr import Gf, Pcp, Sdf, Usd, Vt

PINNED_USD_VERSION = (0, 26, 8)
HERE = os.path.dirname(os.path.abspath(__file__))

# Every ASCII control character but NUL (which OpenUSD's writer truncates
# at), then the characters `Quote` escapes or passes through.
TRICKY = "".join(chr(c) for c in range(1, 32)) + "\x7f say \"hi\" it's \\ é 日"


def attribute(prim, name, type_name, value):
    attr = Sdf.AttributeSpec(prim, name, type_name)
    attr.default = value
    return attr


def author_strings(layer):
    layer.documentation = "Layer doc with 'single' and \"double\" quotes\nand a second line"
    layer.comment = "only 'single' quotes"
    prim = Sdf.PrimSpec(layer, "Strings", Sdf.SpecifierDef)
    prim.documentation = "a\\b\tc"
    attribute(prim, "tricky", Sdf.ValueTypeNames.String, TRICKY)
    attribute(prim, "doubleOnly", Sdf.ValueTypeNames.String, 'say "hi"')
    attribute(prim, "bothQuotes", Sdf.ValueTypeNames.String, "it's \"x\"")
    attribute(prim, "multiline", Sdf.ValueTypeNames.String, "one\ntwo \"\"\" three\n")
    attribute(prim, "token", Sdf.ValueTypeNames.Token, "tab\there")
    attribute(prim, "array", Sdf.ValueTypeNames.StringArray, [TRICKY, "", "plain"])
    prim.SetInfo("customData", {TRICKY: TRICKY, "multi\nline": "v\nw"})


def edit(builder_type, fn):
    builder = builder_type()
    fn(builder)
    return builder.FinalizeAndReset()


def author_array_edits(layer):
    prim = Sdf.PrimSpec(layer, "Edits", Sdf.SpecifierDef)
    every_op = edit(
        Vt.IntArrayEditBuilder,
        lambda b: b.Write(9, 0)
        .WriteRef(-1, 1)
        .Insert(5, 2)
        .InsertRef(0, -2)
        .Prepend(1)
        .PrependRef(-1)
        .Append(4)
        .AppendRef(2)
        .EraseRef(-3)
        .MinSize(2)
        .MinSize(6, 7)
        .MaxSize(20)
        .SetSize(8)
        .SetSize(10, -1),
    )
    attribute(prim, "ints", Sdf.ValueTypeNames.IntArray, every_op)
    attribute(
        prim,
        "points",
        Sdf.ValueTypeNames.Point3fArray,
        edit(
            Vt.Vec3fArrayEditBuilder,
            lambda b: b.Append(Gf.Vec3f(1, 2, 3)).SetSize(4, Gf.Vec3f(0.5, 0, -1)),
        ),
    )
    attribute(
        prim,
        "strings",
        Sdf.ValueTypeNames.StringArray,
        edit(Vt.StringArrayEditBuilder, lambda b: b.Append('say "hi"').MinSize(3, "x\ny")),
    )
    attribute(prim, "empty", Sdf.ValueTypeNames.FloatArray, edit(Vt.FloatArrayEditBuilder, lambda b: None))
    sampled = Sdf.AttributeSpec(prim, "sampled", Sdf.ValueTypeNames.DoubleArray)
    layer.SetTimeSample(sampled.path, 1.0, edit(Vt.DoubleArrayEditBuilder, lambda b: b.SetSize(2, 0.25)))
    layer.SetTimeSample(sampled.path, 2.0, Vt.DoubleArray([1.5, 2.5]))
    layer.SetTimeSample(sampled.path, 3.0, edit(Vt.DoubleArrayEditBuilder, lambda b: b.Write(3.5, -1)))


def author_asset_paths(layer):
    layer.subLayerPaths.append("./sub@1.usda")
    prim = Sdf.PrimSpec(layer, "Assets", Sdf.SpecifierDef)
    prim.referenceList.Prepend(Sdf.Reference("./ref@2.usda", "/Target"))
    for name, path in [
        ("at", "tex@1001.png"),
        ("trailingAt", "a@"),
        ("url", "http://host/dir/a b.png"),
        ("fragment", "a.usdz[b#c.png]"),
        ("quotes", "it's \"q\".png"),
    ]:
        attribute(prim, name, Sdf.ValueTypeNames.Asset, Sdf.AssetPath(path))
    attribute(
        prim,
        "array",
        Sdf.ValueTypeNames.AssetArray,
        [Sdf.AssetPath("x@y.png"), Sdf.AssetPath("plain.png")],
    )


FIXTURES = {
    "strings": author_strings,
    "array_edits": author_array_edits,
    "asset_paths": author_asset_paths,
}

# Text that OpenUSD reads but would not write in this form: the `.usda` is
# this source, and the `.usdc` is OpenUSD's reading of it.
SOURCES = {
    "half_literals": """#usda 1.0

def "Halves"
{
    half nearest = 0.1
    half third = 0.333
    half tieToEven = 1.00048828125
    half aboveTie = 1.0004883
    half tieToOdd = 1.00146484375
    half subnormal = 5.960464477539063e-8
    half subnormalRounded = 1e-7
    half aboveHalfSmallest = 3e-8
    half halfSmallest = 2.9802322387695312e-8
    half subnormalToNormal = 6.1033e-5
    half largest = 65519
    half overflow = 65520
    half negativeOverflow = -1e9
    half negativeZero = -0
    half infinity = inf
    half negativeInfinity = -inf
    half notANumber = nan
    half2 pair = (0.1, -0.333)
    half3 triple = (1e-7, 65519, 0.2)
    half4 quad = (0.3, 0.7, -0.9, 1.1)
    quath rotation = (0.1, 0.2, 0.3, 0.9)
    half[] array = [0.1, 1e-7, 70000]
    half sampled.timeSamples = {
        0: 0.1,
        1: 1e-7,
    }
}
""",
    "numeric_literals": """#usda 1.0

def "Numbers"
{
    bool negativeZero = -0
    bool zero = 0
    bool two = 2
    bool fraction = 1.5
    bool negativeInfinity = -inf
    bool notANumber = nan
    bool word = true
    bool[] array = [-0, 2, 0.0, -1, false]
    bool sampled.timeSamples = {
        1: -0,
        2: 1.5,
        3: 0,
    }
    int truncated = 1.5
    int negativeTruncated = -1.5
    uint unsignedTruncated = 1.5
    int64 wide = 4294967296
    uint64 exponent = 1e3
    float floatNegativeZero = -0
    double doubleFromInt = 2
    timecode timecodeNegativeZero = -0
    int[] ints = [1, 2.9, -2.9]
    float[] floats = [1, -0, 1.5]
    int3 vector = (1.5, -2, 3)
    int intSampled.timeSamples = {
        1: 2.5,
    }
}
""",
    "unresolved_sublayer": """#usda 1.0
(
    subLayers = [
        @./missing.usda@ (offset = 5),
        @./unresolved_sublayer_weak.usda@
    ]
)

def "Root"
{
}
""",
    "unresolved_sublayer_weak": """#usda 1.0

def "Weak"
{
}
""",
}

# Values OpenUSD refuses for their declared type: each makes the whole layer
# fail to read. `numeric_rejected.txt` lists those checked here, one
# attribute statement per line.
REJECTED = [
    "uint a = -1",
    "int a = inf",
    "int a = nan",
    "int a = 4294967296",
    "int64 a = 1e30",
    "uint64 a = -1",
    "float a = true",
    'int a = "3"',
    "int[] a = [1, 1.5e10]",
    "float3 a = (1, true, 2)",
    "int a.timeSamples = {\\n        1: 1e20,\\n    }",
]


def main():
    if Usd.GetVersion() != PINNED_USD_VERSION:
        sys.exit("expected OpenUSD %s, found %s" % (PINNED_USD_VERSION, Usd.GetVersion()))
    for name, author in FIXTURES.items():
        layer = Sdf.Layer.CreateAnonymous(".usda")
        author(layer)
        for ext in ("usda", "usdc"):
            path = os.path.join(HERE, "%s.%s" % (name, ext))
            if not layer.Export(path):
                sys.exit("failed to write " + path)
    for name, text in SOURCES.items():
        source = os.path.join(HERE, name + ".usda")
        with open(source, "w") as f:
            f.write(text)
        if name.endswith("_weak"):
            # Read only as a sublayer, from its USDA.
            continue
        layer = Sdf.Layer.FindOrOpen(source)
        if not layer or not layer.Export(os.path.join(HERE, name + ".usdc")):
            sys.exit("failed to convert " + source)
    for statement in REJECTED:
        text = '#usda 1.0\ndef "P"\n{\n    %s\n}\n' % statement.replace("\\n", "\n")
        try:
            accepted = Sdf.Layer.CreateAnonymous(".usda").ImportFromString(text)
        except Exception:
            accepted = False
        if accepted:
            sys.exit("OpenUSD accepts " + statement)
    with open(os.path.join(HERE, "numeric_rejected.txt"), "w") as f:
        f.write("".join(statement + "\n" for statement in REJECTED))
    check_unresolved_sublayer()


def check_unresolved_sublayer():
    """Requires OpenUSD to compose `unresolved_sublayer` in both formats as
    the test expects: the missing sublayer reported as
    `PcpErrorInvalidSublayerPath`, the rest of the layer stack kept."""
    for ext in ("usda", "usdc"):
        stage = Usd.Stage.Open(os.path.join(HERE, "unresolved_sublayer." + ext))
        prims = [str(p.GetPath()) for p in stage.Traverse()]
        errors = stage.GetCompositionErrors()
        if prims != ["/Weak", "/Root"] or [e.errorType for e in errors] != [
            Pcp.ErrorType_InvalidSublayerPath
        ] or "missing.usda" not in str(errors[0]):
            sys.exit("unexpected composition of unresolved_sublayer.%s: %s %s"
                     % (ext, prims, [str(e) for e in errors]))


if __name__ == "__main__":
    main()
