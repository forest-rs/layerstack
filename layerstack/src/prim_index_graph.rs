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
//! The graph mirrors how composition expands arcs, but the prim's opinion
//! order is not a depth-first walk of it. Each node carries a flat strength
//! key (the outermost arc kind on its arc path, one nested arc kind, the
//! specializes arcs above it, and the remaining tie-breakers), and opinions
//! are ordered by their nodes' keys, then by layer strength within the
//! node's layer stack; opinions of nodes whose keys tie interleave.
//! [`PrimIndexGraph::strength_order`] lists nodes in that order;
//! [`PrimIndexGraph::depth_first`] walks the tree itself. The two differ
//! where Layerstack's order differs from OpenUSD's; the `TODO(graph)`
//! comments in composition name each such difference.
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

use alloc::vec::Vec;
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
/// placeholder, ranked the way a [`NodeStrength`] of the placeholder would
/// be, and by the specializes arc's own index in its site's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpecializesOrigin {
    /// Namespace depth of the prim the specializes node is propagated at.
    ///
    /// Deeper is stronger, as for [`NodeStrength::namespace_depth`].
    pub(crate) namespace_depth: u16,
    /// Arc kind the placeholder ranks under: the outermost arc that brings
    /// the authoring site into the prim index, or [`ArcKind::Specializes`]
    /// for a specializes authored in the composed prim's own layer stack.
    pub(crate) arc_kind: ArcKind,
    /// Nested arc kind the placeholder ranks under, as in
    /// [`NodeStrength::nested_arc_kind`]. A placeholder directly under an
    /// arc target is nested as [`ArcKind::Specializes`], so it ranks after
    /// the other arcs of that target, as OpenUSD orders a node's children.
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

/// The strength key a node's opinions are ranked by.
///
/// This is a flat summary of the node's arc path: the outermost arc kind,
/// one nested arc kind and the specializes arcs above the node, with the
/// namespace depth and list position of the arc that ranks it.
///
// TODO(graph): NestedArcDepth. Rank nodes by walking the graph
// (`PcpCompareNodeStrength` in `pxr/usd/pcp/strengthOrdering.cpp`) instead of
// by this summary, which holds one nested arc kind, so that arcs nested two
// or more deep, and the arcs of a nested target, rank beneath their own
// target; then retire this type.
///
/// Spec: AOUSD Core §10.4 (strength ordering and tie-breakers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeStrength {
    /// `true` for the root node's local opinions.
    pub(crate) is_local: bool,
    /// The specializes arcs on the node's arc path, outermost first.
    ///
    /// Empty for nodes that no specializes arc introduces. Otherwise the
    /// node belongs to the specializes node the last origin names, and the
    /// remaining fields rank it within that node: [`Self::arc_kind`] is
    /// [`ArcKind::Specializes`] and [`Self::nested_arc_kind`] is the arc
    /// inside the specialized prim that introduces it, if any.
    ///
    /// Chains are ordered by their first differing origin (see
    /// [`SpecializesOrigin`]); a chain that extends another ranks right after
    /// it, so a node that no specializes introduces outranks every
    /// specializes node, and a nested node follows only its own enclosing
    /// node.
    ///
    /// Spec: AOUSD Core §10.4.1.
    pub(crate) specializes: Vec<SpecializesOrigin>,
    /// The outermost arc kind on the node's arc path.
    pub(crate) arc_kind: ArcKind,
    /// The first arc kind nested inside the outermost arc, if any.
    ///
    /// Shorter arc chains are stronger than longer ones when the outer arc
    /// kind ties. Arcs nested more deeply keep the outermost arc kind in
    /// [`Self::arc_kind`] and the first nested arc kind here, so a reference
    /// authored inside referenced content is `(References, Some(References))`
    /// and stays weaker than the referenced site's own opinions.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering within an arc's target).
    pub(crate) nested_arc_kind: Option<ArcKind>,
    /// Namespace depth of the site where the ranking arc is introduced.
    ///
    /// For example, opinions introduced via a reference arc authored at `/A/B`
    /// are stronger than otherwise-identical opinions introduced at `/A`,
    /// regardless of which descendant prim paths they affect.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering tie-breakers).
    pub(crate) namespace_depth: u16,
    /// `true` for authored (vs implied) arcs.
    pub(crate) authored: bool,
    /// Index of the ranking arc within its arc list (e.g. the Nth reference).
    pub(crate) arc_list_index: u16,
}

