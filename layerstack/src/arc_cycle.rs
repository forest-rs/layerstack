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

use alloc::{rc::Rc, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::{
    composition_error::{ArcCycle, ArcCycleSite, CompositionError, OpinionAtRelocationSource},
    doc::{LayerId, LayerStore},
    expression_variables::{ExpressionScope, VariableReads, expression_error},
    interner::TokenId,
    layer_stack::LayerStack,
    path::{Path, PathId, PathInterner, TargetPath},
    prim_index::{ArcKind, PrimIndex},
    prim_index_graph::PrimNode,
    relocates::{LiftedSet, RelocationTable, Relocations, Walk},
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

    /// The composed prim path the path `path` of the chain's innermost layer
    /// stack maps to: through the innermost arc whose target contains it,
    /// every arc mapping the paths outside its target to themselves, as
    /// class paths map when implied into stronger layer stacks.
    ///
    /// Spec: AOUSD Core §10.4.2.4. OpenUSD: `_EvalImpliedClasses` maps a
    /// class across each arc with `PcpMapExpression::AddRootIdentity`.
    pub(crate) fn stage_path(&self, paths: &mut PathInterner, path: PathId) -> PathId {
        for site in self.sites[1..].iter().rev() {
            let rel = paths
                .resolve(path)
                .strip_prefix(paths.resolve(site.site))
                .map(<[_]>::to_vec);
            if let Some(rel) = rel {
                let joined = paths.resolve(site.dest).join(&rel);
                return paths.intern(joined);
            }
        }
        path
    }

    /// The root layers of the layer stacks whose expression variables
    /// apply to the chain's innermost site, outermost first: the chain's
    /// layer stacks up to the first site in the innermost site's layer
    /// stack.
    ///
    /// A class arc stays in its layer stack, and an implied class returns
    /// to a stronger one, whose variables are those it was first reached
    /// with.
    ///
    /// OpenUSD: `_EvalRefOrPayloadArcs` in `pxr/usd/pcp/primIndex.cpp`
    /// computes a referenced layer stack's variables over those of the
    /// layer stack that references it.
    pub(crate) fn expression_stacks(&self) -> Vec<LayerId> {
        match self.sites.last() {
            Some(last) => self.stacks_for(last.layer_stack),
            None => Vec::new(),
        }
    }

    /// The root layers of the layer stacks whose expression variables
    /// apply to the layer stack rooted at `root`, outermost first, ending
    /// with `root`: the chain's layer stacks up to the first site in it, or,
    /// for a layer stack not on the chain, which an arc from the innermost
    /// site is about to reach, all of them and then `root`.
    ///
    /// OpenUSD identifies a referenced layer stack with the source of the
    /// variables that override its own
    /// (`PcpLayerStackIdentifier::expressionVariablesOverrideSource`).
    pub(crate) fn stacks_for(&self, root: LayerId) -> Vec<LayerId> {
        let end = self
            .sites
            .iter()
            .position(|site| site.layer_stack == root)
            .map_or(self.sites.len(), |index| index + 1);
        let mut stacks: Vec<LayerId> = self.sites[..end]
            .iter()
            .map(|site| site.layer_stack)
            .collect();
        stacks.dedup();
        if stacks.last() != Some(&root) {
            stacks.push(root);
        }
        stacks
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

/// Detects arc cycles during composition and collects the composition
/// errors found (see [`report`](Self::report)).
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
    /// The relocation tables of the layer stacks reached so far, and the
    /// stage paths relocations prohibit.
    relocations: Relocations,
    errors: Vec<CompositionError>,
    seen: HashSet<CompositionError>,
    /// Target paths authored inside the class an inherit maps, as
    /// `(composed prim, property, mapped target)`: they target no instance
    /// of the class (see `composition_checks::drop_instance_targets`).
    class_internal_targets: HashSet<(PathId, TokenId, TargetPath)>,
    /// The expression variables read evaluating sublayer and arc asset
    /// paths.
    reads: VariableReads,
}

impl CycleDetector {
    /// Creates a detector for a stage whose layer stack is rooted at
    /// `stage_layer_stack`.
    pub(crate) fn new(stage_layer_stack: LayerId) -> Self {
        Self {
            stage_layer_stack,
            chain: ArcChain { sites: Vec::new() },
            arcs: Vec::new(),
            relocations: Relocations::default(),
            errors: Vec::new(),
            seen: HashSet::new(),
            class_internal_targets: HashSet::new(),
            reads: VariableReads::default(),
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
        // A prim relocated from beneath `dest` composes the ancestral
        // opinions of its source, and so `dest`'s arcs, again: it meets the
        // same cycle, reached through its relocate arc.
        //
        // Spec: AOUSD Core §10.3.2.6 ("the composition algorithm is
        // executed with the layer stack and the entry's source path").
        // OpenUSD: `_EvalNodeRelocations` adds the relocate node with
        // `includeAncestralOpinions`, whose recursive index reports the
        // cycle to the relocation target's prim index (`_CheckForCycle` in
        // `pxr/usd/pcp/primIndex.cpp`).
        let relocated: Vec<PathId> = self
            .relocations
            .stage()
            .iter()
            .filter(|relocate| {
                relocate.stage_source.is_some_and(|source| {
                    source != dest && paths.resolve(dest).is_prefix_of(paths.resolve(source))
                })
            })
            .filter_map(|relocate| relocate.stage_target)
            .collect();
        let errors: Vec<ArcCycle> = relocated
            .into_iter()
            .map(|relocation_target| {
                let mut through = alloc::vec![ArcCycleSite {
                    layer_stack: self.stage_layer_stack,
                    path: relocation_target,
                    arc: None,
                }];
                through.extend(sites.iter().enumerate().map(|(at, site)| ArcCycleSite {
                    arc: if at == 0 {
                        Some(ArcKind::Relocates)
                    } else {
                        site.arc
                    },
                    ..*site
                }));
                ArcCycle {
                    prim: relocation_target,
                    sites: through,
                }
            })
            .collect();
        self.report(CompositionError::ArcCycle(ArcCycle { prim: dest, sites }));
        for error in errors {
            self.report(CompositionError::ArcCycle(error));
        }
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

    /// The root layers of the layer stacks whose expression variables
    /// apply to the layer stack rooted at `root` as the chain reaches it
    /// ([`ArcChain::stacks_for`]).
    pub(crate) fn stacks_for(&self, root: LayerId) -> Vec<LayerId> {
        self.chain.stacks_for(root)
    }

    /// The scope the arcs authored in the layer stack at the end of the
    /// chain evaluate their asset path expressions in: that layer stack's
    /// variables, composed along the chain ([`ArcChain::expression_stacks`]).
    ///
    /// Hand it back with [`absorb`](Self::absorb) once the arcs are read.
    ///
    /// OpenUSD composes a referenced layer stack's variables over those of
    /// the layer stack referencing it (`_EvalRefOrPayloadArcs` in
    /// `pxr/usd/pcp/primIndex.cpp`, `PcpExpressionVariables::Compute`).
    pub(crate) fn expression_scope(&self) -> ExpressionScope {
        ExpressionScope::new(self.chain.expression_stacks())
    }

    /// Records what evaluating arc asset paths in `scope`, composing
    /// `prim`, found: the variables read, and a
    /// [`CompositionError::VariableExpressionError`] for each expression
    /// that failed.
    pub(crate) fn absorb(&mut self, scope: ExpressionScope, prim: PathId) {
        let findings = scope.into_findings();
        self.reads.extend(findings.reads);
        for (context, layer, expression, error) in findings.errors {
            self.report(expression_error(
                context,
                layer,
                Some(prim),
                &expression,
                error,
            ));
        }
    }

    /// Adds expression variables composition read outside the chain of
    /// arcs, evaluating variant selections.
    pub(crate) fn add_variable_reads(&mut self, reads: VariableReads) {
        self.reads.extend(reads);
    }

    /// Takes the expression variables composition has read so far.
    pub(crate) fn take_variable_reads(&mut self) -> VariableReads {
        core::mem::take(&mut self.reads)
    }

    /// Gathers the layer stack rooted at `root`, as reached along the chain
    /// ([`ArcChain::stacks_for`]), recording each sublayer it ignores (a
    /// cycle or an unresolved asset path).
    ///
    /// Its sublayer asset path expressions evaluate with the variables
    /// composed along that chain (see [`LayerStack::gather_recording`]).
    pub(crate) fn gather_layer_stack(
        &mut self,
        store: &dyn LayerStore,
        root: LayerId,
    ) -> LayerStack {
        let mut errors = Vec::new();
        let chain = self.chain.stacks_for(root);
        let stack = LayerStack::gather_recording(store, &chain, &mut errors, Some(&mut self.reads));
        for error in errors {
            self.report(error);
        }
        stack
    }

    /// Starts composing with the relocations of the stage's layer stack
    /// (see [`Relocations::new`]).
    pub(crate) fn set_relocations(&mut self, relocations: Relocations) {
        self.relocations = relocations;
    }

    /// The relocation state of the composition.
    pub(crate) fn relocations(&self) -> &Relocations {
        &self.relocations
    }

    /// The relocation state of the composition, for population to extend.
    pub(crate) fn relocations_mut(&mut self) -> &mut Relocations {
        &mut self.relocations
    }

    /// The relocations of the layer stack rooted at `layer_stack`, lifted
    /// through an arc that maps its `target_root` onto the stage path
    /// `dest_root` (see [`LiftedSet::lift`]); `None` when that layer stack
    /// relocates nothing the arc reaches. Lifted sources are prohibited
    /// stage paths from then on.
    pub(crate) fn lift_relocations(
        &mut self,
        store: &mut dyn LayerStore,
        layer_stack: LayerId,
        target_root: PathId,
        dest_root: PathId,
        outer: &Walk<'_>,
    ) -> Option<Rc<LiftedSet>> {
        let table = self.relocation_table(store, layer_stack);
        if table.is_empty() {
            return None;
        }
        let lifted = LiftedSet::lift(store, &table, layer_stack, target_root, dest_root, outer);
        if lifted.is_empty() {
            return None;
        }
        self.relocations.prohibit(&lifted);
        for &error in lifted.blocked() {
            self.report(CompositionError::ArcToProhibitedChild(error));
        }
        self.report_source_opinions(store, &lifted);
        Some(Rc::new(lifted))
    }

    /// Records an [`OpinionAtRelocationSource`] for each layer of the
    /// relocating layer stack with a prim spec at the source of a
    /// relocation of `lifted`, on the prim at its lifted target.
    ///
    /// Spec: AOUSD Core §10.3.2.6. OpenUSD reports these when it adds the
    /// relocate node (`_EvalNodeRelocations` in
    /// `pxr/usd/pcp/primIndex.cpp`), for the source itself only.
    pub(crate) fn report_source_opinions(&mut self, store: &dyn LayerStore, lifted: &LiftedSet) {
        for relocate in lifted.iter() {
            let Some(prim) = relocate.stage_target else {
                continue;
            };
            self.report_opinions_at_source(store, prim, relocate.layer_stack, relocate.source);
        }
    }

    /// Records an [`OpinionAtRelocationSource`] on each composed prim of
    /// `prims` whose own arcs reach a relocate node, for each layer of the
    /// relocating layer stack with a prim spec at the relocation source at
    /// or above the node's site.
    ///
    /// OpenUSD computes the prim index an arc targets from scratch, with
    /// the indexes of the target's namespace ancestors, and adds a relocate
    /// node, reporting the opinions at its source, wherever one of those
    /// sites is a relocation target: a prim referencing a relocated prim,
    /// or a descendant of one, reports them too. The relocate nodes a prim
    /// takes over from its namespace parent report nothing again.
    ///
    /// Spec: AOUSD Core §10.3.2.6. OpenUSD: `_EvalNodeRelocations` and
    /// `_BuildInitialPrimIndexFromAncestor` in `pxr/usd/pcp/primIndex.cpp`.
    pub(crate) fn report_relocate_node_opinions(
        &mut self,
        store: &dyn LayerStore,
        prims: &HashMap<PathId, PrimIndex>,
    ) {
        let mut paths: Vec<PathId> = prims.keys().copied().collect();
        paths.sort_unstable();
        for prim in paths {
            let graph = &prims[&prim].graph;
            let depth = store.paths().resolve(prim).depth();
            // Whether the arcs from the root down to `node` were added at
            // the prim's own depth, not taken over from its parent.
            let own = |node: &PrimNode| {
                let mut cursor = Some(node);
                while let Some(node) = cursor.filter(|node| node.parent().is_some()) {
                    if usize::from(node.namespace_depth()) == depth {
                        return true;
                    }
                    cursor = node.parent().and_then(|parent| graph.node(parent));
                }
                false
            };
            let sites: Vec<(LayerId, PathId)> = graph
                .nodes()
                .filter(|(_, node)| node.arc_kind() == ArcKind::Relocates && own(node))
                .map(|(_, node)| (node.layer_stack(), node.site().prim_path()))
                .collect();
            for (layer_stack, site) in sites {
                let table = self.relocation_table(store, layer_stack);
                if let Some(source) = table.source_at_or_above(store.paths(), site) {
                    self.report_opinions_at_source(store, prim, layer_stack, source);
                }
            }
        }
    }

    /// Records an [`OpinionAtRelocationSource`] on `prim` for each layer of
    /// the layer stack rooted at `layer_stack` with a prim spec at its
    /// relocation source `source`, outside variant branches.
    fn report_opinions_at_source(
        &mut self,
        store: &dyn LayerStore,
        prim: PathId,
        layer_stack: LayerId,
        source: PathId,
    ) {
        let stack = self.gather_layer_stack(store, layer_stack);
        for &layer in &stack.layers {
            // A spec at the source itself, not inside a variant branch.
            let authored = store.layer(layer).is_some_and(|layer| {
                layer
                    .prim_specs(source)
                    .any(|spec| spec.outer_variant_sites.is_empty())
            });
            if authored {
                self.report(CompositionError::OpinionAtRelocationSource(
                    OpinionAtRelocationSource {
                        prim,
                        layer,
                        path: source,
                    },
                ));
            }
        }
    }

    /// The relocation table of the layer stack rooted at `layer_stack`, as
    /// the chain reaches it ([`Self::gather_layer_stack`]), computed once
    /// per layer stack; its errors are recorded.
    pub(crate) fn relocation_table(
        &mut self,
        store: &dyn LayerStore,
        layer_stack: LayerId,
    ) -> Rc<RelocationTable> {
        let stack = self.gather_layer_stack(store, layer_stack);
        let table = self.relocations.table(store, &stack);
        for error in self.relocations.take_errors() {
            self.report(error);
        }
        table
    }

    /// Records `error` unless it was already recorded.
    ///
    /// Composition also reports its other arc errors here (see
    /// [`CompositionError::UnresolvedDefaultPrim`]), so every error of a
    /// stage is kept in one list, in the order found.
    pub(crate) fn report(&mut self, error: CompositionError) {
        if self.seen.insert(error.clone()) {
            self.errors.push(error);
        }
    }

    /// Records that the target path `target` of `prim`'s `property` was
    /// authored inside the class an inherit maps.
    pub(crate) fn note_class_internal_target(
        &mut self,
        prim: PathId,
        property: TokenId,
        target: TargetPath,
    ) {
        self.class_internal_targets.insert((prim, property, target));
    }

    /// Whether [`Self::note_class_internal_target`] recorded `target`.
    pub(crate) fn is_class_internal_target(
        &self,
        prim: PathId,
        property: TokenId,
        target: TargetPath,
    ) -> bool {
        self.class_internal_targets
            .contains(&(prim, property, target))
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
