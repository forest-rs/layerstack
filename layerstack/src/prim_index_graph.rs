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
//! A graph lists its nodes in strength order, as composition builds it: a
//! node is stronger than every node beneath it, and a node's children are
//! ordered by arc kind (LIVERPS), then deeper namespace depth, then the
//! strength of their origins, then their position among the arcs authored
//! beside them (`PcpCompareSiblingNodeStrength`), so the nodes beneath a
//! child come before that child's weaker siblings. A
//! prim's opinions are ranked by their nodes in that order
//! ([`PrimIndexGraph::strength_order`]), then by layer strength within each
//! node's layer stack. A class arc authored inside a reference or payload
//! target is also implied beneath the node of each stronger layer stack,
//! with the class node it is implied from as its origin
//! ([`PrimNode::origin`]; AOUSD Core §10.4.2.4), so it ranks with that layer
//! stack.
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

use crate::layer_stack::LayerStackIdentifier;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::hash::{BuildHasher, Hash, Hasher};

use hashbrown::HashTable;

use crate::{
    doc::{LayerId, LayerOffset},
    prim_index::{ArcKind, OpinionKey},
    spec_path::SpecPath,
    sync::ReadLock,
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

    /// The node with arena index `index`.
    pub(crate) fn from_raw_index(index: usize) -> Self {
        Self::from_index(index)
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
    pub(crate) layer_stack: LayerStackIdentifier,
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

/// Everything about a node of a [`PrimIndexGraph`] but its site: what
/// reaches it, where it sits in the graph and how its layers' times map to
/// the stage.
///
/// Prims whose graphs differ only in their sites, as the prims of one
/// referenced asset beneath many references do, share their nodes' shapes
/// ([`GraphShapes`]). OpenUSD shares a graph's nodes the same way, apart
/// from their site paths (`PcpPrimIndex_Graph::_SharedData` and
/// `_nodeSitePaths` in `pxr/usd/pcp/primIndex_Graph.h`).
#[derive(Clone, Debug)]
pub(crate) struct NodeShape {
    parent: NodeLink,
    origin: NodeLink,
    /// Most nodes have few children; they are kept inline.
    children: smallvec::SmallVec<[NodeId; 4]>,
    arc_kind: ArcKind,
    layer_stack: LayerStackIdentifier,
    namespace_depth: u16,
    sibling_index: u16,
    implied: bool,
    /// The offset from the layers of the node's layer stack to the stage,
    /// before their sublayer offsets.
    layer_offset: LayerOffset,
    stack_offsets: Arc<[(LayerId, LayerOffset)]>,
}

/// A node's parent or origin, or none, in the four bytes of a [`NodeId`]
/// rather than an `Option`'s eight.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct NodeLink(u32);

impl NodeLink {
    /// No node: an index no graph reaches ([`NodeId::from_index`]).
    const NONE: Self = Self(u32::MAX);

    fn of(node: Option<NodeId>) -> Self {
        node.map_or(Self::NONE, |node| Self(node.0))
    }

    fn get(self) -> Option<NodeId> {
        (self != Self::NONE).then_some(NodeId(self.0))
    }
}

impl core::fmt::Debug for NodeLink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.get().fmt(f)
    }
}

impl NodeShape {
    /// Whether `self` and `other` are the same in every respect, their
    /// offsets bit for bit: whether a graph may read one for the other.
    fn same(&self, other: &Self) -> bool {
        self.children == other.children && self.same_but_children(other)
    }

    /// [`Self::same`] but for the children, which the same parents of the
    /// same number of nodes, in order, give two graphs alike.
    fn same_but_children(&self, other: &Self) -> bool {
        let same_offset = |a: &LayerOffset, b: &LayerOffset| {
            a.offset.to_bits() == b.offset.to_bits() && a.scale.to_bits() == b.scale.to_bits()
        };
        self.parent == other.parent
            && self.origin == other.origin
            && self.arc_kind == other.arc_kind
            && self.layer_stack == other.layer_stack
            && self.namespace_depth == other.namespace_depth
            && self.sibling_index == other.sibling_index
            && self.implied == other.implied
            && same_offset(&self.layer_offset, &other.layer_offset)
            && (Arc::ptr_eq(&self.stack_offsets, &other.stack_offsets)
                || self.stack_offsets.len() == other.stack_offsets.len()
                    && self
                        .stack_offsets
                        .iter()
                        .zip(other.stack_offsets.iter())
                        .all(|((a, x), (b, y))| a == b && same_offset(x, y)))
    }

    /// Hashes some of what [`Self::same`] compares: enough to tell most
    /// shapes apart, cheaply, since every composed prim's graph is hashed
    /// unless its parent's is the same.
    fn hash_into(&self, state: &mut impl Hasher) {
        self.parent.hash(state);
        self.arc_kind.hash(state);
        self.layer_stack.root.hash(state);
        self.namespace_depth.hash(state);
        self.sibling_index.hash(state);
    }
}