impl NodeStrength {
    /// The strength of a prim's local opinions, at `namespace_depth`.
    pub(crate) fn local(namespace_depth: u16) -> Self {
        Self {
            is_local: true,
            specializes: Vec::new(),
            arc_kind: ArcKind::Local,
            nested_arc_kind: None,
            namespace_depth,
            authored: true,
            arc_list_index: 0,
        }
    }

    /// Compares strengths with "strongest first" ordering.
    pub(crate) fn cmp_strongest_first(&self, other: &Self) -> Ordering {
        match (self.is_local, other.is_local) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }

        // Specializes nodes rank after every other node. Two chains are
        // ordered by their first differing origin, outermost first, as
        // OpenUSD orders sibling specializes nodes by their originating nodes
        // (`PcpCompareSiblingNodeStrength` in
        // `pxr/usd/pcp/strengthOrdering.cpp`). A chain that extends another
        // is a node nested in that node's specialized prim and ranks right
        // after it, before the enclosing node's weaker siblings
        // (AOUSD Core §10.4.1).
        let specializes = self
            .specializes
            .iter()
            .zip(&other.specializes)
            .map(|(a, b)| a.cmp_strongest_first(b))
            .find(|ordering| ordering.is_ne())
            .unwrap_or_else(|| self.specializes.len().cmp(&other.specializes.len()));
        if specializes != Ordering::Equal {
            return specializes;
        }

        self.arc_kind
            .strength_rank()
            .cmp(&other.arc_kind.strength_rank())
            .then_with(|| cmp_nested_arc_kind(self.nested_arc_kind, other.nested_arc_kind))
            .then_with(|| other.namespace_depth.cmp(&self.namespace_depth))
            .then_with(|| other.authored.cmp(&self.authored))
            .then_with(|| self.arc_list_index.cmp(&other.arc_list_index))
    }
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
    /// The strength key the node's opinions are ranked by.
    pub(crate) strength: NodeStrength,
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

    /// Returns a child of `parent` reached by `arc`'s kind at `arc`'s site,
    /// whatever its strength, adding `arc` if there is none.
    ///
    /// For a node on the way to another: a node's strength ranks only its
    /// own opinions, so the node of a variant branch enclosing another is
    /// the same node whichever of its descendants is added first.
    pub(crate) fn intern_branch(&mut self, parent: NodeId, arc: NodeArc) -> NodeId {
        let same_site = |node: &PrimNode| {
            let other = &node.arc;
            other.arc_kind == arc.arc_kind
                && other.layer_stack == arc.layer_stack
                && other.site == arc.site
                && other.namespace_depth == arc.namespace_depth
                && other.sibling_index == arc.sibling_index
                && other.implied == arc.implied
        };
        if let Some(existing) = self.nodes[parent.index()]
            .children
            .iter()
            .copied()
            .find(|child| same_site(&self.nodes[child.index()]))
        {
            return existing;
        }
        self.intern_child(parent, arc)
    }

    /// The strength key of `node`'s opinions.
    pub(crate) fn strength(&self, node: NodeId) -> &NodeStrength {
        &self.nodes[node.index()].arc.strength
    }

    /// Compares two opinions of this prim with "strongest first" ordering:
    /// by their nodes' strength, then by layer strength within the layer
    /// stack, then by stable ids.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering and tie-breakers).
    pub(crate) fn cmp_keys(&self, a: &OpinionKey, b: &OpinionKey) -> Ordering {
        self.strength(a.node)
            .cmp_strongest_first(self.strength(b.node))
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
        order.sort_by(|a, b| self.strength(*a).cmp_strongest_first(self.strength(*b)));
        order
    }

    /// Returns every node in depth-first preorder from the root, each node's
    /// children strongest first.
    #[must_use]
    pub fn depth_first(&self) -> Vec<NodeId> {
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeId> = self.root().into_iter().collect();
        while let Some(id) = stack.pop() {
            order.push(id);
            let mut children = self.nodes[id.index()].children.clone();
            children.sort_by(|a, b| self.strength(*a).cmp_strongest_first(self.strength(*b)));
            stack.extend(children.into_iter().rev());
        }
        order
    }
}

