// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The graph a prim index is built in.
//!
//! Nodes live in an arena and link to their parent, origin and siblings by
//! index, so a child's index starts as a cheap copy of its parent's. Each
//! node's children are kept in strength order as they are added.
//!
//! Spec: AOUSD Core §10.4 (strength ordering). OpenUSD:
//! `PcpPrimIndex_Graph` (`pxr/usd/pcp/primIndex_Graph.cpp`) and
//! `PcpCompareSiblingNodeStrength` (`pxr/usd/pcp/strengthOrdering.cpp`).

use alloc::vec::Vec;
use core::cmp::Ordering;

use hashbrown::HashMap;

use crate::{path::PathInterner, prim_index::ArcKind, spec_path::SpecPath};

use super::{layer_stacks::StackId, map_function::MapFunction};

/// Index of a node in its [`Graph`].
pub(crate) type NodeIndex = u32;

/// No node.
pub(crate) const NONE: NodeIndex = NodeIndex::MAX;

/// A node's children, collected ([`Graph::child_list`]).
pub(crate) type ChildList = smallvec::SmallVec<[NodeIndex; 8]>;

/// The [`Node::sibling_key`] of a node's only child: the middle of the
/// keys, leaving as much room for stronger siblings as for weaker ones.
const FIRST_SIBLING_KEY: u64 = 1 << 63;

/// The distance between the [`Node::sibling_key`]s of a child appended or
/// prepended to its siblings, or of adjacent siblings renumbered: a child
/// linked between two siblings takes the key halfway between theirs, so
/// about 32 children can be linked between two adjacent siblings before
/// the siblings are renumbered.
const SIBLING_KEY_GAP: u64 = 1 << 32;

#[cfg(test)]
std::thread_local! {
    /// The sibling comparisons made on this thread
    /// ([`Graph::cmp_siblings`]), for tests of how many linking takes.
    static SIBLING_COMPARISONS: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

/// The [`NodeIndex`] of the node at `at` in a graph's node list.
pub(crate) fn node_index(at: usize) -> NodeIndex {
    NodeIndex::try_from(at).expect("prim index node count")
}

/// A layer stack and a site in it: a prim path with the variant selections
/// of the spec it addresses.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Site {
    pub(crate) stack: StackId,
    pub(crate) path: SpecPath,
}

/// One node of a [`Graph`].
#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub(crate) arc_kind: ArcKind,
    pub(crate) parent: NodeIndex,
    pub(crate) origin: NodeIndex,
    pub(crate) first_child: NodeIndex,
    pub(crate) last_child: NodeIndex,
    pub(crate) prev_sibling: NodeIndex,
    pub(crate) next_sibling: NodeIndex,
    /// Orders the node among its parent's children: the keys of a node's
    /// children grow from the strongest to the weakest
    /// ([`Graph::sibling_order`]). Set as the node is linked.
    pub(crate) sibling_key: u64,
    pub(crate) site: Site,
    /// Maps paths of this node's namespace to its parent's.
    pub(crate) map_to_parent: MapFunction,
    /// Maps paths of this node's namespace to the root node's.
    pub(crate) map_to_root: MapFunction,
    /// Namespace depth of the prim the arc to this node is authored on.
    pub(crate) namespace_depth: u16,
    /// Position of the arc among those of its kind authored at its origin.
    pub(crate) sibling_num: u16,
    /// A placeholder that contributes no specs.
    pub(crate) inert: bool,
    /// A node whose subtree contributes no specs, removed when the graph
    /// is finalized.
    pub(crate) culled: bool,
    /// Whether a layer of the node's stack has a spec at its site.
    pub(crate) has_specs: bool,
    /// Whether a layer of the node's stack authors value clips at its site
    /// or an ancestor site.
    pub(crate) has_value_clips: bool,
    /// The namespace depth at which the node was restricted from
    /// contributing specs; `0` for none.
    pub(crate) restricted_depth: u16,
    /// Whether the node was inherited from the parent prim's index.
    pub(crate) due_to_ancestor: bool,
    /// Whether an arc on the way from the root to this node was introduced
    /// at this prim's namespace depth. OpenUSD:
    /// `HasTransitiveDirectDependency`.
    pub(crate) transitive_direct: bool,
    /// Whether an arc on the way to this node was inherited from an
    /// ancestor prim. OpenUSD: `HasTransitiveAncestralDependency`.
    pub(crate) transitive_ancestral: bool,
    /// Whether the node was copied from the namespace parent's finished
    /// index, rather than added while indexing this prim (or built
    /// recursively for one of its arcs).
    pub(crate) from_parent_prim: bool,
    /// For a reference or payload node, the layer that authors the arc,
    /// whose offset in its stack retimes the node.
    pub(crate) authoring_layer: Option<crate::doc::LayerId>,
}

impl Node {
    /// Whether this node contributes the specs at its site.
    pub(crate) fn can_contribute_specs(&self) -> bool {
        !(self.inert || self.culled)
    }
}

