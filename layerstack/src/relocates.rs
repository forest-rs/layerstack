// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Relocation tables.
//!
//! Relocates move prims to new paths in a layer stack's namespace: the prim
//! at a relocation's source is composed at its target, and the source path
//! no longer exists (AOUSD Core §10.3.2.6). Each layer of a layer stack
//! authors entries in its `layerRelocates` metadata ([`Layer::relocates`]);
//! a [`RelocationTable`] is the validated result for the whole layer stack.
//!
//! Entries are gathered strongest layer first, and the first entry for a
//! source path wins. An entry that is invalid on its own, or that conflicts
//! with another entry, is a composition error and is ignored. What remains
//! is the stack's incremental relocation map: each source is a path in the
//! namespace that the entries of its ancestors already relocated (§10.3.2.6:
//! "the relocate source path must use the ancestral relocated path"), so no
//! valid source lies beneath another source, and no target does either.
//!
//! OpenUSD: `Pcp_ComputeRelocationsForLayerStack` and
//! `Pcp_IsValidRelocatesEntry` in `pxr/usd/pcp/layerStack.cpp`, and
//! `PcpLayerStack::GetIncrementalRelocatesSourceToTarget`.
//!
//! [`Layer::relocates`]: crate::Layer::relocates

use alloc::{rc::Rc, vec::Vec};
use core::cmp::Ordering;

use hashbrown::{HashMap, HashSet};

use crate::{
    composition_error::{
        CompositionError, InvalidAuthoredRelocation, InvalidConflictingRelocation,
        InvalidRelocationReason, InvalidSameTargetRelocations, RelocationConflict,
    },
    doc::{LayerId, LayerStore, Relocate},
    layer_stack::LayerStack,
    path::{Path, PathId, PathInterner},
};

/// The valid relocates of one layer stack, in both directions.
///
/// Spec: AOUSD Core §10.3.2.6 (relocates are computed per layer stack from
/// the `layerRelocates` field of each of its layers).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelocationTable {
    /// Source → target (`None`: the source is removed).
    by_source: HashMap<PathId, Option<PathId>>,
    /// Target → source, for entries with a target.
    by_target: HashMap<PathId, PathId>,
}

/// One gathered entry: the relocate and the layer authoring it.
#[derive(Clone, Copy)]
struct Entry {
    relocate: Relocate,
    layer: LayerId,
}

