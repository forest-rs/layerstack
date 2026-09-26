// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition arc helpers.
//!
//! This module provides small helpers for composing arc-specific data such as
//! variant selections and reference lists.
//!
//! The reference and payload resolvers take an `anchor`: the root layer of
//! the layer stack containing the site whose arcs they resolve. Each internal
//! arc they return targets that layer stack (see [`anchor_internal_arcs`]).
//!
//! Spec: AOUSD Core §10 (composition arcs), including variants (§10.5) and references.

use alloc::vec::Vec;

use hashbrown::HashMap;

use crate::variant_fallbacks::{VariantFallbacks, apply_variant_fallbacks};
use crate::{
    doc::{
        Layer, LayerId, LayerStore, PrimSpec, Reference, ReferenceTarget, VariantSpec,
        default_prim_names,
    },
    expression_variables::{ArcAnchor, SiteContext, site_selections},
    interner::TokenId,
    layer_stack::LayerStack,
    listop::{ListOp, resolve_list_chain},
    path::{Path, PathId},
    spec_path::VariantSelectionSite,
};

/// Returns the prim path `reference` targets, like
/// [`Reference::target_path`], without interning: a `defaultPrim` target
/// whose path has never been interned has no prim spec in any layer and
/// yields `None`.
///
/// For read-only passes that only look for specs at the target. Passes that
/// follow the arc use [`Reference::target_path`], so the arc can be
/// reported when it does not resolve.
pub(crate) fn lookup_reference_target_path(
    store: &dyn LayerStore,
    reference: &Reference,
) -> Option<PathId> {
    if reference.is_unresolved() {
        return None;
    }
    match reference.target {
        ReferenceTarget::Prim(path) => Some(path),
        ReferenceTarget::DefaultPrim => {
            let default_prim = store.layer(reference.layer)?.default_prim?;
            let names = default_prim_names(store.tokens().resolve(default_prim))?;
            let segments = names
                .into_iter()
                .map(|name| store.tokens().lookup(name))
                .collect::<Option<Vec<_>>>()?;
            store.paths().lookup(&Path::root().join(&segments))
        }
    }
}

/// Returns `op`, authored in `layer`, unchanged: arcs whose targets do not
/// depend on the layer that authors them (inherits, specializes).
fn as_authored<T: Clone>(op: &ListOp<T>, _layer: LayerId) -> ListOp<T> {
    op.clone()
}

/// Returns `true` when `reference`, authored in `layer`, is internal: it
/// names no asset, and its [`Reference::layer`] is the authoring layer.
///
/// Ingestion records an internal arc (`</Prim>` or `<>`) that way.
fn is_internal(reference: &Reference, layer: LayerId) -> bool {
    reference.asset.is_none() && reference.layer == layer
}

/// Returns `reference`, an arc to another layer stack, with its asset path
/// replaced by its anchored identity, so that list editing compares
/// assets, not spellings. The prim path and layer offset are kept, and are
/// compared exactly.
///
/// - A resolved arc is identified by its [`Reference::layer`], the asset
///   its path anchors to; its asset path is cleared to `""`.
/// - An unresolved arc keeps its path, normalized
///   ([`crate::asset::normalize_asset_path`]).
///
/// OpenUSD compares the items after `SdfComputeAssetPathRelativeToLayer`
/// anchors them (`_PcpComposeSiteReferencesOrPayloads` in
/// `pxr/usd/pcp/composeSite.cpp`); `SdfReference` and `SdfPayload`
/// equality compare that asset path, the prim path and the layer offset
/// (and a reference's custom data). `ArDefaultResolver::_CreateIdentifier`
/// anchors a `./` or `../` path to the authoring layer, and a search path
/// (`granite.usda`) too when an asset exists there, otherwise keeping it
/// as written; either way it is `TfNormPath`-normalized. A resolved arc's
/// layer stands for its anchored path. The directory of the layer that
/// authors an unresolved arc is not known here, so the same unresolved
/// `./` path authored in two directories compares equal.
///
/// The normalization is the one flattening writes asset paths with
/// ([`crate::AssetResolver::anchor_asset_path`]).
fn anchored_asset(reference: Reference) -> Reference {
    let asset = if reference.is_unresolved() {
        crate::asset::normalize_asset_path(reference.asset.as_deref().unwrap_or_default())
    } else {
        alloc::string::String::new()
    };
    Reference {
        asset: Some(asset),
        ..reference
    }
}

/// Returns the references or payloads `op`, authored in `layer`, with each
/// internal arc anchored to `anchor`: the root layer of the layer stack that
/// contains the site the arcs are authored at.
///
/// An internal arc targets that whole layer stack, not the layer that
/// authors it, so a `</Prim>` authored in a sublayer reads every layer of the
/// stack, and a `<>` target names the stack root layer's `defaultPrim`.
/// Anchoring before list editing also makes an internal arc authored in one
/// layer equal to the same arc authored in another, as OpenUSD compares them
/// by asset and prim path.
///
/// An arc whose asset path is a variable expression is evaluated here too,
/// in `layer`'s context ([`ArcAnchor::evaluate`]), so list editing compares
/// the evaluated, anchored arcs: a `delete` removes a weaker layer's
/// expression arc when both evaluate to the same asset. An expression that
/// evaluates to nothing, or fails, drops the item.
///
/// Spec: AOUSD Core §10.3.2.1 (for a reference with no layer asset path,
/// "the layer stack containing the reference is assumed"), §10.3.2.2.
/// OpenUSD: `_EvalRefOrPayloadArcs` in `pxr/usd/pcp/primIndex.cpp` (an
/// empty asset path targets `node.GetLayerStack()` and its root layer);
/// `_PcpComposeSiteReferencesOrPayloads` in `pxr/usd/pcp/composeSite.cpp`
/// evaluates and anchors each item in its layer's list-op callback.
pub(crate) fn anchor_internal_arcs(
    store: &dyn LayerStore,
    op: &ListOp<Reference>,
    layer: LayerId,
    anchor: ArcAnchor<'_>,
) -> ListOp<Reference> {
    let anchored = |items: &[Reference]| -> Vec<Reference> {
        items
            .iter()
            .filter_map(|reference| {
                if is_internal(reference, layer) {
                    Some(Reference {
                        layer: anchor.layer,
                        ..reference.clone()
                    })
                } else if reference.is_expression() {
                    anchor.evaluate(store, reference, layer).map(anchored_asset)
                } else {
                    Some(anchored_asset(reference.clone()))
                }
            })
            .collect()
    };
    ListOp {
        explicit: op.explicit.as_deref().map(anchored),
        prepend: anchored(&op.prepend),
        append: anchored(&op.append),
        delete: anchored(&op.delete),
    }
}

