// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Tests of the strength order a [`Graph`] keeps its nodes in.

use core::cmp::Ordering;

use super::{Graph, NONE, Node, NodeIndex, NodeLookup, Site};
use crate::{
    interner::TokenInterner, path::PathInterner, prim_index::ArcKind,
    prim_indexing::layer_stacks::StackId, spec_path::SpecPath,
};

/// A deterministic stream of numbers: a linear congruential generator.
struct Numbers(u64);

impl Numbers {
    /// The next number, below `bound`.
    fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).expect("31 bits") % bound
    }
}

/// The one site every node of these graphs has: their order does not
/// read it.
fn site(paths: &PathInterner) -> Site {
    Site {
        stack: StackId(0),
        path: SpecPath::from_prim_path(paths.intern_segments(&[])),
    }
}

/// A node at `site` for an arc of `arc_kind` beneath `parent`, from
/// `origin`.
fn node(
    site: &Site,
    arc_kind: ArcKind,
    parent: NodeIndex,
    origin: NodeIndex,
    sibling_num: u16,
) -> Node {
    Node {
        arc_kind,
        parent,
        origin,
        sibling_num,
        ..Graph::new(site.clone(), 0).nodes.remove(0)
    }
}

/// Adds a node for an arc of `arc_kind` beneath `parent`, from `origin`.
fn add(
    graph: &mut Graph,
    paths: &PathInterner,
    arc_kind: ArcKind,
    parent: NodeIndex,
    origin: NodeIndex,
    sibling_num: u16,
) -> NodeIndex {
    let index = graph.push_node(node(&site(paths), arc_kind, parent, origin, sibling_num));
    graph.link_child(paths, parent, index);
    index
}

/// A graph of `count` nodes, each beneath a random earlier node, for an
/// arc authored there or implied from another earlier node: siblings
/// implied from different nodes order by their origins' strength.
fn random_graph(paths: &PathInterner, count: usize, numbers: &mut Numbers) -> Graph {
    let mut graph = Graph::new(site(paths), 0);
    for at in 1..count {
        let parent = super::node_index(numbers.below(at));
        let origin = if numbers.below(2) == 0 {
            parent
        } else {
            super::node_index(numbers.below(at))
        };
        let arc_kind =
            [ArcKind::Inherits, ArcKind::References, ArcKind::Payloads][numbers.below(3)];
        let sibling_num = u16::try_from(numbers.below(4)).expect("small");
        add(&mut graph, paths, arc_kind, parent, origin, sibling_num);
    }
    graph
}

/// OpenUSD's `_OriginIsStronger`: which of `a` and `b` a strong-to-weak
/// walk from `start` meets first.
fn walk_order(graph: &Graph, start: NodeIndex, a: NodeIndex, b: NodeIndex) -> Ordering {
    if start == a {
        return Ordering::Less;
    }
    if start == b {
        return Ordering::Greater;
    }
    for child in graph.children(start) {
        let order = walk_order(graph, child, a, b);
        if order.is_ne() {
            return order;
        }
    }
    Ordering::Equal
}

/// Asserts that the keys of every node's children grow along its child
/// list.
fn assert_sibling_keys_ordered(graph: &Graph) {
    for at in 0..graph.len() {
        let children = graph.child_list(super::node_index(at));
        for pair in children.windows(2) {
            assert!(
                graph.node(pair[0]).sibling_key < graph.node(pair[1]).sibling_key,
                "children of {at}: {pair:?}"
            );
        }
    }
}

#[test]
fn origins_order_as_a_strength_order_walk_meets_them() {
    let paths = PathInterner::default();
    let mut numbers = Numbers(7);
    for count in [1, 2, 3, 17, 160] {
        let mut graph = random_graph(&paths, count, &mut numbers);
        // A node not yet linked, as `link_child` leaves the node it links
        // while it compares it with its siblings.
        let loose = graph.push_node(node(&site(&paths), ArcKind::References, 0, 0, 0));
        let mut nodes: alloc::vec::Vec<NodeIndex> =
            (0..graph.len()).map(super::node_index).collect();
        nodes.push(NONE);
        for &a in &nodes {
            for &b in &nodes {
                assert_eq!(
                    graph.origin_is_stronger(a, b),
                    walk_order(&graph, 0, a, b),
                    "{a} and {b} of {count} nodes (loose: {loose})"
                );
            }
        }
    }
}

