// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{format, string::String, vec::Vec};

use super::*;
use crate::doc::InMemoryStore;
use crate::path::{PropertyPath, TargetPath};

fn text(input: &str) -> String {
    PathExpression::parse(input)
        .unwrap_or_else(|e| panic!("{input:?}: {e}"))
        .text()
}

/// Text round trips as `SdfPathExpression::GetText` writes it; each pair
/// was checked against OpenUSD 26.08.
#[test]
fn text_matches_openusd() {
    for (input, expected) in [
        ("/A{isa:Mesh}", "/A{isa:\"Mesh\"}"),
        ("//{isa:Mesh,Xform}", "//{isa:\"Mesh\",\"Xform\"}"),
        (
            "//{kind(component, strict=true)}",
            "//{kind(\"component\", strict=1)}",
        ),
        ("//{abstract:false}", "//{abstract:0}"),
        ("//{a:1.5}", "//{a:1.5}"),
        ("//{a:1e5}", "//{a:100000}"),
        ("//{a:1e15}", "//{a:1e15}"),
        ("//{a:1e-7}", "//{a:1e-7}"),
        ("//{a:-3}", "//{a:-3}"),
        ("//{a:inf}", "//{a:inf}"),
        ("//{a:-inf}", "//{a:-inf}"),
        ("//{a:\"x y\"}", "//{a:\"x y\"}"),
        ("//{a:'q\"'}", "//{a:'q\"'}"),
        ("//{a:}", "//{a:\"\"}"),
        ("//{a(1, b=2.0)}", "//{a(1, b=2)}"),
        ("//{a()}", "//{a(\"\")}"),
        ("//{a (x)}", "//{a(\"x\")}"),
        ("//{a(x,)}", "//{a(\"x\", \"\")}"),
        ("//{a(b=c,d=e)}", "//{a(b=\"c\", d=\"e\")}"),
        (
            "//{a:99999999999999999999}",
            "//{a:\"99999999999999999999\"}",
        ),
        ("//{a:x\\ty}", "//{a:\"x\\ty\"}"),
        ("//{a:-}", "//{a:\"-\"}"),
        ("//{a:infinity}", "//{a:\"infinity\"}"),
        ("//{a:true,false}", "//{a:1,0}"),
        ("//{a:x=y}", "//{a:\"x=y\"}"),
        ("//{a:a/b.c*}", "//{a:\"a/b.c*\"}"),
        ("//{a:\"line\\nnext\"}", "//{a:\"\"\"line\nnext\"\"\"}"),
        ("//{nota}", "//{nota}"),
        ("//{not a and b or c}", "//{not a and b or c}"),
        ("//{a b}", "//{a b}"),
        ("//{(a or b) c}", "//{(a or b) c}"),
        ("//{not not a}", "//{not not a}"),
        ("//{not(a)}", "//{not a}"),
        ("//{a and (b and c)}", "//{a and (b and c)}"),
        ("//{a or b and c}", "//{a or b and c}"),
        ("//{(a)}", "//{a}"),
        ("/A.b:c*", "/A.b:c*"),
        ("/A//*.x", "/A//*.x"),
        ("//", "//"),
        ("/", "/"),
        (".", "."),
        (".//", ".//"),
        ("..", ".."),
        ("../..//A", "../..//A"),
        ("A/B", "A/B"),
        ("A*/B", "A*/B"),
        ("A", "A"),
        ("A.b", "A.b"),
        ("..//", "..//"),
        ("../A", "../A"),
        (".//A", ".//A"),
        ("{a}", "{a}"),
        (".//{x}", ".//{x}"),
        ("..//{x}", "..//{x}"),
        ("/A[a-]", "/A[a-]"),
        ("/A[!a]", "/A[!a]"),
        ("/A[!]", "/A[!]"),
        ("%/A/B:c", "%/A/B:c"),
        ("%:c", "%:c"),
        ("%..:c", "%..:c"),
        ("%../X:c", "%../X:c"),
        ("%_", "%_"),
        ("/A//B//", "/A//B//"),
        ("/A//{a}/B", "/A//{a}/B"),
        ("/A?/B.[xy]*", "/A?/B.[xy]*"),
        ("/A{a}.b", "/A{a}.b"),
        ("/A.{a}", "/A.{a}"),
        ("/A//{a}", "/A//{a}"),
        ("//{a}.x", "//{a}.x"),
        ("/A/{a}", "/A/{a}"),
        ("/A/B{a}//", "/A/B{a}//"),
        ("/A/B*[0-9]/C{a}", "/A/B*[0-9]/C{a}"),
        ("/*.*", "/*.*"),
        ("/_A", "/_A"),
        ("/1A", "/1A"),
        ("~//", ""),
        ("~ /A", "~/A"),
        ("(/A)", "/A"),
        ("/A + ~(/B & /C) - /D", "/A + ~(/B & /C) - /D"),
        ("/A- /B", "/A - /B"),
        ("/A -/B", "/A - /B"),
        ("/A  +  /B", "/A + /B"),
        ("/A\t/B", "/A /B"),
        ("/A\n/B", "/A"),
        ("/A\r\n", "/A"),
        ("/A \n junk", "/A"),
        ("/A /B", "/A /B"),
        ("/A&/B", "/A & /B"),
        ("(/A /B) & /C", "/A /B & /C"),
        ("/A (/B & /C)", "/A (/B & /C)"),
        ("(/A - /B) - /C", "/A - /B - /C"),
        ("/A - (/B - /C)", "/A - (/B - /C)"),
        ("~(/A /B)", "~(/A /B)"),
        ("~(~/A)", "/A"),
        ("(/A + /B) & /C", "/A + /B & /C"),
        ("(/A - /B) & /C", "(/A - /B) & /C"),
        ("(/A + /B) /C", "(/A + /B) /C"),
        ("/A + (/B + /C)", "/A + (/B + /C)"),
        ("", ""),
    ] {
        assert_eq!(text(input), expected, "{input:?}");
    }
}

