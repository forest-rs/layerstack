// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Per-prim composition graphs.
//!
//! Composition builds one [`PrimIndexGraph`] per composed prim: a tree of
//! [`PrimNode`]s, one per arc expansion that contributes to the prim. The
//! root node is the prim's own site in the stage's root layer stack; every
//! other node is a site reached through an arc from its parent node, and
//! records the arc's kind, the layer stack and site it targets, the
//! namespace depth at which it was introduced and its position among the
//! arcs authored beside it. Every opinion of the prim names the node it came
//! from ([`crate::OpinionKey::node`]).
//!
//! This is the counterpart of OpenUSD's `PcpPrimIndex` graph
//! (`pxr/usd/pcp/primIndex_Graph.h`, `pxr/usd/pcp/node.h`), whose nodes carry
//! the same fields (`PcpNodeRef::GetArcType`, `GetParentNode`,
//! `GetOriginNode`, `GetSiblingNumAtOrigin`, `GetNamespaceDepth`,
//! `GetLayerStack`, `GetPath`). AOUSD Core §10.4 defines strength order as a
//! walk of that structure: a node is stronger than its descendants, and
//! siblings are ordered by arc kind (LIVERPS), then by the other
//! tie-breakers (`pxr/usd/pcp/strengthOrdering.cpp`,
//! `PcpCompareSiblingNodeStrength`).
//!
//! # Strength order
//!
//! A prim's opinions are ranked by a strong-to-weak depth-first walk of its
//! graph ([`PrimIndexGraph::strength_order`]), then by layer strength within
//! each node's layer stack: a node is stronger than every node beneath it,
//! and a node's children are ordered by arc kind (LIVERPS), then deeper
//! namespace depth, then their position among the arcs authored beside them
//! (`PcpCompareSiblingNodeStrength`). The one exception is specializes:
//! each node records the specializes arcs above it, and nodes beneath a
//! specializes arc rank after every other node (AOUSD Core §10.4.1).
//!
//! # Toward precise invalidation
//!
//! A node's layer stack and site ([`PrimNode::layer_stack`],
//! [`PrimNode::site`]) name the specs whose value opinions the prim reads: an
//! edit to an opinion at that site, in a layer of that layer stack, affects
//! the prim. So the `(layer stack, site)` pairs of a prim's graph are its
//! precise dependencies for opinion edits.
//!
//! They are not all of its dependencies. The sites that introduce arcs and
//! select variants are separate: a reference authored on `/Grove` reaches
//! `/Grove/Leaves` as an ancestral arc, yet no node of `/Grove/Leaves` names
//! `/Grove`. [`crate::dependency_map`] tracks those dependencies until the
//! graph represents them, so inverting node sites alone does not replace it.
//!
//! Spec: AOUSD Core §10 (composition arcs), §10.4 (strength ordering).

use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use core::cmp::Ordering;

use crate::{
    doc::LayerId,
    prim_index::{ArcKind, OpinionKey},
    spec_path::SpecPath,
};

/// Identifies a node of a [`PrimIndexGraph`].
///
/// Node ids index the graph's node arena and are stable for the graph's
/// lifetime: composition only ever adds nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    /// The root node: the composed prim's own site in the stage's root layer
    /// stack.
    pub const ROOT: Self = Self(0);

    /// Returns the node's index in [`PrimIndexGraph::nodes`] order.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The node with index `raw`.
    #[cfg(test)]
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    fn from_index(index: usize) -> Self {
        Self(u32::try_from(index).expect("prim index graph node count overflow"))
    }
}