/// Where arc resolution takes the variant selections that decide whether arcs
/// authored inside a variant branch apply (see [`spec_arcs_apply`]).
#[derive(Clone, Copy, Debug)]
pub(crate) enum SelectionScope<'a> {
    /// Admit every branch's arcs. Population uses this to discover all prims
    /// that might be reached; prims left without specs after composition are
    /// removed.
    Discover,
    /// Selections authored in the data layer stack itself.
    Stack,
    /// Composed selections for the enclosing variant hosts, keyed by host path
    /// in the data stack's namespace, falling back to the data stack for hosts
    /// not listed. Used inside arcs, where a stronger site (the referencing
    /// prim, or a layer referenced in between) may select differently from
    /// the target layer stack.
    Composed(&'a HashMap<PathId, HashMap<TokenId, TokenId>>),
}

/// Returns `true` when the composition arcs and variant selections authored
/// on `spec` (the spec stored at `prim` in a layer of `stack`) apply.
///
/// A prim authored inside a variant branch is stored under its namespace
/// path, tagged with [`PrimSpec::outer_variant_sites`]. The specs of a direct
/// child of the variant host (every site hosted on its parent) are resolved
/// by [`push_branch_ops`] against the parent selections the caller passes in,
/// so they are skipped here. Deeper descendants apply unless a different
/// branch is selected at one of their hosts, as decided by `scope`.
///
/// Spec: AOUSD Core §10.5 (only the selected variant contributes, including
/// the arcs authored inside it); OpenUSD adds arcs only beneath the selected
/// variant node (`pxr/usd/pcp/primIndex.cpp`, `_AddVariantArc`).
pub(crate) fn spec_arcs_apply(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stack: &LayerStack,
    prim: PathId,
    spec: &PrimSpec,
    scope: SelectionScope<'_>,
) -> bool {
    if spec.outer_variant_sites.is_empty() || matches!(scope, SelectionScope::Discover) {
        return true;
    }
    let parent = store
        .paths()
        .resolve(prim)
        .parent()
        .and_then(|parent| store.paths().lookup(&parent));
    if spec
        .outer_variant_sites
        .iter()
        .all(|site| Some(site.host_path) == parent)
    {
        return false;
    }
    spec_branches_selected(store, fallbacks, stack, spec, scope)
}

/// Returns `true` unless a branch enclosing `spec` (its
/// [`PrimSpec::outer_variant_sites`]) is not selected, as decided by `scope`.
/// A branch whose set has no known selection counts as selected.
///
/// Spec: AOUSD Core §10.3.2.5 (only the selected variant contributes).
fn spec_branches_selected(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stack: &LayerStack,
    spec: &PrimSpec,
    scope: SelectionScope<'_>,
) -> bool {
    let enclosing = match scope {
        SelectionScope::Discover => return true,
        SelectionScope::Stack => None,
        SelectionScope::Composed(enclosing) => Some(enclosing),
    };
    spec.outer_variant_sites.iter().all(|site| {
        let selected = match enclosing.and_then(|hosts| hosts.get(&site.host_path)) {
            Some(composed) => composed.get(&site.set).copied(),
            None => resolve_variant_selections_for_prim(store, fallbacks, stack, site.host_path)
                .get(&site.set)
                .copied(),
        };
        selected.is_none_or(|selected| selected == site.variant)
    })
}

/// Returns the specs of `prim` in `layer` whose own arcs and variant
/// selections apply (see [`spec_arcs_apply`]).
fn arc_specs<'a>(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stack: &LayerStack,
    layer: &'a Layer,
    prim: PathId,
    scope: SelectionScope<'_>,
) -> Vec<&'a PrimSpec> {
    layer
        .prim_specs(prim)
        .filter(|spec| spec_arcs_apply(store, fallbacks, stack, prim, spec, scope))
        .collect()
}

/// Returns the specs of `prim` in `layer` whose variant sets compose: every
/// spec whose enclosing branches are selected, including a child's spec in
/// its parent's selected branch (`/P{v=x}C`), which may host variant sets of
/// its own (`/P{v=x}C{w=y}`).
///
/// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set specs),
/// §10.3.2.5 (variants).
fn variant_host_specs<'a>(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stack: &LayerStack,
    layer: &'a Layer,
    prim: PathId,
    scope: SelectionScope<'_>,
) -> Vec<&'a PrimSpec> {
    layer
        .prim_specs(prim)
        .filter(|spec| spec_branches_selected(store, fallbacks, stack, spec, scope))
        .collect()
}

/// Finds where the arcs resolved for `prim` in `stack` are authored among its
/// variant branches.
///
/// Arc resolution chains every spec's list op into one list; this finds, for
/// one arc of that list, the strongest opinion that adds it, so the arc's
/// node can be placed beneath the variant node of the branch that authors
/// it.
///
/// Spec: AOUSD Core §10.3.2.5 (arcs authored inside a variant apply when it
/// is selected). OpenUSD adds them beneath the variant node
/// (`pxr/usd/pcp/primIndex.cpp`, `_AddVariantArc`, `_AddArc`).
pub(crate) struct ArcAuthoring<'a> {
    pub(crate) store: &'a dyn LayerStore,
    /// The stage's variant fallbacks.
    pub(crate) fallbacks: &'a VariantFallbacks,
    pub(crate) stack: &'a LayerStack,
    pub(crate) prim: PathId,
    /// The variant selections of `prim`'s own variant sets.
    pub(crate) selections: &'a HashMap<TokenId, TokenId>,
    /// Decides whether the branches enclosing a spec are selected.
    pub(crate) scope: SelectionScope<'a>,
}

