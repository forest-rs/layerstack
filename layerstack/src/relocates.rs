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

use alloc::vec::Vec;
use core::cmp::Ordering;

use hashbrown::HashMap;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{InMemoryStore, Layer, SublayerEntry};
    use alloc::vec;

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
}
