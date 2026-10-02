// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Caller-owned retained computed queries over an explicitly edited live scene.
//!
//! Independent observers borrow synchronized views of a host-owned live stage.
//! An optional session bundles source ownership and queries for convenience. Queries follow namespace paths, including deletion and
//! recreation; they do not promise durable object identity. Polling is explicit:
//! no threads, callbacks, execution graph or hidden scheduling are involved.

use crate::{
    PrimView, Scene, SchemaEdit, Time, XformCacheStats,
    bounds::{BoundingBox, BoundsCache, BoundsError, BoundsOptions, BoundsStats},
    shading::{ValueSourceKind, ValueSources},
};
use alloc::{collections::BTreeMap, vec::Vec};
use layerstack::{
    Applied, ChangeCursor, ChangeHistoryError, Changes, EditError, EditTarget, LayerId, LayerStore,
    LiveStage, PathId, PropertyField, PropertyPath, Provenance, ResolvedValue, StageOptions,
    Transaction, Value,
};

/// One retained computation. Geometry queries use the session's bounds options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    /// Local-to-world matrix, or absence when the prim is missing.
    WorldTransform(PathId),
    /// Oriented world bound, including supported extent providers.
    WorldBound(PathId),
    /// Ordered providers and authored values; shader outputs are not evaluated.
    ShadingValue(PropertyPath),
}

/// Observer-local query handle. Removed handles are never reused.
///
/// A handle is meaningful only in the query collection that issued it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueryId(u64);

/// A shading result separates connection provenance from time-specific values.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadingValue {
    /// Provider paths, connection chains, failed branches and dependencies.
    pub providers: ValueSources,
    /// Value per provider. Shader outputs and unresolved values have no value.
    pub values: Vec<Option<Value>>,
    /// Winning authored value source per provider, when available.
    pub provenance: Vec<Option<Provenance>>,
}

/// Retained answer. Unsupported bounds remain explicit errors.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryAnswer {
    /// World matrix, or a missing prim.
    WorldTransform(Option<[[f64; 4]; 4]>),
    /// World bound or the reason it cannot be computed.
    WorldBound(Result<BoundingBox, BoundsError>),
    /// Shading providers, values and source provenance.
    ShadingValue(ShadingValue),
}

/// Evidence that caused a query to be reconsidered, not proof its answer changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryCause {
    /// First evaluation.
    Initial,
    /// Composed change history expired; all cached evidence was refreshed.
    HistoryLost,
    /// Evaluation time or interpolation changed.
    Time,
    /// A precisely reported authored property field changed.
    Property {
        /// Composed property affected.
        path: PropertyPath,
        /// Authored field affected.
        field: PropertyField,
    },
    /// The report identified a prim but not individual fields.
    Prim(PathId),
    /// A structural resync covers this root and its descendants.
    Resync(PathId),
}

/// Work performed by a session since construction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryStats {
    /// Queries reconsidered by polling (including cache-answered computation).
    pub evaluated: u64,
    /// Polls answered without reconsidering the query.
    pub reused: u64,
    /// Query records examined while routing authored change batches.
    pub routed: u64,
    /// Connection/provider graphs traversed.
    pub shading_walks: u64,
}

/// Borrowed dependency evidence from the last evaluation.
#[derive(Clone, Copy, Debug)]
pub enum QueryDependencies<'a> {
    /// Ancestors consulted up to the transform reset boundary, query prim first.
    TransformAncestors(&'a [PathId]),
    /// Bounds retain their detailed reductions inside `BoundsCache`. Routing
    /// conservatively watches this subtree, inherited ancestor inputs and
    /// retained point-instancer prototype dependencies.
    BoundsNamespace(PathId),
    /// Properties inspected by provider discovery, including missing targets.
    ShadingProperties(&'a [crate::shading::ShadingDependency]),
}

/// Actual computation performed during one poll, including bounds' transforms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryWork {
    /// Local transform evaluations.
    pub local_transforms: usize,
    /// World transform compositions.
    pub world_transforms: usize,
    /// Prim bounds evaluated.
    pub bounds: usize,
    /// Shading connection/provider traversals.
    pub shading_walks: u64,
}

