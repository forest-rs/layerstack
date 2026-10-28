// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Predicate traversal owns snapshot iteration, not population or loading.
//! AOUSD Core §11; OpenUSD `UsdPrimRange`, `UsdPrimDefaultPredicate`.
use super::*;

/// Composed prim flags used by traversal and inspection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PrimStatus {
    /// The prim and its ancestors are active.
    pub active: bool,
    /// Every required ancestor payload is selected by the stage's load rules.
    pub loaded: bool,
    /// The prim and its ancestors have defining specifiers.
    pub defined: bool,
    /// The prim or an ancestor has a class specifier.
    pub abstract_: bool,
    /// The prim is a native instance root.
    pub instance: bool,
    /// The prim is beneath a native instance root.
    pub instance_proxy: bool,
}

/// Conjunction of composed prim flags. `None` accepts either flag value.
/// A failing prim prunes its subtree, matching OpenUSD predicate traversal.
///
/// Build it as a struct literal that names the fields it sets and takes
/// the rest from `..PrimPredicate::default()`: a field a later release adds then
/// keeps its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimPredicate {
    /// Required active state, or either state.
    pub active: Option<bool>,
    /// Required loaded state, or either state.
    pub loaded: Option<bool>,
    /// Required defined state, or either state.
    pub defined: Option<bool>,
    /// Required abstract state, or either state.
    pub abstract_: Option<bool>,
    /// Required instance-root state, or either state.
    pub instance: Option<bool>,
    /// Whether traversal may descend into native instance proxies.
    pub instance_proxies: bool,
}
impl PrimPredicate {
    /// Active, loaded, defined, non-abstract prims; stops at instance roots.
    pub const DEFAULT: Self = Self {
        active: Some(true),
        loaded: Some(true),
        defined: Some(true),
        abstract_: Some(false),
        instance: None,
        instance_proxies: false,
    };
    /// Every populated prim, including inactive roots; stops at instance roots.
    /// Inactive descendants are not populated by composition.
    pub const ALL: Self = Self {
        active: None,
        loaded: None,
        defined: None,
        abstract_: None,
        instance: None,
        instance_proxies: false,
    };
    /// Tests a prim's flags independently of traversal.
    pub fn matches(self, status: PrimStatus) -> bool {
        [
            (self.active, status.active),
            (self.loaded, status.loaded),
            (self.defined, status.defined),
            (self.abstract_, status.abstract_),
            (self.instance, status.instance),
        ]
        .iter()
        .all(|(requirement, actual)| requirement.is_none_or(|v| v == *actual))
            && (self.instance_proxies || !status.instance_proxy)
    }
}
impl Default for PrimPredicate {
    fn default() -> Self {
        Self::DEFAULT
    }
}
impl Stage {
    /// Returns flags for a populated prim; absent paths return `None`.
    pub(crate) fn prim_status(&self, prim: PathId, store: &dyn LayerStore) -> Option<PrimStatus> {
        self.has_prim(prim).then(|| PrimStatus {
            active: self.is_active(prim),
            loaded: self.is_loaded(prim, store.paths()),
            defined: self.is_defined(prim, store),
            abstract_: self.is_abstract(prim, store),
            instance: self.is_instance(prim),
            instance_proxy: self.is_instance_proxy(prim, store.paths()),
        })
    }
    /// Whether a populated path is beneath a native instance root.
    pub(crate) fn is_instance_proxy(&self, prim: PathId, paths: &crate::PathInterner) -> bool {
        if !self.has_prim(prim) {
            return false;
        }
        let mut parent = paths.resolve(prim).parent();
        while let Some(path) = parent {
            if paths.lookup(&path).is_some_and(|p| self.is_instance(p)) {
                return true;
            }
            parent = path.parent();
        }
        false
    }
    /// Traverses the matching subtree in composed order, including `root`.
    /// A rejected root yields an empty range. Auxiliary memory grows with depth.
    /// Proxy traversal is explicit; `traverse` retains its existing inspection semantics.
    pub fn prim_range<'a>(
        &'a self,
        root: PathId,
        store: &'a dyn LayerStore,
        predicate: PrimPredicate,
    ) -> PrimRange<'a> {
        PrimRange {
            visits: PrimVisits {
                stage: self,
                store,
                predicate,
                root: Some(root),
                stack: Vec::new(),
                post_order: false,
                can_prune: false,
            },
        }
    }
}