/// Where one specializes arc on a node's arc path is authored.
///
/// Opinions introduced by specializes arcs are globally weaker than every
/// other opinion of the prim, including opinions of other references and
/// payloads, and include the opinions of arcs authored inside the
/// specialized prim (AOUSD Core §10.4.1). OpenUSD implements this by leaving
/// an inert placeholder where the arc is authored and propagating the
/// specializes node to the root of the prim index, where it ranks after
/// every other arc (`pxr/usd/pcp/primIndex.cpp`, `_EvalImpliedSpecializes`;
/// `pxr/usd/pcp/strengthOrdering.cpp`, `PcpCompareSiblingNodeStrength`).
///
/// An origin identifies one such propagated node by the position of its
/// placeholder, summarized by the arcs that bring the placeholder's site
/// into the prim index, and by the specializes arc's own index in its
/// site's list.
///
// TODO(graph): SpecializesPlacement. Propagate specializes nodes to the
// root with their placeholder as origin, and rank them by walking the graph
// to that origin (`_OriginIsStronger`), instead of by this summary; then
// retire this type and [`NodeArc::specializes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpecializesOrigin {
    /// Namespace depth of the prim the specializes node is propagated at.
    ///
    /// Deeper is stronger, as for sibling nodes.
    pub(crate) namespace_depth: u16,
    /// Arc kind the placeholder ranks under: the outermost arc that brings
    /// the authoring site into the prim index, or [`ArcKind::Specializes`]
    /// for a specializes authored in the composed prim's own layer stack.
    pub(crate) arc_kind: ArcKind,
    /// The first arc kind nested inside [`Self::arc_kind`] on the way to the
    /// placeholder, if any. A placeholder directly under an arc target is
    /// nested as [`ArcKind::Specializes`], so it ranks after the other arcs
    /// of that target, as OpenUSD orders a node's children.
    pub(crate) nested_arc_kind: Option<ArcKind>,
    /// Index of the outermost arc in its arc list.
    pub(crate) arc_list_index: u16,
    /// Index of the specializes arc in the authoring site's specializes list.
    pub(crate) specializes_index: u16,
    /// `true` when the specialized path is mapped into the namespace of the
    /// arc that introduces the authoring site (an implied specializes), as
    /// opposed to the propagated arc itself. OpenUSD ranks the implied node
    /// first (`PcpCompareSiblingNodeStrength`).
    pub(crate) implied: bool,
}

impl SpecializesOrigin {
    /// Compares origins with "strongest first" ordering.
    pub(crate) fn cmp_strongest_first(&self, other: &Self) -> Ordering {
        other
            .namespace_depth
            .cmp(&self.namespace_depth)
            .then_with(|| {
                self.arc_kind
                    .strength_rank()
                    .cmp(&other.arc_kind.strength_rank())
            })
            .then_with(|| cmp_nested_arc_kind(self.nested_arc_kind, other.nested_arc_kind))
            .then_with(|| self.arc_list_index.cmp(&other.arc_list_index))
            .then_with(|| self.specializes_index.cmp(&other.specializes_index))
            .then_with(|| other.implied.cmp(&self.implied))
    }
}

/// Orders nested arc kinds: no nesting is strongest, then LIVERPS order.
fn cmp_nested_arc_kind(a: Option<ArcKind>, b: Option<ArcKind>) -> Ordering {
    match (a, b) {
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.strength_rank().cmp(&b.strength_rank()),
        (None, None) => Ordering::Equal,
    }
}

/// Compares the specializes arcs above two nodes with "strongest first"
/// ordering.
///
/// A node that no specializes arc introduces outranks every specializes
/// node. Two chains are ordered by their first differing origin, outermost
/// first, as OpenUSD orders sibling specializes nodes by their originating
/// nodes (`PcpCompareSiblingNodeStrength`). A chain that extends another is
/// a node nested in that node's specialized prim and ranks right after it,
/// before the enclosing node's weaker siblings.
///
/// Spec: AOUSD Core §10.4.1.
fn cmp_specializes(a: &[SpecializesOrigin], b: &[SpecializesOrigin]) -> Ordering {
    a.iter()
        .zip(b)
        .map(|(a, b)| a.cmp_strongest_first(b))
        .find(|ordering| ordering.is_ne())
        .unwrap_or_else(|| a.len().cmp(&b.len()))
}

