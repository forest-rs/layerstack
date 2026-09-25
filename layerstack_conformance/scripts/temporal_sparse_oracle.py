# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's time-sampled array resolution as test vectors.

Usage: temporal_sparse_oracle.py [OUT_JSON]

Needs OpenUSD 26.03 or newer (sparse array edits) through Python `pxr`, for
example `pip install usd-core==26.8`. Writes
`layerstack_conformance/tests/data/temporal_sparse.json` by default, which
`tests/temporal_sparse.rs` replays against layerstack.

Each case is a set of USDA layers (`root.usda` is the stage root), with array
edits in OpenUSD's syntax, `edit [op; op]`. The script records for every query
time and interpolation mode:

- `openusd`: `UsdAttribute::Get` on the composed stage;
- `flattened`: `UsdAttribute::Get` on `UsdStage::Flatten()` of that stage.

A case records the flattened layer text only when layerstack is expected to
resolve it to the composed result (`flatten_equivalent`).

Where Layerstack deliberately resolves differently, the query pins
Layerstack's `expected` value and names one of the `DIVERGENCES` in
`divergence`. The script refuses to write vectors when OpenUSD no longer
shows a recorded divergence (the override must go), when a divergence was
confirmed with a different OpenUSD release (its source lines must be
re-checked and its version bumped), or when no case shows a named
divergence any more.
"""
import json
import math
import os
import random
import struct
import sys
import tempfile

from pxr import Gf, Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(os.path.join(HERE, "..", "tests", "data", "temporal_sparse.json"))

HELD = "held"
LINEAR = "linear"

# The divergences some query shows.
USED = set()

# The named Core-versus-OpenUSD divergences: results Layerstack resolves
# differently from OpenUSD on purpose. Each records the OpenUSD release it was
# confirmed against (`openusd`), OpenUSD's behaviour and where it comes from
# (`openusd_behavior`, `openusd_source`, with paths and lines in that
# release), and the behaviour Layerstack follows instead with its authority
# (`layerstack`, `authority`). `docs/generic-sparse-composition.md` and
# `tests/temporal_sparse.rs` list the same names and versions; any other
# difference from OpenUSD is a failure.
DIVERGENCES = {
    "nan-before-first-sample": {
        "openusd": "0.26.8",
        "openusd_behavior": (
            "Defaults and fallbacks become samples at time -inf. Before the "
            "first composed time sample the bracketing samples are (-inf, "
            "first), and `Usd_Interpolate` computes alpha = inf/inf, so "
            "interpolating types resolve to NaN under both held and linear "
            "interpolation."
        ),
        "openusd_source": (
            "pxr/usd/usd/stage.cpp:8029 and :8054 "
            "(`_GetValueFromResolveInfoImpl` gives defaults and fallbacks time "
            "-inf); pxr/usd/usd/interpolators.cpp:125 (`Usd_Interpolate` alpha)"
        ),
        "layerstack": (
            "Holds the composed lower sample, as OpenUSD's own flattened "
            "stage does."
        ),
        "authority": (
            "AOUSD Core 12.5.1 (queries before the first sample return the "
            "first sample's value)"
        ),
    },
    "override-early-stop": {
        "openusd": "0.26.8",
        "openusd_behavior": (
            "After a non-composing lower sample moves the query to the upper "
            "sample's time, the resolve-info walk stops at a weaker series "
            "whose own upper sample does not compose, although its sample "
            "held at that time does. Weaker opinions drop out of the "
            "interpolated upper sample, which then differs from the value at "
            "the upper sample's own time and from the flattened stage."
        ),
        "openusd_source": (
            "pxr/usd/usd/stage.cpp:9032 "
            "(`_ResolveInfoResolver::ProcessLayerAtTime`, "
            "`if (!_overrideTime || upperComposes)`)"
        ),
        "layerstack": (
            "Keeps composing weaker series at the upper sample's time, and "
            "agrees with the flattened stage."
        ),
        "authority": (
            "sparse-array-edits proposal, 'Evaluating a Strength-Ordering of "
            "Samples at a Specific Time' (`Evaluate`); AOUSD Core 12.3.2 "
            "(time-based resolution visits every layer's samples, strongest "
            "first)"
        ),
    },
    "transparent-sampled-block": {
        "openusd": "0.26.8",
        "openusd_behavior": (
            "A sampled block that is a weaker series' lower bracketing "
            "sample, with a composing upper sample, contributes no samples "
            "while the walk continues past it, so opinions weaker than the "
            "block show through. At the block's own sample time OpenUSD "
            "honours the block."
        ),
        "openusd_source": (
            "pxr/usd/usd/interpolators.cpp:163 and :180 "
            "(`_GetInterpolatingSamplesImpl` clears the samples of a blocked "
            "lower sample); pxr/usd/usd/stage.cpp:9032 (the walk continues "
            "when the upper sample composes)"
        ),
        "layerstack": "Lets the block end the fold wherever it is held.",
        "authority": (
            "AOUSD Core 12.3.6 (a block discards weaker opinions; individual "
            "time samples can be blocked)"
        ),
    },
    "sampled-block-drops-fallback": {
        "openusd": "0.26.8",
        "openusd_behavior": (
            "A held sampled block resolves to no value, even where the "
            "attribute has a schema fallback, and sparse edits stronger than "
            "it compose over the empty array. A default block continues to "
            "the fallback instead, for both."
        ),
        "openusd_source": (
            "pxr/usd/usd/stage.cpp:9094 (`ProcessLayerAtTime` sends a default "
            "block to `ProcessFallback`; time samples never reach it), :8066 "
            "(edits compose over `VtBackground`) and :8078 "
            "(`Usd_ClearValueIfBlocked`)"
        ),
        "layerstack": (
            "Treats a sampled block like a default block: the schema-aware "
            "query resolves the fallback, and stronger edits compose over it."
        ),
        "authority": (
            "AOUSD Core 12.3.6 (a blocked strongest opinion resolves to the "
            "fallback, at any time) and 16.2.16.3 (blocked time samples have "
            "the same semantics as a blocked default)"
        ),
    },
    "default-time-block-hides-fallback": {
        "openusd": "0.26.8",
        "openusd_behavior": (
            "At the default time, a strongest default block resolves to no "
            "value even where the attribute has a schema fallback, although "
            "the resolve info names the fallback as its source and every "
            "numeric time resolves the fallback."
        ),
        "openusd_source": (
            "pxr/usd/usd/stage.cpp:7262 and :7272 "
            "(`Usd_AttrGetValueHelper::GetValue` reads the strongest `default` "
            "field and clears a block without consulting the fallback), while "
            ":9173 (`ProcessLayerAtDefault`) resolves to the fallback source"
        ),
        "layerstack": "Resolves the schema fallback, as at numeric times.",
        "authority": (
            "AOUSD Core 12.3.6 (if the strongest opinion is blocked and a "
            "fallback is available, the fallback is returned) and 16.2.16.2 "
            "(`None` skips authored weaker opinions and resolves only to the "
            "fallback)"
        ),
    },
}

HEADER = "#usda 1.0\n"


def sublayers(*names):
    """Root layer metadata listing `names` as sublayers, strongest first."""
    return HEADER + "(\n    subLayers = [\n" + ",\n".join(
        f"        {name}" for name in names) + "\n    ]\n)\n"


def prim(spec, name, *lines, meta=""):
    """A single prim spec with one property per line."""
    body = "".join(f"    {line}\n" for line in lines)
    return f'{spec} "{name}"{meta}\n{{\n{body}}}\n'


def layer(*prims):
    return HEADER + "\n".join(prims)


CASES = []


def case(name, description, layers, queries, divergences=None,
         flatten_equivalent=True, flatten_note=None, schema=None):
    """Registers a case.

    `queries` maps an attribute path to its query times (None is the default
    time). `divergences` maps (attribute, time, interp) to
    (expected value, DIVERGENCES key); interp None covers both modes.

    `schema` names a concrete prim type whose attribute fallbacks the case
    relies on, as `(type name, attribute names)`. The case then records the
    fallbacks from OpenUSD's schema registry, and layerstack resolves its
    queries with the same fallbacks through its schema-aware queries.
    """
    CASES.append({
        "name": name,
        "description": description,
        "layers": layers,
        "queries": queries,
        "divergences": divergences or {},
        "flatten_equivalent": flatten_equivalent,
        "flatten_note": flatten_note,
        "schema": schema,
    })


# -- Ports of pxr/usd/usd/testenv/testUsdAttributeArrayEdits.cpp -------------

case(
    "usd_basics_samples_over_default",
    "TestBasics: session-layer sparse samples compose over the root layer's "
    "dense default.",
    {
        # The C++ test's session layer is the stronger sublayer here.
        "root.usda": sublayers("@session.usda@", "@base.usda@"),
        "session.usda": layer(prim("over", "TestBasics",
                                   "int[] attr.timeSamples = { 3: edit [prepend 3; append 3], "
                                   "6: edit [prepend 6; append 7] }")),
        "base.usda": layer(prim("def", "TestBasics", "int[] attr = [3, 2, 1]")),
    },
    {"/TestBasics.attr": [0, 3, 5, 6, 7]},
)

case(
    "usd_basics_samples_over_samples",
    "TestBasics: root-layer sparse samples hide the root default and compose "
    "under the session-layer samples.",
    {
        "root.usda": sublayers("@session.usda@", "@base.usda@"),
        "session.usda": layer(prim("over", "TestBasics",
                                   "int[] attr.timeSamples = { 3: edit [prepend 3; append 3], "
                                   "6: edit [prepend 6; append 7] }")),
        "base.usda": layer(prim("def", "TestBasics",
                                "int[] attr.timeSamples = { 1: edit [prepend -1; append -1], "
                                "5: edit [prepend -5; append -5], "
                                "9: edit [prepend -9; append -9] }")),
    },
    {"/TestBasics.attr": [0, 3, 4, 5, 6, 7, 9, 10]},
)

case(
    "usd_interpolation_single_layer",
    "TestInterpolation: a sparse `resize` sample interpolates towards a "
    "later dense sample.",
    {
        "root.usda": layer(prim("def", "TestInterpolation",
                                "float[] attr.timeSamples = { 1: edit [resize 4], "
                                "3: [2, 4, 6, 8] }")),
    },
    {"/TestInterpolation.attr": [0, 1, 2, 3, 4]},
)

case(
    "usd_interpolation_session_edit",
    "TestInterpolation: a stronger sparse sample at a different time composes "
    "over both bracketing samples before interpolating.",
    {
        "root.usda": sublayers("@session.usda@", "@base.usda@"),
        "session.usda": layer(prim("over", "TestInterpolation",
                                   "float[] attr.timeSamples = { 2: edit [write 8 to [1]] }")),
        "base.usda": layer(prim("def", "TestInterpolation",
                                "float[] attr.timeSamples = { 1: edit [resize 4], "
                                "3: [2, 4, 6, 8] }")),
    },
    {"/TestInterpolation.attr": [0, 1, 2, 2.5, 3, 4]},
)

# -- Mixed dense and sparse samples at differing times ------------------------

case(
    "mixed_dense_sparse_three_layers",
    "Dense and sparse samples interleave across three layers at differing "
    "times.",
    {
        "root.usda": sublayers("@strong.usda@", "@mid.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float[] x.timeSamples = { 0: edit [write 10 to [0]], "
                                  "4: [7, 7, 7], 6: edit [append 1] }")),
        "mid.usda": layer(prim("over", "A",
                               "float[] x.timeSamples = { 1: edit [write 20 to [1]], "
                               "3: [1, 2, 3], 5: edit [erase [0]] }")),
        "weak.usda": layer(prim("def", "A",
                                "float[] x.timeSamples = { 0: [0, 0, 0], 2: [4, 4, 4], "
                                "8: [8, 8, 8] }")),
    },
    {"/A.x": [-1, 0, 0.5, 1, 1.5, 2, 2.5, 3, 3.5, 4, 4.5, 5, 5.5, 6, 7, 8, 9]},
)

case(
    "sparse_over_sparse_differing_times",
    "Two sparse series at interleaved times compose over a dense default.",
    {
        "root.usda": sublayers("@strong.usda@", "@mid.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "int[] x.timeSamples = { 1: edit [append 10], "
                                  "3: edit [append 30] }")),
        "mid.usda": layer(prim("over", "A",
                               "int[] x.timeSamples = { 0: edit [prepend 0], "
                               "2: edit [prepend 2], 4: edit [prepend 4] }")),
        "weak.usda": layer(prim("def", "A", "int[] x = [5]")),
    },
    {"/A.x": [-1, 0, 1, 1.5, 2, 2.5, 3, 3.5, 4, 5]},
)

case(
    "dense_lower_sparse_upper",
    "A dense lower sample hides weaker samples until the stronger series' "
    "next (sparse) sample, which composes over the weaker series there.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float[] x.timeSamples = { 1: [0, 0], "
                                  "5: edit [write 9 to [0]] }")),
        "weak.usda": layer(prim("def", "A",
                                "float[] x.timeSamples = { 2: [1, 1], 4: [2, 2], 6: [4, 4] }")),
    },
    {"/A.x": [0, 1, 2, 3, 4, 4.5, 5, 5.5, 6, 7]},
)

case(
    "dense_lower_sparse_upper_weak_upper_dense",
    "As `dense_lower_sparse_upper`, with a middle series whose sample held at "
    "the upper time is sparse and whose next sample is dense.",
    {
        "root.usda": sublayers("@strong.usda@", "@mid.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float[] x.timeSamples = { 1: [0, 0], "
                                  "5: edit [write 9 to [0]] }")),
        "mid.usda": layer(prim("over", "A",
                               "float[] x.timeSamples = { 4: edit [write 7 to [1]], "
                               "6: [3, 3] }")),
        "weak.usda": layer(prim("def", "A", "float[] x = [1, 1]")),
    },
    {"/A.x": [0, 1, 3, 4, 4.5, 5, 5.5, 6, 7]},
    divergences={
        ("/A.x", 3, LINEAR): ([4.5, 3.5], "override-early-stop"),
        ("/A.x", 4, LINEAR): ([6.75, 5.25], "override-early-stop"),
        ("/A.x", 4.5, LINEAR): ([7.875, 6.125], "override-early-stop"),
    },
)

# -- Layer offsets and scales ---------------------------------------------------

case(
    "sublayer_offset_scale",
    "A sparse series behind a sublayer offset and scale composes over a dense "
    "series in stage time.",
    {
        "root.usda": sublayers("@strong.usda@ (offset = 10; scale = 2)", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float[] x.timeSamples = { 0: edit [write 9 to [0]], "
                                  "1: edit [write 5 to [0]] }")),
        "weak.usda": layer(prim("def", "A",
                                "float[] x.timeSamples = { 10: [1, 2], 14: [3, 4] }")),
    },
    {"/A.x": [0, 10, 11, 12, 13, 14, 15]},
)

case(
    "reference_offset_scale",
    "A local sparse series composes over a referenced dense series mapped by "
    "the reference's offset and scale.",
    {
        "root.usda": layer(prim(
            "def", "A",
            "float[] x.timeSamples = { 6: edit [write 9 to [0]] }",
            meta=" (\n    references = @ref.usda@</B> (offset = 5; scale = 0.5)\n)")),
        "ref.usda": layer(prim("def", "B",
                               "float[] x.timeSamples = { 0: [0, 0], 4: [4, 8] }")),
    },
    {"/A.x": [4, 5, 5.5, 6, 6.5, 7, 8]},
)

case(
    "nested_sublayer_offsets",
    "Offsets compose through nested sublayers for both sparse and dense "
    "series.",
    {
        "root.usda": sublayers("@strong.usda@ (offset = -2)", "@mid.usda@ (offset = 2)"),
        "strong.usda": layer(prim("over", "A",
                                  "float[] x.timeSamples = { 3: edit [write 9 to [1]], "
                                  "5: edit [write 1 to [1]] }")),
        "mid.usda": sublayers("@weak.usda@ (scale = 2)"),
        "weak.usda": layer(prim("def", "A",
                                "float[] x.timeSamples = { 0: [0, 0], 2: [4, 4] }")),
    },
    {"/A.x": [0, 1, 2, 3, 4, 5, 6, 7]},
)

case(
    "scalar_samples_behind_sublayer_offset",
    "Scalar time samples behind a sublayer offset and scale map the query "
    "time through the inverse offset, like array samples.",
    {
        "root.usda": sublayers("@anim.usda@ (offset = 10; scale = 2)"),
        "anim.usda": layer(prim("def", "A", "double x.timeSamples = { 0: 0, 1: 10 }")),
    },
    {"/A.x": [9, 10, 11, 12, 13]},
)

# -- Near-coincident sample times ---------------------------------------------
#
# OpenUSD holds the lower of two bracketing samples of one series whose layer
# times are closer than 1e-6 (`GfIsClose`, strict), even under linear
# interpolation (`_GetInterpolatingSamplesImpl` in `interpolators.cpp`), and
# separately treats samples of different series closer than 1e-6 as one time
# when composing them (`SdfComposeTimeSampleSeries`).

for label, gap in (("below", "0.0000005"), ("at", "0.000001"), ("above", "0.000002")):
    g = float(gap)
    case(
        f"close_samples_{label}_tolerance",
        f"Two samples of one series {gap} apart, {label} the 1e-6 tolerance.",
        {
            "root.usda": layer(prim("def", "A",
                                    f"float[] x.timeSamples = {{ 0: [0], {gap}: [10] }}")),
        },
        {"/A.x": [-1, 0, g / 4, g / 2, g, 1]},
    )

case(
    "close_samples_compressed_by_scale",
    "A sublayer scale compresses samples 4 layer frames apart to 4e-7 stage "
    "frames; the tolerance applies to layer times.",
    {
        "root.usda": sublayers("@anim.usda@ (scale = 0.0000001)"),
        "anim.usda": layer(prim("def", "A", "float[] x.timeSamples = { 0: [0], 4: [10] }")),
    },
    {"/A.x": [0, 0.0000001, 0.0000002, 0.0000004, 1]},
    flatten_equivalent=False,
    flatten_note="Flattening moves the samples to stage times 4e-7 apart, "
                 "where the tolerance holds instead of interpolating.",
)

case(
    "close_samples_expanded_by_scale",
    "A sublayer scale spreads samples 5e-7 layer frames apart to 5e-6 stage "
    "frames; the tolerance applies to layer times.",
    {
        "root.usda": sublayers("@anim.usda@ (scale = 10)"),
        "anim.usda": layer(prim("def", "A",
                                "float[] x.timeSamples = { 0: [0], 0.0000005: [10] }")),
    },
    {"/A.x": [0, 0.0000025, 0.000005, 1]},
    flatten_equivalent=False,
    flatten_note="Flattening moves the samples to stage times 5e-6 apart, "
                 "beyond the tolerance, where they interpolate.",
)

case(
    "close_samples_blocked_upper",
    "A blocked upper sample, close to and far from the lower sample.",
    {
        "root.usda": layer(prim("def", "A",
                                "float[] near.timeSamples = { 0: [0], 0.0000005: None }",
                                "float[] far.timeSamples = { 0: [0], 2: None }")),
    },
    {"/A.near": [0, 0.00000025, 0.0000005, 1], "/A.far": [0, 1, 2, 3]},
)

case(
    "close_samples_across_series",
    "Sparse and dense samples of different series less than 1e-6 apart "
    "compose as one sample time.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float[] x.timeSamples = { 0.0000005: edit [write 9 to [0]], "
                                  "2: edit [write 7 to [0]] }",
                                  "float[] y.timeSamples = { 0.0000005: [5, 5], 2: [6, 6] }")),
        "weak.usda": layer(prim("def", "A",
                                "float[] x.timeSamples = { 0: [0, 0], 1: [1, 1], 2.0000005: [2, 2] }",
                                "float[] y.timeSamples = { 0: edit [write 1 to [1]], "
                                "1.9999995: edit [write 2 to [1]] }")),
    },
    {"/A.x": [0, 0.00000025, 0.0000005, 0.5, 1, 1.5, 2, 2.00000025, 2.0000005, 3],
     "/A.y": [0, 0.00000025, 0.0000005, 1, 1.9999995, 2, 3]},
    flatten_equivalent=False,
    flatten_note="Under held interpolation only each series' lower sample "
                 "composes, so a weaker sample just after a stronger one "
                 "does not merge with it; flattening composes every sample.",
)

# -- Blocks ----------------------------------------------------------------------

case(
    "sampled_block_in_strong_series",
    "A sampled block in the strongest series blocks while it is the held "
    "sample.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float[] x.timeSamples = { 0: edit [write 9 to [0]], "
                                  "2: None, 4: [7, 7] }")),
        "weak.usda": layer(prim("def", "A", "float[] x = [1, 2]")),
    },
    {"/A.x": [-1, 0, 1, 2, 3, 4, 5]},
    divergences={
        ("/A.x", -1, None): ([9, 2], "nan-before-first-sample"),
    },
)

case(
    "block_between_edits_in_one_series",
    "A sampled block between two sparse samples of the strongest series.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "int[] x.timeSamples = { 0: edit [append 7], 2: None, "
                                  "4: edit [append 8] }")),
        "weak.usda": layer(prim("def", "A", "int[] x = [1, 2]")),
    },
    {"/A.x": [-1, 0, 1, 2, 3, 4, 5]},
)

case(
    "sampled_block_in_weaker_series",
    "A sampled block in a weaker series ends the fold for the sparse sample "
    "above it; edits above materialize over the empty array.",
    {
        "root.usda": sublayers("@strong.usda@", "@mid.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A", "int[] x.timeSamples = { 0: edit [append 7] }")),
        "mid.usda": layer(prim("over", "A",
                               "int[] x.timeSamples = { 0: None, 2: edit [append 5], 4: [3] }")),
        "weak.usda": layer(prim("def", "A", "int[] x = [1, 2]")),
    },
    {"/A.x": [-1, 0, 1, 1.5, 2, 3, 4, 5]},
    divergences={
        ("/A.x", 1, None): ([7], "transparent-sampled-block"),
        ("/A.x", 1.5, None): ([7], "transparent-sampled-block"),
    },
    flatten_equivalent=False,
    flatten_note="OpenUSD's Flatten() writes [1, 2, 7] at time 0, although "
                 "the composed stage resolves [7] there.",
)

case(
    "default_block_between_series",
    "A default block between a sparse series and a dense default.",
    {
        "root.usda": sublayers("@strong.usda@", "@mid.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A", "int[] x.timeSamples = { 0: edit [append 7] }")),
        "mid.usda": layer(prim("over", "A", "int[] x = None")),
        "weak.usda": layer(prim("def", "A", "int[] x = [1, 2]")),
    },
    {"/A.x": [0, 1]},
    flatten_equivalent=False,
    flatten_note="Flattening writes the block as the default next to the "
                 "samples; layerstack's layer model keeps one of the two.",
)

case(
    "block_as_upper_sample",
    "Linear interpolation towards a sampled block holds the lower sample.",
    {
        "root.usda": layer(prim("def", "A", "float[] x.timeSamples = { 0: [1, 1], 2: None }")),
    },
    {"/A.x": [0, 1, 2, 3]},
)

# -- Defaults and time samples -----------------------------------------------------

case(
    "sparse_default_over_weaker_samples",
    "A sparse default composes over every sample of a weaker series.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A", "float[] x = edit [write 9 to [0]]")),
        "weak.usda": layer(prim("def", "A", "float[] x.timeSamples = { 1: [0, 0], 3: [2, 2] }")),
    },
    {"/A.x": [0, 1, 2, 3, 4]},
    divergences={
        ("/A.x", 0, None): ([9, 0], "nan-before-first-sample"),
    },
)

case(
    "sparse_samples_over_weaker_default_int",
    "Sparse samples compose over a weaker dense default; before the first "
    "sample the first sample holds.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "int[] x.timeSamples = { 2: edit [write 9 to [0]], "
                                  "4: edit [write 5 to [0]] }")),
        "weak.usda": layer(prim("def", "A", "int[] x = [1, 2]")),
    },
    {"/A.x": [0, 2, 3, 4, 5]},
)

case(
    "sparse_samples_over_weaker_default_float",
    "As `sparse_samples_over_weaker_default_int` with an interpolating type.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float[] x.timeSamples = { 2: edit [write 9 to [0]], "
                                  "4: edit [write 5 to [0]] }")),
        "weak.usda": layer(prim("def", "A", "float[] x = [1, 2]")),
    },
    {"/A.x": [0, 2, 3, 4, 5]},
    divergences={
        ("/A.x", 0, None): ([9, 2], "nan-before-first-sample"),
    },
)

case(
    "dense_default_hides_weaker_samples",
    "A stronger dense default hides a weaker series entirely.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A", "float[] x = [5, 5]")),
        "weak.usda": layer(prim("def", "A",
                                "float[] x.timeSamples = { 1: edit [write 1 to [0]], 3: [2, 2] }")),
    },
    {"/A.x": [0, 1, 2, 3, 4]},
)

# -- Interpolation of composed arrays -----------------------------------------------

case(
    "dense_arrays_interpolate_by_element_type",
    "Linear interpolation of dense array samples follows the element type: "
    "float, double and vector elements interpolate, integers hold, and "
    "arrays of different sizes hold.",
    {
        "root.usda": layer(prim(
            "def", "A",
            "float[] f.timeSamples = { 0: [0, 0], 2: [2, 4] }",
            "double[] d.timeSamples = { 0: [0, 0], 2: [2, 4] }",
            "int[] i.timeSamples = { 0: [0, 0], 2: [2, 4] }",
            "float3[] v.timeSamples = { 0: [(0, 0, 0)], 2: [(2, 4, 6)] }",
            "float[] sized.timeSamples = { 0: [0, 0], 2: [2, 4, 6] }",
        )),
    },
    {"/A.f": [1], "/A.d": [1], "/A.i": [1], "/A.v": [1], "/A.sized": [1]},
)

case(
    "half_arrays_interpolate",
    "Half-precision arrays and half vector arrays interpolate element by "
    "element, rounding each result to half precision; a sparse edit composes "
    "first, and arrays of different sizes hold. The values are exact halves.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "half[] e.timeSamples = { 1: edit [write 8 to [1]] }")),
        "weak.usda": layer(prim(
            "def", "A",
            "half[] h.timeSamples = { 0: [0, 1, -2, 0.0999755859375], "
            "2: [1, 3, 0.5, 1000] }",
            "half3[] v.timeSamples = { 0: [(0, 0, 0), (1, 1, 1)], "
            "2: [(1, 2, 3), (0.5, 0.25, 2)] }",
            "half[] sized.timeSamples = { 0: [0, 0], 2: [2, 4, 6] }",
            "half[] e.timeSamples = { 0: [0, 0], 2: [2, 4] }",
        )),
    },
    {"/A.h": [0, 0.3, 0.5, 1, 1.7, 2],
     "/A.v": [0, 0.3, 1, 1.9],
     "/A.sized": [1],
     "/A.e": [0, 0.5, 1, 1.5, 2]},
)

case(
    "quaternion_arrays_slerp",
    "Quaternion arrays interpolate element by element with `GfSlerp`, taking "
    "the shorter arc; a sparse edit composes first, and arrays of different "
    "sizes hold. The `quath` values are exact halves.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "quatf[] e.timeSamples = { 1: edit [write (0, 0, 1, 0) to [1]] }")),
        "weak.usda": layer(prim(
            "def", "A",
            "quath[] h.timeSamples = { 0: [(1, 0, 0, 0), (0.5, 0.5, 0.5, 0.5)], "
            "2: [(0, 0, 0, 1), (-0.5, 0.5, 0.5, 0.5)] }",
            "quatf[] f.timeSamples = { 0: [(1, 0, 0, 0), (0.5, 0.5, 0.5, 0.5)], "
            "2: [(0, 0, 0, 1), (-0.5, 0.5, 0.5, 0.5)] }",
            "quatd[] d.timeSamples = { 0: [(1, 0, 0, 0), (1, 0, 0, 0)], "
            "2: [(0.6, 0, 0.8, 0), (0.999995, 0.0031622, 0, 0)] }",
            "quatf[] sized.timeSamples = { 0: [(1, 0, 0, 0)], "
            "2: [(0, 0, 0, 1), (1, 0, 0, 0)] }",
            "quatf[] e.timeSamples = { 0: [(1, 0, 0, 0), (1, 0, 0, 0)], "
            "2: [(0, 0, 0, 1), (1, 0, 0, 0)] }",
        )),
    },
    {"/A.h": [0, 0.3, 0.5, 1, 1.7, 2],
     "/A.f": [0, 0.3, 0.5, 1, 1.7, 2],
     "/A.d": [0, 0.3, 0.5, 1, 1.7, 2],
     "/A.sized": [1],
     "/A.e": [0, 1, 1.5, 2]},
)

case(
    "vector_sparse_over_dense_linear",
    "A sparse vector edit composes over both bracketing dense samples before "
    "interpolating.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float3[] x.timeSamples = { 2: edit [write (9, 9, 9) to [0]] }")),
        "weak.usda": layer(prim("def", "A",
                                "float3[] x.timeSamples = { 1: [(0, 0, 0), (1, 1, 1)], "
                                "3: [(2, 2, 2), (3, 5, 7)] }")),
    },
    {"/A.x": [0, 1, 1.5, 2, 2.5, 3]},
)

# -- Scalar time samples -------------------------------------------------------------

case(
    "scalars_interpolate_by_element_type",
    "Scalar samples follow the element rules of arrays: integers hold, while "
    "floating-point scalars, vectors and matrices interpolate "
    "(`USD_LINEAR_INTERPOLATION_TYPES`).",
    {
        "root.usda": layer(prim(
            "def", "A",
            "int i.timeSamples = { 0: 0, 2: 4 }",
            "int64 i64.timeSamples = { 0: 0, 2: 4 }",
            "float f.timeSamples = { 0: 0, 2: 1 }",
            "double d.timeSamples = { 0: 0, 2: 1 }",
            "float3 v.timeSamples = { 0: (0, 0, 0), 2: (1, 2, 3) }",
            "double3 dv.timeSamples = { 0: (0, 0, 0), 2: (1, 2, 3) }",
            "matrix2d m.timeSamples = { 0: ((0, 0), (0, 0)), 2: ((2, 4), (6, 8)) }",
        )),
    },
    {attr: [-1, 0, 0.5, 1, 1.5, 2, 3]
     for attr in ("/A.i", "/A.i64", "/A.f", "/A.d", "/A.v", "/A.dv", "/A.m")},
)

case(
    "half_scalars_interpolate",
    "Half-precision scalars and vectors interpolate, rounding to half "
    "precision. The values are exact halves.",
    {
        "root.usda": layer(prim(
            "def", "A",
            "half h.timeSamples = { 0: 0.0999755859375, 2: 1 }",
            "half2 v2.timeSamples = { 0: (0, 1), 2: (1, 0.5) }",
            "half3 v3.timeSamples = { 0: (0, 0, 0), 2: (1, 2, 3) }",
            "half4 v4.timeSamples = { 0: (0, 0, 0, 0), 2: (1, -1, 1000, 0.25) }",
        )),
    },
    {attr: [0, 0.3, 0.5, 1, 1.7] for attr in ("/A.h", "/A.v2", "/A.v3", "/A.v4")},
)

case(
    "quaternion_scalars_slerp",
    "Quaternion scalars interpolate with `GfSlerp` in their own precision: "
    "the shorter arc when the dot product is negative, and a plain lerp when "
    "the two rotations are within 1e-5 of each other. The `quath` values are "
    "exact halves.",
    {
        "root.usda": layer(prim(
            "def", "A",
            "quath h.timeSamples = { 0: (1, 0, 0, 0), 2: (0, 0, 0, 1) }",
            "quath hflip.timeSamples = { 0: (1, 0, 0, 0), 2: (-0.5, 0.5, 0.5, 0.5) }",
            "quatf f.timeSamples = { 0: (1, 0, 0, 0), 2: (0.6, 0, 0.8, 0) }",
            "quatf fflip.timeSamples = { 0: (0.5, 0.5, 0.5, 0.5), 2: (-0.6, 0, -0.8, 0) }",
            "quatd d.timeSamples = { 0: (0.5, 0.5, 0.5, 0.5), 2: (0, 0, 0, 1) }",
            "quatd near.timeSamples = { 0: (1, 0, 0, 0), 2: (0.999995, 0.0031622, 0, 0) }",
        )),
    },
    {attr: [0, 0.3, 0.5, 1, 1.7, 2]
     for attr in ("/A.h", "/A.hflip", "/A.f", "/A.fflip", "/A.d", "/A.near")},
)

case(
    "scalar_samples_close_together",
    "Two scalar samples closer than 1e-6 in layer time hold the lower one, "
    "as array samples do.",
    {
        "root.usda": layer(prim("def", "A",
                                "double x.timeSamples = { 0: 0, 0.0000005: 10, 1: 20 }")),
    },
    {"/A.x": [0, 0.00000025, 0.0000005, 0.5]},
)


# -- Bit-exact interpolation --------------------------------------------------------
#
# OpenUSD interpolates with `GfLerp`, `(1 - alpha) * a + alpha * b`, in the
# arithmetic of the value type: a scalar lerps in double precision and narrows
# once, while a vector scales and adds with its own `GfVec` operators, which
# narrow each scaled component before adding. These cases record that
# arithmetic bit for bit (`exact`): seeded random endpoints, often of opposite
# sign or nearly equal so the two terms cancel, plus the review probes that
# first showed the difference, each as a scalar and as array elements.

EXACT_TIMES = [0.31410496844772895, 0.5, 0.500000001]


def narrow(value, kind):
    """Rounds `value` to the element type `kind` ('h', 'f' or 'd')."""
    if kind == "d":
        return value
    value = struct.unpack("f", struct.pack("f", value))[0]
    if kind == "h":
        value = struct.unpack("e", struct.pack("e", value))[0]
    return value


def exact_component(rng, kind):
    """A random component exactly representable in `kind`, avoiding the
    subnormals and the range that layerstack's USDA reader cannot keep."""
    top = 4 if kind == "h" else 10
    magnitude = 10 ** rng.uniform(-3, top)
    value = narrow(rng.choice((-1, 1)) * magnitude, kind)
    return min(max(value, -60000.0), 60000.0) if kind == "h" else value


