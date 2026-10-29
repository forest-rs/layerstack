// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prim indexing: composes each prim's index from its namespace parent's.
//!
//! The stage's prims are indexed top down. A prim's index starts as a copy
//! of its parent's, each node moved to the prim's name, and adds the arcs
//! authored at its own sites (see [`indexer`]). Nodes whose subtrees
//! contribute no specs are culled before the index is kept, so a child
//! starts from only the nodes that still contribute. A prim's children are
//! the names its index's sites author, weakest site first; a prim's
//! opinions are its index's specs, strongest first.
//!
//! Spec: AOUSD Core §10 (composition arcs), §10.4 (strength ordering), §11
//! (stage population). OpenUSD: `PcpComputePrimIndex`,
//! `PcpPrimIndex::ComputePrimChildNames` (`pxr/usd/pcp/primIndex.cpp`).

pub(crate) mod dynamic_args;
pub(crate) mod graph;
pub(crate) mod indexer;
pub(crate) mod layer_stacks;
pub(crate) mod map_function;
#[cfg(test)]
mod pending_tests;
pub(crate) mod site;
pub(crate) mod site_paths;

use crate::sort;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use hashbrown::{HashMap, HashSet};

use crate::{
    asset::{AssetRequest, AssetUse, PendingPrim},
    composition_error::{CompositionError, LimitExceeded},
    composition_limits::CompositionLimit,
    dependency_map::{DependencyRecords, PrimDependencies},
    doc::{LayerId, LayerStore},
    expression_variables::VariableReads,
    interner::TokenId,
    layer_stack::LayerStackIdentifier,
    path::PathId,
    prim_index::{ArcKind, Opinion, OpinionKey, OpinionSource, PrimIndex},
    prim_index_graph::{NodeArc, NodeId, PrimIndexGraph},
    spec_path::SpecPath,
    stage::{InstanceIndexKey, InstanceIndexKeys, StageOptions},
    sync::ReadLock,
};

use self::{
    graph::{Graph, NONE, NodeIndex, Site, is_class_based},
    indexer::{Ctx, Outputs, Recorded, build_prim_index, is_instanceable},
    layer_stacks::{LayerStacks, StackId},
};

pub(crate) use self::site::{ChildIndex, ChildIndexWork};

/// Everything a stage is built from.
#[derive(Default)]
pub(crate) struct Composition {
    pub(crate) prims: HashMap<PathId, PrimIndex>,
    pub(crate) children: HashMap<PathId, Vec<PathId>>,
    pub(crate) inactive: HashSet<PathId>,
    pub(crate) inactive_children: HashMap<PathId, Vec<PathId>>,
    pub(crate) instances: HashSet<PathId>,
    pub(crate) loadable: HashSet<PathId>,
    pub(crate) errors: Vec<CompositionError>,
    pub(crate) asset_requests: Vec<AssetRequest>,
    /// The prims not composed, with their descendants, because their
    /// indexes read layers the store has not loaded yet, in the order the
    /// walk joins them ([`LayerStore::is_layer_pending`]).
    pub(crate) pending: Vec<PendingPrim>,
    /// What the prims it keeps depend on, when it records that.
    pub(crate) deps: Option<DependencyRecords>,
    pub(crate) used_layers: HashSet<LayerId>,
    pub(crate) used_layer_sites: HashMap<PathId, HashSet<LayerId>>,
    pub(crate) local_layers: Vec<LayerId>,
    pub(crate) composed_prim_indexes: usize,
    pub(crate) reused_prim_indexes: usize,
    /// The nodes of the prim indexes composed and copied
    /// ([`crate::stage::CompositionWork::prim_index_nodes`]).
    pub(crate) prim_index_nodes: usize,
    /// Whether composition reached [`crate::CompositionLimits::prim_index_nodes`]
    /// and stopped: child lists may name prims it did not compose.
    pub(crate) spent: bool,
    pub(crate) reads: VariableReads,
    /// The whole child list of each requested boundary parent the
    /// composition indexed, whatever its scope lists ([`compose`]).
    pub(crate) boundary_children: HashMap<PathId, Vec<PathId>>,
    /// The [`InstanceIndexKey`] of each active instance root kept that was
    /// indexed rather than copied from another instance's descendants,
    /// found on the thread that indexed it.
    pub(crate) instance_keys: InstanceIndexKeys,
    /// What each composed prim depends on, which `deps` holds at the end.
    records: HashMap<PathId, PrimDependencies>,
    /// The layer stacks whose errors a prim has reported.
    reported_stacks: HashSet<StackId>,
    /// The parents of the boundary parents indexed only for their children
    /// ([`Boundaries::retained`]), whose child lists name prims not kept.
    graph_only_parents: HashSet<PathId>,
    /// Whether a prim's index reads each layer stack, by [`StackId`]: the
    /// stacks whose layers are `used_layers`.
    used_stacks: Vec<bool>,
}

/// Which prims a composition indexes.
pub(crate) enum Scope<'m> {
    /// Every prim.
    All,
    /// The prims a population mask includes, and their ancestors.
    Mask(Selection),
    /// Exactly the listed prims and their ancestors, each with its whole
    /// child list.
    Exact(Selection),
    /// Exactly the listed prims, whose child lists name only listed prims
    /// (the live stage's local namespace updates). Unlisted ancestors are
    /// indexed for their children to start from, but not kept.
    Listed {
        listed: &'m HashSet<PathId>,
        ancestors: HashSet<PathId>,
    },
}

/// The paths a mask selects, kept for lookups that cost a path's depth
/// rather than the number of paths: a live edit can recompose as many
/// sibling subtrees as prims reference what it changed.
pub(crate) struct Selection {
    /// The selected paths.
    selected: HashSet<PathId>,
    /// The selected paths and their ancestors.
    above: HashSet<PathId>,
}

impl Selection {
    /// The selection of the paths `include`.
    pub(crate) fn new(paths: &crate::path::PathInterner, include: &[PathId]) -> Self {
        let selected: HashSet<PathId> = include.iter().copied().collect();
        let mut above = HashSet::new();
        for &path in include {
            let mut current = Some(path);
            while let Some(at) = current {
                if !above.insert(at) {
                    break;
                }
                current = paths.parent(at);
            }
        }
        Self { selected, above }
    }

    /// Whether `path` is a selected path or an ancestor of one.
    fn is_above(&self, path: PathId) -> bool {
        self.above.contains(&path)
    }

    /// Whether `path` is a selected path or beneath one.
    fn is_beneath(&self, paths: &crate::path::PathInterner, path: PathId) -> bool {
        let mut current = Some(path);
        while let Some(at) = current {
            if self.selected.contains(&at) {
                return true;
            }
            current = paths.parent(at);
        }
        false
    }
}

impl<'m> Scope<'m> {
    /// The scope of exactly the prims `listed`.
    pub(crate) fn listed(paths: &crate::path::PathInterner, listed: &'m HashSet<PathId>) -> Self {
        let mut ancestors = HashSet::new();
        for path in listed {
            let segments: Vec<TokenId> = paths.resolve(*path).segments().to_vec();
            for depth in 0..segments.len() {
                ancestors.insert(paths.intern_segments(&segments[..depth]));
            }
        }
        Self::Listed { listed, ancestors }
    }

    /// Whether `path`, once indexed, is kept in the composition.
    fn keeps(&self, paths: &crate::path::PathInterner, path: PathId) -> bool {
        match self {
            Self::Listed { listed, .. } => {
                listed.contains(&path) || paths.resolve(path).depth() == 0
            }
            _ => true,
        }
    }

    /// Whether the scope composes `child`, a child of an indexed prim
    /// ([`Self::includes`]), and whether it lists it among its parent's
    /// children: an exact scope lists every child, a listed one only the
    /// listed prims, and the others the children they compose. Each set is
    /// looked up once, as a wide parent asks for every child.
    fn places(&self, paths: &crate::path::PathInterner, child: PathId) -> (bool, bool) {
        match self {
            Self::All => (true, true),
            Self::Mask(_) => {
                let included = self.includes(paths, child);
                (included, included)
            }
            Self::Listed { listed, ancestors } => {
                // A child is never the pseudo-root.
                let listed = listed.contains(&child);
                (listed || ancestors.contains(&child), listed)
            }
            Self::Exact(_) => (self.includes(paths, child), true),
        }
    }

    fn includes(&self, paths: &crate::path::PathInterner, path: PathId) -> bool {
        match self {
            Self::All => true,
            // As `PopulationMask::includes`: the pseudo-root, the selected
            // subtrees and their ancestors.
            Self::Mask(selection) => {
                paths.parent(path).is_none()
                    || selection.is_above(path)
                    || selection.is_beneath(paths, path)
            }
            Self::Listed { listed, ancestors } => {
                listed.contains(&path)
                    || ancestors.contains(&path)
                    || paths.resolve(path).depth() == 0
            }
            Self::Exact(selection) => selection.is_above(path),
        }
    }
}

/// The boundary parents of a partial composition: the prims just outside
/// the subtrees it recomposes, whose whole child lists it keeps
/// ([`Composition::boundary_children`]).
#[derive(Debug, Default)]
pub(crate) struct Boundaries {
    /// The parents.
    pub(crate) parents: HashSet<PathId>,
    /// Whether the caller keeps the parents' own indexes, which an edit
    /// beneath them leaves as they are: the composition then indexes each
    /// only for its children to start from and for its child list, and
    /// keeps no index of it. An instance is indexed whole all the same.
    ///
    /// OpenUSD keeps the parent's cached index too, starting a new child's
    /// from a copy of its graph (`_BuildInitialPrimIndexFromAncestor` in
    /// `pxr/usd/pcp/primIndex.cpp`), and composes only the parent's child
    /// names again (`UsdStage::_ComposeChildren`); a live stage keeps no
    /// indexing graph, so the parent's is built again here.
    pub(crate) retained: bool,
}