/// Text OpenUSD 26.08 rejects.
#[test]
fn ill_formed_text_is_rejected() {
    for input in [
        "/Café",
        "/A/Café*",
        "/A//.x",
        "/A/B/.x",
        "//{a:1.}",
        "//{a:12abc}",
        "//{a:1e}",
        "//{a(c=3,1)}",
        "%_x",
        "%foo:bar",
        "~~/A",
        "/A~/B",
        "/A.b.c",
        "/A/B.c/D",
        "/A//B/.c",
        ".A",
        "./A",
        "   ",
        "(/A",
        "/A{isa:Mesh",
        "/A[a-z",
        "/A -",
        "%Sub:foo",
    ] {
        assert!(PathExpression::parse(input).is_err(), "{input:?} parses");
    }
}

/// Combining expressions simplifies them as `SdfPathExpression::MakeOp`
/// does, and `%_` splices in the weaker expression.
#[test]
fn composition_simplifies_as_openusd_does() {
    let parse = |t: &str| PathExpression::parse(t).expect(t);
    let compose = |strong: &str, weak: &str| parse(strong).compose_over(&parse(weak)).text();
    assert_eq!(compose("/A %_", "/B /C"), "/A (/B /C)");
    assert_eq!(compose("/A - %_", "/B - /C"), "/A - (/B - /C)");
    assert_eq!(compose("~%_", ""), "//");
    assert_eq!(compose("%_ - %_", ""), "");
    assert_eq!(compose("// - %_", "/A"), "~/A");
    assert!(parse("%/A:b /C").contains_references());
    assert!(!parse("/A /B").contains_references());
    assert!(parse("%_ /C").contains_weaker_reference());
    assert!(!parse("A /B").is_absolute());
    assert!(parse("/A %:x").is_absolute() && !parse("/A %:x").is_complete());
    assert_eq!(
        parse("A ../B %../X:y")
            .make_absolute(&["P".into(), "Q".into()])
            .text(),
        "/P/Q/A /P/B %/P/X:y"
    );
}

