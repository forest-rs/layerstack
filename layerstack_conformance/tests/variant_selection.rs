// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Variant selection regressions driven through the USDA pipeline.
//!
//! USDA ingestion stores prims introduced inside a variant branch in the
//! layer's namespace-keyed prim table. These tests pin that composition
//! only admits opinions from the selected branch (AOUSD Core §10.5), for
//! local variants and for variants reached through references.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, PropertyPath, Stage,
    StageOptions, TokenInterner, Value,
};
use layerstack::{Layer, ResolvedAsset};
use layerstack_usda::{emit, lower, parser::parse_cst};

/// Resolves asset paths against an in-memory set of USDA sources.
struct MemoryResolver {
    sources: BTreeMap<&'static str, &'static str>,
    by_name: BTreeMap<String, LayerId>,
    next_layer_id: u64,
    pending: Vec<Layer>,
}

impl AssetResolver for MemoryResolver {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let name = asset_path.trim_start_matches("./");
        if let Some(id) = self.by_name.get(name) {
            return Ok(ResolvedAsset {
                layer_id: *id,
                resolved_path: Arc::from(name),
                layer: None,
            });
        }
        let source = *self.sources.get(name).ok_or(AssetResolveError::NotFound)?;
        let layer_id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        self.by_name.insert(name.to_string(), layer_id);
        let layer = emit_layer(source, layer_id, tokens, paths, self);
        Ok(ResolvedAsset {
            layer_id,
            resolved_path: Arc::from(name),
            layer: Some(layer),
        })
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

fn emit_layer(
    source: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut MemoryResolver,
) -> Layer {
    let cst = parse_cst(source);
    assert!(cst.diagnostics.is_empty(), "{:?}", cst.diagnostics);
    let ast = lower::lower(&cst.tree, source);
    assert!(ast.diagnostics.is_empty(), "{:?}", ast.diagnostics);
    let result = emit::emit(&ast.layer, layer_id, tokens, paths, resolver);
    resolver.pending.extend(result.resolved_layers);
    result.layer
}

/// Loads `root` (and any layers it references from `others`) into a store.
fn load(root: &'static str, others: &[(&'static str, &'static str)]) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let mut resolver = MemoryResolver {
        sources: others.iter().copied().collect(),
        by_name: BTreeMap::new(),
        next_layer_id: 2,
        pending: Vec::new(),
    };
    let layer = emit_layer(
        root,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut resolver,
    );
    store.insert_layer(layer);
    for layer in resolver.pending.drain(..) {
        store.insert_layer(layer);
    }
    store
}

/// Resolves `prop` and returns its value plus the distinct values of every
/// opinion in its stack, strongest first.
fn resolve(store: &mut InMemoryStore, stage: &Stage, prop: &str) -> (Value, Vec<Value>) {
    let path = PropertyPath::parse(prop, &mut store.tokens, &mut store.paths).expect("path");
    let value = stage
        .resolve_field_path(path)
        .unwrap_or_else(|| panic!("{prop} resolves"))
        .value;
    let mut stack = Vec::new();
    for op in stage.explain_property_path(path).expect("opinions") {
        if let Some(v) = op.value.default_value()
            && !stack.contains(v)
        {
            stack.push(v.clone());
        }
    }
    (value, stack)
}

const MODEL: &str = r#"#usda 1.0
def "Model" (
    variantSets = ["y"]
    variants = {
        string y = "b"
    }
)
{
    variantSet "y" = {
        "a" {
            def "geom"
            {
                double x = 10
            }
        }
        "b" {
            def "geom"
            {
                double x = 20
            }
        }
    }
}
"#;

#[test]
fn unselected_local_variant_child_does_not_contribute() {
    let mut store = load(MODEL, &[]);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let (value, stack) = resolve(&mut store, &stage, "/Model/geom.x");
    assert_eq!(value, Value::Double(20.0), "selected branch `b` wins");
    assert_eq!(
        stack,
        vec![Value::Double(20.0)],
        "branch `a` must not appear in the opinion stack"
    );
}