#[derive(Debug)]
struct Frame<'a> {
    prim: PathId,
    status: PrimStatus,
    children: &'a [PathId],
    next: usize,
}
/// Preorder range with explicit subtree pruning.
#[derive(Debug)]
pub struct PrimRange<'a> {
    visits: PrimVisits<'a>,
}
impl<'a> PrimRange<'a> {
    /// Skips children of the most recently yielded prim. Returns false before
    /// the first visit or after exhaustion. Repeated pruning is harmless.
    pub fn prune_children(&mut self) -> bool {
        self.visits.prune_children()
    }
    /// Visits each prim on entry and exit. Select this before iteration to get
    /// a balanced stream; selecting it midway preserves only the remaining visits.
    pub fn pre_and_post(mut self) -> PrimVisits<'a> {
        self.visits.post_order = true;
        self.visits
    }
}
impl Iterator for PrimRange<'_> {
    type Item = PathId;
    fn next(&mut self) -> Option<PathId> {
        self.visits.next().map(|v| v.prim)
    }
}
/// One visit in a preorder or pre-and-post traversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PrimVisit {
    /// Visited prim path.
    pub prim: PathId,
    /// True on exit, after all unpruned matching children have been visited.
    pub is_post_visit: bool,
}
/// Lazy traversal with balanced entry/exit events and subtree pruning.
pub struct PrimVisits<'a> {
    stage: &'a Stage,
    store: &'a dyn LayerStore,
    predicate: PrimPredicate,
    root: Option<PathId>,
    stack: Vec<Frame<'a>>,
    post_order: bool,
    can_prune: bool,
}
impl core::fmt::Debug for PrimVisits<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PrimVisits")
            .field("predicate", &self.predicate)
            .field("root", &self.root)
            .field("stack", &self.stack)
            .field("post_order", &self.post_order)
            .field("can_prune", &self.can_prune)
            .finish_non_exhaustive()
    }
}
impl PrimVisits<'_> {
    // AOUSD Core §11.5: defined, abstract, loaded and proxy state inherit from
    // ancestors. Carry it on the traversal stack; resolve each local specifier
    // once from the live store, never cache it across traversals or source edits.
    fn status(&self, prim: PathId) -> Option<PrimStatus> {
        if !self.stage.has_prim(prim) {
            return None;
        }
        let stage = self.stage;
        let predicate = self.predicate;
        let parent = self.stack.last().map(|frame| frame.status);
        let active = if predicate.active.is_some() || predicate.loaded.is_some() {
            stage.is_active(prim)
        } else {
            true
        };
        let loaded = if predicate.loaded.is_none() {
            true
        } else if let Some(parent) = parent {
            active
                && parent.loaded
                && (!stage.loadable.contains(&prim)
                    || stage
                        .options
                        .load_rules
                        .is_loaded(self.store.paths().resolve(prim)))
        } else {
            stage.is_loaded(prim, self.store.paths())
        };
        let (defined, abstract_) = if predicate.defined.is_none() && predicate.abstract_.is_none() {
            (true, false)
        } else if let Some(parent) = parent {
            let specifier = stage.resolve_specifier(prim, self.store);
            (
                parent.defined && matches!(specifier, Some(Specifier::Def | Specifier::Class)),
                parent.abstract_ || specifier == Some(Specifier::Class),
            )
        } else {
            (
                predicate.defined.is_none() || stage.is_defined(prim, self.store),
                predicate.abstract_.is_some() && stage.is_abstract(prim, self.store),
            )
        };
        let instance = (predicate.instance.is_some() || !predicate.instance_proxies)
            && stage.is_instance(prim);
        let instance_proxy = !predicate.instance_proxies
            && parent.map_or_else(
                || stage.is_instance_proxy(prim, self.store.paths()),
                |parent| parent.instance || parent.instance_proxy,
            );
        Some(PrimStatus {
            active,
            loaded,
            defined,
            abstract_,
            instance,
            instance_proxy,
        })
    }

    /// Prunes children after an entry visit. Returns false on an exit visit,
    /// before iteration or after exhaustion; a pruned prim still gets its exit.
    pub fn prune_children(&mut self) -> bool {
        if !self.can_prune {
            return false;
        }
        if let Some(frame) = self.stack.last_mut() {
            frame.next = frame.children.len();
        }
        true
    }
}
impl Iterator for PrimVisits<'_> {
    type Item = PrimVisit;
    fn next(&mut self) -> Option<PrimVisit> {
        self.can_prune = false;
        loop {
            let prim = if let Some(root) = self.root.take() {
                root
            } else {
                let frame = self.stack.last_mut()?;
                if let Some(&child) = frame.children.get(frame.next) {
                    frame.next += 1;
                    child
                } else {
                    let prim = self.stack.pop()?.prim;
                    if self.post_order {
                        return Some(PrimVisit {
                            prim,
                            is_post_visit: true,
                        });
                    }
                    continue;
                }
            };
            let Some(status) = self.status(prim) else {
                continue;
            };
            if !self.predicate.matches(status) {
                continue;
            }
            let children = if status.instance && !self.predicate.instance_proxies {
                &[][..]
            } else {
                self.stage.all_children_of(prim).unwrap_or_default()
            };
            self.stack.push(Frame {
                prim,
                status,
                children,
                next: 0,
            });
            self.can_prune = true;
            return Some(PrimVisit {
                prim,
                is_post_visit: false,
            });
        }
    }
}
impl core::iter::FusedIterator for PrimRange<'_> {}
impl core::iter::FusedIterator for PrimVisits<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryStore, Layer, LayerId, PrimSpec};

    #[test]
    fn inherited_traversal_matches_standalone_flags_for_every_predicate_and_subroot() {
        let mut store = InMemoryStore::default();
        let mut layer = Layer::new(LayerId(1));
        for (path, spec) in [
            ("/Def", PrimSpec::def()),
            ("/Def/Child", PrimSpec::def()),
            ("/Over", PrimSpec::over()),
            ("/Over/Child", PrimSpec::def()),
            ("/Class", PrimSpec::class()),
            ("/Class/Child", PrimSpec::def()),
            (
                "/Off",
                PrimSpec {
                    active: Some(false),
                    ..PrimSpec::def()
                },
            ),
        ] {
            let id = store.path(path);
            layer.insert_prim(id, spec);
        }
        store.insert_layer(layer);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let root = store.path("/");
        let roots: Vec<_> = stage.traverse_all(root).collect();
        fn expected(
            stage: &Stage,
            store: &dyn LayerStore,
            prim: PathId,
            predicate: PrimPredicate,
            output: &mut Vec<PathId>,
        ) {
            let status = stage.prim_status(prim, store).unwrap();
            if predicate.matches(status) {
                output.push(prim);
                if !status.instance || predicate.instance_proxies {
                    for &child in stage.all_children_of(prim).unwrap_or_default() {
                        expected(stage, store, child, predicate, output);
                    }
                }
            }
        }
        for root in roots {
            for mut code in 0..243 {
                let mut values = [None; 5];
                for value in &mut values {
                    *value = [None, Some(false), Some(true)][code % 3];
                    code /= 3;
                }
                for instance_proxies in [false, true] {
                    let predicate = PrimPredicate {
                        active: values[0],
                        loaded: values[1],
                        defined: values[2],
                        abstract_: values[3],
                        instance: values[4],
                        instance_proxies,
                    };
                    let mut wanted = Vec::new();
                    expected(&stage, &store, root, predicate, &mut wanted);
                    assert_eq!(
                        stage
                            .prim_range(root, &store, predicate)
                            .collect::<Vec<_>>(),
                        wanted,
                        "{root:?} {predicate:?}"
                    );
                    let visits: Vec<_> = stage
                        .prim_range(root, &store, predicate)
                        .pre_and_post()
                        .collect();
                    assert_eq!(
                        visits
                            .iter()
                            .filter(|v| !v.is_post_visit)
                            .map(|v| v.prim)
                            .collect::<Vec<_>>(),
                        wanted
                    );
                    assert_eq!(visits.len(), wanted.len() * 2);
                }
            }
        }
    }
}