def exact_pair(rng, kind, count):
    """Two endpoints of `count` components: opposite, nearly equal or
    unrelated."""
    a = [exact_component(rng, kind) for _ in range(count)]
    style = rng.randrange(3)
    if style == 0:
        b = [-x for x in a]
    elif style == 1:
        b = [narrow(x * (1 + rng.uniform(-1e-3, 1e-3)), kind) for x in a]
    else:
        b = [exact_component(rng, kind) for _ in range(count)]
    return a, b


def rotation_pair(rng, kind, count):
    """Two quaternions (real part first): a random rotation and one that is
    unrelated, its negation (the longer arc), or within 1e-5 of it (the
    lerp branch). Components are exact in `kind` and rarely exactly unit."""
    def unit():
        q = [rng.gauss(0, 1) for _ in range(4)]
        norm = math.sqrt(sum(c * c for c in q))
        return [narrow(c / norm, kind) for c in q]
    a = unit()
    style = rng.randrange(3)
    if style == 0:
        b = unit()
    elif style == 1:
        b = [narrow(-c * (1 + rng.uniform(-0.2, 0.2)), kind) for c in a]
    else:
        b = [narrow(c + rng.uniform(-3e-3, 3e-3), kind) for c in a]
    return a, b


def usda_value(components, shape):
    """USDA text for one value: a number, a tuple, or rows of a matrix."""
    text = [repr(float(c)) for c in components]
    if shape == 1:
        return text[0]
    if isinstance(shape, tuple):
        n = shape[1]
        return "(" + ", ".join(
            "(" + ", ".join(text[r * n:(r + 1) * n]) + ")" for r in range(n)) + ")"
    return "(" + ", ".join(text) + ")"