/// The arc that introduces a node, and the site it reaches.
///
/// Two expansions that agree on all of this share a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeArc {
    /// The kind of arc from the parent node to this one; [`ArcKind::Local`]
    /// for the root.
    pub(crate) arc_kind: ArcKind,
    /// Root layer of the layer stack the site is in.
    pub(crate) layer_stack: LayerId,
    /// The site's prim spec path in that layer stack, with the variant
    /// selections of a variant node.
    pub(crate) site: SpecPath,
    /// Namespace depth of the prim the arc is authored on.
    pub(crate) namespace_depth: u16,
    /// Position of the arc among the arcs of its kind authored beside it.
    pub(crate) sibling_index: u16,
    /// `true` for an implied class arc (see [`PrimNode::is_implied`]).
    pub(crate) implied: bool,
    /// `true` for a node copied from another prim's graph by the late copy
    /// of an arc target's composed sources.
    pub(crate) copied: bool,
    /// The specializes arcs on the node's arc path, outermost first; empty
    /// for a node that no specializes arc introduces.
    ///
    /// Otherwise the node belongs to the specializes node the last origin
    /// names, and ranks after every node outside it (see
    /// [`SpecializesOrigin`]).
    ///
    /// Spec: AOUSD Core §10.4.1.
    pub(crate) specializes: Box<[SpecializesOrigin]>,
}

/// One node of a [`PrimIndexGraph`]: a site that contributes opinions to a
/// composed prim, and the arc that reaches it.
///
/// Spec: AOUSD Core §10.4. OpenUSD: `PcpNodeRef` (`pxr/usd/pcp/node.h`).
#[derive(Clone, Debug)]
pub struct PrimNode {
    parent: Option<NodeId>,
    origin: Option<NodeId>,
    children: Vec<NodeId>,
    pub(crate) arc: NodeArc,
}

impl PrimNode {
    /// The kind of arc from [`Self::parent`] to this node;
    /// [`ArcKind::Local`] for the root.
    #[must_use]
    pub fn arc_kind(&self) -> ArcKind {
        self.arc.arc_kind
    }

    /// The node whose site authors the arc to this node; `None` for the
    /// root.
    #[must_use]
    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    /// The node this one was implied or propagated from, when it is a copy
    /// of another node (OpenUSD's `PcpNodeRef::GetOriginNode`).
    ///
    /// Composition does not copy nodes that way yet, so this is `None` for
    /// every node.
    // TODO(graph): ImpliedClasses, SpecializesPlacement. Set the origin of
    // implied class nodes and of specializes nodes propagated to the root.
    #[must_use]
    pub fn origin(&self) -> Option<NodeId> {
        self.origin
    }

    /// The nodes reached through arcs authored at this node's site, in the
    /// order they were added.
    #[must_use]
    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    /// The root layer of the layer stack this node's site is in.
    ///
    /// The node reads every layer of that stack: the root layer and its
    /// sublayers, as [`crate::LayerStack::gather`] composes them. A node
    /// reached through an internal reference or payload is in the layer stack
    /// of its parent node, whichever layer of that stack authors the arc.
    ///
    /// OpenUSD: `PcpNodeRef::GetLayerStack`.
    #[must_use]
    pub fn layer_stack(&self) -> LayerId {
        self.arc.layer_stack
    }

    /// The prim spec path this node reads in its layer stack, including the
    /// variant selections of a variant node (`/Model{shape=round}`).
    #[must_use]
    pub fn site(&self) -> &SpecPath {
        &self.arc.site
    }

    /// Namespace depth of the prim the arc to this node is authored on:
    /// `1` for an arc authored on `/A`, `2` on `/A/B`.
    ///
    /// OpenUSD: `PcpNodeRef::GetNamespaceDepth`.
    #[must_use]
    pub fn namespace_depth(&self) -> u16 {
        self.arc.namespace_depth
    }

    /// Position of the arc to this node among the arcs of its kind authored
    /// beside it (the Nth reference of a list).
    ///
    /// OpenUSD: `PcpNodeRef::GetSiblingNumAtOrigin`.
    #[must_use]
    pub fn sibling_index(&self) -> u16 {
        self.arc.sibling_index
    }