/// A child introduced inside a selected variant holds variant opinions, so a
/// plain local opinion from a weaker sublayer still beats it (LIVERPS: local
/// opinions from the whole layer stack precede variants).
#[test]
fn variant_child_is_weaker_than_sublayer_local_opinion() {
    let root = r#"#usda 1.0
(
    subLayers = [@./sub.usda@]
)
def "Model" (
    variantSets = ["y"]
    variants = {
        string y = "b"
    }
)
{
    variantSet "y" = {
        "b" {
            def "geom"
            {
                double x = 20
            }
        }
    }
}
"#;
    let sub = r#"#usda 1.0
over "Model"
{
    over "geom"
    {
        double x = 99
    }
}
"#;
    let mut store = load(root, &[("sub.usda", sub)]);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let (value, stack) = resolve(&mut store, &stage, "/Model/geom.x");
    assert_eq!(value, Value::Double(99.0), "local sublayer opinion wins");
    assert_eq!(stack, vec![Value::Double(99.0), Value::Double(20.0)]);
}

#[test]
fn unselected_referenced_variant_child_does_not_contribute() {
    let root = r#"#usda 1.0
def "UsesDefault" (
    references = @./model.usda@</Model>
)
{
}

def "SelectsA" (
    references = @./model.usda@</Model>
    variants = {
        string y = "a"
    }
)
{
}
"#;
    let mut store = load(root, &[("model.usda", MODEL)]);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    let (value, stack) = resolve(&mut store, &stage, "/UsesDefault/geom.x");
    assert_eq!(value, Value::Double(20.0), "referenced selection `b` wins");
    assert_eq!(stack, vec![Value::Double(20.0)], "branch `a` leaked");

    let (value, stack) = resolve(&mut store, &stage, "/SelectsA/geom.x");
    assert_eq!(value, Value::Double(10.0), "referencing selection `a` wins");
    assert_eq!(stack, vec![Value::Double(10.0)], "branch `b` leaked");
}

const ARC_LIB: &str = r#"#usda 1.0
def "TargetA"
{
    double x = 1
    def "OnlyA"
    {
    }
}

def "TargetB"
{
    double x = 2
    def "OnlyB"
    {
    }
}
"#;

/// Builds a prim `name` whose variant set `v` selects `b`, where branches
/// `a` and `b` each author `arc` (on the branch itself and on a child `Kid`)
/// to their own target.
fn arc_model(name: &str, arc: &str, target_a: &str, target_b: &str) -> String {
    format!(
        r#"def "{name}" (
    variantSets = ["v"]
    variants = {{
        string v = "b"
    }}
)
{{
    variantSet "v" = {{
        "a" (
            {arc} = {target_a}
        ) {{
            def "Kid" (
                {arc} = {target_a}
            )
            {{
            }}
        }}
        "b" (
            {arc} = {target_b}
        ) {{
            def "Kid" (
                {arc} = {target_b}
            )
            {{
            }}
        }}
    }}
}}
"#
    )
}