impl ArcAuthoring<'_> {
    /// Returns the variant branches that author `item`, outermost first:
    /// empty for an arc authored on a spec outside every branch, the
    /// enclosing branches of a selected spec authored inside branches
    /// (`/P{v=x}C`), or the selected branch of one of the prim's own variant
    /// sets whose header authors it (`/P{v=x}`), after the branches enclosing
    /// that set. With them comes the index in the layer stack of the layer
    /// that authors it (`None` when no spec of `prim` adds it).
    ///
    /// Each list op is passed through `edit` with the layer that authors it,
    /// as arc resolution passes it (see [`as_authored`] and
    /// [`anchor_internal_arcs`]).
    fn sites_by<T: Clone + PartialEq>(
        &self,
        item: &T,
        spec_arcs: fn(&PrimSpec) -> &ListOp<T>,
        branch_arcs: fn(&VariantSpec) -> &ListOp<T>,
        edit: impl Fn(&ListOp<T>, LayerId) -> ListOp<T>,
    ) -> (Vec<VariantSelectionSite>, Option<usize>) {
        let Self {
            store,
            fallbacks,
            stack,
            prim,
            selections,
            scope,
        } = *self;
        let adds = |op: &ListOp<T>, layer: LayerId| {
            let op = edit(op, layer);
            op.explicit
                .as_ref()
                .is_some_and(|items| items.contains(item))
                || op.prepend.contains(item)
                || op.append.contains(item)
        };
        // Each layer with its index in `stack`, strongest first.
        let layers = || {
            stack
                .layers
                .iter()
                .enumerate()
                .filter_map(|(index, id)| Some((index, store.layer(*id)?)))
        };
        let outside = layers().find(|(_, layer)| {
            layer
                .prim_specs(prim)
                .any(|spec| spec.outer_variant_sites.is_empty() && adds(spec_arcs(spec), layer.id))
        });
        if let Some((index, _)) = outside {
            return (Vec::new(), Some(index));
        }
        for (index, layer) in layers() {
            for spec in layer.prim_specs(prim) {
                if !spec.outer_variant_sites.is_empty()
                    && adds(spec_arcs(spec), layer.id)
                    && spec_branches_selected(store, fallbacks, stack, spec, scope)
                {
                    return (spec.outer_variant_sites.clone(), Some(index));
                }
            }
        }
        for (index, layer) in layers() {
            for spec in variant_host_specs(store, fallbacks, stack, layer, prim, scope) {
                let mut branches: Vec<_> = spec.selected_variant_branches(selections).collect();
                branches.sort_unstable_by(|a, b| a.chain().cmp(b.chain()));
                if let Some(branch) = branches
                    .iter()
                    .find(|branch| adds(branch_arcs(branch.spec), layer.id))
                {
                    return (branch.sites(&spec.outer_variant_sites, prim), Some(index));
                }
            }
        }
        (Vec::new(), None)
    }

    /// The branches that author the inherits or specializes arc `item` (see
    /// [`Self::sites_by`]).
    ///
    /// A class arc carries no offset of its own, and none from the layer
    /// that authors it: its target is read in the same layer stack, whose
    /// layers keep their own sublayer offsets. OpenUSD maps a class arc with
    /// the identity offset (`_AddClassBasedArcs` in
    /// `pxr/usd/pcp/primIndex.cpp`).
    pub(crate) fn sites(
        &self,
        item: &PathId,
        spec_arcs: fn(&PrimSpec) -> &ListOp<PathId>,
        branch_arcs: fn(&VariantSpec) -> &ListOp<PathId>,
    ) -> Vec<VariantSelectionSite> {
        self.sites_by(item, spec_arcs, branch_arcs, as_authored).0
    }

    /// The reference or payload `item`, resolved with internal arcs anchored
    /// to `anchor`, with the offset of the layer that authors it applied,
    /// and the branches that author it (see [`Self::sites_by`]).
    ///
    /// An arc to another layer stack is read on the timeline of the layer
    /// that authors it: its offset composes beneath that layer's offset in
    /// `stack` (the strongest layer adding the arc). An internal arc keeps
    /// its own offset, as its target is read in `stack`, whose layers
    /// already carry their sublayer offsets.
    ///
    /// Spec: AOUSD Core §12.3.2.1 (layer offsets on sublayers, references
    /// and payloads), §10.3.1.1 (offsets compose along a chain of arcs),
    /// §10.3.2.1 (an arc with no asset path targets the containing layer
    /// stack). OpenUSD: `_EvalRefOrPayloadArcs` in
    /// `pxr/usd/pcp/primIndex.cpp` sets the arc's offset to
    /// `sourceLayerStackOffset * layerOffset` for a non-internal arc, where
    /// `sourceLayerStackOffset` is `PcpLayerStack::GetLayerOffsetForLayer`
    /// of the strongest layer adding the arc
    /// (`_PcpComposeSiteReferencesOrPayloads`, `pxr/usd/pcp/composeSite.cpp`).
    pub(crate) fn authored_reference(
        &self,
        item: Reference,
        spec_arcs: fn(&PrimSpec) -> &ListOp<Reference>,
        branch_arcs: fn(&VariantSpec) -> &ListOp<Reference>,
        anchor: ArcAnchor<'_>,
    ) -> (AuthoredReference, Vec<VariantSelectionSite>) {
        let (sites, index) = self.sites_by(&item, spec_arcs, branch_arcs, |op, layer| {
            anchor_internal_arcs(self.store, op, layer, anchor)
        });
        let internal = item.asset.is_none() && item.layer == anchor.layer;
        let reference = match index {
            Some(index) if !internal => Reference {
                layer_offset: self.stack.offset_at(index).compose(item.layer_offset),
                ..item
            },
            _ => item,
        };
        let layer = index.and_then(|index| self.stack.layers.get(index).copied());
        (AuthoredReference { reference, layer }, sites)
    }
}

/// A reference or payload resolved for a site, with the layer that
/// authors it (see [`ArcAuthoring::authored_reference`]).
///
/// Every prim the arc composes depends on that layer: its offset in the
/// layer stack retimes the arc's opinions.
#[derive(Clone, Debug)]
pub(crate) struct AuthoredReference {
    /// The arc, its offset composed beneath the authoring layer's.
    pub(crate) reference: Reference,
    /// The strongest layer of the site's layer stack that adds the arc;
    /// `None` when no spec of the site adds it.
    pub(crate) layer: Option<LayerId>,
}

/// Returns the parent of `prim`, if it has been interned.
fn parent_of(store: &dyn LayerStore, prim: PathId) -> Option<PathId> {
    store
        .paths()
        .resolve(prim)
        .parent()
        .and_then(|parent| store.paths().lookup(&parent))
}

/// Returns the path of `host`'s child named like `prim`, if it has been
/// interned (a path never interned has no spec).
fn child_of(store: &dyn LayerStore, host: PathId, prim: PathId) -> Option<PathId> {
    let leaf = store.paths().resolve(prim).leaf()?;
    store
        .paths()
        .lookup(&store.paths().resolve(host).join(&[leaf]))
}