impl RelocationTable {
    /// Computes the relocation table of `stack`, appending an error for
    /// each entry it ignores to `errors`.
    ///
    /// Errors are appended in a deterministic order: entries invalid on
    /// their own in gathering order, then shared targets by target path,
    /// then conflicts by source path, conflict reason and conflicting
    /// source path, as OpenUSD sorts them.
    ///
    /// Spec: AOUSD Core §10.3.2.6. OpenUSD:
    /// `Pcp_ComputeRelocationsForLayerStackWorkspace` in
    /// `pxr/usd/pcp/layerStack.cpp`, which reports the same
    /// `PcpErrorInvalidAuthoredRelocation`,
    /// `PcpErrorInvalidSameTargetRelocations` and
    /// `PcpErrorInvalidConflictingRelocation` errors.
    pub fn compute(
        store: &dyn LayerStore,
        stack: &LayerStack,
        errors: &mut Vec<CompositionError>,
    ) -> Self {
        let paths = store.paths();
        let mut entries: Vec<Entry> = Vec::new();
        let mut index: HashMap<PathId, usize> = HashMap::new();
        for &layer in &stack.layers {
            let Some(authored) = store.layer(layer) else {
                continue;
            };
            for &relocate in &authored.relocates {
                if let Some(reason) = invalid_reason(paths, relocate) {
                    errors.push(CompositionError::InvalidAuthoredRelocation(
                        InvalidAuthoredRelocation {
                            layer,
                            source: relocate.source,
                            target: relocate.target,
                            reason,
                        },
                    ));
                    continue;
                }
                // A stronger layer's entry for the same source wins; that is
                // not an error.
                index.entry(relocate.source).or_insert_with(|| {
                    entries.push(Entry { relocate, layer });
                    entries.len() - 1
                });
            }
        }

        let cmp_paths = |a: PathId, b: PathId| cmp_paths(store, a, b);
        let mut ignored = alloc::vec![false; entries.len()];

        // Different sources moved to one target.
        let mut by_target: HashMap<PathId, Vec<usize>> = HashMap::new();
        for (at, entry) in entries.iter().enumerate() {
            if let Some(target) = entry.relocate.target {
                by_target.entry(target).or_default().push(at);
            }
        }
        let mut shared: Vec<(PathId, Vec<usize>)> = by_target
            .into_iter()
            .filter(|(_, sources)| sources.len() > 1)
            .collect();
        shared.sort_by(|a, b| cmp_paths(a.0, b.0));
        let mut same_target = Vec::new();
        for (target, mut sources) in shared {
            sources.sort_by(|a, b| {
                cmp_paths(entries[*a].relocate.source, entries[*b].relocate.source)
            });
            for &at in &sources {
                ignored[at] = true;
            }
            same_target.push(CompositionError::InvalidSameTargetRelocations(
                InvalidSameTargetRelocations {
                    target,
                    sources: sources
                        .iter()
                        .map(|&at| (entries[at].layer, entries[at].relocate.source))
                        .collect(),
                },
            ));
        }

        // Entries that conflict with another entry, each reported from its
        // own side.
        let mut conflicts: Vec<(usize, usize, RelocationConflict)> = Vec::new();
        for (at, entry) in entries.iter().enumerate() {
            let Relocate { source, target } = entry.relocate;
            if let Some(target) = target {
                if let Some(&other) = index.get(&target) {
                    conflicts.push((at, other, RelocationConflict::TargetIsConflictSource));
                    conflicts.push((other, at, RelocationConflict::SourceIsConflictTarget));
                }
                for ancestor in proper_ancestors(paths, target) {
                    if let Some(&other) = index.get(&ancestor) {
                        conflicts.push((
                            at,
                            other,
                            RelocationConflict::TargetIsConflictSourceDescendant,
                        ));
                    }
                }
            }
            for ancestor in proper_ancestors(paths, source) {
                if let Some(&other) = index.get(&ancestor) {
                    conflicts.push((
                        at,
                        other,
                        RelocationConflict::SourceIsConflictSourceDescendant,
                    ));
                }
            }
        }
        conflicts.sort_by(|a, b| {
            cmp_paths(entries[a.0].relocate.source, entries[b.0].relocate.source)
                .then_with(|| a.2.cmp(&b.2))
                .then_with(|| cmp_paths(entries[a.1].relocate.source, entries[b.1].relocate.source))
        });
        conflicts.dedup();

        errors.extend(same_target);
        for &(at, other, reason) in &conflicts {
            ignored[at] = true;
            let (entry, conflict) = (entries[at], entries[other]);
            errors.push(CompositionError::InvalidConflictingRelocation(
                InvalidConflictingRelocation {
                    layer: entry.layer,
                    source: entry.relocate.source,
                    target: entry.relocate.target,
                    conflict_layer: conflict.layer,
                    conflict_source: conflict.relocate.source,
                    conflict_target: conflict.relocate.target,
                    reason,
                },
            ));
        }

        let mut table = Self::default();
        for (entry, ignored) in entries.iter().zip(ignored) {
            if ignored {
                continue;
            }
            let Relocate { source, target } = entry.relocate;
            table.by_source.insert(source, target);
            if let Some(target) = target {
                table.by_target.insert(target, source);
            }
        }
        table
    }

    /// Returns `true` when the layer stack relocates nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_source.is_empty()
    }

    /// Returns the number of valid relocates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_source.len()
    }

    /// The relocate whose source is `path`: `Some(Some(target))` when the
    /// prim moves to `target`, `Some(None)` when it is removed, and `None`
    /// when `path` is no relocation source.
    #[must_use]
    pub fn target_of(&self, path: PathId) -> Option<Option<PathId>> {
        self.by_source.get(&path).copied()
    }

    /// The source of the relocate whose target is `path`, if any.
    #[must_use]
    pub fn source_of(&self, path: PathId) -> Option<PathId> {
        self.by_target.get(&path).copied()
    }

    /// Every valid relocate, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = Relocate> + '_ {
        self.by_source
            .iter()
            .map(|(&source, &target)| Relocate { source, target })
    }

    /// The relocation source at or above `path`, if any: the prim whose
    /// relocation moves `path` elsewhere, or removes it.
    ///
    /// Opinions a layer stack authors at or beneath a relocation source are
    /// not composed there (AOUSD Core §10.3.2.6).
    #[must_use]
    pub fn source_at_or_above(&self, paths: &PathInterner, path: PathId) -> Option<PathId> {
        if self.is_empty() {
            return None;
        }
        if self.by_source.contains_key(&path) {
            return Some(path);
        }
        proper_ancestors(paths, path).find(|ancestor| self.by_source.contains_key(ancestor))
    }
}