/// The shape of a graph: its nodes but for their sites, in node order.
/// Graphs hold it by a thin [`Arc`], so a composed prim's index stays as
/// small as when it held its nodes itself.
#[derive(Debug)]
pub(crate) struct GraphShape {
    nodes: alloc::boxed::Box<[NodeShape]>,
}

impl core::ops::Deref for GraphShape {
    type Target = [NodeShape];
    fn deref(&self) -> &[NodeShape] {
        &self.nodes
    }
}

impl GraphShape {
    fn of(nodes: ShapeBuffer) -> Arc<Self> {
        Arc::new(Self {
            nodes: nodes.into_vec().into_boxed_slice(),
        })
    }
}

/// Whether two graphs' shapes are the same, node for node.
fn same_shape(a: &[NodeShape], b: &[NodeShape]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.same(b))
}

/// A graph's shape while it is built: most graphs have few nodes and
/// share the shape of another, so it is built without allocating.
type ShapeBuffer = smallvec::SmallVec<[NodeShape; 8]>;

/// The number of separately locked tables of a [`GraphShapes`]; a power of
/// two.
const SHAPE_SHARDS: usize = 16;

/// One table of a [`GraphShapes`].
type ShapeTable = ReadLock<HashTable<Arc<GraphShape>>>;

/// The distinct graph shapes of one composition, which the graphs of its
/// prims share ([`NodeShape`]): from any number of threads, each table of
/// shapes locked apart.
pub(crate) struct GraphShapes {
    hasher: hashbrown::DefaultHashBuilder,
    shards: alloc::boxed::Box<[ShapeTable]>,
}

impl Default for GraphShapes {
    fn default() -> Self {
        Self {
            hasher: hashbrown::DefaultHashBuilder::default(),
            shards: (0..SHAPE_SHARDS)
                .map(|_| ReadLock::new(HashTable::new()))
                .collect(),
        }
    }
}

impl core::fmt::Debug for GraphShapes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GraphShapes").finish_non_exhaustive()
    }
}

impl GraphShapes {
    fn hash(&self, shape: &[NodeShape]) -> u64 {
        let mut state = self.hasher.build_hasher();
        shape.len().hash(&mut state);
        for node in shape {
            node.hash_into(&mut state);
        }
        state.finish()
    }

    /// The shape equal to `shape` that a graph already holds, `shape`
    /// itself if none does.
    fn intern(&self, shape: ShapeBuffer) -> Arc<GraphShape> {
        let hash = self.hash(&shape);
        // The top bits pick the table; the table indexes by the low ones.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the top four bits pick one of 16 tables"
        )]
        let shard = &self.shards[(hash >> 60) as usize & (SHAPE_SHARDS - 1)];
        // Most shapes are held already: find them under a shared lock.
        if let Some(found) =
            shard.read(|table| table.find(hash, |held| same_shape(held, &shape)).cloned())
        {
            return found;
        }
        shard.write(|table| {
            match table.entry(
                hash,
                |held| same_shape(held, &shape),
                |held| self.hash(held),
            ) {
                hashbrown::hash_table::Entry::Occupied(held) => Arc::clone(held.get()),
                hashbrown::hash_table::Entry::Vacant(vacant) => {
                    let shape = GraphShape::of(shape);
                    vacant.insert(Arc::clone(&shape));
                    shape
                }
            }
        })
    }
}

/// One node of a [`PrimIndexGraph`]: a site that contributes opinions to a
/// composed prim, and the arc that reaches it. A view into its graph,
/// which [`PrimIndexGraph::node`] hands out.
///
/// Spec: AOUSD Core §10.4. OpenUSD: `PcpNodeRef` (`pxr/usd/pcp/node.h`).
#[derive(Clone, Copy)]
pub struct PrimNode<'a> {
    shape: &'a NodeShape,
    site: &'a SpecPath,
}

impl core::fmt::Debug for PrimNode<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PrimNode")
            .field("parent", &self.shape.parent.get())
            .field("origin", &self.shape.origin.get())
            .field("children", &self.shape.children)
            .field("arc_kind", &self.shape.arc_kind)
            .field("layer_stack", &self.shape.layer_stack)
            .field("site", self.site)
            .field("namespace_depth", &self.shape.namespace_depth)
            .field("sibling_index", &self.shape.sibling_index)
            .field("implied", &self.shape.implied)
            .field("layer_offset", &self.shape.layer_offset)
            .finish_non_exhaustive()
    }
}

impl<'a> PrimNode<'a> {
    /// Whether `self` and `other` are the same node of the same graph.
    pub(crate) fn is(self, other: Self) -> bool {
        core::ptr::eq(self.shape, other.shape) && core::ptr::eq(self.site, other.site)
    }