/// Result of one poll, borrowing retained storage without cloning the answer.
#[derive(Debug)]
pub struct QueryUpdate<'a> {
    /// Current answer.
    pub answer: &'a QueryAnswer,
    /// Whether this poll reconsidered the query.
    pub evaluated: bool,
    /// Whether the computed value (including errors) differs from the last poll.
    /// Float bits are compared exactly: unchanged NaNs are stable and signed
    /// zero changes are observable. No numerical tolerance is applied.
    pub answer_changed: bool,
    /// Whether the dependency recipe changed.
    pub dependencies_changed: bool,
    /// Whether shading provider identity, chains or authored sources changed.
    pub provenance_changed: bool,
    /// Bounded reasons for reconsideration, empty for a clean poll.
    pub causes: &'a [QueryCause],
    /// Additional reasons omitted after the eight-reason budget was exhausted.
    pub omitted_causes: usize,
    /// Composed stage revision consumed by this observer; time changes are separate.
    pub revision: u64,
    /// Domain work performed during this poll; cache hits do not count as computation.
    pub work: QueryWork,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Recipe {
    Transform(Vec<PathId>),
    Bound(PathId),
    Shading,
}

#[derive(Debug)]
struct Held {
    query: Query,
    answer: Option<QueryAnswer>,
    recipe: Recipe,
    dirty: bool,
    topology_dirty: bool,
    varying: bool,
    time_epoch: u64,
    causes: Vec<QueryCause>,
    omitted: usize,
}
impl Held {
    fn shading_dependencies(&self) -> &[crate::shading::ShadingDependency] {
        match &self.answer {
            Some(QueryAnswer::ShadingValue(value)) => &value.providers.dependencies,
            _ => &[],
        }
    }
    fn mark(&mut self, cause: QueryCause) {
        if !self.dirty {
            self.causes.clear();
            self.omitted = 0;
        }
        self.dirty = true;
        self.topology_dirty |= cause != QueryCause::Time;
        if !self.causes.contains(&cause) {
            if self.causes.len() < 8 {
                self.causes.push(cause);
            } else {
                self.omitted = self.omitted.saturating_add(1);
            }
        }
    }
}

/// Caller-owned computed queries, independent of scene authoring ownership.
///
/// Acquire a [`Self::view`] to synchronize a host-owned stage once, replay this
/// observer's change cursor, and poll any number of queries. The view borrows
/// the scene so authoring cannot race with evaluation. Routing scans registered
/// queries; no scheduler or callback dispatcher is involved.
#[derive(Debug)]
pub struct RetainedQueries {
    time: Time,
    time_epoch: u64,
    bounds: BoundsCache,
    bounds_options: BoundsOptions,
    held: BTreeMap<QueryId, Held>,
    next_id: u64,
    revision: u64,
    cursor: Option<ChangeCursor>,
    stats: QueryStats,
}

impl RetainedQueries {
    /// Creates empty retained state. It binds to a live stage on its first view.
    pub fn new(time: Time, bounds: BoundsOptions) -> Self {
        Self {
            time,
            time_epoch: 0,
            bounds: BoundsCache::new(time, bounds.clone()),
            bounds_options: bounds,
            held: BTreeMap::new(),
            next_id: 0,
            revision: 0,
            cursor: None,
            stats: QueryStats::default(),
        }
    }

