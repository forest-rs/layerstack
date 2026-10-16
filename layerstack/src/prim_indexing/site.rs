// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the layers of one layer stack author at one site.
//!
//! A site is a prim spec path, with the variant selections of the spec it
//! addresses: `/Lamp` is the prim spec outside any variant branch,
//! `/Lamp{shade=round}` the variant spec of that branch, and
//! `/Lamp{shade=round}Shade` the child prim spec the branch holds.
//!
//! Spec: AOUSD Core §7.3.6 (variant specs), §10 (arcs are composed per
//! site across a layer stack's layers), §12.4 (list ops). OpenUSD:
//! `pxr/usd/pcp/composeSite.cpp`.

use alloc::vec::Vec;

use hashbrown::{HashMap, HashSet};

use crate::{
    doc::{FieldEntry, Layer, LayerId, LayerOffset, LayerStore, PrimSpec, Reference, VariantSpec},
    expression_variables::{ArcAnchor, ExpressionScope},
    interner::TokenId,
    listop::ListOp,
    path::{PathId, PathInterner},
    spec_path::SpecPath,
};

use super::layer_stacks::ComputedStack;

/// The spec a layer authors at a site: a prim spec, or a variant spec of
/// one.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SpecRef<'a> {
    Prim(&'a PrimSpec),
    Variant(&'a VariantSpec),
}

impl<'a> SpecRef<'a> {
    pub(crate) fn fields(self) -> &'a [FieldEntry] {
        match self {
            Self::Prim(spec) => &spec.fields,
            Self::Variant(spec) => &spec.fields,
        }
    }

    pub(crate) fn properties(self) -> &'a [crate::property::PropertyEntry] {
        match self {
            Self::Prim(spec) => &spec.properties,
            Self::Variant(spec) => &spec.properties,
        }
    }

    pub(crate) fn references(self) -> &'a ListOp<Reference> {
        match self {
            Self::Prim(spec) => &spec.references,
            Self::Variant(spec) => &spec.references,
        }
    }

    pub(crate) fn payloads(self) -> &'a ListOp<Reference> {
        match self {
            Self::Prim(spec) => &spec.payloads,
            Self::Variant(spec) => &spec.payloads,
        }
    }

    pub(crate) fn inherits(self) -> &'a ListOp<PathId> {
        match self {
            Self::Prim(spec) => &spec.inherits,
            Self::Variant(spec) => &spec.inherits,
        }
    }

    pub(crate) fn specializes(self) -> &'a ListOp<PathId> {
        match self {
            Self::Prim(spec) => &spec.specializes,
            Self::Variant(spec) => &spec.specializes,
        }
    }

    pub(crate) fn variant_selections(self) -> &'a HashMap<TokenId, TokenId> {
        match self {
            Self::Prim(spec) => &spec.variant_selections,
            Self::Variant(spec) => &spec.variant_selections,
        }
    }

    pub(crate) fn variant_sets(self) -> &'a HashMap<TokenId, crate::doc::VariantSetSpec> {
        match self {
            Self::Prim(spec) => &spec.variant_sets,
            Self::Variant(spec) => &spec.variant_sets,
        }
    }

    pub(crate) fn variant_set_order(self) -> &'a [TokenId] {
        match self {
            Self::Prim(spec) => &spec.variant_set_order,
            Self::Variant(spec) => &spec.variant_set_order,
        }
    }

    pub(crate) fn deleted_variant_sets(self) -> &'a [TokenId] {
        match self {
            Self::Prim(spec) => &spec.deleted_variant_sets,
            Self::Variant(_) => &[],
        }
    }

    pub(crate) fn authored_children(self) -> &'a [TokenId] {
        match self {
            Self::Prim(spec) => &spec.authored_children,
            Self::Variant(spec) => &spec.authored_children,
        }
    }

    pub(crate) fn prim_order(self) -> Option<&'a [TokenId]> {
        match self {
            Self::Prim(spec) => spec.prim_order.as_deref(),
            Self::Variant(_) => None,
        }
    }

    /// Whether the spec authors an arc or a variant selection, the fields
    /// a newly added site can bring stronger variant selections through.
    pub(crate) fn may_introduce_selections(self) -> bool {
        !self.variant_selections().is_empty()
            || !self.references().is_empty()
            || !self.payloads().is_empty()
            || !self.inherits().is_empty()
            || !self.specializes().is_empty()
    }
}

/// How many of `site`'s selections enclose its prim (are hosted on an
/// ancestor of it), the rest being its own trailing selections.
fn enclosing_count(site: &SpecPath, prim_depth: usize) -> usize {
    site.selection_entries()
        .iter()
        .take_while(|(depth, _)| (*depth as usize) < prim_depth)
        .count()
}