    /// `true` for an implied class arc: an inherits or specializes arc
    /// authored inside another arc's target, whose class path is mapped into
    /// the namespace (and, for references and payloads, the layer stack)
    /// of the site that arc is authored at.
    ///
    /// Spec: AOUSD Core §10.4.2.4 (implied class arcs).
    #[must_use]
    pub fn is_implied(&self) -> bool {
        self.arc.implied
    }
}

/// The composition graph of one composed prim.
///
/// See the [module docs](self) for how it relates to strength order.
#[derive(Clone, Debug, Default)]
pub struct PrimIndexGraph {
    nodes: Vec<PrimNode>,
    /// Each node's position in the strength walk, once ranked: equal for
    /// nodes whose opinions interleave (see [`Self::rank`]). Empty or stale
    /// while composition adds nodes.
    ranks: Vec<u32>,
}

impl PrimIndexGraph {
    /// A graph holding only a root node reached by `arc`.
    pub(crate) fn new(arc: NodeArc) -> Self {
        Self {
            nodes: alloc::vec![PrimNode {
                parent: None,
                origin: None,
                children: Vec::new(),
                arc,
            }],
            ranks: Vec::new(),
        }
    }

    /// Returns the child of `parent` reached by `arc`, adding it if needed.
    pub(crate) fn intern_child(&mut self, parent: NodeId, arc: NodeArc) -> NodeId {
        if let Some(existing) = self.nodes[parent.index()]
            .children
            .iter()
            .copied()
            .find(|child| self.nodes[child.index()].arc == arc)
        {
            return existing;
        }
        let id = NodeId::from_index(self.nodes.len());
        self.nodes.push(PrimNode {
            parent: Some(parent),
            origin: None,
            children: Vec::new(),
            arc,
        });
        self.nodes[parent.index()].children.push(id);
        id
    }

    /// The specializes arcs above `node`, outermost first.
    pub(crate) fn specializes(&self, node: NodeId) -> &[SpecializesOrigin] {
        &self.nodes[node.index()].arc.specializes
    }

    /// The nodes from beneath the root down to `node`, outermost first:
    /// the arc path of `node`, empty for the root.
    pub(crate) fn arc_path(&self, node: NodeId) -> Vec<&PrimNode> {
        let mut path = Vec::new();
        let mut cursor = &self.nodes[node.index()];
        while let Some(parent) = cursor.parent {
            path.push(cursor);
            cursor = &self.nodes[parent.index()];
        }
        path.reverse();
        path
    }

    /// Compares two children of one node with "strongest first" ordering:
    /// by arc kind (LIVERPS), then deeper namespace depth, then their
    /// position among the arcs authored beside them.
    ///
    /// Siblings that compare equal (the implied and authored copies of one
    /// class arc, or the branches of different variant sets) rank as one
    /// node, and their opinions interleave by layer strength.
    ///
    /// Spec: AOUSD Core §10.4. OpenUSD: `PcpCompareSiblingNodeStrength` in
    /// `pxr/usd/pcp/strengthOrdering.cpp`.
    // TODO(graph): ImpliedClasses. Implied class nodes belong under the node
    // of the layer stack they are implied into, and variant nodes carry
    // their variant set's position; then no two siblings compare equal.
    fn cmp_siblings(&self, a: NodeId, b: NodeId) -> Ordering {
        let (a, b) = (&self.nodes[a.index()].arc, &self.nodes[b.index()].arc);
        a.arc_kind
            .strength_rank()
            .cmp(&b.arc_kind.strength_rank())
            .then_with(|| b.namespace_depth.cmp(&a.namespace_depth))
            .then_with(|| a.sibling_index.cmp(&b.sibling_index))
    }

