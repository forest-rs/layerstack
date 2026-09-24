// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Arc cycle regressions driven through the USDA pipeline.
//!
//! Each scene authors a composition arc that leads back to a site already
//! being composed. Composition must terminate, report the cycle through
//! [`Stage::composition_errors`], skip the offending arc, and compose
//! everything else (AOUSD Core §10.6). The cycle rule and the expected prim
//! stacks follow OpenUSD (`_CheckForCycle` in `pxr/usd/pcp/primIndex.cpp`).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{
    ArcKind, AssetResolveError, AssetResolver, CompositionError, InMemoryStore, Layer, LayerId,
    PathInterner, ResolvedAsset, Stage, StageOptions, SublayerCycle, TokenInterner,
};
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

/// A composed scene plus the names of its layers.
struct Scene {
    store: InMemoryStore,
    stage: Stage,
    names: BTreeMap<LayerId, String>,
}

/// Loads `root.usda` (and the layers it names from `others`) and composes it.
///
/// `root.usda` is registered as layer 1, so other layers can refer back to it.
fn compose(root: &'static str, others: &[(&'static str, &'static str)]) -> Scene {
    let mut store = InMemoryStore::default();
    let mut resolver = MemoryResolver {
        sources: others.iter().copied().collect(),
        by_name: BTreeMap::from([("root.usda".to_string(), LayerId(1))]),
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
    let names = resolver
        .by_name
        .iter()
        .map(|(name, id)| (*id, name.clone()))
        .collect();
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    Scene {
        store,
        stage,
        names,
    }
}

impl Scene {
    fn path(&mut self, s: &str) -> layerstack::PathId {
        self.store.path(s)
    }

    fn display(&self, path: layerstack::PathId) -> String {
        self.store.paths.display(path, &self.store.tokens)
    }

    fn layer(&self, id: LayerId) -> &str {
        self.names.get(&id).map_or("?", String::as_str)
    }

    /// Returns the prim stack of `prim` as `layer path` strings, strongest
    /// first.
    fn prim_stack(&mut self, prim: &str) -> Vec<String> {
        let id = self.path(prim);
        let stack = self
            .stage
            .prim_stack(id)
            .unwrap_or_else(|| panic!("{prim} is not composed"));
        stack
            .iter()
            .map(|(layer, spec)| {
                format!(
                    "{} {}",
                    self.layer(*layer),
                    spec.display(&self.store.tokens)
                )
            })
            .collect()
    }

    /// Returns the composed children of `prim`.
    fn children(&mut self, prim: &str) -> Vec<String> {
        let id = self.path(prim);
        self.stage
            .children_of(id)
            .unwrap_or(&[])
            .iter()
            .map(|child| self.display(*child))
            .collect()
    }

    /// Renders each composition error in the shape of OpenUSD's messages:
    /// `prim: site -arc-> site ... -arc-> rejected site`.
    fn errors(&self) -> Vec<String> {
        self.stage
            .composition_errors()
            .iter()
            .map(|error| match error {
                CompositionError::ArcCycle(cycle) => {
                    let mut out = format!("{}:", self.display(cycle.prim));
                    for site in &cycle.sites {
                        if let Some(arc) = site.arc {
                            let arc = match arc {
                                ArcKind::Inherits => "inherits",
                                ArcKind::References => "references",
                                ArcKind::Payloads => "payload",
                                ArcKind::Specializes => "specializes",
                                other => panic!("unexpected arc {other:?}"),
                            };
                            out.push_str(&format!(" -{arc}->"));
                        }
                        out.push_str(&format!(
                            " @{}@<{}>",
                            self.layer(site.layer_stack),
                            self.display(site.path)
                        ));
                    }
                    out
                }
                CompositionError::SublayerCycle(SublayerCycle { layer, sublayer }) => {
                    format!(
                        "sublayer cycle: {} -> {}",
                        self.layer(*layer),
                        self.layer(*sublayer)
                    )
                }
                other => format!("{other:?}"),
            })
            .collect()
    }
}

#[test]
fn self_reference_is_a_cycle() {
    let mut scene = compose(
        r#"#usda 1.0
def "A" (
    references = </A>
)
{
    def "Child"
    {
    }
}
"#,
        &[],
    );
    assert_eq!(
        scene.errors(),
        ["/A: @root.usda@</A> -references-> @root.usda@</A>"]
    );
    assert_eq!(scene.prim_stack("/A"), ["root.usda /A"]);
    assert_eq!(scene.children("/A"), ["/A/Child"]);
    assert!(scene.children("/A/Child").is_empty());
}

#[test]
fn mutual_references_are_a_cycle() {
    // `ErrorArcCycle_root`, `/GroupRoot`: A.usda references B.usda, which
    // references A.usda again.
    let mut scene = compose(
        r#"#usda 1.0
def "Root" (
    references = @./a.usda@</A>
)
{
}
"#,
        &[
            (
                "a.usda",
                r#"#usda 1.0
def "A" (
    references = @./b.usda@</B>
)
{
    double a = 1
    def "ChildA"
    {
    }
}
"#,
            ),
            (
                "b.usda",
                r#"#usda 1.0
def "B" (
    references = @./a.usda@</A>
)
{
    double b = 2
}
"#,
            ),
        ],
    );
    assert_eq!(
        scene.errors(),
        [
            "/Root: @root.usda@</Root> -references-> @a.usda@</A> -references-> @b.usda@</B> -references-> @a.usda@</A>"
        ]
    );
    assert_eq!(
        scene.prim_stack("/Root"),
        ["root.usda /Root", "a.usda /A", "b.usda /B"]
    );
    assert_eq!(scene.children("/Root"), ["/Root/ChildA"]);
    assert_eq!(scene.prim_stack("/Root/ChildA"), ["a.usda /A/ChildA"]);
}

#[test]
fn referencing_an_ancestor_is_a_cycle() {
    // `ErrorArcCycle_root`, `/AnotherParent/AnotherChild`: the referenced
    // prim references the referencing prim's parent, which would nest
    // `/Parent/Child/Child/...` without end.
    let mut scene = compose(
        r#"#usda 1.0
def "Parent"
{
    def "Child" (
        references = @./model.usda@</Model>
    )
    {
    }
}
"#,
        &[(
            "model.usda",
            r#"#usda 1.0
def "Model" (
    references = @./root.usda@</Parent>
)
{
}
"#,
        )],
    );
    assert_eq!(
        scene.errors(),
        [
            "/Parent/Child: @root.usda@</Parent/Child> -references-> @model.usda@</Model> -references-> @root.usda@</Parent>"
        ]
    );
    assert_eq!(
        scene.prim_stack("/Parent/Child"),
        ["root.usda /Parent/Child", "model.usda /Model"]
    );
    assert!(scene.children("/Parent/Child").is_empty());
}

#[test]
fn mutual_inherits_are_a_cycle() {
    // `ErrorArcCycle_root`, `/Parent/Child1` and `/Parent/Child2`.
    let mut scene = compose(
        r#"#usda 1.0
def "Parent"
{
    def "Child1" (
        inherits = </Parent/Child2>
    )
    {
    }
    def "Child2" (
        inherits = </Parent/Child1>
    )
    {
    }
}
"#,
        &[],
    );
    assert_eq!(
        scene.errors(),
        [
            "/Parent/Child1: @root.usda@</Parent/Child1> -inherits-> @root.usda@</Parent/Child2> -inherits-> @root.usda@</Parent/Child1>",
            "/Parent/Child2: @root.usda@</Parent/Child2> -inherits-> @root.usda@</Parent/Child1> -inherits-> @root.usda@</Parent/Child2>",
        ]
    );
    assert_eq!(
        scene.prim_stack("/Parent/Child1"),
        ["root.usda /Parent/Child1", "root.usda /Parent/Child2"]
    );
    assert_eq!(
        scene.prim_stack("/Parent/Child2"),
        ["root.usda /Parent/Child2", "root.usda /Parent/Child1"]
    );
}

#[test]
fn inheriting_an_ancestor_or_descendant_is_a_cycle() {
    // `ErrorArcCycle_root`, `/YetAnotherParent/Child` and `/InheritOfChild`.
    let mut scene = compose(
        r#"#usda 1.0
def "Parent"
{
    def "Child" (
        inherits = </Parent>
    )
    {
    }
}

def "Holder" (
    inherits = </Holder/Class>
)
{
    class "Class"
    {
        def "Leaf"
        {
        }
    }
}
"#,
        &[],
    );
    assert_eq!(
        scene.errors(),
        [
            "/Parent/Child: @root.usda@</Parent/Child> -inherits-> @root.usda@</Parent>",
            "/Holder: @root.usda@</Holder> -inherits-> @root.usda@</Holder/Class>",
        ]
    );
    assert_eq!(
        scene.prim_stack("/Parent/Child"),
        ["root.usda /Parent/Child"]
    );
    assert!(scene.children("/Parent/Child").is_empty());
    assert_eq!(scene.prim_stack("/Holder"), ["root.usda /Holder"]);
    assert_eq!(scene.children("/Holder"), ["/Holder/Class"]);
}

#[test]
fn co_recursive_inherits_stop_at_the_cycle() {
    // `ErrorArcCycle_root`, `/CoRecursiveParent1` and `/CoRecursiveParent2`.
    let mut scene = compose(
        r#"#usda 1.0
def "P1"
{
    over "C1" (
        inherits = </P2>
    )
    {
    }
}
def "P2"
{
    over "C2" (
        inherits = </P1>
    )
    {
    }
}
"#,
        &[],
    );
    assert_eq!(
        scene.errors(),
        [
            "/P1/C1/C2: @root.usda@</P1/C1/C2> -inherits-> @root.usda@</P2/C2> -inherits-> @root.usda@</P1>",
            "/P2/C2/C1: @root.usda@</P2/C2/C1> -inherits-> @root.usda@</P1/C1> -inherits-> @root.usda@</P2>",
        ]
    );
    assert_eq!(
        scene.prim_stack("/P1/C1"),
        ["root.usda /P1/C1", "root.usda /P2"]
    );
    assert_eq!(scene.children("/P1/C1"), ["/P1/C1/C2"]);
    assert_eq!(scene.prim_stack("/P1/C1/C2"), ["root.usda /P2/C2"]);
    assert!(scene.children("/P1/C1/C2").is_empty());
    assert_eq!(
        scene.prim_stack("/P2/C2"),
        ["root.usda /P2/C2", "root.usda /P1"]
    );
    assert_eq!(scene.prim_stack("/P2/C2/C1"), ["root.usda /P1/C1"]);
    assert!(scene.children("/P2/C2/C1").is_empty());
}

#[test]
fn payloads_to_an_ancestor_are_a_cycle() {
    let mut scene = compose(
        r#"#usda 1.0
def "Root" (
    payload = @./a.usda@</A>
)
{
}

def "Parent"
{
    def "Child" (
        payload = @./b.usda@</B>
    )
    {
    }
}
"#,
        &[
            (
                "a.usda",
                r#"#usda 1.0
def "A" (
    payload = @./a.usda@</A>
)
{
    double a = 1
}
"#,
            ),
            (
                "b.usda",
                r#"#usda 1.0
def "B" (
    payload = @./root.usda@</Parent>
)
{
}
"#,
            ),
        ],
    );
    assert_eq!(
        scene.errors(),
        [
            "/Root: @root.usda@</Root> -payload-> @a.usda@</A> -payload-> @a.usda@</A>",
            "/Parent/Child: @root.usda@</Parent/Child> -payload-> @b.usda@</B> -payload-> @root.usda@</Parent>",
        ]
    );
    assert_eq!(scene.prim_stack("/Root"), ["root.usda /Root", "a.usda /A"]);
    assert_eq!(
        scene.prim_stack("/Parent/Child"),
        ["root.usda /Parent/Child", "b.usda /B"]
    );
    assert!(scene.children("/Parent/Child").is_empty());
}

#[test]
fn cycle_through_a_selected_variant() {
    // The selected branch's child references its own variant host; the
    // unselected branch authors no arc.
    let mut scene = compose(
        r#"#usda 1.0
def "Host" (
    variantSets = ["v"]
    variants = {
        string v = "cyclic"
    }
)
{
    variantSet "v" = {
        "cyclic" {
            def "Child" (
                references = </Host>
            )
            {
                double x = 1
            }
        }
        "plain" {
            def "Child"
            {
                double x = 2
            }
        }
    }
}
"#,
        &[],
    );
    assert_eq!(
        scene.errors(),
        ["/Host/Child: @root.usda@</Host/Child> -references-> @root.usda@</Host>"]
    );
    assert_eq!(scene.children("/Host"), ["/Host/Child"]);
    assert_eq!(
        scene.prim_stack("/Host/Child"),
        ["root.usda /Host{v=cyclic}Child"]
    );
    assert!(scene.children("/Host/Child").is_empty());
}

#[test]
fn cycle_through_a_sublayer() {
    // The arc closing the cycle is authored in a sublayer of the referenced
    // layer; the site is identified by the referenced layer stack's root.
    let mut scene = compose(
        r#"#usda 1.0
def "Root" (
    references = @./a.usda@</A>
)
{
}
"#,
        &[
            (
                "a.usda",
                r#"#usda 1.0
(
    subLayers = [@./a_sub.usda@]
)
def "A"
{
}
"#,
            ),
            (
                "a_sub.usda",
                r#"#usda 1.0
over "A" (
    references = @./b.usda@</B>
)
{
}
"#,
            ),
            (
                "b.usda",
                r#"#usda 1.0
def "B" (
    references = @./a.usda@</A>
)
{
}
"#,
            ),
        ],
    );
    assert_eq!(
        scene.errors(),
        [
            "/Root: @root.usda@</Root> -references-> @a.usda@</A> -references-> @b.usda@</B> -references-> @a.usda@</A>"
        ]
    );
    assert_eq!(
        scene.prim_stack("/Root"),
        ["root.usda /Root", "a.usda /A", "a_sub.usda /A", "b.usda /B"]
    );
}

#[test]
fn sublayer_cycle_is_its_own_error() {
    // A `subLayers` cycle is not an arc cycle: OpenUSD reports it as
    // `PcpErrorSublayerCycle` and drops the repeated sublayer.
    let mut scene = compose(
        r#"#usda 1.0
(
    subLayers = [@./a.usda@]
)
def "Root"
{
}
"#,
        &[(
            "a.usda",
            r#"#usda 1.0
(
    subLayers = [@./root.usda@]
)
def "A"
{
}
"#,
        )],
    );
    assert_eq!(scene.errors(), ["sublayer cycle: a.usda -> root.usda"]);
    assert_eq!(scene.prim_stack("/A"), ["a.usda /A"]);
    assert_eq!(scene.children("/"), ["/A", "/Root"]);
}