def exact_case(name, description, types, seed, probes=(), scalars=3, elements=6, ulps=0):
    """Registers a bit-exact interpolation case.

    `types` lists (USDA type, kind, shape), where shape is the component
    count, ("matrix", n), or "quat" for rotations. Each type gets `scalars` scalar attributes and one
    array of `elements`; each probe (type, a, b) adds a scalar and a
    one-element array. `ulps` allows that many units in the last place,
    where OpenUSD's result depends on the C library's `acos` and `sin`.
    """
    rng = random.Random(seed)
    lines, attrs = [], []

    def add(type_name, shape, samples, suffix, array=False):
        attr = f"{type_name}_{suffix}"
        text = [("[" + ", ".join(usda_value(v, shape) for v in s) + "]") if array
                else usda_value(s, shape) for s in samples]
        lines.append(f"{type_name}{'[]' if array else ''} {attr}.timeSamples = "
                     f"{{ 0: {text[0]}, 1: {text[1]} }}")
        attrs.append(attr)

    for type_name, kind, shape in types:
        pair = rotation_pair if shape == "quat" else exact_pair
        count = {"quat": 4}.get(shape, shape) if not isinstance(shape, tuple) \
            else shape[1] ** 2
        for k in range(scalars):
            add(type_name, shape, pair(rng, kind, count), f"s{k}")
        pairs = [pair(rng, kind, count) for _ in range(elements)]
        add(type_name, shape, ([a for a, _ in pairs], [b for _, b in pairs]), "array",
            array=True)
    for k, (type_name, shape, a, b) in enumerate(probes):
        add(type_name, shape, (a, b), f"probe{k}")
        add(type_name, shape, ([a], [b]), f"probe{k}_array", array=True)
    times = EXACT_TIMES + sorted(rng.random() for _ in range(2))
    case(
        name,
        description,
        {"root.usda": layer(prim("def", "A", *lines))},
        {f"/A.{attr}": times for attr in attrs},
        flatten_equivalent=False,
        flatten_note="The case checks interpolation arithmetic; flattening "
                     "rewrites the sample text.",
    )
    CASES[-1]["exact"] = True
    CASES[-1]["ulps"] = ulps