/// Why `relocate` is invalid on its own, if it is.
///
/// Paths are prim paths by construction (the readers reject others).
fn invalid_reason(paths: &PathInterner, relocate: Relocate) -> Option<InvalidRelocationReason> {
    let source = paths.resolve(relocate.source);
    if let Some(target) = relocate.target {
        let target_path = paths.resolve(target);
        if target == relocate.source {
            return Some(InvalidRelocationReason::TargetIsSource);
        }
        if source.is_prefix_of(target_path) {
            return Some(InvalidRelocationReason::TargetIsDescendant);
        }
        if target_path.is_prefix_of(source) {
            return Some(InvalidRelocationReason::TargetIsAncestor);
        }
    }
    // Checked after the target: OpenUSD validates the pair before the
    // root prim restriction.
    (source.depth() <= 1).then_some(InvalidRelocationReason::RootPrimSource)
}

/// The interned proper ancestors of `path` below the pseudo-root, nearest
/// first. Ancestors that were never interned cannot be relocation paths
/// and are skipped.
fn proper_ancestors(paths: &PathInterner, path: PathId) -> impl Iterator<Item = PathId> + '_ {
    let mut current: Option<Path> = paths.resolve(path).parent();
    core::iter::from_fn(move || {
        loop {
            let path = current.take()?;
            if path.depth() == 0 {
                return None;
            }
            current = path.parent();
            if let Some(id) = paths.lookup(&path) {
                return Some(id);
            }
        }
    })
}

fn cmp_paths(store: &dyn LayerStore, a: PathId, b: PathId) -> Ordering {
    let paths = store.paths();
    paths
        .resolve(a)
        .cmp_with_tokens(paths.resolve(b), store.tokens())
}

// ── Relocations in the stage namespace ────────────────────────────────────
//
// Composition maps an arc's target namespace into the stage namespace one
// arc at a time. A relocation of a layer stack that an arc reaches applies
// to the part of the stage namespace that arc maps: its source and target
// are *lifted* through the arcs from the stage to that layer stack. The
// lifted relocations of every layer stack on the way to an arc's target
// then decide where each opinion of the target lands (see [`Walk`]).

/// A relocate of one layer stack, lifted into the stage namespace through
/// the arcs that reach that layer stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiftedRelocate {
    /// Root layer of the relocating layer stack.
    pub(crate) layer_stack: LayerId,
    /// The relocation source, in that layer stack's namespace.
    pub(crate) source: PathId,
    /// The relocation target there; `None` when the source is removed.
    pub(crate) target: Option<PathId>,
    /// Where the source lies in the stage namespace; `None` when the arc
    /// that lifts it maps only the target.
    pub(crate) stage_source: Option<PathId>,
    /// Where the target lies in the stage namespace; `None` when the source
    /// is removed or moved outside the namespace the arc maps.
    pub(crate) stage_target: Option<PathId>,
}

/// The relocations of one layer stack lifted through one arc, indexed by
/// their stage paths.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiftedSet {
    entries: Vec<LiftedRelocate>,
    by_stage_source: HashMap<PathId, usize>,
    by_stage_target: HashMap<PathId, usize>,
}

impl LiftedSet {
    fn push(&mut self, relocate: LiftedRelocate) {
        let at = self.entries.len();
        if let Some(source) = relocate.stage_source {
            self.by_stage_source.entry(source).or_insert(at);
        }
        if let Some(target) = relocate.stage_target {
            self.by_stage_target.entry(target).or_insert(at);
        }
        self.entries.push(relocate);
    }

    /// Returns `true` when nothing is lifted.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn source(&self, stage_path: PathId) -> Option<&LiftedRelocate> {
        self.by_stage_source
            .get(&stage_path)
            .map(|&at| &self.entries[at])
    }

    fn target(&self, stage_path: PathId) -> Option<&LiftedRelocate> {
        self.by_stage_target
            .get(&stage_path)
            .map(|&at| &self.entries[at])
    }

    /// The relocations of `table`, the table of the layer stack rooted at
    /// `layer_stack`, lifted through an arc that maps `target_root` in that
    /// layer stack onto the stage path `dest_root`, the stage relocations
    /// `outer` applying on the way.
    ///
    /// A relocation beneath the arc's target is lifted onto the path the
    /// arc maps it to; one outside the arc's target cannot be reached
    /// through it and is not lifted, and a target outside maps nowhere.
    ///
    /// Spec: AOUSD Core §10.3.2.6.1 (relocates add to the namespace mapping
    /// of the arcs that reach their layer stack).
    pub(crate) fn lift(
        store: &mut dyn LayerStore,
        table: &RelocationTable,
        layer_stack: LayerId,
        target_root: PathId,
        dest_root: PathId,
        outer: &Walk<'_>,
    ) -> Self {
        let mut lifted = Self::default();
        if table.is_empty() {
            return lifted;
        }
        let mut relocates: Vec<Relocate> = table.iter().collect();
        // Deterministic order for the entries with equal stage paths.
        relocates.sort_by(|a, b| cmp_paths(store, a.source, b.source));
        for relocate in relocates {
            let rel_of = |store: &dyn LayerStore, path: PathId| {
                let paths = store.paths();
                paths
                    .resolve(path)
                    .strip_prefix(paths.resolve(target_root))
                    .filter(|rel| !rel.is_empty())
                    .map(<[_]>::to_vec)
            };
            let place = |store: &mut dyn LayerStore, rel: Option<Vec<_>>| {
                rel.and_then(|rel| outer.place(store, dest_root, &rel))
                    .map(|(path, _)| path)
            };
            let source_rel = rel_of(store, relocate.source);
            let target_rel = relocate.target.and_then(|target| rel_of(store, target));
            if source_rel.is_none() && target_rel.is_none() {
                continue;
            }
            let stage_source = place(store, source_rel);
            let stage_target = place(store, target_rel);
            lifted.push(LiftedRelocate {
                layer_stack,
                source: relocate.source,
                target: relocate.target,
                stage_source,
                stage_target,
            });
        }
        lifted
    }

    /// The relocations of the stage's own layer stack, whose namespace is
    /// the stage namespace.
    pub(crate) fn stage(table: &RelocationTable, layer_stack: LayerId) -> Self {
        let mut lifted = Self::default();
        for relocate in table.iter() {
            lifted.push(LiftedRelocate {
                layer_stack,
                source: relocate.source,
                target: relocate.target,
                stage_source: Some(relocate.source),
                stage_target: relocate.target,
            });
        }
        lifted
    }
}