/// A prim index graph being built. See the [module docs](self).
///
/// Not `Clone`: a child's graph starts from its parent's by
/// [`Graph::for_child`], which leaves the parent's shapes behind.
#[derive(Debug, Default)]
pub(crate) struct Graph {
    pub(crate) nodes: Vec<Node>,
    /// Whether the prim this graph indexes introduces a payload.
    pub(crate) has_payloads: bool,
    /// Whether the prim is an instance.
    pub(crate) instanceable: bool,
    /// The shape of the prim index graph built from this graph, which the
    /// graphs of the prim's children share when theirs is the same
    /// ([`crate::prim_index_graph::NodeShape`]); `None` until it is built.
    pub(crate) shape: Option<alloc::sync::Arc<crate::prim_index_graph::GraphShape>>,
    /// The shape of the first of the prim's children's graphs built, for
    /// its siblings to share ([`ChildShape`]).
    pub(crate) child_shape: ChildShape,
    /// The nodes by site and by origin, once the graph being built holds
    /// more than [`NodeLookup::MIN_NODES`] nodes; `None` before, and once
    /// the graph is finalized.
    lookup: Option<NodeLookup>,
}

/// A large graph's nodes by site and by origin, each list in arena order.
///
/// Adding a node to a prim index looks for nodes of the graph with the
/// node's site (a duplicate, or a child of the parent already there), or
/// for the children of a node that a node implied from (`origin`): OpenUSD
/// scans the graph or the parent's children for each node added, a cost
/// that grows with the index. A graph past [`Self::MIN_NODES`] nodes
/// answers these from the lists of the nodes that can match instead; one
/// smaller, as nearly every production index is, scans its few nodes.
#[derive(Debug, Default)]
struct NodeLookup {
    by_site: HashMap<Site, NodeList>,
    by_origin: HashMap<NodeIndex, NodeList>,
}

/// The nodes a [`NodeLookup`] holds for one key, in arena order.
type NodeList = smallvec::SmallVec<[NodeIndex; 2]>;

impl NodeLookup {
    /// The node count past which a graph keeps a lookup.
    const MIN_NODES: usize = 64;

    /// A lookup of every node of `nodes`.
    fn new(nodes: &[Node]) -> Self {
        let mut lookup = Self::default();
        for (at, node) in nodes.iter().enumerate() {
            lookup.add(node_index(at), node);
        }
        lookup
    }

    /// Adds `node`, at `index`, past every node already held.
    fn add(&mut self, index: NodeIndex, node: &Node) {
        self.by_site.entry_ref(&node.site).or_default().push(index);
        if node.origin != NONE {
            self.by_origin.entry(node.origin).or_default().push(index);
        }
    }
}

/// The shape of a prim's children's prim index graphs, once one is built:
/// the children of a prim often lose the same nodes of their parent's
/// graph (culled for having no specs beneath it), so their graphs match
/// one another's but not their parent's. A child's graph
/// ([`Graph::for_child`]) starts without one. Without the `std` feature, which composes on
/// one thread from graphs that must still be `Send`, none is kept.
#[derive(Debug, Default)]
pub(crate) struct ChildShape(
    #[cfg(feature = "std")]
    std::sync::OnceLock<alloc::sync::Arc<crate::prim_index_graph::GraphShape>>,
);

impl ChildShape {
    /// The shape, if a child's graph has been built.
    pub(crate) fn get(&self) -> Option<&alloc::sync::Arc<crate::prim_index_graph::GraphShape>> {
        #[cfg(feature = "std")]
        let shape = self.0.get();
        #[cfg(not(feature = "std"))]
        let shape = None;
        shape
    }

    /// Records `shape` unless a child's shape is recorded already.
    pub(crate) fn offer(&self, shape: &alloc::sync::Arc<crate::prim_index_graph::GraphShape>) {
        #[cfg(feature = "std")]
        self.0.get_or_init(|| alloc::sync::Arc::clone(shape));
        #[cfg(not(feature = "std"))]
        let _ = shape;
    }
}

/// Whether arcs of `kind` are inherits or specializes.
pub(crate) fn is_class_based(kind: ArcKind) -> bool {
    matches!(kind, ArcKind::Inherits | ArcKind::Specializes)
}

/// The namespace depth of `path`, not counting variant selections.
pub(crate) fn non_variant_depth(paths: &PathInterner, path: &SpecPath) -> u16 {
    u16::try_from(paths.resolve(path.prim_path()).depth()).unwrap_or(u16::MAX)
}

impl Graph {
    /// A graph holding only a root node at `site`.
    pub(crate) fn new(site: Site, namespace_depth: u16) -> Self {
        Self {
            nodes: alloc::vec![Node {
                arc_kind: ArcKind::Local,
                parent: NONE,
                origin: NONE,
                first_child: NONE,
                last_child: NONE,
                prev_sibling: NONE,
                next_sibling: NONE,
                sibling_key: FIRST_SIBLING_KEY,
                site,
                map_to_parent: MapFunction::identity(),
                map_to_root: MapFunction::identity(),
                namespace_depth,
                sibling_num: 0,
                inert: false,
                culled: false,
                has_specs: false,
                has_value_clips: false,
                restricted_depth: 0,
                due_to_ancestor: false,
                transitive_direct: false,
                transitive_ancestral: false,
                from_parent_prim: false,
                authoring_layer: None,
            }],
            has_payloads: false,
            instanceable: false,
            shape: None,
            child_shape: ChildShape::default(),
            lookup: None,
        }
    }