/// A resolver over a table of named expressions on one prim.
struct Table(Vec<(&'static str, &'static str)>);

impl ReferenceResolver for Table {
    type Key = String;

    fn resolve(
        &mut self,
        _from: &String,
        reference: &ExpressionReference,
    ) -> Option<(String, PathExpression)> {
        let (name, text) = self.0.iter().find(|(name, _)| *name == reference.name())?;
        Some((String::from(*name), PathExpression::parse(text).ok()?))
    }
}

/// References resolve recursively; a cycle and a missing name resolve to
/// nothing, and the same expression may be reached twice along different
/// branches.
#[test]
fn references_resolve_with_cycles_as_nothing() {
    let mut table = Table(Vec::from([
        ("top", "/T %:mid %:mid2"),
        ("mid", "/M %:leaf %:top"),
        ("mid2", "%:leaf - /Z"),
        ("leaf", "/L %:gone %_"),
    ]));
    let top = PathExpression::parse("/T %:mid %:mid2").unwrap();
    let resolved = top.resolve_complete(String::from("top"), &mut table);
    assert_eq!(resolved.expression.text(), "/T (/M /L) (/L - /Z)");
    assert_eq!(resolved.referenced, ["mid", "leaf", "mid2"]);
    assert_eq!(
        resolved.problems,
        [
            ReferenceProblem::Missing {
                from: String::from("leaf"),
                reference: PathExpression::parse("%:gone").unwrap().references()[0].clone(),
            },
            ReferenceProblem::Cycle {
                from: String::from("mid"),
                reference: PathExpression::parse("%:top").unwrap().references()[0].clone(),
                target: String::from("top"),
            },
            ReferenceProblem::Missing {
                from: String::from("leaf"),
                reference: PathExpression::parse("%:gone").unwrap().references()[0].clone(),
            },
        ]
    );
}

/// Predicates for the matching tests: `even` holds for names of even
/// length, `named:x` for the name `x`, and `open` for paths with no element
/// named `Stop`, constantly so below one.
struct Toy<'s>(&'s InMemoryStore);

#[derive(Clone, Debug, PartialEq)]
enum ToyCall {
    Even,
    Open,
    Named(String),
}

impl Predicates for Toy<'_> {
    type Call = ToyCall;

    fn bind(&self, call: &PredicateCall) -> Result<ToyCall, PredicateBindError> {
        match (call.name.as_str(), call.positional().next()) {
            ("even", None) => Ok(ToyCall::Even),
            ("open", None) => Ok(ToyCall::Open),
            ("named", Some(ArgValue::String(name))) => Ok(ToyCall::Named(name.clone())),
            _ => Err(PredicateBindError::new(&call.name, "unknown")),
        }
    }

    fn evaluate(&self, call: &ToyCall, object: TargetPath) -> MatchResult {
        let tokens = &self.0.tokens;
        let (prim, property) = match object {
            TargetPath::Prim(prim) => (prim, None),
            TargetPath::Property(p) => (p.prim_path(), Some(tokens.resolve(p.property()))),
        };
        let segments = self.0.paths.resolve(prim).segments();
        let name = property
            .or_else(|| segments.last().map(|t| tokens.resolve(*t)))
            .unwrap_or("");
        match call {
            ToyCall::Even => MatchResult::varying(name.len() % 2 == 0),
            ToyCall::Open if segments.iter().any(|t| tokens.resolve(*t) == "Stop") => {
                MatchResult::constant(false)
            }
            ToyCall::Open => MatchResult::varying(true),
            ToyCall::Named(wanted) => MatchResult::varying(name == wanted),
        }
    }
}