/// Arcs authored inside an unselected variant branch must not be followed:
/// neither their target's children, their opinions, nor their dependency
/// edges may reach the composed stage.
///
/// Spec: AOUSD Core §10.5; OpenUSD only adds arcs of the selected variant
/// node (`pxr/usd/pcp/primIndex.cpp`, `_AddVariantArc`).
#[test]
fn arcs_in_unselected_variant_branch_are_not_followed() {
    let mut root = String::from(
        r#"#usda 1.0
class "ClassA"
{
    double x = 1
    def "OnlyA"
    {
    }
}

class "ClassB"
{
    double x = 2
    def "OnlyB"
    {
    }
}
"#,
    );
    let models = [
        (
            "RefModel",
            "references",
            "@./lib.usda@</TargetA>",
            "@./lib.usda@</TargetB>",
        ),
        (
            "PayModel",
            "payload",
            "@./lib.usda@</TargetA>",
            "@./lib.usda@</TargetB>",
        ),
        ("InheritModel", "inherits", "</ClassA>", "</ClassB>"),
        ("SpecModel", "specializes", "</ClassA>", "</ClassB>"),
    ];
    for (name, arc, a, b) in models {
        root.push_str(&arc_model(name, arc, a, b));
    }
    let root: &'static str = Box::leak(root.into_boxed_str());

    let mut store = load(root, &[("lib.usda", ARC_LIB)]);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_dependencies: true,
            ..StageOptions::default()
        },
    );
    let path = |store: &mut InMemoryStore, s: &str| {
        let p = layerstack::Path::parse_absolute(s, &mut store.tokens).expect("path");
        store.paths.intern(p)
    };
    let name_of =
        |store: &InMemoryStore, id: layerstack::PathId| store.paths.display(id, &store.tokens);
    let target_a = path(&mut store, "/TargetA");
    let class_a = path(&mut store, "/ClassA");

    let mut failures = Vec::new();
    for (name, arc, _, _) in models {
        for prim in [format!("/{name}"), format!("/{name}/Kid")] {
            let id = path(&mut store, &prim);
            if !stage.has_prim(id) {
                failures.push(format!("{prim} ({arc}): missing"));
                continue;
            }
            let prop =
                PropertyPath::parse(&format!("{prim}.x"), &mut store.tokens, &mut store.paths)
                    .expect("path");
            let stack: Vec<_> = stage
                .explain_property_path(prop)
                .unwrap_or(&[])
                .iter()
                .filter_map(|op| op.value.default_value().cloned())
                .collect();
            let expected = vec![Value::Double(2.0)];
            if stack != expected {
                failures.push(format!("{prim} ({arc}): x stack {stack:?}"));
            }

            let kids: Vec<String> = stage
                .children_of(id)
                .unwrap_or(&[])
                .iter()
                .map(|c| name_of(&store, *c))
                .collect();
            if kids.iter().any(|k| k.ends_with("/OnlyA"))
                || !kids.iter().any(|k| k.ends_with("/OnlyB"))
            {
                failures.push(format!("{prim} ({arc}): children {kids:?}"));
            }

            for dep in stage.arcs_targeting(id) {
                if dep.source == target_a || dep.source == class_a {
                    failures.push(format!(
                        "{prim} ({arc}): dependency on unselected target {}",
                        name_of(&store, dep.source)
                    ));
                }
            }
        }
    }
    for prim in stage.prims_affected_by_layer(LayerId(2)) {
        let name = name_of(&store, prim);
        if name.contains("OnlyA") {
            failures.push(format!("library layer feeds {name}"));
        }
    }
    assert!(failures.is_empty(), "leaks:\n{}", failures.join("\n"));
}

const NESTED_LOD: &str = r#"#usda 1.0
def "A" (
    variantSets = ["lod"]
    variants = {
        string lod = "high"
    }
)
{
    def "B" (
        variantSets = ["lod"]
    )
    {
        variantSet "lod" = {
            "high" {
                double x = 10
                def "HighOnly"
                {
                }
            }
            "low" {
                double x = 20
                def "LowOnly"
                {
                }
            }
        }
    }

    def "C" (
        variantSets = ["lod"]
        variants = {
            string lod = "low"
        }
    )
    {
        variantSet "lod" = {
            "high" {
                double x = 100
            }
            "low" {
                double x = 200
            }
        }
    }

    variantSet "lod" = {
        "high" {
            double x = 1
            over "B"
            {
                double y = 5
            }
            def "D" (
                variantSets = ["lod"]
            )
            {
                variantSet "lod" = {
                    "high" {
                        double x = 1000
                        def "HighOnly"
                        {
                        }
                    }
                    "low" {
                        double x = 2000
                        def "LowOnly"
                        {
                        }
                    }
                }
            }
        }
        "low" {
            double x = 2
        }
    }
}
"#;