#[test]
fn siblings_linked_between_two_others_renumber_in_order() {
    let paths = PathInterner::default();
    let mut graph = Graph::new(site(&paths), 0);
    add(&mut graph, &paths, ArcKind::References, 0, 0, 0);
    add(&mut graph, &paths, ArcKind::References, 0, 0, u16::MAX);
    // Each new child is linked just before the last: the room between
    // the keys there halves each time, until the children renumber.
    for sibling_num in 1..200 {
        add(&mut graph, &paths, ArcKind::References, 0, 0, sibling_num);
        assert_sibling_keys_ordered(&graph);
    }
    let order: alloc::vec::Vec<u16> = graph
        .children(0)
        .map(|child| graph.node(child).sibling_num)
        .collect();
    assert!(order.is_sorted(), "{order:?}");
    // Prepending, many times, has room before the first key.
    for _ in 0..100 {
        add(&mut graph, &paths, ArcKind::Inherits, 0, 0, 0);
    }
    assert_sibling_keys_ordered(&graph);
    // Finalizing relinks every node in strength order.
    graph.node_mut(1).culled = true;
    graph.finalize();
    assert_sibling_keys_ordered(&graph);
}

/// A graph of `count` nodes like [`random_graph`]'s, at a few sites, some
/// of them inert or culled.
fn graph_at_sites(
    paths: &PathInterner,
    sites: &[Site],
    count: usize,
    numbers: &mut Numbers,
) -> Graph {
    let mut graph = Graph::new(sites[0].clone(), 0);
    for at in 1..count {
        let parent = super::node_index(numbers.below(at));
        let origin = if numbers.below(2) == 0 {
            parent
        } else {
            super::node_index(numbers.below(at))
        };
        let arc_kind =
            [ArcKind::Inherits, ArcKind::References, ArcKind::Specializes][numbers.below(3)];
        let site = &sites[numbers.below(sites.len())];
        let index = graph.push_node(node(site, arc_kind, parent, origin, 0));
        graph.link_child(paths, parent, index);
        graph.node_mut(index).inert = numbers.below(4) == 0;
        graph.node_mut(index).culled = numbers.below(8) == 0;
    }
    graph
}

/// Asserts that `graph`'s lookups answer as scans of its nodes do.
fn assert_lookups_match_scans(graph: &Graph, sites: &[Site]) {
    let nodes = || (0..graph.len()).map(super::node_index);
    for site in sites {
        let scanned = nodes().find(|&n| {
            let node = graph.node(n);
            !(node.inert || node.culled) && node.site == *site
        });
        assert_eq!(graph.node_using_site(site), scanned, "{site:?}");
        for parent in nodes() {
            let scanned = graph
                .children(parent)
                .find(|&child| graph.node(child).site == *site);
            assert_eq!(
                graph.child_using_site(parent, site),
                scanned,
                "{parent}: {site:?}"
            );
        }
    }
    let propagated = |node: NodeIndex| graph.is_propagated_specializes(node);
    let any = |_: NodeIndex| true;
    for parent in nodes() {
        for origin in nodes() {
            let scanned: super::ChildList = graph
                .children(parent)
                .filter(|&child| graph.node(child).origin == origin)
                .collect();
            let strongest = scanned.iter().copied().find(|&child| !propagated(child));
            let weakest = scanned
                .iter()
                .copied()
                .rev()
                .find(|&child| propagated(child));
            assert_eq!(
                graph.strongest_child_from(parent, origin, |child| !propagated(child)),
                strongest,
                "{parent} from {origin}"
            );
            assert_eq!(
                graph.weakest_child_from(parent, origin, propagated),
                weakest,
                "{parent} from {origin}"
            );
            assert_eq!(
                graph.strongest_child_from(parent, origin, any),
                scanned.first().copied()
            );
            assert_eq!(
                graph.weakest_child_from(parent, origin, any),
                scanned.last().copied()
            );
        }
    }
}

