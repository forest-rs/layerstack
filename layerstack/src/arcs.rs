// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition arc helpers.
//!
//! This module provides small helpers for composing arc-specific data such as
//! variant selections and reference lists.
//!
//! Spec: AOUSD Core §10 (composition arcs), including variants (§10.5) and references.

use alloc::vec::Vec;

use hashbrown::HashMap;

use crate::{
    doc::{
        Layer, LayerStore, PrimSpec, Reference, ReferenceTarget, VariantSpec, default_prim_names,
    },
    interner::TokenId,
    layer_stack::LayerStack,
    listop::{ListOp, resolve_list_chain},
    path::{Path, PathId},
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
    stack: &LayerStack,
    prim: PathId,
    spec: &PrimSpec,
    scope: SelectionScope<'_>,
) -> bool {
    if spec.outer_variant_sites.is_empty() {
        return true;
    }
    let enclosing = match scope {
        SelectionScope::Discover => return true,
        SelectionScope::Stack => None,
        SelectionScope::Composed(enclosing) => Some(enclosing),
    };
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
    spec.outer_variant_sites.iter().all(|site| {
        let selected = match enclosing.and_then(|hosts| hosts.get(&site.host_path)) {
            Some(composed) => composed.get(&site.set).copied(),
            None => resolve_variant_selections_for_prim(store, stack, site.host_path)
                .get(&site.set)
                .copied(),
        };
        selected.is_none_or(|selected| selected == site.variant)
    })
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
fn push_branch_ops<T: Clone>(
    layer: &Layer,
    host: PathId,
    child: PathId,
    selections: &HashMap<TokenId, TokenId>,
    arcs: fn(&PrimSpec) -> &ListOp<T>,
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
            .map(|spec| arcs(spec).clone()),
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
fn finish_arc_list<T: Clone + Eq>(
    store: &dyn LayerStore,
    stack: &LayerStack,
    prim: PathId,
    mut ops: Vec<ListOp<T>>,
    scope: SelectionScope<'_>,
    own: fn(&VariantSpec) -> &ListOp<T>,
    branch: fn(&PrimSpec) -> &ListOp<T>,
) -> Vec<T> {
    if !matches!(scope, SelectionScope::Discover) {
        return resolve_list_chain::<T>(&[], ops);
    }
    let parent = parent_of(store, prim);
    for layer in stack.layers.iter().filter_map(|id| store.layer(*id)) {
        if let Some(spec) = layer.prims.get(&prim) {
            for set_spec in spec.variant_sets.values() {
                ops.extend(set_spec.variants.values().map(|v| own(v).clone()));
            }
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
                    .map(|spec| branch(spec).clone()),
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
    stack: &LayerStack,
    prim: PathId,
) -> (HashMap<TokenId, TokenId>, HashMap<TokenId, TokenId>) {
    let selections = resolve_variant_selections_for_prim(store, stack, prim);
    let parent_selections = store
        .paths()
        .resolve(prim)
        .parent()
        .and_then(|parent| store.paths().lookup(&parent))
        .map(|parent| resolve_variant_selections_for_prim(store, stack, parent))
        .unwrap_or_default();
    (selections, parent_selections)
}

pub(crate) fn resolve_inherits_for_prim(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
) -> Vec<PathId> {
    let (selections, parent_selections) = stack_selections(store, local_stack, prim);
    resolve_inherits_for_prim_in(
        store,
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
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        if !spec_arcs_apply(store, local_stack, prim, spec, scope) {
            continue;
        }
        ops.push(spec.inherits.clone());
    }

    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        for (set_tok, selected_variant) in selections {
            if let Some(set_spec) = spec.variant_sets.get(set_tok)
                && let Some(variant_spec) = set_spec.variants.get(selected_variant)
            {
                let vi = &variant_spec.inherits;
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
    )
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
        if !spec_arcs_apply(store, local_stack, prim, spec, SelectionScope::Stack) {
            continue;
        }
        for (set, variant) in &spec.variant_selections {
            selected.entry(*set).or_insert(*variant);
        }
    }
    selected
}

/// Resolves only the direct PrimSpec.references for a prim, without variant
/// branch-level or parent variant child references. Use this when variant
/// refs are resolved separately with proper selection stacks.
pub(crate) fn resolve_direct_references_for_prim(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
) -> Vec<Reference> {
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        if !spec_arcs_apply(store, local_stack, prim, spec, scope) {
            continue;
        }
        ops.push(spec.references.clone());
    }
    resolve_list_chain::<Reference>(&[], ops)
}

pub(crate) fn resolve_references_for_prim(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
) -> Vec<Reference> {
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        if !spec_arcs_apply(store, local_stack, prim, spec, scope) {
            continue;
        }
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

    // Also check the prim's specs inside its parent's selected branches.
    if let Some(parent_id) = parent_of(store, prim) {
        let parent_selections = resolve_variant_selections_for_prim(store, local_stack, parent_id);
        for layer in local_stack.layers.iter().filter_map(|id| store.layer(*id)) {
            push_branch_ops(
                layer,
                parent_id,
                prim,
                &parent_selections,
                |spec| &spec.references,
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
        |v| &v.references,
        |spec| &spec.references,
    )
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
    data_stack: &LayerStack,
    prim: PathId,
    selections: &HashMap<TokenId, TokenId>,
    parent_selections: &HashMap<TokenId, TokenId>,
) -> Vec<Reference> {
    let mut ops = Vec::new();
    if let Some(parent_id) = parent_of(store, prim) {
        let inherits =
            resolve_inherits_for_prim(store, data_stack, parent_id, SelectionScope::Stack);
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
                    &mut ops,
                );
            }
        }
    }
    for layer_id in &data_stack.layers {
        let Some(spec) = store
            .layer(*layer_id)
            .and_then(|layer| layer.prims.get(&prim))
        else {
            continue;
        };
        for (set_tok, selected_variant) in selections {
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

/// Resolves the payloads authored on the branch headers of `prim`'s own
/// selected variants (`"full" (payload = ...) {}`), for the given selections.
pub(crate) fn resolve_branch_payloads_in(
    store: &dyn LayerStore,
    data_stack: &LayerStack,
    prim: PathId,
    selections: &HashMap<TokenId, TokenId>,
) -> Vec<Reference> {
    let mut ops = Vec::new();
    for layer_id in &data_stack.layers {
        let Some(spec) = store
            .layer(*layer_id)
            .and_then(|layer| layer.prims.get(&prim))
        else {
            continue;
        };
        for (set_tok, selected_variant) in selections {
            if let Some(set_spec) = spec.variant_sets.get(set_tok)
                && let Some(variant_spec) = set_spec.variants.get(selected_variant)
            {
                let vp = &variant_spec.payloads;
                if vp.explicit.is_some() || !vp.prepend.is_empty() || !vp.append.is_empty() {
                    ops.push(vp.clone());
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
    data_stack: &LayerStack,
    selections_stack: &LayerStack,
    prim: PathId,
) -> Vec<Reference> {
    let Some(parent_id) = parent_of(store, prim) else {
        return Vec::new();
    };

    // Resolve parent selections with inherit-based chaining.
    let inherits =
        resolve_inherits_for_prim(store, selections_stack, parent_id, SelectionScope::Stack);
    let mut parent_selections = HashMap::new();
    for layer_id in &selections_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        if let Some(spec) = layer.prims.get(&parent_id) {
            for (set, variant) in &spec.variant_selections {
                parent_selections.entry(*set).or_insert(*variant);
            }
        }
        for inherit_target in &inherits {
            if let Some(inherit_spec) = layer.prims.get(inherit_target) {
                for (set, variant) in &inherit_spec.variant_selections {
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
                for (set, selected_variant) in &parent_selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            if !parent_selections.contains_key(inner_set) {
                                new_sels.entry(*inner_set).or_insert(*inner_variant);
                            }
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
                    [spec.references.clone()],
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

/// Resolves variant branch-level payloads using a separate stack for variant
/// selection resolution. Similar to `resolve_variant_branch_references` but
/// for payload arcs on variant branch headers.
pub(crate) fn resolve_variant_branch_payloads(
    store: &dyn LayerStore,
    data_stack: &LayerStack,
    selections_stack: &LayerStack,
    prim: PathId,
) -> Vec<Reference> {
    let inherits = resolve_inherits_for_prim(store, selections_stack, prim, SelectionScope::Stack);
    let mut selections = HashMap::new();
    for layer_id in &selections_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        if let Some(spec) = layer.prims.get(&prim) {
            for (set, variant) in &spec.variant_selections {
                selections.entry(*set).or_insert(*variant);
            }
        }
        for inherit_target in &inherits {
            if let Some(inherit_spec) = layer.prims.get(inherit_target) {
                for (set, variant) in &inherit_spec.variant_selections {
                    selections.entry(*set).or_insert(*variant);
                }
            }
        }
    }

    // Also chain through variant branch selections (from inherited variant sets too).
    let check_paths: Vec<PathId> = core::iter::once(prim)
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
                for (set, selected_variant) in &selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            if !selections.contains_key(inner_set) {
                                new_sels.entry(*inner_set).or_insert(*inner_variant);
                            }
                        }
                    }
                }
            }
        }
        if new_sels.is_empty() {
            break;
        }
        selections.extend(new_sels);
    }

    let mut ops = Vec::new();
    for &check_path in &check_paths {
        for layer_id in &data_stack.layers {
            let Some(layer) = store.layer(*layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&check_path) else {
                continue;
            };
            for (set_tok, selected_variant) in &selections {
                if let Some(set_spec) = spec.variant_sets.get(set_tok)
                    && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                {
                    let vp = &variant_spec.payloads;
                    if vp.explicit.is_some() || !vp.prepend.is_empty() || !vp.append.is_empty() {
                        ops.push(vp.clone());
                    }
                }
            }
        }
    }

    resolve_list_chain::<Reference>(&[], ops)
}

/// Collects ALL variant branch-level payloads for a prim from all variant
/// branches, regardless of selection. Used during population to ensure all
/// potentially-loaded prims are discovered.
pub(crate) fn collect_all_variant_branch_payloads(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
) -> Vec<Reference> {
    let mut all_payloads = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        for (_set_tok, set_spec) in &spec.variant_sets {
            for (_variant_tok, variant_spec) in &set_spec.variants {
                let vp = &variant_spec.payloads;
                if vp.explicit.is_some() || !vp.prepend.is_empty() || !vp.append.is_empty() {
                    let payloads = resolve_list_chain::<Reference>(&[], [vp.clone()]);
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
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
) -> Vec<PathId> {
    let (selections, parent_selections) = stack_selections(store, local_stack, prim);
    resolve_specializes_for_prim_in(
        store,
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
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        if !spec_arcs_apply(store, local_stack, prim, spec, scope) {
            continue;
        }
        ops.push(spec.specializes.clone());
    }

    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        for (set_tok, selected_variant) in selections {
            if let Some(set_spec) = spec.variant_sets.get(set_tok)
                && let Some(variant_spec) = set_spec.variants.get(selected_variant)
            {
                let vs = &variant_spec.specializes;
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
    )
}

/// Resolves the payloads arc list for a prim across the layer stack.
///
/// Spec: AOUSD Core §10 (payloads arc, §5.1.22).
pub(crate) fn resolve_payloads_for_prim(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
    scope: SelectionScope<'_>,
) -> Vec<Reference> {
    let (_, parent_selections) = stack_selections(store, local_stack, prim);
    resolve_payloads_for_prim_in(store, local_stack, prim, &parent_selections, scope)
}

/// Resolves the payloads of `prim` with explicit selections for its parent's
/// variant sets; see [`resolve_inherits_for_prim_in`].
pub(crate) fn resolve_payloads_for_prim_in(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
    parent_selections: &HashMap<TokenId, TokenId>,
    scope: SelectionScope<'_>,
) -> Vec<Reference> {
    let mut ops = Vec::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(spec) = layer.prims.get(&prim) else {
            continue;
        };
        if !spec_arcs_apply(store, local_stack, prim, spec, scope) {
            continue;
        }
        ops.push(spec.payloads.clone());
    }

    if let Some(parent_id) = parent_of(store, prim) {
        for layer in local_stack.layers.iter().filter_map(|id| store.layer(*id)) {
            push_branch_ops(
                layer,
                parent_id,
                prim,
                parent_selections,
                |spec| &spec.payloads,
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
    )
}