/// Pushes the arcs (`arcs`) authored in `layer` for `child` inside the
/// branches of `host` selected by `selections`: those of the prim specs at
/// `/host{set=variant}child`, including those of variant sets nested in
/// other branches of `host` (`/host{a=x}{b=y}child`).
///
/// Every branch enclosing a spec must be selected, not only its innermost
/// one: outer branches may author the same inner branch name, and only the
/// one under the selected outer branch contributes (see
/// [`Layer::selected_branch_prim_specs`]). Specs that also lie in a branch
/// hosted on another prim are resolved by [`spec_arcs_apply`], which checks
/// every enclosing branch.
///
/// Spec: AOUSD Core §7.3.6 (variant specs contain prim specs), §10.3.2.5
/// (only the selected variant contributes). OpenUSD composes the prim specs
/// beneath the selected variant node only (`pxr/usd/pcp/primIndex.cpp`,
/// `_AddVariantArc`).
///
/// Each list op is passed through `edit` with the layer that authors it (see
/// [`as_authored`] and [`anchor_internal_arcs`]).
fn push_branch_ops<T: Clone>(
    layer: &Layer,
    host: PathId,
    child: PathId,
    selections: &HashMap<TokenId, TokenId>,
    arcs: fn(&PrimSpec) -> &ListOp<T>,
    edit: impl Fn(&ListOp<T>, LayerId) -> ListOp<T>,
    ops: &mut Vec<ListOp<T>>,
) {
    ops.extend(
        layer
            .selected_branch_prim_specs(child, host, selections)
            .filter(|spec| {
                spec.outer_variant_sites
                    .iter()
                    .all(|site| site.host_path == host)
            })
            .map(|spec| edit(arcs(spec), layer.id)),
    );
}

/// Finishes an arc list for `scope`.
///
/// For [`SelectionScope::Discover`], `ops` additionally receives the arcs of
/// every variant branch of `prim` (`own`) and of every prim spec of `prim`
/// inside a branch of its parent (`branch`), and each list op is resolved on
/// its own
/// and unioned: discovery must not let one branch's `explicit` list hide
/// another branch's targets. Otherwise the ops are chained strongest-first.
/// The ops read here pass through `edit`, as in [`push_branch_ops`].
fn finish_arc_list<T: Clone + Eq>(
    store: &dyn LayerStore,
    stack: &LayerStack,
    prim: PathId,
    mut ops: Vec<ListOp<T>>,
    scope: SelectionScope<'_>,
    own: fn(&VariantSpec) -> &ListOp<T>,
    branch: fn(&PrimSpec) -> &ListOp<T>,
    edit: impl Fn(&ListOp<T>, LayerId) -> ListOp<T>,
) -> Vec<T> {
    if !matches!(scope, SelectionScope::Discover) {
        return resolve_list_chain::<T>(&[], ops);
    }
    let parent = parent_of(store, prim);
    for layer in stack.layers.iter().filter_map(|id| store.layer(*id)) {
        for spec in layer.prim_specs(prim) {
            ops.extend(
                spec.variant_branches()
                    .map(|branch| edit(own(branch.spec), layer.id)),
            );
        }
        if let Some(parent) = parent {
            ops.extend(
                layer
                    .prim_specs(prim)
                    .filter(|spec| {
                        spec.outer_variant_sites
                            .last()
                            .is_some_and(|site| site.host_path == parent)
                    })
                    .map(|spec| edit(branch(spec), layer.id)),
            );
        }
    }
    let mut all = Vec::new();
    for op in ops {
        for item in resolve_list_chain::<T>(&[], [op]) {
            if !all.contains(&item) {
                all.push(item);
            }
        }
    }
    all
}

/// Returns the variant selections authored in `stack` for `prim` and for its
/// parent (empty when `prim` has no parent).
fn stack_selections(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stack: &LayerStack,
    prim: PathId,
) -> (HashMap<TokenId, TokenId>, HashMap<TokenId, TokenId>) {
    let selections = resolve_variant_selections_for_prim(store, fallbacks, stack, prim);
    let parent_selections = store
        .paths()
        .resolve(prim)
        .parent()
        .and_then(|parent| store.paths().lookup(&parent))
        .map(|parent| resolve_variant_selections_for_prim(store, fallbacks, stack, parent))
        .unwrap_or_default();
    (selections, parent_selections)
}

pub(crate) fn resolve_inherits_for_prim(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
) -> Vec<PathId> {
    let (selections, parent_selections) = stack_selections(store, fallbacks, local_stack, prim);
    resolve_inherits_for_prim_in(
        store,
        fallbacks,
        local_stack,
        prim,
        &selections,
        &parent_selections,
        scope,
    )
}

/// Resolves the inherits of `prim` from the specs in `local_stack`, using the
/// given variant selections for `prim`'s own variant sets and for its
/// parent's (which gate arcs authored for `prim` inside the parent's branches).
///
/// Callers composing inside another arc pass the selections seen from the
/// composed destination, so a referencing layer's selection is honoured.
pub(crate) fn resolve_inherits_for_prim_in(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    selections: &HashMap<TokenId, TokenId>,
    parent_selections: &HashMap<TokenId, TokenId>,
    scope: SelectionScope<'_>,
) -> Vec<PathId> {
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in arc_specs(store, fallbacks, local_stack, layer, prim, scope) {
            ops.push(spec.inherits.clone());
        }
    }

    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in variant_host_specs(store, fallbacks, local_stack, layer, prim, scope) {
            for branch in spec.selected_variant_branches(selections) {
                let vi = &branch.spec.inherits;
                if vi.explicit.is_some() || !vi.prepend.is_empty() || !vi.append.is_empty() {
                    ops.push(vi.clone());
                }
            }
        }
    }

    if let Some(parent_id) = parent_of(store, prim) {
        for layer in local_stack.layers.iter().filter_map(|id| store.layer(*id)) {
            push_branch_ops(
                layer,
                parent_id,
                prim,
                parent_selections,
                |spec| &spec.inherits,
                as_authored,
                &mut ops,
            );
        }
    }

    finish_arc_list(
        store,
        local_stack,
        prim,
        ops,
        scope,
        |v| &v.inherits,
        |spec| &spec.inherits,
        as_authored,
    )
}

/// Applies `fallbacks` to `selections`, found for the prims `paths` in
/// `stack`, for the variant sets of their specs there (see
/// [`apply_variant_fallbacks`]).
fn apply_site_fallbacks(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stack: &LayerStack,
    paths: &[PathId],
    selections: &mut HashMap<TokenId, TokenId>,
) {
    if fallbacks.is_empty() {
        return;
    }
    let specs: Vec<(&PrimSpec, SiteContext<'_>)> = stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| {
            let context = SiteContext::Chain(stack.chain_of(layer.id));
            paths
                .iter()
                .filter_map(|path| layer.prims.get(path))
                .map(move |spec| (spec, context))
        })
        .collect();
    apply_variant_fallbacks(store, fallbacks, selections, &specs);
}