/// The spec `layer` authors at `site`.
pub(crate) fn spec_at<'a>(
    layer: &'a Layer,
    site: &SpecPath,
    paths: &PathInterner,
) -> Option<SpecRef<'a>> {
    let prim = site.prim_path();
    let entries = site.selection_entries();
    if entries.is_empty() {
        let spec = layer.prims.get(&prim)?;
        return spec
            .outer_variant_sites
            .is_empty()
            .then_some(SpecRef::Prim(spec));
    }
    let depth = paths.resolve(prim).depth();
    let enclosing = enclosing_count(site, depth);
    let spec = if enclosing == 0 {
        layer
            .prims
            .get(&prim)
            .filter(|spec| spec.outer_variant_sites.is_empty())?
    } else {
        layer.prim_specs(prim).find(|spec| {
            spec.outer_variant_sites.len() == enclosing
                && spec
                    .outer_variant_sites
                    .iter()
                    .zip(&entries[..enclosing])
                    .all(|(a, (_, b))| a == b)
        })?
    };
    if enclosing == entries.len() {
        return Some(SpecRef::Prim(spec));
    }
    let mut sets = &spec.variant_sets;
    let mut branch = None;
    for (_, selection) in &entries[enclosing..] {
        let variant = sets.get(&selection.set)?.variants.get(&selection.variant)?;
        sets = &variant.variant_sets;
        branch = Some(variant);
    }
    branch.map(SpecRef::Variant)
}