    /// Synchronizes source changes and borrows a scene for a batch of polls.
    ///
    /// Source edits may come from `Layer` methods, direct transactions or another
    /// live stage sharing the store. Raw importer writes must call `Layer::touch`.
    /// The stage must have provenance enabled for winning-layer explanations.
    /// A different stage is rejected; create new queries for a different scene.
    /// History loss conservatively resets caches and reports `HistoryLost`.
    /// Synchronization scans participating layers once, not once per poll.
    pub fn view<'a>(
        &'a mut self,
        live: &'a mut LiveStage,
        store: &'a mut dyn LayerStore,
    ) -> Result<QueryView<'a>, ChangeHistoryError> {
        live.synchronize(store);
        self.sync_reports(live, store)?;
        Ok(QueryView {
            queries: self,
            scene: Scene::new(live.stage(), store),
        })
    }

    fn sync_reports(
        &mut self,
        live: &mut LiveStage,
        store: &dyn LayerStore,
    ) -> Result<(), ChangeHistoryError> {
        let mut cursor = self.cursor.take().unwrap_or_else(|| live.change_cursor());
        let scene = Scene::new(live.stage(), store);
        match live.changes_since(&mut cursor) {
            Ok(reports) => {
                for changes in reports {
                    self.consume(&scene, changes);
                }
            }
            Err(ChangeHistoryError::Expired) => {
                self.bounds = BoundsCache::new(self.time, self.bounds_options.clone());
                for held in self.held.values_mut() {
                    held.mark(QueryCause::HistoryLost);
                }
            }
            Err(error) => {
                self.cursor = Some(cursor);
                return Err(error);
            }
        }
        self.revision = cursor.revision();
        self.cursor = Some(cursor);
        Ok(())
    }

    /// Registers a lazy query. Its first poll computes and reports an initial answer.
    pub fn observe(&mut self, query: Query) -> QueryId {
        let id = QueryId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("query handle space exhausted");
        let recipe = match query {
            Query::WorldTransform(path) => Recipe::Transform(alloc::vec![path]),
            Query::WorldBound(path) => Recipe::Bound(path),
            Query::ShadingValue(_) => Recipe::Shading,
        };
        self.held.insert(
            id,
            Held {
                query,
                recipe,
                answer: None,
                dirty: true,
                topology_dirty: true,
                varying: true,
                time_epoch: self.time_epoch,
                causes: alloc::vec![QueryCause::Initial],
                omitted: 0,
            },
        );
        id
    }
    /// Retires a query and releases its answer and evidence. Returns whether it existed.
    pub fn remove(&mut self, id: QueryId) -> bool {
        self.held.remove(&id).is_some()
    }
    /// Number of retained query records.
    pub fn len(&self) -> usize {
        self.held.len()
    }
    /// Whether no queries are retained.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
    /// Work counters for routing and polling.
    pub fn stats(&self) -> QueryStats {
        self.stats
    }
    /// Dependency evidence retained by the last completed evaluation.
    ///
    /// Returns None before the first poll or after removal. Evidence can be
    /// stale until the next poll after an edit. For authored-value explanations,
    /// inspect the reported properties through `scene().stage()`'s existing
    /// `explain_value_with_schema` / `explain_value_at_time_with_schema` APIs.
    /// Those describe current source opinions, not an historical transaction log.
    pub fn dependencies(&self, id: QueryId) -> Option<QueryDependencies<'_>> {
        let held = self.held.get(&id)?;
        held.answer.as_ref()?;
        Some(match &held.recipe {
            Recipe::Transform(paths) => QueryDependencies::TransformAncestors(paths),
            Recipe::Bound(path) => QueryDependencies::BoundsNamespace(*path),
            Recipe::Shading => QueryDependencies::ShadingProperties(held.shading_dependencies()),
        })
    }
    /// Prototype roots retained by a bounds query's last computation. Includes
    /// missing targets so callers can inspect failures and their recovery inputs.
    /// Returns `None` for unknown IDs and queries other than world bounds.
    pub fn bound_prototype_dependencies(
        &self,
        scene: &Scene<'_>,
        id: QueryId,
    ) -> Option<Vec<PathId>> {
        let Recipe::Bound(path) = self.held.get(&id)?.recipe else {
            return None;
        };
        Some(self.bounds.prototype_dependencies(scene, path))
    }

    fn work(&self) -> QueryWork {
        let bounds = self.bounds.transform_stats();
        QueryWork {
            local_transforms: bounds.local_computed,
            world_transforms: bounds.world_computed,
            bounds: self.bounds.stats().computed,
            shading_walks: self.stats.shading_walks,
        }
    }
    /// Domain transform-cache work counters.
    pub fn transform_stats(&self) -> XformCacheStats {
        self.bounds.transform_stats()
    }
    /// Domain bounds-cache work counters.
    pub fn bounds_stats(&self) -> BoundsStats {
        self.bounds.stats()
    }
    /// Changes time in O(1), without walking scene prims or registered queries. Static transform/shading queries
    /// retain their answers; bounds defer temporal validation to their cache.
    pub fn set_time(&mut self, time: Time) {
        if self.time == time {
            return;
        }
        self.time = time;
        self.bounds.set_time(time);
        self.time_epoch = self
            .time_epoch
            .checked_add(1)
            .expect("query time epoch exhausted");
    }
    fn consume(&mut self, scene: &Scene<'_>, changes: &Changes) {
        self.bounds.apply_changes(scene, changes);
        for held in self.held.values_mut() {
            self.stats.routed += 1;
            for &root in &changes.resynced {
                if structural_overlap(scene, held, root, &self.bounds) {
                    held.mark(QueryCause::Resync(root));
                }
            }
            for &prim in &changes.changed_info_only {
                if let Some(fields) = changes.properties_for(prim) {
                    for field in fields {
                        if property_overlap(scene, held, prim, Some(field.name), &self.bounds) {
                            held.mark(QueryCause::Property {
                                path: PropertyPath::new(prim, field.name),
                                field: field.field,
                            });
                        }
                    }
                } else if property_overlap(scene, held, prim, None, &self.bounds) {
                    held.mark(QueryCause::Prim(prim));
                }
            }
        }
    }
    /// Validates one query and borrows its current answer. A clean poll does not
    /// allocate or inspect scene properties. Unknown/retired handles return None.
    fn poll(&mut self, scene: &Scene<'_>, id: QueryId) -> Option<QueryUpdate<'_>> {
        let before = self.work();
        let held = self.held.get_mut(&id)?;
        if held.time_epoch != self.time_epoch && held.varying {
            held.mark(QueryCause::Time);
        }
        held.time_epoch = self.time_epoch;
        let evaluated = held.dirty;
        let (mut answer_changed, mut dependencies_changed, mut provenance_changed) =
            (false, false, false);
        if evaluated {
            self.stats.evaluated += 1;
            if held.topology_dirty {
                let (answer, recipe, varying) =
                    evaluate(scene, held, self.time, &mut self.bounds, &mut self.stats);
                answer_changed = held
                    .answer
                    .as_ref()
                    .is_none_or(|old| !same_answer(old, &answer));
                dependencies_changed = held.answer.is_none()
                    || held.recipe != recipe
                    || matches!((&held.answer, &answer),
                    (Some(QueryAnswer::ShadingValue(old)), QueryAnswer::ShadingValue(new))
                        if old.providers.dependencies != new.providers.dependencies);
                provenance_changed = held
                    .answer
                    .as_ref()
                    .is_none_or(|old| !same_provenance(old, &answer));
                held.answer = Some(answer);
                held.recipe = recipe;
                held.varying = varying;
            } else {
                (answer_changed, provenance_changed) =
                    evaluate_time(scene, held, self.time, &mut self.bounds);
            }
            held.dirty = false;
            held.topology_dirty = false;
        } else {
            self.stats.reused += 1;
        }
        let bound_xforms = self.bounds.transform_stats();
        let work = QueryWork {
            local_transforms: bound_xforms.local_computed - before.local_transforms,
            world_transforms: bound_xforms.world_computed - before.world_transforms,
            bounds: self.bounds.stats().computed - before.bounds,
            shading_walks: self.stats.shading_walks - before.shading_walks,
        };
        Some(QueryUpdate {
            answer: held.answer.as_ref().expect("evaluated query has an answer"),
            evaluated,
            answer_changed,
            dependencies_changed,
            provenance_changed,
            causes: if evaluated { &held.causes } else { &[] },
            omitted_causes: if evaluated { held.omitted } else { 0 },
            revision: self.revision,
            work,
        })
    }
}

