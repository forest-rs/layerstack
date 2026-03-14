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

    // Also check this prim's own variant branch-level references.
    // When a variant branch header has `(add references = ...)`, those references
    // apply to the prim owning the variant set when selected.
    let selections = resolve_variant_selections_for_prim(store, local_stack, prim);
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        for (set_tok, selected_variant) in &selections {
            if let Some(set_spec) = spec.variant_sets.get(set_tok)
                && let Some(variant_spec) = set_spec.variants.get(selected_variant)
            {
                let vr = &variant_spec.references;
                if vr.explicit.is_some() || !vr.prepend.is_empty() || !vr.append.is_empty() {
                    ops.push(vr.clone());
                }
            }
        }
    }

    // Also check parent's variant specs for child references.
    let leaf = store.paths().resolve(prim).leaf();
    let parent = store.paths().resolve(prim).parent();
    if let (Some(leaf), Some(parent)) = (leaf, parent) {
        if let Some(parent_id) = store.paths().lookup(&parent) {
            let parent_selections =
                resolve_variant_selections_for_prim(store, local_stack, parent_id);
            for layer_id in &local_stack.layers {
                let Some(layer) = store.layer(*layer_id) else {
                    continue;
                };
                let Some(parent_spec) = layer.prims.get(&parent_id) else {
                    continue;
                };
                for (set_tok, selected_variant) in &parent_selections {
                    if let Some(set_spec) = parent_spec.variant_sets.get(set_tok)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                        && let Some(child_refs) = variant_spec.child_references.get(&leaf)
                    {
                        ops.push(child_refs.clone());
                    }
                }
            }
        }
    }

    resolve_list_chain::<Reference>(&[], ops)
}

/// Resolves variant-scoped child references using a separate stack for variant
/// selection resolution. This is needed when composing within a reference arc:
/// the PrimSpec data lives in the remote stack, but variant selections should
/// come from the combined stack (which includes the referencing layer's
/// stronger selections).
pub(crate) fn resolve_variant_child_references(
    store: &dyn LayerStore,
    data_stack: &LayerStack,
    selections_stack: &LayerStack,
    prim: PathId,
) -> Vec<Reference> {
    let leaf = store.paths().resolve(prim).leaf();
    let parent = store.paths().resolve(prim).parent();
    let (Some(leaf), Some(parent)) = (leaf, parent) else {
        return Vec::new();
    };
    let Some(parent_id) = store.paths().lookup(&parent) else {
        return Vec::new();
    };

    let parent_selections =
        resolve_variant_selections_for_prim(store, selections_stack, parent_id);

    let mut ops = Vec::new();
    for layer_id in &data_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(parent_spec) = layer.prims.get(&parent_id) else {
            continue;
        };
        for (set_tok, selected_variant) in &parent_selections {
            if let Some(set_spec) = parent_spec.variant_sets.get(set_tok)
                && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                && let Some(child_refs) = variant_spec.child_references.get(&leaf)
            {
                ops.push(child_refs.clone());
            }
        }
    }

    resolve_list_chain::<Reference>(&[], ops)
}

/// Resolves variant branch-level references using a separate stack for variant
/// selection resolution. This is needed when composing within a reference arc:
/// the PrimSpec data lives in the remote stack, but variant selections should
/// come from the combined stack (including inherit-resolved selections).
pub(crate) fn resolve_variant_branch_references(
    store: &dyn LayerStore,
    data_stack: &LayerStack,
    selections_stack: &LayerStack,
    prim: PathId,
) -> Vec<Reference> {
    // Resolve variant selections with proper LIVERPS ordering:
    // per-layer direct selections, then inherit selections, before next layer.
    let inherits = resolve_inherits_for_prim(store, selections_stack, prim);
    let mut selections = HashMap::new();
    for layer_id in &selections_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        // Direct selections on the prim itself.
        if let Some(spec) = layer.prims.get(&prim) {
            for (set, variant) in &spec.variant_selections {
                selections.entry(*set).or_insert(*variant);
            }
        }
        // Inherit-sourced selections (weaker than direct, stronger than next layer).
        for inherit_target in &inherits {
            if let Some(inherit_spec) = layer.prims.get(inherit_target) {
                for (set, variant) in &inherit_spec.variant_selections {
                    selections.entry(*set).or_insert(*variant);
                }
            }
        }
    }

    let mut ops = Vec::new();
    for layer_id in &data_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        for (set_tok, selected_variant) in &selections {
            if let Some(set_spec) = spec.variant_sets.get(set_tok)
                && let Some(variant_spec) = set_spec.variants.get(selected_variant)
            {
                let vr = &variant_spec.references;
                if vr.explicit.is_some() || !vr.prepend.is_empty() || !vr.append.is_empty() {
                    ops.push(vr.clone());
                }
            }
        }
    }

    resolve_list_chain::<Reference>(&[], ops)
}

/// Collects ALL variant-scoped child references for a prim from all variant
/// branches of its parent, regardless of selection. Used during population
/// to ensure all potentially-referenced prims are discovered.
pub(crate) fn collect_all_variant_child_references(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
) -> Vec<Reference> {
    let leaf = store.paths().resolve(prim).leaf();
    let parent = store.paths().resolve(prim).parent();
    let (Some(leaf), Some(parent)) = (leaf, parent) else {
        return Vec::new();
    };
    let Some(parent_id) = store.paths().lookup(&parent) else {
        return Vec::new();
    };

    let mut all_refs = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(parent_spec) = layer.prims.get(&parent_id) else {
            continue;
        };
        for (_set_tok, set_spec) in &parent_spec.variant_sets {
            for (_variant_tok, variant_spec) in &set_spec.variants {
                if let Some(child_refs) = variant_spec.child_references.get(&leaf) {
                    let refs = resolve_list_chain::<Reference>(&[], [child_refs.clone()]);
                    all_refs.extend(refs);
                }
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
    local_stack: &LayerStack,
    prim: PathId,
) -> Vec<Reference> {
    let mut all_refs = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        for (_set_tok, set_spec) in &spec.variant_sets {
            for (_variant_tok, variant_spec) in &set_spec.variants {
                let vr = &variant_spec.references;
                if vr.explicit.is_some() || !vr.prepend.is_empty() || !vr.append.is_empty() {
                    let refs = resolve_list_chain::<Reference>(&[], [vr.clone()]);
                    all_refs.extend(refs);
                }
            }
        }
    }
    all_refs
}

/// Resolves the specializes arc list for a prim across the layer stack.
///
/// Spec: AOUSD Core §10 (specializes arc, §5.1.33).
pub(crate) fn resolve_specializes_for_prim(
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
        ops.push(spec.specializes.clone());
    }
    resolve_list_chain::<PathId>(&[], ops)
}

/// Resolves the payloads arc list for a prim across the layer stack.
///
/// Spec: AOUSD Core §10 (payloads arc, §5.1.22).
pub(crate) fn resolve_payloads_for_prim(
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
        ops.push(spec.payloads.clone());
    }
    resolve_list_chain::<Reference>(&[], ops)
}
