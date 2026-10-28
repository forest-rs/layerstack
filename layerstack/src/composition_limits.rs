// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How much work one composition may do ([`CompositionLimits`]).
//!
//! Valid scene description can compose without end. A prim that
//! specializes (or inherits, or references) a class which specializes the
//! prim's own parent gains a child of its own name through the class, and
//! that child does the same: no arc site repeats, so there is no arc
//! cycle (AOUSD Core §10.6), yet the namespace is endless (`/Leaf/Leaf/...`,
//! each prim index a few nodes larger than its parent's). Two such
//! children per level make it a tree that doubles each level; a class
//! nested in the class makes each prim's index costlier per node than its
//! parent's. A 230-byte layer is enough.
//!
//! OpenUSD has no limit: Pcp composes such a stage until the process runs
//! out of memory (`Usd.Stage.Open` of the 230-byte layer reached 1.7 GB
//! after eight minutes in OpenUSD 26.11). layerstack stops composing at
//! explicit, finite limits instead ([`CompositionLimits`]), reports
//! [`CompositionError::LimitExceeded`], and keeps the stage composed up to
//! them. The default limits are far above what the production scenes
//! measured compose, so the two compose those alike.
//!
//! Spec: AOUSD Core §10 (composition arcs), §10.6 (composition errors),
//! §11 (stage population).
//!
//! [`CompositionError::LimitExceeded`]: crate::CompositionError::LimitExceeded

/// Bounds on the work one composition does: [`StageOptions::limits`].
///
/// Each limit is `None` for no limit ([`Self::UNLIMITED`]). The defaults
/// are finite, so that no stage composes without end, and far above the
/// production scenes measured: the largest composes 131 000 prim index
/// nodes for its 3 900 prims, whose composed depth is 5 at most, and
/// another 12 900 nodes for 2 700 prims, composed depth 7. A host
/// composing larger scenes raises them; one with less memory to spare,
/// such as a browser tab, lowers them.
///
/// Build it as a struct literal that names the fields it sets and takes
/// the rest from `..CompositionLimits::default()`: a field a later release
/// adds then keeps its default.
///
/// ```
/// use layerstack::{CompositionLimits, StageOptions};
///
/// let options = StageOptions {
///     limits: CompositionLimits {
///         prim_index_nodes: Some(1_000_000),
///         ..CompositionLimits::default()
///     },
///     ..StageOptions::default()
/// };
/// assert_eq!(options.limits.composed_depth, Some(64));
/// ```
///
/// [`StageOptions::limits`]: crate::StageOptions::limits
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompositionLimits {
    /// The most prim index nodes one composition builds or copies, over
    /// all its prims: a prim's index holds a node for each site its arcs
    /// reach (culled nodes included), and an instance's copied
    /// descendants count their indexes' nodes. This bounds both the work
    /// and the memory of composition, whatever the shape of the namespace.
    ///
    /// Composition stops once the prims it has indexed hold this many
    /// nodes: the prim whose index reached the limit is the last composed,
    /// without children, and no prim composition would index after it is
    /// composed. Which prims those are is the same on any number of
    /// threads ([`StageOptions::threads`]): when the limit is reached on
    /// several threads, composition starts again on one, in traversal
    /// order. [`Stage::composition_work`] reports the nodes a composition
    /// built ([`CompositionWork::prim_index_nodes`]).
    ///
    /// A live stage's edit recomposes within the same limit: an edit whose
    /// recomposition reaches it, or any edit to a stage that has reached
    /// it, recomposes the whole stage, as a new stage would compose.
    ///
    /// [`DEFAULT_PRIM_INDEX_NODES`](Self::DEFAULT_PRIM_INDEX_NODES) by
    /// default.
    ///
    /// [`StageOptions::threads`]: crate::StageOptions::threads
    /// [`Stage::composition_work`]: crate::Stage::composition_work
    /// [`CompositionWork::prim_index_nodes`]: crate::stage::CompositionWork::prim_index_nodes
    pub prim_index_nodes: Option<usize>,
    /// The most nodes one prim's index holds: composition adds no more
    /// arcs to an index this large, and the prim composes no children and
    /// is reported. Other prims compose as usual.
    ///
    /// The work of adding a node grows with the index it joins, so an
    /// index that grows without end costs more than its nodes, and
    /// [`Self::prim_index_nodes`], which counts them, would stop it only
    /// after minutes: endless namespaces fuzzing found compose single
    /// indexes of 70 000 nodes, built at 250 microseconds a node. The
    /// largest index of the production scenes measured holds 55 nodes.
    /// [`DEFAULT_NODES_PER_PRIM_INDEX`](Self::DEFAULT_NODES_PER_PRIM_INDEX)
    /// by default.
    pub nodes_per_prim_index: Option<usize>,
    /// How deep composition takes a namespace below what its layers
    /// author: a prim this many levels deeper than the deepest spec
    /// authored at a site of its index, or at a namespace ancestor of one,
    /// composes no children, and when it names any, composition reports
    /// it. Other prims compose as usual.
    ///
    /// A prim a layer authors is at composed depth zero, however deep its
    /// namespace (the USDA reader nests 256 levels; a layer built in code
    /// can nest deeper). A prim a reference brings in is at most as deep
    /// below its specs as the shallower of the referenced namespace and
    /// the referencing prim: `/World/Set/Ref` referencing an asset's
    /// `/Asset` composes `/World/Set/Ref/Geo/Mesh`, five levels deep, from
    /// the specs at `/World/Set/Ref` and `/Asset/Geo/Mesh`, three deep, at
    /// composed depth two. Only a reference more than the limit deep to an
    /// asset more than the limit deep reaches it, whose prims OpenUSD
    /// composes all (as it does with `None`). Arcs make a namespace
    /// endless only by mapping shallow sites ever deeper, each prim a
    /// level deeper and its sites not, so its composed depth grows with
    /// its depth, and with it the work of its index, faster than its
    /// nodes. [`DEFAULT_COMPOSED_DEPTH`](Self::DEFAULT_COMPOSED_DEPTH) by
    /// default.
    pub composed_depth: Option<usize>,
}