    /// The kind of arc from [`Self::parent`] to this node;
    /// [`ArcKind::Local`] for the root.
    #[must_use]
    pub fn arc_kind(self) -> ArcKind {
        self.shape.arc_kind
    }

    /// The node whose site authors the arc to this node; `None` for the
    /// root.
    #[must_use]
    pub fn parent(self) -> Option<NodeId> {
        self.shape.parent.get()
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
    pub fn origin(self) -> Option<NodeId> {
        self.shape.origin.get()
    }

    /// The nodes reached through arcs authored at this node's site, in the
    /// order they were added.
    #[must_use]
    pub fn children(self) -> &'a [NodeId] {
        &self.shape.children
    }

    /// The root layer of the layer stack this node's site is in.
    ///
    /// The node reads the optional session root, the persistent root and their
    /// sublayers, as [`crate::LayerStack::gather_identifier`] composes them. A node
    /// reached through an internal reference or payload is in the layer stack
    /// of its parent node, whichever layer of that stack authors the arc.
    ///
    /// OpenUSD: `PcpNodeRef::GetLayerStack`.
    #[must_use]
    pub fn layer_stack(self) -> LayerId {
        self.shape.layer_stack.root
    }

    /// The complete stack identity, including a stage-local session root.
    /// External references retain a session-free identity.
    #[must_use]
    pub fn layer_stack_identifier(self) -> LayerStackIdentifier {
        self.shape.layer_stack
    }

    /// The prim spec path this node reads in its layer stack, including the
    /// variant selections of a variant node (`/Model{shape=round}`).
    #[must_use]
    pub fn site(self) -> &'a SpecPath {
        self.site
    }

    /// Namespace depth of the prim the arc to this node is authored on:
    /// `1` for an arc authored on `/A`, `2` on `/A/B`.
    ///
    /// OpenUSD: `PcpNodeRef::GetNamespaceDepth`.
    #[must_use]
    pub fn namespace_depth(self) -> u16 {
        self.shape.namespace_depth
    }

    /// Position of the arc to this node among the arcs of its kind authored
    /// beside it (the Nth reference of a list).
    ///
    /// OpenUSD: `PcpNodeRef::GetSiblingNumAtOrigin`.
    #[must_use]
    pub fn sibling_index(self) -> u16 {
        self.shape.sibling_index
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
    pub fn is_implied(self) -> bool {
        self.shape.implied
    }

    /// The persistent root layer's time offset to the stage. Unlike the
    /// stack-domain offset, this includes session/root time-rate conversion.
    #[must_use]
    pub fn root_layer_offset(self) -> LayerOffset {
        self.layer_offset_for_layer(self.layer_stack())
            .unwrap_or_else(|| self.layer_offset())
    }

    /// Maps a contributing layer's time domain into stage time, as captured
    /// by composition. Muted layers and unresolved expression targets are
    /// absent. This includes sublayer and session/root rate conversion.
    #[must_use]
    pub fn layer_offset_for_layer(self, layer: LayerId) -> Option<LayerOffset> {
        self.shape
            .stack_offsets
            .iter()
            .find(|(id, _)| *id == layer)
            .map(|(_, offset)| self.layer_offset().compose(*offset))
    }

    /// The resident, unmuted layers of the node's layer stack, strongest
    /// first, as composition gathered them.
    pub(crate) fn stack_layers(self) -> impl Iterator<Item = LayerId> + 'a {
        self.shape.stack_offsets.iter().map(|(id, _)| *id)
    }

    /// Strength of a resident, unmuted layer, even when it has no spec here.
    pub(crate) fn layer_strength(self, layer: LayerId) -> Option<u16> {
        self.shape
            .stack_offsets
            .iter()
            .position(|(id, _)| *id == layer)
            .map(|position| u16::try_from(position).unwrap_or(u16::MAX))
    }

    /// The offset that maps this node's layer-stack time domain to stage
    /// times: the offsets of every arc from the root down to
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
    pub(crate) fn layer_offset(self) -> LayerOffset {
        self.shape.layer_offset
    }
}

/// One node for [`PrimIndexGraph::from_ordered_in`].
pub(crate) type OrderedNode = (
    Option<NodeId>,
    Option<NodeId>,
    NodeArc,
    LayerOffset,
    Arc<[(LayerId, LayerOffset)]>,
);

/// The composition graph of one composed prim.
///
/// See the [module docs](self) for how it relates to strength order.
#[derive(Clone, Debug)]
pub struct PrimIndexGraph {
    /// The nodes in strength order, but for their sites: each node after
    /// its parent and the nodes beneath its stronger siblings, each node's
    /// children strongest first, as prim indexing lists them. Shared with
    /// every graph of the composition whose nodes are the same.
    shape: Arc<GraphShape>,
    /// Each node's site, in the order of [`Self::shape`].
    sites: alloc::boxed::Box<[SpecPath]>,
}