/// A synchronized borrowed scene and one observer's retained query state.
///
/// Drop this view before editing the source store or stage. Creating a new view
/// automatically detects and consumes intervening changes for this observer.
#[derive(Debug)]
pub struct QueryView<'a> {
    queries: &'a mut RetainedQueries,
    scene: Scene<'a>,
}

impl QueryView<'_> {
    /// Validates one query, borrowing its answer and change evidence.
    /// Clean polls allocate nothing and do not inspect source generations.
    pub fn poll(&mut self, id: QueryId) -> Option<QueryUpdate<'_>> {
        self.queries.poll(&self.scene, id)
    }

    /// The synchronized scene, for source and provenance inspection.
    pub fn scene(&self) -> Scene<'_> {
        self.scene
    }
}

/// Optional convenience owner of a source store, stage and retained queries.
///
/// Hosts that already own authoring use [`RetainedQueries`] directly. This
/// wrapper delivers the same stage-owned journal; it is not a separate observer
/// protocol. Its exclusive ownership avoids generation scans on each poll.
#[derive(Debug)]
pub struct QuerySession<S> {
    store: S,
    live: LiveStage,
    queries: RetainedQueries,
    external_pending: bool,
}

impl<S: LayerStore> QuerySession<S> {
    /// Composes a stage with provenance enabled and creates empty query caches.
    pub fn new(
        mut store: S,
        root: LayerId,
        mut options: StageOptions,
        time: Time,
        bounds: BoundsOptions,
    ) -> Self {
        options.with_provenance = true;
        let live = LiveStage::compose(&mut store, root, options);
        Self {
            store,
            live,
            queries: RetainedQueries::new(time, bounds),
            external_pending: false,
        }
    }
    /// Read-only authored store, including path and token interners.
    pub fn store(&self) -> &S {
        &self.store
    }
    /// Consumes the session and returns its source store.
    pub fn into_store(self) -> S {
        self.store
    }
    /// Interns a namespace path without modifying authored data.
    pub fn path(&mut self, text: &str) -> Result<PathId, layerstack::PathError> {
        let path = layerstack::Path::parse_absolute(text, self.store.tokens_mut())?;
        Ok(self.store.paths_mut().intern(path))
    }
    /// Prototype roots retained by a bounds query, including missing targets.
    pub fn bound_prototype_dependencies(&mut self, id: QueryId) -> Option<Vec<PathId>> {
        self.refresh_external();
        self.queries
            .bound_prototype_dependencies(&Scene::new(self.live.stage(), &self.store), id)
    }