/// The lifted relocations an arc's opinions pass on their way into the
/// stage namespace: those of every layer stack on the way to the arc
/// (`outer`, the stage's own first), and those of the arc's target layer
/// stack (`own`).
///
/// Placing an opinion walks from the stage path of the site authoring the
/// arc down to the opinion's path, one namespace child at a time:
///
/// - Stepping into an `outer` source moves the walk to its target: the arc
///   is authored on an ancestor of the source, and the source's ancestral
///   opinions compose at the target. A source removed by its relocation
///   drops the opinion.
/// - Stepping into an `outer` target drops the opinion: ancestral opinions
///   at a relocation target, other than the relocation's own, are ignored.
/// - Stepping into an `own` source drops the opinion: the target layer
///   stack authors it at a relocation source. `own` targets are the target
///   layer stack's own sites and are kept.
///
/// Spec: AOUSD Core §10.3.2.6 ("All previously-computed ancestral opinions
/// except those due to ancestral variant arcs are removed"; opinions at a
/// relocation source are ignored). OpenUSD: `_EvalNodeRelocations` and
/// `_ElideRelocatedSubtrees` in `pxr/usd/pcp/primIndex.cpp`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Walk<'a> {
    outer: Vec<&'a LiftedSet>,
    own: Option<&'a LiftedSet>,
}

impl<'a> Walk<'a> {
    /// A walk through `outer`, strongest (the stage's) first, then `own`.
    pub(crate) fn new(
        outer: impl IntoIterator<Item = &'a LiftedSet>,
        own: Option<&'a LiftedSet>,
    ) -> Self {
        Self {
            outer: outer.into_iter().filter(|set| !set.is_empty()).collect(),
            own: own.filter(|set| !set.is_empty()),
        }
    }

    /// Returns `true` when no relocation applies, so placing a path only
    /// joins it onto its destination.
    pub(crate) fn is_empty(&self) -> bool {
        self.outer.is_empty() && self.own.is_none()
    }

    /// The same walk with `own` counted as outer: the walk of an arc nested
    /// in the target of this one.
    pub(crate) fn nested(&self) -> Self {
        let mut outer = self.outer.clone();
        outer.extend(self.own);
        Self { outer, own: None }
    }

    /// Places the path `rel` beneath the stage path `dest_root` (see
    /// [`Walk`]): the stage path it lands on, and whether a relocation moved
    /// it; `None` when the walk drops it.
    pub(crate) fn place(
        &self,
        store: &mut dyn LayerStore,
        dest_root: PathId,
        rel: &[crate::interner::TokenId],
    ) -> Option<(PathId, bool)> {
        let mut current = store.paths().resolve(dest_root).clone();
        if self.is_empty() {
            let joined = current.join(rel);
            return Some((store.paths_mut().intern(joined), false));
        }
        let mut moved = false;
        for &name in rel {
            let next = current.join(&[name]);
            let paths = store.paths();
            if let Some(id) = paths.lookup(&next) {
                if self.own.is_some_and(|own| own.source(id).is_some()) {
                    return None;
                }
                if let Some(relocate) = self.outer.iter().rev().find_map(|set| set.source(id)) {
                    let target = relocate.stage_target?;
                    current = paths.resolve(target).clone();
                    moved = true;
                    continue;
                }
                if self.outer.iter().any(|set| set.target(id).is_some()) {
                    return None;
                }
            }
            current = next;
        }
        Some((store.paths_mut().intern(current), moved))
    }