impl Boundaries {
    /// Whether `path` is a parent whose index the caller keeps.
    fn retains(&self, path: PathId) -> bool {
        self.retained && self.parents.contains(&path)
    }
}

/// Composes the stage rooted at `root`, reading the child specs of layers
/// that do not list them from `child_index`.
///
/// For each prim of `boundaries` it indexes, the composition also keeps
/// the prim's whole child list, in composition order and before inactive
/// children are pruned, though `scope` lists only some of them
/// ([`Composition::boundary_children`]): a live stage merging a partial
/// composition orders a boundary parent's children from it, rather than
/// composing the parent's child names again.
pub(crate) fn compose(
    store: &dyn LayerStore,
    root: LayerId,
    options: &StageOptions,
    scope: Scope<'_>,
    boundaries: &Boundaries,
    child_index: &ChildIndex,
    checks: &mut crate::composition_checks::Findings,
) -> Composition {
    child_index.prepare(store);
    let root_path = store.paths().intern_segments(&[]);
    let load_rules = options.load_rules.clone();
    let include_payload = move |paths: &crate::path::PathInterner, path: PathId| {
        load_rules.is_loaded(paths.resolve(path))
    };
    let mut ctx = Ctx {
        store,
        stacks: LayerStacks::muting(&options.muted_layers),
        child_index,
        fallbacks: &options.variant_fallbacks,
        include_payload: &include_payload,
        stage_stack: StackId(0),
        root_path,
        dynamic_formats: &options.dynamic_formats,
        schemas: options.schemas.as_deref(),
        any_type: store.tokens().lookup("__AnyType__"),
        shapes: crate::prim_index_graph::GraphShapes::default(),
        nodes_per_prim_index: options.limits.nodes_per_prim_index,
    };
    let identifier = LayerStackIdentifier {
        root,
        session: options.session_layer,
    };
    ctx.stage_stack = match ctx.stacks.find_or_compute(ctx.store, identifier, None) {
        Ok(stack) => stack,
        // Without its own layer stack the stage has no prim to compose,
        // not even the pseudo-root.
        Err(layers) => {
            return Composition {
                pending: alloc::vec![PendingPrim {
                    prim: root_path,
                    layers,
                }],
                ..Composition::default()
            };
        }
    };
    let mut out = Composition {
        local_layers: ctx.stacks.get(ctx.stage_stack).layers.clone(),
        ..Composition::default()
    };

    let share = matches!(scope, Scope::All)
        && !ctx.stacks.get(ctx.stage_stack).has_relocates()
        && options
            .load_rules
            .rules()
            .all(|(path, rule)| path.depth() == 0 && rule == crate::stage::PayloadRule::All);
    let mut instances = Instances::default();
    let ctx = ctx;
    let claims = Claims {
        share,
        keys: ReadLock::default(),
        hasher: hashbrown::DefaultHashBuilder::default(),
    };
    let budget = Budget::new(options);
    let work = |prim: Pending, added: &mut Vec<Pending>| {
        index_prim(
            &ctx,
            &scope,
            boundaries,
            &claims,
            &budget,
            options.with_dependencies,
            prim,
            added,
        )
    };
    let threads = options.thread_budget();
    let in_order = crate::parallel::with_pool_in(&threads, &work, |pool| {
        let mut walk = Walk {
            ctx: &ctx,
            pool,
            threads: &threads,
            out: &mut out,
            scope: &scope,
            checks,
            instances: &mut instances,
            budget: &budget,
            with_dependencies: options.with_dependencies,
        };
        walk.walk(alloc::vec![root_path], None);
        walk.share_prototypes();
        pool.in_order()
    });
    if budget.is_spent() && !in_order {
        // Which prims threads had indexed when the limit was reached
        // depends on how they were scheduled: one thread indexes them in
        // traversal order, so the limit stops composition at the same
        // prim on any number of threads. The work is bounded by the limit
        // twice over.
        *checks = crate::composition_checks::Findings::default();
        let serial = StageOptions {
            threads: 1,
            executor: None,
            ..options.clone()
        };
        return compose(store, root, &serial, scope, boundaries, child_index, checks);
    }
    out.spent = budget.is_spent();

    if options.with_dependencies {
        // Only the prims the composition keeps depend on anything.
        let mut records = core::mem::take(&mut out.records);
        records.retain(|path, _| out.prims.contains_key(path));
        out.deps = Some(DependencyRecords::new(records));
    }
    let used = (0..).zip(&out.used_stacks).filter(|(_, used)| **used);
    let used: Vec<&[LayerId]> = used
        .map(|(at, _)| &ctx.stacks.get(StackId(at)).layers[..])
        .collect();
    out.used_layers
        .reserve(used.iter().map(|layers| layers.len()).sum());
    out.used_layers.extend(used.into_iter().flatten().copied());
    // The stage's own layer stack, and any no arc reached, report last,
    // their sublayer requests at the pseudo-root.
    for (at, stack) in (0..).zip(ctx.stacks.iter()) {
        if !out.reported_stacks.contains(&StackId(at)) {
            out.errors.extend(stack.errors.iter().cloned());
            out.asset_requests
                .extend(sublayer_requests(stack, ctx.root_path));
        }
        out.reads.extend(stack.reads.clone());
    }
    if let Some(deps) = out.deps.as_mut() {
        let layers: Vec<LayerId> = ctx
            .stacks
            .iter()
            .flat_map(|stack| stack.layers.iter().copied())
            .collect();
        deps.add_relocation_layers(layers);
    }
    finish(&mut out);
    out
}

/// The fewest spent instance indexes freed on a thread of their own while
/// instances get their descendants ([`Walk::share_prototypes`]): for fewer,
/// starting the thread costs more than freeing them.
const SPENT_ON_THREAD: usize = 256;

/// The instances a composition has seen, by the key their prototype is
/// shared by.
#[derive(Default)]
struct Instances {
    /// The first instance composed with each key.
    representatives: HashMap<InstanceKey, PathId>,
    /// Instances whose descendants wait for their representative's, with
    /// the instance's index when it was indexed rather than copied.
    deferred: Vec<(PathId, PathId, Option<Arc<Graph>>)>,
    /// The representative of each instance of `deferred`, by instance.
    waiting: HashMap<PathId, PathId>,
}

impl Instances {
    /// Defers the descendants of `instance` until `representative`'s are
    /// composed.
    fn defer(&mut self, instance: PathId, representative: PathId, graph: Option<Arc<Graph>>) {
        self.deferred.push((instance, representative, graph));
        self.waiting.insert(instance, representative);
    }

    /// The instance deferred last, with its representative and index.
    fn next(&mut self) -> Option<(PathId, PathId, Option<Arc<Graph>>)> {
        let next = self.deferred.pop()?;
        self.waiting.remove(&next.0);
        Some(next)
    }
}

/// What an instance's descendants are composed from: its instanceable
/// nodes, strongest first, each with its arc, layer stack, site and time
/// offset. OpenUSD: `PcpInstanceKey`.
///
/// The key's hash is found once, on the thread that indexes the instance,
/// and is all the key hashes as: the composition looks each key up twice,
/// once to claim it and once to find its representative.
#[derive(Clone, Debug)]
struct InstanceKey {
    hash: u64,
    nodes: Vec<(ArcKind, StackId, SpecPath, u64, u64)>,
}

impl PartialEq for InstanceKey {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.nodes == other.nodes
    }
}

impl Eq for InstanceKey {}

impl core::hash::Hash for InstanceKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

fn instance_key(graph: &Graph, hasher: &hashbrown::DefaultHashBuilder) -> InstanceKey {
    let mut out = Vec::new();
    let mut pending = graph.child_list(0);
    pending.reverse();
    while let Some(node) = pending.pop() {
        let entry = graph.node(node);
        if entry.culled {
            continue;
        }
        if entry.transitive_direct && entry.can_contribute_specs() && entry.has_specs {
            let offset = entry.map_to_root.offset();
            out.push((
                entry.arc_kind,
                entry.site.stack,
                entry.site.path.clone(),
                offset.offset.to_bits(),
                offset.scale.to_bits(),
            ));
        }
        let mut children = graph.child_list(node);
        children.reverse();
        pending.extend(children);
    }
    InstanceKey {
        hash: core::hash::BuildHasher::hash_one(hasher, &out),
        nodes: out,
    }
}

/// What one composition may still build ([`StageOptions::limits`]),
/// shared by the threads indexing its prims.
///
/// Spec: AOUSD Core §10.6, §11; OpenUSD has no limit (see
/// [`crate::composition_limits`]).
struct Budget {
    /// [`crate::CompositionLimits::prim_index_nodes`].
    nodes: Option<usize>,
    /// [`crate::CompositionLimits::composed_depth`].
    depth: Option<usize>,
    /// The nodes of the prim indexes built and copied so far, on every
    /// thread.
    used: AtomicUsize,
    /// Whether `used` reached `nodes`: no prim is indexed after.
    spent: AtomicBool,
}

impl Budget {
    fn new(options: &StageOptions) -> Self {
        Self {
            nodes: options.limits.prim_index_nodes,
            depth: options.limits.composed_depth,
            used: AtomicUsize::new(0),
            spent: AtomicBool::new(false),
        }
    }

    /// Whether the nodes reached their limit.
    fn is_spent(&self) -> bool {
        self.spent.load(Ordering::Relaxed)
    }