impl CompositionLimits {
    /// The default of [`Self::prim_index_nodes`]: 20 million nodes, 150
    /// times those of the largest production scene measured. A namespace
    /// that doubles each level without end reaches it in 17 seconds,
    /// holding 1.6 GB; the slowest endless namespace fuzzing found, whose
    /// indexes cost the most per node, in 50 seconds, and the largest
    /// holds 4.1 GB.
    pub const DEFAULT_PRIM_INDEX_NODES: usize = 20_000_000;

    /// The default of [`Self::nodes_per_prim_index`]: 1 024 nodes, 18
    /// times the largest index of the production scenes measured.
    pub const DEFAULT_NODES_PER_PRIM_INDEX: usize = 1_024;

    /// The default of [`Self::composed_depth`]: 64 levels, nine times the
    /// deepest production scene measured. The endless chains found
    /// compose to it in a second at most.
    pub const DEFAULT_COMPOSED_DEPTH: usize = 64;

    /// No limit: composition runs to its end, as OpenUSD's does, however
    /// long that takes, and never reports
    /// [`CompositionError::LimitExceeded`].
    ///
    /// [`CompositionError::LimitExceeded`]: crate::CompositionError::LimitExceeded
    pub const UNLIMITED: Self = Self {
        prim_index_nodes: None,
        nodes_per_prim_index: None,
        composed_depth: None,
    };
}

impl Default for CompositionLimits {
    fn default() -> Self {
        Self {
            prim_index_nodes: Some(Self::DEFAULT_PRIM_INDEX_NODES),
            nodes_per_prim_index: Some(Self::DEFAULT_NODES_PER_PRIM_INDEX),
            composed_depth: Some(Self::DEFAULT_COMPOSED_DEPTH),
        }
    }
}

/// One of the [`CompositionLimits`], as [`LimitExceeded`] names it.
///
/// [`LimitExceeded`]: crate::LimitExceeded
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CompositionLimit {
    /// [`CompositionLimits::prim_index_nodes`].
    PrimIndexNodes,
    /// [`CompositionLimits::nodes_per_prim_index`].
    NodesPerPrimIndex,
    /// [`CompositionLimits::composed_depth`].
    ComposedDepth,
}

impl CompositionLimit {
    /// The limit's field name in [`CompositionLimits`] (`prim_index_nodes`,
    /// `nodes_per_prim_index`, `composed_depth`): stable, for hosts and
    /// bindings that name the option to raise.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::PrimIndexNodes => "prim_index_nodes",
            Self::NodesPerPrimIndex => "nodes_per_prim_index",
            Self::ComposedDepth => "composed_depth",
        }
    }
}