exact_case(
    "vectors_interpolate_bit_exact",
    "Float, double, vector and matrix scalars and arrays interpolate bit for "
    "bit as OpenUSD does: scalars in double precision, rounded once; float "
    "vectors rounding each scaled component to float before adding.",
    [("float", "f", 1), ("float2", "f", 2), ("float3", "f", 3), ("float4", "f", 4),
     ("double", "d", 1), ("double2", "d", 2), ("double3", "d", 3), ("double4", "d", 4),
     ("matrix2d", "d", ("matrix", 2)), ("matrix3d", "d", ("matrix", 3)),
     ("matrix4d", "d", ("matrix", 4))],
    seed=0x5eed_0001,
    probes=[("float3", 3, [1e10] * 3, [-1e10] * 3)],
)

exact_case(
    "half_vectors_interpolate_bit_exact",
    "Half scalars and half vectors interpolate bit for bit as OpenUSD does: "
    "a half scalar in double precision, narrowed once; a half vector scaling "
    "by the alpha narrowed to float, narrowing each term to half and adding "
    "in half.",
    [("half", "h", 1), ("half2", "h", 2), ("half3", "h", 3), ("half4", "h", 4)],
    seed=0x5eed_0002,
    probes=[("half2", 2, [-1.3857421875] * 2, [41.375] * 2)],
)