/// Variant selections belong to the prim whose variant set they select.
/// `/A` selects `lod = "high"` for its own `lod` set; that must not select a
/// branch of `/A/B`'s unrelated, same-named `lod` set (which has no
/// selection, so none of its branches contribute), nor override `/A/C`'s
/// own selection.
///
/// Spec: AOUSD Core §10.5; OpenUSD resolves a set's selection at the site
/// hosting the set (`pxr/usd/pcp/primIndex.cpp`, `_ComposeVariantSelection`).
#[test]
fn same_named_variant_sets_on_different_hosts_do_not_alias() {
    let check = |store: &mut InMemoryStore, stage: &Stage, prefix: &str| {
        let (value, _) = resolve(store, stage, &format!("{prefix}.x"));
        assert_eq!(value, Value::Double(1.0), "{prefix} uses its own selection");

        let (value, _) = resolve(store, stage, &format!("{prefix}/C.x"));
        assert_eq!(
            value,
            Value::Double(200.0),
            "{prefix}/C uses its own selection"
        );

        let (value, _) = resolve(store, stage, &format!("{prefix}/B.y"));
        assert_eq!(value, Value::Double(5.0), "{prefix}'s branch overrides B");

        // `B` is a plain child that `/A`'s selected branch also overrides;
        // `D` is introduced by that branch.
        for child in ["B", "D"] {
            let prim = format!("{prefix}/{child}");
            let x = PropertyPath::parse(&format!("{prim}.x"), &mut store.tokens, &mut store.paths)
                .expect("path");
            assert!(stage.has_prim(x.prim_path()), "{prim} exists");
            assert_eq!(
                stage.explain_property_path(x).map(<[_]>::len),
                None,
                "{prim} has no `lod` selection, so no branch may contribute"
            );
            let kids: Vec<String> = stage
                .children_of(x.prim_path())
                .unwrap_or(&[])
                .iter()
                .map(|c| store.paths.display(*c, &store.tokens))
                .collect();
            assert!(kids.is_empty(), "{prim} children {kids:?}");
        }
    };

    let mut store = load(NESTED_LOD, &[]);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    check(&mut store, &stage, "/A");

    let root = r#"#usda 1.0
def "R" (
    references = @./model.usda@</A>
)
{
}
"#;
    let mut store = load(root, &[("model.usda", NESTED_LOD)]);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    check(&mut store, &stage, "/R");
}

/// Builds a library `/Model` whose `v` set defaults to the empty branch `b`,
/// while branch `a` introduces `C` (a direct branch child) and a descendant
/// `G` `extra_levels` below it. Both `C` and `G` author an arc of kind `arc`
/// to a prim with `x = 10` and a child `TChild`; `G` also authors `y = 5`.
fn deep_arc_model(arc: &str, target: &str, extra_levels: usize) -> String {
    let mut g = format!("def \"G\" (\n    {arc} = {target}\n)\n{{\n    double y = 5\n}}\n");
    for level in 0..extra_levels {
        g = format!("def \"D{level}\"\n{{\n{g}}}\n");
    }
    format!(
        r#"#usda 1.0
def "Model" (
    variantSets = ["v"]
    variants = {{
        string v = "b"
    }}
)
{{
    class "Cls"
    {{
        double x = 10
        def "TChild"
        {{
        }}
    }}

    variantSet "v" = {{
        "a" {{
            def "C" (
                {arc} = {target}
            )
            {{
{g}            }}
        }}
        "b" {{
        }}
    }}
}}
"#
    )
}

