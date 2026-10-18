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

use alloc::{sync::Arc, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::{
    doc::{
        FieldEntry, FieldValue, Layer, LayerId, LayerOffset, LayerStore, PrimSpec, Reference,
        Value, VariantSpec, get_field,
    },
    expression_variables::{ArcAnchor, ExpressionScope},
    interner::{TokenId, TokenInterner},
    listop::ListOp,
    path::{PathId, PathInterner},
    spec_path::SpecPath,
    sync::ReadLock,
};

use super::layer_stacks::ComputedStack;

/// The authored boolean field `name` among `fields`.
fn bool_field(fields: &[FieldEntry], tokens: &TokenInterner, name: &str) -> Option<bool> {
    match get_field(fields, &tokens.lookup(name)?)? {
        FieldValue::Value(Value::Bool(value)) => Some(*value),
        _ => None,
    }
}

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

    /// This spec's `active` opinion. A prim spec keeps it in a dedicated
    /// member; a variant spec, among its fields.
    ///
    /// Spec: AOUSD Core §7.6.7 (variant specs contribute prim spec fields),
    /// §11.3 (active).
    pub(crate) fn active(self, tokens: &TokenInterner) -> Option<bool> {
        match self {
            Self::Prim(spec) => spec.active,
            Self::Variant(spec) => bool_field(&spec.fields, tokens, "active"),
        }
    }

    /// This spec's `instanceable` opinion, kept as [`Self::active`] is.
    ///
    /// Spec: AOUSD Core §7.6.7, §11.3.3 (instanceable).
    pub(crate) fn instanceable(self, tokens: &TokenInterner) -> Option<bool> {
        match self {
            Self::Prim(spec) => spec.instanceable,
            Self::Variant(spec) => bool_field(&spec.fields, tokens, "instanceable"),
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

    pub(crate) fn variant_set_names(self) -> &'a ListOp<TokenId> {
        match self {
            Self::Prim(spec) => &spec.variant_set_names,
            Self::Variant(spec) => &spec.variant_set_names,
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
/// expression evaluated in `scope`. Payloads to the `dynamic` formats stay
/// as authored, for composition to resolve with their arguments.
/// OpenUSD: `_PcpComposeSiteReferencesOrPayloads`.
pub(crate) fn references(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
    payloads: bool,
    scope: Option<&ExpressionScope>,
    dynamic: Option<&crate::dynamic_format::DynamicFormats>,
) -> Vec<SiteArc> {
    let mut anchor = ArcAnchor::new(stack.identifier, scope);
    if payloads {
        anchor = anchor.payloads();
        if let Some(dynamic) = dynamic.filter(|formats| !formats.is_empty()) {
            anchor = anchor.dynamic(dynamic);
        }
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

/// The variant sets `site` lists, in evaluation order: the
/// `variantSetNames` list ops of its specs applied weakest layer first. A
/// variant set spec its list ops do not name is not one of them.
///
/// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §12.4 (list ops).
/// OpenUSD: `PcpComposeSiteVariantSets` in `pxr/usd/pcp/composeSite.cpp`.
pub(crate) fn variant_sets(
    store: &dyn LayerStore,
    stack: &ComputedStack,
    site: &SpecPath,
) -> Vec<TokenId> {
    let authored: Vec<&ListOp<TokenId>> = specs(store, stack, site)
        .map(|(_, _, spec)| spec.variant_set_names())
        .filter(|op| !op.is_empty())
        .collect();
    let mut names = Vec::new();
    for op in authored.into_iter().rev() {
        op.apply_in_place(&mut names);
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
    children: &ChildIndex,
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
    children: &ChildIndex,
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
                children.with_children(store, layer, site, |children| {
                    // A set, so a prim listing many children stays linear.
                    let listed: HashSet<TokenId> = authored.iter().copied().collect();
                    for &name in children {
                        if !listed.contains(&name) {
                            unlisted.push(name);
                        }
                    }
                });
            }
            _ => children.with_children(store, layer, site, |children| {
                for &name in children {
                    if names.insert(name) {
                        order.push(name);
                    }
                }
            }),
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
/// no children of their own (layers built through `Layer::insert_prim`),
/// shared by the threads composing a stage.
///
/// A live stage keeps one across compositions. A layer's entry stays
/// current while [`Layer::structural_generation`] does, so value edits
/// never re-index it. When only specs were added, the entry is patched from
/// the prim paths the layer recorded as changed
/// ([`Layer::changed_paths_since`]); a removed spec, a replaced layer or
/// expired history indexes the layer anew.
#[derive(Default)]
pub(crate) struct ChildIndex {
    state: ReadLock<IndexState>,
}

#[derive(Default)]
struct IndexState {
    /// The token and path domains the entries' names come from.
    identity: Option<crate::StoreIdentity>,
    layers: HashMap<LayerId, IndexedLayer>,
    work: ChildIndexWork,
}

/// One layer's children, as of the generations it was indexed or patched
/// at.
struct IndexedLayer {
    generation: u64,
    structure: u64,
    children: Arc<LayerChildren>,
}

/// The layers a [`ChildIndex`] has indexed, counted since it was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChildIndexWork {
    /// Layers indexed from every prim spec they hold.
    pub(crate) rebuilds: usize,
    /// Layers whose entry was patched with the specs they added.
    pub(crate) patches: usize,
}

impl core::fmt::Debug for ChildIndex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.state.read(|state| {
            f.debug_struct("ChildIndex")
                .field("layers", &state.layers.len())
                .field("work", &state.work)
                .finish()
        })
    }
}

/// The children of a layer's parent sites, keyed by the parent's prim
/// names (rather than interned paths, since a spec's ancestors need not be
/// interned), then by the variant selections enclosing them, which most
/// parents have one set of. Each list is sorted by name.
type LayerChildren =
    HashMap<Vec<TokenId>, Vec<(Vec<crate::spec_path::VariantSelectionSite>, Vec<TokenId>)>>;

impl ChildIndex {
    /// Forgets every entry when `store` interns names in other domains
    /// than the entries were indexed with.
    pub(crate) fn prepare(&self, store: &dyn LayerStore) {
        let identity = store.identity();
        if self
            .state
            .read(|state| state.identity.as_ref() == Some(&identity))
        {
            return;
        }
        self.state.write(|state| {
            if state.identity.as_ref() != Some(&identity) {
                state.layers.clear();
                state.identity = Some(identity);
            }
        });
    }

    /// Forgets every entry, keeping the work counted.
    pub(crate) fn clear(&self) {
        self.state.write(|state| state.layers.clear());
    }

    /// The layers indexed and patched so far.
    pub(crate) fn work(&self) -> ChildIndexWork {
        self.state.read(|state| state.work)
    }

    /// Calls `f` with the names of the child specs `layer` authors beneath
    /// `site`.
    fn with_children<R>(
        &self,
        store: &dyn LayerStore,
        layer: &Layer,
        site: &SpecPath,
        f: impl FnOnce(&[TokenId]) -> R,
    ) -> R {
        let entry = self.layer_children(store, layer);
        let segments = store.paths().resolve(site.prim_path()).segments();
        let names = entry
            .get(segments)
            .and_then(|branches| {
                branches
                    .iter()
                    .find(|(sites, _)| sites.iter().copied().eq(site.variant_selection_sites()))
            })
            .map_or(&[][..], |(_, names)| names.as_slice());
        f(names)
    }

    /// The children of `layer`'s parent sites, brought up to date.
    fn layer_children(&self, store: &dyn LayerStore, layer: &Layer) -> Arc<LayerChildren> {
        let (generation, structure) = (layer.generation(), layer.structural_generation());
        let seen = self.state.read(|state| {
            state.layers.get(&layer.id).map(|entry| {
                let current = entry.structure == structure;
                (entry.generation, current.then(|| entry.children.clone()))
            })
        });
        match seen {
            Some((seen, Some(children))) => {
                if seen != generation {
                    // Only values changed. Later patches read the layer's
                    // history from here, while it is still retained.
                    self.state.write(|state| {
                        if let Some(entry) = state.layers.get_mut(&layer.id)
                            && entry.structure == structure
                        {
                            entry.generation = generation;
                        }
                    });
                }
                return children;
            }
            Some((seen, None)) => {
                if let Some(changed) = layer.changed_paths_since(seen)
                    && let Some(children) = self.patch(store, layer, seen, &changed)
                {
                    return children;
                }
            }
            None => {}
        }
        // Indexed without the lock; threads indexing one layer at once each
        // keep their own, and the first to finish is kept.
        let children = Arc::new(index_layer(store, layer));
        self.state.write(|state| {
            if let Some(entry) = state.layers.get(&layer.id)
                && (entry.generation, entry.structure) == (generation, structure)
            {
                return entry.children.clone();
            }
            state.work.rebuilds += 1;
            state.layers.insert(
                layer.id,
                IndexedLayer {
                    generation,
                    structure,
                    children: children.clone(),
                },
            );
            children
        })
    }

    /// Adds the specs `layer` authors at the `changed` prim paths to its
    /// entry, indexed at generation `seen`, or `None` when a name the entry
    /// lists for them lost its spec, which only re-indexing drops.
    fn patch(
        &self,
        store: &dyn LayerStore,
        layer: &Layer,
        seen: u64,
        changed: &[PathId],
    ) -> Option<Arc<LayerChildren>> {
        let (generation, structure) = (layer.generation(), layer.structural_generation());
        self.state.write(|state| {
            let entry = state.layers.get_mut(&layer.id)?;
            if entry.structure == structure {
                // Another thread patched it first.
                return Some(entry.children.clone());
            }
            if entry.generation != seen || !listings_hold(store, layer, &entry.children, changed) {
                return None;
            }
            // Unshared between compositions, so patched in place.
            add_specs(store, layer, Arc::make_mut(&mut entry.children), changed);
            entry.generation = generation;
            entry.structure = structure;
            let children = entry.children.clone();
            state.work.patches += 1;
            Some(children)
        })
    }
}

/// Whether `names`, sorted by name, holds `name`, and where it belongs.
fn find_name(names: &[TokenId], name: TokenId, tokens: &TokenInterner) -> Result<usize, usize> {
    let key = tokens.resolve(name);
    names.binary_search_by(|probe| tokens.resolve(*probe).cmp(key))
}

/// Whether every listing `index` holds for a name at the `changed` prim
/// paths still has a spec of `layer` in its variant branches to list it.
/// A spec removed, or moved to other branches, leaves its name listed; so
/// does a name listed only as the ancestor of specs beneath it, which this
/// does not look for.
fn listings_hold(
    store: &dyn LayerStore,
    layer: &Layer,
    index: &LayerChildren,
    changed: &[PathId],
) -> bool {
    let (paths, tokens) = (store.paths(), store.tokens());
    changed.iter().all(|&path| {
        let Some((&name, parent)) = paths.resolve(path).segments().split_last() else {
            return true;
        };
        index
            .get(parent)
            .into_iter()
            .flatten()
            .all(|(sites, names)| {
                find_name(names, name, tokens).is_err()
                    || layer
                        .prim_specs(path)
                        .any(|spec| spec.outer_variant_sites == *sites)
            })
    })
}

/// Lists the specs `layer` authors at the `changed` prim paths in `index`,
/// as [`index_layer`] lists every spec.
fn add_specs(store: &dyn LayerStore, layer: &Layer, index: &mut LayerChildren, changed: &[PathId]) {
    let (paths, tokens) = (store.paths(), store.tokens());
    // Each list grown, with the length of its sorted prefix: names not in
    // the prefix are appended, then sorted in once all are known, so a
    // patch adding many names to one list stays linear.
    let mut grown: HashMap<(Vec<TokenId>, usize), usize> = HashMap::new();
    for &path in changed {
        let segments = paths.resolve(path).segments();
        if segments.is_empty() {
            continue;
        }
        for spec in layer.prim_specs(path) {
            let outer = &spec.outer_variant_sites;
            let first = if outer.is_empty() {
                0
            } else {
                segments.len() - 1
            };
            for depth in first..segments.len() {
                let parent = &segments[..depth];
                if !index.contains_key(parent) {
                    index.insert(parent.to_vec(), Vec::new());
                }
                let branches = index.get_mut(parent).expect("inserted above");
                let at = match branches.iter().position(|(sites, _)| sites == outer) {
                    Some(at) => at,
                    None => {
                        branches.push((outer.clone(), Vec::new()));
                        branches.len() - 1
                    }
                };
                let names = &mut branches[at].1;
                let sorted = *grown
                    .entry((parent.to_vec(), at))
                    .or_insert_with(|| names.len());
                if find_name(&names[..sorted], segments[depth], tokens).is_err() {
                    names.push(segments[depth]);
                }
            }
        }
    }
    for ((parent, at), sorted) in grown {
        let names = &mut index.get_mut(&parent[..]).expect("grown above")[at].1;
        if names.len() - sorted <= 16 {
            for name in names.split_off(sorted) {
                if let Err(slot) = find_name(names, name, tokens) {
                    names.insert(slot, name);
                }
            }
        } else {
            // A stable sort merges the appended run into the sorted one.
            names.sort_by(|a, b| tokens.resolve(*a).cmp(tokens.resolve(*b)));
            names.dedup();
        }
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
                branches[at].1.push(segments[depth]);
            }
        }
    }
    // Names repeat once per spec beneath them; deduplicating after sorting
    // keeps a parent with many children linear.
    let tokens = store.tokens();
    for (_, names) in index.values_mut().flatten() {
        names.sort_unstable();
        names.dedup();
        names.sort_by(|a, b| tokens.resolve(*a).cmp(tokens.resolve(*b)));
    }
    index
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use alloc::vec::Vec;

    use crate::{
        InMemoryStore, Layer, LayerId, ListOp, PrimSpec, PropertySpec, PropertyType, Stage,
        StageOptions, SublayerEntry, TokenId, Value, VariantSetSpec, VariantSpec,
        spec_path::VariantSelectionSite,
    };

    /// A site evaluates the variant sets its layers' `variantSetNames` list
    /// ops give, weakest first: a stronger `delete` removes a weaker
    /// listing, so that set's selection is not evaluated, and a set spec no
    /// list names contributes nothing.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §12.4 (list ops).
    /// OpenUSD: `PcpComposeSiteVariantSets`.
    #[test]
    fn sites_evaluate_the_sets_their_list_ops_compose() {
        let mut store = InMemoryStore::default();
        let [look, wet, size, big, finish, matte, shine, height, gloss] = [
            "look", "wet", "size", "big", "finish", "matte", "shine", "height", "gloss",
        ]
        .map(|name| store.tokens.intern(name));
        let rock = store.path("/Rock");
        let double = PropertyType::parse("double").unwrap();
        let branch = |name, value| VariantSpec {
            properties: PrimSpec::def()
                .with_property(
                    name,
                    PropertySpec::typed_attribute(double.clone())
                        .with_default(Value::Double(value)),
                )
                .properties,
            ..VariantSpec::default()
        };
        let set = |variant, spec| VariantSetSpec {
            variants: [(variant, spec)].into_iter().collect(),
        };
        let mut weak = PrimSpec::def();
        weak.variant_sets.insert(look, set(wet, branch(shine, 1.0)));
        weak.variant_sets
            .insert(size, set(big, branch(height, 2.0)));
        weak.variant_sets
            .insert(finish, set(matte, branch(gloss, 3.0)));
        weak.variant_set_names = ListOp::prepended(vec![look, size]);
        weak.variant_selections = [(look, wet), (size, big), (finish, matte)]
            .into_iter()
            .collect();
        let mut sub = Layer::new(LayerId(2));
        sub.insert_prim(rock, weak);
        store.insert_layer(sub);
        let mut strong = PrimSpec::over();
        strong.variant_set_names = ListOp::deleted(vec![look]);
        let mut root = Layer::new(LayerId(1));
        root.sublayers.push(SublayerEntry::new(LayerId(2)));
        root.insert_prim(rock, strong);
        store.insert_layer(root);

        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let value = |name| {
            stage
                .resolve_field_path(crate::PropertyPath::new(rock, name))
                .map(|resolved| resolved.value)
        };
        assert_eq!(value(shine), None, "deleted by the stronger layer");
        assert_eq!(value(height), Some(Value::Double(2.0)));
        assert_eq!(value(gloss), None, "never listed");
        assert_eq!(stage.variant_sets(rock, &store), [(size, Some(big))]);
    }

    /// `index`'s entry for layer 1 of `store`, brought up to date, with
    /// each parent's branches in a fixed order.
    fn kept(
        index: &super::ChildIndex,
        store: &InMemoryStore,
    ) -> alloc::collections::BTreeMap<Vec<TokenId>, Vec<Branch>> {
        let layer = store.layers.get(&LayerId(1)).unwrap();
        ordered(&index.layer_children(store, layer))
    }

    type Branch = (Vec<VariantSelectionSite>, Vec<TokenId>);

    fn ordered(
        children: &super::LayerChildren,
    ) -> alloc::collections::BTreeMap<Vec<TokenId>, Vec<Branch>> {
        children
            .iter()
            .map(|(parent, branches)| {
                let mut branches = branches.clone();
                branches.sort();
                (parent.clone(), branches)
            })
            .collect()
    }

    /// The entry layer 1 of `store` would be indexed with afresh.
    fn fresh(store: &InMemoryStore) -> alloc::collections::BTreeMap<Vec<TokenId>, Vec<Branch>> {
        ordered(&super::index_layer(
            store,
            store.layers.get(&LayerId(1)).unwrap(),
        ))
    }

    /// A kept child index patches the specs a layer adds, in and out of
    /// variant branches and without their ancestors, into the entry it
    /// indexed, and leaves the entry alone when only values change.
    #[test]
    fn kept_child_index_patches_added_specs() {
        let mut store = InMemoryStore::default();
        let [world, rock, plant, hill] =
            ["/World", "/World/Rock", "/World/Plant", "/Terrain/Hill"].map(|path| store.path(path));
        let [size, big] = ["size", "big"].map(|name| store.tokens.intern(name));
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(world, PrimSpec::def());
        layer.insert_prim(rock, PrimSpec::def());
        store.insert_layer(layer);
        let index = super::ChildIndex::default();
        index.prepare(&store);
        assert_eq!(kept(&index, &store), fresh(&store));
        assert_eq!((index.work().rebuilds, index.work().patches), (1, 0));

        let layer = store.layers.get_mut(&LayerId(1)).unwrap();
        layer.insert_prim(plant, PrimSpec::def());
        layer.insert_prim(hill, PrimSpec::def());
        let mut branch = PrimSpec::def();
        branch.outer_variant_sites = vec![VariantSelectionSite {
            host_path: world,
            set: size,
            variant: big,
        }];
        layer.insert_prim(rock, branch);
        // Enough names at once to sort them in rather than insert each.
        for i in 0..40 {
            let path = store.path(&alloc::format!("/World/Stone{i}"));
            store
                .layers
                .get_mut(&LayerId(1))
                .unwrap()
                .insert_prim(path, PrimSpec::def());
        }
        assert_eq!(kept(&index, &store), fresh(&store));
        assert_eq!((index.work().rebuilds, index.work().patches), (1, 1));

        let x = store.tokens.intern("x");
        let layer = store.layers.get_mut(&LayerId(1)).unwrap();
        let double = PropertyType::parse("double").unwrap();
        layer.set_property(
            crate::PropertyPath::new(rock, x),
            PropertySpec::typed_attribute(double).with_default(Value::Double(1.0)),
        );
        assert!(layer.generation() > layer.structural_generation());
        assert_eq!(kept(&index, &store), fresh(&store));
        assert_eq!(
            (index.work().rebuilds, index.work().patches),
            (1, 1),
            "a value edit re-indexes nothing"
        );
    }

    /// A removed spec, a replaced layer and another store's names each
    /// index the layer anew.
    #[test]
    fn kept_child_index_rebuilds_after_removals_and_replacements() {
        let mut store = InMemoryStore::default();
        let [world, rock, plant] =
            ["/World", "/World/Rock", "/World/Plant"].map(|path| store.path(path));
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(world, PrimSpec::def());
        layer.insert_prim(rock, PrimSpec::def());
        layer.insert_prim(plant, PrimSpec::def());
        store.insert_layer(layer.clone());
        let index = super::ChildIndex::default();
        index.prepare(&store);
        kept(&index, &store);

        store.layers.get_mut(&LayerId(1)).unwrap().remove_prim(rock);
        assert_eq!(kept(&index, &store), fresh(&store));
        assert_eq!((index.work().rebuilds, index.work().patches), (2, 0));

        // The replacement holds what the first layer held: only its
        // advanced generations tell it apart.
        store.insert_layer(layer);
        assert_eq!(kept(&index, &store), fresh(&store));
        assert_eq!((index.work().rebuilds, index.work().patches), (3, 0));

        let snapshot = store.snapshot();
        index.prepare(&snapshot);
        assert_eq!(kept(&index, &snapshot), fresh(&snapshot));
        assert_eq!((index.work().rebuilds, index.work().patches), (4, 0));
    }
}
