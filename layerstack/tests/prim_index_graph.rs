// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The per-prim composition graph exposed by `Stage::explain_prim_graph`.

#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    ArcKind, FieldEntry, HashMap, InMemoryStore, Layer, LayerId, NodeId, PrimIndexGraph, PrimSpec,
    Reference, Stage, StageOptions, Value, VariantSetSpec, VariantSpec,
};

/// `(arc kind, layer stack, site)` of each node from the root to `node`.
fn arc_path(
    store: &InMemoryStore,
    graph: &PrimIndexGraph,
    mut node: NodeId,
) -> Vec<(ArcKind, LayerId, String)> {
    let mut path = Vec::new();
    loop {
        let entry = graph.node(node).expect("node exists");
        path.push((
            entry.arc_kind(),
            entry.layer_stack(),
            entry.site().display(&store.tokens),
        ));
        let Some(parent) = entry.parent() else {
            break;
        };
        node = parent;
    }
    path.reverse();
    path
}

/// A grove of trees: `/Grove` (layer 1) references `/Tree` in the asset
/// layer (layer 2), which inherits `/_class_Tree` there and has a `season`
/// variant set selected to `summer`; `/Tree/Leaves` is a child of the tree.
fn grove() -> (InMemoryStore, Stage) {
    let mut store = InMemoryStore::default();
    let height = store.tokens.intern("height");
    let color = store.tokens.intern("color");
    let leaves_tok = store.tokens.intern("Leaves");
    let season = store.tokens.intern("season");
    let summer = store.tokens.intern("summer");
    let grove = store.path("/Grove");
    let tree = store.path("/Tree");
    let tree_leaves = store.path("/Tree/Leaves");
    let class = store.path("/_class_Tree");

    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        grove,
        PrimSpec::def()
            .with_reference(Reference::new(LayerId(2), tree))
            .with_field(height, Value::Double(3.0)),
    );
    store.insert_layer(root);

    let mut asset = Layer::new(LayerId(2));
    let mut tree_spec = PrimSpec::def()
        .with_inherit(class)
        .with_children(vec![leaves_tok])
        .with_field(height, Value::Double(12.0));
    let mut variants = HashMap::new();
    variants.insert(
        summer,
        VariantSpec {
            fields: vec![FieldEntry {
                name: color,
                value: Value::string("green").into(),
            }],
            ..Default::default()
        },
    );
    tree_spec
        .variant_sets
        .insert(season, VariantSetSpec { variants });
    tree_spec.variant_selections.insert(season, summer);
    asset.insert_prim(tree, tree_spec);
    asset.insert_prim(
        tree_leaves,
        PrimSpec::def().with_field(color, Value::string("green")),
    );
    asset.insert_prim(
        class,
        PrimSpec::class().with_field(height, Value::Double(10.0)),
    );
    store.insert_layer(asset);

    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );
    (store, stage)
}

/// Each source names the node of the arc that reaches it: the prim's own
/// site is the root, the referenced site a child of the root, and the class
/// the referenced site inherits a child of the reference's node.
///
/// Spec: AOUSD Core §10.4 (an arc's target ranks beneath the site that
/// authors it). OpenUSD: `PcpPrimIndex::GetGraph()`.
#[test]
fn sources_name_the_node_of_their_arc_path() {
    let (mut store, stage) = grove();
    let grove = store.path("/Grove");
    let graph = stage.explain_prim_graph(grove).expect("graph");
    let sources = stage.explain_prim(grove).expect("sources");

    let root = graph.root().expect("root");
    let root_node = graph.node(root).expect("root node");
    assert_eq!(root_node.arc_kind(), ArcKind::Local);
    assert_eq!(root_node.parent(), None);

    let paths: Vec<_> = sources
        .iter()
        .map(|source| arc_path(&store, graph, source.node))
        .collect();
    let local = (ArcKind::Local, LayerId(1), "/Grove".to_owned());
    let reference = (ArcKind::References, LayerId(2), "/Tree".to_owned());
    assert_eq!(
        paths,
        [
            vec![local.clone()],
            vec![local.clone(), reference.clone()],
            vec![
                local.clone(),
                reference.clone(),
                (ArcKind::Inherits, LayerId(2), "/_class_Tree".to_owned()),
            ],
            vec![
                local,
                reference,
                (
                    ArcKind::Variants,
                    LayerId(2),
                    "/Tree{season=summer}".to_owned()
                ),
            ],
        ],
        "local, reference, the class it inherits, then its variant (LIVERPS)"
    );

    let reference_node = graph.node(sources[1].node).expect("reference node");
    assert_eq!(reference_node.namespace_depth(), 1);
    assert_eq!(reference_node.sibling_index(), 0);
    assert!(!reference_node.is_implied());
    assert_eq!(reference_node.origin(), None);
    assert!(
        reference_node
            .children()
            .iter()
            .all(|child| graph.node(*child).expect("child").parent() == Some(sources[1].node)),
        "children link back to their parent"
    );
}

