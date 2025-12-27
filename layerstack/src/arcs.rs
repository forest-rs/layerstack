//! Composition arc helpers.
//!
//! This module provides small helpers for composing arc-specific data such as
//! variant selections and reference lists.
//!
//! Spec: AOUSD Core §10 (composition arcs), including variants (§10.5) and references.

use alloc::vec::Vec;

use hashbrown::HashMap;

use crate::{
    doc::{LayerStore, Reference},
    interner::TokenId,
    layer_stack::LayerStack,
    listop::resolve_list_chain,
    path::PathId,
};

pub(crate) fn resolve_inherits_for_prim(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
) -> Vec<PathId> {
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        ops.push(spec.inherits.clone());
    }
    resolve_list_chain::<PathId>(&[], ops)
}

pub(crate) fn resolve_variant_selections_for_prim(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
) -> HashMap<TokenId, TokenId> {
    let mut selected = HashMap::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        for (set, variant) in &spec.variant_selections {
            selected.entry(*set).or_insert(*variant);
        }
    }
    selected
}

pub(crate) fn resolve_references_for_prim(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
) -> Vec<Reference> {
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        ops.push(spec.references.clone());
    }
    resolve_list_chain::<Reference>(&[], ops)
}
