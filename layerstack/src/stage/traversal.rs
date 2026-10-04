// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Predicate traversal owns snapshot iteration, not population or loading.
//! AOUSD Core §11; OpenUSD `UsdPrimRange`, `UsdPrimDefaultPredicate`.
use super::*;

/// Composed prim flags used by traversal and inspection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    pub fn prim_status(&self, prim: PathId, store: &dyn LayerStore) -> Option<PrimStatus> {
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
    pub fn is_instance_proxy(&self, prim: PathId, paths: &crate::PathInterner) -> bool {
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
            let Some(status) = self.stage.prim_status(prim, self.store) else {
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
