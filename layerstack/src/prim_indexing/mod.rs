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
pub(crate) mod site;
pub(crate) mod site_paths;

use alloc::{sync::Arc, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::{
    asset::AssetRequest,
    composition_error::CompositionError,
    dependency_map::{ArcDependency, DependencyBuilder},
    doc::{LayerId, LayerStore},
    expression_variables::VariableReads,
    interner::TokenId,
    layer_stack::LayerStackIdentifier,
    path::PathId,
    prim_index::{ArcKind, Opinion, OpinionKey, PrimIndex},
    prim_index_graph::{NodeArc, NodeId, PrimIndexGraph},
    spec_path::SpecPath,
    stage::StageOptions,
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
    pub(crate) deps: Option<DependencyBuilder>,
    pub(crate) used_layers: HashSet<LayerId>,
    pub(crate) used_layer_sites: HashMap<PathId, HashSet<LayerId>>,
    pub(crate) local_layers: Vec<LayerId>,
    pub(crate) composed_prim_indexes: usize,
    pub(crate) reused_prim_indexes: usize,
    pub(crate) reads: VariableReads,
    /// The whole child list of each requested boundary parent the
    /// composition indexed, whatever its scope lists ([`compose`]).
    pub(crate) boundary_children: HashMap<PathId, Vec<PathId>>,
    /// What each composed prim depends on, applied to `deps` at the end.
    records: HashMap<PathId, PrimRecord>,
    /// The layer stacks whose errors a prim has reported.
    reported_stacks: HashSet<StackId>,
    /// The layer stacks whose layers are in `used_layers`.
    used_stacks: HashSet<StackId>,
}

/// The dependencies one prim index records (see [`DependencyBuilder`]).
#[derive(Clone, Default)]
struct PrimRecord {
    layers: Vec<LayerId>,
    default_prim_layers: Vec<LayerId>,
    arcs: Vec<(PathId, ArcKind, LayerId)>,
}

/// Which prims a composition indexes.
pub(crate) enum Scope<'m> {
    /// Every prim.
    All,
    /// The prims a population mask includes, and their ancestors.
    Mask(&'m crate::stage::PopulationMask),
    /// Exactly the listed prims and their ancestors, each with its whole
    /// child list.
    Exact(&'m [PathId]),
    /// Exactly the listed prims, whose child lists name only listed prims
    /// (the live stage's local namespace updates). Unlisted ancestors are
    /// indexed for their children to start from, but not kept.
    Listed {
        listed: &'m HashSet<PathId>,
        ancestors: HashSet<PathId>,
    },
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
            Self::Mask(mask) => {
                let included = mask.includes(child, paths);
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
            Self::Mask(mask) => mask.includes(path, paths),
            Self::Listed { listed, ancestors } => {
                listed.contains(&path)
                    || ancestors.contains(&path)
                    || paths.resolve(path).depth() == 0
            }
            Self::Exact(list) => {
                let target = paths.resolve(path);
                list.iter()
                    .any(|p| *p == path || target.is_prefix_of(paths.resolve(*p)))
            }
        }
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
    boundaries: &HashSet<PathId>,
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
        stacks: LayerStacks::default(),
        child_index,
        fallbacks: &options.variant_fallbacks,
        include_payload: &include_payload,
        stage_stack: StackId(0),
        root_path,
        dynamic_formats: &options.dynamic_formats,
        schemas: options.schemas.as_deref(),
    };
    let identifier = LayerStackIdentifier {
        root,
        session: options.session_layer,
    };
    ctx.stage_stack = ctx.stacks.find_or_compute(ctx.store, identifier, None);
    let mut out = Composition {
        local_layers: ctx.stacks.get(ctx.stage_stack).layers.clone(),
        deps: options.with_dependencies.then(DependencyBuilder::new),
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
    };
    let work = |prim: Pending, added: &mut Vec<Pending>| {
        index_prim(&ctx, &scope, boundaries, &claims, prim, added)
    };
    crate::parallel::with_pool(options.threads, &work, |pool| {
        let mut walk = Walk {
            ctx: &ctx,
            pool,
            out: &mut out,
            scope: &scope,
            checks,
            instances: &mut instances,
        };
        walk.walk(alloc::vec![root_path], None);
        walk.share_prototypes();
    });

    if let Some(deps) = out.deps.as_mut() {
        for (path, record) in &out.records {
            for &layer in &record.layers {
                deps.add_layer_opinion(layer, *path);
            }
            for &layer in &record.default_prim_layers {
                deps.add_default_prim_dependency(layer, *path);
            }
            for &(source, arc_kind, layer) in &record.arcs {
                deps.add_arc(ArcDependency {
                    source,
                    target: *path,
                    arc_kind,
                    layer,
                });
            }
        }
    }
    // The stage's own layer stack, and any no arc reached, report last.
    for (at, stack) in (0..).zip(ctx.stacks.iter()) {
        if !out.reported_stacks.contains(&StackId(at)) {
            out.errors.extend(stack.errors.iter().cloned());
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

/// The instances a composition has seen, by the key their prototype is
/// shared by.
#[derive(Default)]
struct Instances {
    /// The first instance composed with each key.
    representatives: HashMap<InstanceKey, PathId>,
    /// Instances whose descendants wait for their representative's, with
    /// the instance's index when it was indexed rather than copied.
    deferred: Vec<(PathId, PathId, Option<Arc<Graph>>)>,
}

/// What an instance's descendants are composed from: its instanceable
/// nodes, strongest first, each with its arc, layer stack, site and time
/// offset. OpenUSD: `PcpInstanceKey`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct InstanceKey(Vec<(ArcKind, StackId, SpecPath, u64, u64)>);

fn instance_key(graph: &Graph) -> InstanceKey {
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
    InstanceKey(out)
}

/// The instance keys whose descendants some instance is indexing, while
/// instances share prototypes.
struct Claims {
    /// Whether instances may share prototypes: every prim is composed and
    /// payloads load everywhere alike.
    share: bool,
    keys: ReadLock<HashSet<InstanceKey>>,
}

/// A prim to index, with its namespace parent's finalized index, which its
/// own starts from, when the parent was indexed in this composition.
type Pending = (PathId, Option<Arc<Graph>>);

/// What indexing one prim found, before it joins the composition.
struct Indexed {
    path: PathId,
    graph: Arc<Graph>,
    index: PrimIndex,
    outputs: Outputs,
    findings: crate::composition_checks::Findings,
    active: bool,
    /// The key of an active instance, while instances share prototypes.
    key: Option<InstanceKey>,
    /// Whether the children it composes were indexed with it.
    descended: bool,
    /// The children the scope lists, and those it composes.
    children: Vec<PathId>,
    composed_children: Vec<PathId>,
    /// Every child, for a requested boundary parent.
    boundary_children: Option<Vec<PathId>>,
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
    boundaries: &HashSet<PathId>,
    claims: &Claims,
    (path, parent): Pending,
    added: &mut Vec<Pending>,
) -> Indexed {
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
    drop(parent);
    let mut graph = core::mem::take(&mut outputs.graph);
    cull(&mut graph, ctx.store.paths(), ctx.stage_stack);
    graph.instanceable = path != ctx.root_path && is_instanceable(ctx.store, &ctx.stacks, &graph);
    graph.finalize();

    let names = child_names(ctx, &graph);
    let mut findings = crate::composition_checks::Findings::default();
    let mut index = prim_index(ctx, path, &graph, &mut findings);
    let dynamic = core::mem::take(&mut outputs.dynamic);
    index.dynamic = (!dynamic.is_empty()).then(|| Arc::new(dynamic));
    let active = path == ctx.root_path || is_active(ctx, &graph);
    // An exact scope lists every child but composes only the listed
    // prims and their ancestors: the live stage merges whole child
    // lists. A population mask lists only the children it includes.
    let paths = ctx.store.paths();
    let mut children = Vec::with_capacity(names.len());
    let mut composed_children = Vec::with_capacity(names.len());
    let mut boundary_children = boundaries
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
    let key = (claims.share && active && graph.instanceable).then(|| instance_key(&graph));
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
    Indexed {
        path,
        graph,
        index,
        outputs,
        findings,
        active,
        key,
        descended,
        children,
        composed_children,
        boundary_children,
    }
}

/// The prims a composition indexes on a pool of threads, joined into it
/// one by one in traversal order, as if indexed one by one: each prim's
/// records, errors and instance keys are recorded in the same order
/// whatever the number of threads.
struct Walk<'a, 'c, 'p> {
    ctx: &'a Ctx<'c>,
    pool: &'a crate::parallel::Pool<'p, Pending, Indexed>,
    out: &'a mut Composition,
    scope: &'a Scope<'a>,
    checks: &'a mut crate::composition_checks::Findings,
    instances: &'a mut Instances,
}

impl Walk<'_, '_, '_> {
    /// Indexes the prims of `roots`, children of the prim `parent` indexes
    /// (`None` when it was not indexed here), and their descendants, each
    /// from its parent's index, and joins them depth first, `roots` in
    /// order.
    fn walk(&mut self, roots: Vec<PathId>, parent: Option<&Arc<Graph>>) {
        let tasks = roots.iter().map(|&root| (root, parent.cloned())).collect();
        let indexed = self.pool.run(tasks);
        // One thread indexes prims in the order they join: a prim, then
        // the children it adds, depth first.
        if self.pool.in_order() {
            for prim in indexed {
                self.join(prim);
            }
            return;
        }
        let mut indexed: HashMap<PathId, Indexed> =
            indexed.into_iter().map(|prim| (prim.path, prim)).collect();
        let mut pending = roots;
        pending.reverse();
        while let Some(path) = pending.pop() {
            let prim = indexed.remove(&path).expect("indexed prim");
            if let Some(children) = self.join(prim) {
                pending.extend(children.into_iter().rev());
            }
        }
    }

    /// Joins the indexed `prim` into the composition, and returns the
    /// children to join after it, which are indexed, or `None` when there
    /// are none to join now.
    fn join(&mut self, prim: Indexed) -> Option<Vec<PathId>> {
        let Indexed {
            path,
            graph,
            index,
            outputs,
            findings,
            active,
            key,
            descended,
            children,
            composed_children,
            boundary_children,
        } = prim;
        let out = &mut *self.out;
        if let Some(all) = boundary_children {
            out.boundary_children.insert(path, all);
        }
        out.composed_prim_indexes += 1;
        self.checks.extend(findings);
        record(out, self.ctx, path, &graph, &index, outputs);
        if graph.has_payloads {
            out.loadable.insert(path);
        }
        let mut deferred = false;
        if graph.instanceable {
            out.instances.insert(path);
        }
        // An inactive instance composes no descendants to share. The first
        // instance of a key, in traversal order, composes them.
        if let Some(key) = key {
            match self.instances.representatives.get(&key) {
                Some(&representative) => {
                    self.instances
                        .deferred
                        .push((path, representative, Some(Arc::clone(&graph))));
                    deferred = true;
                }
                None => {
                    self.instances.representatives.insert(key, path);
                }
            }
        }
        let keep = self.scope.keeps(self.ctx.store.paths(), path);
        if keep {
            out.prims.insert(path, index);
        }
        if !active {
            if keep {
                out.inactive.insert(path);
            }
            return None;
        }
        if keep && !children.is_empty() {
            out.children.insert(path, children);
        }
        if deferred {
            return None;
        }
        // Another instance of the key claimed its descendants first: index
        // this one's now, as its own subtree.
        if !descended {
            self.walk(composed_children, Some(&graph));
            return None;
        }
        Some(composed_children)
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
        let mut shareable: HashMap<PathId, bool> = HashMap::new();
        // Composing an instance's descendants can defer the instances nested
        // in them in turn.
        while let Some((instance, representative, graph)) = self.instances.deferred.pop() {
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
            copy_prototype(self.ctx, self.out, self.instances, representative, instance);
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
                    .as_ref()
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
                if let Some(samples) = spec.time_samples.as_mut() {
                    for (_, value) in samples.make_mut() {
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
        if let Some(&(_, nested, _)) = instances
            .deferred
            .iter()
            .find(|(path, _, _)| *path == source)
        {
            // The copy's index is not kept apart: its descendants, if
            // composed, start from indexes of their ancestors built anew.
            instances.deferred.push((target, nested, None));
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
            let record = PrimRecord {
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
        unlisted.sort_by(|a, b| tokens.resolve(*a).cmp(tokens.resolve(*b)));
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
    to_add.sort_by(|a, b| tokens.resolve(*a).cmp(tokens.resolve(*b)));
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
fn prim_index(
    ctx: &Ctx<'_>,
    prim: PathId,
    graph: &Graph,
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
        let stack_offsets = ctx.stacks.get(node.site.stack).layer_offsets.clone();
        (
            to_id(node.parent),
            origin,
            arc,
            node.map_to_root.offset(),
            stack_offsets,
        )
    });
    let mut index = PrimIndex::new(PrimIndexGraph::from_ordered(nodes));
    for (at, node) in graph.nodes.iter().enumerate() {
        if !node.can_contribute_specs() || !node.has_specs {
            continue;
        }
        let id = NodeId::from_raw_index(at);
        let node_offset = node.map_to_root.offset();
        let lookup_path = node.site.path.prim_path();
        let mut found: Vec<LayerSpecEntries> = Vec::new();
        {
            let stack = ctx.stacks.get(node.site.stack);
            for (strength, layer_id, spec) in site::specs(ctx.store, stack, &node.site.path) {
                let layer_strength = u16::try_from(strength).unwrap_or(u16::MAX);
                let layer_offset = node_offset.compose(stack.offset_at(strength));
                let entries = crate::doc::composed_entries(spec.fields(), spec.properties())
                    .map(|entry| (entry.name(), entry.value()))
                    .collect();
                found.push((layer_strength, layer_id, layer_offset, entries));
            }
        }
        // Paths authored across an arc name sites of its target's
        // namespace: they compose mapped through the arcs to the root.
        let maps = !node.map_to_root.is_identity();
        // The class or arc target the node's specs are authored beneath, at
        // the namespace depth it was introduced at: a target a class
        // authors inside itself names no instance of it.
        let at_intro = graph.path_at_introduction(ctx.store.paths(), graph::node_index(at));
        let arc_root = ctx.store.paths().resolve(at_intro.prim_path()).clone();
        let source_path = ctx.store.paths().resolve(lookup_path).clone();
        for (layer_strength, layer_id, layer_offset, entries) in found {
            index.add_source(OpinionKey {
                node: id,
                layer_strength,
                layer_id,
                lookup_path,
                spec_path: node.site.path.clone(),
            });
            let mut opinions = Vec::with_capacity(entries.len());
            for (field, mut value) in entries {
                let key = OpinionKey {
                    node: id,
                    layer_strength,
                    layer_id,
                    lookup_path,
                    spec_path: node.site.path.with_property(field),
                };
                // Relative expressions anchor at every node, the root's
                // included.
                map_path_expressions(ctx.store, &node.map_to_root, &source_path, &mut value);
                if maps {
                    let map_to_root = &node.map_to_root;
                    let map = |store: &dyn LayerStore, path: PathId| {
                        map_to_root.map_source_to_target(store.paths(), path)
                    };
                    crate::composition_checks::map_arc_targets(
                        ctx.store,
                        &mut value,
                        crate::composition_checks::ArcPathMap {
                            arc: node.arc_kind,
                            source: &arc_root,
                            map: &map,
                        },
                        crate::composition_checks::TargetOwner {
                            prim,
                            property: field,
                            layer: layer_id,
                            spec: key.spec_path.clone(),
                        },
                        checks,
                    );
                }
                opinions.push(Opinion {
                    key,
                    field,
                    value,
                    layer_offset,
                });
            }
            index.add_opinions(opinions.into_iter());
        }
    }
    index.finalize();
    index
}

/// One layer's spec at a node's site: its strength in the node's layer
/// stack, the layer, the offset it is read with, and its fields and
/// properties.
type LayerSpecEntries = (
    u16,
    LayerId,
    crate::doc::LayerOffset,
    Vec<(TokenId, crate::prim_index::OpinionValue)>,
);

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
    let contributing: HashSet<NodeId> = index.sources.iter().map(|key| key.node).collect();
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
    unlisted.sort_by(|a, b| tokens.resolve(*a).cmp(tokens.resolve(*b)));
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
    placed.sort_by(|(pa, a), (pb, b)| match (pa, pb) {
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
    site: &crate::path::Path,
    value: &mut crate::prim_index::OpinionValue,
) {
    use crate::path_expression::value::{anchor_value_with, has_path_expression};
    let (default, samples) = match &*value {
        crate::prim_index::OpinionValue::Field(crate::doc::FieldValue::Value(v)) => (Some(v), None),
        crate::prim_index::OpinionValue::Property(spec) => {
            (spec.default.as_ref(), spec.time_samples.as_ref())
        }
        crate::prim_index::OpinionValue::Field(_) => return,
    };
    let default_changes = default.is_some_and(has_path_expression);
    let samples_change =
        samples.is_some_and(|samples| samples.iter().any(|(_, v)| has_path_expression(v)));
    if !default_changes && !samples_change {
        return;
    }
    let anchor: Vec<alloc::string::String> = site
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
            if samples_change && let Some(samples) = spec.time_samples.as_mut() {
                for (_, v) in samples.make_mut() {
                    if has_path_expression(v) {
                        anchor_value_with(v, &anchor, &mut map);
                    }
                }
            }
        }
        crate::prim_index::OpinionValue::Field(_) => {}
    }
}

/// Records what indexing `path` found.
fn record(
    out: &mut Composition,
    ctx: &Ctx<'_>,
    path: PathId,
    graph: &Graph,
    index: &PrimIndex,
    outputs: Outputs,
) {
    for recorded in outputs.errors {
        match recorded {
            Recorded::Error(error) => out.errors.push(error),
            Recorded::StackErrors { stack, prim } => {
                if out.reported_stacks.insert(stack) {
                    for mut error in ctx.stacks.get(stack).errors.iter().cloned() {
                        if let CompositionError::VariableExpressionError(error) = &mut error {
                            error.prim = Some(prim);
                        }
                        out.errors.push(error);
                    }
                }
            }
        }
    }
    out.asset_requests.extend(outputs.asset_requests);
    out.reads.extend(outputs.reads);
    // A layer stack's use belongs to the prim whose index first adds a
    // node reading it; its descendants inherit the node.
    for (at, node) in graph.nodes.iter().enumerate() {
        let stack = ctx.stacks.get(node.site.stack);
        if out.used_stacks.insert(node.site.stack) {
            out.used_layers.extend(stack.layers.iter().copied());
        }
        if at != 0 && !node.from_parent_prim {
            out.used_layer_sites
                .entry(path)
                .or_default()
                .extend(stack.layers.iter().copied());
        }
    }
    if out.deps.is_none() {
        return;
    }
    let mut prim_record = PrimRecord::default();
    prim_record
        .layers
        .extend(index.sources.iter().map(|source| source.layer_id));
    // The layers authoring the references and payloads on the way to each
    // node retime it.
    prim_record.layers.extend(outputs.arc_layers);
    prim_record
        .layers
        .extend(graph.nodes.iter().filter_map(|node| node.authoring_layer));
    prim_record.default_prim_layers = outputs.default_prim_layers;
    for (at, node) in graph.nodes.iter().enumerate() {
        if at == 0 || matches!(node.arc_kind, ArcKind::Local | ArcKind::Variants) {
            continue;
        }
        prim_record.arcs.push((
            node.site.path.prim_path(),
            node.arc_kind,
            ctx.stacks.get(node.site.stack).identifier.root,
        ));
    }
    out.records.insert(path, prim_record);
}

/// Lists inactive children apart and deduplicates the errors.
///
/// Every composed child name is a prim, whether or not its index has
/// specs: names come from the sites of the parent's index, or from a
/// relocation, as OpenUSD populates them (`UsdStage::_ComposeChildren`).
fn finish(out: &mut Composition) {
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
