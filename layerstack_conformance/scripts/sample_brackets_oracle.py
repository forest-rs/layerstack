# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records, call by call, OpenUSD's sample-time reads: the times in an
interval, the sample count, the bracketing samples of a time and the union
of several attributes' times, on `UsdAttribute` and `UsdAttributeQuery`.

Usage: sample_brackets_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/sample_brackets` by default,
the layers below and `oracle.json`, which
`tests/conformance/sample_brackets.rs` replays against layerstack.

`root.usda` sublayers `weak.usda` and, with offset 3 and scale 0.5,
`shifted.usda`, and defines one prim per case:

- `Plain`: `a` and `b` with three samples each and `d` with a default
  only;
- `Offset`: samples behind a reference with offset 10 and scale 2;
- `Shifted.s`: samples in the sublayer with an offset;
- `Single.a`: one sample;
- `Blocked.a`, `AnimBlocked.a`: a block and an animation block over weaker
  samples;
- `Empty.a`: an empty `timeSamples` over weaker samples;
- `Spline.s`: a spline (its knots are not sample times);
- `Sparse.e`: a sparse array edit sample over weaker samples;
  `Mixed.e`: a dense sample, then a sparse one, over weaker samples;
- `SparseDefault.e`: a sparse array edit default over weaker samples;
  `SparseOverBlock.e`: a sparse default over a block;
- `Ball.radius`: a schema fallback and no opinion;
- `Clipped.size`: two value clips, the second activated at 10, mapped by
  `times`, whose activation and mapping times count as samples.

It records, for each attribute of `ATTRIBUTES`:

- `samples`: `UsdAttribute::GetTimeSamples`;
- `count`: `UsdAttribute::GetNumTimeSamples`;
- `in`: `UsdAttribute::GetTimeSamplesInInterval` for each of `INTERVALS`
  (`[min, max, min_closed, max_closed]`, `null` for an infinite end);
- `bracket`: `UsdAttribute::GetBracketingTimeSamples` at each of `TIMES`:
  `[lower, upper]`, `"none"` where the call succeeds without samples
  (`hasTimeSamples` false) and `false` where it fails;
- `nan`: the same call at a NaN time, `UsdTimeCode::Default()`, which
  reports a coding error (`"error"`, raised by the wrapper) and fails.

and `unions`: `UsdAttribute::GetUnionedTimeSamples` and
`GetUnionedTimeSamplesInInterval` over each of `UNIONS`. Every call is
repeated on a `UsdAttributeQuery` of the same attribute, and the script
fails unless the query answers the same, so one answer serves both.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixture.
"""
import argparse
import filecmp
import json
import math
import os
import tempfile

from pxr import Gf, Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "sample_brackets"))

LAYERS = {
    "root.usda": """#usda 1.0
(
    subLayers = [
        @./weak.usda@,
        @./shifted.usda@ (offset = 3; scale = 0.5)
    ]
)

def "Plain"
{
    double a.timeSamples = {
        1: 1,
        3: 3,
        5: 5,
    }
    double b.timeSamples = {
        2: 2,
        3: 3,
        7: 7,
    }
    double d = 1
}

def "Offset" (
    references = @./ref.usda@</Source> (offset = 10; scale = 2)
)
{
}

def "Single"
{
    double a.timeSamples = {
        4: 4,
    }
}

def "Blocked"
{
    double a = None
}

def "AnimBlocked"
{
    double a = AnimationBlock
}

def "Empty"
{
    double a.timeSamples = {
    }
}

def "Spline"
{
    double s.spline = {
        bezier,
        1: 1; post linear,
        5: 5; post linear,
    }
}

def "Sparse"
{
    int[] e.timeSamples = {
        4: edit [append 4],
    }
}

def "Mixed"
{
    int[] e.timeSamples = {
        2: [2],
        5: edit [append 5],
    }
}

def "SparseDefault"
{
    int[] e = edit [append 9]
}

def "SparseOverBlock"
{
    int[] e = edit [append 9]
}

def Sphere "Ball"
{
}

def "Clipped" (
    clips = {
        dictionary default = {
            double2[] active = [(0, 0), (10, 1)]
            asset[] assetPaths = [@./clipA.usda@, @./clipB.usda@]
            string primPath = "/Source"
            double2[] times = [(0, 0), (30, 30)]
        }
    }
)
{
    double size
}
""",
    "weak.usda": """#usda 1.0

over "Blocked"
{
    double a.timeSamples = {
        1: 1,
        2: 2,
    }
}

over "AnimBlocked"
{
    double a.timeSamples = {
        1: 1,
        2: 2,
    }
}

over "Empty"
{
    double a.timeSamples = {
        1: 1,
        2: 2,
    }
}

over "Sparse"
{
    int[] e.timeSamples = {
        1: [1],
        6: [6],
    }
}

over "Mixed"
{
    int[] e.timeSamples = {
        1: [1],
        3: [3],
        8: [8],
    }
}

over "SparseDefault"
{
    int[] e.timeSamples = {
        1: [1],
        3: [3],
    }
}

over "SparseOverBlock"
{
    int[] e = None
}
""",
    "shifted.usda": """#usda 1.0

def "Shifted"
{
    double s.timeSamples = {
        0: 0,
        4: 4,
        8: 8,
    }
}
""",
    "ref.usda": """#usda 1.0

def "Source"
{
    double a.timeSamples = {
        0: 0,
        5: 5,
        10: 10,
    }
}
""",
    "clipA.usda": """#usda 1.0

def "Source"
{
    double size.timeSamples = {
        0: 0,
        5: 5,
    }
}
""",
    "clipB.usda": """#usda 1.0

def "Source"
{
    double size.timeSamples = {
        12: 12,
        15: 15,
    }
}
""",
}

ATTRIBUTES = [
    "/Plain.a", "/Plain.b", "/Plain.d", "/Offset.a", "/Shifted.s",
    "/Single.a", "/Blocked.a", "/AnimBlocked.a", "/Empty.a", "/Spline.s", "/Sparse.e",
    "/Mixed.e", "/SparseDefault.e", "/SparseOverBlock.e", "/Ball.radius", "/Clipped.size",
]
# `[min, max, min_closed, max_closed]`; `None` is an infinite end.
INTERVALS = [
    [2, 5, True, True],
    [2, 5, False, False],
    [2, 5, True, False],
    [2, 5, False, True],
    [None, 3, False, True],
    [5, None, True, False],
    [None, None, False, False],
    [10, 30, True, True],
    [3, 3, True, True],
    [3, 3, False, False],
    [5, 2, True, True],
]
TIMES = [-1, 1, 2, 3.5, 4, 5, 6, 7, 11, 25, 100]
UNIONS = [
    [],
    ["/Plain.a", "/Plain.b"],
    ["/Plain.a", "/Offset.a"],
    ["/Plain.b", "/Clipped.size", "/Shifted.s"],
    ["/Plain.a", "/Plain.d", "/Blocked.a", "/Spline.s"],
    ["/Sparse.e", "/Mixed.e"],
]


def interval(bounds):
    lo, hi, lo_closed, hi_closed = bounds
    return Gf.Interval(
        -math.inf if lo is None else lo, math.inf if hi is None else hi, lo_closed, hi_closed)


def times(xs):
    return [float(x) for x in xs]


def bracket(result):
    # The wrapper returns `(lower, upper)`, `()` without samples and `None`
    # where the C++ call returns false.
    if result is None:
        return False
    return [float(result[0]), float(result[1])] if len(result) == 2 else "none"


def nan_bracket(attribute_or_query):
    # NaN is `UsdTimeCode::Default()`: the call reports a coding error,
    # which the wrapper raises, and returns false.
    try:
        return bracket(attribute_or_query.GetBracketingTimeSamples(math.nan))
    except Exception:
        return "error"


def same(attribute, query, label):
    if attribute != query:
        raise SystemExit(f"{label}: UsdAttributeQuery {query} != UsdAttribute {attribute}")
    return attribute


def record(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    attributes = []
    for path in ATTRIBUTES:
        attribute = stage.GetAttributeAtPath(path)
        query = Usd.AttributeQuery(attribute)
        entry = {
            "attribute": path,
            "samples": same(times(attribute.GetTimeSamples()), times(query.GetTimeSamples()),
                            f"{path} samples"),
            "count": same(attribute.GetNumTimeSamples(), query.GetNumTimeSamples(),
                          f"{path} count"),
            "in": [
                same(times(attribute.GetTimeSamplesInInterval(interval(bounds))),
                     times(query.GetTimeSamplesInInterval(interval(bounds))),
                     f"{path} in {bounds}")
                for bounds in INTERVALS
            ],
            "bracket": [
                same(bracket(attribute.GetBracketingTimeSamples(time)),
                     bracket(query.GetBracketingTimeSamples(time)),
                     f"{path} bracket {time}")
                for time in TIMES
            ],
        }
        entry["nan"] = same(nan_bracket(attribute), nan_bracket(query), f"{path} bracket NaN")
        attributes.append(entry)
    unions = []
    for paths in UNIONS:
        members = [stage.GetAttributeAtPath(path) for path in paths]
        queries = [Usd.AttributeQuery(member) for member in members]
        unions.append({
            "attributes": paths,
            "all": same(times(Usd.Attribute.GetUnionedTimeSamples(members)),
                        times(Usd.AttributeQuery.GetUnionedTimeSamples(queries)),
                        f"union {paths}"),
            "in": [
                same(times(Usd.Attribute.GetUnionedTimeSamplesInInterval(
                         members, interval(bounds))),
                     times(Usd.AttributeQuery.GetUnionedTimeSamplesInInterval(
                         queries, interval(bounds))),
                     f"union {paths} in {bounds}")
                for bounds in INTERVALS
            ],
        })
    return {"attributes": attributes, "unions": unions}


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
    oracle = {"openusd": f"{minor}.{patch}", "intervals": INTERVALS, "times": TIMES}
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