    /// The graph a child of the prim this graph indexes starts from: the
    /// same nodes, each marked as its parent prim's, and no shapes. The
    /// child reads the parent's shapes from the parent: a copy of their
    /// handles, made for every child on every thread and then discarded,
    /// would contend for the one count they share, as OpenUSD 26.11 found
    /// of the counts its child graphs copy and discard
    /// (`PcpPrimIndex_Graph::_ApplyNodeIndexMapping`).
    pub(crate) fn for_child(&self) -> Self {
        Self {
            nodes: self
                .nodes
                .iter()
                .map(|node| Node {
                    from_parent_prim: true,
                    ..node.clone()
                })
                .collect(),
            has_payloads: self.has_payloads,
            instanceable: self.instanceable,
            shape: None,
            child_shape: ChildShape::default(),
            // The child's sites differ from these
            // ([`Self::append_child_name`]): its lookup is its own.
            lookup: None,
        }
    }

    pub(crate) fn node(&self, index: NodeIndex) -> &Node {
        &self.nodes[index as usize]
    }

    pub(crate) fn node_mut(&mut self, index: NodeIndex) -> &mut Node {
        &mut self.nodes[index as usize]
    }

    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    /// The children of `parent`, strongest first.
    pub(crate) fn children(&self, parent: NodeIndex) -> Children<'_> {
        Children {
            graph: self,
            next: self.node(parent).first_child,
        }
    }

    /// The children of `parent`, collected, without allocating for the
    /// few children most nodes have.
    pub(crate) fn child_list(&self, parent: NodeIndex) -> ChildList {
        self.children(parent).collect()
    }

    /// `node` and every node beneath it, in strength order.
    pub(crate) fn subtree(&self, node: NodeIndex) -> Vec<NodeIndex> {
        // Room for every node, so the list never grows as it is filled.
        let mut out = Vec::with_capacity(self.nodes.len());
        self.push_subtree(node, &mut out);
        out
    }

    fn push_subtree(&self, node: NodeIndex, out: &mut Vec<NodeIndex>) {
        out.push(node);
        let mut child = self.node(node).first_child;
        while child != NONE {
            self.push_subtree(child, out);
            child = self.node(child).next_sibling;
        }
    }

    /// The node's depth below the namespace depth its arc was introduced
    /// at. OpenUSD: `PcpNodeRef::GetDepthBelowIntroduction`.
    pub(crate) fn depth_below_introduction(&self, paths: &PathInterner, node: NodeIndex) -> u16 {
        let entry = self.node(node);
        if entry.parent == NONE {
            return 0;
        }
        non_variant_depth(paths, &self.node(entry.parent).site.path)
            .saturating_sub(entry.namespace_depth)
    }

    /// The node's site path at the namespace depth its arc was introduced
    /// at. OpenUSD: `PcpNodeRef::GetPathAtIntroduction`.
    pub(crate) fn path_at_introduction(&self, paths: &PathInterner, node: NodeIndex) -> SpecPath {
        let levels = self.depth_below_introduction(paths, node);
        truncated(paths, &self.node(node).site.path, levels)
    }

    /// The parent's site path at the namespace depth `node`'s arc was
    /// introduced at. OpenUSD: `PcpNodeRef::GetIntroPath`.
    pub(crate) fn intro_path(&self, paths: &PathInterner, node: NodeIndex) -> SpecPath {
        let parent = self.node(node).parent;
        if parent == NONE {
            let root = paths.intern_segments(&[]);
            return SpecPath::from_prim_path(root);
        }
        let levels = self.depth_below_introduction(paths, node);
        truncated(paths, &self.node(parent).site.path, levels)
    }

    /// Whether `node` is a specializes node propagated to the root.
    /// OpenUSD: `Pcp_IsPropagatedSpecializesNode`.
    pub(crate) fn is_propagated_specializes(&self, node: NodeIndex) -> bool {
        let entry = self.node(node);
        entry.arc_kind == ArcKind::Specializes
            && entry.parent == 0
            && entry.origin != NONE
            && entry.site == self.node(entry.origin).site
    }

    /// The node at the start of `node`'s class hierarchy and the class node
    /// it inherits or specializes. OpenUSD:
    /// `Pcp_FindStartingNodeOfClassHierarchy`.
    pub(crate) fn starting_node_of_class_hierarchy(
        &self,
        paths: &PathInterner,
        node: NodeIndex,
    ) -> (NodeIndex, NodeIndex) {
        let mut instance = node;
        let mut class = NONE;
        if self.is_propagated_specializes(instance) {
            instance = self.node(instance).origin;
        }
        let depth = self.depth_below_introduction(paths, instance);
        while is_class_based(self.node(instance).arc_kind)
            && self.depth_below_introduction(paths, instance) == depth
        {
            class = instance;
            instance = self.node(instance).parent;
            if self.is_propagated_specializes(instance) {
                instance = self.node(instance).origin;
            }
        }
        (instance, class)
    }

    /// Inserts `child`, already in the arena, beneath `parent` in strength
    /// order. OpenUSD: `_InsertChildInStrengthOrder`.
    pub(crate) fn link_child(&mut self, paths: &PathInterner, parent: NodeIndex, child: NodeIndex) {
        self.insert_child(paths, parent, child);
        self.place_sibling_key(parent, child);
    }

    /// Links `child` among `parent`'s children, ordered by
    /// [`Self::cmp_siblings`].
    fn insert_child(&mut self, paths: &PathInterner, parent: NodeIndex, child: NodeIndex) {
        let first = self.node(parent).first_child;
        let last = self.node(parent).last_child;
        self.node_mut(child).parent = parent;
        if first == NONE {
            let p = self.node_mut(parent);
            p.first_child = child;
            p.last_child = child;
            return;
        }
        if self.cmp_siblings(paths, child, first).is_lt() {
            self.node_mut(child).next_sibling = first;
            self.node_mut(first).prev_sibling = child;
            self.node_mut(parent).first_child = child;
            return;
        }
        if !self.cmp_siblings(paths, child, last).is_lt() {
            self.node_mut(child).prev_sibling = last;
            self.node_mut(last).next_sibling = child;
            self.node_mut(parent).last_child = child;
            return;
        }
        let next = if self.node(child).arc_kind == ArcKind::Specializes {
            self.first_stronger_from_front(paths, child, first)
        } else {
            self.first_stronger_from_both_ends(paths, child, first, last)
        };
        let prev = self.node(next).prev_sibling;
        self.node_mut(child).prev_sibling = prev;
        self.node_mut(child).next_sibling = next;
        self.node_mut(next).prev_sibling = child;
        self.node_mut(prev).next_sibling = child;
    }

    /// The first of the siblings from `first` that `child` is stronger
    /// than, as OpenUSD finds it: walking the siblings strongest first.
    /// The last sibling is one.
    fn first_stronger_from_front(
        &self,
        paths: &PathInterner,
        child: NodeIndex,
        first: NodeIndex,
    ) -> NodeIndex {
        let mut at = first;
        loop {
            if self.cmp_siblings(paths, child, at).is_lt() {
                return at;
            }
            at = self.node(at).next_sibling;
        }
    }

    /// [`Self::first_stronger_from_front`] for a `child` that is not a
    /// specializes, which `child` is stronger than past `first` (it is not
    /// stronger than `first`, and is stronger than `last`), walking in
    /// from both ends of the siblings at once: a child added among many
    /// siblings is usually added near one end of them.
    ///
    /// The siblings are in [`Self::cmp_siblings`] order, which for nodes
    /// other than specializes compares fields of each node that never
    /// change once it is linked (arc kind, namespace depth, sibling
    /// number) and the order of the nodes' origins in the graph, which
    /// nodes added later leave as it is: the siblings `child` is stronger
    /// than all come after those it is not, so the first of them is the
    /// one after the last sibling it is not stronger than. Specializes
    /// compare by how their origins nest, and are found from the front.
    fn first_stronger_from_both_ends(
        &self,
        paths: &PathInterner,
        child: NodeIndex,
        first: NodeIndex,
        last: NodeIndex,
    ) -> NodeIndex {
        let (mut front, mut back) = (first, last);
        loop {
            // `child` is not stronger than `front`, and is stronger than
            // `back`.
            let after_front = self.node(front).next_sibling;
            if after_front == back || self.cmp_siblings(paths, child, after_front).is_lt() {
                return after_front;
            }
            front = after_front;
            let before_back = self.node(back).prev_sibling;
            if before_back == front || !self.cmp_siblings(paths, child, before_back).is_lt() {
                return back;
            }
            back = before_back;
        }
    }

    /// Gives `child`, just linked beneath `parent`, a
    /// [`Node::sibling_key`] between its neighbours' keys, renumbering
    /// `parent`'s children when there is no room between them.
    fn place_sibling_key(&mut self, parent: NodeIndex, child: NodeIndex) {
        let (prev, next) = (self.node(child).prev_sibling, self.node(child).next_sibling);
        let key = match (prev, next) {
            (NONE, NONE) => Some(FIRST_SIBLING_KEY),
            (prev, NONE) => self.node(prev).sibling_key.checked_add(SIBLING_KEY_GAP),
            (NONE, next) => self.node(next).sibling_key.checked_sub(SIBLING_KEY_GAP),
            (prev, next) => {
                let (low, high) = (self.node(prev).sibling_key, self.node(next).sibling_key);
                (high - low >= 2).then(|| low + (high - low) / 2)
            }
        };
        match key {
            Some(key) => self.node_mut(child).sibling_key = key,
            None => self.renumber_siblings(parent),
        }
    }

    /// Spaces the [`Node::sibling_key`]s of `parent`'s children evenly
    /// about [`FIRST_SIBLING_KEY`].
    fn renumber_siblings(&mut self, parent: NodeIndex) {
        let count = self.children(parent).count() as u64;
        let mut key = FIRST_SIBLING_KEY - (count / 2) * SIBLING_KEY_GAP;
        let mut child = self.node(parent).first_child;
        while child != NONE {
            self.node_mut(child).sibling_key = key;
            key += SIBLING_KEY_GAP;
            child = self.node(child).next_sibling;
        }
    }

    /// Appends a new node to the arena (not yet linked to a parent).
    pub(crate) fn push_node(&mut self, node: Node) -> NodeIndex {
        let index = NodeIndex::try_from(self.nodes.len()).expect("prim index node count");
        self.nodes.push(node);
        self.extend_lookup(index);
        index
    }

    /// Adds the nodes from `first` on to the graph's [`NodeLookup`],
    /// making one for every node once the graph holds more than
    /// [`NodeLookup::MIN_NODES`].
    fn extend_lookup(&mut self, first: NodeIndex) {
        match &mut self.lookup {
            Some(lookup) => {
                for at in first as usize..self.nodes.len() {
                    lookup.add(node_index(at), &self.nodes[at]);
                }
            }
            None if self.nodes.len() > NodeLookup::MIN_NODES => {
                self.lookup = Some(NodeLookup::new(&self.nodes));
            }
            None => {}
        }
    }

    /// Appends every node of `subgraph` to this graph, its root reached by
    /// the arc `root`'s fields describe, beneath `parent`. Returns the new
    /// index of the subgraph's root. OpenUSD: `InsertChildSubgraph`.
    pub(crate) fn insert_subgraph(
        &mut self,
        paths: &PathInterner,
        parent: NodeIndex,
        mut subgraph: Self,
        arc: SubgraphArc,
    ) -> NodeIndex {
        let base = NodeIndex::try_from(self.nodes.len()).expect("prim index node count");
        let shift = |index: NodeIndex| if index == NONE { NONE } else { index + base };
        let root_map_to_root = self
            .node(parent)
            .map_to_root
            .compose(paths, &arc.map_to_parent);
        for (at, node) in subgraph.nodes.iter_mut().enumerate() {
            if at == 0 {
                node.arc_kind = arc.arc_kind;
                node.parent = parent;
                node.origin = arc.origin;
                node.map_to_parent = arc.map_to_parent.clone();
                node.map_to_root = root_map_to_root.clone();
                node.namespace_depth = arc.namespace_depth;
                node.sibling_num = arc.sibling_num;
                node.prev_sibling = NONE;
                node.next_sibling = NONE;
            } else {
                node.map_to_root = root_map_to_root.compose(paths, &node.map_to_root);
                node.parent = shift(node.parent);
                node.origin = shift(node.origin);
                node.prev_sibling = shift(node.prev_sibling);
                node.next_sibling = shift(node.next_sibling);
            }
            node.first_child = shift(node.first_child);
            node.last_child = shift(node.last_child);
        }
        self.nodes.append(&mut subgraph.nodes);
        self.extend_lookup(base);
        self.link_child(paths, parent, base);
        base
    }

    /// Compares two children of one node, strongest first.
    /// OpenUSD: `PcpCompareSiblingNodeStrength`.
    pub(crate) fn cmp_siblings(
        &self,
        paths: &PathInterner,
        a: NodeIndex,
        b: NodeIndex,
    ) -> Ordering {
        #[cfg(test)]
        SIBLING_COMPARISONS.with(|count| count.set(count.get() + 1));
        if a == b {
            return Ordering::Equal;
        }
        let (na, nb) = (self.node(a), self.node(b));
        let by_kind = na
            .arc_kind
            .strength_rank()
            .cmp(&nb.arc_kind.strength_rank());
        if by_kind.is_ne() {
            return by_kind;
        }
        if na.arc_kind == ArcKind::Specializes {
            if let Some(order) = self.cmp_specializes(paths, a, b) {
                return order;
            }
        } else {
            let by_depth = nb.namespace_depth.cmp(&na.namespace_depth);
            if by_depth.is_ne() {
                return by_depth;
            }
            if na.origin != nb.origin {
                let order = self.origin_is_stronger(na.origin, nb.origin);
                if order.is_ne() {
                    return order;
                }
            }
        }
        na.sibling_num.cmp(&nb.sibling_num)
    }

    /// The specializes part of [`Self::cmp_siblings`]; `None` to fall back
    /// to sibling numbers.
    fn cmp_specializes(
        &self,
        paths: &PathInterner,
        a: NodeIndex,
        b: NodeIndex,
    ) -> Option<Ordering> {
        let (root_a, chain_a) = self.origin_root(a);
        let (root_b, chain_b) = self.origin_root(b);
        if !self.origins_are_nested(root_a, root_b) {
            let by_depth = self
                .node(b)
                .namespace_depth
                .cmp(&self.node(a).namespace_depth);
            if by_depth.is_ne() {
                return Some(by_depth);
            }
        }
        let (origin_a, origin_b) = (self.node(a).origin, self.node(b).origin);
        let authored_a = origin_a == self.node(a).parent;
        let authored_b = origin_b == self.node(b).parent;
        let implied =
            |node: NodeIndex, origin: NodeIndex| self.node(node).site != self.node(origin).site;
        if origin_a == origin_b {
            if !authored_a && !authored_b {
                return Some(implied(b, origin_b).cmp(&implied(a, origin_a)));
            }
            return None;
        }
        if root_a != root_b {
            return Some(self.origin_is_stronger(root_a, root_b));
        }
        let hierarchy_depth = |authored: bool, origin: NodeIndex| {
            if authored {
                0
            } else {
                self.namespace_depth_for_class_hierarchy(paths, origin)
            }
        };
        let by_hierarchy =
            hierarchy_depth(authored_a, origin_a).cmp(&hierarchy_depth(authored_b, origin_b));
        if by_hierarchy.is_ne() {
            return Some(by_hierarchy);
        }
        let by_chain = chain_b.cmp(&chain_a);
        if by_chain.is_ne() {
            return Some(by_chain);
        }
        let root_stack = self.node(0).site.stack;
        if self.node(a).site.stack == root_stack
            && self.node(b).site.stack == root_stack
            && !authored_a
            && !authored_b
        {
            let by_implied = implied(b, origin_b).cmp(&implied(a, origin_a));
            if by_implied.is_ne() {
                return Some(by_implied);
            }
        }
        Some(self.origin_is_stronger(origin_a, origin_b))
    }

    /// `_GetNamespaceDepthForClassHierarchy`.
    fn namespace_depth_for_class_hierarchy(&self, paths: &PathInterner, node: NodeIndex) -> u16 {
        let (mut instance, _) = self.starting_node_of_class_hierarchy(paths, node);
        while self.node(instance).arc_kind == ArcKind::Relocates {
            instance = self.node(instance).parent;
        }
        self.node(instance).namespace_depth
    }

    /// The first node of `node`'s chain of origins whose origin is its
    /// parent, and the length of the chain. OpenUSD: `_GetOriginRootNode`.
    fn origin_root(&self, node: NodeIndex) -> (NodeIndex, usize) {
        let mut cursor = node;
        let mut count = 0;
        while self.node(cursor).origin != self.node(cursor).parent {
            cursor = self.node(cursor).origin;
            count += 1;
        }
        (cursor, count)
    }

    /// OpenUSD: `_OriginsAreNestedArcs`.
    fn origins_are_nested(&self, a: NodeIndex, b: NodeIndex) -> bool {
        let nested_under = |x: NodeIndex, y: NodeIndex| {
            let mut n = x;
            while n != NONE {
                if n == y {
                    return true;
                }
                n = if self.is_propagated_specializes(n) {
                    self.node(n).origin
                } else {
                    self.node(n).parent
                };
            }
            false
        };
        nested_under(a, b) || nested_under(b, a)
    }

    /// Which of `a` and `b` a strong-to-weak walk of the graph from the
    /// root reaches first: `Less` for `a`, `Equal` for neither (a node not
    /// yet linked beneath the root). OpenUSD: `_OriginIsStronger`, called
    /// with the root node.
    ///
    /// OpenUSD walks the nodes in strength order until it meets one of the
    /// two, which costs a walk of most of the graph for each comparison of
    /// two siblings, and a graph's size again for each node linked. The
    /// walk visits a node before the nodes beneath it and a node's
    /// children strongest first, so the first of the two it meets is the
    /// ancestor of the other, or else the one beneath the stronger of the
    /// two children of their deepest common ancestor that lead to them:
    /// this answers the same from the two nodes' ancestors and the order
    /// of those two children ([`Node::sibling_key`]), without the walk.
    pub(crate) fn origin_is_stronger(&self, a: NodeIndex, b: NodeIndex) -> Ordering {
        let depth_a = self.linked_depth(a);
        let depth_b = self.linked_depth(b);
        let (depth_a, depth_b) = match (depth_a, depth_b) {
            (None, None) => return Ordering::Equal,
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (Some(depth_a), Some(depth_b)) => (depth_a, depth_b),
        };
        if a == b {
            return Ordering::Less;
        }
        // Bring the deeper node up to the other's depth: when that reaches
        // the other node, the other is its ancestor and is met first.
        let (mut x, mut y) = (a, b);
        for _ in depth_b..depth_a {
            x = self.node(x).parent;
        }
        if x == b {
            return Ordering::Greater;
        }
        for _ in depth_a..depth_b {
            y = self.node(y).parent;
        }
        if y == a {
            return Ordering::Less;
        }
        while self.node(x).parent != self.node(y).parent {
            x = self.node(x).parent;
            y = self.node(y).parent;
        }
        self.sibling_order(x, y)
    }

    /// The number of parents above `node` when it is linked beneath the
    /// root, each node below the root among its parent's children; `None`
    /// for a node outside the graph's tree (one not yet linked, or
    /// beneath one).
    fn linked_depth(&self, node: NodeIndex) -> Option<usize> {
        if node == NONE {
            return None;
        }
        let mut depth = 0;
        let mut at = node;
        while at != 0 {
            let entry = self.node(at);
            // A node is among its parent's children once linked:
            // `link_child` sets the parent before it links the node.
            if entry.parent == NONE
                || (entry.prev_sibling == NONE && self.node(entry.parent).first_child != at)
            {
                return None;
            }
            at = entry.parent;
            depth += 1;
        }
        Some(depth)
    }

    /// Which of two children of one node comes first among its children:
    /// `Less` for `a`.
    fn sibling_order(&self, a: NodeIndex, b: NodeIndex) -> Ordering {
        self.node(a).sibling_key.cmp(&self.node(b).sibling_key)
    }

    /// Compares the strength of any two nodes. OpenUSD:
    /// `PcpCompareNodeStrength`.
    pub(crate) fn cmp_nodes(&self, paths: &PathInterner, a: NodeIndex, b: NodeIndex) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let chain = |mut n: NodeIndex| {
            let mut out = Vec::new();
            while n != NONE {
                out.push(n);
                n = self.node(n).parent;
            }
            out.reverse();
            out
        };
        let (ca, cb) = (chain(a), chain(b));
        match ca.iter().zip(&cb).find(|(x, y)| x != y) {
            Some((x, y)) => self.cmp_siblings(paths, *x, *y),
            None => ca.len().cmp(&cb.len()),
        }
    }

    /// Marks `node` and every node beneath it inert.
    pub(crate) fn inert_subtree(&mut self, paths: &PathInterner, node: NodeIndex) {
        for n in self.subtree(node) {
            self.set_inert(paths, n, true);
        }
    }

    /// Marks `node` inert (or not), recording the namespace depth at which
    /// it stopped contributing specs. OpenUSD: `PcpNodeRef::SetInert`.
    pub(crate) fn set_inert(&mut self, paths: &PathInterner, node: NodeIndex, inert: bool) {
        if self.node(node).inert == inert {
            return;
        }
        self.node_mut(node).inert = inert;
        self.record_restriction_depth(paths, node, inert);
    }

    /// Marks `node` culled, recording the restriction depth as
    /// [`Self::set_inert`] does. OpenUSD: `PcpNodeRef::SetCulled`.
    pub(crate) fn set_culled(&mut self, paths: &PathInterner, node: NodeIndex) {
        if self.node(node).culled {
            return;
        }
        self.record_restriction_depth(paths, node, true);
        self.node_mut(node).culled = true;
    }

    /// OpenUSD: `PcpNodeRef::_RecordRestrictionDepth`.
    fn record_restriction_depth(
        &mut self,
        paths: &PathInterner,
        node: NodeIndex,
        restricted: bool,
    ) {
        let restricted = restricted || !self.node(node).can_contribute_specs();
        let entry = self.node(node);
        let depth = if !restricted {
            0
        } else if entry.restricted_depth == 0 {
            let count = paths.resolve(entry.site.path.prim_path()).depth()
                + entry.site.path.selection_entries().len();
            u16::try_from(count).unwrap_or(u16::MAX)
        } else {
            entry.restricted_depth
        };
        self.node_mut(node).restricted_depth = depth;
    }

    /// The nodes in strength order: a node before the nodes beneath it,
    /// children strongest first.
    pub(crate) fn strength_order(&self) -> Vec<NodeIndex> {
        self.subtree(0)
    }

    /// Whether the arena holds the nodes in [`Self::strength_order`].
    fn in_strength_order(&self) -> bool {
        match self.nodes.len() {
            // An empty arena, or the root alone.
            0 | 1 => true,
            count => self.strength_order().into_iter().eq(0..node_index(count)),
        }
    }

    /// Reorders the arena into strength order and removes culled nodes
    /// that are not origins of kept nodes. OpenUSD:
    /// `PcpPrimIndex_Graph::Finalize`.
    pub(crate) fn finalize(&mut self) {
        // Already in strength order with nothing to erase, as most indexes
        // are (every one without arcs): relinking would rebuild the links
        // the order was read from.
        // A finished index adds no nodes: its lookup would only hold
        // memory for as long as the index is kept.
        self.lookup = None;
        if self.nodes.iter().all(|node| !node.culled) && self.in_strength_order() {
            return;
        }
        let order = self.strength_order();
        let count = self.nodes.len();
        let mut erase: Vec<bool> = self.nodes.iter().map(|node| node.culled).collect();
        // A culled origin of a kept node, and its parents, stay.
        for at in 0..count {
            if self.nodes[at].origin == NONE {
                continue;
            }
            self.keep_origins(node_index(at), &mut erase);
        }
        let mut remap = alloc::vec![NONE; count];
        let mut next = 0;
        for &at in &order {
            if !erase[at as usize] {
                remap[at as usize] = next;
                next += 1;
            }
        }
        let map = |index: NodeIndex| {
            if index == NONE {
                NONE
            } else {
                remap[index as usize]
            }
        };
        let mut nodes: Vec<Option<Node>> = core::mem::take(&mut self.nodes)
            .into_iter()
            .map(Some)
            .collect();
        let mut out = Vec::with_capacity(next as usize);
        for &at in &order {
            if erase[at as usize] {
                continue;
            }
            let mut node = nodes[at as usize].take().expect("each node once");
            node.parent = map(node.parent);
            node.origin = map(node.origin);
            out.push(node);
        }
        // Relink children from the kept parents, in strength order.
        for node in &mut out {
            node.first_child = NONE;
            node.last_child = NONE;
            node.prev_sibling = NONE;
            node.next_sibling = NONE;
        }
        for at in 1..out.len() {
            let parent = out[at].parent as usize;
            let last = out[parent].last_child;
            let index = node_index(at);
            if last == NONE {
                out[parent].first_child = index;
            } else {
                out[last as usize].next_sibling = index;
                out[at].prev_sibling = last;
            }
            out[parent].last_child = index;
        }
        self.nodes = out;
        // The relinked children keep the keys of their old places.
        for at in 0..self.nodes.len() {
            let index = node_index(at);
            if self.node(index).first_child != NONE {
                self.renumber_siblings(index);
            }
        }
    }

    /// OpenUSD: `processOriginChain` in `_ComputeEraseCulledNodeIndexMapping`.
    fn keep_origins(&self, start: NodeIndex, erase: &mut [bool]) {
        let mut n = start;
        let mut keep_rest = false;
        loop {
            let culled = erase[n as usize];
            if !culled {
                keep_rest = true;
            } else if keep_rest {
                let mut p = n;
                while p != NONE && erase[p as usize] {
                    erase[p as usize] = false;
                    if p != n {
                        self.keep_origins(p, erase);
                    }
                    p = self.node(p).parent;
                }
            }
            let node = self.node(n);
            if node.origin == node.parent || node.origin == NONE {
                break;
            }
            n = node.origin;
        }
    }

    /// Appends `name` to every node's site, for the index of the child
    /// prim `name`. OpenUSD: `AppendChildNameToAllSites`.
    pub(crate) fn append_child_name(
        &mut self,
        paths: &PathInterner,
        name: crate::interner::TokenId,
    ) {
        for node in &mut self.nodes {
            let child = paths.intern_joined(node.site.path.prim_path(), &[name]);
            node.site.path = node.site.path.child(child);
        }
        if self.lookup.is_some() {
            self.lookup = Some(NodeLookup::new(&self.nodes));
        }
    }

    /// The node of this graph using `site`, if any, that is neither inert
    /// nor culled. OpenUSD: `GetNodeUsingSite`.
    pub(crate) fn node_using_site(&self, site: &Site) -> Option<NodeIndex> {
        let contributes = |node: &Node| !(node.inert || node.culled) && node.site == *site;
        match &self.lookup {
            Some(lookup) => lookup
                .by_site
                .get(site)?
                .iter()
                .copied()
                .find(|&index| contributes(self.node(index))),
            None => self.nodes.iter().position(contributes).map(node_index),
        }
    }

    /// The strongest child of `parent` at `site`, if any.
    pub(crate) fn child_using_site(&self, parent: NodeIndex, site: &Site) -> Option<NodeIndex> {
        match &self.lookup {
            Some(lookup) => self.strongest_child(parent, lookup.by_site.get(site)?),
            None => self
                .children(parent)
                .find(|&child| self.node(child).site == *site),
        }
    }

    /// The strongest child of `parent` whose origin is `origin` and that
    /// `accept` takes.
    pub(crate) fn strongest_child_from(
        &self,
        parent: NodeIndex,
        origin: NodeIndex,
        accept: impl Fn(NodeIndex) -> bool,
    ) -> Option<NodeIndex> {
        match &self.lookup {
            Some(lookup) => lookup
                .by_origin
                .get(&origin)?
                .iter()
                .copied()
                .filter(|&node| self.is_child_of(node, parent) && accept(node))
                .min_by(|&a, &b| self.sibling_order(a, b)),
            None => self
                .children(parent)
                .find(|&child| self.node(child).origin == origin && accept(child)),
        }
    }

    /// The weakest child of `parent` whose origin is `origin` and that
    /// `accept` takes.
    pub(crate) fn weakest_child_from(
        &self,
        parent: NodeIndex,
        origin: NodeIndex,
        accept: impl Fn(NodeIndex) -> bool,
    ) -> Option<NodeIndex> {
        match &self.lookup {
            Some(lookup) => lookup
                .by_origin
                .get(&origin)?
                .iter()
                .copied()
                .filter(|&node| self.is_child_of(node, parent) && accept(node))
                .max_by(|&a, &b| self.sibling_order(a, b)),
            None => {
                let mut child = self.node(parent).last_child;
                while child != NONE {
                    if self.node(child).origin == origin && accept(child) {
                        return Some(child);
                    }
                    child = self.node(child).prev_sibling;
                }
                None
            }
        }
    }

    /// Whether `node` is linked among `parent`'s children.
    fn is_child_of(&self, node: NodeIndex, parent: NodeIndex) -> bool {
        let entry = self.node(node);
        entry.parent == parent
            && (entry.prev_sibling != NONE || self.node(parent).first_child == node)
    }

    /// The strongest of the `nodes` linked among `parent`'s children.
    fn strongest_child(&self, parent: NodeIndex, nodes: &[NodeIndex]) -> Option<NodeIndex> {
        nodes
            .iter()
            .copied()
            .filter(|&node| self.is_child_of(node, parent))
            .min_by(|&a, &b| self.sibling_order(a, b))
    }
}