/// Resolves the variant selections of `prim` in `local_stack`: the
/// selections authored for it ([`authored_variant_selections_for_prim`]),
/// completed with `fallbacks` for its variant sets there
/// ([`apply_variant_fallbacks`]).
///
/// Spec: AOUSD Core §10.3.2.5.1 (computing variant selection).
pub(crate) fn resolve_variant_selections_for_prim(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
) -> HashMap<TokenId, TokenId> {
    let mut selected = authored_variant_selections_for_prim(store, fallbacks, local_stack, prim);
    if !fallbacks.is_empty() {
        let specs: Vec<(&PrimSpec, SiteContext<'_>)> = local_stack
            .layers
            .iter()
            .filter_map(|id| store.layer(*id))
            .flat_map(|layer| {
                let context = SiteContext::Chain(local_stack.chain_of(layer.id));
                variant_host_specs(
                    store,
                    fallbacks,
                    local_stack,
                    layer,
                    prim,
                    SelectionScope::Stack,
                )
                .into_iter()
                .map(move |spec| (spec, context))
            })
            .collect();
        apply_variant_fallbacks(store, fallbacks, &mut selected, &specs);
    }
    selected
}

/// Resolves the variant selections authored for `prim` in `local_stack`:
/// on its specs outside any variant branch and on its specs inside selected
/// branches (`/P{v=x}C (variants = ...)`), stronger layers first.
///
/// Callers that go on to add weaker selections from other layer stacks use
/// this, and apply fallbacks once every authored selection is known.
/// `fallbacks` only decides which branches enclosing a spec are selected.
///
/// Spec: AOUSD Core §10.3.2.5.1 (computing variant selection).
pub(crate) fn authored_variant_selections_for_prim(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
) -> HashMap<TokenId, TokenId> {
    let mut selected = HashMap::new();
    for (spec, chain) in selection_host_specs(store, fallbacks, local_stack, prim) {
        let selections =
            site_selections(store, &spec.variant_selections, SiteContext::Chain(chain));
        for (set, variant) in selections.iter() {
            selected.entry(*set).or_insert(*variant);
        }
    }
    selected
}

/// A spec of a prim with the chain of layer stacks that reaches its layer,
/// the context its variant selection expressions evaluate in
/// ([`LayerStack::chain_of`]).
pub(crate) type HostSpec<'a, 's> = (&'a PrimSpec, &'s [LayerId]);

/// Returns the specs of `prim` in `local_stack`, stronger layers first,
/// whose variant selections and variant sets compose: its specs outside any
/// variant branch and its specs inside selected branches of its ancestors
/// (`/P{v=x}C`), each with the chain that reaches its layer.
///
/// Spec: AOUSD Core §7.3.6 (variant specs contain prim specs), §10.3.2.5.
pub(crate) fn selection_host_specs<'a, 's>(
    store: &'a dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &'s LayerStack,
    prim: PathId,
) -> Vec<HostSpec<'a, 's>> {
    local_stack
        .layers
        .iter()
        .zip(&local_stack.chains)
        .filter_map(|(id, chain)| Some((store.layer(*id)?, &**chain)))
        .flat_map(|(layer, chain)| {
            variant_host_specs(
                store,
                fallbacks,
                local_stack,
                layer,
                prim,
                SelectionScope::Stack,
            )
            .into_iter()
            .map(move |spec| (spec, chain))
        })
        .collect()
}

/// Resolves only the direct PrimSpec.references for a prim, without variant
/// branch-level or parent variant child references. Use this when variant
/// refs are resolved separately with proper selection stacks.
pub(crate) fn resolve_direct_references_for_prim(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in arc_specs(store, fallbacks, local_stack, layer, prim, scope) {
            ops.push(anchor_internal_arcs(
                store,
                &spec.references,
                *layer_id,
                anchor,
            ));
        }
    }
    resolve_list_chain::<Reference>(&[], ops)
}

/// Resolves the references arc list of `prim` across `local_stack`: its
/// specs', its selected branches' and its specs' in its parent's selected
/// branches, with the selections of its own variant sets resolved in
/// `local_stack` ([`resolve_variant_selections_for_prim`]).
///
/// Spec: AOUSD Core §10 (references arc), §10.3.2.5 (variants).
pub(crate) fn resolve_references_for_prim(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let selections = resolve_variant_selections_for_prim(store, fallbacks, local_stack, prim);
    resolve_references_for_prim_selected(
        store,
        fallbacks,
        local_stack,
        prim,
        scope,
        anchor,
        &selections,
    )
}

/// Resolves the references of `prim` as [`resolve_references_for_prim`]
/// does, with `selections` for its own variant sets: the selections
/// composed for the prim, which weaker arcs may author.
///
/// Spec: AOUSD Core §10.3.2.5 (arcs authored in a selected branch apply).
pub(crate) fn resolve_references_for_prim_selected(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
    anchor: ArcAnchor<'_>,
    selections: &HashMap<TokenId, TokenId>,
) -> Vec<Reference> {
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in arc_specs(store, fallbacks, local_stack, layer, prim, scope) {
            ops.push(anchor_internal_arcs(
                store,
                &spec.references,
                *layer_id,
                anchor,
            ));
        }
    }

    // Then those of this prim's own selected branches: a branch header's
    // `(references = ...)` applies to the prim hosting the variant set.
    let specs: Vec<(LayerId, &PrimSpec)> = local_stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| {
            variant_host_specs(store, fallbacks, local_stack, layer, prim, scope)
                .into_iter()
                .map(move |spec| (layer.id, spec))
        })
        .collect();
    let branches = selected_branch_arcs(
        &specs,
        selections,
        |branch| &branch.references,
        |op, layer| anchor_internal_arcs(store, op, layer, anchor),
    );
    // Discovery takes each list on its own, so the branches' list can join
    // them.
    let discover = matches!(scope, SelectionScope::Discover);
    if discover {
        ops.push(ListOp {
            explicit: Some(branches.clone()),
            ..ListOp::default()
        });
    }

    // Also check the prim's specs inside its parent's selected branches.
    if let Some(parent_id) = parent_of(store, prim) {
        let parent_selections =
            resolve_variant_selections_for_prim(store, fallbacks, local_stack, parent_id);
        for layer in local_stack.layers.iter().filter_map(|id| store.layer(*id)) {
            push_branch_ops(
                layer,
                parent_id,
                prim,
                &parent_selections,
                |spec| &spec.references,
                |op, layer| anchor_internal_arcs(store, op, layer, anchor),
                &mut ops,
            );
        }
    }

    let mut references = finish_arc_list(
        store,
        local_stack,
        prim,
        ops,
        scope,
        |v| &v.references,
        |spec| &spec.references,
        |op, layer| anchor_internal_arcs(store, op, layer, anchor),
    );
    if !discover {
        for reference in branches {
            if !references.contains(&reference) {
                references.push(reference);
            }
        }
    }
    references
}