#[test]
fn a_large_graph_finds_nodes_as_scans_of_it_do() {
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let site_at = |name: &str| Site {
        stack: StackId(0),
        path: SpecPath::from_prim_path(paths.intern_segments(&[tokens.intern(name)])),
    };
    let mut sites: alloc::vec::Vec<Site> = ["A", "B", "C", "D"].into_iter().map(site_at).collect();
    let mut numbers = Numbers(11);
    let small = graph_at_sites(&paths, &sites, NodeLookup::MIN_NODES, &mut numbers);
    assert!(small.lookup.is_none());
    assert_lookups_match_scans(&small, &sites);
    let mut graph = graph_at_sites(&paths, &sites, 3 * NodeLookup::MIN_NODES, &mut numbers);
    assert!(graph.lookup.is_some());
    assert_lookups_match_scans(&graph, &sites);
    // A child prim's index starts from its parent's, every site one name
    // deeper.
    let name = tokens.intern("Child");
    graph.append_child_name(&paths, name);
    for site in &mut sites {
        site.path = site
            .path
            .child(paths.intern_joined(site.path.prim_path(), &[name]));
    }
    assert_lookups_match_scans(&graph, &sites);
    // A finished index keeps no lookup. (The indexer culls whole
    // subtrees; these culled nodes are scattered.)
    for at in 0..graph.len() {
        graph.node_mut(super::node_index(at)).culled = false;
    }
    graph.finalize();
    assert!(graph.lookup.is_none());
}

/// The sibling OpenUSD links `child` before, walking `parent`'s children
/// strongest first (`_InsertChildInStrengthOrder`); `NONE` for none.
fn front_scan_place(
    graph: &Graph,
    paths: &PathInterner,
    parent: NodeIndex,
    child: NodeIndex,
) -> NodeIndex {
    let (first, last) = (
        graph.node(parent).first_child,
        graph.node(parent).last_child,
    );
    if first == NONE || graph.cmp_siblings(paths, child, first).is_lt() {
        return first;
    }
    if !graph.cmp_siblings(paths, child, last).is_lt() {
        return NONE;
    }
    graph
        .children(parent)
        .find(|&at| graph.cmp_siblings(paths, child, at).is_lt())
        .expect("stronger than the last")
}

#[test]
fn children_are_linked_where_a_walk_from_the_strongest_links_them() {
    let paths = PathInterner::default();
    let mut numbers = Numbers(5);
    let mut graph = Graph::new(site(&paths), 0);
    let kinds = [
        ArcKind::Inherits,
        ArcKind::Variants,
        ArcKind::References,
        ArcKind::Payloads,
        ArcKind::Specializes,
    ];
    for at in 1..400 {
        // Few parents, so most nodes are linked among many siblings.
        let parent = super::node_index(numbers.below(at.min(4)));
        let origin = if numbers.below(2) == 0 {
            parent
        } else {
            super::node_index(numbers.below(at))
        };
        let arc_kind = kinds[numbers.below(kinds.len())];
        let sibling_num = u16::try_from(numbers.below(3)).expect("small");
        let mut new = node(&site(&paths), arc_kind, parent, origin, sibling_num);
        new.namespace_depth = u16::try_from(numbers.below(2)).expect("small");
        let index = graph.push_node(new);
        // What `link_child` records before it compares the node.
        graph.keep_ancestry(parent, index);
        let expected = front_scan_place(&graph, &paths, parent, index);
        graph.link_child(&paths, parent, index);
        assert_eq!(graph.node(index).next_sibling, expected, "node {index}");
    }
}

#[test]
fn a_child_linked_near_the_end_of_many_siblings_compares_with_few() {
    let paths = PathInterner::default();
    let mut graph = Graph::new(site(&paths), 0);
    // A weaker sibling, then many inherits each weaker than the last:
    // each new one goes just before the weaker sibling.
    add(&mut graph, &paths, ArcKind::Payloads, 0, 0, 0);
    let count = 2_000;
    let before = super::SIBLING_COMPARISONS.with(core::cell::Cell::get);
    for at in 0..count {
        let origin = add(&mut graph, &paths, ArcKind::References, 1, 1, at);
        add(&mut graph, &paths, ArcKind::Inherits, 0, origin, 0);
    }
    let made = super::SIBLING_COMPARISONS.with(core::cell::Cell::get) - before;
    // A walk from the strongest would compare each with every earlier one.
    assert!(made <= 6 * usize::from(count), "{made} comparisons");
}