    /// The `outer` relocations a walk from `host` took to reach the stage
    /// path `dest`, latest first, each with the site the walk passed in the
    /// relocating layer stack: the relocation's source extended towards
    /// `dest`. Also returns `dest` mapped back to the stage path the walk
    /// would have reached without them.
    ///
    /// A walk reaches a target only through its source, so every target
    /// above `dest` that is not above `host` was reached that way: the
    /// deepest one is the last relocation taken.
    pub(crate) fn unwind(
        &self,
        store: &mut dyn LayerStore,
        host: PathId,
        dest: PathId,
    ) -> (Vec<(LiftedRelocate, PathId)>, PathId) {
        let mut taken = Vec::new();
        let mut view = dest;
        if self.outer.is_empty() {
            return (taken, view);
        }
        // Each relocation is taken at most once on a walk.
        let limit: usize = self.outer.iter().map(|set| set.entries.len()).sum();
        while taken.len() < limit {
            let paths = store.paths();
            let host_path = paths.resolve(host);
            let mut cursor = Some(paths.resolve(view).clone());
            let mut found = None;
            while let Some(path) = cursor {
                if path.is_prefix_of(host_path) {
                    break;
                }
                if let Some(relocate) = paths
                    .lookup(&path)
                    .and_then(|id| self.outer.iter().rev().find_map(|set| set.target(id)))
                    && relocate.stage_source.is_some()
                {
                    found = Some((*relocate, path));
                    break;
                }
                cursor = path.parent();
            }
            let Some((relocate, target_path)) = found else {
                return (taken, view);
            };
            let stage_source = relocate
                .stage_source
                .expect("an unwound relocation has a stage source");
            let rel = paths
                .resolve(view)
                .strip_prefix(&target_path)
                .expect("the target is above the view")
                .to_vec();
            let source_view = paths.resolve(stage_source).join(&rel);
            let site = paths.resolve(relocate.source).join(&rel);
            let site = store.paths_mut().intern(site);
            view = store.paths_mut().intern(source_view);
            taken.push((relocate, site));
        }
        (taken, view)
    }
}

/// The relocation state of one composition: the relocation table of each
/// layer stack it reaches, the stage's own relocations, and the stage paths
/// that relocations prohibit.
///
/// Only the relocations of the stage's layer stack and those composition
/// lifts through the arcs it follows are [prohibited](Self::prohibit).
/// Population follows the arcs of every variant branch, selected or not,
/// so the relocations it lifts are only [proposed](Self::propose): they
/// may add the paths of targets, but never remove the paths of sources.
#[derive(Debug, Default)]
pub(crate) struct Relocations {
    tables: HashMap<LayerId, Rc<RelocationTable>>,
    stage: Rc<LiftedSet>,
    /// Stage paths of lifted relocation sources: prims that do not exist.
    prohibited: HashSet<PathId>,
    /// Stage paths that population placed only through a relocation.
    moved: HashSet<PathId>,
    /// Stage paths of lifted relocation targets, each with its source's.
    targets: HashMap<PathId, PathId>,
    /// The targets population proposes, as `targets`.
    proposed: HashMap<PathId, PathId>,
    /// Errors found computing tables, not yet reported.
    errors: Vec<CompositionError>,
}

impl Relocations {
    /// The relocation state of a stage whose layer stack is `stack`.
    pub(crate) fn new(store: &dyn LayerStore, stack: &LayerStack) -> Self {
        let mut relocations = Self::default();
        let Some(&root) = stack.layers.first() else {
            return relocations;
        };
        let table = relocations.table(store, stack);
        let stage = LiftedSet::stage(&table, root);
        relocations.prohibit(&stage);
        relocations.stage = Rc::new(stage);
        relocations
    }

    /// The relocation table of `stack`, computed once per layer stack.
    pub(crate) fn table(
        &mut self,
        store: &dyn LayerStore,
        stack: &LayerStack,
    ) -> Rc<RelocationTable> {
        let Some(&root) = stack.layers.first() else {
            return Rc::default();
        };
        if let Some(table) = self.tables.get(&root) {
            return Rc::clone(table);
        }
        let table = Rc::new(RelocationTable::compute(store, stack, &mut self.errors));
        self.tables.insert(root, Rc::clone(&table));
        table
    }

    /// The table of the layer stack rooted at `root`, if already computed.
    pub(crate) fn cached_table(&self, root: LayerId) -> Option<Rc<RelocationTable>> {
        self.tables.get(&root).cloned()
    }

