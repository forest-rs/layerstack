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

use crate::{
    doc::{LayerId, LayerStore, PrimSpec, Reference},
    expression_variables::ArcAnchor,
    listop::ListOp,
};

/// Conservative absence proof for composition arcs and variant opinions.
/// Explicit empty edits also fall back to ordinary composition. AOUSD Core
/// §10 (arcs), §10.5 (variants): local specs alone cannot introduce other sites.
pub(crate) fn is_local_spec(spec: &PrimSpec) -> bool {
    spec.references.is_empty()
        && spec.payloads.is_empty()
        && spec.inherits.is_empty()
        && spec.specializes.is_empty()
        && spec.outer_variant_sites.is_empty()
        && spec.variant_sets.is_empty()
        && spec.variant_selections.is_empty()
        && spec.variant_set_names.is_empty()
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
/// A payload to a dynamic file format the anchor names
/// ([`ArcAnchor::dynamic`]) keeps its asset path, deferred: the layer it
/// targets depends on the arguments composition gives it
/// (`crate::dynamic_format`).
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
                } else if anchor.is_dynamic(reference) {
                    // Its layer depends on the arguments composition gives
                    // it: keep the asset path, deferred to composition.
                    Some(reference.clone())
                } else if reference.deferred {
                    Some(resolve_deferred(store, reference))
                } else if reference.is_expression() {
                    anchor.evaluate(store, reference, layer).map(|evaluated| {
                        if anchor.is_dynamic(&evaluated) {
                            Reference {
                                layer,
                                deferred: true,
                                ..evaluated
                            }
                        } else {
                            anchored_asset(evaluated)
                        }
                    })
                } else {
                    Some(anchored_asset(reference.clone()))
                }
            })
            .collect()
    };
    op.map_lists(anchored)
}

/// Returns the deferred arc `reference` ([`Reference::deferred`]) resolved
/// through `store`, anchored for list editing as an importer's resolved or
/// unresolved arc is ([`anchored_asset`]): the layer the host loaded for its
/// asset, or [`LayerId::UNRESOLVED`] for an asset the host confirmed
/// missing. An asset the host has not examined keeps the arc deferred, for
/// composition to ask for when it follows the arc.
///
/// Spec: AOUSD Core §9.4 (relative asset paths are anchored to the layer
/// that authors them), §10.3.2.1. OpenUSD resolves each arc's asset path
/// relative to its layer as it composes it (`_EvalRefOrPayloadArcs` in
/// `pxr/usd/pcp/primIndex.cpp`).
fn resolve_deferred(store: &dyn LayerStore, reference: &Reference) -> Reference {
    let asset = reference.asset.as_deref().unwrap_or_default();
    let layer = match store.asset_availability(reference.layer, asset) {
        crate::AssetAvailability::Loaded(layer) => layer,
        crate::AssetAvailability::Missing => LayerId::UNRESOLVED,
        crate::AssetAvailability::Unresolved => return reference.clone(),
    };
    anchored_asset(Reference {
        layer,
        deferred: false,
        ..reference.clone()
    })
}
