// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Arc cycle detection.
//!
//! Composing a prim follows composition arcs recursively: an arc's target
//! site may author arcs of its own, and so on. Scene description can make
//! that recursion close on itself (`/A` references `/B`, which references
//! `/A`), or grow without bound (`/A/B` inherits `/A`, so `/A/B/B` inherits
//! `/A/B`, ...). [`ArcChain`] tracks the sites on the current chain of arcs so
//! an arc that would close a cycle can be rejected before it is followed.
//!
//! The rule is OpenUSD's (`_CheckForCycle` in `pxr/usd/pcp/primIndex.cpp`):
//! an arc to a site is a cycle when any site already on the chain, translated
//! to the namespace depth of the prim being composed, is in the same layer
//! stack and its path is a prefix of the target path or the target path is a
//! prefix of it. The prefix test catches ancestral cycles such as a prim
//! inheriting its own parent, whose recursion never revisits the same site.
//!
//! A target already on the chain is always rejected (its translated site has
//! the target as a prefix), and arc targets are authored scene description,
//! so a chain is no longer than the number of distinct arc targets: recursion
//! that follows only arcs admitted by the chain terminates.
//!
//! Spec: AOUSD Core §10.2.1 (the composition algorithm recurses through the
//! arcs of each target site) and §10.6 (an invalid arc is a composition error
//! that is skipped, and composition continues). The Core specification does
//! not define arc cycles itself; this follows OpenUSD's `PcpErrorArcCycle`.

use alloc::vec::Vec;

use hashbrown::{HashMap, HashSet};

use crate::{
    composition_error::{ArcCycle, ArcCycleSite, CompositionError},
    doc::{LayerId, LayerStore},
    layer_stack::LayerStack,
    path::{Path, PathId, PathInterner},
    prim_index::ArcKind,
};

/// One site on an [`ArcChain`].
#[derive(Clone, Copy, Debug)]
struct ChainSite {
    /// Root layer of the layer stack holding the site.
    layer_stack: LayerId,
    /// The site's prim path at the namespace depth of `dest`.
    site: PathId,
    /// The composed prim path the site contributes to.
    dest: PathId,
}

/// The chain of sites from a composed prim to the arc being followed.
///
/// A layer stack is identified by its root layer: [`LayerStack::gather`]
/// derives the whole stack from it.
///
/// [`LayerStack::gather`]: crate::LayerStack::gather
#[derive(Clone, Debug)]
pub(crate) struct ArcChain {
    sites: Vec<ChainSite>,
}

impl ArcChain {
    /// Starts a chain at the composed prim `prim` in the stage's layer stack
    /// (rooted at `layer_stack`).
    pub(crate) fn new(layer_stack: LayerId, prim: PathId) -> Self {
        Self {
            sites: alloc::vec![ChainSite {
                layer_stack,
                site: prim,
                dest: prim,
            }],
        }
    }

    /// Returns `true` when an arc from the current end of the chain, mapped
    /// onto the composed prim `dest`, to `target` in the layer stack rooted
    /// at `layer_stack` would close a cycle.
    ///
    /// `dest` must be at or under the `dest` of every site on the chain,
    /// which holds for arcs found while mapping an arc's target namespace.
    pub(crate) fn closes_cycle(
        &self,
        paths: &PathInterner,
        dest: PathId,
        layer_stack: LayerId,
        target: PathId,
    ) -> bool {
        reaches(&self.sites, paths, dest, target, |stack| {
            stack == layer_stack
        })
    }

    /// Appends the target of an arc that is being followed.
    ///
    /// `target` is the arc's target path in the layer stack rooted at
    /// `layer_stack`, and `dest` the composed prim it contributes to.
    pub(crate) fn push(&mut self, layer_stack: LayerId, target: PathId, dest: PathId) {
        self.sites.push(ChainSite {
            layer_stack,
            site: target,
            dest,
        });
    }

    /// Removes the site added by the matching [`push`](Self::push).
    pub(crate) fn pop(&mut self) {
        debug_assert!(
            self.sites.len() > 1,
            "the chain's root site is never popped"
        );
        self.sites.pop();
    }

    /// Returns the chain's sites, from the composed prim outwards, as
    /// `(layer stack, path)` pairs translated to the namespace depth of
    /// `dest`.
    fn sites_at(&self, paths: &mut PathInterner, dest: PathId) -> Vec<(LayerId, PathId)> {
        self.sites
            .iter()
            .map(|site| {
                let path = match translate(paths, site, dest) {
                    Some(path) => paths.intern(path),
                    None => site.site,
                };
                (site.layer_stack, path)
            })
            .collect()
    }
}

