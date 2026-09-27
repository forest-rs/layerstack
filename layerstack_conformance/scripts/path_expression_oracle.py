# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD parses path expressions and matches collections'.

Usage: path_expression_oracle.py [FIXTURES [OUT]]
       path_expression_oracle.py --check [FIXTURES]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.

Opens `fixtures/path_expression/scene.usda` (with its sublayer `weak.usda`
and a reference to `asset.usda`) and writes `oracle.json` beside it:

- `texts`: for each expression text in TEXTS, `SdfPathExpression`'s
  `GetText`, or `null` when it does not parse;
- `paths`: the paths matched, the pseudo-root, every prim
  (`TraverseAll`) and every property (`GetPropertyNames`), except the
  properties of `/Expressions`, which only holds collections;
- `collections`: for every `CollectionAPI` instance, the composed
  `membershipExpression` text, `ResolveCompleteMembershipExpression`'s
  text, which of `paths` `IsPathIncluded` includes (as indices), and which
  `ComputeIncludedPathsFromCollection` computes (default predicate),
  among `paths`; or `unlinkable` when the expression does not link (an
  unknown predicate, arguments it does not bind, a malformed glob), where
  OpenUSD's evaluator matches nothing but Python raises.

`tests/path_expression.rs` compares everything.
"""
import json
import os
import sys

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "path_expression"))

TEXTS = [
    "/A /B", "/A    /B", "/A+/B", "/A&/B", "/A-/B", "~ /A", "(/A /B) & /C",
    "/A (/B & /C)", "((/A))", "(/A - /B) - /C", "/A - (/B - /C)", "~(/A /B)",
    "~(~/A)", "(/A + /B) & /C", "(/A - /B) & /C", "/A - (/B + /C)",
    "(/A + /B) /C", "/A & (/B /C)", "/A + (/B + /C)", "%_ /A", "%:heroes /B",
    "%/Sets:heroes", ".//", "../B", "/World//*{isa:\"Mesh\"}", "/A/B.x:y*", "",
    "   ", "/A\t/B", "/A\n/B", "/A\r\n", "/A \n junk",
    "/A{isa:Mesh}", "//{isa:Mesh,Xform}", "//{kind(component, strict=true)}",
    "//{abstract:false}", "//{a:1.5}", "//{a:1e5}", "//{a:1e14}", "//{a:1e15}",
    "//{a:1.25e20}", "//{a:1e-7}", "//{a:0.000001}", "//{a:-3}", "//{a:inf}",
    "//{a:-inf}", "//{a:\"x y\"}", "//{a:'q\"'}", "//{a:\"x\\\"y'z\"}", "//{a:}",
    "//{a(1, b=2.0)}", "//{a()}", "//{a( )}", "//{a (x)}", "//{a(x,)}",
    "//{a(b=c,d=e)}", "//{a(1,2,c=3)}", "//{a(c=3,1)}",
    "//{a:99999999999999999999}", "//{a:-99999999999999999999}", "//{a:x\\ty}",
    "//{a:-}", "//{a:infinity}", "//{a:true,false}", "//{a:x=y}",
    "//{a:\"tab\\there\"}", "//{a:\"line\\nnext\"}", "//{a:a/b.c*}", "//{a:1.}",
    "//{a:12abc}", "//{a:1e}", "//{nota}", "//{not a and b or c}", "//{a b}",
    "//{(a or b) c}", "//{not not a}", "//{not  not a}", "//{not(a)}",
    "//{a and (b and c)}", "//{a or b and c}", "//{a b and c d or e}", "//{(a)}",
    "/A.b:c*", "/A//.x", "/A//*.x", "//", "/", ".", "..", "../..//A", "A/B",
    "A*/B", "/A/B/.x", "/A[a-]", "/A[!a]", "/A[!]", "%/A/B:c", "%:c", "%..:c",
    "%../X:c", "%_", "%_x", "%foo:bar", "/A//B//", "//{a}", "/A//{a}/B",
    "/A?/B.[xy]*", "/A{a}.b", "/A.{a}", "~//", "(/A)", "/A + ~(/B & /C) - /D",
    "~~/A", "/A~/B", "/A- /B", "/A -/B", "/A  +  /B", "/A/*/B",
    "/A/B*[0-9]/C{a}", "/A//B{x}//C", "/*.*", "/A.*{b}", "//*.x:y", "/A.x{p}",
    "/A{p}/B", "/_A", "/1A", "/A1*", "/A[a]", ".//{x}", "..//{x}", "/A//{x}//",
    "/A.b.c", "/A/B.c/D", "/A//B/.c", ".//A", ".A", "./A", "A", "A.b", "..//",
    "../A", "/A//{a}", "//{a}.x", "/A/{a}", "/A/B{a}//", "{a}", "(/A", "/A -",
    "/A{isa:Mesh", "/A[a-z", "/A[a-z]", "%Sub:foo", "/X\u00e9", "/A/X\u00e9*",
]


def text_of(text):
    try:
        return Sdf.PathExpression(text).GetText()
    except Exception:
        return None


def record(scene):
    stage = Usd.Stage.Open(scene)
    prims = list(stage.TraverseAll())
    paths = ["/"]
    for prim in prims:
        paths.append(str(prim.GetPath()))
        if str(prim.GetPath()) == "/Expressions":
            continue
        paths.extend(str(prim.GetPath().AppendProperty(n)) for n in prim.GetPropertyNames())
    index = {path: i for i, path in enumerate(paths)}
    collections = {}
    for prim in prims:
        for collection in Usd.CollectionAPI.GetAllCollections(prim):
            authored = collection.GetMembershipExpressionAttr().Get()
            record = {
                "expression": authored.GetText() if authored is not None else None,
                "resolved": collection.ResolveCompleteMembershipExpression().GetText(),
            }
            try:
                query = collection.ComputeMembershipQuery()
            except Exception:
                # The expression does not link: OpenUSD's evaluator is
                # empty and matches nothing, but Python raises the error.
                record["unlinkable"] = True
            else:
                computed = Usd.ComputeIncludedPathsFromCollection(query, stage)
                record["included"] = [
                    i for i, p in enumerate(paths) if query.IsPathIncluded(Sdf.Path(p))
                ]
                record["computed"] = sorted(index[str(p)] for p in computed if str(p) in index)
            collections[str(collection.GetCollectionPath())] = record
    return paths, collections


def main():
    args = sys.argv[1:]
    check = bool(args) and args[0] == "--check"
    if check:
        args = args[1:]
    fixtures = args[0] if args else FIXTURES
    out_path = args[1] if len(args) > 1 else os.path.join(fixtures, "oracle.json")
    paths, collections = record(os.path.join(fixtures, "scene.usda"))
    _, minor, patch = Usd.GetVersion()
    result = {
        "openusd_version": f"{minor}.{patch}",
        "texts": {text: text_of(text) for text in TEXTS},
        "paths": paths,
        "collections": collections,
    }
    if check:
        with open(os.path.join(fixtures, "oracle.json")) as f:
            if json.load(f) != json.loads(json.dumps(result)):
                sys.exit("oracle.json is not what OpenUSD computes; rerun path_expression_oracle.py")
        return
    with open(out_path, "w") as f:
        json.dump(result, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
