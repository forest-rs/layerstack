# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records OpenUSD's time-sampled array resolution as test vectors.

Usage: temporal_sparse_oracle.py [OUT_JSON]

Needs OpenUSD 26.03 or newer (sparse array edits) through Python `pxr`, for
example `pip install usd-core==26.8`. Writes
`layerstack_conformance/tests/data/temporal_sparse.json` by default, which
`tests/temporal_sparse.rs` replays against layerstack.

Each case is a set of USDA layers (`root.usda` is the stage root) written in
layerstack's array edit syntax, `edit (op, op)`. The script rewrites edits to
OpenUSD's `edit [op; op]` before handing the layers to OpenUSD, and records
for every query time and interpolation mode:

- `openusd`: `UsdAttribute::Get` on the composed stage;
- `flattened`: `UsdAttribute::Get` on `UsdStage::Flatten()` of that stage.

A case records the flattened layer text only when layerstack is expected to
resolve it to the composed result (`flatten_equivalent`).

Where OpenUSD's composed result is a defect, the case pins layerstack's
`expected` value instead and names the defect in `divergence`. The script
fails if OpenUSD no longer shows a recorded defect, so a fixed OpenUSD forces
the override to be removed.
"""
import json
import math
import os
import re
import sys
import tempfile

from pxr import Gf, Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(os.path.join(HERE, "..", "tests", "data", "temporal_sparse.json"))

HELD = "held"
LINEAR = "linear"

# OpenUSD defects the vectors pin instead of replicating.
DIVERGENCES = {
    "nan-before-first-sample": (
        "OpenUSD gives defaults and fallbacks the sample time -inf "
        "(pxr/usd/usd/stage.cpp, `_GetValueFromResolveInfoImpl`). Before the "
        "first composed time sample, the bracketing samples are (-inf, first) "
        "and `Usd_Interpolate` computes alpha = inf/inf, so interpolating "
        "types resolve to NaN under both held and linear interpolation. "
        "Composed and flattened stages agree on the first sample's value."
    ),
    "override-early-stop": (
        "After a non-composing lower sample moves the query to the upper "
        "sample's time, `_ResolveInfoResolver::ProcessLayerAtTime` stops at a "
        "weaker series whose upper sample does not compose, although its held "
        "value at that time does. Weaker opinions then drop out of the "
        "interpolated upper sample, so the value differs from the value at "
        "the upper sample's own time and from the flattened stage. The "
        "proposal's `Evaluate` (sparse-array-edits README, 'Evaluating a "
        "Strength-Ordering of Samples at a Specific Time') keeps composing "
        "and agrees with the flattened stage."
    ),
    "transparent-sampled-block": (
        "A sampled block that is a weaker series' lower bracketing sample, "
        "with a composing upper sample, contributes no samples "
        "(`_GetInterpolatingSamplesImpl` clears them) while the chain "
        "continues past it, so opinions weaker than the block show through. "
        "A block discards weaker opinions (AOUSD Core 12.3.6), and OpenUSD "
        "honours that at the block's own sample time."
    ),
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
         flatten_equivalent=True, flatten_note=None):
    """Registers a case.

    `queries` maps an attribute path to its query times (None is the default
    time). `divergences` maps (attribute, time, interp) to
    (expected value, DIVERGENCES key); interp None covers both modes.
    """
    CASES.append({
        "name": name,
        "description": description,
        "layers": layers,
        "queries": queries,
        "divergences": divergences or {},
        "flatten_equivalent": flatten_equivalent,
        "flatten_note": flatten_note,
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
                                   "int[] attr.timeSamples = { 3: edit (prepend 3, append 3), "
                                   "6: edit (prepend 6, append 7) }")),
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
                                   "int[] attr.timeSamples = { 3: edit (prepend 3, append 3), "
                                   "6: edit (prepend 6, append 7) }")),
        "base.usda": layer(prim("def", "TestBasics",
                                "int[] attr.timeSamples = { 1: edit (prepend -1, append -1), "
                                "5: edit (prepend -5, append -5), "
                                "9: edit (prepend -9, append -9) }")),
    },
    {"/TestBasics.attr": [0, 3, 4, 5, 6, 7, 9, 10]},
)

case(
    "usd_interpolation_single_layer",
    "TestInterpolation: a sparse `resize` sample interpolates towards a "
    "later dense sample.",
    {
        "root.usda": layer(prim("def", "TestInterpolation",
                                "float[] attr.timeSamples = { 1: edit (resize 4), "
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
                                   "float[] attr.timeSamples = { 2: edit (write 8 to [1]) }")),
        "base.usda": layer(prim("def", "TestInterpolation",
                                "float[] attr.timeSamples = { 1: edit (resize 4), "
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
                                  "float[] x.timeSamples = { 0: edit (write 10 to [0]), "
                                  "4: [7, 7, 7], 6: edit (append 1) }")),
        "mid.usda": layer(prim("over", "A",
                               "float[] x.timeSamples = { 1: edit (write 20 to [1]), "
                               "3: [1, 2, 3], 5: edit (erase [0]) }")),
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
                                  "int[] x.timeSamples = { 1: edit (append 10), "
                                  "3: edit (append 30) }")),
        "mid.usda": layer(prim("over", "A",
                               "int[] x.timeSamples = { 0: edit (prepend 0), "
                               "2: edit (prepend 2), 4: edit (prepend 4) }")),
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
                                  "5: edit (write 9 to [0]) }")),
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
                                  "5: edit (write 9 to [0]) }")),
        "mid.usda": layer(prim("over", "A",
                               "float[] x.timeSamples = { 4: edit (write 7 to [1]), "
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
                                  "float[] x.timeSamples = { 0: edit (write 9 to [0]), "
                                  "1: edit (write 5 to [0]) }")),
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
            "float[] x.timeSamples = { 6: edit (write 9 to [0]) }",
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
                                  "float[] x.timeSamples = { 3: edit (write 9 to [1]), "
                                  "5: edit (write 1 to [1]) }")),
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
                                  "float[] x.timeSamples = { 0.0000005: edit (write 9 to [0]), "
                                  "2: edit (write 7 to [0]) }",
                                  "float[] y.timeSamples = { 0.0000005: [5, 5], 2: [6, 6] }")),
        "weak.usda": layer(prim("def", "A",
                                "float[] x.timeSamples = { 0: [0, 0], 1: [1, 1], 2.0000005: [2, 2] }",
                                "float[] y.timeSamples = { 0: edit (write 1 to [1]), "
                                "1.9999995: edit (write 2 to [1]) }")),
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
                                  "float[] x.timeSamples = { 0: edit (write 9 to [0]), "
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
                                  "int[] x.timeSamples = { 0: edit (append 7), 2: None, "
                                  "4: edit (append 8) }")),
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
        "strong.usda": layer(prim("over", "A", "int[] x.timeSamples = { 0: edit (append 7) }")),
        "mid.usda": layer(prim("over", "A",
                               "int[] x.timeSamples = { 0: None, 2: edit (append 5), 4: [3] }")),
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
        "strong.usda": layer(prim("over", "A", "int[] x.timeSamples = { 0: edit (append 7) }")),
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
        "strong.usda": layer(prim("over", "A", "float[] x = edit (write 9 to [0])")),
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
                                  "int[] x.timeSamples = { 2: edit (write 9 to [0]), "
                                  "4: edit (write 5 to [0]) }")),
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
                                  "float[] x.timeSamples = { 2: edit (write 9 to [0]), "
                                  "4: edit (write 5 to [0]) }")),
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
                                "float[] x.timeSamples = { 1: edit (write 1 to [0]), 3: [2, 2] }")),
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
    "vector_sparse_over_dense_linear",
    "A sparse vector edit composes over both bracketing dense samples before "
    "interpolating.",
    {
        "root.usda": sublayers("@strong.usda@", "@weak.usda@"),
        "strong.usda": layer(prim("over", "A",
                                  "float3[] x.timeSamples = { 2: edit (write (9, 9, 9) to [0]) }")),
        "weak.usda": layer(prim("def", "A",
                                "float3[] x.timeSamples = { 1: [(0, 0, 0), (1, 1, 1)], "
                                "3: [(2, 2, 2), (3, 5, 7)] }")),
    },
    {"/A.x": [0, 1, 1.5, 2, 2.5, 3]},
)


# -- Driver ------------------------------------------------------------------------

EDIT = re.compile(r"\bedit\s*\(")


def to_openusd(text):
    """Rewrites layerstack's `edit (a, b)` array edits as `edit [a; b]`."""
    out = []
    pos = 0
    while True:
        m = EDIT.search(text, pos)
        if not m:
            out.append(text[pos:])
            return "".join(out)
        out.append(text[pos:m.start()])
        i = m.end()
        depth = 1
        parts, cur = [], []
        while depth:
            c = text[i]
            if c in "([":
                depth += 1
            elif c in ")]":
                depth -= 1
            if depth == 1 and c == ",":
                parts.append("".join(cur))
                cur = []
            elif depth:
                cur.append(c)
            i += 1
        parts.append("".join(cur))
        out.append("edit [" + "; ".join(p.strip() for p in parts if p.strip()) + "]")
        pos = i