/// A graph of `count` nodes like [`random_graph`]'s, with specializes
/// among them: those beneath the root from another node are propagated
/// specializes (every node has the same site).
fn graph_with_specializes(paths: &PathInterner, count: usize, numbers: &mut Numbers) -> Graph {
    let mut graph = Graph::new(site(paths), 0);
    for at in 1..count {
        // Half beneath the first few nodes, for many siblings; half
        // beneath any, for deep walks.
        let parent = if numbers.below(2) == 0 {
            super::node_index(numbers.below(at.min(4)))
        } else {
            super::node_index(numbers.below(at))
        };
        let origin = if numbers.below(3) == 0 {
            parent
        } else {
            super::node_index(numbers.below(at))
        };
        let arc_kind =
            [ArcKind::Inherits, ArcKind::References, ArcKind::Specializes][numbers.below(3)];
        add(&mut graph, paths, arc_kind, parent, origin, 0);
    }
    graph
}

/// OpenUSD's `_GetOriginRootNode`.
fn walked_origin_root(graph: &Graph, node: NodeIndex) -> (NodeIndex, usize) {
    let (mut at, mut count) = (node, 0);
    while graph.node(at).origin != graph.node(at).parent {
        at = graph.node(at).origin;
        count += 1;
    }
    (at, count)
}

/// OpenUSD's `_OriginsAreNestedArcs`.
fn walked_nesting(graph: &Graph, a: NodeIndex, b: NodeIndex) -> bool {
    let nested_under = |x: NodeIndex, y: NodeIndex| {
        let mut at = x;
        while at != NONE {
            if at == y {
                return true;
            }
            at = if graph.is_propagated_specializes(at) {
                graph.node(at).origin
            } else {
                graph.node(at).parent
            };
        }
        false
    };
    nested_under(a, b) || nested_under(b, a)
}

/// Asserts that every node's kept ancestry answers as walks do.
fn assert_ancestries_match_walks(graph: &Graph) {
    let nodes = || (0..graph.len()).map(super::node_index);
    for a in nodes() {
        assert_eq!(
            graph.origin_root(a),
            walked_origin_root(graph, a),
            "node {a}"
        );
        for b in nodes() {
            assert_eq!(
                graph.origins_are_nested(a, b),
                walked_nesting(graph, a, b),
                "{a} and {b}"
            );
        }
    }
}

#[test]
fn ancestries_answer_as_walks_of_the_graph_do() {
    let paths = PathInterner::default();
    let mut numbers = Numbers(3);
    let mut graph = graph_with_specializes(&paths, 120, &mut numbers);
    assert_ancestries_match_walks(&graph);
    // A finalized subgraph, whose nodes are in strength order, so that
    // some origins follow the nodes implied from them, grafted beneath
    // a node of the graph.
    let mut subgraph = graph_with_specializes(&paths, 80, &mut numbers);
    subgraph.finalize();
    assert_ancestries_match_walks(&subgraph);
    graph.insert_subgraph(
        &paths,
        5,
        subgraph,
        super::SubgraphArc {
            arc_kind: ArcKind::References,
            origin: 5,
            map_to_parent: crate::prim_indexing::map_function::MapFunction::identity(),
            namespace_depth: 0,
            sibling_num: 0,
        },
    );
    assert_ancestries_match_walks(&graph);
    for _ in 0..40 {
        let at = graph.len();
        let origin = super::node_index(numbers.below(at));
        add(&mut graph, &paths, ArcKind::Specializes, 0, origin, 0);
    }
    assert_ancestries_match_walks(&graph);
    assert!(!graph.ancestries.is_empty());
    graph.finalize();
    assert!(graph.ancestries.is_empty());
    assert_ancestries_match_walks(&graph);
    // A child prim's graph, in the finalized order, keeps ancestries
    // again from its first specializes linked beside a sibling.
    let mut child = graph.for_child();
    assert_ancestries_match_walks(&child);
    add(&mut child, &paths, ArcKind::Specializes, 0, 7, 0);
    assert!(!child.ancestries.is_empty());
    assert_ancestries_match_walks(&child);
}
