// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition limits ([`CompositionLimits`]) on scenes that compose
//! without end.
//!
//! `/Branch/Leaf` specializes `/_LeafClass`, which specializes `/Branch`:
//! `/Leaf`, specializing the same class, gains `/Branch`'s child `Leaf`
//! through it, which does the same, level after level. No arc site
//! repeats, so there is no arc cycle (AOUSD Core §10.6), and OpenUSD 26.11
//! composes `/Leaf/Leaf/...` until the process runs out of memory. With a
//! second child `Twig` the namespace is a tree that doubles each level.
//! Both are minimized fuzz inputs. Composition stops at the namespace
//! depth limit for the chain and at the prim index node limit for the
//! tree, deterministically, and keeps what it composed before.

use layerstack::{
    AssetResolveError, AssetResolver, CompositionError, CompositionLimit, CompositionLimits,
    EditTarget, InMemoryStore, Layer, LayerId, ListPosition, LiveStage, PathId, PathInterner,
    PrimPredicate, PrimSpec, Reference, ResolvedAsset, Stage, StageOptions, TokenInterner,
    Transaction,
};

/// An endless chain: `/Leaf/Leaf/...`.
const CHAIN: &str = r#"#usda 1.0

def "Leaf" (
    prepend specializes = </_LeafClass>
)
{
}

class "_LeafClass" (
    specializes = </Branch>
)
{
}

class "Branch"
{
    def "Leaf" (
        prepend specializes = </_LeafClass>
    )
    {
    }
}
"#;

/// An endless tree: `/Leaf/Leaf`, `/Leaf/Twig`, `/Leaf/Leaf/Leaf`, ...,
/// before `/_LeafClass` specializes `/Branch` when `cycle`.
fn tree(cycle: bool) -> String {
    let arc = if cycle {
        "(\n    specializes = </Branch>\n)\n"
    } else {
        ""
    };
    format!(
        r#"#usda 1.0

def "Leaf" (
    prepend specializes = </_LeafClass>
)
{{
}}

class "_LeafClass" {arc}{{
}}

class "Branch"
{{
    def "Leaf" (
        prepend specializes = </_LeafClass>
    )
    {{
    }}

    def "Twig" (
        prepend specializes = </_LeafClass>
    )
    {{
    }}
}}
"#
    )
}

struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// A store holding `text` as layer 1.
fn store_of(text: &str) -> InMemoryStore {
    let parsed = layerstack_usda::parser::parse(text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let mut store = InMemoryStore::default();
    let result = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    store.insert_layer(result.layer);
    store
}

fn options(limits: CompositionLimits, threads: usize) -> StageOptions {
    StageOptions {
        limits,
        threads,
        ..StageOptions::default()
    }
}

/// What a stage composed: every prim, with its children, and every error,
/// as text.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    prims: Vec<String>,
    errors: Vec<String>,
}

fn outcome(stage: &Stage, store: &InMemoryStore) -> Outcome {
    let root = store.path("/");
    let predicate = PrimPredicate {
        instance_proxies: true,
        ..PrimPredicate::ALL
    };
    let prims = stage
        .prim_range(root, store, predicate)
        .map(|path| {
            let children: Vec<String> = stage
                .view(store)
                .prim(path)
                .expect("a traversed prim")
                .child_paths()
                .iter()
                .map(|child| name(store, *child))
                .collect();
            format!("{} {children:?}", name(store, path))
        })
        .collect();
    let errors = stage
        .composition_errors()
        .iter()
        .map(|error| error.display(store).to_string())
        .collect();
    Outcome { prims, errors }
}

fn name(store: &InMemoryStore, path: PathId) -> String {
    store.paths().display(path, store.tokens())
}