def encode(value):
    """Encodes a resolved array or scalar as JSON (NaN as the string "NaN")."""
    if value is None:
        return None

    def num(x):
        x = float(x)
        return "NaN" if math.isnan(x) else x

    if isinstance(value, (int, float)):
        return num(value)

    out = []
    for item in value:
        if isinstance(item, (Gf.Vec2f, Gf.Vec3f, Gf.Vec4f, Gf.Vec2d, Gf.Vec3d, Gf.Vec4d)):
            out.append([num(c) for c in item])
        else:
            out.append(num(item))
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


def run_case(tmp, spec):
    d = tempfile.mkdtemp(dir=tmp)
    for name, text in spec["layers"].items():
        with open(os.path.join(d, name), "w") as f:
            f.write(to_openusd(text))
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
                    if close(openusd, expected):
                        sys.exit(f"{spec['name']} {attr}@{time} {interp}: OpenUSD no "
                                 f"longer shows `{reason}`; remove the override")
                    q["expected"] = expected
                    q["divergence"] = reason
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
    if spec["flatten_equivalent"]:
        out["flattened_layer"] = flattened_text(stage)
    else:
        out["flatten_note"] = spec["flatten_note"]
    return out


def main():
    out_path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    version = ".".join(str(v) for v in Usd.GetVersion())
    with tempfile.TemporaryDirectory() as tmp:
        cases = [run_case(tmp, spec) for spec in CASES]
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