    /// Counts `nodes` more nodes built, and returns the limit when they
    /// are the first to reach it.
    fn spend(&self, nodes: usize) -> Option<usize> {
        let limit = self.nodes?;
        let used = self.used.fetch_add(nodes, Ordering::Relaxed) + nodes;
        (used >= limit && !self.spent.swap(true, Ordering::Relaxed)).then_some(limit)
    }

    /// The limit the prim at `path`, whose index is `graph`, reaches when
    /// it names children: [`composed_depth`] levels at least. A prim
    /// shallower than the limit is within it whatever its sites author.
    fn too_deep(&self, ctx: &Ctx<'_>, path: PathId, graph: &Graph) -> Option<usize> {
        let limit = self.depth?;
        let depth = ctx.store.paths().resolve(path).depth();
        (depth >= limit && composed_depth(ctx, depth, graph) >= limit).then_some(limit)
    }
}

/// How many levels a prim `depth` deep, whose index is `graph`, lies below
/// the deepest spec its layers author at a site of its index or at a
/// namespace ancestor of one ([`crate::CompositionLimits::composed_depth`]):
/// zero for a prim a layer authors, one for a referenced asset's child,
/// however deep either is, but as deep as the prim for a namespace that
/// arcs make endless, whose sites stay shallow while its prims go deeper.
///
/// Spec: AOUSD Core §10 (composition arcs map sites into the prim's
/// namespace), §11 (stage population).
fn composed_depth(ctx: &Ctx<'_>, depth: usize, graph: &Graph) -> usize {
    let paths = ctx.store.paths();
    let mut deepest = 0;
    for node in &graph.nodes {
        // A site's ancestors are no deeper than the site.
        let mut at = Some(node.site.path.clone());
        let mut has_specs = node.has_specs;
        while let Some(site) = at {
            let site_depth = paths.resolve(site.prim_path()).depth();
            if site_depth <= deepest {
                break;
            }
            if has_specs {
                deepest = site_depth;
                break;
            }
            at = site_paths::parent(paths, &site);
            has_specs = at.as_ref().is_some_and(|parent| {
                site::has_specs(ctx.store, ctx.stacks.get(node.site.stack), parent)
            });
        }
        if deepest >= depth {
            return 0;
        }
    }
    depth - deepest
}

/// The instance keys whose descendants some instance is indexing, while
/// instances share prototypes.
struct Claims {
    /// Whether instances may share prototypes: every prim is composed and
    /// payloads load everywhere alike.
    share: bool,
    keys: ReadLock<HashSet<InstanceKey>>,
    /// What instance keys are hashed with.
    hasher: hashbrown::DefaultHashBuilder,
}

/// A prim to index, with its namespace parent's finalized index, which its
/// own starts from, when the parent was indexed in this composition.
type Pending = (PathId, Option<Arc<Graph>>);

/// What indexing one prim found, before it joins the composition.
///
/// Every prim's result waits for the whole walk before it joins, on one
/// thread, so it is kept small: what most prims do not report is boxed,
/// and only when reported, and what the join reads of the prim's graph is
/// taken from it on the indexing thread, which frees it when its children
/// are done with it.
struct Indexed {
    path: PathId,
    /// The prim's graph, for an instance that did not claim its key
    /// ([`Self::key`]), whose descendants may wait for or be walked from
    /// it; `None` otherwise.
    graph: Option<Arc<Graph>>,
    has_payloads: bool,
    instanceable: bool,
    /// The layer stacks of the graph's nodes, each once.
    stacks: smallvec::SmallVec<[StackId; 2]>,
    /// The layer stacks of the nodes the prim's own arcs add, rather than
    /// its parent's.
    arc_stacks: smallvec::SmallVec<[StackId; 1]>,
    /// What the prim depends on, when the composition records it.
    record: Option<PrimDependencies>,
    index: PrimIndex,
    /// The errors, asset requests and variable reads it reported, if any.
    reports: Option<Box<Reports>>,
    active: bool,
    /// Whether it is a boundary parent indexed only for its children
    /// ([`Boundaries::retained`]): `index` is empty, and it is not kept.
    graph_only: bool,
    /// The key of an active instance, while instances share prototypes.
    key: Option<InstanceKey>,
    /// The prototype key part of an active instance
    /// ([`Composition::instance_keys`]).
    index_key: Option<Box<InstanceIndexKey>>,
    /// Whether the children it composes were indexed with it.
    descended: bool,
    /// The children the scope lists.
    children: Vec<PathId>,
    /// The children it composes, when the scope lists others; `None` when
    /// they are [`Self::children`].
    composed_children: Option<Vec<PathId>>,
    /// Every child, for a requested boundary parent.
    boundary_children: Option<Vec<PathId>>,
    /// The nodes its index was built with, culled ones included.
    nodes: usize,
    /// Whether it was not indexed, the budget spent first: it is not
    /// composed ([`Budget`]).
    beyond_budget: bool,
}

impl Indexed {
    /// The result of indexing the prim at `path`, which reads the layers
    /// `pending` that the store has not loaded yet: it composes nothing,
    /// and adds no children.
    fn pending(path: PathId, pending: Vec<LayerId>) -> Self {
        Self {
            path,
            graph: None,
            has_payloads: false,
            instanceable: false,
            stacks: smallvec::SmallVec::new(),
            arc_stacks: smallvec::SmallVec::new(),
            record: None,
            index: PrimIndex::default(),
            reports: Some(Box::new(Reports {
                errors: Vec::new(),
                asset_requests: Vec::new(),
                reads: VariableReads::default(),
                findings: crate::composition_checks::Findings::default(),
                pending,
            })),
            active: false,
            graph_only: false,
            key: None,
            index_key: None,
            descended: false,
            children: Vec::new(),
            composed_children: None,
            boundary_children: None,
            nodes: 0,
            beyond_budget: false,
        }
    }

    /// The result for the prim at `path`, not indexed because the
    /// composition's budget was spent first: it is not composed.
    fn beyond_budget(path: PathId) -> Self {
        Self {
            reports: None,
            beyond_budget: true,
            ..Self::pending(path, Vec::new())
        }
    }
}

/// Where each of a walk's results is, by the path it indexed: a table
/// by path id when the results are dense among the ids, as for a whole
/// stage's prims, a map otherwise.
enum Places {
    Dense(Vec<u32>),
    Sparse(HashMap<PathId, usize>),
}

impl Places {
    fn new(indexed: &[Indexed]) -> Self {
        let ids = indexed
            .iter()
            .map(|prim| prim.path.get() as usize + 1)
            .max()
            .unwrap_or(0);
        if ids <= 4 * indexed.len() + 1024 {
            let mut at = alloc::vec![u32::MAX; ids];
            for (place, prim) in indexed.iter().enumerate() {
                at[prim.path.get() as usize] = u32::try_from(place).expect("u32 results");
            }
            Self::Dense(at)
        } else {
            Self::Sparse(
                indexed
                    .iter()
                    .enumerate()
                    .map(|(place, prim)| (prim.path, place))
                    .collect(),
            )
        }
    }

    fn get(&self, path: PathId) -> Option<usize> {
        match self {
            Self::Dense(at) => at
                .get(path.get() as usize)
                .filter(|&&place| place != u32::MAX)
                .map(|&place| place as usize),
            Self::Sparse(at) => at.get(&path).copied(),
        }
    }
}

/// What indexing one prim reported besides its index, which most prims
/// leave empty.
struct Reports {
    errors: Vec<Recorded>,
    asset_requests: Vec<AssetRequest>,
    reads: VariableReads,
    findings: crate::composition_checks::Findings,
    /// The layers the store has not loaded yet that the prim's index
    /// reads: when there are any, the prim is not composed, and reports
    /// nothing else.
    pending: Vec<LayerId>,
}

