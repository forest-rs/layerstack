# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD resolves attribute values across every value source.

Usage: value_matrix_oracle.py [--check] [OUT_DIR]

Needs OpenUSD 26.08 through Python `pxr`, for example
`pip install usd-core==26.8`. Writes, under
`layerstack_conformance/fixtures/value_matrix` by default, one directory of
USDA layers per composition arc (`root.usda` is each stage's root) and
`oracle.json`, which `tests/value_matrix.rs` replays against layerstack.
`--check` writes everything into a temporary directory and fails unless it
matches the checked-in files.

Each case places the same subjects behind one composition arc: sublayers,
references, payloads, inherits, specializes, variants, relocates or an
instance (whose descendants are instance proxies). A subject is a prim with a
strong site (local opinions; inside the instanced asset for instance proxies)
and a weak site the arc brings in, in `lib/weak.usda`, in `lib/weak_off.usda`
(reached with offset 10 and scale 2: through the sublayer for the layer stack
arcs, through the arc itself for references, payloads, relocates and
instances) or in `lib/weak_tcps.usda` (48 time codes per second under the
root's 24). The subjects are:

- `M`, the value matrix: one attribute per value form and kind, `<form>_<kind>`.
  Kinds are `double`, `int`, `timecode`, `float[]`, `token`, `string`,
  `asset` (anchored at the authoring layer), `asset[]` and `pathExpression`
  (whose relative paths anchor at the authoring prim, and whose `%_` splices
  in the next weaker opinion; a strong sample without `%_` precedes weaker
  samples, which OpenUSD lists since the type composes). Forms combine defaults, time samples, value
  blocks (`None`, default or sampled), animation blocks, splines (`double`)
  and sparse array edits (`float[]`) between the two sites. Attributes with
  `customData` compose dictionaries across the sites.
- `O` and `R`, the matrix's animated forms behind the weak layer's offset
  and scale (`O`) and its time code rate (`R`).
- `CE`, `CN`, `CT`, `CI` and `CO`, value clips anchored at the weak site:
  explicit `active` and `times` (with a discontinuity) with a manifest, the
  same without one, template clips, the same as `CE` with
  `interpolateMissingClipValues`, and `CE`'s clips behind the weak layer's
  offset. `CE` is a `Sphere`, so `radius` has a schema fallback under clips.
- `FS_*`, `FA_*` and `FV_*`, schema fallbacks of `Sphere`, `SpatialAudio`
  and `RenderVar` attributes, with nothing authored, blocked, block-sampled,
  and with stronger samples and sparse edits over a weaker block.

The full product of kinds and forms runs under sublayers; the other arcs
keep every form for `double` and `float[]` and the composing forms for every
kind, so every pair of value kind, value form or source, and composition arc
appears in some case.

For every attribute and every query time (the default time, and per subject
one unit before and after its composed samples, each sample, the midpoints
between them and the clip boundaries) the vectors record:

- `UsdAttribute::Get` under held and under linear interpolation
  (`UsdStage::SetInterpolationType`); asset paths with the path they resolve
  to, relative to the case directory, or `""` where nothing resolves;
- `UsdAttribute::GetResolveInfo`: its source, node (arc type, site path and
  layer stack root) and the chain of `GetNextWeakerInfo`;
- `UsdAttribute::GetBracketingTimeSamples`;
- per attribute, `GetTimeSamples`, `GetTimeSamplesInInterval` over the full,
  a closed and an open interval, `ValueMightBeTimeVarying`, whether the
  attribute is defined, and its composed `customData`.

`UsdAttributeQuery` must report the same time-variance. Where its values,
samples or brackets differ from `UsdAttribute`'s, the record keeps them too
(`query_held`, `query_linear`, `query_samples`, `query_brackets`): at the
default time a query composes over blocks as numeric times do, and it lists
the samples an animation block hides. Layerstack's `AttributeQuery` reads
as `Attribute::get` does.

Where Layerstack deliberately resolves differently, the record pins
Layerstack's `expected` result and names one of the `DIVERGENCES`, each
computed from OpenUSD itself: its flattened stage, its attribute query, its
schema fallback, its prototype, its sample grid, or the same layers with
their sampled path expressions anchored as its parser anchors defaults. The
script refuses to write vectors when a divergence was confirmed with
another OpenUSD release, or when no record shows it any more.
"""
import argparse
import filecmp
import json
import math
import os
import shutil
import tempfile

from pxr import Gf, Pcp, Sdf, Usd

import temporal_sparse_oracle

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(os.path.join(HERE, "..", "fixtures", "value_matrix"))
OPENUSD = "0.26.8"

# The named divergences the matrix shows: results Layerstack resolves
# differently from OpenUSD on purpose, recorded as in
# `temporal_sparse_oracle.py`, whose three it shares.
# `docs/generic-sparse-composition.md` ("Divergences From OpenUSD") and
# `tests/value_matrix.rs` list the same names and versions.
DIVERGENCES = {
    name: temporal_sparse_oracle.DIVERGENCES[name]
    for name in ("nan-before-first-sample", "sampled-block-drops-fallback",
                 "default-time-block-hides-fallback")
}
DIVERGENCES.update({
    "default-time-edits-over-block": {
        "openusd": OPENUSD,
        "openusd_behavior": (
            "At the default time, sparse array edits stronger than a default "
            "block resolve to no value, although numeric times and "
            "`UsdAttributeQuery::Get` at the default time compose them over "
            "the empty array."
        ),
        "openusd_source": (
            "pxr/usd/usd/stage.cpp:7261 and :7272 "
            "(`Usd_AttrGetValueHelper::GetValue` composes the `default` field "
            "as metadata and clears the block)"
        ),
        "layerstack": (
            "Composes the edits over the empty array, as at numeric times."
        ),
        "authority": (
            "AOUSD Core 12.3.6 (a block discards weaker opinions only) and "
            "the sparse-array-edits proposal (edits compose over an empty "
            "array when nothing weaker remains)"
        ),
    },
    "sampled-path-expression-anchoring": {
        "openusd": OPENUSD,
        "openusd_behavior": (
            "The text parser anchors a default path expression's relative "
            "patterns at the prim that authors it, but not a time sample's. "
            "Composition then keeps a sample's relative patterns as authored "
            "under an identity mapping, and drops them across a reference, "
            "payload or relocation."
        ),
        "openusd_source": (
            "pxr/usd/sdf/textParserHelpers.cpp:114 (only default values are "
            "made absolute); pxr/usd/usd/stage.cpp:272 "
            "(`_MapPathExpressionToPrim` maps patterns as they are)"
        ),
        "layerstack": (
            "Anchors every opinion's relative patterns at the prim that "
            "authors it, samples included, and maps them into the stage "
            "namespace (`Value::PathExpression`)."
        ),
        "authority": (
            "OpenUSD's own anchoring of defaults, applied to every opinion; "
            "the type is an OpenUSD extension that AOUSD Core does not define"
        ),
    },
    "instance-proxy-clips-not-varying": {
        "openusd": OPENUSD,
        "openusd_behavior": (
            "`ValueMightBeTimeVarying` is false for an attribute of an "
            "instance proxy whose values come from value clips, although its "
            "prototype's attribute is time-varying and the proxy's values "
            "vary."
        ),
        "openusd_source": (
            "pxr/usd/usd/stage.cpp:9769 "
            "(`_ValueMightBeTimeVaryingFromResolveInfo` looks clips up by the "
            "proxy's path, which the clip cache does not hold)"
        ),
        "layerstack": "Reports the prototype's time-variance.",
        "authority": (
            "AOUSD Core 12.3.4 (value clips supply time samples) and 11 "
            "(an instance proxy reads its prototype's values)"
        ),
    },
    "bracketing-composability-in-stage-time": {
        "openusd": OPENUSD,
        "openusd_behavior": (
            "`GetBracketingTimeSamples` reads whether a series' lower sample "
            "composes at that sample's stage time instead of its layer time, "
            "so behind a layer offset it can stop at the stronger series, "
            "bracketing with its last sample although `GetTimeSamples` lists "
            "the weaker samples sparse edits compose over and the value "
            "changes there."
        ),
        "openusd_source": (
            "pxr/usd/usd/stage.cpp:8362 "
            "(`_BracketingSamplesResolver::ProcessLayerAtTime` passes the "
            "stage-time `lower` to `_GetSampleComposability`)"
        ),
        "layerstack": "Brackets with the composed sample grid.",
        "authority": (
            "AOUSD Core 12.3.2.1 (layer offsets map layer times to stage "
            "times) and 12.3.2 (time samples)"
        ),
    },
    "missing-clip-values-across-jump": {
        "openusd": OPENUSD,
        "openusd_behavior": (
            "With `interpolateMissingClipValues`, where the active clip lacks "
            "the attribute and no later clip has it, the bracket search takes "
            "the earlier clip's samples at the query time through the time "
            "mapping after a jump discontinuity, holding that clip's first "
            "sample, although `GetTimeSamples` ends with its last sample "
            "before the jump and the value there differs."
        ),
        "openusd_source": (
            "pxr/usd/usd/clipSet.cpp:681 "
            "(`GetBracketingTimeSamplesForPath` uses the earlier clip's "
            "bracket at the query time)"
        ),
        "layerstack": (
            "Holds the value of the last composed sample, the earlier clip's "
            "last sample before the jump."
        ),
        "authority": (
            "AOUSD Core 12.3.4 (clip samples on the stage timeline) and "
            "12.5 (a time after the last sample takes its value)"
        ),
    },
})

# -- Value kinds ---------------------------------------------------------------
#
# Each kind spells a weak and a strong default, weak samples at 1 and 3 and
# strong samples at 2 and 4.


class Kind:
    def __init__(self, type_name, wd, sd, ws, ss):
        self.type_name = type_name
        self.wd = wd
        self.sd = sd
        self.ws = ws
        self.ss = ss


KINDS = {
    "dbl": Kind("double", "1.5", "2.5", {1: "10", 3: "30"}, {2: "200", 4: "400"}),
    "int": Kind("int", "1", "2", {1: "10", 3: "30"}, {2: "200", 4: "400"}),
    "tc": Kind("timecode", "1.5", "2.5", {1: "10", 3: "30"}, {2: "200", 4: "400"}),
    "arr": Kind("float[]", "[1, 2]", "[3, 4]",
                {1: "[10, 20]", 3: "[30, 40]"}, {2: "[200, 400]", 4: "[400, 800]"}),
    "tok": Kind("token", '"wd"', '"sd"', {1: '"w1"', 3: '"w3"'}, {2: '"s2"', 4: '"s4"'}),
    "str": Kind("string", '"wd"', '"sd"', {1: '"w1"', 3: '"w3"'}, {2: '"s2"', 4: '"s4"'}),
    # Weak opinions are authored in `lib/`, strong ones next to `root.usda`
    # (inside `lib/asset.usda` for instance proxies, where they resolve to
    # nothing).
    "asset": Kind("asset", "@./weak.usda@", "@./lib/weak_tcps.usda@",
                  {1: "@./weak.usda@", 3: "@./clips/clip_a.usda@"},
                  {2: "@./lib/weak.usda@", 4: "@./root.usda@"}),
    "assets": Kind("asset[]", "[@./weak.usda@, @./clips/clip_b.usda@]", "[@./root.usda@]",
                   {1: "[@./weak.usda@]", 3: "[@./clips/clip_a.usda@]"},
                   {2: "[@./lib/weak.usda@]", 4: "[@./root.usda@, @./missing.usda@]"}),
    "expr": Kind("pathExpression", '"Leaf ../Sib /Elsewhere"', '"%_ Other"',
                 {1: '"Leaf"', 3: '"/Elsewhere"'}, {2: '"Other"', 4: '"%_ Other"'}),
}

# Kinds whose values interpolate under linear interpolation.
INTERPOLATING = {"double", "float", "timecode", "float[]", "float3[]"}


def samples(values, block=None):
    """A time-sample map's text; `block` adds a value block at that time."""
    items = dict(values)
    if block is not None:
        items[block] = "None"
    return "{ " + ", ".join(f"{t}: {items[t]}" for t in sorted(items)) + " }"


WEAK_SPLINE = "{ 1: 10; post linear, 3: 30; post linear }"
STRONG_SPLINE = "{ 2: 200; post linear, 4: 400; post held }"
EDIT = "edit [append 9]"
EDIT_SAMPLES = {2: "edit [write 7 to [0]]", 4: "edit [append 5]"}


def form_lines(form, kind, name):
    """The (strong, weak) attribute lines of one form of one kind."""
    k = KINDS[kind]
    t = k.type_name
    d = lambda v: f"{t} {name} = {v}"
    s = lambda m: f"{t} {name}.timeSamples = {m}"
    forms = {
        "def": ([], [d(k.wd)]),
        "dd": ([d(k.sd)], [d(k.wd)]),
        "s": ([], [s(samples(k.ws))]),
        "both": ([], [d(k.wd), s(samples(k.ws))]),
        "sd": ([s(samples(k.ss))], [d(k.wd)]),
        "ds": ([d(k.sd)], [s(samples(k.ws))]),
        "ss": ([s(samples(k.ss))], [s(samples(k.ws))]),
        "blk": ([d("None")], [d(k.wd), s(samples(k.ws))]),
        "sblk": ([], [s(samples(k.ws, block=2))]),
        "ssblk": ([s(samples({4: k.ss[4]}, block=2))], [s(samples(k.ws))]),
        "ablk": ([d("AnimationBlock")], [d(k.wd), s(samples(k.ws))]),
        # Splines (`double`).
        "sp": ([], [f"{t} {name}.spline = {WEAK_SPLINE}"]),
        "spd": ([d(k.sd)], [f"{t} {name}.spline = {WEAK_SPLINE}"]),
        "ssp": ([f"{t} {name}.spline = {STRONG_SPLINE}"], [s(samples(k.ws))]),
        "absp": ([d("AnimationBlock")], [d(k.wd), f"{t} {name}.spline = {WEAK_SPLINE}"]),
        # Sparse array edits (`float[]`).
        "ed": ([d(EDIT)], [d(k.wd)]),
        "es": ([s(samples(EDIT_SAMPLES))], [s(samples(k.ws))]),
        "esd": ([s(samples(EDIT_SAMPLES))], [d(k.wd)]),
        "edb": ([d(EDIT)], [d("None")]),
        # Composed `customData` dictionaries.
        "cdw": ([], [d(k.wd) + " (\n" + CUSTOM_WEAK + "\n)"]),
        "cdd": ([d(k.sd) + " (\n" + CUSTOM_STRONG + "\n)"],
                [d(k.wd) + " (\n" + CUSTOM_WEAK + "\n)"]),
    }
    return forms[form]


CUSTOM_WEAK = """    customData = {
        string a = "weak"
        int w = 1
        timecode t = 3
        asset p = @./weak.usda@
        dictionary n = {
            int x = 1
            string y = "weak"
        }
    }"""

CUSTOM_STRONG = """    customData = {
        string a = "strong"
        dictionary n = {
            string y = "strong"
            int z = 3
        }
    }"""

ALL_FORMS = ["def", "dd", "s", "both", "sd", "ds", "ss", "blk", "sblk", "ssblk", "ablk"]
DOUBLE_FORMS = ["sp", "spd", "ssp", "absp", "cdw", "cdd"]
ARRAY_FORMS = ["ed", "es", "esd", "edb"]
# The forms every kind keeps under arcs other than sublayers.
COMPOSING_FORMS = ["dd", "sd", "ss", "both"]


class Subject:
    """A prim with opinions at a strong and a weak site."""

    def __init__(self, name, layer="weak", type_name=None):
        self.name = name
        self.layer = layer
        self.type_name = type_name
        self.strong = []
        self.weak = []
        self.weak_meta = []
        self.attrs = []

    def add(self, name, strong, weak):
        self.strong += strong
        self.weak += weak
        if name not in self.attrs:
            self.attrs.append(name)


def matrix_subject(arc):
    subject = Subject("M")
    for kind in KINDS:
        forms = ALL_FORMS if arc == "sublayers" or kind in ("dbl", "arr") else COMPOSING_FORMS
        if kind == "dbl":
            forms = forms + DOUBLE_FORMS
        if kind == "arr":
            forms = forms + ARRAY_FORMS
        for form in forms:
            name = f"{form}_{kind}"
            subject.add(name, *form_lines(form, kind, name))
    return subject


def offset_subject(name, layer):
    subject = Subject(name, layer)
    for kind in ("dbl", "tc", "arr", "tok"):
        for form in ("s", "sd", "ss", "sblk"):
            attr = f"{form}_{kind}"
            subject.add(attr, *form_lines(form, kind, attr))
    for kind in ("int", "str", "asset", "expr"):
        subject.add(f"ss_{kind}", *form_lines("ss", kind, f"ss_{kind}"))
    for form, kind in (("sp", "dbl"), ("ssp", "dbl"), ("es", "arr"), ("esd", "arr"),
                       ("cdw", "dbl"), ("both", "tc")):
        attr = f"{form}_{kind}"
        subject.add(attr, *form_lines(form, kind, attr))
    return subject


# -- Value clips -------------------------------------------------------------

CLIP_ATTRS = ["double dbl", "float[] arr", "token tok", "asset asset", "timecode tc",
              "double gap", "double over_dbl"]

CLIP_FILES = {
    "lib/clips/clip_a.usda": """#usda 1.0

def "Clip"
{
    double dbl.timeSamples = { 0: 0, 1: 10, 2: 20 }
    float[] arr.timeSamples = { 0: [0, 0], 2: [20, 20] }
    token tok.timeSamples = { 0: "a0", 1: "a1" }
    asset asset.timeSamples = { 0: @./clip_a.usda@ }
    timecode tc.timeSamples = { 0: 0, 2: 2 }
    double gap.timeSamples = { 0: 0, 2: 20 }
    double radius.timeSamples = { 0: 3, 2: 5 }
    double anchor_dbl.timeSamples = { 0: 100, 2: 120 }
    double over_dbl.timeSamples = { 0: 1 }
}
""",
    "lib/clips/clip_b.usda": """#usda 1.0

def "Clip"
{
    double dbl.timeSamples = { 0: 50, 2: 70 }
    float[] arr.timeSamples = { 0: [50, 50], 2: [70, 70] }
    token tok.timeSamples = { 0: "b0" }
    asset asset.timeSamples = { 0: @./clip_b.usda@ }
    timecode tc.timeSamples = { 0: 5, 2: 7 }
    double anchor_dbl.timeSamples = { 0: 150 }
    double over_dbl.timeSamples = { 0: 2 }
}
""",
    "lib/clips/manifest.usda": """#usda 1.0

over "Clip"
{
    double dbl
    float[] arr
    token tok
    asset asset
    timecode tc
    double gap
    double radius
    double anchor_dbl
    double over_dbl
}
""",
    "lib/clips/tmpl.0.usda": """#usda 1.0

def "Clip"
{
    double dbl.timeSamples = { 0: 0, 2: 2 }
    token tok.timeSamples = { 0: "t0" }
    double gap.timeSamples = { 0: 0 }
}
""",
    "lib/clips/tmpl.2.usda": """#usda 1.0

def "Clip"
{
    double dbl.timeSamples = { 2: 20, 3: 30 }
    token tok.timeSamples = { 2: "t2" }
}
""",
    "lib/clips/tmpl.4.usda": """#usda 1.0

def "Clip"
{
    double dbl.timeSamples = { 4: 40 }
    token tok.timeSamples = { 4: "t4" }
    double gap.timeSamples = { 4: 40 }
}
""",
}

EXPLICIT_CLIPS = """        asset[] assetPaths = [@./clips/clip_a.usda@, @./clips/clip_b.usda@]
        string primPath = "/Clip"
        double2[] active = [(0, 0), (2, 1)]
        double2[] times = [(0, 0), (2, 2), (2, 0), (4, 2)]"""

MANIFEST = "        asset manifestAssetPath = @./clips/manifest.usda@"

TEMPLATE_CLIPS = """        string templateAssetPath = "./clips/tmpl.#.usda"
        double templateStartTime = 0
        double templateEndTime = 4
        double templateStride = 2
        string primPath = "/Clip\""""


def clips_meta(*entries):
    return ["clips = {\n    dictionary default = {\n" + "\n".join(entries) + "\n    }\n}"]


def clip_subject(name, entries, layer="weak", type_name=None, radius=False):
    subject = Subject(name, layer, type_name)
    subject.weak_meta = clips_meta(*entries)
    for decl in CLIP_ATTRS:
        attr = decl.split()[1]
        if attr == "over_dbl":
            subject.add(attr, ["double over_dbl = 7"], [decl])
        else:
            subject.add(attr, [], [decl])
    # A default at the anchor site, under the clips.
    subject.add("anchor_dbl", [], ["double anchor_dbl = 5"])
    if radius:
        subject.attrs.append("radius")
    return subject


def clip_subjects():
    return [
        clip_subject("CE", [EXPLICIT_CLIPS, MANIFEST], type_name="Sphere", radius=True),
        clip_subject("CN", [EXPLICIT_CLIPS]),
        clip_subject("CT", [TEMPLATE_CLIPS, MANIFEST]),
        clip_subject("CI", [EXPLICIT_CLIPS, MANIFEST,
                            "        bool interpolateMissingClipValues = true"]),
        clip_subject("CO", [EXPLICIT_CLIPS, MANIFEST], layer="weak_off"),
    ]


# -- Schema fallbacks --------------------------------------------------------

FALLBACK_TYPES = {
    "FS": ("Sphere", [("double", "radius", "5", "7"), ("float3[]", "extent",
                                                       "[(0, 0, 0), (2, 2, 2)]",
                                                       "[(1, 1, 1), (3, 3, 3)]"),
                      ("token", "visibility", '"invisible"', '"inherited"')]),
    "FA": ("SpatialAudio", [("asset", "filePath", "@./weak.usda@", None),
                            ("timecode", "startTime", "4", None),
                            ("double", "gain", "0.5", "0.25")]),
    "FV": ("RenderVar", [("string", "sourceName", '"weak"', None),
                         ("token", "dataType", '"float"', None)]),
}


def fallback_subjects(arc):
    subjects = []
    for prefix, (type_name, attrs) in FALLBACK_TYPES.items():
        forms = ["none", "blk"]
        if prefix == "FS":
            forms += ["sblk", "over"]
        for form in forms:
            subject = Subject(f"{prefix}_{form}", type_name=type_name)
            for t, name, a, b in attrs:
                if form == "none":
                    subject.add(name, [], [])
                elif form == "blk":
                    subject.add(name, [], [f"{t} {name} = None"])
                elif form == "sblk":
                    subject.add(name, [], [f"{t} {name}.timeSamples = {{ 1: {a}, 2: None, 3: {b} }}"])
                elif name == "extent":
                    subject.add(name, [f"{t} {name} = edit [write (9, 9, 9) to [1]]"],
                                [f"{t} {name} = None"])
                else:
                    subject.add(name, [f"{t} {name}.timeSamples = {{ 1: {a}, 3: {b} }}"],
                                [f"{t} {name} = None"])
            subjects.append(subject)
    return subjects


def subjects(arc):
    return ([matrix_subject(arc), offset_subject("O", "weak_off"),
             offset_subject("R", "weak_tcps")] + clip_subjects() + fallback_subjects(arc))


# -- Layer text ----------------------------------------------------------------


def indent(text, by):
    pad = " " * by
    return "\n".join(pad + line if line else line for line in text.split("\n"))


def prim_spec(specifier, type_name, name, meta, body, children=()):
    head = f'{specifier} {type_name + " " if type_name else ""}"{name}"'
    if meta:
        head += " (\n" + indent("\n".join(meta), 4) + "\n)"
    lines = [indent(line, 4) for line in body]
    lines += [indent(child, 4) for child in children]
    return head + "\n{\n" + "\n".join(lines) + ("\n" if lines else "") + "}\n"


def layer_text(prims, meta=()):
    text = "#usda 1.0\n"
    if meta:
        text += "(\n" + indent("\n".join(meta), 4) + "\n)\n"
    return text + "\n" + "\n".join(prims)


OFFSET = "(offset = 10; scale = 2)"
WEAK_LAYERS = ("weak", "weak_off", "weak_tcps")


def stack_sublayers(prefix):
    return ["subLayers = [\n"
            f"    @{prefix}weak.usda@,\n"
            f"    @{prefix}weak_off.usda@ {OFFSET},\n"
            f"    @{prefix}weak_tcps.usda@\n"
            "]"]


def weak_layer_meta(layer):
    return ["timeCodesPerSecond = 48"] if layer == "weak_tcps" else []


def arc_suffix(layer):
    return f" {OFFSET}" if layer == "weak_off" else ""


ARCS = ["sublayers", "references", "payloads", "inherits", "specializes", "variants",
        "relocates", "instance_proxies"]


def build(arc, subs):
    """The case's layers, and each subject's composed prim path."""
    weak = {layer: [] for layer in WEAK_LAYERS}
    root, root_meta, strong, asset = [], [], [], []
    paths = {}
    relocates = []
    for sub in subs:
        n = sub.name
        if arc == "sublayers":
            strong.append(prim_spec("over", sub.type_name, n, [], sub.strong))
            weak[sub.layer].append(prim_spec("def", None, n, sub.weak_meta, sub.weak))
            paths[n] = f"/{n}"
        elif arc in ("references", "payloads"):
            key = "references" if arc == "references" else "payload"
            meta = [f"prepend {key} = @./lib/{sub.layer}.usda@</{n}_W>{arc_suffix(sub.layer)}"]
            root.append(prim_spec("def", sub.type_name, n, meta, sub.strong))
            weak[sub.layer].append(prim_spec("def", None, f"{n}_W", sub.weak_meta, sub.weak))
            paths[n] = f"/{n}"
        elif arc in ("inherits", "specializes"):
            root.append(prim_spec("def", sub.type_name, n, [f"{arc} = </_{n}>"], sub.strong))
            weak[sub.layer].append(prim_spec("class", None, f"_{n}", sub.weak_meta, sub.weak))
            paths[n] = f"/{n}"
        elif arc == "variants":
            meta = ['variants = {\n    string v = "a"\n}', 'prepend variantSets = "v"']
            root.append(prim_spec("def", sub.type_name, n, meta, sub.strong))
            variant = prim_spec("", None, "a", sub.weak_meta, sub.weak).lstrip()
            body = 'variantSet "v" = {\n' + indent(variant, 4) + "}"
            weak[sub.layer].append(prim_spec("over", None, n, [], [body]))
            paths[n] = f"/{n}"
        elif arc == "relocates":
            meta = [f"prepend references = @./lib/{sub.layer}.usda@</{n}_Rig>{arc_suffix(sub.layer)}"]
            new = prim_spec("over", sub.type_name, "New", [], sub.strong)
            root.append(prim_spec("def", None, n, meta, [], [new]))
            old = prim_spec("def", None, "Old", sub.weak_meta, sub.weak)
            weak[sub.layer].append(prim_spec("def", None, f"{n}_Rig", [], [], [old]))
            relocates.append(f"</{n}/Old>: </{n}/New>")
            paths[n] = f"/{n}/New"
        elif arc == "instance_proxies":
            ref = f"@./lib/asset.usda@</{n}_A>{arc_suffix(sub.layer)}"
            meta = ["instanceable = true", f"prepend references = {ref}"]
            # Local opinions on an instance's descendants are ignored.
            ignored = prim_spec("over", None, "Child", [], ["double dd_dbl = 999"])
            root.append(prim_spec("def", None, n, meta, [], [ignored]))
            child = prim_spec("def", sub.type_name, "Child", [], sub.strong)
            asset.append(prim_spec("def", None, f"{n}_A", [], [], [child]))
            weak_child = prim_spec("over", None, "Child", sub.weak_meta, sub.weak)
            weak[sub.layer].append(prim_spec("over", None, f"{n}_A", [], [], [weak_child]))
            paths[n] = f"/{n}/Child"
        else:
            raise ValueError(arc)
    files = {}
    if arc == "sublayers":
        root_meta = ["subLayers = [\n"
                     "    @./strong.usda@,\n"
                     "    @./lib/weak.usda@,\n"
                     f"    @./lib/weak_off.usda@ {OFFSET},\n"
                     "    @./lib/weak_tcps.usda@\n"
                     "]"]
        files["strong.usda"] = layer_text(strong)
    elif arc in ("inherits", "specializes", "variants"):
        root_meta = stack_sublayers("./lib/")
    elif arc == "relocates":
        root_meta = ["relocates = {\n" + ",\n".join(f"    {r}" for r in relocates) + "\n}"]
    elif arc == "instance_proxies":
        files["lib/asset.usda"] = layer_text(asset, stack_sublayers("./"))
    files["root.usda"] = layer_text(root, root_meta)
    for layer in WEAK_LAYERS:
        files[f"lib/{layer}.usda"] = layer_text(weak[layer], weak_layer_meta(layer))
    files.update(CLIP_FILES)
    return files, paths


# -- Recording -----------------------------------------------------------------

ARC_NAMES = {
    Pcp.ArcTypeRoot: "local",
    Pcp.ArcTypeInherit: "inherits",
    Pcp.ArcTypeVariant: "variants",
    Pcp.ArcTypeRelocate: "relocates",
    Pcp.ArcTypeReference: "references",
    Pcp.ArcTypePayload: "payloads",
    Pcp.ArcTypeSpecialize: "specializes",
}

SOURCES = {
    Usd.ResolveInfoSourceNone: "none",
    Usd.ResolveInfoSourceFallback: "fallback",
    Usd.ResolveInfoSourceDefault: "default",
    Usd.ResolveInfoSourceTimeSamples: "samples",
    Usd.ResolveInfoSourceValueClips: "clips",
    Usd.ResolveInfoSourceSpline: "spline",
}


def num(x):
    x = float(x)
    if math.isnan(x):
        return "NaN"
    if x == int(x) and abs(x) < 2 ** 53:
        return float(int(x))
    return x


def relative(case_dir, path):
    if not path:
        return ""
    return os.path.relpath(os.path.realpath(path), os.path.realpath(case_dir)).replace(os.sep, "/")


def encode(value, case_dir):
    """A value as JSON: numbers, strings, lists (arrays and tuples), and
    tagged objects for asset paths, path expressions and dictionaries."""
    if value is None:
        return None
    if isinstance(value, bool):
        return value
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return num(value)
    if isinstance(value, str):
        return value
    if isinstance(value, Sdf.TimeCode):
        return num(value.GetValue())
    if isinstance(value, Sdf.AssetPath):
        return {"asset": value.path, "resolved": relative(case_dir, value.resolvedPath)}
    if isinstance(value, Sdf.PathExpression):
        return {"expr": value.GetText()}
    if isinstance(value, dict):
        return {"dict": {k: encode(v, case_dir) for k, v in sorted(value.items())}}
    if isinstance(value, (Gf.Vec2f, Gf.Vec3f, Gf.Vec4f, Gf.Vec2d, Gf.Vec3d, Gf.Vec4d)):
        return [num(c) for c in value]
    if hasattr(value, "__len__"):
        return [encode(v, case_dir) for v in value]
    raise TypeError(f"cannot encode {value!r}")


def node_text(node, case_dir):
    if not node:
        return ""
    root = node.layerStack.identifier.rootLayer
    return "|".join([ARC_NAMES[node.arcType], str(node.path),
                     relative(case_dir, root.realPath)])


def info_text(info, case_dir):
    """`source|arc|site|layer stack`, then each next weaker info."""
    chain = []
    while True:
        chain.append(SOURCES[info.GetSource()] + "|" + node_text(info.GetNode(), case_dir))
        if not info.HasNextWeakerInfo():
            break
        info = info.GetNextWeakerInfo()
    return " > ".join(chain)


def brackets(result):
    if not result:
        return None
    return [num(result[0]), num(result[1])]


def query_times(attrs, prim):
    """One unit before and after the composed samples, each sample, the
    midpoints between them, and the clip boundaries."""
    times = set()
    for attr in attrs:
        times.update(attr.GetTimeSamples())
    for clip_set in prim.GetMetadata("clips") or {}:
        times.update([0.0, 2.0, 4.0])
    times.update([0.0])
    ordered = sorted(times)
    probe = set(ordered)
    probe.add(ordered[0] - 1)
    probe.add(ordered[-1] + 1)
    for a, b in zip(ordered, ordered[1:]):
        probe.add((a + b) / 2)
    return sorted(probe)


def info_index(infos, text):
    """`text`'s index in the case's table of resolve infos."""
    if text not in infos:
        infos[text] = len(infos)
    return infos[text]


def anchored_stage(case_dir, tmp):
    """The case with every time-sampled path expression anchored at the prim
    that authors it, as OpenUSD's text parser anchors defaults
    (`pxr/usd/sdf/textParserHelpers.cpp:114`), opened from a copy of the
    case under `tmp`."""
    copy = os.path.join(tmp, "anchored")
    shutil.copytree(case_dir, copy)
    for directory, _, names in os.walk(copy):
        for name in names:
            layer = Sdf.Layer.FindOrOpen(os.path.join(directory, name))

            def anchor(path):
                spec = layer.GetAttributeAtPath(path)
                if not spec or spec.typeName != Sdf.ValueTypeNames.PathExpression:
                    return
                prim = path.GetPrimPath().StripAllVariantSelections()
                for t in layer.ListTimeSamplesForPath(path):
                    expr = layer.QueryTimeSample(path, t)
                    if isinstance(expr, Sdf.PathExpression):
                        layer.SetTimeSample(path, t, expr.MakeAbsolute(prim))

            layer.Traverse(Sdf.Path.absoluteRootPath, anchor)
            layer.Save()
    return Usd.Stage.Open(os.path.join(copy, "root.usda"), Usd.Stage.LoadAll)


def grid_brackets(samples, t):
    """The samples bracketing `t` in the composed grid `samples`."""
    if not samples:
        return None
    if t <= samples[0]:
        return [num(samples[0])] * 2
    if t >= samples[-1]:
        return [num(samples[-1])] * 2
    for a, b in zip(samples, samples[1:]):
        if t == a:
            return [num(a)] * 2
        if a < t < b:
            return [num(a), num(b)]
    return [num(samples[-1])] * 2


def interpolates_missing_clip_values(prim):
    return any(clip_set.get("interpolateMissingClipValues")
               for clip_set in (prim.GetMetadata("clips") or {}).values())


def fallback_of(attr):
    definition = attr.GetPrim().GetPrimDefinition()
    return definition.GetAttributeFallbackValue(attr.GetName())


def has_nan(value):
    if value == "NaN":
        return True
    return isinstance(value, list) and any(has_nan(v) for v in value)


class Pins:
    """The values Layerstack resolves on purpose where OpenUSD differs,
    each naming one of the `DIVERGENCES`."""

    def __init__(self):
        self.pins = []

    def pin(self, field, mode, index, openusd, expected, name):
        assert name in DIVERGENCES, name
        if json.dumps(openusd) == json.dumps(expected):
            return
        USED.add(name)
        self.pins.append([field, mode, index, expected, name])


USED = set()


def record_case(arc, case_dir, subs, paths, tmp):
    """The case's resolve infos, and each subject's records."""
    stage = Usd.Stage.Open(os.path.join(case_dir, "root.usda"), Usd.Stage.LoadAll)
    flat = Usd.Stage.Open(stage.Flatten())
    anchored = anchored_stage(case_dir, tmp)
    prims = []
    infos = {}
    for sub in subs:
        prim = stage.GetPrimAtPath(paths[sub.name])
        assert prim, (arc, paths[sub.name])
        attrs = [prim.GetAttribute(name) for name in sub.attrs]
        # Clip metadata reaches the composed prim through any of its nodes.
        times = query_times([a for a in attrs if a], prim)
        lo, hi = times[1], times[-2]
        records = []
        for name, attr in zip(sub.attrs, attrs):
            path = f"{paths[sub.name]}.{name}"
            if not attr.IsDefined():
                records.append({"attr": path, "defined": False})
                continue
            query = Usd.AttributeQuery(attr)
            pins = Pins()
            samples = attr.GetTimeSamples()
            record = {
                "attr": path,
                "type": str(attr.GetTypeName()),
                "defined": True,
                "samples": [num(t) for t in samples],
                "intervals": [
                    [num(t) for t in attr.GetTimeSamplesInInterval(Gf.Interval.GetFullInterval())],
                    [num(t) for t in attr.GetTimeSamplesInInterval(Gf.Interval(lo, hi, True, True))],
                    [num(t) for t in attr.GetTimeSamplesInInterval(Gf.Interval(lo, hi, False, False))],
                ],
                "varying": attr.ValueMightBeTimeVarying(),
            }
            if prim.IsInstanceProxy():
                proto = stage.GetPrimAtPath(prim.GetPrimInPrototype().GetPath())
                pins.pin("varying", "", 0, record["varying"],
                         proto.GetAttribute(name).ValueMightBeTimeVarying(),
                         "instance-proxy-clips-not-varying")
            # `UsdAttributeQuery` reports the samples an animation block
            # hides, where `UsdAttribute` does not; keep both.
            if query.GetTimeSamples() != samples:
                record["query_samples"] = [num(t) for t in query.GetTimeSamples()]
            assert query.ValueMightBeTimeVarying() == attr.ValueMightBeTimeVarying(), path
            if attr.HasAuthoredCustomData():
                record["custom_data"] = encode(attr.GetCustomData(), case_dir)["dict"]
            fallback = fallback_of(attr)
            values = {}
            for mode, interp in (("held", Usd.InterpolationTypeHeld),
                                 ("linear", Usd.InterpolationTypeLinear)):
                for each in (stage, flat, anchored):
                    each.SetInterpolationType(interp)
                row, queried = [], []
                for index, t in enumerate([None] + times):
                    code = Usd.TimeCode.Default() if t is None else Usd.TimeCode(t)
                    value = encode(attr.Get(code), case_dir)
                    row.append(value)
                    queried.append(encode(query.Get(code), case_dir))
                    if has_nan(value):
                        pins.pin("value", mode, index, value,
                                 encode(flat.GetAttributeAtPath(path).Get(code), case_dir),
                                 "nan-before-first-sample")
                    elif t is None and value is None and queried[-1] is not None:
                        pins.pin("value", mode, index, value, queried[-1],
                                 "default-time-block-hides-fallback" if fallback is not None
                                 else "default-time-edits-over-block")
                    elif (t is not None and value is None and fallback is not None
                          and attr.GetResolveInfo(code).GetSource()
                          == Usd.ResolveInfoSourceTimeSamples):
                        pins.pin("value", mode, index, value, encode(fallback, case_dir),
                                 "sampled-block-drops-fallback")
                    elif attr.GetTypeName() == Sdf.ValueTypeNames.PathExpression:
                        pins.pin("value", mode, index, value,
                                 encode(anchored.GetAttributeAtPath(path).Get(code), case_dir),
                                 "sampled-path-expression-anchoring")
                    elif t is not None and interpolates_missing_clip_values(prim):
                        lower, upper = grid_brackets(samples, t) or (None, None)
                        if lower == upper and lower is not None and lower != t:
                            pins.pin("value", mode, index, value,
                                     encode(attr.Get(Usd.TimeCode(lower)), case_dir),
                                     "missing-clip-values-across-jump")
                values[mode] = row
                if queried != row:
                    record[f"query_{mode}"] = queried
            for each in (stage, flat, anchored):
                each.SetInterpolationType(Usd.InterpolationTypeLinear)
            record["held"] = values["held"]
            if values["linear"] != values["held"]:
                record["linear"] = values["linear"]
            record["info"] = []
            for index, t in enumerate([None] + times):
                code = Usd.TimeCode.Default() if t is None else Usd.TimeCode(t)
                text = info_text(attr.GetResolveInfo(code), case_dir)
                record["info"].append(info_index(infos, text))
                # Where a sampled block resolves the fallback, so does its
                # source.
                if any(pin[0] == "value" and pin[2] == index
                       and pin[4] == "sampled-block-drops-fallback" for pin in pins.pins):
                    pins.pin("info", "", index, text, "fallback|",
                             "sampled-block-drops-fallback")
            record["brackets"] = []
            for index, t in enumerate(times):
                b = brackets(attr.GetBracketingTimeSamples(t))
                record["brackets"].append(b)
                pins.pin("brackets", "", index, b, grid_brackets(samples, t),
                         "missing-clip-values-across-jump" if interpolates_missing_clip_values(prim)
                         else "bracketing-composability-in-stage-time")
                q = brackets(query.GetBracketingTimeSamples(t))
                if q != b:
                    record.setdefault("query_brackets", {})[str(num(t))] = q
            if pins.pins:
                record["pins"] = pins.pins
            records.append(record)
        prims.append({"path": paths[sub.name], "times": [num(t) for t in times],
                      "interval": [num(lo), num(hi)], "attrs": records})
    return list(infos), prims


def write(out):
    if os.path.isdir(out):
        shutil.rmtree(out)
    cases = []
    for arc in ARCS:
        subs = subjects(arc)
        files, paths = build(arc, subs)
        case_dir = os.path.join(out, arc)
        for name, text in sorted(files.items()):
            path = os.path.join(case_dir, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as f:
                f.write(text)
        with tempfile.TemporaryDirectory() as tmp:
            infos, prims = record_case(arc, case_dir, subs, paths, tmp)
        cases.append({"name": arc, "infos": infos, "prims": prims})
    _, minor, patch = Usd.GetVersion()
    version = f"0.{minor}.{patch}"
    for name, divergence in sorted(DIVERGENCES.items()):
        if divergence["openusd"] != version:
            raise SystemExit(f"`{name}` was confirmed with OpenUSD {divergence['openusd']}, "
                             f"not {version}: re-check its source lines")
        if name not in USED:
            raise SystemExit(f"no query shows `{name}` any more")
    divergences = {name: {"openusd": d["openusd"]} for name, d in sorted(DIVERGENCES.items())}
    with open(os.path.join(out, "oracle.json"), "w") as f:
        f.write('{"openusd_version": "%d.%d",\n"divergences": %s,\n"cases": [\n'
                % (minor, patch, json.dumps(divergences, sort_keys=True)))
        for i, case in enumerate(cases):
            f.write('{"name": %s,\n"infos": %s,\n"prims": [\n'
                    % (json.dumps(case["name"]), json.dumps(case["infos"], indent=0)))
            for j, prim in enumerate(case["prims"]):
                head = {k: v for k, v in prim.items() if k != "attrs"}
                f.write(json.dumps(head, sort_keys=True)[:-1] + ', "attrs": [\n')
                f.write(",\n".join(json.dumps(a, sort_keys=True) for a in prim["attrs"]))
                f.write("\n]}" + ("," if j + 1 < len(case["prims"]) else "") + "\n")
            f.write("]}" + ("," if i + 1 < len(cases) else "") + "\n")
        f.write("]}\n")


def same_tree(a, b):
    cmp = filecmp.dircmp(a, b)
    if cmp.left_only or cmp.right_only or cmp.funny_files:
        return False
    _, mismatch, errors = filecmp.cmpfiles(a, b, cmp.common_files, shallow=False)
    if mismatch or errors:
        return False
    return all(same_tree(os.path.join(a, d), os.path.join(b, d)) for d in cmp.common_dirs)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=DEFAULT_OUT)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, "value_matrix")
        write(out)
        if not same_tree(out, args.out):
            raise SystemExit(f"{args.out} is stale")


if __name__ == "__main__":
    main()