/// The specs of the layers of `stack` at `site`, strongest first, each with
/// its layer's strength and id.
pub(crate) fn specs<'a>(
    store: &'a dyn LayerStore,
    stack: &'a ComputedStack,
    site: &'a SpecPath,
) -> impl Iterator<Item = (usize, LayerId, SpecRef<'a>)> + 'a {
    stack
        .layers
        .iter()
        .enumerate()
        .filter_map(move |(strength, id)| {
            let layer = store.layer(*id)?;
            Some((strength, *id, spec_at(layer, site, store.paths())?))
        })
}

/// Whether a layer of `stack` authors a spec at `site`.
/// OpenUSD: `PcpComposeSiteHasSpecs`.
pub(crate) fn has_specs(store: &dyn LayerStore, stack: &ComputedStack, site: &SpecPath) -> bool {
    specs(store, stack, site).next().is_some()
}

/// Whether a layer of `stack` authors value clips at `site`.
/// OpenUSD: `PcpComposeSiteHasValueClips`.
pub(crate) fn has_value_clips(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
) -> bool {
    let Some(clips) = store.tokens().lookup("clips") else {
        return false;
    };
    specs(store, stack, site).any(|(_, _, spec)| spec.fields().iter().any(|f| f.name == clips))
}

/// One reference or payload composed at a site.
#[derive(Clone, Debug)]
pub(crate) struct SiteArc {
    pub(crate) arc: Reference,
    /// The strongest layer that authors it.
    pub(crate) source_layer: LayerId,
    /// That layer's offset within the stack.
    pub(crate) source_offset: LayerOffset,
}

/// The references (or payloads, when `payloads`) composed across `stack`'s
/// layers at `site`, each internal arc anchored to the stack, each variable
/// expression evaluated in `scope`.
/// OpenUSD: `_PcpComposeSiteReferencesOrPayloads`.
pub(crate) fn references(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
    payloads: bool,
    scope: Option<&ExpressionScope>,
) -> Vec<SiteArc> {
    let mut anchor = ArcAnchor::new(stack.identifier, scope);
    if payloads {
        anchor = anchor.payloads();
    }
    let authored: Vec<(usize, LayerId, &ListOp<Reference>)> = specs(store, stack, site)
        .map(|(strength, id, spec)| {
            let op = if payloads {
                spec.payloads()
            } else {
                spec.references()
            };
            (strength, id, op)
        })
        .filter(|(_, _, op)| !op.is_empty())
        .collect();
    if authored.is_empty() {
        return Vec::new();
    }
    let mut result: Vec<Reference> = Vec::new();
    let mut sources: Vec<(Reference, usize)> = Vec::new();
    for (strength, id, op) in authored.into_iter().rev() {
        let op = crate::arcs::anchor_internal_arcs(store, op, id, anchor);
        for item in op
            .explicit
            .iter()
            .flatten()
            .chain(&op.add)
            .chain(&op.prepend)
            .chain(&op.append)
            .chain(&op.reorder)
        {
            match sources.iter_mut().find(|(arc, _)| arc == item) {
                Some(entry) => entry.1 = strength,
                None => sources.push((item.clone(), strength)),
            }
        }
        op.apply_in_place(&mut result);
    }
    result
        .into_iter()
        .map(|arc| {
            let strength = sources
                .iter()
                .find(|(item, _)| *item == arc)
                .map_or(0, |(_, strength)| *strength);
            SiteArc {
                source_layer: stack.layers[strength],
                source_offset: stack.offset_at(strength),
                arc,
            }
        })
        .collect()
}

/// The inherits (or specializes, when `specializes`) composed across
/// `stack`'s layers at `site`. OpenUSD: `PcpComposeSiteInherits`,
/// `PcpComposeSiteSpecializes`.
pub(crate) fn class_arcs(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
    specializes: bool,
) -> Vec<PathId> {
    let authored: Vec<&ListOp<PathId>> = specs(store, stack, site)
        .map(|(_, _, spec)| {
            if specializes {
                spec.specializes()
            } else {
                spec.inherits()
            }
        })
        .filter(|op| !op.is_empty())
        .collect();
    let mut result = Vec::new();
    for op in authored.into_iter().rev() {
        op.apply_in_place(&mut result);
    }
    result
}

/// The variant sets declared at `site`, in declaration order: each layer's
/// declared names, strongest layer first, without the names a stronger
/// layer deletes. OpenUSD: `PcpComposeSiteVariantSets`.
pub(crate) fn variant_sets(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
) -> Vec<TokenId> {
    let mut names: Vec<TokenId> = Vec::new();
    let mut deleted: Vec<TokenId> = Vec::new();
    for (_, _, spec) in specs(store, stack, site) {
        for name in spec.variant_set_order() {
            if !names.contains(name) && !deleted.contains(name) {
                names.push(*name);
            }
        }
        // Sets authored without a declaration (specs built in code) follow
        // the declared ones, by name.
        let mut undeclared: Vec<TokenId> = spec
            .variant_sets()
            .keys()
            .copied()
            .filter(|name| !names.contains(name) && !deleted.contains(name))
            .collect();
        let tokens = store.tokens();
        undeclared.sort_by(|a, b| tokens.resolve(*a).cmp(tokens.resolve(*b)));
        names.extend(undeclared);
        deleted.extend_from_slice(spec.deleted_variant_sets());
    }
    names
}

/// The variant names of the set `set` declared at `site` in any layer.
/// OpenUSD: `PcpComposeSiteVariantSetOptions`.
pub(crate) fn variant_set_options(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
    set: TokenId,
) -> HashSet<TokenId> {
    let mut options = HashSet::new();
    for (_, _, spec) in specs(store, stack, site) {
        if let Some(set_spec) = spec.variant_sets().get(&set) {
            options.extend(set_spec.variants.keys().copied());
        }
    }
    options
}

/// Whether a layer of `stack` authors a variant selection at `site`.
/// OpenUSD: `PcpComposeSiteHasVariantSelections`.
pub(crate) fn has_variant_selections(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
) -> bool {
    specs(store, stack, site).any(|(_, _, spec)| !spec.variant_selections().is_empty())
}

/// The variant selections for `set` authored at `site`, strongest first,
/// as authored (an expression is evaluated by the caller). OpenUSD:
/// `PcpComposeSiteVariantSelection`.
pub(crate) fn authored_selections(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
    set: TokenId,
) -> Vec<TokenId> {
    specs(store, stack, site)
        .filter_map(|(_, _, spec)| spec.variant_selections().get(&set).copied())
        .collect()
}

/// Composes the child prim names `stack`'s layers author at `site` over
/// `order`, weakest layer first: each layer's children not yet named, then
/// its `reorder nameChildren`. A spec that lists no children names the
/// child prim specs its layer holds, by name. OpenUSD:
/// `PcpComposeSiteChildNames`.
pub(crate) fn compose_child_names(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
    children: &mut ChildIndex,
    order: &mut Vec<TokenId>,
    names: &mut HashSet<TokenId>,
    unlisted: &mut Vec<TokenId>,
) {
    compose_child_names_in(store, &stack.layers, site, children, order, names, unlisted);
}

/// [`compose_child_names`] over `layers`, strongest first.
pub(crate) fn compose_child_names_in(
    store: &dyn LayerStore,
    layers: &[LayerId],
    site: &SpecPath,
    children: &mut ChildIndex,
    order: &mut Vec<TokenId>,
    names: &mut HashSet<TokenId>,
    unlisted: &mut Vec<TokenId>,
) {
    for &id in layers.iter().rev() {
        let Some(layer) = store.layer(id) else {
            continue;
        };
        let spec = spec_at(layer, site, store.paths());
        match spec.map(SpecRef::authored_children) {
            Some(authored) if !authored.is_empty() => {
                // A listed name with no child spec names no prim.
                for &name in authored {
                    if has_child_spec(store, layer, site, name) && names.insert(name) {
                        order.push(name);
                    }
                }
                // Child specs a list leaves out still exist; they follow
                // every named child.
                for &name in children.children(store, layer, site) {
                    if !authored.contains(&name) {
                        unlisted.push(name);
                    }
                }
            }
            _ => {
                for &name in children.children(store, layer, site) {
                    if names.insert(name) {
                        order.push(name);
                    }
                }
            }
        }
        if let Some(reorder) = spec.and_then(SpecRef::prim_order) {
            opinionated::apply_list_order_by_key(order, reorder, |name| *name);
        }
    }
}

/// Whether `layer` authors a prim spec named `name` beneath `site`, in the
/// variant branches `site` names.
fn has_child_spec(store: &dyn LayerStore, layer: &Layer, site: &SpecPath, name: TokenId) -> bool {
    let paths = store.paths();
    let Some(child) = paths.lookup_joined(site.prim_path(), &[name]) else {
        return false;
    };
    if !site.has_variant_selections() {
        return layer
            .prims
            .get(&child)
            .is_some_and(|spec| spec.outer_variant_sites.is_empty());
    }
    layer.prim_specs(child).any(|spec| {
        spec.outer_variant_sites
            .iter()
            .copied()
            .eq(site.variant_selection_sites())
    })
}

/// The child prim specs of each prim spec of a layer, for specs that list
/// no children of their own (layers built through `Layer::insert_prim`).
#[derive(Default)]
pub(crate) struct ChildIndex {
    /// Per layer, its generation and the children under each parent site.
    layers: HashMap<LayerId, (u64, LayerChildren)>,
}

/// The children of a layer's parent sites, keyed by the parent's prim
/// names (rather than interned paths, since a spec's ancestors need not be
/// interned), then by the variant selections enclosing them, which most
/// parents have one set of.
type LayerChildren =
    HashMap<Vec<TokenId>, Vec<(Vec<crate::spec_path::VariantSelectionSite>, Vec<TokenId>)>>;

impl ChildIndex {
    fn children(&mut self, store: &dyn LayerStore, layer: &Layer, site: &SpecPath) -> &[TokenId] {
        let generation = layer.generation();
        let entry = self
            .layers
            .entry(layer.id)
            .or_insert_with(|| (u64::MAX, HashMap::new()));
        if entry.0 != generation {
            entry.0 = generation;
            entry.1 = index_layer(store, layer);
        }
        let segments = store.paths().resolve(site.prim_path()).segments();
        entry
            .1
            .get(segments)
            .and_then(|branches| {
                branches
                    .iter()
                    .find(|(sites, _)| sites.iter().copied().eq(site.variant_selection_sites()))
            })
            .map_or(&[], |(_, names)| names.as_slice())
    }
}

/// Every prim spec of `layer` listed under its parent site, by name; the
/// ancestors of a spec outside variant branches are listed too, so specs
/// authored without their ancestors still form a hierarchy.
fn index_layer(store: &dyn LayerStore, layer: &Layer) -> LayerChildren {
    let paths = store.paths();
    let mut index: LayerChildren = HashMap::new();
    for spec_path in layer.prims.keys().chain(layer.variant_prims.keys()) {
        let segments = paths.resolve(*spec_path).segments();
        for spec in layer.prim_specs(*spec_path) {
            let outer = &spec.outer_variant_sites;
            let first = if outer.is_empty() {
                0
            } else {
                segments.len() - 1
            };
            for depth in first..segments.len() {
                let branches = index.entry(segments[..depth].to_vec()).or_default();
                let at = match branches.iter().position(|(sites, _)| sites == outer) {
                    Some(at) => at,
                    None => {
                        branches.push((outer.clone(), Vec::new()));
                        branches.len() - 1
                    }
                };
                let names = &mut branches[at].1;
                if !names.contains(&segments[depth]) {
                    names.push(segments[depth]);
                }
            }
        }
    }
    let tokens = store.tokens();
    for (_, names) in index.values_mut().flatten() {
        names.sort_by(|a, b| tokens.resolve(*a).cmp(tokens.resolve(*b)));
    }
    index
}