    /// The relocations of the stage's own layer stack.
    pub(crate) fn stage(&self) -> Rc<LiftedSet> {
        Rc::clone(&self.stage)
    }

    /// Records the lifted sources of `set` as prohibited stage paths, and
    /// its lifted targets.
    pub(crate) fn prohibit(&mut self, set: &LiftedSet) {
        for relocate in &set.entries {
            if let Some(source) = relocate.stage_source {
                self.prohibited.insert(source);
                if let Some(target) = relocate.stage_target {
                    self.targets.entry(target).or_insert(source);
                }
            }
        }
    }

    /// Records the lifted targets of `set`, a set population reaches
    /// through an arc composition may not follow, as possible stage paths.
    pub(crate) fn propose(&mut self, set: &LiftedSet) {
        for relocate in &set.entries {
            if let (Some(source), Some(target)) = (relocate.stage_source, relocate.stage_target) {
                self.proposed.entry(target).or_insert(source);
            }
        }
    }

    /// Each lifted relocation target in the stage namespace that
    /// population proposed or composition prohibited the source of, with
    /// the stage path of its source.
    pub(crate) fn proposed_targets(&self) -> impl Iterator<Item = (PathId, PathId)> + '_ {
        self.proposed
            .iter()
            .filter(|(target, _)| !self.targets.contains_key(*target))
            .chain(&self.targets)
            .map(|(&target, &source)| (target, source))
    }

    /// Returns `true` when `path` is a lifted relocation target.
    pub(crate) fn is_target(&self, path: PathId) -> bool {
        self.targets.contains_key(&path)
    }

    /// Records that population placed the stage path `path`, through a
    /// relocation when `moved`; `new` when the path was not populated yet.
    pub(crate) fn place(&mut self, path: PathId, moved: bool, new: bool) {
        if !moved {
            self.moved.remove(&path);
        } else if new {
            self.moved.insert(path);
        }
    }

    /// Takes the stage paths population placed only through a relocation.
    pub(crate) fn take_moved(&mut self) -> HashSet<PathId> {
        core::mem::take(&mut self.moved)
    }

    /// Returns `true` when `path` is at or beneath a prohibited stage path.
    pub(crate) fn is_prohibited(&self, paths: &PathInterner, path: PathId) -> bool {
        if self.prohibited.is_empty() {
            return false;
        }
        self.prohibited.contains(&path)
            || proper_ancestors(paths, path).any(|ancestor| self.prohibited.contains(&ancestor))
    }

    /// Takes the errors found computing tables since the last call.
    pub(crate) fn take_errors(&mut self) -> Vec<CompositionError> {
        core::mem::take(&mut self.errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{InMemoryStore, Layer, SublayerEntry};
    use alloc::{string::String, vec};

    fn path(store: &mut InMemoryStore, text: &str) -> PathId {
        store.path(text)
    }

    fn relocate(store: &mut InMemoryStore, source: &str, target: &str) -> Relocate {
        Relocate {
            source: path(store, source),
            target: (!target.is_empty()).then(|| path(store, target)),
        }
    }

    fn table(store: &InMemoryStore, root: u64) -> (RelocationTable, Vec<CompositionError>) {
        let stack = LayerStack::gather(store, LayerId(root));
        let mut errors = Vec::new();
        let table = RelocationTable::compute(store, &stack, &mut errors);
        (table, errors)
    }

    #[test]
    fn stronger_layers_win_a_shared_source() {
        let mut store = InMemoryStore::default();
        let mut root = Layer::new(LayerId(1));
        root.sublayers = vec![SublayerEntry::new(LayerId(2))];
        root.relocates = vec![relocate(&mut store, "/A/B", "/A/C")];
        let mut sub = Layer::new(LayerId(2));
        sub.relocates = vec![
            relocate(&mut store, "/A/B", "/A/D"),
            relocate(&mut store, "/A/E", ""),
        ];
        store.insert_layer(root);
        store.insert_layer(sub);

        let (table, errors) = table(&store, 1);
        assert!(errors.is_empty(), "{errors:?}");
        let (b, c, e) = (
            path(&mut store, "/A/B"),
            path(&mut store, "/A/C"),
            path(&mut store, "/A/E"),
        );
        assert_eq!(table.len(), 2);
        assert_eq!(table.target_of(b), Some(Some(c)));
        assert_eq!(table.source_of(c), Some(b));
        assert_eq!(table.target_of(e), Some(None));
        let child = path(&mut store, "/A/E/Child");
        assert_eq!(table.source_at_or_above(&store.paths, child), Some(e));
        assert_eq!(table.source_at_or_above(&store.paths, c), None);
    }

    #[test]
    fn invalid_entries_are_reported_and_ignored() {
        // The cases of the supplemental `ErrorInvalidAuthoredRelocates`
        // fixture (AOUSD Core §10.3.2.6).
        let mut store = InMemoryStore::default();
        let mut root = Layer::new(LayerId(1));
        root.relocates = vec![
            relocate(&mut store, "/M1/I/T", "/M1/I/T"),
            relocate(&mut store, "/M2/I/T", "/M2/I"),
            relocate(&mut store, "/M3/I", "/M3/I/T"),
            relocate(&mut store, "/M4", "/M5"),
        ];
        let authored = root.relocates.clone();
        store.insert_layer(root);

        let (table, errors) = table(&store, 1);
        assert!(table.is_empty());
        let reasons = [
            InvalidRelocationReason::TargetIsSource,
            InvalidRelocationReason::TargetIsAncestor,
            InvalidRelocationReason::TargetIsDescendant,
            InvalidRelocationReason::RootPrimSource,
        ];
        let expected: Vec<CompositionError> = authored
            .iter()
            .zip(reasons)
            .map(|(relocate, reason)| {
                CompositionError::InvalidAuthoredRelocation(InvalidAuthoredRelocation {
                    layer: LayerId(1),
                    source: relocate.source,
                    target: relocate.target,
                    reason,
                })
            })
            .collect();
        assert_eq!(errors, expected);
    }

    #[test]
    fn conflicting_entries_are_all_ignored() {
        let mut store = InMemoryStore::default();
        let mut root = Layer::new(LayerId(1));
        root.relocates = vec![
            // Two sources, one target.
            relocate(&mut store, "/A/B", "/A/T"),
            relocate(&mut store, "/A/C", "/A/T"),
            // A chain: the target of one is the source of the next.
            relocate(&mut store, "/X/A", "/X/B"),
            relocate(&mut store, "/X/B", "/X/C"),
            // A source beneath another source: only the deeper one is
            // ignored.
            relocate(&mut store, "/Y/A", "/Y/Z"),
            relocate(&mut store, "/Y/A/B", "/Y/W"),
            // Kept.
            relocate(&mut store, "/K/A", "/K/B"),
        ];
        store.insert_layer(root);

        let (table, errors) = table(&store, 1);
        let mut kept: Vec<PathId> = table.iter().map(|r| r.source).collect();
        kept.sort_unstable();
        let mut expected_kept = vec![path(&mut store, "/Y/A"), path(&mut store, "/K/A")];
        expected_kept.sort_unstable();
        assert_eq!(kept, expected_kept);

        let p = |store: &mut InMemoryStore, text: &str| path(store, text);
        let conflict = |store: &mut InMemoryStore,
                        (source, target): (&str, &str),
                        (conflict_source, conflict_target): (&str, &str),
                        reason| {
            CompositionError::InvalidConflictingRelocation(InvalidConflictingRelocation {
                layer: LayerId(1),
                source: p(store, source),
                target: Some(p(store, target)),
                conflict_layer: LayerId(1),
                conflict_source: p(store, conflict_source),
                conflict_target: Some(p(store, conflict_target)),
                reason,
            })
        };
        let expected = vec![
            CompositionError::InvalidSameTargetRelocations(InvalidSameTargetRelocations {
                target: p(&mut store, "/A/T"),
                sources: vec![
                    (LayerId(1), p(&mut store, "/A/B")),
                    (LayerId(1), p(&mut store, "/A/C")),
                ],
            }),
            conflict(
                &mut store,
                ("/X/A", "/X/B"),
                ("/X/B", "/X/C"),
                RelocationConflict::TargetIsConflictSource,
            ),
            conflict(
                &mut store,
                ("/X/B", "/X/C"),
                ("/X/A", "/X/B"),
                RelocationConflict::SourceIsConflictTarget,
            ),
            conflict(
                &mut store,
                ("/Y/A/B", "/Y/W"),
                ("/Y/A", "/Y/Z"),
                RelocationConflict::SourceIsConflictSourceDescendant,
            ),
        ];
        assert_eq!(errors, expected);
    }

    #[test]
    fn stages_report_the_errors_of_their_layer_stack() {
        let mut store = InMemoryStore::default();
        let mut root = Layer::new(LayerId(1));
        let invalid = relocate(&mut store, "/A", "/B");
        root.relocates = vec![invalid];
        store.insert_layer(root);
        let stage = crate::Stage::compose(&mut store, LayerId(1), crate::StageOptions::default());
        assert_eq!(
            stage.composition_errors(),
            [CompositionError::InvalidAuthoredRelocation(
                InvalidAuthoredRelocation {
                    layer: LayerId(1),
                    source: invalid.source,
                    target: invalid.target,
                    reason: InvalidRelocationReason::RootPrimSource,
                }
            )]
        );
        assert_eq!(stage.composition_errors()[0].prim(), None);
    }

    #[test]
    fn a_target_beneath_a_source_conflicts() {
        let mut store = InMemoryStore::default();
        let mut root = Layer::new(LayerId(1));
        root.relocates = vec![
            relocate(&mut store, "/A/B", "/A/C"),
            relocate(&mut store, "/D/E", "/A/B/E"),
        ];
        store.insert_layer(root);
        let (table, errors) = table(&store, 1);
        assert_eq!(table.len(), 1, "only the conflicting entry is ignored");
        assert!(matches!(
            errors.as_slice(),
            [CompositionError::InvalidConflictingRelocation(
                InvalidConflictingRelocation {
                    reason: RelocationConflict::TargetIsConflictSourceDescendant,
                    ..
                }
            )]
        ));
    }

    /// A table relocating `/A/B` to `/A/C` and removing `/A/D`, lifted
    /// through an arc from the stage path `/X` to `/A`.
    fn lifted(store: &mut InMemoryStore) -> LiftedSet {
        let mut layer = Layer::new(LayerId(2));
        layer.relocates = vec![relocate(store, "/A/B", "/A/C"), relocate(store, "/A/D", "")];
        store.insert_layer(layer);
        let (table, errors) = table(store, 2);
        assert!(errors.is_empty(), "{errors:?}");
        let (target_root, dest_root) = (path(store, "/A"), path(store, "/X"));
        LiftedSet::lift(
            store,
            &table,
            LayerId(2),
            target_root,
            dest_root,
            &Walk::default(),
        )
    }

    fn names(store: &mut InMemoryStore, text: &str) -> Vec<crate::interner::TokenId> {
        text.split('/')
            .filter(|name| !name.is_empty())
            .map(|name| store.tokens.intern(name))
            .collect()
    }

    fn place(store: &mut InMemoryStore, walk: &Walk<'_>, rel: &str) -> Option<String> {
        let root = path(store, "/X");
        let rel = names(store, rel);
        walk.place(store, root, &rel)
            .map(|(path, _)| store.paths.display(path, &store.tokens))
    }

    #[test]
    fn relocations_lift_through_the_arc_that_reaches_them() {
        let mut store = InMemoryStore::default();
        let set = lifted(&mut store);
        let stage = |store: &InMemoryStore, path: Option<PathId>| {
            path.map(|path| store.paths.display(path, &store.tokens))
        };
        let lifted: Vec<(Option<String>, Option<String>)> = set
            .entries
            .iter()
            .map(|relocate| {
                (
                    stage(&store, relocate.stage_source),
                    stage(&store, relocate.stage_target),
                )
            })
            .collect();
        assert_eq!(
            lifted,
            [
                (Some("/X/B".into()), Some("/X/C".into())),
                (Some("/X/D".into()), None),
            ]
        );
    }

    #[test]
    fn walks_move_ancestral_opinions_and_drop_the_others() {
        // Spec: AOUSD Core §10.3.2.6. Opinions of an arc authored above
        // the source move to the target; ancestral opinions at the target
        // and opinions at a removed source are dropped.
        let mut store = InMemoryStore::default();
        let set = lifted(&mut store);
        let outer = Walk::new([&set], None);
        assert_eq!(
            place(&mut store, &outer, "B/Child"),
            Some("/X/C/Child".into())
        );
        assert_eq!(place(&mut store, &outer, "C/Child"), None);
        assert_eq!(place(&mut store, &outer, "D"), None);
        assert_eq!(place(&mut store, &outer, "E"), Some("/X/E".into()));

        // The relocating layer stack's own opinions at a source are
        // ignored; at a target they are its own.
        let own = Walk::new([], Some(&set));
        assert_eq!(place(&mut store, &own, "B/Child"), None);
        assert_eq!(
            place(&mut store, &own, "C/Child"),
            Some("/X/C/Child".into())
        );
    }

    #[test]
    fn unwinding_retraces_the_relocations_a_walk_took() {
        let mut store = InMemoryStore::default();
        let set = lifted(&mut store);
        let outer = Walk::new([&set], None);
        let (host, dest) = (path(&mut store, "/X"), path(&mut store, "/X/C/Child"));
        let (taken, view) = outer.unwind(&mut store, host, dest);
        let site = path(&mut store, "/A/B/Child");
        assert_eq!(
            taken.iter().map(|(_, site)| *site).collect::<Vec<_>>(),
            [site]
        );
        assert_eq!(view, path(&mut store, "/X/B/Child"));

        // A walk from beneath the target took no relocation.
        let inside = path(&mut store, "/X/C");
        let (taken, view) = outer.unwind(&mut store, inside, dest);
        assert!(taken.is_empty());
        assert_eq!(view, dest);
    }
}