impl Default for PrimIndexGraph {
    fn default() -> Self {
        Self {
            shape: GraphShape::of(ShapeBuffer::new()),
            sites: alloc::boxed::Box::default(),
        }
    }
}

impl PrimIndexGraph {
    /// Whether a node is reached through an arc introduced at `depth` of
    /// the composed prim's namespace, even when its own arc is ancestral or
    /// was propagated from another node. Root opinions and arcs authored
    /// only above or below that site are local to the occurrence. Callers
    /// separately require a contributing source.
    ///
    /// A node's [`PrimNode::namespace_depth`] counts in its parent's
    /// namespace: the depth of the parent's site when the arc was authored
    /// there. Where the parent's site has since descended with the prim
    /// being composed, the arc was introduced as many levels above the
    /// prim, so an arc authored inside a referenced asset, beneath the
    /// referencing prim, is direct at that descendant's depth.
    ///
    /// AOUSD Core §11.3.3. OpenUSD: `HasTransitiveDirectDependency` and
    /// `Pcp_ChildNodeIsInstanceable` (`pxr/usd/pcp/instancing.h`), with the
    /// namespace depth `_AddArc` records from the parent node's path.
    pub(crate) fn has_direct_dependency(
        &self,
        paths: &crate::path::PathInterner,
        node: NodeId,
        depth: u16,
    ) -> bool {
        let site_depth = |id: NodeId| {
            self.node(id).map(|node| {
                u16::try_from(paths.resolve(node.site().prim_path()).depth()).unwrap_or(u16::MAX)
            })
        };
        let Some(prim_depth) = site_depth(NodeId::ROOT) else {
            return false;
        };
        let mut pending = Vec::new();
        let mut origins = Vec::new();
        let mut cursor = Some(node);
        while let Some(id) = cursor.take().or_else(|| pending.pop()) {
            if id == NodeId::ROOT {
                continue;
            }
            let Some(current) = self.node(id) else {
                continue;
            };
            let introduced = current.parent().and_then(site_depth).map(|parent| {
                prim_depth.saturating_sub(parent.saturating_sub(current.namespace_depth()))
            });
            if introduced == Some(depth) {
                return true;
            }
            cursor = current.parent();
            if let Some(origin) = current.origin()
                && !origins.contains(&origin)
            {
                origins.push(origin);
                pending.push(origin);
            }
        }
        false
    }