/// A selection authored at a stronger site than the library (the referencing
/// prim, or a layer referenced in between) must decide which branch's
/// descendants, arcs and opinions participate, however deep below the branch
/// they sit. The library's own default (`b`) must not veto them.
///
/// Spec: AOUSD Core §10.5; OpenUSD resolves the selection in the prim index
/// composed so far, strongest first (`pxr/usd/pcp/primIndex.cpp`,
/// `_ComposeVariantSelection`).
#[test]
fn stronger_selection_admits_deep_arcs_and_opinions() {
    let root = r#"#usda 1.0
def "Direct" (
    references = @./model.usda@</Model>
    variants = {
        string v = "a"
    }
)
{
}

def "Nested" (
    references = @./mid.usda@</M>
)
{
}

def "Default" (
    references = @./model.usda@</Model>
)
{
}
"#;
    let mid = r#"#usda 1.0
def "M" (
    references = @./model.usda@</Model>
    variants = {
        string v = "a"
    }
)
{
}
"#;
    let lib = r#"#usda 1.0
def "Target"
{
    double x = 10
    def "TChild"
    {
    }
}
"#;
    let arcs = [
        ("references", "@./lib.usda@</Target>"),
        ("payload", "@./lib.usda@</Target>"),
        ("inherits", "</Model/Cls>"),
        ("specializes", "</Model/Cls>"),
    ];
    let mut failures = Vec::new();
    for (arc, target) in arcs {
        for extra_levels in [0, 1] {
            let model: &'static str =
                Box::leak(deep_arc_model(arc, target, extra_levels).into_boxed_str());
            let mut store = load(
                root,
                &[("model.usda", model), ("mid.usda", mid), ("lib.usda", lib)],
            );
            let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
            let below: String = (0..extra_levels).map(|l| format!("/D{l}")).collect();

            for prim in ["Direct", "Nested"] {
                let c = format!("/{prim}/C");
                let g = format!("{c}{below}/G");
                for (prop, expected) in [
                    (format!("{c}.x"), 10.0),
                    (format!("{g}.x"), 10.0),
                    (format!("{g}.y"), 5.0),
                ] {
                    let path = PropertyPath::parse(&prop, &mut store.tokens, &mut store.paths)
                        .expect("path");
                    let value = stage.resolve_field_path(path).map(|r| r.value);
                    if value != Some(Value::Double(expected)) {
                        failures.push(format!("{arc}: {prop} = {value:?}"));
                    }
                }
                for owner in [&c, &g] {
                    let child = format!("{owner}/TChild");
                    let path =
                        layerstack::Path::parse_absolute(&child, &mut store.tokens).expect("path");
                    let id = store.paths.intern(path);
                    if !stage.has_prim(id) {
                        failures.push(format!("{arc}: arc target child {child} missing"));
                    }
                }
            }

            let path =
                layerstack::Path::parse_absolute("/Default/C", &mut store.tokens).expect("path");
            let default_c = store.paths.intern(path);
            if stage.has_prim(default_c) {
                failures.push(format!("{arc}: unselected /Default/C populated"));
            }
        }
    }
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

/// An arc kind as authored in USDA, and the targets used by the arc matrix
/// for branch `a` (`x = 10`, child `TChild`) and branch `b` (`x = 20`, child
/// `BChild`).
#[derive(Clone, Copy, Debug)]
struct MatrixArc {
    keyword: &'static str,
    target_a: &'static str,
    target_b: &'static str,
    /// Dependency source path recorded for the branch `a` target.
    source_a: &'static str,
    /// Dependency source path recorded for the branch `b` target.
    source_b: &'static str,
}

const MATRIX_ARCS: [MatrixArc; 4] = [
    MatrixArc {
        keyword: "references",
        target_a: "@./lib.usda@</Target>",
        target_b: "@./lib.usda@</TargetB>",
        source_a: "/Target",
        source_b: "/TargetB",
    },
    MatrixArc {
        keyword: "payload",
        target_a: "@./lib.usda@</Target>",
        target_b: "@./lib.usda@</TargetB>",
        source_a: "/Target",
        source_b: "/TargetB",
    },
    MatrixArc {
        keyword: "inherits",
        target_a: "</Model/Cls>",
        target_b: "</Model/ClsB>",
        source_a: "/Model/Cls",
        source_b: "/Model/ClsB",
    },
    MatrixArc {
        keyword: "specializes",
        target_a: "</Model/Cls>",
        target_b: "</Model/ClsB>",
        source_a: "/Model/Cls",
        source_b: "/Model/ClsB",
    },
];

/// Where the selection of `/Model`'s `v` set comes from in a matrix scene.
#[derive(Clone, Copy, Debug, PartialEq)]
enum MatrixSelection {
    /// The prim that brings `/Model` in selects `a` (library default `b`).
    Site,
    /// An intermediate prim between the site and `/Model` selects `a`.
    Between,
    /// Nobody overrides the library default `a`.
    DefaultA,
    /// Nobody overrides the library default `b`.
    DefaultB,
}

const MATRIX_LIB: &str = r#"#usda 1.0
def "Target"
{
    double x = 10
    def "TChild"
    {
    }
}

def "TargetB"
{
    double x = 20
    def "BChild"
    {
    }
}
"#;

/// Library `/Model`: branch `a` introduces `C` and `C/G`, branch `b`
/// introduces `CB` and `CB/G`; each authors `inner` to its branch's target.
fn matrix_model(inner: MatrixArc, default: &str) -> String {
    let branch = |child: &str, target: &str| {
        format!(
            r#"            def "{child}" (
                {arc} = {target}
            )
            {{
                def "G" (
                    {arc} = {target}
                )
                {{
                }}
            }}
"#,
            arc = inner.keyword
        )
    };
    format!(
        r#"#usda 1.0
def "Model" (
    variantSets = ["v"]
    variants = {{
        string v = "{default}"
    }}
)
{{
    class "Cls"
    {{
        double x = 10
        def "TChild"
        {{
        }}
    }}

    class "ClsB"
    {{
        double x = 20
        def "BChild"
        {{
        }}
    }}

    variantSet "v" = {{
        "a" {{
{a}        }}
        "b" {{
{b}        }}
    }}
}}
"#,
        a = branch("C", inner.target_a),
        b = branch("CB", inner.target_b),
    )
}