/// Resolves the references authored for `prim` inside its parent's variant
/// branches (the prim specs at `/parent{set=variant}prim`) and on `prim`'s own variant
/// branch headers, for the given selections of the parent's and `prim`'s
/// variant sets. Specs come from `data_stack` (and, for child references,
/// also from the parent's inherit targets).
///
/// Callers composing inside another arc pass the selections seen from the
/// composed destination, so a referencing layer's selection decides which
/// branch's arcs are followed.
///
/// Spec: AOUSD Core §10.5 (arcs inside the selected variant only).
pub(crate) fn resolve_variant_references_in(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    data_stack: &LayerStack,
    prim: PathId,
    selections: &HashMap<TokenId, TokenId>,
    parent_selections: &HashMap<TokenId, TokenId>,
    scope: SelectionScope<'_>,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let mut ops = Vec::new();
    if let Some(parent_id) = parent_of(store, prim) {
        let inherits = resolve_inherits_for_prim(
            store,
            fallbacks,
            data_stack,
            parent_id,
            SelectionScope::Stack,
        );
        for check_path in core::iter::once(parent_id).chain(inherits) {
            let Some(child) = child_of(store, check_path, prim) else {
                continue;
            };
            for layer in data_stack.layers.iter().filter_map(|id| store.layer(*id)) {
                push_branch_ops(
                    layer,
                    check_path,
                    child,
                    parent_selections,
                    |spec| &spec.references,
                    |op, layer| anchor_internal_arcs(store, op, layer, anchor),
                    &mut ops,
                );
            }
        }
    }
    for layer_id in &data_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in variant_host_specs(store, fallbacks, data_stack, layer, prim, scope) {
            for branch in spec.selected_variant_branches(selections) {
                let vr = &branch.spec.references;
                if vr.explicit.is_some() || !vr.prepend.is_empty() || !vr.append.is_empty() {
                    ops.push(anchor_internal_arcs(store, vr, *layer_id, anchor));
                }
            }
        }
    }
    resolve_list_chain::<Reference>(&[], ops)
}

/// Resolves the payloads authored on the branch headers of `prim`'s own
/// selected variants (`"full" (payload = ...) {}`), for the given selections.
pub(crate) fn resolve_branch_payloads_in(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    data_stack: &LayerStack,
    prim: PathId,
    selections: &HashMap<TokenId, TokenId>,
    scope: SelectionScope<'_>,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let anchor = anchor.payloads();
    let mut ops = Vec::new();
    for layer_id in &data_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in variant_host_specs(store, fallbacks, data_stack, layer, prim, scope) {
            for branch in spec.selected_variant_branches(selections) {
                let vp = &branch.spec.payloads;
                if vp.explicit.is_some() || !vp.prepend.is_empty() || !vp.append.is_empty() {
                    ops.push(anchor_internal_arcs(store, vp, *layer_id, anchor));
                }
            }
        }
    }
    resolve_list_chain::<Reference>(&[], ops)
}

/// Resolves variant-scoped child references using a separate stack for variant
/// selection resolution. This is needed when composing within a reference arc:
/// the `PrimSpec` data lives in the remote stack, but variant selections should
/// come from the combined stack (which includes the referencing layer's
/// stronger selections).
///
/// Includes variant selection chaining through inherited variant sets.
pub(crate) fn resolve_variant_child_references(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    data_stack: &LayerStack,
    selections_stack: &LayerStack,
    prim: PathId,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let Some(parent_id) = parent_of(store, prim) else {
        return Vec::new();
    };

    // Resolve parent selections with inherit-based chaining.
    let inherits = resolve_inherits_for_prim(
        store,
        fallbacks,
        selections_stack,
        parent_id,
        SelectionScope::Stack,
    );
    let mut parent_selections = HashMap::new();
    for layer_id in &selections_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let context = SiteContext::Chain(selections_stack.chain_of(*layer_id));
        if let Some(spec) = layer.prims.get(&parent_id) {
            for (set, variant) in site_selections(store, &spec.variant_selections, context).iter() {
                parent_selections.entry(*set).or_insert(*variant);
            }
        }
        for inherit_target in &inherits {
            if let Some(inherit_spec) = layer.prims.get(inherit_target) {
                let authored = site_selections(store, &inherit_spec.variant_selections, context);
                for (set, variant) in authored.iter() {
                    parent_selections.entry(*set).or_insert(*variant);
                }
            }
        }
    }

    // Chain through variant branch selections (check inherited variant sets too).
    let check_paths: Vec<PathId> = core::iter::once(parent_id)
        .chain(inherits.iter().copied())
        .collect();
    loop {
        let mut new_sels = HashMap::new();
        for &check_path in &check_paths {
            for layer_id in &selections_stack.layers {
                let Some(layer) = store.layer(*layer_id) else {
                    continue;
                };
                let Some(spec) = layer.prims.get(&check_path) else {
                    continue;
                };
                let context = SiteContext::Chain(selections_stack.chain_of(*layer_id));
                for branch in spec.selected_variant_branches(&parent_selections) {
                    let inner = site_selections(store, &branch.spec.variant_selections, context);
                    for (inner_set, inner_variant) in inner.iter() {
                        if !parent_selections.contains_key(inner_set) {
                            new_sels.entry(*inner_set).or_insert(*inner_variant);
                        }
                    }
                }
            }
        }
        if new_sels.is_empty() {
            break;
        }
        parent_selections.extend(new_sels);
    }
    apply_site_fallbacks(
        store,
        fallbacks,
        selections_stack,
        &check_paths,
        &mut parent_selections,
    );

    let mut ops = Vec::new();
    // Check the branches of the parent and of its inherit targets.
    for &check_path in &check_paths {
        let Some(child) = child_of(store, check_path, prim) else {
            continue;
        };
        for layer in data_stack.layers.iter().filter_map(|id| store.layer(*id)) {
            push_branch_ops(
                layer,
                check_path,
                child,
                &parent_selections,
                |spec| &spec.references,
                |op, layer| anchor_internal_arcs(store, op, layer, anchor),
                &mut ops,
            );
        }
    }

    resolve_list_chain::<Reference>(&[], ops)
}