    /// Ranks every node by a strong-to-weak depth-first walk of the graph.
    ///
    /// A node ranks above every node beneath it, and each node's children
    /// rank in [`Self::cmp_siblings`] order, so the nodes beneath a child
    /// rank before that child's weaker siblings. Siblings that compare equal
    /// share a rank, and their children are walked as one sibling set.
    ///
    /// Composition calls this once the graph is complete, and before
    /// comparing opinions while it still adds nodes.
    ///
    /// Spec: AOUSD Core §10.4. OpenUSD: `PcpCompareNodeStrength` in
    /// `pxr/usd/pcp/strengthOrdering.cpp`.
    pub(crate) fn rank(&mut self) {
        if self.ranks.len() != self.nodes.len() {
            self.ranks = self.walk_ranks();
        }
    }

    /// The rank of every node in the strength walk (see [`Self::rank`]).
    fn walk_ranks(&self) -> Vec<u32> {
        let mut ranks = alloc::vec![0; self.nodes.len()];
        let mut next = 0_u32;
        let mut stack: Vec<Vec<NodeId>> = self
            .root()
            .map(|root| alloc::vec![root])
            .into_iter()
            .collect();
        while let Some(group) = stack.pop() {
            let mut children = Vec::new();
            for id in &group {
                ranks[id.index()] = next;
                children.extend_from_slice(&self.nodes[id.index()].children);
            }
            next += 1;
            children.sort_by(|a, b| self.cmp_siblings(*a, *b));
            let groups: Vec<Vec<NodeId>> = children
                .chunk_by(|a, b| self.cmp_siblings(*a, *b).is_eq())
                .map(<[NodeId]>::to_vec)
                .collect();
            stack.extend(groups.into_iter().rev());
        }
        ranks
    }

    /// Sorts `keys` strongest first, as [`Self::cmp_keys`] orders them,
    /// whether or not the graph is ranked.
    pub(crate) fn sort_keys(&self, keys: &mut [OpinionKey]) {
        let ranks = if self.ranks.len() == self.nodes.len() {
            Cow::Borrowed(self.ranks.as_slice())
        } else {
            Cow::Owned(self.walk_ranks())
        };
        keys.sort_by(|a, b| self.cmp_keys_by(&ranks, a, b));
    }

    /// Compares two nodes' opinions with "strongest first" ordering: nodes
    /// beneath a specializes arc after every other node (see
    /// [`SpecializesOrigin`]), then by their rank in the strength walk.
    ///
    /// Needs the graph to be ranked ([`Self::rank`]).
    pub(crate) fn cmp_nodes(&self, a: NodeId, b: NodeId) -> Ordering {
        debug_assert_eq!(self.ranks.len(), self.nodes.len(), "graph is ranked");
        self.cmp_nodes_by(&self.ranks, a, b)
    }

    fn cmp_nodes_by(&self, ranks: &[u32], a: NodeId, b: NodeId) -> Ordering {
        cmp_specializes(self.specializes(a), self.specializes(b))
            .then_with(|| ranks[a.index()].cmp(&ranks[b.index()]))
    }