/// The arc that connects an inserted subgraph's root to its new parent.
pub(crate) struct SubgraphArc {
    pub(crate) arc_kind: ArcKind,
    pub(crate) origin: NodeIndex,
    pub(crate) map_to_parent: MapFunction,
    pub(crate) namespace_depth: u16,
    pub(crate) sibling_num: u16,
}

/// Iterates a node's children, strongest first.
pub(crate) struct Children<'a> {
    graph: &'a Graph,
    next: NodeIndex,
}

impl Iterator for Children<'_> {
    type Item = NodeIndex;

    fn next(&mut self) -> Option<NodeIndex> {
        if self.next == NONE {
            return None;
        }
        let current = self.next;
        self.next = self.graph.node(current).next_sibling;
        Some(current)
    }
}

/// `path` with its last `levels` prim names removed, each with the
/// variant selections that follow it. OpenUSD: `_GetPathAtIntroDepth`.
pub(crate) fn truncated(paths: &PathInterner, path: &SpecPath, levels: u16) -> SpecPath {
    if levels == 0 {
        return path.clone();
    }
    let segments = paths.resolve(path.prim_path()).segments();
    let keep = segments.len().saturating_sub(levels as usize);
    let prefix: crate::path::Segments = segments[..keep].into();
    let prim = paths.intern_segments(&prefix);
    path.truncated_to(prim, keep)
}

#[cfg(test)]
mod tests;