/// Indexes the prim at `path` from its parent's index, and adds the
/// children it composes to `added`, unless the prim is inactive, or an
/// active instance whose key another instance has claimed: one instance's
/// descendants are indexed for each key, and the rest copy them.
///
/// Any number of threads index prims at once; each child is added with
/// its parent's index, which no thread changes. Which instance claims a
/// key is whichever gets there first, so [`Walk::join`] still decides
/// which composes them.
fn index_prim(
    ctx: &Ctx<'_>,
    scope: &Scope<'_>,
    boundaries: &Boundaries,
    claims: &Claims,
    budget: &Budget,
    with_dependencies: bool,
    (path, parent): Pending,
    added: &mut Vec<Pending>,
) -> Indexed {
    if budget.is_spent() {
        return Indexed::beyond_budget(path);
    }
    let site = Site {
        stack: ctx.stage_stack,
        path: SpecPath::from_prim_path(path),
    };
    let mut outputs = Outputs::default();
    build_prim_index(
        ctx,
        &site,
        &site,
        true,
        true,
        true,
        None,
        parent.as_deref(),
        &mut outputs,
    );
    if !outputs.pending.is_empty() {
        return Indexed::pending(path, outputs.pending);
    }
    // A child's graph is often its parent's but for its sites, or else its
    // siblings'. The shape is borrowed, not cloned: the children of one
    // parent, on every thread, would contend for its count.
    let like = parent
        .as_deref()
        .and_then(|parent| parent.child_shape.get().or(parent.shape.as_ref()));
    let mut graph = core::mem::take(&mut outputs.graph);
    let nodes = graph.nodes.len();
    let spent = budget.spend(nodes);
    cull(&mut graph, ctx.store.paths(), ctx.stage_stack);
    graph.instanceable = path != ctx.root_path && is_instanceable(ctx.store, &ctx.stacks, &graph);
    graph.finalize();

    let mut names = child_names(ctx, &graph);
    // A prim at a limit composes no children: the prim index nodes ran
    // out with its own, its index is as large as an index grows, or it is
    // as far below what its layers author as composition places prims.
    let limit = match (spent, ctx.nodes_per_prim_index) {
        (Some(value), _) => Some((CompositionLimit::PrimIndexNodes, value)),
        (None, Some(value)) if outputs.full => Some((CompositionLimit::NodesPerPrimIndex, value)),
        _ if names.is_empty() => None,
        _ => budget
            .too_deep(ctx, path, &graph)
            .map(|value| (CompositionLimit::ComposedDepth, value)),
    };
    if let Some((limit, value)) = limit {
        names.clear();
        outputs
            .errors
            .push(Recorded::Error(CompositionError::LimitExceeded(
                LimitExceeded {
                    limit,
                    value,
                    prim: path,
                },
            )));
    }
    let mut findings = crate::composition_checks::Findings::default();
    // A boundary parent whose index the caller keeps is indexed for its
    // graph alone, which its children start from.
    let graph_only = !graph.instanceable && path != ctx.root_path && boundaries.retains(path);
    let index = if graph_only {
        PrimIndex::default()
    } else {
        let mut index = prim_index(ctx, path, &graph, like, &mut findings);
        if let Some(parent) = parent.as_deref() {
            parent.child_shape.offer(index.graph.shape());
        }
        // Found here, on the thread indexing the prim, for the stage's type
        // info to read.
        index.type_name = Some(crate::Stage::source_type_name(
            &index,
            ctx.store,
            ctx.any_type,
        ));
        let dynamic = core::mem::take(&mut outputs.dynamic);
        index.dynamic = (!dynamic.is_empty()).then(|| Arc::new(dynamic));
        index
    };
    drop(parent);
    let active = path == ctx.root_path || is_active(ctx, &graph);
    // Found here, while the index is at hand, rather than for every
    // instance on one thread once the walk is done.
    let index_key = (active && graph.instanceable).then(|| {
        let depth = ctx.store.paths().resolve(path).depth();
        Box::new(InstanceIndexKey::of(
            &index,
            depth,
            ctx.store,
            ctx.fallbacks,
        ))
    });
    // An exact scope lists every child but composes only the listed
    // prims and their ancestors: the live stage merges whole child
    // lists. A population mask lists only the children it includes.
    let paths = ctx.store.paths();
    let mut children = Vec::with_capacity(names.len());
    let mut composed_children = Vec::with_capacity(names.len());
    let mut boundary_children = boundaries
        .parents
        .contains(&path)
        .then(|| Vec::with_capacity(names.len()));
    for name in names {
        let child = paths.intern_joined(path, &[name]);
        let (included, listed) = scope.places(paths, child);
        if included {
            composed_children.push(child);
        }
        if listed {
            children.push(child);
        }
        if let Some(all) = boundary_children.as_mut() {
            all.push(child);
        }
    }
    let key = (claims.share && active && graph.instanceable)
        .then(|| instance_key(&graph, &claims.hasher));
    let mut stacks = smallvec::SmallVec::new();
    let mut arc_stacks = smallvec::SmallVec::new();
    for (at, node) in graph.nodes.iter().enumerate() {
        if !stacks.contains(&node.site.stack) {
            stacks.push(node.site.stack);
        }
        if at != 0 && !node.from_parent_prim && !arc_stacks.contains(&node.site.stack) {
            arc_stacks.push(node.site.stack);
        }
    }
    let record =
        (with_dependencies && !graph_only).then(|| prim_record(ctx, &graph, &index, &mut outputs));
    let (has_payloads, instanceable) = (graph.has_payloads, graph.instanceable);
    // Only the children of the prim read its shape from its graph.
    if !graph_only && !composed_children.is_empty() {
        graph.shape = Some(Arc::clone(index.graph.shape()));
    }
    let graph = Arc::new(graph);
    let descended = active
        && key
            .as_ref()
            .is_none_or(|key| claims.keys.write(|keys| keys.insert(key.clone())));
    if descended {
        added.extend(
            composed_children
                .iter()
                .map(|&child| (child, Some(Arc::clone(&graph)))),
        );
    }
    let reports = (!outputs.errors.is_empty()
        || !outputs.asset_requests.is_empty()
        || !outputs.reads.is_empty()
        || !findings.is_empty())
    .then(|| {
        Box::new(Reports {
            errors: outputs.errors,
            asset_requests: outputs.asset_requests,
            reads: outputs.reads,
            findings,
            pending: Vec::new(),
        })
    });
    let composed_children = (composed_children != children).then_some(composed_children);
    Indexed {
        path,
        // An instance that claimed its key has added its children with its
        // graph: the join reads it no more, and it is freed here, with
        // them, rather than on the thread joining the prims. Should the
        // instance's descendants not be shared after all, they start from
        // indexes of their ancestors built anew.
        graph: (key.is_some() && !descended).then_some(graph),
        has_payloads,
        instanceable,
        stacks,
        arc_stacks,
        record,
        index,
        reports,
        active,
        graph_only,
        key,
        index_key,
        descended,
        children,
        composed_children,
        boundary_children,
        nodes,
        beyond_budget: false,
    }
}

/// The dependencies the prim index `index`, over `graph`, records, its
/// layers and arcs each once (see [`DependencyRecords`]).
fn prim_record(
    ctx: &Ctx<'_>,
    graph: &Graph,
    index: &PrimIndex,
    outputs: &mut Outputs,
) -> PrimDependencies {
    let mut record = PrimDependencies::default();
    record
        .layers
        .extend(index.sources.iter().map(|source| source.key.layer_id));
    // The layers authoring the references and payloads on the way to each
    // node retime it.
    record
        .layers
        .extend(core::mem::take(&mut outputs.arc_layers));
    record
        .layers
        .extend(graph.nodes.iter().filter_map(|node| node.authoring_layer));
    // A layer contributes once per spec and arc of the index: each counts
    // once.
    sort::sort_unstable(&mut record.layers);
    record.layers.dedup();
    record.default_prim_layers = core::mem::take(&mut outputs.default_prim_layers);
    for (at, node) in graph.nodes.iter().enumerate() {
        if at == 0 || matches!(node.arc_kind, ArcKind::Local | ArcKind::Variants) {
            continue;
        }
        record.arcs.push((
            node.site.path.prim_path(),
            node.arc_kind,
            ctx.stacks.get(node.site.stack).identifier.root,
        ));
    }
    // Nodes at one site prim, as variant branches of it are, read it
    // through one arc.
    record.dedup_arcs();
    record
}

/// The prims a composition indexes on a pool of threads, joined into it
/// one by one in traversal order, as if indexed one by one: each prim's
/// records, errors and instance keys are recorded in the same order
/// whatever the number of threads.
struct Walk<'a, 'c, 'p> {
    ctx: &'a Ctx<'c>,
    pool: &'a crate::parallel::Pool<'p, Pending, Indexed>,
    /// The budget the pool's threads are drawn from.
    threads: &'a crate::parallel::Threads,
    out: &'a mut Composition,
    scope: &'a Scope<'a>,
    checks: &'a mut crate::composition_checks::Findings,
    instances: &'a mut Instances,
    /// What the composition may still build.
    budget: &'a Budget,
    /// Whether each prim records its dependencies.
    with_dependencies: bool,
}