    /// Nodes with surviving specs reached through a direct arc. Graph nodes
    /// alone may be empty dependencies or inert specializes placeholders;
    /// registered sources establish actual spec participation. Repeated
    /// occurrences and layer contributions retain their original node ids.
    pub(crate) fn instanceable_nodes<'a>(
        &'a self,
        paths: &'a crate::path::PathInterner,
        sources: &'a [OpinionKey],
        depth: u16,
    ) -> impl Iterator<Item = NodeId> + 'a {
        sources
            .iter()
            .map(|source| source.node)
            .filter(move |&node| self.has_direct_dependency(paths, node, depth))
    }

    /// A graph of `nodes`, listed in strength order: each node after its
    /// parent, and a node's children in the order they rank. Each entry is
    /// the node's parent, origin, arc, layer offset, and the resident layers
    /// of its layer stack with their offsets.
    #[cfg(test)]
    pub(crate) fn from_ordered(nodes: impl IntoIterator<Item = OrderedNode>) -> Self {
        let (shape, sites) = Self::split(nodes);
        Self {
            shape: GraphShape::of(shape),
            sites,
        }
    }

    /// A graph of `nodes`, listed in strength order, as `from_ordered`
    /// builds one (in tests), its shape shared: `like`'s, when it is the
    /// same, as a child prim's often is its parent's, or else the one in
    /// `shapes` that is the same, if any is ([`NodeShape`]).
    pub(crate) fn from_ordered_in(
        nodes: impl IntoIterator<Item = OrderedNode>,
        shapes: &GraphShapes,
        like: Option<&Arc<GraphShape>>,
    ) -> Self {
        let nodes = nodes.into_iter();
        let mut sites: Vec<SpecPath> = Vec::with_capacity(nodes.size_hint().0);
        // While every node so far is `like`'s, nothing is built but the
        // sites; the first that differs builds the shape, from `like`'s
        // nodes before it.
        let mut matched = 0;
        let mut built: Option<ShapeBuffer> = None;
        for node in nodes {
            let (node, site) = Self::shape_of(node);
            sites.push(site);
            let shape = match (&mut built, like) {
                (Some(shape), _) => shape,
                (None, Some(like))
                    if like
                        .get(matched)
                        .is_some_and(|it| it.same_but_children(&node)) =>
                {
                    matched += 1;
                    continue;
                }
                (None, _) => built.insert(Self::prefix(like, matched)),
            };
            Self::push(shape, node);
        }
        let shape = match (built, like) {
            (None, Some(like)) if matched == like.len() => Arc::clone(like),
            (built, like) => shapes.intern(built.unwrap_or_else(|| Self::prefix(like, matched))),
        };
        Self {
            shape,
            sites: sites.into_boxed_slice(),
        }
    }

    /// The first `len` nodes of `like` as a shape still being built: their
    /// children found again, as [`Self::push`] adds them.
    fn prefix(like: Option<&Arc<GraphShape>>, len: usize) -> ShapeBuffer {
        let mut shape = ShapeBuffer::new();
        for node in like.map_or(&[][..], |like| &like[..len]) {
            Self::push(
                &mut shape,
                NodeShape {
                    children: smallvec::SmallVec::new(),
                    ..node.clone()
                },
            );
        }
        shape
    }

    /// Adds `node` to `shape`, and to its parent's children.
    fn push(shape: &mut ShapeBuffer, node: NodeShape) {
        let id = NodeId::from_index(shape.len());
        if let Some(parent) = node.parent.get() {
            shape[parent.index()].children.push(id);
        }
        shape.push(node);
    }

    /// The shapes and sites of `nodes`, apart.
    #[cfg(test)]
    fn split(
        nodes: impl IntoIterator<Item = OrderedNode>,
    ) -> (ShapeBuffer, alloc::boxed::Box<[SpecPath]>) {
        let nodes = nodes.into_iter();
        let mut shape = ShapeBuffer::with_capacity(nodes.size_hint().0);
        let mut sites: Vec<SpecPath> = Vec::with_capacity(nodes.size_hint().0);
        for node in nodes {
            let (node, site) = Self::shape_of(node);
            Self::push(&mut shape, node);
            sites.push(site);
        }
        (shape, sites.into_boxed_slice())
    }

    /// `node`'s shape, without children, and its site.
    fn shape_of(
        (parent, origin, arc, layer_offset, stack_offsets): OrderedNode,
    ) -> (NodeShape, SpecPath) {
        let NodeArc {
            arc_kind,
            layer_stack,
            site,
            namespace_depth,
            sibling_index,
            implied,
            skips_duplicates: _,
        } = arc;
        let shape = NodeShape {
            parent: NodeLink::of(parent),
            origin: NodeLink::of(origin),
            children: smallvec::SmallVec::new(),
            arc_kind,
            layer_stack,
            namespace_depth,
            sibling_index,
            implied,
            layer_offset,
            stack_offsets,
        };
        (shape, site)
    }

    /// The nodes' shapes, which graphs of the same shape share.
    pub(crate) fn shape(&self) -> &Arc<GraphShape> {
        &self.shape
    }

    /// Moves the sites of `stage_stack`'s nodes beneath `from` to beneath
    /// `to`: the graph of a prototype's descendant, from one instance to
    /// another.
    pub(crate) fn reroot_stage_sites(
        &mut self,
        paths: &crate::path::PathInterner,
        stage_stack: LayerStackIdentifier,
        from: crate::path::PathId,
        to: crate::path::PathId,
    ) {
        let (from_site, to_site) = (SpecPath::from_prim_path(from), SpecPath::from_prim_path(to));
        for (node, site) in self.shape.iter().zip(self.sites.iter_mut()) {
            if node.layer_stack != stage_stack {
                continue;
            }
            let prim = site.prim_path();
            if prim != from
                && !paths
                    .resolve(prim)
                    .segments()
                    .starts_with(paths.resolve(from).segments())
            {
                continue;
            }
            *site =
                crate::prim_indexing::site_paths::replace_prefix(paths, site, &from_site, &to_site);
        }
    }

    /// `true` when two nodes read the same site of the same layer stack.
    fn same_site(&self, a: NodeId, b: NodeId) -> bool {
        self.shape[a.index()].layer_stack == self.shape[b.index()].layer_stack
            && self.sites[a.index()] == self.sites[b.index()]
    }

    /// `true` for a specializes node propagated to the root: a child of the
    /// root at the same site as its origin, the placeholder where its arc is
    /// authored.
    ///
    /// OpenUSD: `Pcp_IsPropagatedSpecializesNode` in `pxr/usd/pcp/utils.h`.
    fn is_propagated_specializes(&self, node: NodeId) -> bool {
        let entry = &self.shape[node.index()];
        entry.arc_kind == ArcKind::Specializes
            && entry.parent.get() == Some(NodeId::ROOT)
            && entry
                .origin
                .get()
                .is_some_and(|origin| self.same_site(node, origin))
    }

    /// Placeholders retain ordering/origin evidence but never author opinions.
    /// Their propagated specializes nodes carry the actual sites, including
    /// sites whose layers have no specs yet (AOUSD Core §10.4.1).
    pub(crate) fn is_inert_specializes_site(&self, mut node: NodeId) -> bool {
        loop {
            if self.nodes().any(|(id, entry)| {
                entry.origin() == Some(node) && self.is_propagated_specializes(id)
            }) {
                return true;
            }
            let Some(parent) = self.node(node).and_then(PrimNode::parent) else {
                return false;
            };
            node = parent;
        }
    }

    /// Sorts `keys` strongest first, as [`Self::cmp_keys`] orders them.
    pub(crate) fn sort_keys(&self, keys: &mut [OpinionKey]) {
        keys.sort_by(|a, b| self.cmp_keys(a, b));
    }

    /// Compares two nodes' opinions with "strongest first" ordering: by
    /// their place in strength order.
    ///
    /// Spec: AOUSD Core §10.4. OpenUSD: `PcpCompareNodeStrength` in
    /// `pxr/usd/pcp/strengthOrdering.cpp`.
    pub(crate) fn cmp_nodes(&self, a: NodeId, b: NodeId) -> Ordering {
        a.index().cmp(&b.index())
    }

    /// Compares two opinions of this prim with "strongest first" ordering:
    /// by their nodes ([`Self::cmp_nodes`]), then by layer strength within
    /// the node's layer stack. Two opinions at the same node and layer
    /// strength are opinions of one site in one layer, and compare equal.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering). OpenUSD orders a prim's
    /// opinions by node, then by layer within the node's layer stack
    /// (`PcpBuildPrimPropertyIndex` in `pxr/usd/pcp/propertyIndex.cpp`).
    pub(crate) fn cmp_keys(&self, a: &OpinionKey, b: &OpinionKey) -> Ordering {
        self.cmp_nodes(a.node, b.node)
            .then_with(|| a.layer_strength.cmp(&b.layer_strength))
    }

    /// The root node, the composed prim's own site; `None` only for an empty
    /// graph.
    #[must_use]
    pub fn root(&self) -> Option<NodeId> {
        (!self.shape.is_empty()).then_some(NodeId::ROOT)
    }

    /// Returns the node `id`, if it is a node of this graph.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<PrimNode<'_>> {
        Some(PrimNode {
            shape: self.shape.get(id.index())?,
            site: &self.sites[id.index()],
        })
    }

    /// Returns the number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shape.len()
    }

    /// Returns `true` when the graph has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shape.is_empty()
    }

    /// Iterates over every node with its id, in the order nodes were added.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, PrimNode<'_>)> {
        self.shape
            .iter()
            .zip(self.sites.iter())
            .enumerate()
            .map(|(index, (shape, site))| (NodeId::from_index(index), PrimNode { shape, site }))
    }

    /// Returns every node in the order the prim's opinions are ranked,
    /// strongest first.
    #[must_use]
    pub fn strength_order(&self) -> Vec<NodeId> {
        self.nodes().map(|(id, _)| id).collect()
    }

    /// Returns every node in depth-first preorder from the root, each node's
    /// children strongest first: [`Self::strength_order`].
    #[must_use]
    pub fn depth_first(&self) -> Vec<NodeId> {
        let mut order = Vec::with_capacity(self.shape.len());
        let mut stack: Vec<NodeId> = self.root().into_iter().collect();
        while let Some(id) = stack.pop() {
            order.push(id);
            stack.extend(self.shape[id.index()].children.iter().rev());
        }
        order
    }
}