fn paths(store: &mut InMemoryStore) -> Vec<TargetPath> {
    let mut out = Vec::new();
    for text in [
        "/World",
        "/World/Rocks",
        "/World/Rocks/Pebble",
        "/World/Rocks/Pebble/Chip",
        "/World/Rocks/Stop",
        "/World/Rocks/Stop/Deep",
        "/World/Sky",
        "/World/Sky/Cloud",
        "/Other",
        "/Other/Rocks",
    ] {
        let prim = store.path(text);
        out.push(TargetPath::Prim(prim));
        for name in ["size", "points:x"] {
            let name = store.tokens.intern(name);
            out.push(TargetPath::Property(PropertyPath::new(prim, name)));
        }
    }
    out
}

/// Matches computed by hand from `SdfPathExpressionEval::_Match`'s rules,
/// and the same results incrementally: whenever the incremental search
/// says a result is constant, every descendant has it.
#[test]
fn matching_and_searching_agree() {
    let mut store = InMemoryStore::default();
    let all = paths(&mut store);
    let display =
        |path: TargetPath, store: &InMemoryStore| path.display(&store.paths, &store.tokens);
    for (expression, expected) in [
        ("/World", &["/World"][..]),
        ("/World/*", &["/World/Rocks", "/World/Sky"][..]),
        (
            "/World//",
            &[
                "/World",
                "/World.size",
                "/World.points:x",
                "/World/Rocks",
                "/World/Rocks.size",
                "/World/Rocks.points:x",
                "/World/Rocks/Pebble",
                "/World/Rocks/Pebble.size",
                "/World/Rocks/Pebble.points:x",
                "/World/Rocks/Pebble/Chip",
                "/World/Rocks/Pebble/Chip.size",
                "/World/Rocks/Pebble/Chip.points:x",
                "/World/Rocks/Stop",
                "/World/Rocks/Stop.size",
                "/World/Rocks/Stop.points:x",
                "/World/Rocks/Stop/Deep",
                "/World/Rocks/Stop/Deep.size",
                "/World/Rocks/Stop/Deep.points:x",
                "/World/Sky",
                "/World/Sky.size",
                "/World/Sky.points:x",
                "/World/Sky/Cloud",
                "/World/Sky/Cloud.size",
                "/World/Sky/Cloud.points:x",
            ][..],
        ),
        ("//Rocks", &["/World/Rocks", "/Other/Rocks"][..]),
        ("//Rocks//Ch?p", &["/World/Rocks/Pebble/Chip"][..]),
        (
            "/World//*.size - //Rocks//",
            &["/World/Sky.size", "/World/Sky/Cloud.size"][..],
        ),
        (
            "//*.points:*",
            &[
                "/World.points:x",
                "/World/Rocks.points:x",
                "/World/Rocks/Pebble.points:x",
                "/World/Rocks/Pebble/Chip.points:x",
                "/World/Rocks/Stop.points:x",
                "/World/Rocks/Stop/Deep.points:x",
                "/World/Sky.points:x",
                "/World/Sky/Cloud.points:x",
                "/Other.points:x",
                "/Other/Rocks.points:x",
            ][..],
        ),
        (
            "/World/Rocks/*{even}",
            &["/World/Rocks/Pebble", "/World/Rocks/Stop"][..],
        ),
        (
            "/World//{open}",
            &[
                "/World",
                "/World.size",
                "/World.points:x",
                "/World/Rocks",
                "/World/Rocks.size",
                "/World/Rocks.points:x",
                "/World/Rocks/Pebble",
                "/World/Rocks/Pebble.size",
                "/World/Rocks/Pebble.points:x",
                "/World/Rocks/Pebble/Chip",
                "/World/Rocks/Pebble/Chip.size",
                "/World/Rocks/Pebble/Chip.points:x",
                "/World/Sky",
                "/World/Sky.size",
                "/World/Sky.points:x",
                "/World/Sky/Cloud",
                "/World/Sky/Cloud.size",
                "/World/Sky/Cloud.points:x",
            ][..],
        ),
        (
            "/World/Rocks/*{named:Stop}//",
            &[
                "/World/Rocks/Stop",
                "/World/Rocks/Stop.size",
                "/World/Rocks/Stop.points:x",
                "/World/Rocks/Stop/Deep",
                "/World/Rocks/Stop/Deep.size",
                "/World/Rocks/Stop/Deep.points:x",
            ][..],
        ),
        (
            "~//Rocks// & /Other//",
            &["/Other", "/Other.size", "/Other.points:x"][..],
        ),
        (
            "/World/Rocks.size /Other.points:x",
            &["/World/Rocks.size", "/Other.points:x"][..],
        ),
        ("", &[][..]),
    ] {
        let parsed = PathExpression::parse(expression).expect(expression);
        let toy = Toy(&store);
        let matcher = parsed.matcher(&toy).expect(expression);
        let mut searcher = matcher.searcher();
        let mut got = Vec::new();
        let mut constants: Vec<(TargetPath, bool)> = Vec::new();
        for path in &all {
            let result = matcher.matches(&toy, &store, *path);
            let incremental = searcher.next(&toy, &store, *path);
            assert_eq!(
                result.value,
                incremental.value,
                "{expression}: {} matched and searched differently",
                display(*path, &store)
            );
            if result.value {
                got.push(display(*path, &store));
            }
            for (ancestor, value) in &constants {
                if is_descendant(&store, *path, *ancestor) {
                    assert_eq!(
                        result.value,
                        *value,
                        "{expression}: {} breaks a constant",
                        display(*path, &store)
                    );
                }
            }
            if result.is_constant() {
                constants.push((*path, result.value));
            }
        }
        let mut expected: Vec<String> = expected.iter().map(|s| String::from(*s)).collect();
        expected.sort();
        got.sort();
        assert_eq!(got, expected, "{expression}");
    }
}