impl Walk<'_, '_, '_> {
    /// Indexes the prims of `roots`, children of the prim `parent` indexes
    /// (`None` when it was not indexed here), and their descendants, each
    /// from its parent's index, and joins them depth first, `roots` in
    /// order.
    fn walk(&mut self, roots: Vec<PathId>, parent: Option<&Arc<Graph>>) {
        let tasks = roots.iter().map(|&root| (root, parent.cloned())).collect();
        let indexed = self.pool.run(tasks);
        // Grown once, rather than rehashing as the prims join.
        self.out.prims.reserve(indexed.len());
        if self.with_dependencies {
            self.out.records.reserve(indexed.len());
        }
        // One thread indexes prims in the order they join: a prim, then
        // the children it adds, depth first.
        if self.pool.in_order() {
            let mut ignored = Vec::new();
            for prim in indexed {
                self.join(prim, &mut ignored);
                ignored.clear();
            }
            return;
        }
        // Found by place, so each prim moves once more, into its join.
        let places = Places::new(&indexed);
        let mut indexed: Vec<Option<Indexed>> = indexed.into_iter().map(Some).collect();
        let mut pending = roots;
        pending.reverse();
        while let Some(path) = pending.pop() {
            let prim = places
                .get(path)
                .and_then(|at| indexed[at].take())
                .expect("indexed prim");
            self.join(prim, &mut pending);
        }
    }

    /// Joins the indexed `prim` into the composition, and pushes the
    /// children to join after it, which are indexed, onto `pending`, last
    /// first.
    fn join(&mut self, prim: Indexed, pending: &mut Vec<PathId>) {
        let Indexed {
            path,
            graph,
            has_payloads,
            instanceable,
            stacks,
            arc_stacks,
            record,
            index,
            reports,
            active,
            graph_only,
            key,
            index_key,
            descended,
            children,
            composed_children,
            boundary_children,
            nodes,
            beyond_budget,
        } = prim;
        if beyond_budget {
            return;
        }
        let ctx = self.ctx;
        let out = &mut *self.out;
        if let Some(all) = boundary_children {
            out.boundary_children.insert(path, all);
        }
        if let Some(reports) = reports {
            let Reports {
                errors,
                asset_requests,
                reads,
                findings,
                pending,
            } = *reports;
            if !pending.is_empty() {
                out.pending.push(PendingPrim {
                    prim: path,
                    layers: pending,
                });
                return;
            }
            self.checks.extend(findings);
            record_errors(out, ctx, errors);
            out.asset_requests.extend(asset_requests);
            out.reads.extend(reads);
        }
        out.composed_prim_indexes += 1;
        out.prim_index_nodes += nodes;
        // A layer stack's use belongs to the prim whose index first adds a
        // node reading it; its descendants inherit the node. Its layers
        // join `used_layers` once the walk is done.
        for stack in stacks {
            let at = stack.0 as usize;
            if at >= out.used_stacks.len() {
                out.used_stacks.resize(at + 1, false);
            }
            out.used_stacks[at] = true;
        }
        if !arc_stacks.is_empty() {
            let sites = out.used_layer_sites.entry(path).or_default();
            for stack in arc_stacks {
                sites.extend(ctx.stacks.get(stack).layers.iter().copied());
            }
        }
        if let Some(record) = record {
            out.records.insert(path, record);
        }
        if has_payloads {
            out.loadable.insert(path);
        }
        let mut deferred = false;
        // An inactive prim is no instance, whatever its index.
        // OpenUSD: `Usd_PrimData::_ComposeAndCacheFlags`
        // (`active && IsInstanceable()`). Spec: AOUSD Core §11.3.3.
        if instanceable && active {
            out.instances.insert(path);
        }
        // An inactive instance composes no descendants to share. The first
        // instance of a key, in traversal order, composes them.
        if let Some(key) = key {
            match self.instances.representatives.get(&key) {
                Some(&representative) => {
                    self.instances.defer(path, representative, graph.clone());
                    deferred = true;
                }
                None => {
                    self.instances.representatives.insert(key, path);
                }
            }
        }
        let keep = !graph_only && self.scope.keeps(ctx.store.paths(), path);
        if graph_only && let Some(parent) = ctx.store.paths().parent(path) {
            out.graph_only_parents.insert(parent);
        }
        if keep {
            out.prims.insert(path, index);
            if let Some(index_key) = index_key {
                out.instance_keys.insert(path, *index_key);
            }
        }
        if !active {
            if keep {
                out.inactive.insert(path);
            }
            return;
        }
        // The children it composes, when the scope lists them all, are
        // read back from where they are kept.
        let composed = match composed_children {
            Some(composed) => {
                if keep && !children.is_empty() {
                    out.children.insert(path, children);
                }
                composed
            }
            None if keep && !children.is_empty() => {
                if deferred || descended {
                    let kept = out.children.entry(path).insert(children);
                    if !deferred {
                        pending.extend(kept.get().iter().rev().copied());
                    }
                    return;
                }
                out.children.insert(path, children.clone());
                children
            }
            None => children,
        };
        if deferred {
            return;
        }
        // Another instance of the key claimed its descendants first: index
        // this one's now, as its own subtree.
        if !descended {
            self.walk(composed, graph.as_ref());
            return;
        }
        pending.extend(composed.into_iter().rev());
    }
    /// Gives each deferred instance its descendants: copies of its
    /// representative's, re-rooted, when their records do not depend on where
    /// the instance is; composed otherwise.
    ///
    /// Instances with one key read the same specs below them through the same
    /// sites: only the sites of the stage's own layer stack, which contribute
    /// nothing beneath an instance, name the instance.
    ///
    /// Spec: AOUSD Core §11.3.3 (instances share prototypes). OpenUSD:
    /// `Usd_InstanceCache`, `PcpInstanceKey`.
    fn share_prototypes(&mut self) {
        // Whether a representative's descendants can be shared is decided
        // by what was composed beneath it, which giving deferred instances
        // their descendants does not change: those are written beneath the
        // instances, and an instance nested beneath a representative gets
        // its own only after every instance copying that representative.
        let mut shareable: HashMap<PathId, bool> = HashMap::new();
        let mut spent = Vec::new();
        for (_, representative, graph) in &mut self.instances.deferred {
            let can_share = *shareable
                .entry(*representative)
                .or_insert_with(|| prototype_is_shareable(self.ctx, self.out, *representative));
            // The index of an instance that copies its representative's
            // descendants is not read again: it is freed on a spare thread
            // while the copies are made.
            if can_share {
                spent.extend(graph.take());
            }
        }
        if spent.len() < SPENT_ON_THREAD {
            self.give_descendants(shareable);
            return;
        }
        let threads = self.threads;
        crate::parallel::join(threads, || self.give_descendants(shareable), || drop(spent));
    }

    /// Gives each deferred instance its descendants (see
    /// [`Self::share_prototypes`]), `shareable` holding whether the
    /// descendants of the representatives decided so far can be shared.
    fn give_descendants(&mut self, mut shareable: HashMap<PathId, bool>) {
        // Composing an instance's descendants can defer the instances nested
        // in them in turn.
        while let Some((instance, representative, graph)) = self.instances.next() {
            // Once the budget is spent, no prim is composed after.
            if self.budget.is_spent() {
                break;
            }
            let can_share = *shareable
                .entry(representative)
                .or_insert_with(|| prototype_is_shareable(self.ctx, self.out, representative));
            if !can_share {
                let children = self
                    .out
                    .children
                    .get(&instance)
                    .cloned()
                    .unwrap_or_default();
                let paths = self.ctx.store.paths();
                let roots: Vec<PathId> = children
                    .into_iter()
                    .filter(|child| self.scope.includes(paths, *child))
                    .collect();
                self.walk(roots, graph.as_ref());
                continue;
            }
            copy_prototype(
                self.ctx,
                self.out,
                self.instances,
                self.budget,
                representative,
                instance,
            );
        }
    }
}

/// Whether the descendants of the instance `representative` compose the
/// same records wherever the instance is, once the paths they name beneath
/// it are re-rooted: no value clip in their opinions (clips resolve per
/// prim path), and no error or asset request reported for them.
fn prototype_is_shareable(ctx: &Ctx<'_>, out: &Composition, representative: PathId) -> bool {
    let paths = ctx.store.paths();
    let root = paths.resolve(representative).clone();
    let beneath = |path: PathId| path != representative && root.is_prefix_of(paths.resolve(path));
    let clips = ctx.store.tokens().lookup("clips");
    let clip_sets = ctx.store.tokens().lookup("clipSets");
    if clips.is_some() || clip_sets.is_some() {
        for (path, index) in &out.prims {
            if beneath(*path)
                && index
                    .opinions
                    .iter()
                    .any(|opinion| Some(opinion.field) == clips || Some(opinion.field) == clip_sets)
            {
                return false;
            }
        }
    }
    !out.errors
        .iter()
        .any(|error| error.prim().is_some_and(beneath))
        && !out
            .asset_requests
            .iter()
            .any(|request| beneath(request.prim))
        && !out.pending.iter().any(|pending| beneath(pending.prim))
}

/// Re-roots the paths beneath `from` that the opinions of `index` name
/// (relationship targets, connections, path list fields and path
/// expressions) beneath `to`, detaching only opinions that name one.
fn reroot_opinion_paths(store: &dyn LayerStore, index: &mut PrimIndex, from: PathId, to: PathId) {
    use crate::doc::FieldValue;
    use crate::path::TargetPath;
    use crate::path_expression::value::{anchor_value_with, has_path_expression};
    use crate::prim_index::OpinionValue;
    let names_path = |value: &OpinionValue| match value {
        OpinionValue::Property(spec) => {
            spec.targets.is_some()
                || spec.default.as_ref().is_some_and(has_path_expression)
                || spec
                    .time_samples
                    .get()
                    .is_some_and(|samples| samples.iter().any(|(_, v)| has_path_expression(v)))
        }
        OpinionValue::Field(FieldValue::PathListOp(_)) => true,
        OpinionValue::Field(FieldValue::Value(value)) => has_path_expression(value),
        OpinionValue::Field(_) => false,
    };
    if !index
        .opinions
        .iter()
        .any(|opinion| names_path(&opinion.value))
    {
        return;
    }
    let reroot = |store: &dyn LayerStore, path: PathId| {
        store
            .paths()
            .intern_reparented(path, from, to)
            .unwrap_or(path)
    };
    let reroot_target = |store: &dyn LayerStore, target: TargetPath| match target {
        TargetPath::Prim(path) => TargetPath::Prim(reroot(store, path)),
        TargetPath::Property(path) => TargetPath::Property(crate::path::PropertyPath::new(
            reroot(store, path.prim_path()),
            path.property(),
        )),
    };
    let from_names: Vec<alloc::string::String> = {
        let paths = store.paths();
        paths
            .resolve(from)
            .segments()
            .iter()
            .map(|t| alloc::string::String::from(store.tokens().resolve(*t)))
            .collect()
    };
    let to_names: Vec<alloc::string::String> = {
        let paths = store.paths();
        paths
            .resolve(to)
            .segments()
            .iter()
            .map(|t| alloc::string::String::from(store.tokens().resolve(*t)))
            .collect()
    };
    let mut reroot_names = |names: &[alloc::string::String]| -> Option<Vec<alloc::string::String>> {
        if !names.starts_with(&from_names) {
            return Some(names.to_vec());
        }
        let mut out = to_names.clone();
        out.extend_from_slice(&names[from_names.len()..]);
        Some(out)
    };
    for opinion in &mut index.opinions {
        if !names_path(&opinion.value) {
            continue;
        }
        match &mut opinion.value {
            OpinionValue::Property(spec) => {
                let spec = Arc::make_mut(spec);
                if let Some(targets) = spec.targets.as_mut() {
                    for list in targets.lists_mut() {
                        for target in list.iter_mut() {
                            *target = reroot_target(store, *target);
                        }
                    }
                }
                if let Some(value) = spec.default.as_mut() {
                    anchor_value_with(value, &[], &mut reroot_names);
                }
                if let Some(samples) = spec.time_samples.get_mut() {
                    for (_, value) in samples {
                        if has_path_expression(value) {
                            anchor_value_with(value, &[], &mut reroot_names);
                        }
                    }
                }
            }
            OpinionValue::Field(FieldValue::PathListOp(list)) => {
                for items in list.lists_mut() {
                    for target in items.iter_mut() {
                        *target = reroot_target(store, *target);
                    }
                }
            }
            OpinionValue::Field(FieldValue::Value(value)) => {
                anchor_value_with(value, &[], &mut reroot_names);
            }
            OpinionValue::Field(_) => {}
        }
    }
}