/// The limits the stage reached, with their values and prims.
fn limits_reached(stage: &Stage, store: &InMemoryStore) -> Vec<(CompositionLimit, usize, String)> {
    stage
        .composition_errors()
        .iter()
        .filter_map(|error| match error {
            CompositionError::LimitExceeded(limit) => {
                Some((limit.limit, limit.value, name(store, limit.prim)))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn an_endless_chain_stops_at_the_default_composed_depth() {
    let mut store = store_of(CHAIN);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let depth = CompositionLimits::DEFAULT_COMPOSED_DEPTH;
    // `/Leaf/Leaf` reads `/Branch/Leaf`, two levels deep, the deepest spec
    // any prim below it reads: each is a level further below it.
    let deepest = "/Leaf".repeat(depth + 2);
    assert_eq!(
        limits_reached(&stage, &store),
        [(CompositionLimit::ComposedDepth, depth, deepest.clone())]
    );
    let error = stage
        .composition_errors()
        .iter()
        .find(|error| matches!(error, CompositionError::LimitExceeded(_)))
        .expect("the limit");
    assert_eq!(error.code(), "composition.limit_exceeded");
    assert!(
        error.display(&store).to_string().ends_with(
            ": the prim is 64 levels below the deepest spec its sites author, the composed \
                 depth limit (`composed_depth`); its children are not composed"
        ),
        "{}",
        error.display(&store)
    );
    let view = stage.view(&store);
    assert!(
        view.prim(&deepest)
            .expect("the deepest prim")
            .child_paths()
            .is_empty()
    );
    assert_eq!(
        view.prim(&deepest[.."/Leaf".len() * (depth + 1)])
            .expect("its parent")
            .child_paths()
            .len(),
        1
    );
    // The chain stops long before the node limit.
    let nodes = stage.composition_work().prim_index_nodes;
    assert!(
        nodes < CompositionLimits::DEFAULT_PRIM_INDEX_NODES / 10,
        "{nodes}"
    );
}

/// A store whose layer 1 authors a namespace `referencing` levels deep,
/// whose deepest prim references `/Asset`, a namespace `asset` levels
/// deep, and the path of the deepest prim the reference composes.
fn referenced_asset(referencing: usize, asset: usize) -> (InMemoryStore, String) {
    let mut store = InMemoryStore::default();
    let mut layer = Layer::new(LayerId(1));
    let mut path = String::from("/Asset");
    layer.insert_prim(store.path(&path), PrimSpec::def());
    for _ in 1..asset {
        path.push_str("/Part");
        layer.insert_prim(store.path(&path), PrimSpec::def());
    }
    let mut path = String::new();
    for _ in 1..referencing {
        path.push_str("/Deep");
        layer.insert_prim(store.path(&path), PrimSpec::def());
    }
    let mut spec = PrimSpec::def();
    spec.references.make_mut().explicit =
        Some(vec![Reference::new(LayerId(1), store.path("/Asset"))]);
    path.push_str("/Ref");
    layer.insert_prim(store.path(&path), spec);
    store.insert_layer(layer);
    path.push_str(&"/Part".repeat(asset - 1));
    (store, path)
}

#[test]
fn arcs_authored_deep_or_to_deep_assets_compose_whole() {
    // A reference 100 levels deep to an asset 30 deep, and one at the root
    // to an asset 100 deep: each prim is within the default composed depth
    // of the spec its referencing prim or its asset authors.
    for (referencing, asset) in [(100, 30), (1, 100)] {
        let (mut store, deepest) = referenced_asset(referencing, asset);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(outcome(&stage, &store).errors, Vec::<String>::new());
        assert!(stage.view(&store).prim(&deepest).is_some());
    }
    // A reference more than the limit deep to an asset more than the limit
    // deep: the asset's prims more than the limit below the reference are
    // as far below the referencing prim's spec as below the asset's
    // deepest, and are not composed.
    let (mut store, _) = referenced_asset(100, 100);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let reached = limits_reached(&stage, &store);
    assert_eq!(reached.len(), 1, "{reached:?}");
    let stopped = format!("{}/Ref{}", "/Deep".repeat(99), "/Part".repeat(64));
    assert_eq!(reached[0], (CompositionLimit::ComposedDepth, 64, stopped));
}

/// A minimized fuzz input: `/C/n` gains `/Base/n`'s child `n` through
/// `/SiblingsA`, which specializes `/Base`, and so on, each prim's index
/// larger than its parent's and costlier per node. OpenUSD 26.11 composes
/// it without end (its memory still growing after 90 seconds).
const NESTED_CLASS_CHAIN: &str = r#"#usda 1.0

def "C" (
    specializes = </Base>
)
{
}

class "Base"
{
    class "n" (
        specializes = </SiblingsA>
    )
    {
        def "Cld" (
            specializes = </Base/I>
        )
        {
        }
    }
}

class "SiblingsA" (
    specializes = </Base>
)
{
}
"#;

#[test]
fn an_endless_chain_through_a_nested_class_stops_at_the_default_composed_depth() {
    let mut store = store_of(NESTED_CLASS_CHAIN);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let reached = limits_reached(&stage, &store);
    assert_eq!(reached.len(), 1, "{reached:?}");
    assert_eq!(
        (reached[0].0, reached[0].1),
        (
            CompositionLimit::ComposedDepth,
            CompositionLimits::DEFAULT_COMPOSED_DEPTH
        )
    );
    assert!(reached[0].2.starts_with("/C/n/n/"), "{}", reached[0].2);
}

#[test]
fn an_index_stops_growing_at_the_nodes_per_prim_index() {
    // Without the other limits, the nested class chain's indexes grow
    // until one holds 64 nodes: that prim has no more arcs and no
    // children, on any number of threads.
    let limits = CompositionLimits {
        nodes_per_prim_index: Some(64),
        ..CompositionLimits::UNLIMITED
    };
    let mut store = store_of(NESTED_CLASS_CHAIN);
    let serial = Stage::compose(&mut store, LayerId(1), options(limits, 1));
    let reached = limits_reached(&serial, &store);
    assert!(!reached.is_empty());
    assert!(
        reached
            .iter()
            .all(|(limit, value, _)| *limit == CompositionLimit::NodesPerPrimIndex && *value == 64),
        "{reached:?}"
    );
    let view = serial.view(&store);
    for (_, _, prim) in &reached {
        assert!(view.prim(prim).expect("composed").child_paths().is_empty());
    }
    let error = serial
        .composition_errors()
        .iter()
        .find(|error| matches!(error, CompositionError::LimitExceeded(_)))
        .expect("the limit");
    assert!(
        error.display(&store).to_string().ends_with(
            ": the prim's index reached 64 nodes, the limit of one index \
             (`nodes_per_prim_index`); it has no more arcs, and its children are not composed"
        ),
        "{}",
        error.display(&store)
    );
    #[cfg(not(target_os = "wasi"))]
    {
        let threaded = Stage::compose(&mut store, LayerId(1), options(limits, 8));
        assert_eq!(outcome(&threaded, &store), outcome(&serial, &store));
    }
}

#[test]
fn a_namespace_one_layer_authors_deeper_than_usda_nests_composes_whole() {
    // 300 levels: deeper than a USDA layer nests (256), within the default.
    let mut store = InMemoryStore::default();
    let mut layer = Layer::new(LayerId(1));
    let mut path = String::new();
    for _ in 0..300 {
        path.push_str("/Deep");
        layer.insert_prim(store.path(&path), PrimSpec::def());
    }
    store.insert_layer(layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert!(stage.composition_errors().is_empty());
    assert!(stage.view(&store).prim(&path).is_some());
}

#[test]
fn an_endless_tree_stops_at_the_node_limit_on_any_number_of_threads() {
    let limits = CompositionLimits {
        prim_index_nodes: Some(20_000),
        composed_depth: Some(40),
        ..CompositionLimits::default()
    };
    let mut store = store_of(&tree(true));
    let serial = Stage::compose(&mut store, LayerId(1), options(limits, 1));
    let reached = limits_reached(&serial, &store);
    let stopped: Vec<_> = reached
        .iter()
        .filter(|(limit, _, _)| *limit == CompositionLimit::PrimIndexNodes)
        .collect();
    assert_eq!(stopped.len(), 1, "{reached:?}");
    assert_eq!(stopped[0].1, 20_000);
    // The prim the nodes ran out at is the last composed, without children.
    let view = serial.view(&store);
    assert!(
        view.prim(&stopped[0].2)
            .expect("composed")
            .child_paths()
            .is_empty()
    );
    let nodes = serial.composition_work().prim_index_nodes;
    assert!((20_000..21_000).contains(&nodes), "{nodes}");
    // Composition went 40 levels deep first, in traversal order, and every
    // prim there names children it did not compose.
    assert!(
        reached
            .iter()
            .any(|(limit, value, _)| *limit == CompositionLimit::ComposedDepth && *value == 40)
    );

    let expected = outcome(&serial, &store);
    #[cfg(not(target_os = "wasi"))]
    for threads in [2, 8] {
        let threaded = Stage::compose(&mut store, LayerId(1), options(limits, threads));
        assert_eq!(outcome(&threaded, &store), expected, "{threads} threads");
        assert_eq!(threaded.composition_work().prim_index_nodes, nodes);
    }
}

#[test]
fn unlimited_composition_of_a_finite_scene_is_the_default_composition() {
    let mut store = store_of(&tree(false));
    let limited = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let unlimited = Stage::compose(
        &mut store,
        LayerId(1),
        options(CompositionLimits::UNLIMITED, 1),
    );
    assert!(limited.composition_errors().is_empty());
    assert_eq!(outcome(&limited, &store), outcome(&unlimited, &store));
    assert_eq!(
        limited.composition_work().prim_index_nodes,
        unlimited.composition_work().prim_index_nodes
    );
}

#[test]
fn a_live_edit_that_makes_the_tree_endless_stops_at_the_limit() {
    let limits = CompositionLimits {
        prim_index_nodes: Some(5_000),
        composed_depth: Some(24),
        ..CompositionLimits::default()
    };
    let mut store = store_of(&tree(false));
    let mut live = LiveStage::compose(&mut store, LayerId(1), options(limits, 1));
    assert!(live.stage().composition_errors().is_empty());

    let class = store.path("/_LeafClass");
    let branch = store.path("/Branch");
    let at = || EditTarget::for_layer(LayerId(1)).prim(class);
    let mut edit = Transaction::new();
    edit.add_specialize(at(), branch, ListPosition::BackOfAppend);
    live.apply(&mut store, &edit).expect("the edit applies");
    // The stage stopped at its node limit: it recomposed whole, as a new
    // stage composes, rather than merge what the edit's prims composed.
    assert!(live.recomposition_work().full_rebuild);
    let fresh = Stage::compose(&mut store, LayerId(1), options(limits, 1));
    assert!(
        limits_reached(&fresh, &store)
            .iter()
            .any(|(limit, _, _)| *limit == CompositionLimit::PrimIndexNodes)
    );
    assert_eq!(outcome(live.stage(), &store), outcome(&fresh, &store));

    // Undoing the arc composes the finite tree again.
    let mut undo = Transaction::new();
    undo.remove_specialize(at(), branch);
    live.apply(&mut store, &undo).expect("the undo applies");
    let fresh = Stage::compose(&mut store, LayerId(1), options(limits, 1));
    assert!(fresh.composition_errors().is_empty());
    assert_eq!(outcome(live.stage(), &store), outcome(&fresh, &store));
}

#[test]
fn a_live_edit_that_makes_a_chain_endless_stops_at_the_depth() {
    let limits = CompositionLimits {
        prim_index_nodes: None,
        composed_depth: Some(32),
        ..CompositionLimits::default()
    };
    let text = CHAIN.replace("    specializes = </Branch>\n", "");
    let text = text.replace("class \"_LeafClass\" (\n)\n", "class \"_LeafClass\"\n");
    let mut store = store_of(&text);
    let mut live = LiveStage::compose(&mut store, LayerId(1), options(limits, 1));
    assert!(live.stage().composition_errors().is_empty());

    let class = store.path("/_LeafClass");
    let branch = store.path("/Branch");
    let mut edit = Transaction::new();
    edit.add_specialize(
        EditTarget::for_layer(LayerId(1)).prim(class),
        branch,
        ListPosition::BackOfAppend,
    );
    live.apply(&mut store, &edit).expect("the edit applies");
    let fresh = Stage::compose(&mut store, LayerId(1), options(limits, 1));
    assert_eq!(
        limits_reached(&fresh, &store),
        [(CompositionLimit::ComposedDepth, 32, "/Leaf".repeat(34))]
    );
    assert_eq!(outcome(live.stage(), &store), outcome(&fresh, &store));
}

#[test]
fn instances_copying_their_prototype_count_its_nodes() {
    // Every instance shares `/Model`'s prototype: one is composed, the rest
    // copy its descendants, and the copies count toward the limit.
    let mut text = String::from("#usda 1.0\n\nclass \"Model\"\n{\n");
    for part in 0..8 {
        text.push_str(&format!("    def \"Part{part}\"\n    {{\n    }}\n"));
    }
    text.push_str("}\n");
    for instance in 0..20 {
        text.push_str(&format!(
            "\ndef \"Instance{instance:02}\" (\n    instanceable = true\n    references = </Model>\n)\n{{\n}}\n"
        ));
    }
    let mut store = store_of(&text);
    let whole = Stage::compose(
        &mut store,
        LayerId(1),
        options(CompositionLimits::UNLIMITED, 1),
    );
    let work = whole.composition_work();
    assert!(work.reused_prim_indexes > 0);
    let limits = CompositionLimits {
        prim_index_nodes: Some(work.prim_index_nodes - 1),
        ..CompositionLimits::default()
    };
    let serial = Stage::compose(&mut store, LayerId(1), options(limits, 1));
    // The instance whose copies would reach the limit gets none; the
    // instances copied before it keep theirs.
    let reached = limits_reached(&serial, &store);
    assert_eq!(reached.len(), 1, "{reached:?}");
    let (limit, value, stopped) = &reached[0];
    assert_eq!(
        (*limit, *value),
        (CompositionLimit::PrimIndexNodes, work.prim_index_nodes - 1)
    );
    let view = serial.view(&store);
    for instance in 0..20 {
        let path = format!("/Instance{instance:02}");
        let parts = view.prim(&path).expect("composed").child_paths().len();
        assert_eq!(parts, if path == *stopped { 0 } else { 8 }, "{path}");
    }
    #[cfg(not(target_os = "wasi"))]
    {
        let threaded = Stage::compose(&mut store, LayerId(1), options(limits, 8));
        assert_eq!(outcome(&threaded, &store), outcome(&serial, &store));
    }
}