/// Root (and intermediate) layers bringing `/Model` in as `/R` through
/// `outer`, with the selection authored as `selection` says.
fn matrix_root(outer: MatrixArc, selection: MatrixSelection) -> (String, Option<String>) {
    let select = "\n    variants = {\n        string v = \"a\"\n    }";
    let site_select = if selection == MatrixSelection::Site {
        select
    } else {
        ""
    };
    let between = selection == MatrixSelection::Between;
    let arc = outer.keyword;
    match arc {
        "references" | "payload" => {
            let target = if between {
                "@./mid.usda@</M>"
            } else {
                "@./model.usda@</Model>"
            };
            let root =
                format!("#usda 1.0\ndef \"R\" (\n    {arc} = {target}{site_select}\n)\n{{\n}}\n");
            let mid = between.then(|| {
                format!(
                    "#usda 1.0\ndef \"M\" (\n    {arc} = @./model.usda@</Model>{select}\n)\n{{\n}}\n"
                )
            });
            (root, mid)
        }
        _ => {
            let mut root = String::from("#usda 1.0\n(\n    subLayers = [@./model.usda@]\n)\n");
            if between {
                root.push_str(&format!(
                    "class \"Mid\" (\n    {arc} = </Model>{select}\n)\n{{\n}}\n"
                ));
            }
            let target = if between { "</Mid>" } else { "</Model>" };
            root.push_str(&format!(
                "def \"R\" (\n    {arc} = {target}{site_select}\n)\n{{\n}}\n"
            ));
            (root, None)
        }
    }
}