exact_case(
    "quaternions_interpolate_bit_exact",
    "`quath` and `quatf` scalars and arrays slerp bit for bit as `GfSlerp` "
    "does in their scalar type: random rotations, negated ones that take the "
    "shorter arc, and nearly equal ones that take the lerp branch.",
    [("quath", "h", "quat"), ("quatf", "f", "quat")],
    seed=0x5eed_0003,
    scalars=4,
    elements=8,
)

exact_case(
    "quatd_interpolates_to_the_last_place",
    "`quatd` slerps as `GfSlerp` does in double precision. Its result keeps "
    "the last-place error of `acos` and `sin`, which differs between C "
    "libraries (layerstack uses `libm` on every target), so it matches to "
    "four units in the last place.",
    [("quatd", "d", "quat")],
    seed=0x5eed_0004,
    scalars=4,
    elements=8,
    ulps=4,
)

# -- Schema fallbacks at a time -------------------------------------------------------
#
# `Cube` declares `float3[] extent = [(-1, -1, -1), (1, 1, 1)]` and
# `double size = 2`. Layerstack resolves these cases through
# `Stage::resolve_value_at_time_with_schema` (and `resolve_field_with_schema`
# at the default time).

CUBE = ("Cube", ("extent", "size"))
FALLBACK_EXTENT = [[-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]]

