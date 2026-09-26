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
//! namespace depth, then the strength of their origins, then their position
//! among the arcs authored beside them (`PcpCompareSiblingNodeStrength`). A
//! class arc authored inside a reference or payload target is also implied
//! beneath the node of each stronger layer stack, with the class node it is
//! implied from as its origin ([`PrimNode::origin`]; AOUSD Core §10.4.2.4),
//! so it ranks with that layer stack.
//!
//! Specializes are weaker than every other arc (AOUSD Core §10.4.1). A
//! specializes arc authored beneath the root leaves an inert placeholder
//! node where it is authored, and a copy of that node, with the arcs of the
//! specialized prim beneath it, is propagated to the root with the
//! placeholder as its origin. Specializes children of the root rank after
//! every other child, and among themselves by where their origins sit
//! (`_EvalImpliedSpecializes` in `pxr/usd/pcp/primIndex.cpp`).
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

use alloc::{borrow::Cow, vec::Vec};
use core::cmp::Ordering;

use crate::{
    doc::{LayerId, LayerOffset},
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
    /// `true` for a node added where OpenUSD skips a node duplicating a
    /// site the prim index uses: beneath an ancestral arc of a class arc's
    /// target, within the recursive index OpenUSD builds for the class site
    /// (`skipDuplicateNodes` in `_AddArc`, `pxr/usd/pcp/primIndex.cpp`).
    /// Composition adds such nodes whatever order it reaches the sites in,
    /// and drops their registrations of sites another node registers.
    pub(crate) skips_duplicates: bool,
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
    /// The offset from the layers of the node's layer stack to the stage,
    /// before their sublayer offsets; `None` until composition records it.
    layer_offset: Option<LayerOffset>,
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

    /// The node this one was implied from; `None` for a node whose arc is
    /// authored at its parent's site.
    ///
    /// A class arc authored inside a reference or payload target is implied
    /// into each stronger layer stack on the way to the root, as a node
    /// beneath that layer stack's node whose origin is the class node it is
    /// implied from (see [`Self::is_implied`]). A specializes node
    /// propagated to the root has the inert placeholder where its arc is
    /// authored as its origin (see the [module docs](self)). OpenUSD:
    /// `PcpNodeRef::GetOriginNode`, which names the parent for an authored
    /// arc.
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
    /// of the site that arc is authored at. Its [`Self::origin`] is the
    /// class node it is implied from.
    ///
    /// Spec: AOUSD Core §10.4.2.4 (implied class arcs). OpenUSD:
    /// `_EvalImpliedClasses` in `pxr/usd/pcp/primIndex.cpp`.
    #[must_use]
    pub fn is_implied(&self) -> bool {
        self.arc.implied
    }

    /// The offset that maps times in the root layer of this node's layer
    /// stack to stage times: the offsets of every arc from the root down to
    /// this node, composed, each after the offset of the sublayer that
    /// authors it (an arc is read on its authoring layer's timeline). A layer of the stack is read with this offset
    /// composed with its own sublayer offset
    /// ([`crate::LayerStack::offset_at`]), which is the
    /// [`crate::Opinion::layer_offset`] of the node's opinions from it.
    ///
    /// OpenUSD: the time offset of `PcpNodeRef::GetMapToRoot`.
    ///
    /// Spec: AOUSD Core §10.3.1.1 (offsets concatenate along an arc
    /// chain), §12.3.2.1 (layer offset and scale).
    pub(crate) fn layer_offset(&self) -> LayerOffset {
        self.layer_offset.unwrap_or(LayerOffset::IDENTITY)
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
                layer_offset: Some(LayerOffset::IDENTITY),
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
            layer_offset: None,
        });
        self.nodes[parent.index()].children.push(id);
        id
    }

    /// Records `origin` as the node `node` is implied from, unless it has
    /// one.
    pub(crate) fn set_origin(&mut self, node: NodeId, origin: NodeId) {
        let slot = &mut self.nodes[node.index()].origin;
        if slot.is_none() {
            *slot = Some(origin);
            self.ranks.clear();
        }
    }

    /// Records `offset` as the layer offset of `node` (see
    /// [`PrimNode::layer_offset`]), unless it has one: every expansion that
    /// shares a node reaches it through the same arcs.
    pub(crate) fn set_layer_offset(&mut self, node: NodeId, offset: LayerOffset) {
        self.nodes[node.index()].layer_offset.get_or_insert(offset);
    }

    /// Removes every node `keep` rejects, returning the new id of each old
    /// node, `None` for a removed one. A node is kept whenever one of its
    /// descendants is, so the graph stays a tree, and so is the origin of a
    /// kept node, so it still ranks by that origin. The graph is ranked
    /// again once it is complete (see [`Self::rank`]): the order siblings
    /// are placed in depends on the siblings present.
    pub(crate) fn retain_nodes(
        &mut self,
        mut keep: impl FnMut(NodeId, &PrimNode) -> bool,
    ) -> Vec<Option<NodeId>> {
        let mut kept = alloc::vec![false; self.nodes.len()];
        let mut pending: Vec<NodeId> = self
            .nodes()
            .filter(|(id, node)| keep(*id, node))
            .map(|(id, _)| id)
            .collect();
        while let Some(id) = pending.pop() {
            if core::mem::replace(&mut kept[id.index()], true) {
                continue;
            }
            let node = &self.nodes[id.index()];
            pending.extend(node.parent);
            pending.extend(node.origin);
        }
        let mut remap = alloc::vec![None; self.nodes.len()];
        let mut next = 0;
        for (index, kept) in kept.iter().enumerate() {
            if *kept {
                remap[index] = Some(NodeId::from_index(next));
                next += 1;
            }
        }
        let map = |id: NodeId| remap[id.index()];
        let nodes = core::mem::take(&mut self.nodes);
        self.ranks.clear();
        for (index, mut node) in nodes.into_iter().enumerate() {
            if !kept[index] {
                continue;
            }
            node.parent = node.parent.and_then(map);
            node.children.retain(|child| kept[child.index()]);
            for child in &mut node.children {
                *child = map(*child).expect("kept child");
            }
            node.origin = node.origin.and_then(map);
            self.nodes.push(node);
        }
        remap
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
    /// by arc kind (LIVERPS), then deeper namespace depth, then the strength
    /// of their origins, then their position among the arcs authored beside
    /// them.
    ///
    /// An authored arc's origin is the parent itself, which is stronger than
    /// every node beneath it, so an arc authored at a site outranks a class
    /// implied there from deeper in the graph; implied classes rank by where
    /// their origins sit. Specializes rank by their origins as
    /// [`Self::cmp_specializes`] describes.
    ///
    /// The branches of different variant sets at one site rank by their
    /// sets' declared order, which their sibling index carries.
    ///
    /// Spec: AOUSD Core §10.4. OpenUSD: `PcpCompareSiblingNodeStrength` in
    /// `pxr/usd/pcp/strengthOrdering.cpp`.
    fn cmp_siblings(&self, a: NodeId, b: NodeId) -> Ordering {
        let (node_a, node_b) = (&self.nodes[a.index()], &self.nodes[b.index()]);
        let (arc_a, arc_b) = (&node_a.arc, &node_b.arc);
        let by_kind = arc_a
            .arc_kind
            .strength_rank()
            .cmp(&arc_b.arc_kind.strength_rank());
        if by_kind.is_ne() {
            return by_kind;
        }
        if arc_a.arc_kind == ArcKind::Specializes {
            return self
                .cmp_specializes(a, b)
                .then_with(|| arc_a.sibling_index.cmp(&arc_b.sibling_index));
        }
        arc_b
            .namespace_depth
            .cmp(&arc_a.namespace_depth)
            .then_with(|| {
                let origin_a = node_a.origin.or(node_a.parent);
                let origin_b = node_b.origin.or(node_b.parent);
                match (origin_a, origin_b) {
                    (Some(origin_a), Some(origin_b)) if origin_a != origin_b => {
                        self.cmp_positions(origin_a, origin_b)
                    }
                    _ => Ordering::Equal,
                }
            })
            .then_with(|| arc_a.sibling_index.cmp(&arc_b.sibling_index))
    }

    /// Compares two sibling specializes nodes with "strongest first"
    /// ordering, before their positions among the arcs authored beside
    /// them.
    ///
    /// Specializes nodes propagated to the root, and specializes implied
    /// there, rank by the authored specializes arcs they come from: a deeper
    /// namespace depth first, unless one origin lies beneath the other, then
    /// by where those authored arcs sit in the graph. Nodes that come from
    /// the same authored arc rank the node implied into the root layer stack
    /// before the node propagated there, a class hierarchy started at a
    /// shallower prim first, and a node implied further from that arc first,
    /// then by where their origins sit.
    ///
    /// Spec: AOUSD Core §10.4.1. OpenUSD: the specializes case of
    /// `PcpCompareSiblingNodeStrength` in
    /// `pxr/usd/pcp/strengthOrdering.cpp`.
    fn cmp_specializes(&self, a: NodeId, b: NodeId) -> Ordering {
        let (root_a, chain_a) = self.origin_root(a);
        let (root_b, chain_b) = self.origin_root(b);
        if !self.nested_origins(root_a, root_b) {
            let (depth_a, depth_b) = (self.namespace_depth(a), self.namespace_depth(b));
            if depth_a != depth_b {
                return depth_b.cmp(&depth_a);
            }
        }
        let origin_a = self.nodes[a.index()].origin;
        let origin_b = self.nodes[b.index()].origin;
        let implied_first = || {
            let implied = |node: NodeId, origin: NodeId| !self.same_site(node, origin);
            match (origin_a, origin_b) {
                (Some(origin_a), Some(origin_b)) => implied(b, origin_b).cmp(&implied(a, origin_a)),
                _ => Ordering::Equal,
            }
        };
        let (Some(origin_a), Some(origin_b)) = (
            origin_a.or(self.nodes[a.index()].parent),
            origin_b.or(self.nodes[b.index()].parent),
        ) else {
            return Ordering::Equal;
        };
        if origin_a == origin_b {
            // Both authored at the parent, or one implied into the root layer
            // stack and one propagated from the same placeholder.
            return implied_first();
        }
        if root_a != root_b {
            return self.cmp_positions(root_a, root_b);
        }
        let hierarchy_depth = |node: NodeId, origin: NodeId| {
            if self.nodes[node.index()].origin.is_some() {
                self.class_hierarchy_depth(origin)
            } else {
                0
            }
        };
        let root_layer_stack = self.nodes[NodeId::ROOT.index()].arc.layer_stack;
        hierarchy_depth(a, origin_a)
            .cmp(&hierarchy_depth(b, origin_b))
            .then_with(|| chain_b.cmp(&chain_a))
            .then_with(|| {
                let in_root_stack =
                    |node: NodeId| self.nodes[node.index()].arc.layer_stack == root_layer_stack;
                if in_root_stack(a) && in_root_stack(b) {
                    implied_first()
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| self.cmp_positions(origin_a, origin_b))
    }

    fn namespace_depth(&self, node: NodeId) -> u16 {
        self.nodes[node.index()].arc.namespace_depth
    }

    /// `true` when two nodes read the same site of the same layer stack.
    fn same_site(&self, a: NodeId, b: NodeId) -> bool {
        let (a, b) = (&self.nodes[a.index()].arc, &self.nodes[b.index()].arc);
        a.layer_stack == b.layer_stack && a.site == b.site
    }

    /// `true` for a specializes node propagated to the root: a child of the
    /// root at the same site as its origin, the placeholder where its arc is
    /// authored.
    ///
    /// OpenUSD: `Pcp_IsPropagatedSpecializesNode` in `pxr/usd/pcp/utils.h`.
    fn is_propagated_specializes(&self, node: NodeId) -> bool {
        let entry = &self.nodes[node.index()];
        entry.arc.arc_kind == ArcKind::Specializes
            && entry.parent == Some(NodeId::ROOT)
            && entry
                .origin
                .is_some_and(|origin| self.same_site(node, origin))
    }

    /// The node at the start of `node`'s chain of origins, the authored arc
    /// it comes from, and the number of origins on the way.
    ///
    /// OpenUSD: `_GetOriginRootNode` in `pxr/usd/pcp/strengthOrdering.cpp`.
    fn origin_root(&self, node: NodeId) -> (NodeId, usize) {
        let mut cursor = node;
        let mut count = 0;
        while let Some(origin) = self.nodes[cursor.index()].origin {
            cursor = origin;
            count += 1;
        }
        (cursor, count)
    }

    /// `true` when one of two nodes lies beneath the other, reading a
    /// propagated specializes node as beneath its placeholder.
    ///
    /// OpenUSD: `_OriginsAreNestedArcs` in
    /// `pxr/usd/pcp/strengthOrdering.cpp`.
    fn nested_origins(&self, a: NodeId, b: NodeId) -> bool {
        let beneath = |mut cursor: NodeId, ancestor: NodeId| loop {
            if cursor == ancestor {
                return true;
            }
            let next = if self.is_propagated_specializes(cursor) {
                self.nodes[cursor.index()].origin
            } else {
                self.nodes[cursor.index()].parent
            };
            match next {
                Some(next) => cursor = next,
                None => return false,
            }
        };
        beneath(a, b) || beneath(b, a)
    }

    /// `true` when OpenUSD adds `a` to the prim index before `b` because
    /// `b` lies beneath a class implied from a node whose subtree holds `a`,
    /// outside that subtree's variant branches.
    ///
    /// A class node is added with the recursive index of its site, every
    /// arc but the variant branches at once, and its implied class is added
    /// after it, by the parent's `EvalImpliedClasses` task: so every node of
    /// the origin's subtree outside a variant branch comes before the
    /// implied class and all beneath it. This holds through a chain of
    /// origins, as each implied class comes after its own origin's subtree.
    ///
    /// OpenUSD: `_AddArc` (`includeAncestralOpinions`), `_EvalImpliedClasses`
    /// and `Task::PriorityOrder` in `pxr/usd/pcp/primIndex.cpp`.
    pub(crate) fn implied_after(&self, a: NodeId, b: NodeId) -> bool {
        // `a`'s ancestors up to the first variant branch: the nodes whose
        // recursive index adds `a`.
        let mut holders = Vec::new();
        let mut cursor = Some(a);
        while let Some(node) = cursor {
            holders.push(node);
            let entry = &self.nodes[node.index()];
            if entry.arc.arc_kind == ArcKind::Variants {
                break;
            }
            cursor = entry.parent;
        }
        let mut pending = alloc::vec![b];
        let mut seen = Vec::new();
        while let Some(node) = pending.pop() {
            let mut cursor = Some(node);
            while let Some(current) = cursor {
                let entry = &self.nodes[current.index()];
                if entry.arc.implied
                    && let Some(origin) = entry.origin
                    && !seen.contains(&origin)
                {
                    if holders.contains(&origin) {
                        return true;
                    }
                    seen.push(origin);
                    pending.push(origin);
                }
                cursor = entry.parent;
            }
        }
        false
    }

    /// How far a node is below the prim its arc is authored on.
    ///
    /// OpenUSD: `PcpNodeRef::GetDepthBelowIntroduction`.
    fn depth_below_introduction(&self, node: NodeId) -> u16 {
        let root_depth = self.nodes[NodeId::ROOT.index()].arc.namespace_depth;
        root_depth.saturating_sub(self.namespace_depth(node))
    }

    /// The namespace depth of the node that inherits or specializes the
    /// class hierarchy `node` is a member of: `0` for the root, which no arc
    /// introduces.
    ///
    /// OpenUSD: `_GetNamespaceDepthForClassHierarchy` in
    /// `pxr/usd/pcp/strengthOrdering.cpp`, and
    /// `Pcp_FindStartingNodeOfClassHierarchy` in `pxr/usd/pcp/utils.cpp`.
    fn class_hierarchy_depth(&self, node: NodeId) -> u16 {
        let hop = |node: NodeId| {
            if self.is_propagated_specializes(node) {
                self.nodes[node.index()].origin.unwrap_or(node)
            } else {
                node
            }
        };
        let mut instance = hop(node);
        let depth = self.depth_below_introduction(instance);
        while matches!(
            self.nodes[instance.index()].arc.arc_kind,
            ArcKind::Inherits | ArcKind::Specializes
        ) && self.depth_below_introduction(instance) == depth
        {
            let Some(parent) = self.nodes[instance.index()].parent else {
                break;
            };
            instance = hop(parent);
        }
        if instance == NodeId::ROOT {
            0
        } else {
            self.namespace_depth(instance)
        }
    }

    /// Compares where two nodes sit in the strong-to-weak depth-first walk of
    /// the graph: a node before the nodes beneath it, and the nodes beneath
    /// two siblings in [`Self::cmp_siblings`] order.
    ///
    /// OpenUSD: `_OriginIsStronger` in `pxr/usd/pcp/strengthOrdering.cpp`.
    fn cmp_positions(&self, a: NodeId, b: NodeId) -> Ordering {
        let ancestry = |node: NodeId| {
            let mut path = alloc::vec![node];
            let mut cursor = node;
            while let Some(parent) = self.nodes[cursor.index()].parent {
                path.push(parent);
                cursor = parent;
            }
            path.reverse();
            path
        };
        let (path_a, path_b) = (ancestry(a), ancestry(b));
        match path_a.iter().zip(&path_b).find(|(a, b)| a != b) {
            Some((a, b)) => self.cmp_siblings(*a, *b),
            None => path_a.len().cmp(&path_b.len()),
        }
    }

    /// Orders sibling nodes strongest first, inserting each, in the order
    /// they were added, before the first sibling already placed that it is
    /// stronger than.
    ///
    /// [`Self::cmp_siblings`] is not a total order for specializes nodes
    /// (a namespace depth decides only between origins that do not lie
    /// beneath one another), so this places siblings as OpenUSD inserts
    /// them rather than sorting.
    ///
    /// OpenUSD: `PcpPrimIndex_Graph::_InsertChildInStrengthOrder` in
    /// `pxr/usd/pcp/primIndex_Graph.cpp`.
    fn strength_ordered(&self, siblings: Vec<NodeId>) -> Vec<NodeId> {
        let mut ordered: Vec<NodeId> = Vec::with_capacity(siblings.len());
        for sibling in siblings {
            let stronger = |other: &NodeId| self.cmp_siblings(sibling, *other).is_lt();
            let at = match (ordered.first(), ordered.last()) {
                (Some(first), _) if stronger(first) => 0,
                (_, Some(last)) if !stronger(last) => ordered.len(),
                _ => ordered.iter().position(stronger).unwrap_or(ordered.len()),
            };
            ordered.insert(at, sibling);
        }
        ordered
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
            let children = self.strength_ordered(children);
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

    /// Compares two nodes' opinions with "strongest first" ordering: by
    /// their rank in the strength walk.
    ///
    /// Needs the graph to be ranked ([`Self::rank`]).
    pub(crate) fn cmp_nodes(&self, a: NodeId, b: NodeId) -> Ordering {
        debug_assert_eq!(self.ranks.len(), self.nodes.len(), "graph is ranked");
        self.cmp_nodes_by(&self.ranks, a, b)
    }

    fn cmp_nodes_by(&self, ranks: &[u32], a: NodeId, b: NodeId) -> Ordering {
        ranks[a.index()].cmp(&ranks[b.index()])
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
    /// This is [`Self::strength_order`], except that the nodes beneath
    /// siblings that rank equal are visited one sibling after the other
    /// rather than interleaved.
    #[must_use]
    pub fn depth_first(&self) -> Vec<NodeId> {
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeId> = self.root().into_iter().collect();
        while let Some(id) = stack.pop() {
            order.push(id);
            let children = self.strength_ordered(self.nodes[id.index()].children.clone());
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
            skips_duplicates: false,
        };
        let mut graph = Self::new(arc(ArcKind::Local, namespace_depth));
        for (parent, arc_kind, namespace_depth) in arcs {
            let id = NodeId::from_index(graph.nodes.len());
            graph.nodes.push(PrimNode {
                parent: Some(parent),
                origin: None,
                children: Vec::new(),
                arc: arc(arc_kind, namespace_depth),
                layer_offset: None,
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
            skips_duplicates: false,
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
        // The branches of two variant sets hosted at one site.
        let other_set = NodeArc {
            layer_stack: LayerId(2),
            ..arc(ArcKind::Variants, 1, 0)
        };
        let graph = graph(vec![
            (0, arc(ArcKind::Variants, 1, 0)),
            (0, other_set),
            (2, arc(ArcKind::References, 1, 0)),
            (1, arc(ArcKind::Inherits, 1, 0)),
        ]);
        assert_eq!(graph.cmp_nodes(NodeId(1), NodeId(2)), Ordering::Equal);
        assert_order(&graph, &[0, 1, 4, 3]);
        assert_order(&graph, &[2, 4]);
    }

    #[test]
    fn implied_classes_rank_after_authored_ones_and_by_their_origins() {
        // Spec: AOUSD Core §10.4.2.4. `/A` references two targets that each
        // inherit a class, implied beneath the root with the class nodes as
        // origins; an inherit authored at `/A` has the root as its origin and
        // outranks both (`PcpCompareSiblingNodeStrength`).
        let implied = NodeArc {
            implied: true,
            ..arc(ArcKind::Inherits, 1, 0)
        };
        let mut graph = graph(vec![
            (0, arc(ArcKind::References, 1, 0)),
            (0, arc(ArcKind::References, 1, 1)),
            (1, arc(ArcKind::Inherits, 1, 0)),
            (2, arc(ArcKind::Inherits, 1, 0)),
            (0, implied.clone()),
            (
                0,
                NodeArc {
                    layer_stack: LayerId(2),
                    ..implied
                },
            ),
            (
                0,
                NodeArc {
                    layer_stack: LayerId(3),
                    ..arc(ArcKind::Inherits, 1, 0)
                },
            ),
        ]);
        // The class of the second reference is implied first.
        graph.set_origin(NodeId(5), NodeId(4));
        graph.set_origin(NodeId(6), NodeId(3));
        graph.rank();
        assert_eq!(
            graph.node(NodeId(5)).and_then(PrimNode::origin),
            Some(NodeId(4))
        );
        assert_order(&graph, &[0, 7, 6, 5, 1, 3, 2, 4]);
    }

    #[test]
    fn implied_classes_come_after_their_origins_subtrees() {
        // OpenUSD adds a class node with its recursive index, then implies
        // it (`_AddArc`, `_EvalImpliedClasses`): the class `2`'s subtree
        // comes before its implied class `5` and everything beneath it,
        // except the variant branch `8`, which is selected last.
        let in_stack = |layer_stack, arc_kind| NodeArc {
            layer_stack: LayerId(layer_stack),
            ..arc(arc_kind, 1, 0)
        };
        let mut graph = graph(vec![
            (0, in_stack(1, ArcKind::Inherits)),
            (1, in_stack(2, ArcKind::Inherits)),
            (2, in_stack(3, ArcKind::Inherits)),
            (3, in_stack(4, ArcKind::References)),
            (
                0,
                NodeArc {
                    implied: true,
                    ..in_stack(5, ArcKind::Inherits)
                },
            ),
            (5, in_stack(6, ArcKind::Inherits)),
            (6, in_stack(4, ArcKind::References)),
            (2, in_stack(7, ArcKind::Variants)),
            (8, in_stack(8, ArcKind::References)),
        ]);
        graph.set_origin(NodeId(5), NodeId(2));
        let after = |a, b| graph.implied_after(NodeId(a), NodeId(b));
        assert!(after(4, 7));
        assert!(after(2, 5));
        assert!(after(3, 6));
        assert!(!after(7, 4));
        assert!(!after(5, 2));
        assert!(!after(9, 7));
        assert!(!after(1, 7));
    }

    #[test]
    fn retained_nodes_keep_their_origins() {
        let mut graph = graph(vec![
            (0, arc(ArcKind::References, 1, 0)),
            (1, arc(ArcKind::Inherits, 1, 0)),
            (
                0,
                NodeArc {
                    implied: true,
                    ..arc(ArcKind::Inherits, 1, 0)
                },
            ),
        ]);
        graph.set_origin(NodeId(3), NodeId(2));
        let remap = graph.retain_nodes(|id, _| id == NodeId(0) || id == NodeId(3));
        assert_eq!(
            remap,
            [
                Some(NodeId(0)),
                Some(NodeId(1)),
                Some(NodeId(2)),
                Some(NodeId(3))
            ]
        );
        assert_eq!(
            graph.node(NodeId(3)).and_then(PrimNode::origin),
            Some(NodeId(2))
        );
    }

    #[test]
    fn retained_nodes_keep_their_ancestors() {
        let mut graph = graph(vec![
            (0, arc(ArcKind::References, 1, 0)),
            (0, arc(ArcKind::Inherits, 1, 0)),
            (1, arc(ArcKind::Payloads, 1, 0)),
            (2, arc(ArcKind::References, 1, 0)),
        ]);
        let remap = graph.retain_nodes(|id, _| id == NodeId(0) || id == NodeId(3));
        assert_eq!(
            remap,
            [
                Some(NodeId(0)),
                Some(NodeId(1)),
                None,
                Some(NodeId(2)),
                None
            ]
        );
        assert_eq!(graph.len(), 3);
        let payload = graph.node(NodeId(2)).expect("payload");
        assert_eq!(payload.parent(), Some(NodeId(1)));
        assert_eq!(
            graph.node(NodeId(1)).expect("reference").children(),
            [NodeId(2)]
        );
        graph.rank();
        assert_order(&graph, &[0, 1, 2]);
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

    #[test]
    fn traversals_visit_every_node_strongest_first() {
        let graph = graph(vec![
            (0, arc(ArcKind::Specializes, 1, 0)),
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

    /// `arc` in the layer stack `layer_stack`.
    fn in_stack(layer_stack: u64, arc: NodeArc) -> NodeArc {
        NodeArc {
            layer_stack: LayerId(layer_stack),
            ..arc
        }
    }

    #[test]
    fn propagated_specializes_are_weaker_than_payloads() {
        // Spec: AOUSD Core §10.4.1: a specializes authored inside a
        // reference target is weaker than every other arc, not only than
        // that reference. Its placeholder (3) stays beneath the reference;
        // the node propagated to the root (4) ranks after the payload.
        let mut graph = graph(vec![
            (0, in_stack(2, arc(ArcKind::References, 1, 0))),
            (0, in_stack(3, arc(ArcKind::Payloads, 1, 0))),
            (1, in_stack(2, arc(ArcKind::Specializes, 1, 0))),
            (0, in_stack(2, arc(ArcKind::Specializes, 1, 0))),
        ]);
        graph.set_origin(NodeId(4), NodeId(3));
        graph.rank();
        assert_order(&graph, &[0, 1, 3, 2, 4]);
    }

    #[test]
    fn propagated_specializes_follow_their_placeholders() {
        // `/A` specializes `[B, C]` and `B` specializes `D`: `D`'s
        // placeholder (3) sits beneath `B`, so `D` (4) ranks after `B` and
        // before `C` (`_OriginIsStronger`).
        let mut graph = graph(vec![
            (0, in_stack(1, arc(ArcKind::Specializes, 1, 0))),
            (0, in_stack(1, arc(ArcKind::Specializes, 1, 1))),
            (1, in_stack(4, arc(ArcKind::Specializes, 1, 0))),
            (0, in_stack(4, arc(ArcKind::Specializes, 1, 0))),
        ]);
        graph.set_origin(NodeId(4), NodeId(3));
        graph.rank();
        assert_order(&graph, &[0, 1, 4, 2]);
    }

    #[test]
    fn implied_specializes_outrank_the_node_propagated_beside_them() {
        // A specializes authored inside a reference target (2) is implied
        // into the root layer stack (3) and propagated to the root (4), both
        // with the placeholder as their origin: the implied node ranks first
        // (`PcpCompareSiblingNodeStrength`).
        let implied = NodeArc {
            implied: true,
            ..arc(ArcKind::Specializes, 1, 0)
        };
        let mut graph = graph(vec![
            (0, in_stack(2, arc(ArcKind::References, 1, 0))),
            (1, in_stack(2, arc(ArcKind::Specializes, 1, 0))),
            (0, in_stack(2, arc(ArcKind::Specializes, 1, 0))),
            (0, implied),
        ]);
        graph.set_origin(NodeId(3), NodeId(2));
        graph.set_origin(NodeId(4), NodeId(2));
        graph.rank();
        assert_order(&graph, &[0, 1, 4, 3]);
    }

    /// Every opinion of every prim is read with its node's layer offset
    /// composed with the sublayer offset of the opinion's layer, through
    /// sublayers, nested references, inherits and variants, and through a
    /// reference authored in an offset sublayer, which is read on that
    /// sublayer's timeline, and through a relocation of a referenced child,
    /// whose relocate node is read in the relocating layer stack.
    #[test]
    fn node_offsets_compose_to_opinion_offsets() {
        use crate::{
            InMemoryStore, Layer, LayerOffset, LayerStack, PrimSpec, PropertySpec, Reference,
            Stage, StageOptions, SublayerEntry, Value, VariantSetSpec, VariantSpec,
        };

        let mut store = InMemoryStore::default();
        let spin = store.tokens.intern("spin");
        let shape = store.tokens.intern("shape");
        let jagged = store.tokens.intern("jagged");
        let (chip, moved) = (store.path("/Rock/Chip"), store.path("/World/Moved"));
        let world_chip = store.path("/World/Chip");
        let (world, grove, rock, pebble, class) = (
            store.path("/World"),
            store.path("/Grove"),
            store.path("/Rock"),
            store.path("/Pebble"),
            store.path("/_class_Rock"),
        );
        let offset = |offset, scale| LayerOffset { offset, scale };
        let sampled =
            |t: f64| PropertySpec::attribute().with_time_samples(vec![(t, Value::Double(t))]);

        let mut scene = Layer::new(LayerId(1));
        scene
            .sublayers
            .push(SublayerEntry::with_offset(LayerId(2), offset(5.0, 1.0)));
        let reference = Reference {
            layer_offset: offset(10.0, 2.0),
            ..Reference::new(LayerId(3), rock)
        };
        scene.insert_prim(
            world,
            PrimSpec::def()
                .with_reference(reference)
                .with_property(spin, sampled(1.0)),
        );
        scene.relocates.push(crate::doc::Relocate {
            source: world_chip,
            target: Some(moved),
        });
        store.insert_layer(scene);
        let mut weaker = Layer::new(LayerId(2));
        weaker.insert_prim(world, PrimSpec::over().with_property(spin, sampled(2.0)));
        let in_sublayer = Reference {
            layer_offset: offset(10.0, 2.0),
            ..Reference::new(LayerId(3), rock)
        };
        weaker.insert_prim(grove, PrimSpec::def().with_reference(in_sublayer));
        store.insert_layer(weaker);

        let mut asset = Layer::new(LayerId(3));
        asset
            .sublayers
            .push(SublayerEntry::with_offset(LayerId(4), offset(1.0, 1.0)));
        asset.insert_prim(class, PrimSpec::class().with_property(spin, sampled(3.0)));
        asset.insert_prim(chip, PrimSpec::def().with_property(spin, sampled(7.0)));
        store.insert_layer(asset);
        let mut asset_sub = Layer::new(LayerId(4));
        let nested = Reference {
            layer_offset: offset(3.0, 0.5),
            ..Reference::new(LayerId(5), pebble)
        };
        asset_sub.insert_prim(
            rock,
            PrimSpec::def()
                .with_reference(nested)
                .with_inherit(class)
                .with_property(spin, sampled(4.0)),
        );
        store.insert_layer(asset_sub);

        let mut library = Layer::new(LayerId(5));
        let mut pebble_spec = PrimSpec::def().with_property(spin, sampled(5.0));
        let mut variants = crate::HashMap::new();
        variants.insert(
            jagged,
            VariantSpec {
                properties: vec![crate::PropertyEntry {
                    name: spin,
                    spec: sampled(6.0),
                }],
                ..VariantSpec::default()
            },
        );
        pebble_spec
            .variant_sets
            .insert(shape, VariantSetSpec { variants });
        pebble_spec.variant_selections.insert(shape, jagged);
        library.insert_prim(pebble, pebble_spec);
        store.insert_layer(library);

        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let opinions = stage
            .explain_property_path(crate::PropertyPath::new(world, spin))
            .expect("spin composes");
        assert_eq!(opinions.len(), 6, "every layer contributes one opinion");
        let grove_graph = stage.explain_prim_graph(grove).expect("graph");
        let (_, grove_reference) = grove_graph
            .nodes()
            .find(|(_, node)| node.arc_kind() == ArcKind::References)
            .expect("reference node");
        assert_eq!(
            grove_reference.layer_offset(),
            offset(5.0, 1.0).compose(offset(10.0, 2.0)),
            "the authoring sublayer's offset applies to its arcs"
        );
        let grove_opinions = stage
            .explain_property_path(crate::PropertyPath::new(grove, spin))
            .expect("spin composes");
        assert_eq!(grove_opinions.len(), 4, "the asset's opinions");
        let moved_graph = stage.explain_prim_graph(moved).expect("relocated prim");
        assert!(
            moved_graph
                .nodes()
                .any(|(_, node)| node.arc_kind() == ArcKind::Relocates),
            "a relocate node"
        );
        let moved_opinions = stage
            .explain_property_path(crate::PropertyPath::new(moved, spin))
            .expect("spin composes at the target");
        assert!(!moved_opinions.is_empty());
        let world_graph = stage.explain_prim_graph(world).expect("graph");
        let checked = opinions
            .iter()
            .map(|opinion| (world_graph, opinion))
            .chain(grove_opinions.iter().map(|opinion| (grove_graph, opinion)))
            .chain(moved_opinions.iter().map(|opinion| (moved_graph, opinion)));
        for (graph, opinion) in checked {
            let node = graph.node(opinion.key.node).expect("opinion node");
            let stack = LayerStack::gather(&store, node.layer_stack());
            let index = usize::from(opinion.key.layer_strength);
            assert_eq!(
                stack.layers[index], opinion.key.layer_id,
                "layer strength indexes the stack"
            );
            assert_eq!(
                node.layer_offset().compose(stack.offset_at(index)),
                opinion.layer_offset,
                "opinion from layer {:?}",
                opinion.key.layer_id
            );
        }
    }
}