/// Collects ALL variant-scoped child references for a prim from all variant
/// branches of its parent, regardless of selection. Used during population
/// to ensure all potentially-referenced prims are discovered.
/// Prims reached only through unselected branches receive no specs during
/// arc expansion and are removed after composition
/// (`compose::remove_prims_without_specs`).
pub(crate) fn collect_all_variant_child_references(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let Some(parent_id) = parent_of(store, prim) else {
        return Vec::new();
    };

    let mut all_refs = Vec::new();
    for layer in local_stack.layers.iter().filter_map(|id| store.layer(*id)) {
        for spec in layer.prim_specs(prim) {
            if spec
                .outer_variant_sites
                .last()
                .is_some_and(|site| site.host_path == parent_id)
            {
                all_refs.extend(resolve_list_chain::<Reference>(
                    &[],
                    [anchor_internal_arcs(
                        store,
                        &spec.references,
                        layer.id,
                        anchor,
                    )],
                ));
            }
        }
    }
    all_refs
}

/// Collects ALL variant branch-level references for a prim from all variant
/// branches, regardless of selection. Used during population to ensure all
/// potentially-referenced prims are discovered.
pub(crate) fn collect_all_variant_branch_references(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let mut all_refs = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in variant_host_specs(
            store,
            fallbacks,
            local_stack,
            layer,
            prim,
            SelectionScope::Discover,
        ) {
            for branch in spec.variant_branches() {
                let vr = &branch.spec.references;
                if vr.explicit.is_some() || !vr.prepend.is_empty() || !vr.append.is_empty() {
                    let refs = resolve_list_chain::<Reference>(
                        &[],
                        [anchor_internal_arcs(store, vr, *layer_id, anchor)],
                    );
                    all_refs.extend(refs);
                }
            }
        }
    }
    all_refs
}

/// Resolves the payloads authored on the selected variant branches of
/// `prim` and of its inherit targets in `stack`, for `selections`, the
/// selections composed for the prim.
///
/// Spec: AOUSD Core §10.3.2.5 (arcs authored in a selected branch apply).
pub(crate) fn resolve_variant_branch_payloads(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stack: &LayerStack,
    prim: PathId,
    anchor: ArcAnchor<'_>,
    selections: &HashMap<TokenId, TokenId>,
) -> Vec<Reference> {
    let anchor = anchor.payloads();
    let inherits = resolve_inherits_for_prim(store, fallbacks, stack, prim, SelectionScope::Stack);
    let check_paths = core::iter::once(prim).chain(inherits.iter().copied());
    let mut payloads = Vec::new();
    for check_path in check_paths {
        let specs: Vec<(LayerId, &PrimSpec)> = stack
            .layers
            .iter()
            .filter_map(|id| store.layer(*id))
            .filter_map(|layer| Some((layer.id, layer.prims.get(&check_path)?)))
            .collect();
        for payload in selected_branch_arcs(
            &specs,
            selections,
            |branch| &branch.payloads,
            |op, layer| anchor_internal_arcs(store, op, layer, anchor),
        ) {
            if !payloads.contains(&payload) {
                payloads.push(payload);
            }
        }
    }
    payloads
}

/// The strength order of the variant nodes beneath one node of a prim
/// index, from the specs of that node's layer stack.
///
/// Each selected branch is a variant node of its own: the branches of the
/// sets declared at one spec (the prim spec, or a variant spec for the sets
/// nested in it) rank by node, in `variantSets` order across the layer
/// stack (the sets no layer names last, by token), and a set nested in a
/// branch ranks beneath it. Only the specs of one node rank by layer, so a
/// weaker layer's branch of an earlier set is stronger than a stronger
/// layer's branch of a later set.
///
/// Spec: AOUSD Core §10.4 (LIVERPS), §10.3.2.5 (variants). OpenUSD adds a
/// variant arc per set beneath the node declaring it, with the set's index
/// in that node's `variantSetNames` as its sibling number (`_AddVariantArc`
/// and `Task::PriorityOrder` in `pxr/usd/pcp/primIndex.cpp`), and ranks
/// sibling arcs of one type by that number (`PcpCompareSiblingNodeStrength`
/// in `pxr/usd/pcp/strengthOrdering.cpp`).
pub(crate) struct VariantNodeOrder<'s> {
    /// The node's prim specs, stronger layers first.
    specs: Vec<&'s PrimSpec>,
}

impl<'s> VariantNodeOrder<'s> {
    /// The order of the variant nodes of the node whose layer stack holds
    /// `specs`, stronger layers first.
    pub(crate) fn new(specs: impl IntoIterator<Item = &'s PrimSpec>) -> Self {
        Self {
            specs: specs.into_iter().collect(),
        }
    }

    /// The variant sets declared at `enclosing` (the branches of the prim
    /// enclosing them, outermost first), strongest first.
    fn sets_at(&self, enclosing: &[(TokenId, TokenId)]) -> Vec<TokenId> {
        let declared = || {
            self.specs
                .iter()
                .filter_map(|spec| spec.variant_sets_in(enclosing))
        };
        let mut sets: Vec<TokenId> = Vec::new();
        for (_, order) in declared() {
            for set in order {
                if !sets.contains(set) {
                    sets.push(*set);
                }
            }
        }
        let mut unordered: Vec<TokenId> = declared()
            .flat_map(|(sets, _)| sets.keys().copied())
            .filter(|set| !sets.contains(set))
            .collect();
        unordered.sort_unstable();
        unordered.dedup();
        sets.extend(unordered);
        sets
    }

    /// The rank of the variant node of the branch whose path on the prim
    /// is `chain` (outermost first): the rank of each set on the way, among
    /// the sets declared where it is. A branch ranks after the branches it
    /// is nested in and before the next set's.
    pub(crate) fn rank(&self, chain: &[(TokenId, TokenId)]) -> Vec<usize> {
        (0..chain.len())
            .map(|level| {
                let sets = self.sets_at(&chain[..level]);
                let set = chain[level].0;
                sets.iter().position(|s| *s == set).unwrap_or(sets.len())
            })
            .collect()
    }
}