    /// Current scene, completing an interrupted external refresh if necessary.
    pub fn scene(&mut self) -> Scene<'_> {
        self.refresh_external();
        Scene::new(self.live.stage(), &self.store)
    }
    /// Prepares an ordinary schema transaction.
    pub fn edit(&mut self, target: EditTarget) -> SchemaEdit<'_> {
        self.refresh_external();
        SchemaEdit::new(self.live.stage(), &mut self.store, target)
    }
    /// Applies a transaction and consumes its stage-owned report.
    pub fn apply(&mut self, transaction: &Transaction) -> Result<Applied, EditError> {
        self.refresh_external();
        let applied = self.live.apply(&mut self.store, transaction)?;
        self.queries
            .sync_reports(&mut self.live, &self.store)
            .expect("session stage is unchanged");
        Ok(applied)
    }
    /// Mutates arbitrary source fields and conservatively recomposes.
    /// If the closure unwinds, the next scene access completes the refresh.
    /// Keep interned identifiers valid; replacing interners is unsupported.
    pub fn edit_sources<R>(&mut self, edit: impl FnOnce(&mut S) -> R) -> R {
        self.external_pending = true;
        let result = edit(&mut self.store);
        self.refresh_external();
        result
    }
    fn refresh_external(&mut self) {
        if self.external_pending {
            self.live.notify_structural_change();
            self.live.recompose_changes(&mut self.store);
            self.queries
                .sync_reports(&mut self.live, &self.store)
                .expect("session stage is unchanged");
            self.external_pending = false;
        }
    }
    /// Registers a lazy query.
    pub fn observe(&mut self, query: Query) -> QueryId {
        self.queries.observe(query)
    }
    /// Retires a query, releasing its answer and recipe.
    pub fn remove(&mut self, id: QueryId) -> bool {
        self.queries.remove(id)
    }
    /// Number of retained queries.
    pub fn len(&self) -> usize {
        self.queries.len()
    }
    /// Whether there are no retained queries.
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }
    /// Query work counters.
    pub fn stats(&self) -> QueryStats {
        self.queries.stats()
    }
    /// Last evaluated dependency evidence.
    pub fn dependencies(&self, id: QueryId) -> Option<QueryDependencies<'_>> {
        self.queries.dependencies(id)
    }
    /// Transform-cache work counters.
    pub fn transform_stats(&self) -> XformCacheStats {
        self.queries.transform_stats()
    }
    /// Bounds-cache work counters.
    pub fn bounds_stats(&self) -> BoundsStats {
        self.queries.bounds_stats()
    }
    /// Advances evaluation time in O(1).
    pub fn set_time(&mut self, time: Time) {
        self.queries.set_time(time);
    }
    /// Validates one query and borrows its answer and change evidence.
    pub fn poll(&mut self, id: QueryId) -> Option<QueryUpdate<'_>> {
        self.refresh_external();
        self.queries
            .sync_reports(&mut self.live, &self.store)
            .expect("session stage is unchanged");
        self.queries
            .poll(&Scene::new(self.live.stage(), &self.store), id)
    }
}