/// Composes one matrix scene and returns a description of each failed check.
fn matrix_cell(outer: MatrixArc, inner: MatrixArc, selection: MatrixSelection) -> Vec<String> {
    let default = if selection == MatrixSelection::DefaultA {
        "a"
    } else {
        "b"
    };
    let model: &'static str = Box::leak(matrix_model(inner, default).into_boxed_str());
    let (root, mid) = matrix_root(outer, selection);
    let root: &'static str = Box::leak(root.into_boxed_str());
    let mut others = vec![("model.usda", model), ("lib.usda", MATRIX_LIB)];
    if let Some(mid) = mid {
        others.push(("mid.usda", Box::leak(mid.into_boxed_str())));
    }
    let mut store = load(root, &others);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_dependencies: true,
            ..StageOptions::default()
        },
    );

    let selected_a = selection != MatrixSelection::DefaultB;
    let (on, off, x, target_child, other_child) = if selected_a {
        ("C", "CB", 10.0, "TChild", "BChild")
    } else {
        ("CB", "C", 20.0, "BChild", "TChild")
    };
    let mut path = |s: &str| {
        let p = layerstack::Path::parse_absolute(s, &mut store.tokens).expect("path");
        store.paths.intern(p)
    };
    let off_prim = path(&format!("/R/{off}"));
    let (selected_source, other_source) = if selected_a {
        (inner.source_a, inner.source_b)
    } else {
        (inner.source_b, inner.source_a)
    };
    let selected_source_id = path(selected_source);
    let other_source_id = path(other_source);

    let mut failures = Vec::new();
    if stage.has_prim(off_prim) {
        failures.push(format!("unselected /R/{off} populated"));
    }
    for owner in [format!("/R/{on}"), format!("/R/{on}/G")] {
        let prop = PropertyPath::parse(&format!("{owner}.x"), &mut store.tokens, &mut store.paths)
            .expect("path");
        // Distinct values: the stack must hold the selected branch's value
        // and nothing from the other branch. (Repeated identical opinions
        // from an outer inherit/specialize of a prim that is also composed
        // on the stage are a separate, pre-existing issue.)
        let mut stack: Vec<Value> = Vec::new();
        for op in stage.explain_property_path(prop).unwrap_or(&[]) {
            if let Some(value) = op.value.default_value()
                && !stack.contains(value)
            {
                stack.push(value.clone());
            }
        }
        let expected = vec![Value::Double(x)];
        if stack != expected {
            failures.push(format!("{owner}.x stack {stack:?}"));
        }
        let has = |store: &mut InMemoryStore, name: &str| {
            let p = layerstack::Path::parse_absolute(&format!("{owner}/{name}"), &mut store.tokens)
                .expect("path");
            stage.has_prim(store.paths.intern(p))
        };
        if !has(&mut store, target_child) {
            failures.push(format!("{owner}/{target_child} missing"));
        }
        if has(&mut store, other_child) {
            failures.push(format!("{owner}/{other_child} leaked"));
        }
        let owner_id = prop.prim_path();
        let sources: Vec<layerstack::PathId> = stage
            .arcs_targeting(owner_id)
            .iter()
            .map(|dep| dep.source)
            .collect();
        // Arcs nested inside another arc are tracked through layer
        // dependencies (top-level arcs also record arc edges): the layer
        // holding the selected branch's arc target must feed this prim.
        let target_layer = store
            .layers
            .iter()
            .find(|(_, layer)| layer.prims.contains_key(&selected_source_id))
            .map(|(id, _)| *id);
        if !target_layer
            .is_some_and(|layer| stage.prims_affected_by_layer(layer).contains(&owner_id))
        {
            failures.push(format!(
                "{owner} lacks a layer dependency on the layer holding {selected_source}"
            ));
        }
        if sources.contains(&other_source_id) {
            failures.push(format!("{owner} depends on unselected {other_source}"));
        }
    }
    // Dependency data may only name prims that exist: a dependency on a prim
    // an unselected branch introduced would invalidate phantom prims.
    for layer in 1..=4 {
        for prim in stage.prims_affected_by_layer(LayerId(layer)) {
            if !stage.has_prim(prim) {
                let name = store.paths.display(prim, &store.tokens);
                failures.push(format!("layer {layer} dependency on missing {name}"));
            }
        }
    }
    for dep in stage.arc_dependencies() {
        if !stage.has_prim(dep.target) {
            let name = store.paths.display(dep.target, &store.tokens);
            failures.push(format!("arc dependency targets missing {name}"));
        }
    }
    failures
}

/// Arcs authored on a selected variant branch's child (and on a deeper
/// descendant) must compose, and the unselected branch must contribute
/// nothing, whichever arc brings the variant host in and wherever the
/// selection is authored.
///
/// Matrix: outer arc × inner arc × selection source, checking values
/// (whole opinion stack), arc-target children, the other branch's content,
/// and dependency data.
///
/// Spec: AOUSD Core §10.5; OpenUSD resolves a set's selection by searching
/// the prim index composed so far, strongest first, and adds arcs only
/// beneath the selected variant node (`pxr/usd/pcp/primIndex.cpp`).
#[test]
fn variant_arc_matrix() {
    let selections = [
        MatrixSelection::Site,
        MatrixSelection::Between,
        MatrixSelection::DefaultA,
        MatrixSelection::DefaultB,
    ];
    let mut failed_cells = Vec::new();
    let mut cells = 0;
    for outer in MATRIX_ARCS {
        for inner in MATRIX_ARCS {
            for selection in selections {
                cells += 1;
                let failures = matrix_cell(outer, inner, selection);
                if !failures.is_empty() {
                    failed_cells.push(format!(
                        "outer {} / inner {} / {selection:?}: {}",
                        outer.keyword,
                        inner.keyword,
                        failures.join("; ")
                    ));
                }
            }
        }
    }
    assert!(
        failed_cells.is_empty(),
        "{} of {cells} cells failed:\n{}",
        failed_cells.len(),
        failed_cells.join("\n")
    );
}
