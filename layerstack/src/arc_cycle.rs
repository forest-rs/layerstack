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

use crate::{
    doc::LayerId,
    path::{Path, PathId, PathInterner},
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
        let target = paths.resolve(target);
        self.sites
            .iter()
            .filter(|site| site.layer_stack == layer_stack)
            .filter_map(|site| translate(paths, site, dest))
            .any(|site| site.is_prefix_of(target) || target.is_prefix_of(&site))
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
}

/// Translates `site` to the namespace depth of `dest`.
///
/// A site maps onto its `dest`; the namespace descendant of `dest` at
/// `dest/rel` is contributed by the site's own descendant at `site/rel`.
fn translate(paths: &PathInterner, site: &ChainSite, dest: PathId) -> Option<Path> {
    let rel = paths.resolve(dest).strip_prefix(paths.resolve(site.dest))?;
    Some(paths.resolve(site.site).join(rel))
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