case(
    "schema_fallback_only",
    "Nothing authored, or a spec without a value, resolves the schema "
    "fallback at every time.",
    {
        "root.usda": layer(
            prim("def Cube", "A"),
            prim("def Cube", "B",
                 'double size (\n        customData = { string note = "no value" }\n    )',
                 'float3[] extent (\n        customData = { string note = "no value" }\n    )'),
        ),
    },
    {attr: [None, -1, 0, 1] for attr in ("/A.extent", "/A.size", "/B.extent", "/B.size")},
    schema=CUBE,
)

case(
    "schema_sparse_samples_over_fallback",
    "Time-sampled sparse edits compose over the schema fallback; arrays of "
    "different sizes hold.",
    {
        "root.usda": layer(prim(
            "def Cube", "A",
            "float3[] extent.timeSamples = { 1: edit [write (0, 0, 0) to [0]], "
            "3: edit [append (5, 5, 5)] }")),
    },
    {"/A.extent": [None, 0, 1, 2, 3, 4]},
    divergences={
        ("/A.extent", 0, None): ([[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]],
                                 "nan-before-first-sample"),
    },
    schema=CUBE,
)

case(
    "schema_default_block",
    "A default block discards weaker opinions; stronger sampled edits compose "
    "over the schema fallback, and with nothing stronger the fallback "
    "resolves.",
    {
        "root.usda": sublayers("@strong.usda@", "@mid.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float3[] extent.timeSamples = { 1: edit [append (5, 5, 5)] }")),
        "mid.usda": layer(prim("over", "A", "float3[] extent = None", "double size = None")),
        "weak.usda": layer(prim("def Cube", "A",
                                "float3[] extent = [(2, 2, 2)]", "double size = 5")),
    },
    {"/A.extent": [None, 0, 1, 2], "/A.size": [None, 0, 1]},
    divergences={
        ("/A.extent", None, None): (FALLBACK_EXTENT, "default-time-block-hides-fallback"),
        ("/A.extent", 0, None): (FALLBACK_EXTENT + [[5.0, 5.0, 5.0]],
                                 "nan-before-first-sample"),
        ("/A.size", None, None): (2.0, "default-time-block-hides-fallback"),
    },
    flatten_equivalent=False,
    flatten_note="Flattening writes the block as the default next to the "
                 "samples; layerstack's layer model keeps one of the two.",
    schema=CUBE,
)

