# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records which variant selections OpenUSD's `variant` predicate matches.

Usage: selection_glob_oracle.py [FIXTURES [OUT]]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Opens `fixtures/path_expression/selections.usda` (a prim per selection of
the `color` set, and one selecting nothing) and, for each selection glob
in GLOBS and GENERATED (random, from a fixed seed), matches
`//{variant(color="<glob>")}`, writing `selection_globs.json` beside it:
the prims whose selection matches, or `null` when the predicate does not
bind (`ArchRegex` rejects the glob; OpenUSD's Python raises, and its
evaluator matches nothing).

`ArchRegex` searches with the platform's POSIX `regcomp`, and what that
accepts beyond POSIX differs between platforms. These results are
macOS's; CI, on Linux, does not rerun this oracle. `tests/path_expression.rs`
requires every glob to behave as recorded, except those
`selection_glob_divergences.json` lists: syntax POSIX leaves undefined
(`undefined`) and POSIX features layerstack does not implement
(`unsupported`), which layerstack rejects.
"""
import json
import os
import random
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "path_expression"))

GLOBS = [
    "b*", "l", "l|z", "^b.*e$", "^b*e$", "1", "u?", "^b(l|r)[u]e$", "^(bl|gl)[a-z]e$",
    "[a-c]", "[^b]", "^[[:alpha:]]+$", "^[[:digit:]]+$", "^[[:alnum:]_]+$", "l{1}",
    "^(b|l){2}", "^b{2}", "*", "(a|aa)+b", "[]a]", "[^]a]", "a{2,}", "^a{2,3}b$",
    "a{255}", "a{256}", "a{0,255}", "(*){1000000}", "(a{255}){255}", "a|", "|a",
    "(|a)", "()", "()*", "+a", "a**", "a*+", "a{2}{3}", "{", "a{", "a{x}", "}", "a}",
    "a{2,1}", "^*", "$*", "\\a", "\\.", "\\q", "\\|", "[a-]", "[-a]", "[[:bogus:]]",
    "[[=a=]]", "((((((((((a))))))))))", "a.b", "a\\{2\\}", "a{2}", "x|y|", "e$", "^r",
    "r^", "$b", "(a)\\1", "[z-a]", "^(a+)+$", "(a*)*b", "^_", "-", "^-", "[|]", "a\\|b",
    "big-?1", "^[0-9]$", "[[:upper:]]", "^.$", "^..?$", "^(ab|c)*$", "(abc){2}",
    # Atoms repeated zero times, and what is next to them.
    "a{0}", "(a{0})", "a{0}|b", "a{0,0}", "(a|b){0}", "x(a{0})y", "a{0}*", "^a{0}$", "a{0}^", "$a{0}", "^(a{0})$", "(a{0})*", "r(e{0})d", "b{0}lue", "a{0}{2}", "(a{0}|)",
]

ALPHABET = "ab1-|.*?()[]^$+{},\\:r2"


def generated(count=400, seed=69):
    rng = random.Random(seed)
    out = []
    while len(out) < count:
        text = "".join(rng.choice(ALPHABET) for _ in range(rng.randint(1, 7)))
        if text not in out and text not in GLOBS:
            out.append(text)
    return out


def record(scene, globs):
    stage = Usd.Stage.Open(scene)
    prims = [p for p in stage.Traverse()]
    holder = stage.GetSessionLayer()
    stage.SetEditTarget(holder)
    host = stage.OverridePrim("/Globs")
    collection = Usd.CollectionAPI.Apply(host, "glob")
    attr = collection.CreateMembershipExpressionAttr()
    results = {}
    for glob in globs:
        quoted = glob.replace("\\", "\\\\").replace('"', '\\"')
        attr.Set(Sdf.PathExpression('//{variant(color="%s")}' % quoted))
        try:
            query = collection.ComputeMembershipQuery()
        except Exception:
            results[glob] = None
            continue
        results[glob] = [
            str(p.GetPath()) for p in prims
            if str(p.GetPath()) != "/Globs" and query.IsPathIncluded(p.GetPath())
        ]
    return results


def main():
    args = sys.argv[1:]
    fixtures = args[0] if args else FIXTURES
    out_path = args[1] if len(args) > 1 else os.path.join(fixtures, "selection_globs.json")
    globs = GLOBS + generated()
    results = record(os.path.join(fixtures, "selections.usda"), globs)
    _, minor, patch = Usd.GetVersion()
    with open(out_path, "w") as f:
        json.dump(
            {"openusd_version": f"{minor}.{patch}", "globs": results},
            f,
            indent=1,
            sort_keys=True,
        )
        f.write("\n")


if __name__ == "__main__":
    main()