    /// Compares two opinions of this prim with "strongest first" ordering:
    /// by their nodes ([`Self::cmp_nodes`]), then by layer strength within
    /// the layer stack, then by stable ids.
    ///
    /// Needs the graph to be ranked ([`Self::rank`]).
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering and tie-breakers).
    pub(crate) fn cmp_keys(&self, a: &OpinionKey, b: &OpinionKey) -> Ordering {
        debug_assert_eq!(self.ranks.len(), self.nodes.len(), "graph is ranked");
        self.cmp_keys_by(&self.ranks, a, b)
    }

    fn cmp_keys_by(&self, ranks: &[u32], a: &OpinionKey, b: &OpinionKey) -> Ordering {
        self.cmp_nodes_by(ranks, a.node, b.node)
            .then_with(|| a.layer_strength.cmp(&b.layer_strength))
            .then_with(|| a.layer_id.cmp(&b.layer_id))
            .then_with(|| a.spec_path.cmp(&b.spec_path))
    }

    /// The root node, the composed prim's own site; `None` only for an empty
    /// graph.
    #[must_use]
    pub fn root(&self) -> Option<NodeId> {
        (!self.nodes.is_empty()).then_some(NodeId::ROOT)
    }

    /// Returns the node `id`, if it is a node of this graph.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&PrimNode> {
        self.nodes.get(id.index())
    }

    /// Returns the number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` when the graph has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Iterates over every node with its id, in the order nodes were added.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &PrimNode)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (NodeId::from_index(index), node))
    }

    /// Returns every node in the order the prim's opinions are ranked:
    /// strongest first, ties in the order nodes were added.
    ///
    /// Opinions of nodes that rank equal interleave by layer strength.
    #[must_use]
    pub fn strength_order(&self) -> Vec<NodeId> {
        let mut order: Vec<NodeId> = self.nodes().map(|(id, _)| id).collect();
        order.sort_by(|a, b| self.cmp_nodes(*a, *b));
        order
    }

    /// Returns every node in depth-first preorder from the root, each node's
    /// children strongest first.
    ///
    /// This is [`Self::strength_order`] except where specializes arcs, which
    /// rank after every other node, are authored.
    #[must_use]
    pub fn depth_first(&self) -> Vec<NodeId> {
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeId> = self.root().into_iter().collect();
        while let Some(id) = stack.pop() {
            order.push(id);
            let mut children = self.nodes[id.index()].children.clone();
            children.sort_by(|a, b| self.cmp_siblings(*a, *b));
            stack.extend(children.into_iter().rev());
        }
        order
    }
}

#[cfg(test)]
impl PrimIndexGraph {
    /// A ranked graph whose root, at `site`, is at `namespace_depth`, with
    /// one node per entry of `arcs`, `(parent, arc kind, namespace depth)`,
    /// all at `site`. Node ids follow `arcs`, starting at 1.
    pub(crate) fn from_arcs(
        site: &SpecPath,
        namespace_depth: u16,
        arcs: impl IntoIterator<Item = (NodeId, ArcKind, u16)>,
    ) -> Self {
        let arc = |arc_kind, namespace_depth| NodeArc {
            arc_kind,
            layer_stack: LayerId(1),
            site: site.clone(),
            namespace_depth,
            sibling_index: 0,
            implied: false,
            copied: false,
            specializes: Box::default(),
        };
        let mut graph = Self::new(arc(ArcKind::Local, namespace_depth));
        for (parent, arc_kind, namespace_depth) in arcs {
            let id = NodeId::from_index(graph.nodes.len());
            graph.nodes.push(PrimNode {
                parent: Some(parent),
                origin: None,
                children: Vec::new(),
                arc: arc(arc_kind, namespace_depth),
            });
            graph.nodes[parent.index()].children.push(id);
        }
        graph.rank();
        graph
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        interner::TokenInterner,
        path::{Path, PathId, PathInterner},
    };
    use alloc::{vec, vec::Vec};

    fn spec_path(text: &str) -> SpecPath {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let path = paths.intern(Path::parse_absolute(text, &mut tokens).expect("path"));
        SpecPath::from_prim_path(path, &paths)
    }

    fn arc(arc_kind: ArcKind, namespace_depth: u16, sibling_index: u16) -> NodeArc {
        NodeArc {
            arc_kind,
            layer_stack: LayerId(1),
            site: spec_path("/A"),
            namespace_depth,
            sibling_index,
            implied: false,
            copied: false,
            specializes: Box::default(),
        }
    }

    /// A graph with a root and the nodes `arcs`, each `(parent, arc)`,
    /// ranked. Node ids follow `arcs`, starting at 1.
    fn graph(arcs: Vec<(u32, NodeArc)>) -> PrimIndexGraph {
        let mut graph = PrimIndexGraph::new(arc(ArcKind::Local, 1, 0));
        for (parent, arc) in arcs {
            graph.intern_child(NodeId(parent), arc);
        }
        graph.rank();
        graph
    }