#[cfg(test)]
impl PrimIndexGraph {
    /// A graph whose root, at `site`, is at `namespace_depth`, with one node
    /// per entry of `arcs`, `(parent, arc kind, namespace depth)`, all at
    /// `site`, in strength order. Node ids follow `arcs`, starting at 1.
    pub(crate) fn from_arcs(
        site: &SpecPath,
        namespace_depth: u16,
        arcs: impl IntoIterator<Item = (NodeId, ArcKind, u16)>,
    ) -> Self {
        let arc = |arc_kind, namespace_depth| NodeArc {
            arc_kind,
            layer_stack: LayerId(1).into(),
            site: site.clone(),
            namespace_depth,
            sibling_index: 0,
            implied: false,
            skips_duplicates: false,
        };
        Self::from_ordered(
            core::iter::once((
                None,
                None,
                arc(ArcKind::Local, namespace_depth),
                LayerOffset::IDENTITY,
                Arc::default(),
            ))
            .chain(arcs.into_iter().map(|(parent, arc_kind, depth)| {
                (
                    Some(parent),
                    None,
                    arc(arc_kind, depth),
                    LayerOffset::IDENTITY,
                    Arc::default(),
                )
            })),
        )
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
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let path = paths.intern(Path::parse_absolute(text, &tokens).expect("path"));
        SpecPath::from_prim_path(path)
    }

    fn arc(arc_kind: ArcKind, namespace_depth: u16, sibling_index: u16) -> NodeArc {
        NodeArc {
            arc_kind,
            layer_stack: LayerId(1).into(),
            site: spec_path("/A/B/C"),
            namespace_depth,
            sibling_index,
            implied: false,
            skips_duplicates: false,
        }
    }