fn prefix(scene: &Scene<'_>, a: PathId, b: PathId) -> bool {
    scene
        .store()
        .paths()
        .resolve(a)
        .is_prefix_of(scene.store().paths().resolve(b))
}
fn structural_overlap(scene: &Scene<'_>, held: &Held, root: PathId, bounds: &BoundsCache) -> bool {
    match &held.recipe {
        Recipe::Transform(paths) => paths.iter().any(|&p| prefix(scene, root, p)),
        Recipe::Bound(path) => {
            prefix(scene, root, *path)
                || prefix(scene, *path, root)
                || bounds.prototype_overlap(scene, *path, root)
        }
        Recipe::Shading => {
            let deps = held.shading_dependencies();
            deps.iter().any(|d| prefix(scene, root, d.prim))
                || matches!(held.query, Query::ShadingValue(p) if prefix(scene,root,p.prim_path()))
        }
    }
}
fn property_overlap(
    scene: &Scene<'_>,
    held: &Held,
    prim: PathId,
    name: Option<layerstack::TokenId>,
    bounds: &BoundsCache,
) -> bool {
    let name = name.map(|n| scene.store().tokens().resolve(n));
    match &held.recipe {
        Recipe::Transform(paths) => {
            paths.contains(&prim) && name.is_none_or(crate::xform::transform_property)
        }
        Recipe::Bound(root) => {
            if bounds.prototype_overlap(scene, *root, prim)
                && name.is_none_or(crate::bounds::bounds_property)
            {
                return true;
            }
            if prefix(scene, *root, prim) {
                name.is_none_or(crate::bounds::bounds_property)
            } else {
                prefix(scene, prim, *root)
                    && name.is_none_or(|n| crate::xform::transform_property(n) || n == "purpose")
            }
        }
        Recipe::Shading => held
            .shading_dependencies()
            .iter()
            .any(|d| d.prim == prim && name.is_none_or(|n| n == d.property)),
    }
}
fn evaluate(
    scene: &Scene<'_>,
    held: &Held,
    time: Time,
    bounds: &mut BoundsCache,
    stats: &mut QueryStats,
) -> (QueryAnswer, Recipe, bool) {
    match held.query {
        Query::WorldTransform(path) => {
            let xforms = bounds.transforms_mut();
            let answer = xforms.local_to_world(scene, path);
            let ancestors = xforms.world_dependencies(path);
            let varying = xforms.world_might_vary(path);
            (
                QueryAnswer::WorldTransform(answer),
                Recipe::Transform(ancestors),
                varying,
            )
        }
        Query::WorldBound(path) => (
            QueryAnswer::WorldBound(bounds.world_bound(scene, path)),
            Recipe::Bound(path),
            true,
        ),
        Query::ShadingValue(path) => {
            stats.shading_walks += 1;
            let providers = scene.value_sources(path);
            let mut values = Vec::with_capacity(providers.sources.len());
            let mut provenance = Vec::with_capacity(providers.sources.len());
            let mut varying = false;
            for source in &providers.sources {
                if source.kind == ValueSourceKind::ShaderOutput {
                    values.push(None);
                    provenance.push(None);
                    continue;
                }
                let p = source.attribute;
                varying |= PrimView::new(*scene, p.prim_path())
                    .property_might_vary(scene.store().tokens().resolve(p.property()));
                let (value, origin) = provider_value(scene, p, time);
                values.push(value);
                provenance.push(origin);
            }
            let recipe = Recipe::Shading;
            (
                QueryAnswer::ShadingValue(ShadingValue {
                    providers,
                    values,
                    provenance,
                }),
                recipe,
                varying,
            )
        }
    }
}
fn same_answer(a: &QueryAnswer, b: &QueryAnswer) -> bool {
    match (a, b) {
        (QueryAnswer::ShadingValue(a), QueryAnswer::ShadingValue(b)) => {
            same_values(&a.values, &b.values)
                && a.providers.issues == b.providers.issues
                && a.providers.sources.iter().map(|s| s.kind).eq(b
                    .providers
                    .sources
                    .iter()
                    .map(|s| s.kind))
        }
        (QueryAnswer::WorldTransform(a), QueryAnswer::WorldTransform(b)) => match (a, b) {
            (Some(a), Some(b)) => same_matrix(a, b),
            (None, None) => true,
            _ => false,
        },
        (QueryAnswer::WorldBound(Ok(a)), QueryAnswer::WorldBound(Ok(b))) => {
            same_vector(&a.range.min, &b.range.min)
                && same_vector(&a.range.max, &b.range.max)
                && same_matrix(&a.matrix, &b.matrix)
        }
        _ => a == b,
    }
}
fn same_provenance(a: &QueryAnswer, b: &QueryAnswer) -> bool {
    match (a, b) {
        (QueryAnswer::ShadingValue(a), QueryAnswer::ShadingValue(b)) => {
            a.providers.sources == b.providers.sources && a.provenance == b.provenance
        }
        _ => true,
    }
}