/// Copies the descendants of the instance `from` beneath the instance
/// `to`, re-rooting the sites of the stage's layer stack.
fn copy_prototype(
    ctx: &Ctx<'_>,
    out: &mut Composition,
    instances: &mut Instances,
    budget: &Budget,
    from: PathId,
    to: PathId,
) {
    let stage_stack = ctx.stacks.get(ctx.stage_stack).identifier;
    let mut pending: Vec<PathId> = out.children.get(&from).cloned().unwrap_or_default();
    let mut moved: Vec<(PathId, PathId)> = Vec::new();
    while let Some(source) = pending.pop() {
        let Some(target) = ctx.store.paths().intern_reparented(source, from, to) else {
            continue;
        };
        moved.push((source, target));
        if let Some(children) = out.children.get(&source) {
            pending.extend(children.iter().copied());
        }
    }
    // The copies count their nodes as the indexes built do: an instance
    // whose copied descendants reach the limit gets none of them.
    let nodes: usize = moved
        .iter()
        .filter_map(|(source, _)| out.prims.get(source))
        .map(|index| index.graph.len())
        .sum();
    if let Some(value) = budget.spend(nodes) {
        out.errors
            .push(CompositionError::LimitExceeded(LimitExceeded {
                limit: CompositionLimit::PrimIndexNodes,
                value,
                prim: to,
            }));
        return;
    }
    out.prim_index_nodes += nodes;
    for &(source, target) in &moved {
        let Some(index) = out.prims.get(&source) else {
            continue;
        };
        let mut index = index.clone();
        index
            .graph
            .reroot_stage_sites(ctx.store.paths(), stage_stack, from, to);
        reroot_opinion_paths(ctx.store, &mut index, from, to);
        let paths = ctx.store.paths();
        out.prims.insert(target, index);
        out.reused_prim_indexes += 1;
        // A nested instance still waiting for its own representative's
        // descendants waits beneath the copy too.
        if let Some(&nested) = instances.waiting.get(&source) {
            // The copy's index is not kept apart: its descendants, if
            // composed, start from indexes of their ancestors built anew.
            instances.defer(target, nested, None);
        }
        let map = |path: PathId, paths: &crate::path::PathInterner| {
            paths.intern_reparented(path, from, to).unwrap_or(path)
        };
        if let Some(children) = out.children.get(&source).cloned() {
            let children = children
                .into_iter()
                .map(|child| map(child, paths))
                .collect();
            out.children.insert(target, children);
        }
        if out.inactive.contains(&source) {
            out.inactive.insert(target);
        }
        if out.instances.contains(&source) {
            out.instances.insert(target);
        }
        if out.loadable.contains(&source) {
            out.loadable.insert(target);
        }
        if let Some(layers) = out.used_layer_sites.get(&source).cloned() {
            out.used_layer_sites.insert(target, layers);
        }
        if let Some(record) = out.records.get(&source).cloned() {
            let record = PrimDependencies {
                arcs: record
                    .arcs
                    .into_iter()
                    .map(|(site, kind, layer)| {
                        let site = if layer == stage_stack.root {
                            map(site, paths)
                        } else {
                            site
                        };
                        (site, kind, layer)
                    })
                    .collect(),
                ..record
            };
            out.records.insert(target, record);
        }
    }
}

/// Marks the subtrees of `graph` that contribute no specs culled.
/// OpenUSD: `_CullSubtreesWithNoOpinions`.
fn cull(graph: &mut Graph, paths: &crate::path::PathInterner, stage_stack: StackId) {
    for child in graph.child_list(0) {
        cull_subtree(graph, paths, child, stage_stack);
    }
}

fn cull_subtree(
    graph: &mut Graph,
    paths: &crate::path::PathInterner,
    node: NodeIndex,
    stage_stack: StackId,
) {
    for child in graph.child_list(node) {
        cull_subtree(graph, paths, child, stage_stack);
    }
    if can_be_culled(graph, paths, node, stage_stack) {
        graph.set_culled(paths, node);
    }
}

/// OpenUSD: `_NodeCanBeCulled`.
fn can_be_culled(
    graph: &Graph,
    paths: &crate::path::PathInterner,
    node: NodeIndex,
    stage_stack: StackId,
) -> bool {
    let entry = graph.node(node);
    if entry.culled {
        return true;
    }
    if node == 0 {
        return false;
    }
    // A node that adds an arc at this prim records a dependency.
    if graph.depth_below_introduction(paths, node) == 0 {
        return false;
    }
    if entry.has_value_clips {
        return false;
    }
    if entry.arc_kind == ArcKind::Inherits && entry.site.stack == stage_stack {
        let origin = if entry.origin == entry.parent {
            node
        } else {
            origin_root(graph, node)
        };
        let at_intro = graph.path_at_introduction(paths, origin);
        let root_prim =
            !at_intro.has_variant_selections() && paths.resolve(at_intro.prim_path()).depth() == 1;
        if !root_prim {
            return false;
        }
    }
    if graph.children(node).any(|child| !graph.node(child).culled) {
        return false;
    }
    !(entry.has_specs && entry.can_contribute_specs())
}

/// The first node of `node`'s chain of origins whose origin is its parent.
/// OpenUSD: `PcpNodeRef::GetOriginRootNode`.
fn origin_root(graph: &Graph, node: NodeIndex) -> NodeIndex {
    let mut cursor = node;
    while graph.node(cursor).origin != graph.node(cursor).parent
        && graph.node(cursor).origin != NONE
    {
        cursor = graph.node(cursor).origin;
    }
    cursor
}

/// The composed child names of the prim `graph` indexes, weakest site
/// first. OpenUSD: `PcpPrimIndex::ComputePrimChildNames`.
fn child_names(ctx: &Ctx<'_>, graph: &Graph) -> Vec<TokenId> {
    let mut order = Vec::new();
    let mut names = HashSet::new();
    let mut prohibited = HashSet::new();
    let mut unlisted = Vec::new();
    let nodes: Vec<NodeIndex> = if graph.instanceable {
        let mut out = Vec::new();
        instanceable_weak_to_strong(graph, 0, true, &mut out);
        out
    } else {
        let mut all = graph.strength_order();
        all.reverse();
        all
    };
    // Weak to strong: a node's subtree before the node, children weakest
    // first.
    for node in nodes {
        let entry = graph.node(node);
        if !entry.can_contribute_specs() {
            continue;
        }
        let stack = ctx.stacks.get(entry.site.stack);
        if stack.has_relocates() && !entry.site.path.has_variant_selections() {
            apply_relocated_names(
                ctx,
                entry.site.stack,
                entry.site.path.prim_path(),
                &mut order,
                &mut names,
                &mut prohibited,
            );
        }
        let stack = ctx.stacks.get(entry.site.stack);
        site::compose_child_names(
            ctx.store,
            stack,
            &entry.site.path,
            ctx.child_index,
            &mut order,
            &mut names,
            &mut unlisted,
        );
    }
    if !unlisted.is_empty() {
        let tokens = ctx.store.tokens();
        tokens.sort_by_text(&mut unlisted);
        for name in unlisted {
            if names.insert(name) {
                order.push(name);
            }
        }
    }
    if !prohibited.is_empty() {
        order.retain(|name| !prohibited.contains(name));
    }
    order
}

/// The nodes of an instance's index whose names its children compose,
/// weakest first. OpenUSD: `Pcp_TraverseInstanceableWeakToStrong` with
/// `Pcp_PrimChildNameVisitor`.
fn instanceable_weak_to_strong(
    graph: &Graph,
    node: NodeIndex,
    is_root: bool,
    out: &mut Vec<NodeIndex>,
) {
    let entry = graph.node(node);
    if entry.culled {
        return;
    }
    let children = graph.child_list(node);
    for child in children.into_iter().rev() {
        instanceable_weak_to_strong(graph, child, false, out);
    }
    let instanceable =
        !is_root && entry.transitive_direct && entry.can_contribute_specs() && entry.has_specs;
    if instanceable {
        out.push(node);
    }
}