/// The arcs `arcs` authored on the branches `selections` selects of the
/// specs `specs` of one prim, stronger layers first, each with the layer
/// authoring it.
///
/// Each branch is its own variant node, so each composes its own list
/// across the layer stack, and the lists follow in node order
/// ([`VariantNodeOrder`]): an explicit list in one branch does not replace
/// another branch's.
///
/// Spec: AOUSD Core §10.3.2.5 (arcs authored in a selected branch apply),
/// §12.4 (list ops). OpenUSD evaluates the arcs of each variant node on its
/// own (`_EvalNodeReferences`, `_EvalNodePayloads` in
/// `pxr/usd/pcp/primIndex.cpp`).
fn selected_branch_arcs<T: Clone + Eq>(
    specs: &[(LayerId, &PrimSpec)],
    selections: &HashMap<TokenId, TokenId>,
    arcs: fn(&VariantSpec) -> &ListOp<T>,
    edit: impl Fn(&ListOp<T>, LayerId) -> ListOp<T>,
) -> Vec<T> {
    let order = VariantNodeOrder::new(specs.iter().map(|(_, spec)| *spec));
    // Each variant node by its path on the prim.
    type Chain = Vec<(TokenId, TokenId)>;
    let mut nodes: Vec<(Chain, Vec<ListOp<T>>)> = Vec::new();
    for (layer, spec) in specs {
        for branch in spec.selected_variant_branches(selections) {
            let list = arcs(branch.spec);
            if list.explicit.is_none() && list.prepend.is_empty() && list.append.is_empty() {
                continue;
            }
            let chain: Chain = branch.chain().collect();
            let op = edit(list, *layer);
            match nodes.iter_mut().find(|(c, _)| *c == chain) {
                Some((_, ops)) => ops.push(op),
                None => nodes.push((chain, alloc::vec![op])),
            }
        }
    }
    nodes.sort_by_cached_key(|(chain, _)| order.rank(chain));
    let mut all: Vec<T> = Vec::new();
    for (_, ops) in nodes {
        for item in resolve_list_chain::<T>(&[], ops) {
            if !all.contains(&item) {
                all.push(item);
            }
        }
    }
    all
}

/// Collects ALL variant branch-level payloads for a prim from all variant
/// branches, regardless of selection. Used during population to ensure all
/// potentially-loaded prims are discovered.
pub(crate) fn collect_all_variant_branch_payloads(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let anchor = anchor.payloads();
    let mut all_payloads = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in variant_host_specs(
            store,
            fallbacks,
            local_stack,
            layer,
            prim,
            SelectionScope::Discover,
        ) {
            for branch in spec.variant_branches() {
                let vp = &branch.spec.payloads;
                if vp.explicit.is_some() || !vp.prepend.is_empty() || !vp.append.is_empty() {
                    let payloads = resolve_list_chain::<Reference>(
                        &[],
                        [anchor_internal_arcs(store, vp, *layer_id, anchor)],
                    );
                    all_payloads.extend(payloads);
                }
            }
        }
    }
    all_payloads
}

/// Resolves the specializes arc list for a prim across the layer stack.
///
/// Spec: AOUSD Core §10 (specializes arc, §5.1.33).
pub(crate) fn resolve_specializes_for_prim(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
) -> Vec<PathId> {
    let (selections, parent_selections) = stack_selections(store, fallbacks, local_stack, prim);
    resolve_specializes_for_prim_in(
        store,
        fallbacks,
        local_stack,
        prim,
        &selections,
        &parent_selections,
        scope,
    )
}

/// Resolves the specializes arcs of `prim` with explicit variant selections;
/// see [`resolve_inherits_for_prim_in`].
pub(crate) fn resolve_specializes_for_prim_in(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    selections: &HashMap<TokenId, TokenId>,
    parent_selections: &HashMap<TokenId, TokenId>,
    scope: SelectionScope<'_>,
) -> Vec<PathId> {
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in arc_specs(store, fallbacks, local_stack, layer, prim, scope) {
            ops.push(spec.specializes.clone());
        }
    }

    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in variant_host_specs(store, fallbacks, local_stack, layer, prim, scope) {
            for branch in spec.selected_variant_branches(selections) {
                let vs = &branch.spec.specializes;
                if vs.explicit.is_some() || !vs.prepend.is_empty() || !vs.append.is_empty() {
                    ops.push(vs.clone());
                }
            }
        }
    }

    if let Some(parent_id) = parent_of(store, prim) {
        for layer in local_stack.layers.iter().filter_map(|id| store.layer(*id)) {
            push_branch_ops(
                layer,
                parent_id,
                prim,
                parent_selections,
                |spec| &spec.specializes,
                as_authored,
                &mut ops,
            );
        }
    }

    finish_arc_list(
        store,
        local_stack,
        prim,
        ops,
        scope,
        |v| &v.specializes,
        |spec| &spec.specializes,
        as_authored,
    )
}

/// Resolves the payloads arc list for a prim across the layer stack.
///
/// Spec: AOUSD Core §10 (payloads arc, §5.1.22).
pub(crate) fn resolve_payloads_for_prim(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let anchor = anchor.payloads();
    let (_, parent_selections) = stack_selections(store, fallbacks, local_stack, prim);
    resolve_payloads_for_prim_in(
        store,
        fallbacks,
        local_stack,
        prim,
        &parent_selections,
        scope,
        anchor,
    )
}

/// Resolves the payloads of `prim` with explicit selections for its parent's
/// variant sets; see [`resolve_inherits_for_prim_in`].
pub(crate) fn resolve_payloads_for_prim_in(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
    parent_selections: &HashMap<TokenId, TokenId>,
    scope: SelectionScope<'_>,
    anchor: ArcAnchor<'_>,
) -> Vec<Reference> {
    let anchor = anchor.payloads();
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        for spec in arc_specs(store, fallbacks, local_stack, layer, prim, scope) {
            ops.push(anchor_internal_arcs(
                store,
                &spec.payloads,
                *layer_id,
                anchor,
            ));
        }
    }

    if let Some(parent_id) = parent_of(store, prim) {
        for layer in local_stack.layers.iter().filter_map(|id| store.layer(*id)) {
            push_branch_ops(
                layer,
                parent_id,
                prim,
                parent_selections,
                |spec| &spec.payloads,
                |op, layer| anchor_internal_arcs(store, op, layer, anchor),
                &mut ops,
            );
        }
    }

    finish_arc_list(
        store,
        local_stack,
        prim,
        ops,
        scope,
        |v| &v.payloads,
        |spec| &spec.payloads,
        |op, layer| anchor_internal_arcs(store, op, layer, anchor),
    )
}