fn same_vector<const N: usize>(a: &[f64; N], b: &[f64; N]) -> bool {
    a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}
fn same_matrix(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> bool {
    a.iter().zip(b).all(|(a, b)| same_vector(a, b))
}
fn same_values(a: &[Option<Value>], b: &[Option<Value>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_value(a, b))
}

// A time change cannot alter provider topology or ancestor relationships.
// Keep their allocations and evaluate only the already discovered inputs.
fn evaluate_time(
    scene: &Scene<'_>,
    held: &mut Held,
    time: Time,
    bounds: &mut BoundsCache,
) -> (bool, bool) {
    match (
        held.query,
        held.answer
            .as_mut()
            .expect("a prepared query has an answer"),
    ) {
        (Query::WorldTransform(path), answer @ QueryAnswer::WorldTransform(_)) => {
            let next =
                QueryAnswer::WorldTransform(bounds.transforms_mut().local_to_world(scene, path));
            let changed = !same_answer(answer, &next);
            *answer = next;
            (changed, false)
        }
        (Query::WorldBound(path), answer @ QueryAnswer::WorldBound(_)) => {
            let next = QueryAnswer::WorldBound(bounds.world_bound(scene, path));
            let changed = !same_answer(answer, &next);
            *answer = next;
            (changed, false)
        }
        (Query::ShadingValue(_), QueryAnswer::ShadingValue(value)) => {
            let mut changed = false;
            let mut provenance_changed = false;
            for (index, source) in value.providers.sources.iter().enumerate() {
                if source.kind == ValueSourceKind::ShaderOutput {
                    continue;
                }
                let (next, provenance) = provider_value(scene, source.attribute, time);
                changed |= !same_value(&value.values[index], &next);
                provenance_changed |= value.provenance[index] != provenance;
                value.values[index] = next;
                value.provenance[index] = provenance;
            }
            (changed, provenance_changed)
        }
        _ => unreachable!("query kind and answer agree"),
    }
}
fn provider_value(
    scene: &Scene<'_>,
    p: PropertyPath,
    time: Time,
) -> (Option<Value>, Option<Provenance>) {
    let resolved = match time {
        Time::Default => scene
            .stage()
            .resolve_value_with_schema(p.prim_path(), p.property(), scene.store())
            .and_then(|v| match v.value {
                ResolvedValue::Scalar(value) => Some((value, v.provenance)),
                _ => None,
            }),
        Time::At {
            code,
            interpolation,
        } => scene
            .stage()
            .resolve_value_at_time_with_schema(
                p.prim_path(),
                p.property(),
                code,
                interpolation,
                scene.store(),
            )
            .map(|v| (v.value, v.provenance)),
    };
    resolved.map_or((None, None), |(value, provenance)| {
        (Some(value), provenance)
    })
}
fn same_value(a: &Option<Value>, b: &Option<Value>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.same_representation(b),
        (None, None) => true,
        _ => false,
    }
}