/// Applies the relocations of `stack` that move children of `parent` to
/// the names gathered so far. OpenUSD: the relocation part of
/// `_ComposePrimChildNamesAtNode`.
fn apply_relocated_names(
    ctx: &Ctx<'_>,
    stack: StackId,
    parent: PathId,
    order: &mut Vec<TokenId>,
    names: &mut HashSet<TokenId>,
    prohibited: &mut HashSet<TokenId>,
) {
    let paths = ctx.store.paths();
    let relocations = &ctx.stacks.get(stack).relocations;
    let mut to_add: Vec<TokenId> = Vec::new();
    let mut to_remove: HashSet<TokenId> = HashSet::new();
    let mut to_replace: HashMap<TokenId, TokenId> = HashMap::new();
    for relocate in relocations.iter() {
        let source_parent = paths.parent(relocate.source);
        let source_name = paths.resolve(relocate.source).leaf();
        if source_parent == Some(parent) {
            let Some(source_name) = source_name else {
                continue;
            };
            match relocate.target {
                Some(target) if paths.parent(target) == Some(parent) => {
                    if let Some(name) = paths.resolve(target).leaf() {
                        to_replace.insert(source_name, name);
                    }
                }
                _ => {
                    to_remove.insert(source_name);
                }
            }
            prohibited.insert(source_name);
        }
        if let Some(target) = relocate.target
            && paths.parent(target) == Some(parent)
            && source_parent != Some(parent)
            && let Some(name) = paths.resolve(target).leaf()
            && !names.contains(&name)
        {
            to_add.push(name);
        }
    }
    if !to_replace.is_empty() || !to_remove.is_empty() {
        let mut kept = Vec::with_capacity(order.len());
        for name in order.iter().copied() {
            if let Some(new_name) = to_replace.get(&name) {
                names.remove(&name);
                if names.insert(*new_name) {
                    kept.push(*new_name);
                }
            } else if !to_remove.contains(&name) {
                kept.push(name);
            } else {
                names.remove(&name);
            }
        }
        *order = kept;
    }
    let tokens = ctx.store.tokens();
    tokens.sort_by_text(&mut to_add);
    to_add.dedup();
    for name in to_add {
        if names.insert(name) {
            order.push(name);
        }
    }
}

/// Whether the prim `graph` indexes is active: its strongest `active`
/// opinion, on a prim spec or a selected variant branch, `true` without
/// one.
fn is_active(ctx: &Ctx<'_>, graph: &Graph) -> bool {
    for node in graph.strength_order() {
        let entry = graph.node(node);
        if !entry.can_contribute_specs() || !entry.has_specs {
            continue;
        }
        let stack = ctx.stacks.get(entry.site.stack);
        for (_, _, spec) in site::specs(ctx.store, stack, &entry.site.path) {
            if let Some(active) = spec.active(ctx.store.tokens()) {
                return active;
            }
        }
    }
    true
}

/// The prim index the stage reads: `graph`'s nodes and the opinions of
/// their specs, strongest first, each node's target paths mapped into the
/// stage namespace.
/// The index's graph shares its shape with `like`, the graph of the
/// prim's parent, when they are the same, and otherwise with any graph
/// of the composition that has it ([`PrimIndexGraph::from_ordered_in`]).
fn prim_index(
    ctx: &Ctx<'_>,
    prim: PathId,
    graph: &Graph,
    like: Option<&Arc<crate::prim_index_graph::GraphShape>>,
    checks: &mut crate::composition_checks::Findings,
) -> PrimIndex {
    let nodes = graph.nodes.iter().map(|node| {
        let to_id =
            |index: NodeIndex| (index != NONE).then(|| NodeId::from_raw_index(index as usize));
        let origin = if node.origin == node.parent {
            None
        } else {
            to_id(node.origin)
        };
        let arc = NodeArc {
            arc_kind: node.arc_kind,
            layer_stack: ctx.stacks.get(node.site.stack).identifier,
            site: node.site.path.clone(),
            namespace_depth: node.namespace_depth,
            sibling_index: node.sibling_num,
            implied: is_class_based(node.arc_kind) && node.origin != node.parent,
            skips_duplicates: false,
        };
        // Borrowed: most graphs' shapes are held already, and only a new
        // one takes a count of its stacks' offsets.
        let stack_offsets = &ctx.stacks.get(node.site.stack).layer_offsets;
        (
            to_id(node.parent),
            origin,
            arc,
            node.map_to_root.offset(),
            stack_offsets,
        )
    });
    let mut index = PrimIndex::new(PrimIndexGraph::from_ordered_in(nodes, &ctx.shapes, like));
    // At most one source per layer of each contributing node's stack.
    index.reserve_sources(
        graph
            .nodes
            .iter()
            .filter(|node| node.can_contribute_specs() && node.has_specs)
            .map(|node| ctx.stacks.get(node.site.stack).layers.len())
            .sum(),
    );
    for (at, node) in graph.nodes.iter().enumerate() {
        if !node.can_contribute_specs() || !node.has_specs {
            continue;
        }
        let id = NodeId::from_raw_index(at);
        let node_offset = node.map_to_root.offset();
        let lookup_path = node.site.path.prim_path();
        // Paths authored across an arc name sites of its target's
        // namespace: they compose mapped through the arcs to the root.
        let maps = !node.map_to_root.is_identity();
        // The class or arc target the node's specs are authored beneath, at
        // the namespace depth it was introduced at: a target a class
        // authors inside itself names no instance of it. Only an arc that
        // maps paths reads it.
        let arc_root = maps.then(|| {
            let at_intro = graph.path_at_introduction(ctx.store.paths(), graph::node_index(at));
            ctx.store.paths().resolve(at_intro.prim_path()).clone()
        });
        let stack = ctx.stacks.get(node.site.stack);
        for (strength, layer_id, spec) in site::specs(ctx.store, stack, &node.site.path) {
            let layer_strength = u16::try_from(strength).unwrap_or(u16::MAX);
            let layer_offset = node_offset.compose(stack.offset_at(strength));
            // The spec's opinions share its source's key and offset
            // (AOUSD Core §12.3.2.1), as OpenUSD's prim stack entries share
            // their node and layer.
            let source = index.add_source(OpinionSource {
                key: OpinionKey {
                    node: id,
                    layer_strength,
                    layer_id,
                    lookup_path,
                    spec_path: node.site.path.clone(),
                },
                layer_offset,
            });
            let entries = crate::doc::composed_entries(spec.fields(), spec.properties());
            index.add_opinions(entries.map(|entry| {
                let (field, mut value) = (entry.name(), entry.value());
                // Relative expressions anchor at every node, the root's
                // included.
                map_path_expressions(ctx.store, &node.map_to_root, lookup_path, &mut value);
                if let Some(arc_root) = &arc_root {
                    let map_to_root = &node.map_to_root;
                    let map = |store: &dyn LayerStore, path: PathId| {
                        map_to_root.map_source_to_target(store.paths(), path)
                    };
                    crate::composition_checks::map_arc_targets(
                        ctx.store,
                        &mut value,
                        crate::composition_checks::ArcPathMap {
                            arc: node.arc_kind,
                            source: arc_root,
                            map: &map,
                            root_identity: map_to_root.has_root_identity(),
                        },
                        crate::composition_checks::TargetOwner {
                            prim,
                            property: field,
                            layer: layer_id,
                            prim_spec: &node.site.path,
                        },
                        checks,
                    );
                }
                Opinion {
                    source,
                    field,
                    value,
                }
            }));
        }
    }
    // Most prims have one source, which a vector grown by pushes would
    // give room for four; the rest give back what their records outgrew.
    index.trim();
    index.finalize();
    index
}

/// Orders `children`, child prims of the prim `index` composes, as
/// composition orders them: by the names its contributing sites author,
/// weakest site first ([`child_names`]); children no site names follow,
/// by name. Live recomposition uses this to rebuild the list of a
/// boundary parent its partial composition did not index; for the others
/// it takes the whole child list that composition kept
/// ([`Composition::boundary_children`]).
pub(crate) fn order_children(
    store: &dyn LayerStore,
    index: &PrimIndex,
    child_index: &ChildIndex,
    children: &mut [PathId],
) {
    child_index.prepare(store);
    let graph = &index.graph;
    let contributing: HashSet<NodeId> =
        index.sources.iter().map(|source| source.key.node).collect();
    let mut order: Vec<TokenId> = Vec::new();
    let mut names: HashSet<TokenId> = HashSet::new();
    let mut unlisted: Vec<TokenId> = Vec::new();
    for id in graph.strength_order().into_iter().rev() {
        if !contributing.contains(&id) {
            continue;
        }
        let Some(node) = graph.node(id) else {
            continue;
        };
        let layers: Vec<LayerId> = node.stack_layers().collect();
        site::compose_child_names_in(
            store,
            &layers,
            node.site(),
            child_index,
            &mut order,
            &mut names,
            &mut unlisted,
        );
    }
    let tokens = store.tokens();
    tokens.sort_by_text(&mut unlisted);
    for name in unlisted {
        if names.insert(name) {
            order.push(name);
        }
    }
    let position: HashMap<TokenId, usize> =
        order.iter().enumerate().map(|(at, n)| (*n, at)).collect();
    let paths = store.paths();
    // Each child's place is looked up once, not at every comparison: a
    // wide parent's children are sorted for every child added to it.
    let mut placed: Vec<(Option<usize>, PathId)> = children
        .iter()
        .map(|&child| {
            let name = paths.resolve(child).leaf();
            (name.and_then(|n| position.get(&n).copied()), child)
        })
        .collect();
    sort::sort_by(&mut placed, |(pa, a), (pb, b)| match (pa, pb) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => core::cmp::Ordering::Less,
        (None, Some(_)) => core::cmp::Ordering::Greater,
        (None, None) => paths.resolve(*a).cmp_with_tokens(paths.resolve(*b), tokens),
    });
    for (slot, (_, child)) in children.iter_mut().zip(placed) {
        *slot = child;
    }
}

