// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Caller-owned compiled collections and ordered membership decisions.
use super::{Membership, MembershipProblem, MembershipQuery, SCHEMA_BASE_NAMES};
use crate::Scene;
use alloc::{collections::BTreeMap, string::String, vec, vec::Vec};
use layerstack::{Changes, PathId, PropertyPath, TargetPath};

/// A collection instance, scoped to its originating store.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CollectionIdentity {
    /// Prim that owns the collection.
    pub owner: PathId,
    /// Instance name, including any namespace components.
    pub name: String,
}
/// Evidence used to invalidate a retained collection capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipDependencies {
    /// Own collection plus included, referenced and failed collection identities.
    pub collections: Vec<CollectionIdentity>,
    /// Missing expression references require watching every collection.
    pub all_collections: bool,
    /// Effective expressions read object existence and scene predicates. Prim
    /// metadata and namespace changes are handled conservatively across the scene.
    pub scene_objects: bool,
}
/// Cache-local content stamps; compare only within the same cache instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MembershipRevisions {
    /// Compiled query, dependency evidence or collection problems changed.
    pub query: u64,
    /// Target order or membership decisions changed.
    pub decisions: u64,
}
/// A borrowed retained capture, in caller-supplied order including duplicates.
#[derive(Clone, Copy, Debug)]
pub struct MembershipSample<'a> {
    /// Requested collection identity.
    pub collection: &'a CollectionIdentity,
    /// Compiled query, including its resolution problems.
    pub query: &'a MembershipQuery,
    /// Candidate identities; no channel-width restriction is imposed.
    pub targets: &'a [TargetPath],
    /// One decision per target, retaining the inclusion rule.
    pub membership: &'a [Membership],
    /// Invalidation evidence, including conservative scopes.
    pub dependencies: &'a MembershipDependencies,
    /// Content stamps for selective engine uploads.
    pub revisions: MembershipRevisions,
    /// This request rebuilt the query or tested candidate paths.
    pub evaluated: bool,
}
/// Observable work since construction or statistics reset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MembershipCacheStats {
    /// Capture requests, including invalid requests.
    pub requests: u64,
    /// Requests answered without building a query or testing paths.
    pub cache_hits: u64,
    /// Queries compiled from a scene.
    pub query_builds: u64,
    /// Candidate path tests, including duplicate candidates.
    pub path_tests: u64,
    /// Retained entries examined while routing changes.
    pub routed: u64,
    /// Clean entries made dirty by change reports.
    pub invalidated: u64,
}
/// Retained work-item counts, rather than estimated heap bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MembershipCacheMemory {
    /// Retained collection entries.
    pub collections: usize,
    /// Retained candidates and decisions, including duplicates.
    pub targets: usize,
    /// Collection identity dependencies across entries.
    pub dependencies: usize,
    /// Compiled rule-map entries across collections.
    pub rules: usize,
}
/// Why a collection capture request is invalid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipCacheError {
    /// The owning prim is absent.
    MissingPrim(PathId),
    /// An empty, malformed or reserved collection instance name.
    InvalidName(String),
}
impl core::fmt::Display for MembershipCacheError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "collection capture: {self:?}")
    }
}
impl core::error::Error for MembershipCacheError {}
#[derive(Debug)]
struct Held {
    query: MembershipQuery,
    dependencies: MembershipDependencies,
    targets: Vec<TargetPath>,
    membership: Vec<Membership>,
    revisions: MembershipRevisions,
    query_dirty: bool,
    decisions_dirty: bool,
}
/// Retains collection queries and one candidate list per collection for one
/// stage/store pair. Call `apply_changes` with every complete successful edit
/// report before capturing again; clear after losing history or changing the
/// stage, schema registry or kind registry. No scene is borrowed or scheduled.
///
/// Compiled query changes and decision changes have independent revisions.
/// Recomputing unchanged content preserves its stamp. Namespace changes and
/// info-only edits without a complete property inventory rebuild queries
/// conservatively; precise unrelated property edits preserve them. Expressions
/// additionally watch edits to candidate properties for object existence.
/// AOUSD Core §15; OpenUSD `UsdCollectionMembershipQuery` and `ObjectsChanged`.
#[derive(Debug, Default)]
pub struct MembershipCache {
    held: BTreeMap<CollectionIdentity, Held>,
    revision: u64,
    stats: MembershipCacheStats,
}
impl MembershipCache {
    /// Creates an empty caller-owned cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Drops retained data without recycling revision stamps or resetting stats.
    pub fn clear(&mut self) {
        self.held.clear();
    }
    /// Work performed since construction or the last reset.
    #[must_use]
    pub fn stats(&self) -> MembershipCacheStats {
        self.stats
    }
    /// Resets work counters without dropping retained data.
    pub fn reset_stats(&mut self) {
        self.stats = MembershipCacheStats::default();
    }
    /// Retained work-item counts.
    #[must_use]
    pub fn memory(&self) -> MembershipCacheMemory {
        MembershipCacheMemory {
            collections: self.held.len(),
            targets: self.held.values().map(|h| h.targets.len()).sum(),
            dependencies: self
                .held
                .values()
                .map(|h| h.dependencies.collections.len())
                .sum(),
            rules: self.held.values().map(|h| h.query.rules().count()).sum(),
        }
    }
    /// Captures membership in caller order. Like `MembershipQuery::compute`,
    /// collection schema fallbacks apply even without authored properties.
    /// Problems in the query remain inspectable alongside partial decisions.
    pub fn capture<'a>(
        &'a mut self,
        scene: &Scene<'_>,
        owner: PathId,
        name: &str,
        targets: &[TargetPath],
    ) -> Result<MembershipSample<'a>, MembershipCacheError> {
        let key = CollectionIdentity {
            owner,
            name: name.into(),
        };
        let evaluated = self.prepare(scene, &key, targets)?;
        Ok(self.sample(&key, evaluated))
    }
    pub(crate) fn prepare(
        &mut self,
        scene: &Scene<'_>,
        key: &CollectionIdentity,
        targets: &[TargetPath],
    ) -> Result<bool, MembershipCacheError> {
        self.stats.requests += 1;
        if key
            .name
            .split(':')
            .any(|s| !layerstack::ident::is_identifier(s))
            || SCHEMA_BASE_NAMES.contains(&key.name.rsplit(':').next().unwrap_or_default())
        {
            return Err(MembershipCacheError::InvalidName(key.name.clone()));
        }
        if !scene.stage().has_prim(key.owner) {
            self.held.remove(key);
            return Err(MembershipCacheError::MissingPrim(key.owner));
        }
        if self
            .held
            .get(key)
            .is_some_and(|h| !h.query_dirty && !h.decisions_dirty && h.targets == targets)
        {
            self.stats.cache_hits += 1;
            return Ok(false);
        }
        let old = self.held.remove(key);
        let rebuild = old.as_ref().is_none_or(|h| h.query_dirty);
        let query = if rebuild {
            self.stats.query_builds += 1;
            MembershipQuery::compute(scene, key.owner, &key.name)
        } else {
            old.as_ref().unwrap().query.clone()
        };
        let dependencies = dependencies(scene, key, &query);
        let query_changed = old
            .as_ref()
            .is_none_or(|h| h.query != query || h.dependencies != dependencies);
        let test = query_changed
            || old
                .as_ref()
                .is_none_or(|h| h.decisions_dirty || h.targets != targets);
        let membership = if test {
            self.stats.path_tests += targets.len() as u64;
            targets
                .iter()
                .map(|p| query.is_included(scene, *p))
                .collect()
        } else {
            old.as_ref().unwrap().membership.clone()
        };
        let decisions_changed = old
            .as_ref()
            .is_none_or(|h| h.targets != targets || h.membership != membership);
        let mut revisions = old
            .as_ref()
            .map_or(MembershipRevisions::default(), |h| h.revisions);
        if query_changed {
            revisions.query = self.next_revision();
        }
        if decisions_changed {
            revisions.decisions = self.next_revision();
        }
        self.held.insert(
            key.clone(),
            Held {
                query,
                dependencies,
                targets: targets.to_vec(),
                membership,
                revisions,
                query_dirty: false,
                decisions_dirty: false,
            },
        );
        Ok(true)
    }
    pub(crate) fn sample(&self, key: &CollectionIdentity, evaluated: bool) -> MembershipSample<'_> {
        let (key, h) = self.held.get_key_value(key).expect("prepared collection");
        MembershipSample {
            collection: key,
            query: &h.query,
            targets: &h.targets,
            membership: &h.membership,
            dependencies: &h.dependencies,
            revisions: h.revisions,
            evaluated,
        }
    }
    fn next_revision(&mut self) -> u64 {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("membership revision exhausted");
        self.revision
    }
    /// Routes complete edit reports and drops entries whose owning prim vanished;
    /// other work happens lazily on the next capture.
    /// Unknown prim metadata can change expression references or predicates,
    /// so it conservatively invalidates every compiled query. Rule maps do not
    /// depend on unrelated geometry values, nor do collection predicates read
    /// attribute values. Host changes outside edit reports require `clear`.
    pub fn apply_changes(&mut self, scene: &Scene<'_>, changes: &Changes) {
        let structural = !changes.created.is_empty()
            || !changes.removed.is_empty()
            || !changes.resynced.is_empty();
        let unknown = changes
            .changed_info_only
            .iter()
            .any(|p| changes.properties_for(*p).is_none());
        let tokens = scene.store().tokens();
        self.held.retain(|key, held| {
            self.stats.routed += 1;
            if structural && !scene.stage().has_prim(key.owner) {
                return false;
            }
            let was_dirty = held.query_dirty || held.decisions_dirty;
            if structural || unknown {
                held.query_dirty = true;
                held.decisions_dirty = true;
            }
            for inventory in &changes.property_changes {
                for field in &inventory.fields {
                    if let Some(name) = tokens
                        .resolve(field.name)
                        .strip_prefix("collection:")
                        .and_then(|s| s.rsplit_once(':').map(|(name, _)| name))
                    {
                        let dependency = CollectionIdentity {
                            owner: inventory.prim,
                            name: name.into(),
                        };
                        if held.dependencies.all_collections
                            || held.dependencies.collections.contains(&dependency)
                        {
                            held.query_dirty = true;
                        }
                    }
                    if held.dependencies.scene_objects
                        && held
                            .targets
                            .contains(&TargetPath::Property(PropertyPath::new(
                                inventory.prim,
                                field.name,
                            )))
                    {
                        held.decisions_dirty = true;
                    }
                }
            }
            if !was_dirty && (held.query_dirty || held.decisions_dirty) {
                self.stats.invalidated += 1;
            }
            true
        });
    }
}
fn dependencies(
    scene: &Scene<'_>,
    own: &CollectionIdentity,
    query: &MembershipQuery,
) -> MembershipDependencies {
    let mut paths = query.included_collections().to_vec();
    paths.extend_from_slice(query.referenced_collections());
    for problem in query.problems() {
        match problem {
            MembershipProblem::CircularInclusion { collection }
            | MembershipProblem::MissingPrim { collection }
            | MembershipProblem::InvalidExpression { collection, .. }
            | MembershipProblem::ReferenceCycle { collection, .. } => paths.push(*collection),
            MembershipProblem::MissingReference { from, .. } => paths.push(*from),
            MembershipProblem::UnevaluableExpression { .. } => {}
        }
    }
    let mut collections = vec![own.clone()];
    for path in paths {
        if let Some(name) = super::collection_name(scene.store().tokens().resolve(path.property()))
        {
            collections.push(CollectionIdentity {
                owner: path.prim_path(),
                name: name.into(),
            });
        }
    }
    collections.sort();
    collections.dedup();
    MembershipDependencies {
        collections,
        all_collections: query
            .problems()
            .iter()
            .any(|p| matches!(p, MembershipProblem::MissingReference { .. })),
        scene_objects: !query.uses_rule_map() && query.evaluator().is_some(),
    }
}