#[cfg(test)]
impl PrimIndexGraph {
    /// A graph whose root, at `site`, ranks as `root`, with one child of the
    /// root per entry of `children`, in order, at the same site.
    pub(crate) fn from_strengths(
        site: SpecPath,
        root: NodeStrength,
        children: impl IntoIterator<Item = NodeStrength>,
    ) -> Self {
        let arc = |strength: NodeStrength, sibling_index: u16| NodeArc {
            arc_kind: strength.arc_kind,
            layer_stack: LayerId(1),
            site: site.clone(),
            namespace_depth: strength.namespace_depth,
            sibling_index,
            implied: false,
            strength,
        };
        let mut graph = Self::new(arc(root, 0));
        for (index, strength) in children.into_iter().enumerate() {
            let index = u16::try_from(index).expect("small test graph");
            graph.intern_child(NodeId::ROOT, arc(strength, index));
        }
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

    fn strength(
        is_local: bool,
        arc_kind: ArcKind,
        nested_arc_kind: Option<ArcKind>,
        namespace_depth: u16,
        authored: bool,
        arc_list_index: u16,
    ) -> NodeStrength {
        NodeStrength {
            is_local,
            specializes: Vec::new(),
            arc_kind,
            nested_arc_kind,
            namespace_depth,
            authored,
            arc_list_index,
        }
    }

    fn assert_stronger(a: &NodeStrength, b: &NodeStrength) {
        assert_eq!(a.cmp_strongest_first(b), Ordering::Less, "{a:?} < {b:?}");
        assert_eq!(b.cmp_strongest_first(a), Ordering::Greater, "{b:?} > {a:?}");
    }

    #[test]
    fn local_beats_remote() {
        // Spec: local opinions are stronger than opinions introduced by arcs.
        let local = strength(true, ArcKind::Local, None, 1, true, 0);
        let remote = strength(false, ArcKind::Inherits, None, 999, false, 999);
        assert_stronger(&local, &remote);
    }

    #[test]
    fn arc_kind_follows_liverps_order() {
        // Spec ordering (strongest -> weakest): Inherits, Variants, Relocates,
        // References, Payloads, Specializes.
        // Spec: AOUSD Core §10 (LIVERPS ordering).
        let order: Vec<NodeStrength> = [
            ArcKind::Inherits,
            ArcKind::Variants,
            ArcKind::Relocates,
            ArcKind::References,
            ArcKind::Payloads,
            ArcKind::Specializes,
        ]
        .into_iter()
        .map(|kind| strength(false, kind, None, 3, true, 0))
        .collect();
        for pair in order.windows(2) {
            assert_stronger(&pair[0], &pair[1]);
        }
    }

    #[test]
    fn deeper_namespace_wins_ties() {
        // Spec: deeper namespace is stronger when arc kind ties.
        let shallow = strength(false, ArcKind::References, None, 1, true, 0);
        let deep = strength(false, ArcKind::References, None, 2, true, 0);
        assert_stronger(&deep, &shallow);
    }

    #[test]
    fn authored_beats_implied() {
        // Spec: authored arc beats implied.
        let implied = strength(false, ArcKind::References, None, 1, false, 0);
        let authored = strength(false, ArcKind::References, None, 1, true, 0);
        assert_stronger(&authored, &implied);
    }

    #[test]
    fn earlier_arc_in_list_is_stronger() {
        // Spec: otherwise, list order of arcs.
        let first = strength(false, ArcKind::References, None, 1, true, 0);
        let second = strength(false, ArcKind::References, None, 1, true, 1);
        assert_stronger(&first, &second);
    }

    #[test]
    fn shorter_arc_chain_is_stronger() {
        let base = strength(false, ArcKind::References, None, 1, true, 0);
        let nested = strength(
            false,
            ArcKind::References,
            Some(ArcKind::Inherits),
            1,
            true,
            0,
        );
        assert_stronger(&base, &nested);
    }

    fn spec_path(text: &str, paths: &mut PathInterner, tokens: &mut TokenInterner) -> SpecPath {
        let path = paths.intern(Path::parse_absolute(text, tokens).expect("path"));
        SpecPath::from_prim_path(path, paths)
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
        let a = spec_path("/A", &mut paths, &mut tokens);
        let b = spec_path("/B", &mut paths, &mut tokens);
        let graph = PrimIndexGraph::from_strengths(
            a.clone(),
            NodeStrength::local(1),
            [strength(false, ArcKind::Variants, None, 1, true, 0)],
        );
        let variant = graph.nodes().nth(1).expect("variant node").0;
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

    #[test]
    fn traversals_visit_every_node_strongest_first() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let graph = PrimIndexGraph::from_strengths(
            spec_path("/A", &mut paths, &mut tokens),
            NodeStrength::local(2),
            [
                strength(false, ArcKind::Specializes, None, 1, true, 0),
                strength(false, ArcKind::Variants, None, 3, true, 0),
            ],
        );
        let kinds = |order: Vec<NodeId>| -> Vec<ArcKind> {
            order
                .into_iter()
                .map(|id| graph.node(id).expect("node").arc_kind())
                .collect()
        };
        let expected = [ArcKind::Local, ArcKind::Variants, ArcKind::Specializes];
        assert_eq!(kinds(graph.strength_order()), expected);
        assert_eq!(kinds(graph.depth_first()), expected);
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

    fn specialized(specializes: Vec<SpecializesOrigin>) -> NodeStrength {
        NodeStrength {
            specializes,
            ..strength(false, ArcKind::Specializes, None, 1, true, 0)
        }
    }

    #[test]
    fn specializes_reached_through_a_reference_are_weaker_than_payloads() {
        // Spec: AOUSD Core §10.4.1: a specializes is weaker than every other
        // arc, not only than the arc it is reached through.
        let payload = strength(
            false,
            ArcKind::Payloads,
            Some(ArcKind::References),
            1,
            true,
            3,
        );
        let class = specialized(vec![origin(
            ArcKind::References,
            Some(ArcKind::Specializes),
        )]);
        assert_stronger(&payload, &class);
    }

    #[test]
    fn nested_specializes_nodes_are_weaker_than_their_enclosing_node() {
        let outer = origin(ArcKind::Specializes, None);
        let inner = SpecializesOrigin {
            namespace_depth: 2,
            ..origin(ArcKind::Specializes, Some(ArcKind::Specializes))
        };
        let enclosing = NodeStrength {
            nested_arc_kind: Some(ArcKind::References),
            ..specialized(vec![outer])
        };
        assert_stronger(&enclosing, &specialized(vec![outer, inner]));
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
        let c = SpecializesOrigin {
            namespace_depth: 1,
            ..origin(ArcKind::Specializes, Some(ArcKind::Specializes))
        };
        assert_stronger(&specialized(vec![a]), &specialized(vec![a, c]));
        assert_stronger(&specialized(vec![a, c]), &specialized(vec![b]));
        assert_stronger(&specialized(vec![a, c]), &specialized(vec![b, c]));
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
            assert_stronger(&specialized(vec![pair[0]]), &specialized(vec![pair[1]]));
        }
    }
}