/// Anchors the path expressions `value` authors at `site` (a node's site
/// prim) and maps them into the stage namespace through `map_to_root`.
/// OpenUSD: `PcpMapFunction::MapSourceToTarget(SdfPathExpression)`.
fn map_path_expressions(
    store: &dyn LayerStore,
    map_to_root: &map_function::MapFunction,
    site: PathId,
    value: &mut crate::prim_index::OpinionValue,
) {
    use crate::path_expression::value::{anchor_value_with, has_path_expression};
    let (default, samples) = match &*value {
        crate::prim_index::OpinionValue::Field(crate::doc::FieldValue::Value(v)) => (Some(v), None),
        crate::prim_index::OpinionValue::Property(spec) => {
            (spec.default.as_ref(), spec.time_samples.get())
        }
        crate::prim_index::OpinionValue::Field(_) => return,
    };
    let default_changes = default.is_some_and(has_path_expression);
    let samples_change =
        samples.is_some_and(|samples| samples.iter().any(|(_, v)| has_path_expression(v)));
    if !default_changes && !samples_change {
        return;
    }
    let anchor: Vec<alloc::string::String> = store
        .paths()
        .resolve(site)
        .segments()
        .iter()
        .map(|name| alloc::string::String::from(store.tokens().resolve(*name)))
        .collect();
    let mut map = |names: &[alloc::string::String]| -> Option<Vec<alloc::string::String>> {
        let segments: Vec<TokenId> = names
            .iter()
            .map(|name| store.tokens().intern(name))
            .collect();
        let path = store.paths().intern_segments(&segments);
        let mapped = map_to_root.map_source_to_target(store.paths(), path)?;
        Some(
            store
                .paths()
                .resolve(mapped)
                .segments()
                .iter()
                .map(|name| alloc::string::String::from(store.tokens().resolve(*name)))
                .collect(),
        )
    };
    match value {
        crate::prim_index::OpinionValue::Field(crate::doc::FieldValue::Value(v)) => {
            anchor_value_with(v, &anchor, &mut map);
        }
        crate::prim_index::OpinionValue::Property(spec) => {
            let spec = Arc::make_mut(spec);
            if default_changes && let Some(v) = spec.default.as_mut() {
                anchor_value_with(v, &anchor, &mut map);
            }
            if samples_change && let Some(samples) = spec.time_samples.get_mut() {
                for (_, v) in samples {
                    if has_path_expression(v) {
                        anchor_value_with(v, &anchor, &mut map);
                    }
                }
            }
        }
        crate::prim_index::OpinionValue::Field(_) => {}
    }
}

/// Records the errors indexing a prim found, the errors and sublayer
/// requests of a layer stack with the first prim to use it.
fn record_errors(out: &mut Composition, ctx: &Ctx<'_>, errors: Vec<Recorded>) {
    for recorded in errors {
        match recorded {
            Recorded::Error(error) => out.errors.push(error),
            Recorded::StackErrors { stack, prim } => {
                if out.reported_stacks.insert(stack) {
                    let stack = ctx.stacks.get(stack);
                    out.asset_requests.extend(sublayer_requests(stack, prim));
                    for mut error in stack.errors.iter().cloned() {
                        if let CompositionError::VariableExpressionError(error) = &mut error {
                            error.prim = Some(prim);
                        }
                        out.errors.push(error);
                    }
                }
            }
        }
    }
}

/// The asset requests for the sublayers `stack` names that the store has
/// not resolved, each [`AssetUse::Sublayer`] at `prim`, the first prim to read
/// the stack.
fn sublayer_requests(
    stack: &layer_stacks::ComputedStack<'_>,
    prim: PathId,
) -> impl Iterator<Item = AssetRequest> {
    stack.requests.iter().map(move |request| AssetRequest {
        anchor: request.anchor,
        asset_path: request.asset_path.clone(),
        prim,
        used_by: AssetUse::Sublayer,
    })
}

/// Lists inactive children apart and deduplicates the errors.
///
/// Every composed child name is a prim, whether or not its index has
/// specs: names come from the sites of the parent's index, or from a
/// relocation, as OpenUSD populates them (`UsdStage::_ComposeChildren`).
fn finish(out: &mut Composition) {
    // A child list names only the prims kept.
    for parent in core::mem::take(&mut out.graph_only_parents) {
        if let Some(list) = out.children.get_mut(&parent) {
            list.retain(|child| out.prims.contains_key(child));
        }
    }
    // Prims after the one that spent the budget are not composed.
    if out.spent {
        let prims = &out.prims;
        for list in out.children.values_mut() {
            list.retain(|child| prims.contains_key(child));
        }
    }
    // A prim waiting for layers is not composed.
    if !out.pending.is_empty() {
        let pending: HashSet<PathId> = out.pending.iter().map(|pending| pending.prim).collect();
        for list in out.children.values_mut() {
            list.retain(|child| !pending.contains(child));
        }
    }
    // Inactive children are listed apart.
    let inactive = &out.inactive;
    for (parent, list) in out.children.iter_mut() {
        if list.iter().any(|child| inactive.contains(child)) {
            out.inactive_children.insert(*parent, list.clone());
            list.retain(|child| !inactive.contains(child));
        }
    }
    out.children.retain(|_, list| !list.is_empty());
    out.inactive.retain(|prim| out.prims.contains_key(prim));
    let mut seen = HashSet::new();
    out.errors.retain(|error| seen.insert(error.clone()));
}

/// The index of the prim at `path`, composed as [`compose`] composes it but
/// with no node culled, at the prim or at any of its ancestors: every node
/// an arc reaches is kept, whether or not a layer of its stack has a spec
/// at its site. Composition culls such nodes, so a site no spec is at yet
/// has no node in a stage's index; this one places it with its strength.
///
/// Spec: AOUSD Core §10.4 (strength ordering). OpenUSD:
/// `UsdPrim::ComputeExpandedPrimIndex`, `PcpComputePrimIndex` with
/// `PcpPrimIndexInputs::Cull(false)`.
pub(crate) fn expanded_prim_index(
    store: &dyn LayerStore,
    root: LayerId,
    options: &StageOptions,
    path: PathId,
) -> PrimIndex {
    let child_index = ChildIndex::default();
    child_index.prepare(store);
    let load_rules = options.load_rules.clone();
    let include_payload = move |paths: &crate::path::PathInterner, path: PathId| {
        load_rules.is_loaded(paths.resolve(path))
    };
    let mut ctx = Ctx {
        store,
        stacks: LayerStacks::muting(&options.muted_layers),
        child_index: &child_index,
        fallbacks: &options.variant_fallbacks,
        include_payload: &include_payload,
        stage_stack: StackId(0),
        root_path: store.paths().intern_segments(&[]),
        dynamic_formats: &options.dynamic_formats,
        schemas: options.schemas.as_deref(),
        any_type: store.tokens().lookup("__AnyType__"),
        shapes: crate::prim_index_graph::GraphShapes::default(),
        nodes_per_prim_index: options.limits.nodes_per_prim_index,
    };
    let identifier = LayerStackIdentifier {
        root,
        session: options.session_layer,
    };
    // A stage stack that reads layers the store has not loaded yet has no
    // prims to index.
    let Ok(stage_stack) = ctx.stacks.find_or_compute(ctx.store, identifier, None) else {
        return PrimIndex::default();
    };
    ctx.stage_stack = stage_stack;
    // The prim and its ancestors, the pseudo-root first.
    let mut chain = alloc::vec![path];
    while let Some(parent) = store.paths().parent(*chain.last().expect("a path")) {
        chain.push(parent);
    }
    let mut parent: Option<Graph> = None;
    for &at in chain.iter().rev() {
        let site = Site {
            stack: ctx.stage_stack,
            path: SpecPath::from_prim_path(at),
        };
        let mut outputs = Outputs::default();
        build_prim_index(
            &ctx,
            &site,
            &site,
            true,
            true,
            true,
            None,
            parent.as_ref(),
            &mut outputs,
        );
        let mut graph = core::mem::take(&mut outputs.graph);
        graph.instanceable = at != ctx.root_path && is_instanceable(ctx.store, &ctx.stacks, &graph);
        graph.finalize();
        parent = Some(graph);
    }
    let graph = parent.expect("the prim's graph");
    prim_index(
        &ctx,
        path,
        &graph,
        None,
        &mut crate::composition_checks::Findings::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::{Scope, Selection};
    use crate::{InMemoryStore, PathId, stage::PopulationMask};

    /// A mask scope includes what [`PopulationMask::includes`] does, and an
    /// exact scope the selected paths and their ancestors, by ancestor
    /// walks rather than a scan of the selected paths.
    #[test]
    fn selection_scopes_include_what_the_mask_selects() {
        let store = InMemoryStore::default();
        let paths: alloc::vec::Vec<PathId> = [
            "/", "/A", "/A/B", "/A/B/C", "/A/D", "/E", "/E/F", "/E/F/G", "/H",
        ]
        .into_iter()
        .map(|path| store.path(path))
        .collect();
        for include in [
            alloc::vec![],
            alloc::vec![store.path("/A/B"), store.path("/E/F")],
            alloc::vec![store.path("/"), store.path("/E")],
        ] {
            let mask = PopulationMask {
                include: include.clone(),
            };
            let interner = store.paths();
            let scope = Scope::Mask(Selection::new(interner, &include));
            let exact = Scope::Exact(Selection::new(interner, &include));
            for &path in &paths {
                assert_eq!(
                    scope.includes(interner, path),
                    mask.includes(path, interner),
                    "{path:?} in {include:?}"
                );
                let above = include
                    .iter()
                    .any(|s| interner.resolve(path).is_prefix_of(interner.resolve(*s)));
                assert_eq!(exact.includes(interner, path), above, "{path:?}");
            }
        }
    }
}