/// Detects arc cycles during composition and collects cycle errors.
///
/// Composition starts a chain for each composed prim with
/// [`begin`](Self::begin). Each arc from there is checked with
/// [`closes_cycle`](Self::closes_cycle), which records an [`ArcCycle`] for
/// an arc that must be skipped; an arc that is followed is bracketed with
/// [`enter`](Self::enter) and [`exit`](Self::exit). Errors are deduplicated
/// and kept in the order found.
#[derive(Debug)]
pub(crate) struct CycleDetector {
    stage_layer_stack: LayerId,
    chain: ArcChain,
    /// The arc that introduced each site on `chain` after its root.
    arcs: Vec<ArcKind>,
    /// The layers of each layer stack gathered so far, by root layer.
    stack_layers: HashMap<LayerId, HashSet<LayerId>>,
    errors: Vec<CompositionError>,
    seen: HashSet<CompositionError>,
}

impl CycleDetector {
    /// Creates a detector for a stage whose layer stack is rooted at
    /// `stage_layer_stack`.
    pub(crate) fn new(stage_layer_stack: LayerId) -> Self {
        Self {
            stage_layer_stack,
            chain: ArcChain { sites: Vec::new() },
            arcs: Vec::new(),
            stack_layers: HashMap::new(),
            errors: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Returns the root layer of the stage's layer stack.
    pub(crate) fn stage_layer_stack(&self) -> LayerId {
        self.stage_layer_stack
    }

    /// Starts the chain of arcs for the composed prim `prim`.
    pub(crate) fn begin(&mut self, prim: PathId) {
        self.chain = ArcChain::new(self.stage_layer_stack, prim);
        self.arcs.clear();
    }

    /// Returns `true`, and records an [`ArcCycle`], when an `arc` from the
    /// current end of the chain, mapped onto the composed prim `dest`, to
    /// `target` in the layer stack rooted at `layer_stack` would close a
    /// cycle. The caller must then skip the arc.
    pub(crate) fn closes_cycle(
        &mut self,
        paths: &mut PathInterner,
        dest: PathId,
        layer_stack: LayerId,
        target: PathId,
        arc: ArcKind,
    ) -> bool {
        if !self.chain.closes_cycle(paths, dest, layer_stack, target) {
            return false;
        }
        let arcs = core::iter::once(None).chain(self.arcs.iter().copied().map(Some));
        let mut sites: Vec<ArcCycleSite> = self
            .chain
            .sites_at(paths, dest)
            .into_iter()
            .zip(arcs)
            .map(|((layer_stack, path), arc)| ArcCycleSite {
                layer_stack,
                path,
                arc,
            })
            .collect();
        sites.push(ArcCycleSite {
            layer_stack,
            path: target,
            arc: Some(arc),
        });
        self.report(CompositionError::ArcCycle(ArcCycle { prim: dest, sites }));
        true
    }

    /// Follows an `arc` to `target` in the layer stack rooted at
    /// `layer_stack`, mapped onto the composed prim `dest`: the target joins
    /// the chain until the matching [`exit`](Self::exit). Check the arc with
    /// [`closes_cycle`](Self::closes_cycle) first.
    pub(crate) fn enter(
        &mut self,
        layer_stack: LayerId,
        target: PathId,
        dest: PathId,
        arc: ArcKind,
    ) {
        self.chain.push(layer_stack, target, dest);
        self.arcs.push(arc);
    }

    /// Leaves the arc followed by the matching [`enter`](Self::enter).
    pub(crate) fn exit(&mut self) {
        self.chain.pop();
        self.arcs.pop();
    }

    /// Returns `true` when an opinion found at `path` in `layer`, copied
    /// into the composed prim `dest` from the index of the site the current
    /// arc targets, would close a cycle: `path` is prefix-related to a site
    /// on the chain before that target, in a layer stack that contains
    /// `layer`.
    ///
    /// Composition copies opinions the target prim has already accumulated
    /// through its own arcs. Those arcs were checked against that prim's
    /// chain, not this one, so a copy can carry `dest` back into its own
    /// namespace (`/P2/C2/C1` copying `/P1/C1`'s inherit of `/P2`). The
    /// cycle itself was reported when this chain rejected the arc. The
    /// target's own site is excluded: its opinions are what the arc brings.
    pub(crate) fn copies_cycle(
        &self,
        paths: &PathInterner,
        dest: PathId,
        layer: LayerId,
        path: PathId,
    ) -> bool {
        let sites = &self.chain.sites;
        let before_target = &sites[..sites.len().saturating_sub(1)];
        reaches(before_target, paths, dest, path, |stack| {
            self.stack_layers
                .get(&stack)
                .is_some_and(|layers| layers.contains(&layer))
        })
    }

    /// Gathers the layer stack rooted at `root`, recording each sublayer
    /// cycle it ignores.
    pub(crate) fn gather_layer_stack(
        &mut self,
        store: &dyn LayerStore,
        root: LayerId,
    ) -> LayerStack {
        let mut sublayer_cycles = Vec::new();
        let stack = LayerStack::gather_reporting(store, root, &mut sublayer_cycles);
        for cycle in sublayer_cycles {
            self.report(CompositionError::SublayerCycle(cycle));
        }
        self.stack_layers
            .entry(root)
            .or_insert_with(|| stack.layers.iter().copied().collect());
        stack
    }

    /// Records `error` unless it was already recorded.
    fn report(&mut self, error: CompositionError) {
        if self.seen.insert(error.clone()) {
            self.errors.push(error);
        }
    }

    /// Returns the recorded errors in the order they were found.
    pub(crate) fn into_errors(self) -> Vec<CompositionError> {
        self.errors
    }
}

/// Translates `site` to the namespace depth of `dest`.
///
/// A site maps onto its `dest`; the namespace descendant of `dest` at
/// `dest/rel` is contributed by the site's own descendant at `site/rel`.
fn translate(paths: &PathInterner, site: &ChainSite, dest: PathId) -> Option<Path> {
    let rel = paths.resolve(dest).strip_prefix(paths.resolve(site.dest))?;
    Some(paths.resolve(site.site).join(rel))
}

/// Returns `true` when `path` is prefix-related to one of `sites`,
/// translated to the namespace depth of `dest`, whose layer stack (by root
/// layer) `in_stack` accepts.
fn reaches(
    sites: &[ChainSite],
    paths: &PathInterner,
    dest: PathId,
    path: PathId,
    in_stack: impl Fn(LayerId) -> bool,
) -> bool {
    let path = paths.resolve(path);
    sites
        .iter()
        .filter(|site| in_stack(site.layer_stack))
        .filter_map(|site| translate(paths, site, dest))
        .any(|site| site.is_prefix_of(path) || path.is_prefix_of(&site))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interner::TokenInterner;

    fn path(paths: &mut PathInterner, tokens: &mut TokenInterner, s: &str) -> PathId {
        paths.intern(Path::parse_absolute(s, tokens).expect("path"))
    }

    #[test]
    fn prefix_related_targets_close_cycles() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let stage = LayerId(1);
        let child = path(&mut paths, &mut tokens, "/Parent/Child");
        let parent = path(&mut paths, &mut tokens, "/Parent");
        let grandchild = path(&mut paths, &mut tokens, "/Parent/Child/Class");
        let sibling = path(&mut paths, &mut tokens, "/Parent/Sibling");

        let chain = ArcChain::new(stage, child);
        // Inheriting an ancestor or a descendant is a cycle; a sibling is not.
        assert!(chain.closes_cycle(&paths, child, stage, parent));
        assert!(chain.closes_cycle(&paths, child, stage, grandchild));
        assert!(chain.closes_cycle(&paths, child, stage, child));
        assert!(!chain.closes_cycle(&paths, child, stage, sibling));
        // The same path in another layer stack is a different site.
        assert!(!chain.closes_cycle(&paths, child, LayerId(2), parent));
    }

    #[test]
    fn sites_are_translated_to_the_destination_depth() {
        // `/CoRecursiveParent1/Child1` inherits `/CoRecursiveParent2`, whose
        // child `Child2` inherits `/CoRecursiveParent1`. Mapped onto
        // `/CoRecursiveParent1/Child1/Child2`, the root site becomes that
        // path, which `/CoRecursiveParent1` is a prefix of.
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let stage = LayerId(1);
        let p1_child1 = path(&mut paths, &mut tokens, "/CoRecursiveParent1/Child1");
        let p2 = path(&mut paths, &mut tokens, "/CoRecursiveParent2");
        let p1 = path(&mut paths, &mut tokens, "/CoRecursiveParent1");
        let dest = path(&mut paths, &mut tokens, "/CoRecursiveParent1/Child1/Child2");

        let mut chain = ArcChain::new(stage, p1_child1);
        assert!(!chain.closes_cycle(&paths, p1_child1, stage, p2));
        chain.push(stage, p2, p1_child1);
        assert!(chain.closes_cycle(&paths, dest, stage, p1));
        // Following `/CoRecursiveParent2` again from inside it is a cycle too.
        assert!(chain.closes_cycle(&paths, dest, stage, p2));
    }
}