    /// A graph with a root and the nodes `arcs`, each `(parent, origin,
    /// arc)`, in strength order. Node ids follow `arcs`, starting at 1.
    fn graph(arcs: Vec<(u32, Option<u32>, NodeArc)>) -> PrimIndexGraph {
        PrimIndexGraph::from_ordered(
            core::iter::once((
                None,
                None,
                arc(ArcKind::Local, 1, 0),
                LayerOffset::IDENTITY,
                Arc::default(),
            ))
            .chain(arcs.into_iter().map(|(parent, origin, arc)| {
                (
                    Some(NodeId(parent)),
                    origin.map(NodeId),
                    arc,
                    LayerOffset::IDENTITY,
                    Arc::default(),
                )
            })),
        )
    }

    #[test]
    fn instanceable_nodes_require_specs_and_a_transitive_direct_arc() {
        // The prim is at depth 3, beneath an instance at depth 2, and every
        // site is `/A/B/C`, so each arc's namespace depth is also the depth
        // it was introduced at. `spec_path` interns each site afresh, so
        // `/A/B/C` has the same id in this interner.
        let paths = PathInterner::default();
        let site =
            paths.intern(Path::parse_absolute("/A/B/C", &TokenInterner::default()).expect("path"));
        assert_eq!(site, spec_path("/A/B/C").prim_path());
        // A propagated specializes keeps its direct dependency through the
        // original placeholder, even when it is attached beneath the root.
        let graph = graph(vec![
            (0, None, arc(ArcKind::References, 1, 0)),
            (0, None, arc(ArcKind::References, 2, 0)),
            (2, None, arc(ArcKind::Inherits, 1, 0)),
            (0, None, arc(ArcKind::References, 3, 0)),
            (0, None, arc(ArcKind::Inherits, 2, 0)),
            (2, None, arc(ArcKind::Specializes, 1, 0)),
            (0, Some(6), arc(ArcKind::Specializes, 1, 0)),
        ]);
        let sources: Vec<_> = [0, 1, 2, 3, 4, 7]
            .into_iter()
            .map(|id| OpinionKey {
                node: NodeId(id),
                layer_strength: 0,
                layer_id: LayerId(1),
                lookup_path: PathId::from_raw(0),
                spec_path: spec_path("/A"),
            })
            .collect();
        assert_eq!(
            graph
                .instanceable_nodes(&paths, &sources, 2)
                .collect::<Vec<_>>(),
            vec![NodeId(2), NodeId(3), NodeId(7)]
        );
        assert!(graph.has_direct_dependency(&paths, NodeId(5), 2));
        assert!(
            !graph
                .instanceable_nodes(&paths, &sources, 2)
                .any(|node| node == NodeId(5)),
            "an empty dependency node cannot affect instanceability or keys"
        );
    }

    /// An arc authored inside a referenced asset, on the site the
    /// reference has descended to, is direct at the descendant's depth:
    /// its namespace depth counts in the referenced namespace, not the
    /// stage's. The reference itself was introduced higher up.
    ///
    /// OpenUSD: `HasTransitiveDirectDependency`, with `_AddArc` taking the
    /// namespace depth from the parent node's path.
    #[test]
    fn arcs_inside_a_referenced_asset_are_direct_where_they_are_authored() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let site = |text: &str| {
            SpecPath::from_prim_path(
                paths.intern(Path::parse_absolute(text, &tokens).expect("path")),
            )
        };
        let node = |site: SpecPath, namespace_depth| NodeArc {
            site,
            namespace_depth,
            ..arc(ArcKind::References, namespace_depth, 0)
        };
        // `/Grove/Oak/Canopy/Acorn` (depth 4): `/Grove/Oak` references
        // `/Tree`, whose `/Tree/Canopy/Acorn` references `/Acorn`.
        let graph = PrimIndexGraph::from_ordered([
            (
                None,
                None,
                node(site("/Grove/Oak/Canopy/Acorn"), 0),
                LayerOffset::IDENTITY,
                Arc::default(),
            ),
            (
                Some(NodeId::ROOT),
                None,
                node(site("/Tree/Canopy/Acorn"), 2),
                LayerOffset::IDENTITY,
                Arc::default(),
            ),
            (
                Some(NodeId(1)),
                None,
                node(site("/Acorn"), 3),
                LayerOffset::IDENTITY,
                Arc::default(),
            ),
        ]);
        assert!(!graph.has_direct_dependency(&paths, NodeId(1), 4));
        assert!(graph.has_direct_dependency(&paths, NodeId(1), 2));
        assert!(graph.has_direct_dependency(&paths, NodeId(2), 4));
        assert!(!graph.has_direct_dependency(&paths, NodeId(2), 3));
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