/// An arc authored on an ancestor contributes a node to each descendant,
/// at the site the descendant's path maps to, introduced at the ancestor's
/// namespace depth.
///
/// Spec: AOUSD Core §10.2 (arcs map the target's namespace onto the
/// referencing prim's).
#[test]
fn ancestral_arcs_add_nodes_to_descendants() {
    let (mut store, stage) = grove();
    let leaves = store.path("/Grove/Leaves");
    let graph = stage.explain_prim_graph(leaves).expect("graph");
    let sources = stage.explain_prim(leaves).expect("sources");
    assert_eq!(sources.len(), 1, "only the referenced child has a spec");
    assert_eq!(
        arc_path(&store, graph, sources[0].node),
        [
            (ArcKind::Local, LayerId(1), "/Grove/Leaves".to_owned()),
            (ArcKind::References, LayerId(2), "/Tree/Leaves".to_owned()),
        ]
    );
    let node = graph.node(sources[0].node).expect("reference node");
    assert_eq!(node.namespace_depth(), 1, "the arc is authored on `/Grove`");
}

/// The prim's opinions are ranked in the order the graph's strength
/// traversal visits their nodes, and the depth-first traversal visits every
/// node once.
///
/// Spec: AOUSD Core §10.4 (strength ordering).
#[test]
fn opinion_order_follows_the_graphs_strength_order() {
    let (store, stage) = grove();
    let root = store.paths.lookup(&layerstack::Path::root()).expect("root");
    for prim in stage.traverse(root).filter(|prim| *prim != root) {
        let graph = stage.explain_prim_graph(prim).expect("graph");
        let rank: HashMap<NodeId, usize> = graph
            .strength_order()
            .into_iter()
            .enumerate()
            .map(|(rank, node)| (node, rank))
            .collect();
        let ranks: Vec<usize> = stage
            .explain_prim(prim)
            .expect("sources")
            .iter()
            .map(|source| rank[&source.node])
            .collect();
        assert!(
            ranks.is_sorted(),
            "sources of {prim:?} follow the strength order: {ranks:?}"
        );

        let mut visited = graph.depth_first();
        assert_eq!(visited.first(), graph.root().as_ref());
        visited.sort();
        visited.dedup();
        assert_eq!(visited.len(), graph.len(), "depth-first visits every node");
    }
}

/// The selection authored on the referenced tree governs the grove until
/// the grove's own layer authors a stronger one, which then wins even for
/// a variant that no layer defines.
///
/// Spec: AOUSD Core §10.5. OpenUSD: `UsdVariantSets::GetAllVariantSelections`.
#[test]
fn variant_selections_report_the_strongest_opinion() {
    let (mut store, stage) = grove();
    let grove = store.path("/Grove");
    let season = store.tokens.intern("season");
    let summer = store.tokens.intern("summer");
    let winter = store.tokens.intern("winter");
    let selections = stage.variant_selections(grove, &store);
    assert_eq!(selections.get(&season), Some(&summer));
    assert_eq!(selections.len(), 1, "one variant set is selected");
    let absent = store.path("/Absent");
    assert!(stage.variant_selections(absent, &store).is_empty());

    store
        .layers
        .get_mut(&LayerId(1))
        .and_then(|layer| layer.prims.get_mut(&grove))
        .expect("grove spec")
        .variant_selections
        .insert(season, winter);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        stage.variant_selections(grove, &store).get(&season),
        Some(&winter)
    );
}