case(
    "schema_sampled_block",
    "A held sampled block resolves the schema fallback, and stronger sparse "
    "edits compose over the fallback, as over a default block.",
    {
        "root.usda": sublayers("@strong.usda@", "@mid.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float3[] extent.timeSamples = { 1: edit [append (5, 5, 5)] }")),
        "mid.usda": layer(prim("over", "A",
                               "float3[] extent.timeSamples = { 0: None, 2: [(3, 3, 3)] }")),
        "weak.usda": layer(
            prim("def Cube", "A", "float3[] extent = [(2, 2, 2)]"),
            prim("def Cube", "B",
                 "float3[] extent.timeSamples = { 0: [(1, 2, 3)], 2: None, 4: [(4, 4, 4)] }",
                 "double size.timeSamples = { 0: 1, 2: None, 4: 4 }"),
        ),
    },
    {"/A.extent": [None, 0, 1, 1.5, 2, 3],
     "/B.extent": [None, 0, 1, 2, 3, 4],
     "/B.size": [None, 0, 1, 2, 3, 4]},
    divergences={
        **{("/A.extent", t, None): (FALLBACK_EXTENT + [[5.0, 5.0, 5.0]],
                                    "sampled-block-drops-fallback")
           for t in (0, 1, 1.5)},
        **{("/B.extent", t, None): (FALLBACK_EXTENT, "sampled-block-drops-fallback")
           for t in (2, 3)},
        **{("/B.size", t, None): (2.0, "sampled-block-drops-fallback")
           for t in (2, 3)},
    },
    flatten_equivalent=False,
    flatten_note="Flattening composes the edit over the sampled block into a "
                 "dense sample, which no longer sees the fallback.",
    schema=CUBE,
)