    /// Prims whose graphs differ only in their sites share their nodes'
    /// shapes: a prim and its child reached through the same reference,
    /// and the prims of one asset beneath two references to it. Each graph
    /// still reads its own sites.
    #[test]
    fn graphs_differing_only_in_sites_share_their_shape() {
        use crate::{InMemoryStore, Layer, PrimSpec, Reference, Stage, StageOptions};

        let mut store = InMemoryStore::default();
        let (a, b, asset) = (store.path("/A"), store.path("/B"), store.path("/Asset"));
        let (a_leaf, b_leaf, asset_leaf) = (
            store.path("/A/Leaf"),
            store.path("/B/Leaf"),
            store.path("/Asset/Leaf"),
        );
        let mut scene = Layer::new(LayerId(1));
        scene.insert_prim(
            a,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), asset)),
        );
        scene.insert_prim(
            b,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), asset)),
        );
        store.insert_layer(scene);
        let mut library = Layer::new(LayerId(2));
        library.insert_prim(asset, PrimSpec::def());
        library.insert_prim(asset_leaf, PrimSpec::def());
        store.insert_layer(library);

        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let graph = |prim| stage.explain_prim_graph(prim).expect("composed prim");
        for prim in [b, a_leaf, b_leaf] {
            assert!(
                Arc::ptr_eq(graph(a).shape(), graph(prim).shape()),
                "one shape for every prim referencing the asset"
            );
        }
        let sites = |prim| -> Vec<SpecPath> {
            graph(prim)
                .nodes()
                .map(|(_, node)| node.site().clone())
                .collect()
        };
        assert_eq!(
            sites(a_leaf),
            vec![
                SpecPath::from_prim_path(a_leaf),
                SpecPath::from_prim_path(asset_leaf)
            ]
        );
        assert_eq!(
            sites(b_leaf),
            vec![
                SpecPath::from_prim_path(b_leaf),
                SpecPath::from_prim_path(asset_leaf)
            ]
        );
        let reference = graph(b_leaf).node(NodeId(1)).expect("reference node");
        assert_eq!(reference.arc_kind(), ArcKind::References);
        assert_eq!(reference.parent(), Some(NodeId::ROOT));
    }

    /// The children of a prim that lose the same culled nodes of its graph
    /// share one shape with each other, not their parent's.
    #[test]
    fn siblings_losing_the_same_nodes_share_their_shape() {
        use crate::{InMemoryStore, Layer, PrimSpec, Reference, Stage, StageOptions};

        let mut store = InMemoryStore::default();
        let (target, source) = (store.path("/Target"), store.path("/Source"));
        let children: Vec<_> = (0..4)
            .map(|at| store.path(&alloc::format!("/Target/C{at}")))
            .collect();
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(source, PrimSpec::def());
        layer.insert_prim(
            target,
            PrimSpec::def().with_reference(Reference::new(LayerId(1), source)),
        );
        for &child in &children {
            layer.insert_prim(child, PrimSpec::def());
        }
        store.insert_layer(layer);

        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let graph = |prim| stage.explain_prim_graph(prim).expect("composed prim");
        assert_eq!(graph(target).len(), 2, "the prim and its reference");
        assert_eq!(
            graph(children[0]).len(),
            1,
            "the reference has nothing beneath it"
        );
        for &child in &children[1..] {
            assert!(Arc::ptr_eq(
                graph(children[0]).shape(),
                graph(child).shape()
            ));
            assert_eq!(
                graph(child)
                    .node(NodeId::ROOT)
                    .map(|node| node.site().clone()),
                Some(SpecPath::from_prim_path(child))
            );
        }
    }

    #[test]
    fn opinions_rank_by_node_then_layer() {
        // Spec: AOUSD Core §10.4. Layer stack order breaks ties within a
        // node; nothing else does, so the order of the prim's opinions
        // does not depend on how its paths were interned.
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let path = |text: &str| {
            let path = paths.intern(Path::parse_absolute(text, &tokens).expect("path"));
            SpecPath::from_prim_path(path)
        };
        let (a, b) = (path("/A"), path("/B"));
        let graph = graph(vec![(0, None, arc(ArcKind::Variants, 1, 0))]);
        let variant = NodeId(1);
        let order = [
            key(NodeId::ROOT, 0, 1, a.clone()),
            key(NodeId::ROOT, 1, 0, b.clone()),
            key(variant, 0, 0, a.clone()),
        ];
        for pair in order.windows(2) {
            assert_eq!(graph.cmp_keys(&pair[0], &pair[1]), Ordering::Less);
            assert_eq!(graph.cmp_keys(&pair[1], &pair[0]), Ordering::Greater);
        }
        assert_eq!(
            graph.cmp_keys(&key(NodeId::ROOT, 0, 1, a), &key(NodeId::ROOT, 0, 1, b)),
            Ordering::Equal
        );
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
                    spec: sampled(6.0).into(),
                }],
                ..VariantSpec::default()
            },
        );
        pebble_spec
            .variant_sets
            .insert(shape, VariantSetSpec { variants });
        pebble_spec.variant_set_names.make_mut().prepend.push(shape);
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