fn is_descendant(store: &InMemoryStore, path: TargetPath, ancestor: TargetPath) -> bool {
    let TargetPath::Prim(ancestor) = ancestor else {
        return false;
    };
    let prim = match path {
        TargetPath::Prim(prim) if prim == ancestor => return false,
        TargetPath::Prim(prim) => prim,
        TargetPath::Property(p) => p.prim_path(),
    };
    store
        .paths
        .resolve(ancestor)
        .is_prefix_of(store.paths.resolve(prim))
}

/// Linking fails as `SdfMakePathExpressionEval` does: on references,
/// relative prefixes, unbound predicates and malformed globs.
#[test]
fn linking_reports_why_it_fails() {
    let store = InMemoryStore::default();
    let toy = Toy(&store);
    let link = |t: &str| PathExpression::parse(t).unwrap().matcher(&toy).err();
    assert!(matches!(
        link("/A %:b"),
        Some(MatcherError::Incomplete { .. })
    ));
    assert!(matches!(link("A"), Some(MatcherError::Incomplete { .. })));
    assert!(matches!(
        link("//{odd}"),
        Some(MatcherError::Predicate { .. })
    ));
    assert!(matches!(link("/A[!]"), Some(MatcherError::Glob { .. })));
    assert_eq!(link("//{even} /B"), None);
    assert!(PathExpression::nothing().matcher(&toy).unwrap().is_empty());

    // A final segment that starts with a bare predicate, matched against
    // the pattern's prefix itself, is too short to match; OpenUSD 26.08
    // reads before its path's first element there (undefined behavior).
    let mut store = InMemoryStore::default();
    let world = store.path("/World");
    let toy = Toy(&store);
    let matcher = PathExpression::parse("/World//{even}/*")
        .unwrap()
        .matcher(&toy)
        .unwrap();
    assert_eq!(
        matcher.matches(&toy, &store, TargetPath::Prim(world)),
        MatchResult::varying(false)
    );
    let _ = format!("{}", link("//{odd}").unwrap());
}