    fn assert_order(graph: &PrimIndexGraph, order: &[u32]) {
        for pair in order.windows(2) {
            let (a, b) = (NodeId(pair[0]), NodeId(pair[1]));
            assert_eq!(graph.cmp_nodes(a, b), Ordering::Less, "{a:?} < {b:?}");
            assert_eq!(graph.cmp_nodes(b, a), Ordering::Greater, "{b:?} > {a:?}");
        }
    }

    #[test]
    fn siblings_follow_liverps_then_depth_then_list_order() {
        // Spec: AOUSD Core §10.4. Local opinions first, then arcs in LIVERPS
        // order; a deeper arc wins a tie; then the earlier arc of a list.
        let graph = graph(vec![
            (0, arc(ArcKind::Payloads, 1, 0)),
            (0, arc(ArcKind::References, 1, 1)),
            (0, arc(ArcKind::References, 1, 0)),
            (0, arc(ArcKind::References, 2, 3)),
            (0, arc(ArcKind::Variants, 1, 0)),
            (0, arc(ArcKind::Inherits, 1, 0)),
        ]);
        assert_order(&graph, &[0, 6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn a_node_ranks_its_descendants_before_its_weaker_siblings() {
        // Spec: AOUSD Core §10.4. A reference authored inside a payload
        // inside a reference ranks with that reference, before the prim's
        // second reference, and after the payload's own site.
        let graph = graph(vec![
            (0, arc(ArcKind::References, 1, 0)),
            (0, arc(ArcKind::References, 1, 1)),
            (1, arc(ArcKind::Payloads, 1, 0)),
            (3, arc(ArcKind::References, 1, 0)),
            (1, arc(ArcKind::Inherits, 1, 0)),
        ]);
        assert_order(&graph, &[0, 1, 5, 3, 4, 2]);
    }

    #[test]
    fn equal_siblings_share_a_rank_and_their_children() {
        let implied = NodeArc {
            implied: true,
            ..arc(ArcKind::Inherits, 1, 0)
        };
        let graph = graph(vec![
            (0, arc(ArcKind::Inherits, 1, 0)),
            (0, implied),
            (2, arc(ArcKind::References, 1, 0)),
            (1, arc(ArcKind::Inherits, 1, 0)),
        ]);
        assert_eq!(graph.cmp_nodes(NodeId(1), NodeId(2)), Ordering::Equal);
        assert_order(&graph, &[0, 1, 4, 3]);
        assert_order(&graph, &[2, 4]);
    }

    fn key(node: NodeId, layer_strength: u16, layer_id: u64, spec_path: SpecPath) -> OpinionKey {
        OpinionKey {
            node,
            layer_strength,
            layer_id: LayerId(layer_id),
            lookup_path: PathId::from_raw(0),
            spec_path,
        }
    }

    #[test]
    fn opinions_rank_by_node_then_layer_then_stable_ids() {
        // Spec: AOUSD Core §10.4. Layer stack order breaks ties within a
        // node; stable ids break the remaining ties.
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let mut path = |text: &str| {
            let path = paths.intern(Path::parse_absolute(text, &mut tokens).expect("path"));
            SpecPath::from_prim_path(path, &paths)
        };
        let (a, b) = (path("/A"), path("/B"));
        let graph = graph(vec![(0, arc(ArcKind::Variants, 1, 0))]);
        let variant = NodeId(1);
        let order = [
            key(NodeId::ROOT, 0, 1, a.clone()),
            key(NodeId::ROOT, 0, 1, b),
            key(NodeId::ROOT, 0, 2, a.clone()),
            key(NodeId::ROOT, 1, 0, a.clone()),
            key(variant, 0, 0, a),
        ];
        for pair in order.windows(2) {
            assert_eq!(graph.cmp_keys(&pair[0], &pair[1]), Ordering::Less);
            assert_eq!(graph.cmp_keys(&pair[1], &pair[0]), Ordering::Greater);
        }
    }

    fn origin(arc_kind: ArcKind, nested_arc_kind: Option<ArcKind>) -> SpecializesOrigin {
        SpecializesOrigin {
            namespace_depth: 1,
            arc_kind,
            nested_arc_kind,
            arc_list_index: 0,
            specializes_index: 0,
            implied: false,
        }
    }

    fn specialized(specializes: Vec<SpecializesOrigin>) -> NodeArc {
        NodeArc {
            specializes: specializes.into_boxed_slice(),
            ..arc(ArcKind::Specializes, 1, 0)
        }
    }

    #[test]
    fn traversals_visit_every_node_strongest_first() {
        let graph = graph(vec![
            (0, specialized(vec![origin(ArcKind::Specializes, None)])),
            (0, arc(ArcKind::Variants, 3, 0)),
            (0, arc(ArcKind::References, 1, 0)),
        ]);
        let kinds = |order: Vec<NodeId>| -> Vec<ArcKind> {
            order
                .into_iter()
                .map(|id| graph.node(id).expect("node").arc_kind())
                .collect()
        };
        let expected = [
            ArcKind::Local,
            ArcKind::Variants,
            ArcKind::References,
            ArcKind::Specializes,
        ];
        assert_eq!(kinds(graph.strength_order()), expected);
        assert_eq!(kinds(graph.depth_first()), expected);
    }

    #[test]
    fn specializes_reached_through_a_reference_are_weaker_than_payloads() {
        // Spec: AOUSD Core §10.4.1: a specializes is weaker than every other
        // arc, not only than the arc it is reached through.
        let graph = graph(vec![
            (0, arc(ArcKind::References, 1, 0)),
            (0, arc(ArcKind::Payloads, 1, 3)),
            (
                1,
                specialized(vec![origin(
                    ArcKind::References,
                    Some(ArcKind::Specializes),
                )]),
            ),
        ]);
        assert_order(&graph, &[0, 1, 2, 3]);
    }

    fn assert_chain_stronger(a: &[SpecializesOrigin], b: &[SpecializesOrigin]) {
        assert_eq!(cmp_specializes(a, b), Ordering::Less, "{a:?} < {b:?}");
        assert_eq!(cmp_specializes(b, a), Ordering::Greater, "{b:?} > {a:?}");
    }

    #[test]
    fn nested_specializes_nodes_are_weaker_than_their_enclosing_node() {
        let outer = origin(ArcKind::Specializes, None);
        let inner = SpecializesOrigin {
            namespace_depth: 2,
            ..origin(ArcKind::Specializes, Some(ArcKind::Specializes))
        };
        assert_chain_stronger(&[outer], &[outer, inner]);
    }

    #[test]
    fn nested_specializes_nodes_rank_before_weaker_siblings_of_their_node() {
        // `P` specializes `[A, B]` and `A` specializes `C`: `C` follows `A`,
        // before `B` (`PcpCompareSiblingNodeStrength`).
        let a = origin(ArcKind::Specializes, None);
        let b = SpecializesOrigin {
            arc_list_index: 1,
            specializes_index: 1,
            ..a
        };
        let c = origin(ArcKind::Specializes, Some(ArcKind::Specializes));
        assert_chain_stronger(&[a], &[a, c]);
        assert_chain_stronger(&[a, c], &[b]);
        assert_chain_stronger(&[a, c], &[b, c]);
    }

    #[test]
    fn specializes_nodes_follow_their_placeholders() {
        // A deeper node is stronger; then the placeholder's own rank, so a
        // specializes under a nested reference outranks one authored beside
        // that reference; then the implied node outranks the propagated one
        // (`PcpCompareSiblingNodeStrength`).
        let deep = SpecializesOrigin {
            namespace_depth: 2,
            ..origin(ArcKind::Specializes, None)
        };
        let beside = origin(ArcKind::References, Some(ArcKind::Specializes));
        let nested = origin(ArcKind::References, Some(ArcKind::References));
        let direct = origin(ArcKind::Specializes, None);
        let implied = SpecializesOrigin {
            implied: true,
            ..direct
        };
        let order = [deep, nested, beside, implied, direct];
        for pair in order.windows(2) {
            assert_chain_stronger(&[pair[0]], &[pair[1]]);
        }
    }
}
