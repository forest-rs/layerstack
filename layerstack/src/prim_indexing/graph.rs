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

use crate::{path::PathInterner, prim_index::ArcKind, spec_path::SpecPath};

use super::{layer_stacks::StackId, map_function::MapFunction};

/// Index of a node in its [`Graph`].
pub(crate) type NodeIndex = u32;

/// No node.
pub(crate) const NONE: NodeIndex = NodeIndex::MAX;

/// A node's children, collected ([`Graph::child_list`]).
pub(crate) type ChildList = smallvec::SmallVec<[NodeIndex; 8]>;

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
#[derive(Clone, Debug, Default)]
pub(crate) struct Graph {
    pub(crate) nodes: Vec<Node>,
    /// Whether the prim this graph indexes introduces a payload.
    pub(crate) has_payloads: bool,
    /// Whether the prim is an instance.
    pub(crate) instanceable: bool,
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
        let mut out = Vec::new();
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
        let mut at = first;
        while at != NONE {
            if self.cmp_siblings(paths, child, at).is_lt() {
                let prev = self.node(at).prev_sibling;
                self.node_mut(child).prev_sibling = prev;
                self.node_mut(child).next_sibling = at;
                self.node_mut(at).prev_sibling = child;
                self.node_mut(prev).next_sibling = child;
                return;
            }
            at = self.node(at).next_sibling;
        }
    }

    /// Appends a new node to the arena (not yet linked to a parent).
    pub(crate) fn push_node(&mut self, node: Node) -> NodeIndex {
        let index = NodeIndex::try_from(self.nodes.len()).expect("prim index node count");
        self.nodes.push(node);
        index
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
                let order = self.origin_is_stronger(0, na.origin, nb.origin);
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
            return Some(self.origin_is_stronger(0, root_a, root_b));
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
        Some(self.origin_is_stronger(0, origin_a, origin_b))
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

    /// Which of `a` and `b` a strong-to-weak walk from `start` reaches
    /// first: `Less` for `a`. OpenUSD: `_OriginIsStronger`.
    pub(crate) fn origin_is_stronger(
        &self,
        start: NodeIndex,
        a: NodeIndex,
        b: NodeIndex,
    ) -> Ordering {
        if start == a {
            return Ordering::Less;
        }
        if start == b {
            return Ordering::Greater;
        }
        let mut child = self.node(start).first_child;
        while child != NONE {
            let order = self.origin_is_stronger(child, a, b);
            if order.is_ne() {
                return order;
            }
            child = self.node(child).next_sibling;
        }
        Ordering::Equal
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

    /// Reorders the arena into strength order and removes culled nodes
    /// that are not origins of kept nodes. OpenUSD:
    /// `PcpPrimIndex_Graph::Finalize`.
    pub(crate) fn finalize(&mut self) {
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
    }

    /// The node of this graph using `site`, if any, that is neither inert
    /// nor culled. OpenUSD: `GetNodeUsingSite`.
    pub(crate) fn node_using_site(&self, site: &Site) -> Option<NodeIndex> {
        self.nodes
            .iter()
            .position(|node| !(node.inert || node.culled) && node.site == *site)
            .map(node_index)
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