case(
    "schema_samples_behind_layer_offset",
    "Sparse edits over the schema fallback and scalar samples behind a "
    "sublayer offset and scale.",
    {
        "root.usda": sublayers("@anim.usda@ (offset = 10; scale = 2)")
        + "\n" + prim("def Cube", "A"),
        "anim.usda": layer(prim(
            "over", "A",
            "float3[] extent.timeSamples = { 0: edit [write (0, 0, 0) to [0]], "
            "1: edit [write (4, 4, 4) to [0]] }",
            "double size.timeSamples = { 0: 1, 1: 3 }")),
    },
    {"/A.extent": [None, 9, 10, 11, 12, 13], "/A.size": [None, 9, 10, 11, 12, 13]},
    divergences={
        ("/A.extent", 9, None): ([[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]],
                                 "nan-before-first-sample"),
    },
    schema=CUBE,
)


# -- Driver ------------------------------------------------------------------------

VECTORS = (Gf.Vec2h, Gf.Vec3h, Gf.Vec4h, Gf.Vec2f, Gf.Vec3f, Gf.Vec4f,
           Gf.Vec2d, Gf.Vec3d, Gf.Vec4d)
MATRICES = (Gf.Matrix2d, Gf.Matrix3d, Gf.Matrix4d)
QUATERNIONS = (Gf.Quath, Gf.Quatf, Gf.Quatd)


def num(x):
    x = float(x)
    return "NaN" if math.isnan(x) else x


def components(value):
    """A tuple-valued element's components, or None for a plain number.

    Quaternions list the real part first, as USDA writes them.
    """
    if isinstance(value, QUATERNIONS):
        return [num(value.GetReal())] + [num(c) for c in value.GetImaginary()]
    if isinstance(value, MATRICES):
        return [num(c) for row in value for c in row]
    if isinstance(value, VECTORS):
        return [num(c) for c in value]
    return None


def encode(value):
    """Encodes a resolved value as JSON (NaN as the string "NaN").

    An array is a list of elements, each a number or a list of components;
    a scalar is a number, or `{"tuple": components}` for a vector, matrix or
    quaternion.
    """
    if value is None:
        return None
    if isinstance(value, (int, float)):
        return num(value)
    scalar = components(value)
    if scalar is not None:
        return {"tuple": scalar}
    out = []
    for item in value:
        comps = components(item)
        out.append(num(item) if comps is None else comps)
    return out


def resolve(stage, attr, time, interp):
    stage.SetInterpolationType(
        Usd.InterpolationTypeHeld if interp == HELD else Usd.InterpolationTypeLinear)
    a = stage.GetAttributeAtPath(attr)
    assert a, attr
    value = a.Get() if time is None else a.Get(time)
    return encode(value)


def flattened_text(stage):
    layer = stage.Flatten()
    # Drop the generated `doc`, which names the temporary directory.
    layer.documentation = ""
    return layer.ExportToString()


def close(a, b):
    if a is None or b is None:
        return a is b
    if isinstance(a, dict) or isinstance(b, dict):
        return (isinstance(a, dict) and isinstance(b, dict)
                and close(a["tuple"], b["tuple"]))
    if not isinstance(a, list) or not isinstance(b, list):
        return close([a], [b]) if type(a) is type(b) else False
    if len(a) != len(b):
        return False
    for x, y in zip(a, b):
        if isinstance(x, list) != isinstance(y, list):
            return False
        if isinstance(x, list):
            if not close(x, y):
                return False
        elif x == "NaN" or y == "NaN":
            if x != y:
                return False
        elif abs(x - y) > 1e-5:
            return False
    return True


def run_case(tmp, spec, version):
    d = tempfile.mkdtemp(dir=tmp)
    for name, text in spec["layers"].items():
        with open(os.path.join(d, name), "w") as f:
            f.write(text)
    stage = Usd.Stage.Open(os.path.join(d, "root.usda"))
    flat = Usd.Stage.Open(stage.Flatten())
    queries = []
    for attr, times in spec["queries"].items():
        for time in times:
            for interp in (HELD, LINEAR):
                openusd = resolve(stage, attr, time, interp)
                q = {
                    "attr": attr,
                    "time": time,
                    "interp": interp,
                    "openusd": openusd,
                    "flattened": resolve(flat, attr, time, interp),
                }
                div = (spec["divergences"].get((attr, time, interp))
                       or spec["divergences"].get((attr, time, None)))
                if div:
                    expected, reason = div
                    if reason not in DIVERGENCES:
                        sys.exit(f"{spec['name']}: unnamed divergence `{reason}`")
                    if close(openusd, expected):
                        sys.exit(f"{spec['name']} {attr}@{time} {interp}: OpenUSD no "
                                 f"longer shows `{reason}`; remove the override")
                    if DIVERGENCES[reason]["openusd"] != version:
                        sys.exit(f"{spec['name']} {attr}@{time} {interp}: `{reason}` "
                                 f"was confirmed with OpenUSD "
                                 f"{DIVERGENCES[reason]['openusd']}, not {version}; "
                                 "re-check its source lines and bump it")
                    q["expected"] = expected
                    q["divergence"] = reason
                    USED.add(reason)
                queries.append(q)
    for key in spec["divergences"]:
        if not any(q["attr"] == key[0] and q["time"] == key[1] for q in queries):
            sys.exit(f"{spec['name']}: divergence {key} matches no query")
    out = {
        "name": spec["name"],
        "description": spec["description"],
        "layers": spec["layers"],
        "queries": queries,
    }
    if spec.get("exact"):
        out["exact"] = True
        if spec["ulps"]:
            out["ulps"] = spec["ulps"]
    if spec["schema"]:
        type_name, names = spec["schema"]
        definition = Usd.SchemaRegistry().FindConcretePrimDefinition(type_name)
        out["schema"] = {
            "type": type_name,
            "fallbacks": {name: encode(definition.GetAttributeFallbackValue(name))
                          for name in names},
        }
    if spec["flatten_equivalent"]:
        out["flattened_layer"] = flattened_text(stage)
    else:
        out["flatten_note"] = spec["flatten_note"]
    return out


def main():
    out_path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    version = ".".join(str(v) for v in Usd.GetVersion())
    with tempfile.TemporaryDirectory() as tmp:
        cases = [run_case(tmp, spec, version) for spec in CASES]
    unused = sorted(set(DIVERGENCES) - USED)
    if unused:
        sys.exit(f"divergences no case shows: {unused}")
    doc = {
        "generator": "layerstack_conformance/scripts/temporal_sparse_oracle.py",
        "openusd_version": version,
        "divergences": DIVERGENCES,
        "cases": cases,
    }
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=1, sort_keys=False)
        f.write("\n")
    print(f"wrote {len(cases)} cases from OpenUSD {version} to {out_path}")


if __name__ == "__main__":
    main()
