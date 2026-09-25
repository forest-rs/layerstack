// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition entry points.
//!
//! This module is responsible for producing composition results (`PrimIndex`es)
//! which are then wrapped by [`crate::stage::Stage`].
//!
//! Spec: AOUSD Core §9–§12 (layer stacks, arcs/strength ordering, population, and resolution).

use alloc::{borrow::Cow, collections::BTreeSet, vec::Vec};

use core::cmp::Ordering;

use hashbrown::{HashMap, HashSet};

use crate::{
    arc_cycle::CycleDetector,
    arcs::{
        ArcAuthoring, SelectionScope, anchor_internal_arcs, lookup_reference_target_path,
        resolve_branch_payloads_in, resolve_direct_references_for_prim, resolve_inherits_for_prim,
        resolve_inherits_for_prim_in, resolve_payloads_for_prim, resolve_payloads_for_prim_in,
        resolve_references_for_prim, resolve_specializes_for_prim, resolve_specializes_for_prim_in,
        resolve_variant_branch_payloads, resolve_variant_child_references,
        resolve_variant_references_in, resolve_variant_selections_for_prim, spec_arcs_apply,
    },
    composition_error::{CompositionError, UnresolvedAsset, UnresolvedDefaultPrim},
    dependency_map::{ArcDependency, DependencyBuilder},
    doc::{
        FieldValue, LayerId, LayerOffset, LayerStore, Reference, ReferenceTarget, composed_entries,
    },
    interner::TokenId,
    layer_stack::LayerStack,
    path::{PathId, PropertyPath, TargetPath},
    population::populate,
    prim_index::{ArcKind, Opinion, OpinionKey, OpinionValue, PrimIndex},
    prim_index_graph::{
        NodeArc, NodeId, NodeStrength, PrimIndexGraph, PrimNode, SpecializesOrigin,
    },
    property::PropertyType,
    spec_path::{SpecPath, VariantSelectionSite},
    stage::{Stage, StageOptions},
};

fn prim_spec_path(
    store: &dyn LayerStore,
    prim_path: PathId,
    outer_variant_sites: &[VariantSelectionSite],
) -> SpecPath {
    if outer_variant_sites.is_empty() {
        SpecPath::from_prim_path(prim_path, store.paths())
    } else {
        SpecPath::from_variant_selection_sites(prim_path, outer_variant_sites, store.paths())
    }
}

fn property_spec_path(
    store: &dyn LayerStore,
    prim_path: PathId,
    outer_variant_sites: &[VariantSelectionSite],
    property: TokenId,
) -> SpecPath {
    prim_spec_path(store, prim_path, outer_variant_sites).with_property(property)
}

fn variant_spec_path(
    store: &dyn LayerStore,
    prim_path: PathId,
    selection_sites: &[VariantSelectionSite],
) -> SpecPath {
    SpecPath::from_variant_selection_sites(prim_path, selection_sites, store.paths())
}

fn variant_property_spec_path(
    store: &dyn LayerStore,
    prim_path: PathId,
    selection_sites: &[VariantSelectionSite],
    property: TokenId,
) -> SpecPath {
    variant_spec_path(store, prim_path, selection_sites).with_property(property)
}

fn combined_variant_sites(
    outer: &[VariantSelectionSite],
    current: VariantSelectionSite,
) -> Vec<VariantSelectionSite> {
    let mut out = outer.to_vec();
    out.push(current);
    out
}

/// Maps a forwarded opinion's source spec path through an optional
/// provenance namespace remap.
///
/// Variant selections named in the path are kept as authored: they identify
/// the branch the opinion came from, and `prune_unselected_variant_specs`
/// relies on them to drop opinions from unselected branches. (Rewriting them
/// to the selected variant would relabel leaked opinions as legitimate ones.)
fn normalize_forwarded_spec_path(
    store: &mut dyn LayerStore,
    spec_path: &SpecPath,
    provenance_remap: Option<(PathId, PathId)>,
) -> SpecPath {
    if let Some((dest_root, src_root)) = provenance_remap {
        let dest_root = store.paths().resolve(dest_root).clone();
        let src_root = store.paths().resolve(src_root).clone();
        remap_spec_path(store, spec_path, &dest_root, &src_root)
    } else {
        spec_path.clone()
    }
}

fn normalized_prim_spec_path(
    store: &mut dyn LayerStore,
    prim_path: PathId,
    outer_variant_sites: &[VariantSelectionSite],
    provenance_remap: Option<(PathId, PathId)>,
) -> SpecPath {
    let raw = prim_spec_path(store, prim_path, outer_variant_sites);
    normalize_forwarded_spec_path(store, &raw, provenance_remap)
}

fn normalized_property_spec_path(
    store: &mut dyn LayerStore,
    prim_path: PathId,
    outer_variant_sites: &[VariantSelectionSite],
    property: TokenId,
    provenance_remap: Option<(PathId, PathId)>,
) -> SpecPath {
    let raw = property_spec_path(store, prim_path, outer_variant_sites, property);
    normalize_forwarded_spec_path(store, &raw, provenance_remap)
}

fn normalized_variant_spec_path(
    store: &mut dyn LayerStore,
    prim_path: PathId,
    selection_sites: &[VariantSelectionSite],
    provenance_remap: Option<(PathId, PathId)>,
) -> SpecPath {
    let raw = variant_spec_path(store, prim_path, selection_sites);
    normalize_forwarded_spec_path(store, &raw, provenance_remap)
}

fn normalized_variant_property_spec_path(
    store: &mut dyn LayerStore,
    prim_path: PathId,
    selection_sites: &[VariantSelectionSite],
    property: TokenId,
    provenance_remap: Option<(PathId, PathId)>,
) -> SpecPath {
    let raw = variant_property_spec_path(store, prim_path, selection_sites, property);
    normalize_forwarded_spec_path(store, &raw, provenance_remap)
}

/// Composes a stage from a root layer.
///
/// This implements:
/// - Layer stack gathering (layer is stronger than its sublayers)
/// - Stage population (including prims introduced via references)
/// - Value resolution (scalar + `ListOp`)
pub(crate) fn compose_stage(
    store: &mut dyn LayerStore,
    root: LayerId,
    options: StageOptions,
) -> Stage {
    let mut cycles = CycleDetector::new(root);
    let layer_stack = cycles.gather_layer_stack(store, root);
    let (paths, mut children) = populate(store, &layer_stack, options.mask.as_ref());

    // Every prim's graph starts at its own site in the root layer stack
    // (OpenUSD: the root node of `PcpPrimIndex`).
    let mut prims: HashMap<PathId, PrimIndex> = paths
        .iter()
        .copied()
        .map(|path| {
            let namespace_depth =
                u16::try_from(store.paths().resolve(path).depth()).unwrap_or(u16::MAX);
            let root_node = NodeArc {
                arc_kind: ArcKind::Local,
                layer_stack: root,
                site: SpecPath::from_prim_path(path, store.paths()),
                namespace_depth,
                sibling_index: 0,
                implied: false,
                copied: false,
                strength: NodeStrength::local(namespace_depth),
            };
            (path, PrimIndex::new(PrimIndexGraph::new(root_node)))
        })
        .collect();

    let mut prim_order_opinions: HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>> = HashMap::new();
    let mut authored_children_opinions: HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>> =
        HashMap::new();

    let mut dep_builder = if options.with_dependencies {
        Some(DependencyBuilder::new())
    } else {
        None
    };

    add_local_and_variant_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        dep_builder.as_mut(),
    );
    add_inherit_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        &mut cycles,
        dep_builder.as_mut(),
    );
    add_reference_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        &mut cycles,
        dep_builder.as_mut(),
    );
    add_payload_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        &mut cycles,
        dep_builder.as_mut(),
    );
    add_specializes_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        &mut cycles,
        dep_builder.as_mut(),
    );

    for prim in prims.values_mut() {
        drop_reached_copies(prim);
        prim.finalize();
    }

    prune_unselected_variant_specs(store, &layer_stack, &mut prims);

    apply_child_order(
        store,
        &prims,
        &authored_children_opinions,
        &prim_order_opinions,
        &mut children,
    );

    filter_variant_children(store, &prims, &mut children);

    strip_instance_descendants(
        store,
        &mut prims,
        &mut children,
        &authored_children_opinions,
    );

    prune_deactivated(store, &mut prims, &mut children);

    // Runs last so the ordering passes above see the populated child lists;
    // removal only drops entries.
    remove_prims_without_specs(store, &mut prims, &mut children);

    if let Some(builder) = dep_builder.as_mut() {
        builder.retain_prims(&prims);
    }
    let dependencies = dep_builder.map(DependencyBuilder::finish);
    // Only prims of the composed stage report arc errors: population
    // over-approximates, and pruned prims are not part of the stage.
    let errors = cycles
        .into_errors()
        .into_iter()
        .filter(|error| error.prim().is_none_or(|prim| prims.contains_key(&prim)))
        .collect();
    Stage::from_parts(prims, children, options.with_provenance, dependencies)
        .with_composition_errors(errors)
}

/// Drops the registrations the late copy (see `Forwarding`) made of sites
/// the prim already registers, wherever that leaves the order of its sites
/// unchanged: a copy ranked after a kept registration of its site, and a
/// copy the next registration of its site follows directly. A copy that
/// ranks its site above the expansion's own registration, with other sites
/// between them, stays.
fn drop_reached_copies(prim: &mut PrimIndex) {
    let registration = |key: &OpinionKey| OpinionKey {
        spec_path: key.spec_path.prim_spec(),
        ..key.clone()
    };
    let dropped: HashSet<OpinionKey> = {
        let graph = &prim.graph;
        let copied = |node: NodeId| graph.node(node).is_some_and(|node| node.arc.copied);
        let mut sources: Vec<&OpinionKey> = prim.sources.iter().collect();
        sources.sort_by(|a, b| graph.cmp_keys(a, b));
        sources.dedup();
        let same_site =
            |a: &OpinionKey, b: &OpinionKey| a.layer_id == b.layer_id && a.spec_path == b.spec_path;
        let mut kept: HashSet<(LayerId, &SpecPath)> = HashSet::new();
        let mut dropped = HashSet::new();
        for (index, key) in sources.iter().enumerate() {
            let site = (key.layer_id, &key.spec_path);
            if !copied(key.node) {
                kept.insert(site);
                continue;
            }
            // A copy weaker than a kept registration of its site, or one
            // the next registration of its site follows with no other site
            // between, does not change the order of the prim's sites.
            let adjacent = sources
                .get(index + 1)
                .is_some_and(|next| same_site(key, next));
            if kept.contains(&site) || adjacent {
                dropped.insert((*key).clone());
            } else {
                kept.insert(site);
            }
        }
        dropped
    };
    let graph = &prim.graph;
    let copied = |node: NodeId| graph.node(node).is_some_and(|node| node.arc.copied);
    let mut seen = HashSet::new();
    prim.sources
        .retain(|key| !copied(key.node) || (!dropped.contains(key) && seen.insert(key.clone())));
    for opinions in prim.opinions_by_field.values_mut() {
        let mut seen = HashSet::new();
        opinions.retain(|opinion| {
            !copied(opinion.key.node)
                || (!dropped.contains(&registration(&opinion.key))
                    && seen.insert(opinion.key.clone()))
        });
    }
    prim.opinions_by_field
        .retain(|_, opinions| !opinions.is_empty());
}

/// Removes populated prims whose prim index holds no spec, together with
/// spec-less descendants.
///
/// Population over-approximates: to discover prims introduced through
/// variant-scoped arcs before selections are known, it expands the arcs of
/// every branch (`collect_all_variant_*`). Arc expansion then follows only the
/// selected branches, so a prim reached solely through an unselected branch
/// ends up with no specs. Such a path is not a prim: a prim exists only where
/// its composed prim index contains at least one spec. A spec-less prim that
/// still has children with specs is kept, so this never changes hierarchy
/// above a real prim.
///
/// Spec: AOUSD Core §11 (stage population from composed prim indexes);
/// OpenUSD only populates prims whose index has specs
/// (`PcpPrimIndex::HasSpecs`).
fn remove_prims_without_specs(
    store: &dyn LayerStore,
    prims: &mut HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
) {
    let Some(root) = store.paths().lookup(&crate::path::Path::root()) else {
        return;
    };
    loop {
        let removable: Vec<PathId> = prims
            .iter()
            .filter(|(path, index)| {
                **path != root
                    && index.sources.is_empty()
                    && children.get(*path).is_none_or(Vec::is_empty)
            })
            .map(|(path, _)| *path)
            .collect();
        if removable.is_empty() {
            return;
        }
        for path in &removable {
            prims.remove(path);
            children.remove(path);
        }
        let removed: HashSet<PathId> = removable.into_iter().collect();
        for list in children.values_mut() {
            list.retain(|child| !removed.contains(child));
        }
    }
}

/// Resolves the variant selections that govern a composed prim, in strength
/// order.
///
/// Sources are visited strongest-first. A variant node (a source whose spec
/// path ends in `{set=variant}`) contributes the selections authored inside
/// that branch at the node's own strength, so a selection authored in a
/// stronger variant beats one authored on a weaker referenced prim. Sets not
/// resolved this way fall back to [`composed_variant_selections`].
///
/// Spec: AOUSD Core §10.5 (the strongest variant selection opinion in the
/// prim index wins, independent of which arc introduced the variant set).
fn strength_ordered_variant_selections(
    store: &dyn LayerStore,
    prim_index: &PrimIndex,
) -> HashMap<TokenId, TokenId> {
    use crate::spec_path::SpecComponent;

    let mut selections: HashMap<TokenId, TokenId> = HashMap::new();
    for source in &prim_index.sources {
        let Some(spec) = store.layer(source.layer_id).and_then(|layer| {
            layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
        }) else {
            continue;
        };
        let authored = match source.spec_path.components().last() {
            Some(SpecComponent::VariantSelection { set, variant }) => spec
                .variant_sets
                .get(set)
                .and_then(|set_spec| set_spec.variants.get(variant))
                .map(|variant_spec| &variant_spec.variant_selections),
            _ => Some(&spec.variant_selections),
        };
        for (set, variant) in authored.into_iter().flatten() {
            selections.entry(*set).or_insert(*variant);
        }
    }
    for (set, variant) in composed_variant_selections(store, prim_index) {
        selections.entry(set).or_insert(variant);
    }
    selections
}

/// Resolves the variant selections of a composed prim from its prim index.
///
/// Sources are visited strongest-first, so the strongest selection authored
/// directly on a source spec wins, regardless of which arc introduced it;
/// then selections authored inside selected branches are chained in.
///
/// Spec: AOUSD Core §10.5 (variant selection).
fn composed_variant_selections(
    store: &dyn LayerStore,
    prim_index: &PrimIndex,
) -> HashMap<TokenId, TokenId> {
    let mut selections: HashMap<TokenId, TokenId> = HashMap::new();
    for source in &prim_index.sources {
        let Some(layer) = store.layer(source.layer_id) else {
            continue;
        };
        let Some(spec) =
            layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
        else {
            continue;
        };
        for (set, variant) in &spec.variant_selections {
            selections.entry(*set).or_insert(*variant);
        }
    }

    // Expand selections from within selected variant branches (chaining).
    loop {
        let mut new_sels = HashMap::new();
        for source in &prim_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
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
        if new_sels.is_empty() {
            break;
        }
        selections.extend(new_sels);
    }
    selections
}

/// Removes opinions and sources whose source spec lies inside an unselected
/// variant branch.
///
/// Every opinion carries its source identity as a [`SpecPath`], which names
/// each variant branch on the way to the authored spec
/// (`/Model{x=a}Child.attr`). An opinion may only contribute while every one
/// of those branches is selected. The arc passes cannot decide this on their
/// own: USDA ingestion also stores prims introduced inside a variant branch in
/// `Layer::prims` under their namespace path, so they are found by namespace
/// lookup, and the selection that governs a variant set may be authored by a
/// sibling arc that has not been expanded yet.
///
/// Arcs map namespace structure one-to-one, so the composed prim hosting a
/// variant set is the ancestor of the destination prim at the same relative
/// depth as the variant host is to the source spec. The selection is taken
/// from that composed host's prim index (strongest selection across all
/// arcs). A host that this walk does not reach, such as the ancestor of a
/// subroot arc target, is looked up by its own path: a source layer in the
/// stage's layer stack takes the stage's composed selection for that host,
/// since an arc into that layer stack composes the host's ancestral opinions
/// the same way; any other layer takes the selection authored on the host in
/// the source layer. When no selection is known for a set at all (fallback
/// selections are not modeled yet), the opinion is kept rather than guessed
/// away.
///
/// Prims are pruned parents first, and each prim in two steps: sources inside
/// unselected branches of its ancestors go first, then its own selections are
/// read from what remains. A prim authored in every branch of an ancestor's
/// variant set (`/P{v=a}C` and `/P{v=b}C`, each with its own `variants`)
/// otherwise takes its selection from whichever branch spec happens to rank
/// first, selected or not.
///
/// Spec: AOUSD Core §10.5 (only the selected variant of each variant set
/// contributes opinions). Where the Core is silent (§4.2), this follows
/// OpenUSD v26.08: `pxr/usd/pcp/primIndex.cpp:4471` adds a variant arc only
/// for the selection found by `_ComposeVariantSelection` (`:4130`), which
/// searches the whole prim index in strength order.
fn prune_unselected_variant_specs(
    store: &dyn LayerStore,
    stage_stack: &LayerStack,
    prims: &mut HashMap<PathId, PrimIndex>,
) {
    let mut selection_cache: HashMap<PathId, HashMap<TokenId, TokenId>> = HashMap::new();

    let mut prim_paths: Vec<(usize, PathId)> = prims
        .keys()
        .map(|path| (store.paths().resolve(*path).depth(), *path))
        .collect();
    prim_paths.sort_unstable();
    for (_, prim_path) in prim_paths {
        for hosts in [BranchHosts::Ancestors, BranchHosts::Own] {
            let mut rejected: HashSet<(LayerId, SpecPath)> = HashSet::new();
            {
                let index = &prims[&prim_path];
                let mut checked: HashSet<(LayerId, &SpecPath)> = HashSet::new();
                let all_keys = index
                    .sources
                    .iter()
                    .chain(index.opinions_by_field.values().flatten().map(|op| &op.key));
                for key in all_keys {
                    if !checked.insert((key.layer_id, &key.spec_path)) {
                        continue;
                    }
                    if !spec_path_branches_selected(
                        store,
                        stage_stack,
                        prims,
                        &mut selection_cache,
                        prim_path,
                        key.layer_id,
                        &key.spec_path,
                        hosts,
                    ) {
                        rejected.insert((key.layer_id, key.spec_path.clone()));
                    }
                }
            }
            if rejected.is_empty() {
                continue;
            }

            let is_rejected =
                |key: &OpinionKey| rejected.contains(&(key.layer_id, key.spec_path.clone()));
            prims
                .get_mut(&prim_path)
                .expect("prim exists")
                .retain_keys(|_, key| !is_rejected(key));
            // Read this prim's selections again from what remains.
            selection_cache.remove(&prim_path);
        }
    }
}

/// Which variant hosts [`spec_path_branches_selected`] checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BranchHosts {
    /// Hosts that compose to an ancestor of the destination prim.
    Ancestors,
    /// Hosts that compose to the destination prim itself.
    Own,
}

/// Returns `true` unless `spec_path` names a variant branch, hosted where
/// `hosts` says, that is not selected for the composed prim `prim_path`.
fn spec_path_branches_selected(
    store: &dyn LayerStore,
    stage_stack: &LayerStack,
    prims: &HashMap<PathId, PrimIndex>,
    selection_cache: &mut HashMap<PathId, HashMap<TokenId, TokenId>>,
    prim_path: PathId,
    layer_id: LayerId,
    spec_path: &SpecPath,
    hosts: BranchHosts,
) -> bool {
    use crate::spec_path::SpecComponent;

    let components = spec_path.components();
    if !components
        .iter()
        .any(|c| matches!(c, SpecComponent::VariantSelection { .. }))
    {
        return true;
    }
    let spec_depth = components
        .iter()
        .filter(|c| matches!(c, SpecComponent::Prim(_)))
        .count();
    let paths = store.paths();
    let dest = paths.resolve(prim_path).clone();

    let mut host_segments: Vec<TokenId> = Vec::new();
    for component in components {
        let (set, variant) = match *component {
            SpecComponent::Prim(segment) => {
                host_segments.push(segment);
                continue;
            }
            SpecComponent::VariantSelection { set, variant } => (set, variant),
        };
        let own = host_segments.len() == spec_depth;
        if own != (hosts == BranchHosts::Own) {
            continue;
        }

        // Walk up from the destination prim by the host's relative depth.
        let mut composed_host = Some(dest.clone());
        for _ in host_segments.len()..spec_depth {
            composed_host = composed_host.and_then(|p| p.parent());
        }
        let composed_selection = composed_host
            .and_then(|p| paths.lookup(&p))
            .filter(|id| prims.contains_key(id))
            .and_then(|id| {
                selection_cache
                    .entry(id)
                    .or_insert_with(|| strength_ordered_variant_selections(store, &prims[&id]))
                    .get(&set)
                    .copied()
            });
        let host = paths.lookup(&crate::path::Path::root().join(&host_segments));
        let selection = composed_selection
            .or_else(|| {
                let host = host.filter(|id| prims.contains_key(id))?;
                if !stage_stack.layers.contains(&layer_id) {
                    return None;
                }
                selection_cache
                    .entry(host)
                    .or_insert_with(|| strength_ordered_variant_selections(store, &prims[&host]))
                    .get(&set)
                    .copied()
            })
            .or_else(|| {
                store
                    .layer(layer_id)?
                    .prims
                    .get(&host?)?
                    .variant_selections
                    .get(&set)
                    .copied()
            });
        if selection.is_some_and(|selected| selected != variant) {
            return false;
        }
    }
    true
}

/// Filters children maps by removing variant-only children that don't belong
/// to the selected variant.
///
/// For each prim that has variant sets (found via its composed opinion sources),
/// the selected variants determine which variant children remain. Children that
/// exist only in non-selected variant branches are removed.
///
/// Spec: AOUSD Core §10.5 (variant selection), §11 (population).
fn filter_variant_children(
    store: &dyn LayerStore,
    prims: &HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
) {
    use hashbrown::HashSet;

    let parent_paths: Vec<PathId> = children.keys().copied().collect();
    for parent_path in parent_paths {
        let Some(prim_index) = prims.get(&parent_path) else {
            continue;
        };

        // Collect all variant set specs and variant selections across opinion sources.
        let mut all_variant_children: HashSet<TokenId> = HashSet::new();
        let mut selected_children: HashSet<TokenId> = HashSet::new();
        let mut has_variant_sets = false;
        let mut variant_set_order: Vec<TokenId> = Vec::new();

        // First, resolve variant selections and variant set order from all opinion sources.
        let selections = composed_variant_selections(store, prim_index);
        for source in &prim_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            // Use the first non-empty variant_set_order we find.
            if !spec.variant_set_order.is_empty() {
                variant_set_order = spec.variant_set_order.clone();
                break;
            }
        }

        // Then check each source for variant sets.
        for source in &prim_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };

            for (set_name, set_spec) in &spec.variant_sets {
                for (variant_name, variant_spec) in &set_spec.variants {
                    for child in &variant_spec.authored_children {
                        has_variant_sets = true;
                        all_variant_children.insert(*child);
                        if selections.get(set_name) == Some(variant_name) {
                            // A child of a variant set nested in other
                            // branches of this prim also needs those
                            // branches selected.
                            let branch = VariantSelectionSite {
                                host_path: source.lookup_path,
                                set: *set_name,
                                variant: *variant_name,
                            };
                            let requirements =
                                nested_branch_requirements(store, layer, *child, branch);
                            let outer_ok = requirements.is_empty()
                                || requirements.iter().any(|reqs| {
                                    reqs.iter().all(|site| {
                                        selections.get(&site.set) == Some(&site.variant)
                                    })
                                });
                            if outer_ok {
                                selected_children.insert(*child);
                            }
                        }
                    }
                }
            }
        }

        if !has_variant_sets || all_variant_children.is_empty() {
            continue;
        }

        let unselected: HashSet<TokenId> = all_variant_children
            .difference(&selected_children)
            .copied()
            .collect();

        if unselected.is_empty() && variant_set_order.is_empty() {
            continue;
        }

        // Filter children list.
        if let Some(child_list) = children.get_mut(&parent_path) {
            child_list.retain(|child_path| {
                let child = store.paths().resolve(*child_path);
                if let Some(leaf) = child.leaf() {
                    !unselected.contains(&leaf)
                } else {
                    true
                }
            });

            // Re-order variant children: group by source arc (weakest first),
            // then by variant set order within each group (later sets first).
            // Within a variant set, children from deeper nesting levels
            // (more enclosing branches of the same prim) come before
            // shallower ones because deeper variant opinions are stronger.
            use alloc::collections::BTreeMap;
            let mut arc_groups: BTreeMap<u16, Vec<TokenId>> = BTreeMap::new();

            // Collect nesting depth for each child across all sources.
            let mut child_nesting_depth: HashMap<TokenId, usize> = HashMap::new();
            for source in &prim_index.sources {
                let Some(layer) = store.layer(source.layer_id) else {
                    continue;
                };
                let Some(spec) =
                    layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
                else {
                    continue;
                };
                for (set, set_spec) in &spec.variant_sets {
                    for (variant, variant_spec) in &set_spec.variants {
                        let branch = VariantSelectionSite {
                            host_path: source.lookup_path,
                            set: *set,
                            variant: *variant,
                        };
                        for child in &variant_spec.authored_children {
                            let depth = nested_branch_requirements(store, layer, *child, branch)
                                .iter()
                                .map(Vec::len)
                                .max()
                                .unwrap_or(0);
                            let entry = child_nesting_depth.entry(*child).or_insert(0);
                            *entry = (*entry).max(depth);
                        }
                    }
                }
            }

            for set_tok in variant_set_order.iter().rev() {
                let Some(&selected_variant) = selections.get(set_tok) else {
                    continue;
                };
                for source in &prim_index.sources {
                    let Some(layer) = store.layer(source.layer_id) else {
                        continue;
                    };
                    let Some(spec) = layer.source_prim_spec(
                        source.lookup_path,
                        &source.spec_path,
                        store.paths(),
                    ) else {
                        continue;
                    };
                    if let Some(set_spec) = spec.variant_sets.get(set_tok)
                        && let Some(variant_spec) = set_spec.variants.get(&selected_variant)
                    {
                        let arc_list_index = prim_index.graph.strength(source.node).arc_list_index;
                        let group = arc_groups.entry(arc_list_index).or_default();
                        for child in &variant_spec.authored_children {
                            if !group.contains(child) {
                                group.push(*child);
                            }
                        }
                    }
                }
            }
            // Weakest arc first (highest arc_list_index first).
            let mut ordered_variant_children: Vec<TokenId> = Vec::new();
            for (_arc_idx, children) in arc_groups.iter().rev() {
                for child in children {
                    if !ordered_variant_children.contains(child) {
                        ordered_variant_children.push(*child);
                    }
                }
            }

            // Sort by nesting depth (deepest first = strongest opinions).
            // Children from deeper variant nesting have more required
            // outer selections and represent stronger opinions.
            ordered_variant_children.sort_by(|a, b| {
                let a_depth = child_nesting_depth.get(a).copied().unwrap_or(0);
                let b_depth = child_nesting_depth.get(b).copied().unwrap_or(0);
                b_depth.cmp(&a_depth)
            });

            if !ordered_variant_children.is_empty() {
                // Build a position map for stable sorting.
                let child_pos: HashMap<TokenId, usize> = ordered_variant_children
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (*c, i))
                    .collect();

                // Sort: variant children first (in authored order),
                // then non-variant children (preserving existing order).
                child_list.sort_by(|a, b| {
                    let a_leaf = store.paths().resolve(*a).leaf();
                    let b_leaf = store.paths().resolve(*b).leaf();
                    let a_pos = a_leaf.and_then(|l| child_pos.get(&l).copied());
                    let b_pos = b_leaf.and_then(|l| child_pos.get(&l).copied());
                    match (a_pos, b_pos) {
                        (None, None) => Ordering::Equal,
                        (None, Some(_)) => Ordering::Greater,
                        (Some(_), None) => Ordering::Less,
                        (Some(ai), Some(bi)) => ai.cmp(&bi),
                    }
                });
            }
        }
    }

    // Second pass: filter children authored by this prim's specs inside its
    // parent's variant branches (`/Parent{v=x}Prim/Child`). Children that
    // only those specs of unselected branches author are removed.
    let parent_paths2: Vec<PathId> = children.keys().copied().collect();
    for parent_path in parent_paths2 {
        let parent_leaf = store.paths().resolve(parent_path).leaf();
        let grandparent = store.paths().resolve(parent_path).parent();
        let (Some(leaf), Some(gp)) = (parent_leaf, grandparent) else {
            continue;
        };
        let Some(gp_id) = store.paths().lookup(&gp) else {
            continue;
        };
        let Some(gp_index) = prims.get(&gp_id) else {
            continue;
        };

        // Resolve grandparent's variant selections (with chaining).
        let mut gp_selections: HashMap<TokenId, TokenId> = HashMap::new();
        for source in &gp_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            for (set, variant) in &spec.variant_selections {
                gp_selections.entry(*set).or_insert(*variant);
            }
        }
        // Chain through variant branches to discover transitive selections.
        loop {
            let mut new_sels = HashMap::new();
            for source in &gp_index.sources {
                let Some(layer) = store.layer(source.layer_id) else {
                    continue;
                };
                let Some(spec) =
                    layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
                else {
                    continue;
                };
                for (set, selected_variant) in &gp_selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            if !gp_selections.contains_key(inner_set) {
                                new_sels.entry(*inner_set).or_insert(*inner_variant);
                            }
                        }
                    }
                }
            }
            if new_sels.is_empty() {
                break;
            }
            gp_selections.extend(new_sels);
        }

        // Collect all and selected grandchild authored children.
        let mut all_gc: HashSet<TokenId> = HashSet::new();
        let mut selected_gc: HashSet<TokenId> = HashSet::new();
        let mut has_gc = false;

        for source in &gp_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            let Some(child_path) = store
                .paths()
                .lookup(&store.paths().resolve(source.lookup_path).join(&[leaf]))
            else {
                continue;
            };
            if spec.variant_sets.is_empty() {
                continue;
            }
            // Every branch enclosing a grandchild's spec must be selected,
            // not only its innermost one: outer branches may reuse an inner
            // branch name.
            for branch_spec in layer.prim_specs(child_path).filter(|branch_spec| {
                branch_spec
                    .outer_variant_sites
                    .last()
                    .is_some_and(|site| site.host_path == source.lookup_path)
            }) {
                if branch_spec.authored_children.is_empty() {
                    continue;
                }
                has_gc = true;
                all_gc.extend(branch_spec.authored_children.iter().copied());
            }
            for branch_spec in
                layer.selected_branch_prim_specs(child_path, source.lookup_path, &gp_selections)
            {
                // Branches hosted on the grandparent's ancestors must be
                // selected there too.
                let ancestors_selected = branch_spec
                    .outer_variant_sites
                    .iter()
                    .filter(|site| site.host_path != source.lookup_path)
                    .all(|site| {
                        prims.get(&site.host_path).is_none_or(|index| {
                            composed_variant_selections(store, index)
                                .get(&site.set)
                                .is_none_or(|selected| *selected == site.variant)
                        })
                    });
                if ancestors_selected {
                    selected_gc.extend(branch_spec.authored_children.iter().copied());
                }
            }
        }

        if !has_gc {
            continue;
        }

        let unselected_gc: HashSet<TokenId> = all_gc.difference(&selected_gc).copied().collect();
        if unselected_gc.is_empty() {
            continue;
        }

        if let Some(child_list) = children.get_mut(&parent_path) {
            child_list.retain(|child_path| {
                let child = store.paths().resolve(*child_path);
                if let Some(child_leaf) = child.leaf() {
                    !unselected_gc.contains(&child_leaf)
                } else {
                    true
                }
            });

            // Re-order: variant-scoped grandchildren should come after
            // non-variant children (e.g. children from references).
            let (gc_children, other_children): (Vec<_>, Vec<_>) =
                child_list.iter().copied().partition(|child_path| {
                    let child = store.paths().resolve(*child_path);
                    child
                        .leaf()
                        .map(|l| selected_gc.contains(&l))
                        .unwrap_or(false)
                });
            *child_list = other_children;
            child_list.extend(gc_children);
        }
    }

    // Third pass: for prims that inherit or specialize from another prim,
    // remove any children that exist under the destination but were filtered
    // out from the source's children. This handles the case where variant
    // children of an inherited class are filtered at the class level but
    // still appear under the inheriting prim.
    let parent_paths3: Vec<PathId> = children.keys().copied().collect();
    for parent_path in parent_paths3 {
        let Some(prim_index) = prims.get(&parent_path) else {
            continue;
        };

        // Check all sources for inherit/specialize arcs by looking at the
        // prim's opinion sources for inherit-kind arcs.
        let mut inherited_sources: Vec<PathId> = Vec::new();
        for source in &prim_index.sources {
            let strength = prim_index.graph.strength(source.node);
            if strength.arc_kind == ArcKind::Inherits
                || strength.arc_kind == ArcKind::Specializes
                || strength.nested_arc_kind == Some(ArcKind::Inherits)
                || strength.nested_arc_kind == Some(ArcKind::Specializes)
            {
                // The spec_path points to the source prim in its original namespace.
                // We need the mapped path in the same namespace as parent_path.
                // The source might be in a different namespace (e.g. /Model/Class
                // for /Model/Scope). We need the direct inherit source path.
                if source.lookup_path != parent_path {
                    inherited_sources.push(source.lookup_path);
                }
            }
        }

        if inherited_sources.is_empty() {
            continue;
        }

        // For each inherited source, check which of its children survived filtering.
        let mut to_remove: HashSet<TokenId> = HashSet::new();
        for src_path in &inherited_sources {
            let src_children = children.get(src_path);
            if let Some(src_child_list) = src_children {
                let src_leaves: HashSet<TokenId> = src_child_list
                    .iter()
                    .filter_map(|c| store.paths().resolve(*c).leaf())
                    .collect();

                // Any child of parent_path whose leaf matches a child that was
                // present at the source but got filtered out should be removed.
                if let Some(dest_child_list) = children.get(&parent_path) {
                    for child in dest_child_list {
                        let child_leaf = store.paths().resolve(*child).leaf();
                        if let Some(leaf) = child_leaf {
                            // Check if this child comes from inheritance by checking
                            // if the source prim originally had a path with this leaf
                            // as a child. If the source no longer has it (filtered),
                            // but the source's parent's variants had it, remove it.
                            let src_child_path = {
                                let sp = store.paths().resolve(*src_path).clone();
                                sp.join(&[leaf])
                            };
                            let src_child_id = store.paths().lookup(&src_child_path);
                            if let Some(sc_id) = src_child_id {
                                // Source namespace has this path, but it's not in
                                // source's filtered children → it was filtered out.
                                if !src_leaves.contains(&leaf) && prims.contains_key(&sc_id) {
                                    to_remove.insert(leaf);
                                }
                            }
                        }
                    }
                }
            }
        }

        if !to_remove.is_empty()
            && let Some(child_list) = children.get_mut(&parent_path)
        {
            child_list.retain(|child| {
                let leaf = store.paths().resolve(*child).leaf();
                leaf.map(|l| !to_remove.contains(&l)).unwrap_or(true)
            });
        }

        // Reorder: children from the prim's own arcs come before children
        // inherited from the source prim. Use the source's filtered child
        // list to identify which children are inherited. Only reorder when
        // variant filtering actually removed children — otherwise
        // `apply_authored_children_base_order` already established the
        // correct ordering.
        if to_remove.is_empty() {
            continue;
        }

        let inherited_leaves: HashSet<TokenId> = inherited_sources
            .iter()
            .filter_map(|src| children.get(src))
            .flat_map(|list| list.iter())
            .filter_map(|c| store.paths().resolve(*c).leaf())
            .collect();

        // Collect the source's child order for inherited children.
        let src_order: Vec<TokenId> = inherited_sources
            .iter()
            .filter_map(|src| children.get(src))
            .flat_map(|list| list.iter())
            .filter_map(|c| store.paths().resolve(*c).leaf())
            .collect();

        if !inherited_leaves.is_empty()
            && let Some(child_list) = children.get_mut(&parent_path)
        {
            // Only apply partition+reorder when there are children that
            // exist ONLY as direct children (not from inheritance). When
            // all children also exist in the inherited source, the normal
            // `apply_child_order` with `prim_order` opinions handles
            // ordering correctly.
            let has_direct_only = child_list.iter().any(|child| {
                let leaf = store.paths().resolve(*child).leaf();
                leaf.map(|l| !inherited_leaves.contains(&l))
                    .unwrap_or(false)
            });

            if has_direct_only {
                let (direct, mut inherited): (Vec<_>, Vec<_>) =
                    child_list.iter().copied().partition(|child| {
                        let leaf = store.paths().resolve(*child).leaf();
                        leaf.map(|l| !inherited_leaves.contains(&l)).unwrap_or(true)
                    });

                // Sort inherited children to match the source's child order.
                inherited.sort_by(|a, b| {
                    let a_leaf = store.paths().resolve(*a).leaf();
                    let b_leaf = store.paths().resolve(*b).leaf();
                    let a_pos = a_leaf.and_then(|l| src_order.iter().position(|s| *s == l));
                    let b_pos = b_leaf.and_then(|l| src_order.iter().position(|s| *s == l));
                    a_pos.cmp(&b_pos)
                });

                *child_list = direct;
                child_list.extend(inherited);
            }
        }
    }
}

/// Returns, for each prim spec of `host`'s child `child` authored directly
/// in `branch` in `layer`, the other branches of `host` enclosing it: for a
/// child authored in a variant set nested in another branch of the same prim
/// (`/P{a=x}{b=y}C`, with `branch` `{b=y}`), that is `[{a=x}]`. The child is
/// only populated while those branches are selected too.
///
/// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set specs),
/// §10.3.2.5 (variants).
fn nested_branch_requirements(
    store: &dyn LayerStore,
    layer: &crate::doc::Layer,
    child: TokenId,
    branch: VariantSelectionSite,
) -> Vec<Vec<VariantSelectionSite>> {
    let host = store.paths().resolve(branch.host_path);
    let Some(child_path) = store.paths().lookup(&host.join(&[child])) else {
        return Vec::new();
    };
    layer
        .branch_prim_specs(child_path, branch)
        .map(|spec| {
            let (_, enclosing) = spec
                .outer_variant_sites
                .split_last()
                .expect("a branch spec has a branch");
            enclosing
                .iter()
                .filter(|site| site.host_path == branch.host_path)
                .copied()
                .collect()
        })
        .collect()
}

/// Strips descendant opinions and children for effective instances.
///
/// A prim is an *effective instance* when it has `instanceable = true`
/// (strongest opinion) AND at least one composition arc (references,
/// payloads, inherits). For each effective instance:
///
/// 1. Children introduced only by identity (local/instanceable) sources are
///    removed along with their entire subtrees.
/// 2. On surviving descendants, local (`is_local == true`) sources whose
///    `spec_path` is a namespace descendant of an identity path are stripped.
///    Non-local sources survive only when their arc was introduced at the
///    instance or below (`namespace_depth` at least the instance's depth):
///    sites reached through arcs authored on the instance's ancestors are
///    local to the instance, not brought in by its own arcs. OpenUSD marks
///    those nodes inert (`pxr/usd/pcp/instancing.h`,
///    `Pcp_ChildNodeIsInstanceable`).
///
/// Spec: AOUSD Core §11.3.3 (scene graph instancing: only opinions brought
/// in by the instance's composition arcs are used), §5.1.14 (instanceable).
fn strip_instance_descendants(
    store: &dyn LayerStore,
    prims: &mut HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
    authored_children_opinions: &HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
) {
    use hashbrown::HashSet;

    // Step 1: Identify effective instances and their identity paths.
    //
    // An "identity path" is a (LayerId, PathId) pair representing the instance
    // prim's own definition. Sources that are local or whose PrimSpec has
    // `instanceable == Some(true)` are considered identity.
    let mut instance_identity: Vec<(PathId, Vec<(LayerId, PathId)>)> = Vec::new();

    let all_prim_paths: Vec<PathId> = prims.keys().copied().collect();
    for &prim_path in &all_prim_paths {
        let Some(index) = prims.get(&prim_path) else {
            continue;
        };

        // Resolve instanceable: strongest opinion wins.
        let mut is_instanceable = false;
        for source in &index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            if let Some(val) = spec.instanceable {
                is_instanceable = val;
                break; // strongest wins
            }
        }
        if !is_instanceable {
            continue;
        }

        // Check for composition arcs (references, payloads, inherits).
        let has_arcs = index.sources.iter().any(|s| {
            let s = index.graph.strength(s.node);
            matches!(
                s.arc_kind,
                ArcKind::References | ArcKind::Payloads | ArcKind::Inherits
            ) && !s.is_local
        });
        if !has_arcs {
            continue;
        }

        // Collect identity paths: local sources and sources with instanceable=true.
        let mut identity_paths: Vec<(LayerId, PathId)> = Vec::new();
        for source in &index.sources {
            if index.graph.strength(source.node).is_local {
                identity_paths.push((source.layer_id, source.lookup_path));
                continue;
            }
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            if spec.instanceable == Some(true) {
                identity_paths.push((source.layer_id, source.lookup_path));
            }
        }

        if !identity_paths.is_empty() {
            instance_identity.push((prim_path, identity_paths));
        }
    }

    // Step 2: For each effective instance, determine surviving children and
    // strip descendant local opinions.
    for (instance_path, identity_paths) in &instance_identity {
        // 2a. Determine which children survive: a child survives if it appears
        // in any non-identity source's authored_children or variant children.
        let non_identity_children = collect_non_identity_children(
            store,
            *instance_path,
            identity_paths,
            prims,
            authored_children_opinions,
        );

        // Find all descendant prim paths.
        let descendants: Vec<PathId> = all_prim_paths
            .iter()
            .copied()
            .filter(|p| {
                *p != *instance_path
                    && store
                        .paths()
                        .resolve(*instance_path)
                        .is_prefix_of(store.paths().resolve(*p))
            })
            .collect();

        // Collect variant-introduced children from identity sources.
        // Descendants under variant-introduced children keep their local
        // sources because variants are their own arc (V in LIVERPS) and
        // should survive instancing stripping.
        let variant_children: HashSet<TokenId> = {
            let identity_set: HashSet<(LayerId, PathId)> = identity_paths.iter().copied().collect();
            let mut vc = HashSet::new();
            if let Some(index) = prims.get(instance_path) {
                let mut selections: HashMap<TokenId, TokenId> = HashMap::new();
                for source in &index.sources {
                    let Some(layer) = store.layer(source.layer_id) else {
                        continue;
                    };
                    let Some(spec) = layer.source_prim_spec(
                        source.lookup_path,
                        &source.spec_path,
                        store.paths(),
                    ) else {
                        continue;
                    };
                    for (set, variant) in &spec.variant_selections {
                        selections.entry(*set).or_insert(*variant);
                    }
                }
                for source in &index.sources {
                    if !identity_set.contains(&(source.layer_id, source.lookup_path)) {
                        continue;
                    }
                    let Some(layer) = store.layer(source.layer_id) else {
                        continue;
                    };
                    let Some(spec) = layer.source_prim_spec(
                        source.lookup_path,
                        &source.spec_path,
                        store.paths(),
                    ) else {
                        continue;
                    };
                    for (set, set_spec) in &spec.variant_sets {
                        let Some(selected) = selections.get(set) else {
                            continue;
                        };
                        let Some(variant_spec) = set_spec.variants.get(selected) else {
                            continue;
                        };
                        for child in &variant_spec.authored_children {
                            vc.insert(*child);
                        }
                    }
                }
            }
            vc
        };

        let instance_resolved = store.paths().resolve(*instance_path).clone();
        let instance_depth = u16::try_from(instance_resolved.depth()).unwrap_or(u16::MAX);

        // 2b. Strip local sources and opinions on surviving descendants.
        for &desc_path in &descendants {
            // Skip stripping for descendants under variant-introduced children.
            // Variant opinions are their own arc (V in LIVERPS) and survive
            // instancing, so their descendant local sources should too.
            let desc_resolved = store.paths().resolve(desc_path);
            let is_under_variant_child = desc_resolved
                .strip_prefix(&instance_resolved)
                .and_then(|rel| rel.first().copied())
                .is_some_and(|first_name| variant_children.contains(&first_name));
            if is_under_variant_child {
                continue;
            }

            let Some(desc_index) = prims.get_mut(&desc_path) else {
                continue;
            };

            // Strip LOCAL sources that are namespace-descendants of identity
            // paths, and arc sources introduced above the instance, together
            // with their opinions and property declarations; the property
            // type then comes from the strongest surviving declaration.
            // Variant/inherit/reference sources of the instance's own arcs
            // survive.
            desc_index.retain_keys(|graph, key| {
                let strength = graph.strength(key.node);
                if !strength.is_local {
                    return strength.namespace_depth >= instance_depth;
                }
                !is_identity_descendant(store, key.layer_id, key.lookup_path, identity_paths)
            });
        }

        // 2c. Strip children of the instance that are identity-only.
        if let Some(child_list) = children.get_mut(instance_path) {
            child_list.retain(|child| {
                let child_leaf = store.paths().resolve(*child).leaf();
                child_leaf.is_some_and(|name| non_identity_children.contains(&name))
            });
        }

        // 2d. Also apply child stripping to descendant prims (recursive
        // instances or descendant prims with identity-introduced children).
        for &desc_path in &descendants {
            if let Some(child_list) = children.get_mut(&desc_path) {
                child_list
                    .retain(|child| prims.get(child).is_some_and(|idx| !idx.sources.is_empty()));
            }
        }

        // 2e. Remove subtrees of stripped children. A descendant is removed
        // if its first namespace component under the instance is not a
        // surviving child.
        let surviving_child_paths: HashSet<PathId> = children
            .get(instance_path)
            .map(|list| list.iter().copied().collect())
            .unwrap_or_default();

        for &desc_path in &descendants {
            let desc_resolved = store.paths().resolve(desc_path);
            let Some(rel) = desc_resolved.strip_prefix(&instance_resolved) else {
                continue;
            };
            if let Some(first_name) = rel.first().copied() {
                let child_path_obj = instance_resolved.join(&[first_name]);
                if let Some(child_id) = store.paths().lookup(&child_path_obj)
                    && !surviving_child_paths.contains(&child_id)
                {
                    prims.remove(&desc_path);
                    children.remove(&desc_path);
                }
            }
        }
    }
}

/// Collects child names introduced by non-identity sources for an instance prim.
///
/// A child survives instancing if it appears in the `authored_children` of any
/// source that is NOT an identity source (local or instanceable). This includes
/// children from reference targets, inherit classes, and variant branches on
/// non-identity sources.
fn collect_non_identity_children(
    store: &dyn LayerStore,
    instance_path: PathId,
    identity_paths: &[(LayerId, PathId)],
    prims: &HashMap<PathId, PrimIndex>,
    authored_children_opinions: &HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
) -> HashSet<TokenId> {
    use hashbrown::HashSet;

    let identity_set: HashSet<(LayerId, PathId)> = identity_paths.iter().copied().collect();
    let mut surviving = HashSet::new();

    // Check authored_children_opinions: these include both local and variant
    // authored_children with their OpinionKey provenance.
    if let Some(opinions) = authored_children_opinions.get(&instance_path) {
        for (key, children) in opinions {
            // A non-identity opinion contributes surviving children.
            // Identity = source whose (layer_id, spec_path) is in identity_paths
            // (either local or with instanceable=true).
            let is_identity = identity_set.contains(&(key.layer_id, key.lookup_path));
            if !is_identity {
                for child in children {
                    surviving.insert(*child);
                }
            }
        }
    }

    // Also check variant-introduced children directly from the PrimSpec
    // variant branches. These may not all be in authored_children_opinions
    // because a branch's `authored_children` for the prim itself are not
    // forwarded there; only those of the prim specs inside branches are.
    if let Some(index) = prims.get(&instance_path) {
        // Resolve variant selections from all sources.
        let mut selections: HashMap<TokenId, TokenId> = HashMap::new();
        for source in &index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            for (set, variant) in &spec.variant_selections {
                selections.entry(*set).or_insert(*variant);
            }
        }

        // Collect variant children from non-identity sources.
        for source in &index.sources {
            let is_identity = identity_set.contains(&(source.layer_id, source.lookup_path));
            if is_identity {
                continue;
            }
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            for (set, set_spec) in &spec.variant_sets {
                let Some(selected) = selections.get(set) else {
                    continue;
                };
                let Some(variant_spec) = set_spec.variants.get(selected) else {
                    continue;
                };
                for child in &variant_spec.authored_children {
                    surviving.insert(*child);
                }
            }
        }

        // Also add variant children from identity sources' variant branches,
        // because variants are their own arc (V in LIVERPS) and should survive
        // instancing stripping even when authored on identity sources.
        for source in &index.sources {
            let is_identity = identity_set.contains(&(source.layer_id, source.lookup_path));
            if !is_identity {
                continue;
            }
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            for (set, set_spec) in &spec.variant_sets {
                let Some(selected) = selections.get(set) else {
                    continue;
                };
                let Some(variant_spec) = set_spec.variants.get(selected) else {
                    continue;
                };
                for child in &variant_spec.authored_children {
                    surviving.insert(*child);
                }
            }
        }

        // Collect authored_children from non-identity sources directly from
        // the PrimSpec (supplements authored_children_opinions).
        for source in &index.sources {
            let is_identity = identity_set.contains(&(source.layer_id, source.lookup_path));
            if is_identity {
                continue;
            }
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            for child in &spec.authored_children {
                surviving.insert(*child);
            }
        }
    }

    surviving
}

/// Checks if `(layer_id, spec_path)` has a `spec_path` that is a descendant of
/// any identity path from the same layer.
fn is_identity_descendant(
    store: &dyn LayerStore,
    layer_id: LayerId,
    spec_path: PathId,
    identity_paths: &[(LayerId, PathId)],
) -> bool {
    let spec_resolved = store.paths().resolve(spec_path);
    for &(id_layer, id_path) in identity_paths {
        if layer_id != id_layer {
            continue;
        }
        let id_resolved = store.paths().resolve(id_path);
        // spec_path must be a STRICT descendant (not equal to) the identity path.
        if id_resolved.is_prefix_of(spec_resolved) && spec_path != id_path {
            return true;
        }
    }
    false
}

/// Removes deactivated prims and their namespace descendants from the stage.
///
/// A prim is deactivated when its strongest `active` opinion across all
/// contributing sources resolves to `false`. When a prim is deactivated,
/// both it and all its namespace descendants are removed from the prim index
/// and children map.
///
/// Spec: AOUSD Core §7.6 (active metadata), §11 (stage population).
fn prune_deactivated(
    store: &dyn LayerStore,
    prims: &mut HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
) {
    let mut deactivated: Vec<PathId> = Vec::new();

    let all_paths: Vec<PathId> = prims.keys().copied().collect();
    for &prim_path in &all_paths {
        let Some(index) = prims.get(&prim_path) else {
            continue;
        };

        // Resolve active: strongest opinion wins.
        let mut active_value: Option<bool> = None;
        for source in &index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            if let Some(val) = spec.active {
                active_value = Some(val);
                break; // strongest wins
            }
        }

        if active_value == Some(false) {
            deactivated.push(prim_path);
        }
    }

    // For each deactivated prim, collect all its namespace descendants
    // and remove them all.
    let mut to_remove: HashSet<PathId> = HashSet::new();
    for &deact_path in &deactivated {
        to_remove.insert(deact_path);
        let deact_resolved = store.paths().resolve(deact_path);
        for &path in &all_paths {
            if path != deact_path && deact_resolved.is_prefix_of(store.paths().resolve(path)) {
                to_remove.insert(path);
            }
        }
    }

    // Remove from prims and children.
    for path in &to_remove {
        prims.remove(path);
        children.remove(path);
    }

    // Remove deactivated paths from parent children lists.
    for child_list in children.values_mut() {
        child_list.retain(|c| !to_remove.contains(c));
    }
}

/// Resolves variant selections considering both the local layer stack and
/// referenced layers (weaker selections from references fill in gaps).
///
/// The result is keyed by set name but scoped to the variant sets hosted on
/// `path` itself: every selection comes from a site that maps to `path` (its
/// own specs, selections authored for it inside its parent's selected branch,
/// and its inherit, reference and payload targets). Ancestors' selections are
/// deliberately not merged in: a same-named set on an ancestor is a different
/// variant set, and letting its selection apply here would pick a branch
/// nobody selected.
///
/// Spec: AOUSD Core §10.5 (variant selection), §9 (LIVERPS strength ordering).
/// OpenUSD resolves a set's selection at the site hosting it
/// (`pxr/usd/pcp/primIndex.cpp`, `_ComposeVariantSelection`).
fn resolve_full_variant_selections(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    path: PathId,
) -> HashMap<TokenId, TokenId> {
    let mut selections = resolve_variant_selections_for_prim(store, local_stack, path);
    for (set, variant) in resolve_variant_child_selections_for_prim(store, local_stack, path) {
        selections.entry(set).or_insert(variant);
    }

    // Also gather selections from inherit targets (weaker than local, per LIVERPS).
    let inherits = resolve_inherits_for_prim(store, local_stack, path, SelectionScope::Stack);
    for inherit_target in inherits.iter().copied() {
        let inherit_selections =
            resolve_variant_selections_for_prim(store, local_stack, inherit_target);
        for (set, variant) in inherit_selections {
            selections.entry(set).or_insert(variant);
        }
    }

    // Gather selections introduced by selected local/inherit variant branches
    // before consulting weaker reference and payload targets.
    loop {
        let mut new_selections = HashMap::new();
        let check_paths = core::iter::once(path).chain(inherits.iter().copied());
        for check_path in check_paths {
            for layer_id in &local_stack.layers {
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
                                new_selections.entry(*inner_set).or_insert(*inner_variant);
                            }
                        }
                    }
                }
            }
        }
        if new_selections.is_empty() {
            break;
        }
        selections.extend(new_selections);
    }

    // Also gather selections from reference targets (weaker). Internal arcs
    // target the whole stack (AOUSD Core §10.3.2.1).
    let Some(&anchor) = local_stack.layers.first() else {
        return selections;
    };
    let refs = {
        let mut ops = Vec::new();
        for layer_id in &local_stack.layers {
            let Some(layer) = store.layer(*layer_id) else {
                continue;
            };
            for spec in layer.prim_specs(path) {
                if !spec_arcs_apply(store, local_stack, path, spec, SelectionScope::Stack) {
                    continue;
                }
                ops.push(anchor_internal_arcs(&spec.references, *layer_id, anchor));
            }
        }
        crate::listop::resolve_list_chain::<Reference>(&[], ops)
    };

    let mut ref_stacks: Vec<(LayerStack, PathId)> = Vec::new();
    for reference in refs {
        let ref_stack = LayerStack::gather(store, reference.layer);
        let Some(reference_path) = lookup_reference_target_path(store, &reference) else {
            continue;
        };
        let ref_selections = resolve_variant_selections_for_prim(store, &ref_stack, reference_path);
        for (set, variant) in ref_selections {
            selections.entry(set).or_insert(variant);
        }
        ref_stacks.push((ref_stack, reference_path));
    }

    loop {
        let mut new_selections = HashMap::new();
        for (ref_stack, ref_path) in &ref_stacks {
            for layer_id in &ref_stack.layers {
                let Some(layer) = store.layer(*layer_id) else {
                    continue;
                };
                let Some(spec) = layer.prims.get(ref_path) else {
                    continue;
                };
                for (set, selected_variant) in &selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            if !selections.contains_key(inner_set) {
                                new_selections.entry(*inner_set).or_insert(*inner_variant);
                            }
                        }
                    }
                }
            }
        }
        if new_selections.is_empty() {
            break;
        }
        for (set, variant) in new_selections {
            selections.entry(set).or_insert(variant);
        }
    }

    // Also gather selections from payload targets (weaker than references,
    // stronger than specializes in LIVERPS).
    let payloads = {
        let mut ops = Vec::new();
        for layer_id in &local_stack.layers {
            let Some(layer) = store.layer(*layer_id) else {
                continue;
            };
            for spec in layer.prim_specs(path) {
                if !spec_arcs_apply(store, local_stack, path, spec, SelectionScope::Stack) {
                    continue;
                }
                ops.push(anchor_internal_arcs(&spec.payloads, *layer_id, anchor));
            }
        }
        crate::listop::resolve_list_chain::<Reference>(&[], ops)
    };

    let mut payload_stacks: Vec<(LayerStack, PathId)> = Vec::new();
    for payload in payloads {
        let payload_stack = LayerStack::gather(store, payload.layer);
        let Some(payload_path) = lookup_reference_target_path(store, &payload) else {
            continue;
        };
        let payload_selections =
            resolve_variant_selections_for_prim(store, &payload_stack, payload_path);
        for (set, variant) in payload_selections {
            selections.entry(set).or_insert(variant);
        }
        payload_stacks.push((payload_stack, payload_path));
    }

    loop {
        let mut new_selections = HashMap::new();
        for (payload_stack, payload_path) in &payload_stacks {
            for layer_id in &payload_stack.layers {
                let Some(layer) = store.layer(*layer_id) else {
                    continue;
                };
                let Some(spec) = layer.prims.get(payload_path) else {
                    continue;
                };
                for (set, selected_variant) in &selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            new_selections.entry(*inner_set).or_insert(*inner_variant);
                        }
                    }
                }
            }
        }
        if new_selections.is_empty() {
            break;
        }
        for (set, variant) in new_selections {
            selections.entry(set).or_insert(variant);
        }
    }

    selections
}

/// Resolves the variant selections authored on `prim`'s specs inside the
/// selected branches of its parent (`/Parent{v=x}Prim (variants = ...)`).
///
/// Spec: AOUSD Core §7.3.6 (variant specs contain prim specs), §10.3.2.5.1
/// (computing variant selection).
fn resolve_variant_child_selections_for_prim(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
) -> HashMap<TokenId, TokenId> {
    let Some(parent) = store.paths().resolve(prim).parent() else {
        return HashMap::new();
    };
    let Some(parent_id) = store.paths().lookup(&parent) else {
        return HashMap::new();
    };

    let parent_selections = resolve_full_variant_selections(store, local_stack, parent_id);
    let mut selected = HashMap::new();
    for layer in local_stack.layers.iter().filter_map(|id| store.layer(*id)) {
        for spec in layer.selected_branch_prim_specs(prim, parent_id, &parent_selections) {
            // Branches hosted on the parent's ancestors must be selected too.
            let ancestors_selected = spec
                .outer_variant_sites
                .iter()
                .filter(|site| site.host_path != parent_id)
                .all(|site| {
                    resolve_full_variant_selections(store, local_stack, site.host_path)
                        .get(&site.set)
                        .is_none_or(|selected| *selected == site.variant)
                });
            if !ancestors_selected {
                continue;
            }
            for (child_set, child_variant) in &spec.variant_selections {
                selected.entry(*child_set).or_insert(*child_variant);
            }
        }
    }

    selected
}

/// Returns the variant selections of every variant host enclosing
/// `remote_path` (the prim itself and each ancestor), keyed by host path in
/// `remote_stack`'s namespace, as decided by the composition in progress.
///
/// Inside the arc's namespace (`arc_target` and below) each host maps to a
/// destination prim. Its selections are read strongest-first from that
/// prim's index as composed so far, which already holds every stronger site
/// (the referencing prim and any layers referenced in between), and fall
/// back to the selections forwarded from the stage and target stacks. Hosts
/// above `arc_target` lie outside the arc, so only the target layer stack
/// selects their variants.
///
/// `cache` memoizes hosts within one arc expansion: every host maps to one
/// destination there.
///
/// Spec: AOUSD Core §10.5 (the strongest selection wins across arcs).
/// OpenUSD searches the prim index built so far in strength order
/// (`pxr/usd/pcp/primIndex.cpp`, `_ComposeVariantSelection`).
fn enclosing_variant_selections(
    store: &dyn LayerStore,
    out: &HashMap<PathId, PrimIndex>,
    stage_stack: &LayerStack,
    remote_stack: &LayerStack,
    arc_target: PathId,
    remote_path: PathId,
    dest_path: PathId,
    cache: &mut HashMap<PathId, HashMap<TokenId, TokenId>>,
) -> HashMap<PathId, HashMap<TokenId, TokenId>> {
    let paths = store.paths();
    let parent_of = |id: PathId| {
        paths
            .resolve(id)
            .parent()
            .and_then(|parent| paths.lookup(&parent))
    };
    let mut enclosing = HashMap::new();
    let mut remote = Some(remote_path);
    let mut dest = Some(dest_path);
    while let Some(host) = remote {
        let selections = cache
            .entry(host)
            .or_insert_with(|| match dest {
                Some(dest_host) => {
                    let mut selections = out
                        .get(&dest_host)
                        .map(|index| {
                            let mut sources = index.sources.clone();
                            sources.sort_by(|a, b| index.graph.cmp_keys(a, b));
                            let so_far = PrimIndex {
                                sources,
                                ..PrimIndex::default()
                            };
                            strength_ordered_variant_selections(store, &so_far)
                        })
                        .unwrap_or_default();
                    for (set, variant) in resolve_forwarded_variant_selections(
                        store,
                        stage_stack,
                        dest_host,
                        remote_stack,
                        host,
                    ) {
                        selections.entry(set).or_insert(variant);
                    }
                    selections
                }
                None => resolve_full_variant_selections(store, remote_stack, host),
            })
            .clone();
        enclosing.insert(host, selections);
        dest = if host == arc_target {
            None
        } else {
            dest.and_then(parent_of)
        };
        remote = parent_of(host);
    }
    enclosing
}

/// The arcs authored for one prim of an arc's target namespace, admitted for
/// the variant selections in force at the composed destination.
///
/// This is the single place that decides which arcs nested inside another arc
/// are followed, whichever outer arc (reference, payload, inherit or
/// specialize) brought the content in.
///
/// Each arc comes with the variant branches of the prim that author it,
/// outermost first (see [`ArcAuthoring::sites`]); its node goes beneath theirs.
#[derive(Debug, Default)]
struct AdmittedArcs {
    inherits: Vec<(PathId, Vec<VariantSelectionSite>)>,
    specializes: Vec<(PathId, Vec<VariantSelectionSite>)>,
    references: Vec<(Reference, Vec<VariantSelectionSite>)>,
    payloads: Vec<(Reference, Vec<VariantSelectionSite>)>,
}

/// Resolves the arcs authored for `remote_path` in `data_stack` that apply
/// when it is composed as `dest_path` through an arc targeting `arc_target`.
///
/// Arcs on the prim's own specs, on its own selected variant branches, and
/// authored for it inside its parent's selected branches are all included.
/// Every enclosing variant host's selection comes from
/// [`enclosing_variant_selections`], so a stronger site (the prim bringing the
/// content in, or an arc in between) decides which branch's arcs apply, at any
/// depth below the branch.
///
/// Spec: AOUSD Core §10.5 (arcs inside the selected variant only); OpenUSD
/// adds arcs only beneath the selected variant node, choosing the selection by
/// searching the prim index built so far (`pxr/usd/pcp/primIndex.cpp`).
fn admitted_arcs(
    store: &dyn LayerStore,
    out: &HashMap<PathId, PrimIndex>,
    stage_stack: &LayerStack,
    data_stack: &LayerStack,
    arc_target: PathId,
    remote_path: PathId,
    dest_path: PathId,
    cache: &mut HashMap<PathId, HashMap<TokenId, TokenId>>,
    anchor: LayerId,
) -> AdmittedArcs {
    let enclosing = enclosing_variant_selections(
        store,
        out,
        stage_stack,
        data_stack,
        arc_target,
        remote_path,
        dest_path,
        cache,
    );
    let selections = enclosing.get(&remote_path).cloned().unwrap_or_default();
    let parent_selections = store
        .paths()
        .resolve(remote_path)
        .parent()
        .and_then(|parent| store.paths().lookup(&parent))
        .and_then(|parent| enclosing.get(&parent).cloned())
        .unwrap_or_default();
    let scope = SelectionScope::Composed(&enclosing);

    let mut references =
        resolve_direct_references_for_prim(store, data_stack, remote_path, scope, anchor);
    references.extend(resolve_variant_references_in(
        store,
        data_stack,
        remote_path,
        &selections,
        &parent_selections,
        scope,
        anchor,
    ));
    let mut payloads = resolve_payloads_for_prim_in(
        store,
        data_stack,
        remote_path,
        &parent_selections,
        scope,
        anchor,
    );
    payloads.extend(resolve_branch_payloads_in(
        store,
        data_stack,
        remote_path,
        &selections,
        scope,
        anchor,
    ));
    let inherits = resolve_inherits_for_prim_in(
        store,
        data_stack,
        remote_path,
        &selections,
        &parent_selections,
        scope,
    );
    let specializes = resolve_specializes_for_prim_in(
        store,
        data_stack,
        remote_path,
        &selections,
        &parent_selections,
        scope,
    );
    let authoring = ArcAuthoring {
        store,
        stack: data_stack,
        prim: remote_path,
        selections: &selections,
        scope,
    };
    AdmittedArcs {
        inherits: inherits
            .into_iter()
            .map(|item| {
                let sites = authoring.sites(&item, |spec| &spec.inherits, |b| &b.inherits);
                (item, sites)
            })
            .collect(),
        specializes: specializes
            .into_iter()
            .map(|item| {
                let sites = authoring.sites(&item, |spec| &spec.specializes, |b| &b.specializes);
                (item, sites)
            })
            .collect(),
        references: references
            .into_iter()
            .map(|item| {
                let sites = authoring.reference_sites(
                    &item,
                    |spec| &spec.references,
                    |b| &b.references,
                    anchor,
                );
                (item, sites)
            })
            .collect(),
        payloads: payloads
            .into_iter()
            .map(|item| {
                let sites = authoring.reference_sites(
                    &item,
                    |spec| &spec.payloads,
                    |b| &b.payloads,
                    anchor,
                );
                (item, sites)
            })
            .collect(),
    }
}

/// Returns the source prims of an arc's target namespace that exist only as
/// specs of variant branches that are not selected for their destination.
///
/// `pairs` lists `(source prim, destination used for selection)`. A source
/// prim with no spec, or with any spec outside variant branches, is kept.
/// Otherwise it is rejected unless one of its branch specs is selected at
/// every enclosing host inside the arc's namespace, as decided by
/// [`enclosing_variant_selections`]; hosts with no known selection, and hosts
/// above the arc target, count as selected. Callers drop rejected prims
/// (and their descendants) from the arc's namespace mapping, so neither their
/// specs nor opinions accumulated on the same path elsewhere in the stage
/// (for example a class composed with its own default selection) are carried
/// across the arc.
///
/// Spec: AOUSD Core §10.5 (only the selected variant contributes).
fn unselected_branch_prims(
    store: &dyn LayerStore,
    out: &HashMap<PathId, PrimIndex>,
    stage_stack: &LayerStack,
    data_stack: &LayerStack,
    arc_target: PathId,
    pairs: &[(PathId, PathId)],
    cache: &mut HashMap<PathId, HashMap<TokenId, TokenId>>,
) -> HashSet<PathId> {
    let mut rejected = HashSet::new();
    for &(remote_path, dest_path) in pairs {
        let specs: Vec<&crate::doc::PrimSpec> = data_stack
            .layers
            .iter()
            .filter_map(|id| store.layer(*id))
            .flat_map(|layer| layer.prim_specs(remote_path))
            .collect();
        if specs.is_empty() || specs.iter().any(|spec| spec.outer_variant_sites.is_empty()) {
            continue;
        }
        let enclosing = enclosing_variant_selections(
            store,
            out,
            stage_stack,
            data_stack,
            arc_target,
            remote_path,
            dest_path,
            cache,
        );
        let target_path = store.paths().resolve(arc_target);
        let selected = specs.iter().any(|spec| {
            spec.outer_variant_sites.iter().all(|site| {
                // Hosts above the arc target belong to an enclosing arc's
                // namespace, whose selections this arc cannot see; leave
                // those to `prune_unselected_variant_specs`.
                let inside = store
                    .paths()
                    .resolve(site.host_path)
                    .strip_prefix(target_path)
                    .is_some();
                !inside
                    || enclosing
                        .get(&site.host_path)
                        .and_then(|selections| selections.get(&site.set))
                        .is_none_or(|selected| *selected == site.variant)
            })
        });
        if !selected {
            rejected.insert(remote_path);
        }
    }
    rejected
}

/// Returns `true` when `path` is one of `roots` or lies beneath one.
fn is_at_or_under(store: &dyn LayerStore, path: PathId, roots: &HashSet<PathId>) -> bool {
    if roots.is_empty() {
        return false;
    }
    let mut cursor = Some(store.paths().resolve(path).clone());
    while let Some(current) = cursor {
        if store
            .paths()
            .lookup(&current)
            .is_some_and(|id| roots.contains(&id))
        {
            return true;
        }
        cursor = current.parent();
    }
    false
}

fn resolve_forwarded_variant_selections(
    store: &dyn LayerStore,
    stronger_stack: &LayerStack,
    selection_path: PathId,
    weaker_stack: &LayerStack,
    source_path: PathId,
) -> HashMap<TokenId, TokenId> {
    let mut selections = resolve_full_variant_selections(store, stronger_stack, selection_path);
    for (set, variant) in resolve_full_variant_selections(store, weaker_stack, source_path) {
        selections.entry(set).or_insert(variant);
    }
    selections
}

/// The node in `out[path]`'s graph of the local variant branches `sites`,
/// outermost first: the selected branches of the prim's own variant sets, or
/// of an ancestor's variant sets that author a spec for the prim. Each branch
/// is a node beneath the node of the branch enclosing it, or the root.
///
/// Spec: AOUSD Core §10.3.2.5 (variants), §10.4 (LIVERPS: local before
/// variants). OpenUSD: `_EvalNodeVariantSets` in `pxr/usd/pcp/primIndex.cpp`.
fn local_variant_node(
    store: &mut dyn LayerStore,
    out: &mut HashMap<PathId, PrimIndex>,
    path: PathId,
    sites: &[VariantSelectionSite],
) -> NodeId {
    let layer_stack = root_layer_stack(out, path);
    let mut cursor = PathCursor::root(path);
    intern_steps(
        store,
        out,
        path,
        &local_variant_steps(layer_stack, sites),
        &mut cursor,
    );
    cursor.node
}

/// The layer stack of the root node of `out[path]`'s graph: the stage's.
fn root_layer_stack(out: &HashMap<PathId, PrimIndex>, path: PathId) -> LayerId {
    out[&path]
        .graph
        .node(NodeId::ROOT)
        .expect("a prim graph has a root")
        .layer_stack()
}

fn add_local_and_variant_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    mut deps: Option<&mut DependencyBuilder>,
) {
    for path in paths.iter().copied() {
        let selections = resolve_full_variant_selections(store, local_stack, path);

        for (layer_strength_idx, layer_id) in local_stack.layers.iter().copied().enumerate() {
            let Some(layer) = store.layer(layer_id).cloned() else {
                continue;
            };
            for spec in layer.prim_specs(path) {
                if let Some(d) = deps.as_deref_mut() {
                    d.add_layer_opinion(layer_id, path);
                }

                let accumulated_offset = local_stack.offset_at(layer_strength_idx);
                let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
                // A spec reached through variant branches (see
                // `PrimSpec::outer_variant_sites`) holds variant opinions, which are
                // weaker than local opinions from every layer of the stack.
                //
                // Spec: AOUSD Core §10.4 (LIVERPS: local before variants).
                let spec_path = prim_spec_path(store, path, &spec.outer_variant_sites);
                let node = if spec.outer_variant_sites.is_empty() {
                    NodeId::ROOT
                } else {
                    local_variant_node(store, out, path, &spec.outer_variant_sites)
                };
                out.get_mut(&path)
                    .expect("path exists")
                    .add_source(OpinionKey {
                        node,
                        layer_strength,
                        layer_id,
                        lookup_path: path,
                        spec_path: spec_path.clone(),
                    });

                for entry in composed_entries(&spec.fields, &spec.properties) {
                    let key = OpinionKey {
                        node,
                        layer_strength,
                        layer_id,
                        lookup_path: path,
                        spec_path: property_spec_path(
                            store,
                            path,
                            &spec.outer_variant_sites,
                            entry.name(),
                        ),
                    };
                    let index = out.get_mut(&path).expect("path exists");
                    if let Some(property_type) = entry.property_type() {
                        index.add_property_type(entry.name(), key.clone(), property_type.clone());
                    }
                    index.add_opinion(Opinion {
                        key,
                        field: entry.name(),
                        value: entry.value(),
                        layer_offset: accumulated_offset,
                    });
                }

                if !spec.authored_children.is_empty() {
                    authored_children_out.entry(path).or_default().push((
                        OpinionKey {
                            node,
                            layer_strength,
                            layer_id,
                            lookup_path: path,
                            spec_path: spec_path.clone(),
                        },
                        spec.authored_children.clone(),
                    ));
                }

                if let Some(order) = &spec.prim_order {
                    prim_order_out.entry(path).or_default().push((
                        OpinionKey {
                            node,
                            layer_strength,
                            layer_id,
                            lookup_path: path,
                            spec_path: spec_path.clone(),
                        },
                        order.clone(),
                    ));
                }

                for (set, selected_variant) in &selections {
                    let Some(set_spec) = spec.variant_sets.get(set) else {
                        continue;
                    };
                    let Some(variant_spec) = set_spec.variants.get(selected_variant) else {
                        continue;
                    };
                    let branch_selections = combined_variant_sites(
                        &variant_spec.outer_variant_sites,
                        VariantSelectionSite {
                            host_path: path,
                            set: *set,
                            variant: *selected_variant,
                        },
                    );

                    let branch_path = variant_spec_path(store, path, &branch_selections);
                    let variant_node = local_variant_node(store, out, path, &branch_selections);
                    out.get_mut(&path)
                        .expect("path exists")
                        .add_source(OpinionKey {
                            node: variant_node,
                            layer_strength,
                            layer_id,
                            lookup_path: path,
                            spec_path: branch_path,
                        });

                    for entry in composed_entries(&variant_spec.fields, &variant_spec.properties) {
                        let key = OpinionKey {
                            node: variant_node,
                            layer_strength,
                            layer_id,
                            lookup_path: path,
                            spec_path: variant_property_spec_path(
                                store,
                                path,
                                &branch_selections,
                                entry.name(),
                            ),
                        };
                        let index = out.get_mut(&path).expect("path exists");
                        if let Some(property_type) = entry.property_type() {
                            index.add_property_type(
                                entry.name(),
                                key.clone(),
                                property_type.clone(),
                            );
                        }
                        index.add_opinion(Opinion {
                            key,
                            field: entry.name(),
                            value: entry.value(),
                            layer_offset: accumulated_offset,
                        });
                    }
                }
            }
        }
    }
}

/// Resolves the prim a reference or payload (`arc`) followed for
/// `dest_root` targets (see [`Reference::target_path`]).
///
/// An arc whose asset path could not be resolved
/// ([`Reference::is_unresolved`]) is reported as [`UnresolvedAsset`] and
/// ignored (AOUSD Core §10.3.2.1; OpenUSD `PcpErrorInvalidAssetPath`).
///
/// For a [`ReferenceTarget::DefaultPrim`] target, records that `dest_root`
/// depends on the target layer's `defaultPrim`, and reports
/// [`UnresolvedDefaultPrim`] when it names no prim path (the arc is then
/// ignored) or no layer of the target layer stack has a spec at the path it
/// names (the arc is followed and contributes nothing, as in OpenUSD).
///
/// Spec: AOUSD Core §10.3.2.1 (an omitted prim path assumes the target
/// layer's `defaultPrim`; a reference to a path without specs is a
/// composition error). OpenUSD: `_EvalRefOrPayloadArcs` and
/// `_EvalUnresolvedPrimPathError` in `pxr/usd/pcp/primIndex.cpp`.
fn resolve_arc_target(
    store: &mut dyn LayerStore,
    reference: &Reference,
    dest_root: PathId,
    arc: ArcKind,
    cycles: &mut CycleDetector,
    deps: Option<&mut DependencyBuilder>,
) -> Option<PathId> {
    if reference.is_unresolved() {
        cycles.report(CompositionError::UnresolvedAsset(UnresolvedAsset {
            prim: dest_root,
            arc,
            asset: reference.asset.clone().unwrap_or_default(),
        }));
        return None;
    }
    let target = reference.target_path(store);
    if reference.target != ReferenceTarget::DefaultPrim {
        return target;
    }
    if let Some(d) = deps {
        d.add_default_prim_dependency(reference.layer, dest_root);
    }
    let has_spec = |store: &dyn LayerStore, path: PathId| {
        LayerStack::gather(store, reference.layer)
            .layers
            .iter()
            .filter_map(|id| store.layer(*id))
            .any(|layer| layer.prims.contains_key(&path))
    };
    let unresolved = match target {
        None => Some(None),
        Some(path) if !has_spec(store, path) => Some(Some(path)),
        Some(_) => None,
    };
    if let Some(path) = unresolved {
        cycles.report(CompositionError::UnresolvedDefaultPrim(
            UnresolvedDefaultPrim {
                prim: dest_root,
                arc,
                layer: reference.layer,
                path,
            },
        ));
    }
    target
}

fn add_reference_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Spec: AOUSD Core §10 (references arcs). For v0.1 we expand references
    // recursively so that nested references contribute opinions.
    let mut visited: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_specializes: HashSet<(PathId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        cycles.begin(dest_root);
        // Internal arcs authored in the stage's layer stack target it.
        let anchor = cycles.stage_layer_stack();
        let refs = resolve_references_for_prim(
            store,
            local_stack,
            dest_root,
            SelectionScope::Stack,
            anchor,
        );
        // Also resolve variant child references with full selection chaining.
        let variant_child_refs =
            resolve_variant_child_references(store, local_stack, local_stack, dest_root, anchor);
        let all_refs = refs.into_iter().chain(variant_child_refs);
        let selections = resolve_full_variant_selections(store, local_stack, dest_root);
        for (arc_list_index, reference) in all_refs.enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            // An unresolved target is reported when the arc is followed.
            if let Some(d) = deps.as_deref_mut()
                && let Some(reference_path) = reference.target_path(store)
            {
                d.add_arc(ArcDependency {
                    source: reference_path,
                    target: dest_root,
                    arc_kind: ArcKind::References,
                    layer: reference.layer,
                });
            }
            let sites = ArcAuthoring {
                store,
                stack: local_stack,
                prim: dest_root,
                selections: &selections,
                scope: SelectionScope::Stack,
            }
            .reference_sites(
                &reference,
                |spec| &spec.references,
                |branch| &branch.references,
                anchor,
            );
            let branch = local_variant_steps(root_layer_stack(out, dest_root), &sites);
            add_reference_edge_opinions(
                store,
                local_stack,
                dest_root,
                reference,
                None,
                &[],
                namespace_depth,
                arc_list_index,
                ArcParent::nested(&branch),
                out,
                &mut visited,
                &mut visited_inherits,
                &mut visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }
}

fn add_inherit_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Spec: AOUSD Core §10 (inherits arc).
    let mut visited: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_specializes: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_refs: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        cycles.begin(dest_root);
        let inherits =
            resolve_inherits_for_prim(store, local_stack, dest_root, SelectionScope::Stack);
        let selections = resolve_full_variant_selections(store, local_stack, dest_root);
        for (arc_list_index, inherited_root) in inherits.into_iter().enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            if let Some(d) = deps.as_deref_mut() {
                d.add_arc(ArcDependency {
                    source: inherited_root,
                    target: dest_root,
                    arc_kind: ArcKind::Inherits,
                    layer: local_stack.layers[0],
                });
            }
            let sites = ArcAuthoring {
                store,
                stack: local_stack,
                prim: dest_root,
                selections: &selections,
                scope: SelectionScope::Stack,
            }
            .sites(
                &inherited_root,
                |spec| &spec.inherits,
                |branch| &branch.inherits,
            );
            let branch = local_variant_steps(root_layer_stack(out, dest_root), &sites);
            add_inherit_edge_opinions(
                store,
                local_stack,
                dest_root,
                inherited_root,
                cycles.stage_layer_stack(),
                None,
                &[],
                namespace_depth,
                arc_list_index,
                ArcParent::nested(&branch),
                out,
                &mut visited,
                &mut visited_specializes,
                &mut visited_refs,
                prim_order_out,
                authored_children_out,
                None,
                None,
                LayerOffset::IDENTITY,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }
}

/// The specializes arcs of an opinion introduced by a specializes arc
/// authored at `origin`, inside the specializes nodes `enclosing`.
///
/// Spec: AOUSD Core §10.4.1.
fn specializes_chain(
    enclosing: &[SpecializesOrigin],
    origin: SpecializesOrigin,
) -> Vec<SpecializesOrigin> {
    let mut chain = Vec::with_capacity(enclosing.len() + 1);
    chain.extend_from_slice(enclosing);
    chain.push(origin);
    chain
}

/// What an [`ArcStep`] reaches for a composed prim.
#[derive(Clone, Copy, Debug)]
enum StepTarget {
    /// An arc that maps the namespace at `dest_root` onto `target_root`, so
    /// the node it adds to a prim at or below `dest_root` has the site the
    /// prim's path maps to (AOUSD Core §10.2; OpenUSD
    /// `PcpNodeRef::GetMapToParent`).
    Namespace {
        dest_root: PathId,
        target_root: PathId,
    },
    /// The selected branch of a variant set hosted at the previous step's
    /// site or at one of its namespace ancestors: the node's site is that
    /// site with this selection after those of the variant steps before it
    /// (`/A{v=x}Child`, `/A{v=x}{b=y}`).
    Variant(VariantSelectionSite),
    /// A [`StepTarget::Variant`] of the composed prim's own layer stack. Its
    /// node ranks with the prim's local variant opinions, at the composed
    /// prim's namespace depth, whatever the step's `namespace_depth` and
    /// `strength` (see [`local_variant_strength`]).
    LocalVariant(VariantSelectionSite),
}

impl StepTarget {
    fn is_variant(self) -> bool {
        matches!(self, Self::Variant(_) | Self::LocalVariant(_))
    }
}

/// One arc on the way from a composed prim to the sites an arc expansion
/// reads: the step from a parent node to a child node of the prim's
/// [`PrimIndexGraph`].
#[derive(Clone, Debug)]
struct ArcStep {
    arc_kind: ArcKind,
    layer_stack: LayerId,
    target: StepTarget,
    namespace_depth: u16,
    sibling_index: u16,
    implied: bool,
    /// The strength of the opinions the arc's node holds.
    strength: NodeStrength,
}

/// Where interning an arc path has reached in a composed prim's graph: the
/// node, the prim path of the last namespace step's site and the variant
/// selections below it.
struct PathCursor {
    node: NodeId,
    prim: PathId,
    variants: Vec<VariantSelectionSite>,
}

impl PathCursor {
    /// The root node of the composed prim `dest`.
    fn root(dest: PathId) -> Self {
        Self {
            node: NodeId::ROOT,
            prim: dest,
            variants: Vec::new(),
        }
    }
}

impl ArcStep {
    /// The node this step adds beneath `cursor` for the composed prim `dest`,
    /// moving `cursor` to its site; `None` for a variant hosted outside the
    /// cursor's site, which the step cannot reach.
    fn arc(
        &self,
        store: &mut dyn LayerStore,
        dest: PathId,
        cursor: &mut PathCursor,
    ) -> Option<NodeArc> {
        let (site, namespace_depth, strength) = match self.target {
            StepTarget::Namespace {
                dest_root,
                target_root,
            } => {
                cursor.prim = map_namespace(store, dest, dest_root, target_root);
                cursor.variants.clear();
                (
                    SpecPath::from_prim_path(cursor.prim, store.paths()),
                    self.namespace_depth,
                    self.strength.clone(),
                )
            }
            StepTarget::Variant(site) | StepTarget::LocalVariant(site) => {
                let paths = store.paths();
                let hosted = paths
                    .resolve(cursor.prim)
                    .strip_prefix(paths.resolve(site.host_path))
                    .is_some();
                debug_assert!(hosted, "a variant step is hosted at or above its site");
                if !hosted {
                    return None;
                }
                cursor.variants.push(site);
                let site =
                    SpecPath::from_variant_selection_sites(cursor.prim, &cursor.variants, paths);
                if let StepTarget::LocalVariant(_) = self.target {
                    let depth = u16::try_from(paths.resolve(dest).depth()).unwrap_or(u16::MAX);
                    (site, depth, local_variant_strength(depth))
                } else {
                    (site, self.namespace_depth, self.strength.clone())
                }
            }
        };
        Some(NodeArc {
            arc_kind: self.arc_kind,
            layer_stack: self.layer_stack,
            site,
            namespace_depth,
            sibling_index: self.sibling_index,
            implied: self.implied,
            copied: false,
            strength,
        })
    }
}

/// Maps the composed prim `dest` through an arc from `dest_root` to
/// `target_root`; a prim outside `dest_root` maps to `target_root`.
fn map_namespace(
    store: &mut dyn LayerStore,
    dest: PathId,
    dest_root: PathId,
    target_root: PathId,
) -> PathId {
    if dest == dest_root {
        return target_root;
    }
    let rel = store
        .paths()
        .resolve(dest)
        .strip_prefix(store.paths().resolve(dest_root))
        .map(<[_]>::to_vec);
    match rel {
        Some(rel) => {
            let target = store.paths().resolve(target_root).join(&rel);
            store.paths_mut().intern(target)
        }
        None => target_root,
    }
}

/// Adds the nodes of `steps` beneath `cursor` in the graph of the composed
/// prim `dest`, moving `cursor` to the last one.
///
/// A variant node on the way to a later step is shared with any node of the
/// same branch, whatever its strength: only the last step's node holds the
/// opinions that strength ranks.
fn intern_steps(
    store: &mut dyn LayerStore,
    out: &mut HashMap<PathId, PrimIndex>,
    dest: PathId,
    steps: &[ArcStep],
    cursor: &mut PathCursor,
) {
    for (index, step) in steps.iter().enumerate() {
        let Some(arc) = step.arc(store, dest, cursor) else {
            continue;
        };
        let graph = &mut out.get_mut(&dest).expect("path exists").graph;
        cursor.node = if index + 1 < steps.len() && step.target.is_variant() {
            graph.intern_branch(cursor.node, arc)
        } else {
            graph.intern_child(cursor.node, arc)
        };
    }
}

/// The steps from a composed prim's root node through the selected branches
/// `sites` of the prim's own layer stack, rooted at `layer_stack`.
fn local_variant_steps(layer_stack: LayerId, sites: &[VariantSelectionSite]) -> Vec<ArcStep> {
    sites
        .iter()
        .map(|site| ArcStep {
            arc_kind: ArcKind::Variants,
            layer_stack,
            target: StepTarget::LocalVariant(*site),
            namespace_depth: 0,
            sibling_index: 0,
            implied: false,
            strength: local_variant_strength(0),
        })
        .collect()
}

/// The strength of a composed prim's local variant opinions, at the prim's
/// `namespace_depth`: weaker than its local opinions from every layer of the
/// stack (AOUSD Core §10.4, LIVERPS).
fn local_variant_strength(namespace_depth: u16) -> NodeStrength {
    NodeStrength {
        is_local: false,
        arc_kind: ArcKind::Variants,
        ..NodeStrength::local(namespace_depth)
    }
}

/// The arcs an arc expansion is nested in, outermost first, and whether the
/// expansion is a class arc implied into a stronger layer stack.
#[derive(Clone, Copy, Debug)]
struct ArcParent<'a> {
    steps: &'a [ArcStep],
    implied: bool,
}

impl<'a> ArcParent<'a> {
    /// An arc authored at a site the arcs `steps` reach.
    fn nested(steps: &'a [ArcStep]) -> Self {
        Self {
            steps,
            implied: false,
        }
    }

    /// The same arc, implied into a stronger layer stack.
    ///
    // TODO(graph): ImpliedClasses. OpenUSD adds an implied class node under
    // the node of the stronger layer stack the class is implied into, with
    // the class node it is implied from as its origin
    // (`pxr/usd/pcp/primIndex.cpp`, `_EvalImpliedClasses`); this places it
    // beside the class node it is implied from, and ranks it with that node.
    fn implied(self) -> Self {
        Self {
            implied: true,
            ..self
        }
    }
}

/// The nodes one arc expansion adds to the graph of each prim it composes
/// into.
///
/// Spec: AOUSD Core §10.4 (an arc's target is ranked beneath the site that
/// authors it). OpenUSD: `_AddArc` in `pxr/usd/pcp/primIndex.cpp`.
struct ArcNodes {
    /// The arc path from the composed prim to this arc, this arc last.
    path: Vec<ArcStep>,
    /// The node of this arc in each destination prim's graph, and the prim
    /// path of its site.
    nodes: HashMap<PathId, (NodeId, PathId)>,
}

impl ArcNodes {
    /// The nodes of the arc `step` authored at a site `parent` reaches.
    fn new(parent: ArcParent<'_>, step: ArcStep) -> Self {
        let mut path = Vec::with_capacity(parent.steps.len() + 1);
        path.extend_from_slice(parent.steps);
        path.push(ArcStep {
            implied: parent.implied,
            ..step
        });
        Self {
            path,
            nodes: HashMap::new(),
        }
    }

    /// The arc path to this arc's selected branches `sites`, for arcs
    /// authored inside them (see [`Self::variant_node`]); to this arc for
    /// arcs authored outside every branch.
    fn branch_path(
        &self,
        sites: &[VariantSelectionSite],
        nested_arc_kind: Option<ArcKind>,
    ) -> Cow<'_, [ArcStep]> {
        if sites.is_empty() {
            return Cow::Borrowed(&self.path);
        }
        let mut path = self.path.clone();
        path.extend(self.variant_steps(sites, nested_arc_kind));
        Cow::Owned(path)
    }

    fn step(&self) -> &ArcStep {
        self.path.last().expect("an arc path ends with its arc")
    }

    /// The arc's node in the graph of the composed prim `dest`, added with
    /// the nodes of the arcs it is nested in.
    fn node(
        &mut self,
        store: &mut dyn LayerStore,
        out: &mut HashMap<PathId, PrimIndex>,
        dest: PathId,
    ) -> NodeId {
        self.cursor(store, out, dest).node
    }

    fn cursor(
        &mut self,
        store: &mut dyn LayerStore,
        out: &mut HashMap<PathId, PrimIndex>,
        dest: PathId,
    ) -> PathCursor {
        if let Some(&(node, prim)) = self.nodes.get(&dest) {
            return PathCursor {
                node,
                prim,
                variants: Vec::new(),
            };
        }
        let mut cursor = PathCursor::root(dest);
        intern_steps(store, out, dest, &self.path, &mut cursor);
        self.nodes.insert(dest, (cursor.node, cursor.prim));
        cursor
    }

    /// The variant steps through the selected branches `sites` of this arc's
    /// target, whose nodes rank with `nested_arc_kind`.
    fn variant_steps(
        &self,
        sites: &[VariantSelectionSite],
        nested_arc_kind: Option<ArcKind>,
    ) -> impl Iterator<Item = ArcStep> {
        let step = self.step();
        let strength = NodeStrength {
            nested_arc_kind,
            ..step.strength.clone()
        };
        let (layer_stack, namespace_depth) = (step.layer_stack, step.namespace_depth);
        sites.iter().map(move |site| ArcStep {
            arc_kind: ArcKind::Variants,
            layer_stack,
            target: StepTarget::Variant(*site),
            namespace_depth,
            sibling_index: 0,
            implied: false,
            strength: strength.clone(),
        })
    }

    /// The node of the selected variant branches `sites` of the arc's
    /// target, outermost first, in the graph of the composed prim `dest`,
    /// whose opinions rank with `nested_arc_kind`. Each branch is a node
    /// beneath the node of the branch enclosing it, or the arc's node.
    ///
    /// Spec: AOUSD Core §10.3.2.5 (variants). OpenUSD adds the branch as a
    /// variant node under the node hosting the variant set
    /// (`_EvalNodeVariantSets` in `pxr/usd/pcp/primIndex.cpp`).
    fn variant_node(
        &mut self,
        store: &mut dyn LayerStore,
        out: &mut HashMap<PathId, PrimIndex>,
        dest: PathId,
        sites: &[VariantSelectionSite],
        nested_arc_kind: Option<ArcKind>,
    ) -> NodeId {
        let mut cursor = self.cursor(store, out, dest);
        let steps: Vec<ArcStep> = self.variant_steps(sites, nested_arc_kind).collect();
        intern_steps(store, out, dest, &steps, &mut cursor);
        cursor.node
    }

    /// The node of a spec of the arc's target authored inside the branches
    /// `sites` (its [`crate::doc::PrimSpec::outer_variant_sites`]), in the graph of the
    /// composed prim `dest`: the arc's node for a spec outside every branch.
    /// Its opinions rank with the arc's own.
    fn spec_node(
        &mut self,
        store: &mut dyn LayerStore,
        out: &mut HashMap<PathId, PrimIndex>,
        dest: PathId,
        sites: &[VariantSelectionSite],
    ) -> NodeId {
        let nested_arc_kind = self.step().strength.nested_arc_kind;
        self.variant_node(store, out, dest, sites, nested_arc_kind)
    }
}

/// The strength of an arc's node: an arc no specializes arc encloses is
/// ranked `(arc_kind, nested_arc_kind)`; see [`NodeStrength`].
fn arc_strength(
    specializes: &[SpecializesOrigin],
    arc_kind: ArcKind,
    nested_arc_kind: Option<ArcKind>,
    namespace_depth: u16,
    arc_list_index: u16,
) -> NodeStrength {
    NodeStrength {
        is_local: false,
        specializes: specializes.to_vec(),
        arc_kind,
        nested_arc_kind,
        namespace_depth,
        authored: true,
        arc_list_index,
    }
}

/// An opinion of a class arc's target, held until the sources of its layer
/// are added: the destination prim, the source prim, the spec path, the
/// field, the value, the declared property type and the node.
type PendingOpinion = (
    PathId,
    PathId,
    SpecPath,
    TokenId,
    OpinionValue,
    Option<PropertyType>,
    NodeId,
);

/// How an arc copies the opinions already composed for the stage prim at
/// its target path to its destination prim.
///
/// The arc's own expansion reads its target's specs and follows the arcs
/// authored there, which builds the target's subgraph once beneath the
/// arc's node, as OpenUSD does (`_AddArc` in `pxr/usd/pcp/primIndex.cpp`).
/// The copy grafts that stage prim's graph beneath the arc's node as well,
/// and [`drop_reached_copies`] drops every copied registration of a site the
/// expansion reaches, so the copy keeps only what the expansion misses.
///
// TODO(graph): AncestralArcs, ImpliedClasses, NestedArcDepth. The expansion
// misses the arcs authored on a subroot target's namespace ancestors and the
// classes implied into the layer stacks between a class arc and the root,
// which the copy supplies from the stage prim's index; and where a flat
// strength key ranks a nested arc's target above its own node, a copied
// registration that ranks a site more strongly keeps the resolved order.
// Retire the copy once the graph covers these.
struct Forwarding<'a> {
    /// Specializes nodes the forwarding arc is expanded in.
    specializes: &'a [SpecializesOrigin],
    /// Arc kind the forwarding arc ranks its opinions under.
    arc_kind: ArcKind,
    /// Nested arc kind the forwarding arc gives its own opinions, if any;
    /// otherwise forwarded opinions nest under their own arc kind.
    nested_arc_kind: Option<ArcKind>,
    namespace_depth: u16,
    arc_list_index: u16,
}

impl Forwarding<'_> {
    /// The strength of `source`, a node of the target prim `remote`,
    /// forwarded to `dest`.
    ///
    /// A node no specializes arc introduces is ranked inside the forwarding
    /// arc. A node of a specializes node keeps its rank within that node;
    /// the node's placeholder now sits under the forwarding arc, so its first
    /// origin is re-ranked there and its namespace depth follows the
    /// namespace mapping from `remote` to `dest` (AOUSD Core §10.4.1; OpenUSD
    /// `_EvalImpliedSpecializes` in `pxr/usd/pcp/primIndex.cpp`).
    fn strength(
        &self,
        store: &dyn LayerStore,
        source: &NodeStrength,
        remote: PathId,
        dest: PathId,
    ) -> NodeStrength {
        let nest = |kind: ArcKind| self.nested_arc_kind.or(Some(kind));
        let Some((first, rest)) = source.specializes.split_first() else {
            return arc_strength(
                self.specializes,
                self.arc_kind,
                nest(source.arc_kind),
                self.namespace_depth,
                self.arc_list_index,
            );
        };
        let depth = |path: PathId| i64::try_from(store.paths().resolve(path).depth()).unwrap_or(0);
        let shifted = i64::from(first.namespace_depth) + depth(dest) - depth(remote);
        let mut specializes = specializes_chain(
            self.specializes,
            SpecializesOrigin {
                namespace_depth: u16::try_from(shifted.max(0)).unwrap_or(u16::MAX),
                arc_kind: self.arc_kind,
                nested_arc_kind: nest(first.arc_kind),
                arc_list_index: self.arc_list_index,
                ..*first
            },
        );
        specializes.extend_from_slice(rest);
        NodeStrength {
            specializes,
            ..source.clone()
        }
    }

    /// Copies into each destination of `mapping` the sources and opinions
    /// already composed for the stage prim at its source path, beneath the
    /// forwarding arc's node. [`drop_reached_copies`] later drops the copies
    /// of sites the arc's own expansion reaches.
    fn copy_composed(
        &self,
        store: &mut dyn LayerStore,
        out: &mut HashMap<PathId, PrimIndex>,
        cycles: &CycleDetector,
        nodes: &mut ArcNodes,
        mapping: &[(PathId, PathId)],
        provenance_remap: Option<(PathId, PathId)>,
    ) {
        for &(remote, dest) in mapping {
            let Some(src_index) = out.get(&remote).cloned() else {
                continue;
            };
            // Local opinions of the target are the expansion's own, and a
            // copy must not carry `dest` back into its own namespace.
            let copies = |key: &OpinionKey, store: &dyn LayerStore| {
                src_index.graph.strength(key.node).arc_kind != ArcKind::Local
                    && !cycles.copies_cycle(
                        store.paths(),
                        dest,
                        key.layer_id,
                        key.spec_path.prim_path(),
                    )
            };
            let mut graft = self.graft(&src_index.graph, remote, dest);
            for source in &src_index.sources {
                if !copies(source, store) {
                    continue;
                }
                let key = OpinionKey {
                    node: graft.node(store, out, nodes, source.node),
                    spec_path: normalize_forwarded_spec_path(
                        store,
                        &source.spec_path,
                        provenance_remap,
                    ),
                    ..source.clone()
                };
                out.get_mut(&dest).expect("path exists").add_source(key);
            }
            for opinion in src_index.opinions_by_field.values().flatten() {
                if !copies(&opinion.key, store) {
                    continue;
                }
                let key = OpinionKey {
                    node: graft.node(store, out, nodes, opinion.key.node),
                    spec_path: normalize_forwarded_spec_path(
                        store,
                        &opinion.key.spec_path,
                        provenance_remap,
                    ),
                    ..opinion.key.clone()
                };
                out.get_mut(&dest)
                    .expect("path exists")
                    .add_opinion(Opinion {
                        key,
                        field: opinion.field,
                        value: opinion.value.clone(),
                        layer_offset: opinion.layer_offset,
                    });
            }
        }
    }

    /// Grafts the target prim `remote`'s graph `source` beneath the
    /// forwarding arc's node in the graph of `dest`.
    fn graft<'g>(&'g self, source: &'g PrimIndexGraph, remote: PathId, dest: PathId) -> Graft<'g> {
        Graft {
            forwarding: self,
            source,
            remote,
            dest,
            under: None,
            copies: HashMap::new(),
        }
    }
}

/// The copy of one target prim's graph beneath a forwarding arc's node.
struct Graft<'a> {
    forwarding: &'a Forwarding<'a>,
    source: &'a PrimIndexGraph,
    remote: PathId,
    dest: PathId,
    /// The forwarding arc's node in the graph of `dest`, once added.
    under: Option<NodeId>,
    /// The copy of each source node already grafted.
    copies: HashMap<NodeId, NodeId>,
}

impl Graft<'_> {
    /// The copy of the source node `node` in `out[dest]`'s graph, grafted
    /// with its ancestors below the source root beneath the forwarding arc's
    /// node (`nodes`); the source root's own copy is a child of that node.
    fn node(
        &mut self,
        store: &mut dyn LayerStore,
        out: &mut HashMap<PathId, PrimIndex>,
        nodes: &mut ArcNodes,
        node: NodeId,
    ) -> NodeId {
        if let Some(copy) = self.copies.get(&node) {
            return *copy;
        }
        let graph = self.source;
        let source = graph.node(node).expect("source node exists");
        let parent = match source.parent() {
            Some(NodeId::ROOT) | None => match self.under {
                Some(under) => under,
                None => {
                    let under = nodes.node(store, out, self.dest);
                    self.under = Some(under);
                    under
                }
            },
            Some(parent) => self.node(store, out, nodes, parent),
        };
        let arc = NodeArc {
            copied: true,
            strength: self
                .forwarding
                .strength(store, &source.arc.strength, self.remote, self.dest),
            ..source.arc.clone()
        };
        let copy = out
            .get_mut(&self.dest)
            .expect("path exists")
            .graph
            .intern_child(parent, arc);
        self.copies.insert(node, copy);
        copy
    }
}

/// Keeps one registration of each class arc site: drops from `pending` the
/// sources of a class arc whose site its destination already registers at
/// least as strongly, returning them as `(destination, layer, site)`, and
/// drops from the destination the weaker class arc registrations of a site
/// that `pending` registers more strongly, with their opinions and the
/// registrations of the same layer beneath their node (the selected
/// variant branches of the replaced site), which the stronger registration
/// brings again.
///
/// A class arc adds no node for a site the prim index already uses: an
/// implied class that an arc also reaches directly is one site (AOUSD Core
/// §10.4.2.4; OpenUSD's `skipDuplicateNodes` for class-based arcs in
/// `pxr/usd/pcp/primIndex.cpp`). OpenUSD adds arcs in strength order, so
/// the registration it keeps is the strongest one; expansion order here is
/// not strength order, so strength decides which registration stays.
fn retain_new_class_sites(
    out: &mut HashMap<PathId, PrimIndex>,
    pending: &mut Vec<(PathId, OpinionKey)>,
) -> HashSet<(PathId, LayerId, SpecPath)> {
    let mut redundant = HashSet::new();
    let mut weaker: HashSet<(PathId, NodeId, LayerId, SpecPath)> = HashSet::new();
    pending.retain(|(dest, key)| {
        let index = &out[dest];
        let graph = &index.graph;
        let mut registered = false;
        for known in &index.sources {
            if known.layer_id != key.layer_id || known.spec_path != key.spec_path {
                continue;
            }
            let class_based = graph.node(known.node).is_some_and(|node| {
                matches!(node.arc_kind(), ArcKind::Inherits | ArcKind::Specializes)
            });
            let stronger = graph
                .strength(key.node)
                .cmp_strongest_first(graph.strength(known.node))
                .is_lt();
            if stronger && class_based {
                weaker.insert((*dest, known.node, known.layer_id, known.spec_path.clone()));
            } else if !stronger {
                registered = true;
            }
        }
        if registered {
            redundant.insert((*dest, key.layer_id, key.spec_path.clone()));
        }
        !registered
    });
    // A weaker registration is dropped only for a site `pending` keeps.
    weaker.retain(|(dest, _, layer, site)| !redundant.contains(&(*dest, *layer, site.clone())));
    let dests: HashSet<PathId> = weaker.iter().map(|(dest, ..)| *dest).collect();
    for dest in dests {
        let replaced: HashSet<(NodeId, LayerId)> = weaker
            .iter()
            .filter(|(weaker_dest, ..)| *weaker_dest == dest)
            .map(|(_, node, layer, _)| (*node, *layer))
            .collect();
        let kept: HashSet<NodeId> = pending
            .iter()
            .filter(|(pending_dest, _)| *pending_dest == dest)
            .map(|(_, key)| key.node)
            .collect();
        out.get_mut(&dest)
            .expect("path exists")
            .retain_keys(|graph, key| {
                if weaker.contains(&(dest, key.node, key.layer_id, key.spec_path.prim_spec())) {
                    return false;
                }
                // A registration of the same layer beneath a replaced node,
                // and not beneath a kept one, goes with that node.
                let mut beneath_replaced = false;
                let mut cursor = Some(key.node);
                while let Some(node) = cursor {
                    if kept.contains(&node) {
                        return true;
                    }
                    if node != key.node && replaced.contains(&(node, key.layer_id)) {
                        beneath_replaced = true;
                    }
                    cursor = graph.node(node).and_then(PrimNode::parent);
                }
                !beneath_replaced
            });
    }
    redundant
}

fn add_inherit_edge_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    dest_root: PathId,
    inherited_root: PathId,
    // Root layer of the layer stack the inherit is authored in.
    arc_stack: LayerId,
    outer_arc_kind: Option<ArcKind>,
    // Specializes nodes the inherit is expanded in.
    specializes: &[SpecializesOrigin],
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, PathId)>,
    visited_specializes: &mut HashSet<(PathId, PathId)>,
    visited_refs: &mut HashSet<(PathId, LayerId, PathId)>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    // Optional reference namespace for remapping field values (dest, src).
    ref_remap: Option<(&crate::path::Path, &crate::path::Path)>,
    // Optional source-namespace remap for provenance spec paths (dest, src).
    provenance_remap: Option<(PathId, PathId)>,
    // Accumulated offset from outer arcs (references/payloads). Composed with
    // each layer's sublayer offset to produce the final opinion offset.
    base_offset: LayerOffset,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // An arc that would close a cycle is a composition error and is skipped
    // (AOUSD Core §10.6; OpenUSD `_CheckForCycle`).
    if cycles.closes_cycle(
        store.paths_mut(),
        dest_root,
        arc_stack,
        inherited_root,
        ArcKind::Inherits,
    ) {
        return;
    }
    if !visited.insert((dest_root, inherited_root)) {
        return;
    }
    cycles.enter(arc_stack, inherited_root, dest_root, ArcKind::Inherits);

    let base_path = store.paths().resolve(dest_root).clone();
    let inherited_path = store.paths().resolve(inherited_root).clone();

    let mut remote_paths: Vec<PathId> = local_stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| layer.prims.keys().copied())
        .collect();
    remote_paths.sort_by(|a, b| {
        store
            .paths()
            .resolve(*a)
            .cmp_with_tokens(store.paths().resolve(*b), store.tokens())
    });
    remote_paths.dedup();

    let mut mapping: Vec<(PathId, PathId)> = Vec::new();
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&inherited_path) else {
                continue;
            };
            rel.to_vec()
        };
        let dest_path_id = store.paths_mut().intern(base_path.join(&rel));
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }

    let mut host_selection_cache = HashMap::new();
    // Branch-only source prims whose branch is not selected for the
    // destination take no part in this arc.
    let unselected = {
        let pairs: Vec<(PathId, PathId)> = mapping.clone();
        unselected_branch_prims(
            store,
            out,
            local_stack,
            local_stack,
            inherited_root,
            &pairs,
            &mut host_selection_cache,
        )
    };
    mapping.retain(|(remote, _)| !is_at_or_under(store, *remote, &unselected));

    let (arc_kind, nested_arc_kind) = match outer_arc_kind {
        Some(outer) => (outer, Some(ArcKind::Inherits)),
        None => (ArcKind::Inherits, None),
    };
    let mut nodes = ArcNodes::new(
        parent,
        ArcStep {
            arc_kind: ArcKind::Inherits,
            layer_stack: arc_stack,
            target: StepTarget::Namespace {
                dest_root,
                target_root: inherited_root,
            },
            namespace_depth,
            sibling_index: arc_list_index,
            implied: false,
            strength: arc_strength(
                specializes,
                arc_kind,
                nested_arc_kind,
                namespace_depth,
                arc_list_index,
            ),
        },
    );

    for (layer_strength_idx, layer_id) in local_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let layer_offset = base_offset.compose(local_stack.offset_at(layer_strength_idx));
        let mut pending: Vec<PendingOpinion> = Vec::new();
        let mut pending_sources = Vec::new();
        {
            let Some(layer) = store.layer(layer_id).cloned() else {
                continue;
            };

            for (remote_path_id, dest_path_id) in &mapping {
                for spec in layer.prim_specs(*remote_path_id) {
                    if let Some(d) = deps.as_deref_mut() {
                        d.add_layer_opinion(layer_id, *dest_path_id);
                    }
                    let node =
                        nodes.spec_node(store, out, *dest_path_id, &spec.outer_variant_sites);
                    if let Some(order) = &spec.prim_order {
                        prim_order_out.entry(*dest_path_id).or_default().push((
                            OpinionKey {
                                node,
                                layer_strength,
                                layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_prim_spec_path(
                                    store,
                                    *remote_path_id,
                                    &spec.outer_variant_sites,
                                    provenance_remap,
                                ),
                            },
                            order.clone(),
                        ));
                    }

                    if !spec.authored_children.is_empty() {
                        authored_children_out
                            .entry(*dest_path_id)
                            .or_default()
                            .push((
                                OpinionKey {
                                    node,
                                    layer_strength,
                                    layer_id,
                                    lookup_path: *remote_path_id,
                                    spec_path: normalized_prim_spec_path(
                                        store,
                                        *remote_path_id,
                                        &spec.outer_variant_sites,
                                        provenance_remap,
                                    ),
                                },
                                spec.authored_children.clone(),
                            ));
                    }

                    pending_sources.push((
                        *dest_path_id,
                        OpinionKey {
                            node,
                            layer_strength,
                            layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_prim_spec_path(
                                store,
                                *remote_path_id,
                                &spec.outer_variant_sites,
                                provenance_remap,
                            ),
                        },
                    ));
                    for entry in composed_entries(&spec.fields, &spec.properties) {
                        pending.push((
                            *dest_path_id,
                            *remote_path_id,
                            normalized_property_spec_path(
                                store,
                                *remote_path_id,
                                &spec.outer_variant_sites,
                                entry.name(),
                                provenance_remap,
                            ),
                            entry.name(),
                            entry.value(),
                            entry.property_type().cloned(),
                            node,
                        ));
                    }

                    // Forward variant opinions from selected variants through inherits.
                    let inherits_selections = resolve_forwarded_variant_selections(
                        store,
                        local_stack,
                        *dest_path_id,
                        local_stack,
                        *remote_path_id,
                    );
                    for (set, selected) in &inherits_selections {
                        if let Some(set_spec) = spec.variant_sets.get(set)
                            && let Some(variant_spec) = set_spec.variants.get(selected)
                        {
                            let branch_selections = combined_variant_sites(
                                &variant_spec.outer_variant_sites,
                                VariantSelectionSite {
                                    host_path: *remote_path_id,
                                    set: *set,
                                    variant: *selected,
                                },
                            );
                            let branch_path = normalized_variant_spec_path(
                                store,
                                *remote_path_id,
                                &branch_selections,
                                provenance_remap,
                            );
                            let variant_node = nodes.variant_node(
                                store,
                                out,
                                *dest_path_id,
                                &branch_selections,
                                nested_arc_kind.or(Some(ArcKind::Variants)),
                            );
                            // TODO(graph): NestedArcDepth. The branch's
                            // opinions rank with the inherit's own opinions,
                            // not with the branch's variant node, which
                            // OpenUSD ranks beneath the inherit's site
                            // (`PcpCompareSiblingNodeStrength`).
                            let branch_opinions_node = nodes.variant_node(
                                store,
                                out,
                                *dest_path_id,
                                &branch_selections,
                                nested_arc_kind,
                            );
                            pending_sources.push((
                                *dest_path_id,
                                OpinionKey {
                                    node: variant_node,
                                    layer_strength,
                                    layer_id,
                                    lookup_path: *remote_path_id,
                                    spec_path: branch_path,
                                },
                            ));
                            for entry in
                                composed_entries(&variant_spec.fields, &variant_spec.properties)
                            {
                                pending.push((
                                    *dest_path_id,
                                    *remote_path_id,
                                    normalized_variant_property_spec_path(
                                        store,
                                        *remote_path_id,
                                        &branch_selections,
                                        entry.name(),
                                        provenance_remap,
                                    ),
                                    entry.name(),
                                    entry.value(),
                                    entry.property_type().cloned(),
                                    branch_opinions_node,
                                ));
                            }
                        }
                    }
                }
            }
        }

        let redundant = retain_new_class_sites(out, &mut pending_sources);
        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }

        for (dest_path_id, remote_path_id, spec_path, field, value, property_type, node) in pending
        {
            if redundant.contains(&(dest_path_id, layer_id, spec_path.prim_spec())) {
                continue;
            }
            let mut value = value;
            remap_opinion_target_paths(store, &base_path, &inherited_path, &mut value);
            // Also apply reference namespace remapping if within a reference context.
            if let Some((ref_dest, ref_src)) = ref_remap {
                remap_opinion_target_paths(store, ref_dest, ref_src, &mut value);
            }
            let key = OpinionKey {
                node,
                layer_strength,
                layer_id,
                lookup_path: remote_path_id,
                spec_path,
            };
            let index = out.get_mut(&dest_path_id).expect("path exists");
            if let Some(property_type) = property_type {
                index.add_property_type(field, key.clone(), property_type);
            }
            index.add_opinion(Opinion {
                key,
                field,
                value,
                layer_offset,
            });
        }
    }

    for &(remote_path_id, dest_path_id) in &mapping {
        let AdmittedArcs {
            inherits: nested_inherits,
            specializes: nested_specializes,
            references: nested_refs,
            payloads: nested_payloads,
        } = admitted_arcs(
            store,
            out,
            local_stack,
            local_stack,
            inherited_root,
            remote_path_id,
            dest_path_id,
            &mut host_selection_cache,
            arc_stack,
        );
        for (nested_index, (nested, sites)) in nested_inherits.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, nested_arc_kind.or(Some(ArcKind::Variants)));
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // Inherit arcs authored inside inherited namespace may refer to
            // paths within that same namespace. When those specs are mapped
            // onto the destination prim, the inherit targets participate in
            // the destination namespace as well.
            //
            // We apply both:
            // - the translated target (to pick up local opinions at the
            //   destination path), and
            // - the original target (to pick up the class opinions authored
            //   at the source path).
            //
            // Spec: AOUSD Core §10 (inherits arc), including namespace mapping
            // behavior for inherited class namespaces.
            let translated = remap_path_id(store, &base_path, &inherited_path, nested);
            if translated != nested {
                add_inherit_edge_opinions(
                    store,
                    local_stack,
                    dest_path_id,
                    translated,
                    arc_stack,
                    outer_arc_kind,
                    specializes,
                    namespace_depth,
                    nested_index,
                    ArcParent::nested(&branch).implied(),
                    out,
                    visited,
                    visited_specializes,
                    visited_refs,
                    prim_order_out,
                    authored_children_out,
                    ref_remap,
                    None,
                    base_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }

            // Also allow translation relative to the parent mapping site.
            // This handles cases where the inherited class’s own inherits
            // target is a sibling rather than a descendant (e.g. /Looks/Metal
            // inherits /Looks/Material which inherits /Looks/BaseMaterial —
            // the parent remap /Looks → /Model/Looks correctly translates
            // /Looks/BaseMaterial → /Model/Looks/BaseMaterial).
            //
            // Spec: AOUSD Core §10 (inherits arc) and supplemental fixtures
            // involving nested classes (e.g. `BasicLocalAndGlobalClassCombination_root`).
            if let (Some(base_parent), Some(inherited_parent)) =
                (base_path.parent(), inherited_path.parent())
            {
                let parent_translated =
                    remap_path_id(store, &base_parent, &inherited_parent, nested);
                if parent_translated != translated && parent_translated != nested {
                    add_inherit_edge_opinions(
                        store,
                        local_stack,
                        dest_path_id,
                        parent_translated,
                        arc_stack,
                        outer_arc_kind,
                        specializes,
                        namespace_depth,
                        nested_index,
                        ArcParent::nested(&branch).implied(),
                        out,
                        visited,
                        visited_specializes,
                        visited_refs,
                        prim_order_out,
                        authored_children_out,
                        ref_remap,
                        None,
                        base_offset,
                        cycles,
                        deps.as_deref_mut(),
                    );
                }
            }
            add_inherit_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                nested,
                arc_stack,
                outer_arc_kind,
                specializes,
                namespace_depth,
                nested_index,
                ArcParent::nested(&branch),
                out,
                visited,
                visited_specializes,
                visited_refs,
                prim_order_out,
                authored_children_out,
                ref_remap,
                None,
                base_offset,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Propagate specializes from the inherited class.
        //
        // When an inherited class specializes another class, those opinions
        // propagate at specializes strength. This completes the LIVERPS chain
        // for inherits: inherits sees the full composition of the inherited
        // namespace including its specializes.
        //
        // Spec: AOUSD Core §10 (LIVERPS composition ordering), §10.4.1 (the
        // specializes node ranks after every other opinion of the prim).
        for (spec_index, (specialized, sites)) in nested_specializes.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, nested_arc_kind.or(Some(ArcKind::Variants)));
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            let chain = |implied| {
                specializes_chain(
                    specializes,
                    SpecializesOrigin {
                        namespace_depth,
                        arc_kind,
                        nested_arc_kind: nested_arc_kind.or(Some(ArcKind::Specializes)),
                        arc_list_index,
                        specializes_index: spec_index,
                        implied,
                    },
                )
            };

            let translated = remap_path_id(store, &base_path, &inherited_path, specialized);
            if translated != specialized {
                add_specializes_edge_opinions(
                    store,
                    local_stack,
                    dest_path_id,
                    dest_path_id,
                    translated,
                    arc_stack,
                    &chain(true),
                    namespace_depth,
                    spec_index,
                    ArcParent::nested(&branch).implied(),
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    base_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }

            if let (Some(base_parent), Some(inherited_parent)) =
                (base_path.parent(), inherited_path.parent())
            {
                let parent_translated =
                    remap_path_id(store, &base_parent, &inherited_parent, specialized);
                if parent_translated != translated && parent_translated != specialized {
                    add_specializes_edge_opinions(
                        store,
                        local_stack,
                        dest_path_id,
                        dest_path_id,
                        parent_translated,
                        arc_stack,
                        &chain(true),
                        namespace_depth,
                        spec_index,
                        ArcParent::nested(&branch).implied(),
                        out,
                        visited_specializes,
                        prim_order_out,
                        authored_children_out,
                        None,
                        base_offset,
                        cycles,
                        deps.as_deref_mut(),
                    );
                }
            }

            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                dest_path_id,
                specialized,
                arc_stack,
                &chain(false),
                namespace_depth,
                spec_index,
                ArcParent::nested(&branch),
                out,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                base_offset,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Propagate references from the inherited class.
        //
        // When an inherited class has references, those reference opinions
        // propagate through the inherits arc. This completes the LIVERPS chain
        // for inherits: the inherited namespace's references contribute opinions.
        //
        // Spec: AOUSD Core §10 (LIVERPS composition ordering).
        for (ref_index, (nested_ref, sites)) in nested_refs.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, nested_arc_kind.or(Some(ArcKind::Variants)));
            let ref_index = u16::try_from(ref_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_reference_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                nested_ref,
                Some(arc_kind),
                specializes,
                namespace_depth,
                ref_index,
                ArcParent::nested(&branch),
                out,
                visited_refs,
                visited,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Propagate payloads from the inherited class, as for references.
        //
        // Spec: AOUSD Core §10 (LIVERPS composition ordering).
        for (payload_index, (nested_payload, sites)) in nested_payloads.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, nested_arc_kind.or(Some(ArcKind::Variants)));
            let payload_index = u16::try_from(payload_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                nested_payload,
                Some(arc_kind),
                specializes,
                namespace_depth,
                payload_index,
                ArcParent::nested(&branch),
                out,
                visited_refs,
                visited,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }

    // What the class's composed index holds beyond this expansion (see
    // `Forwarding`).
    let forwarding = Forwarding {
        specializes,
        arc_kind,
        nested_arc_kind: None,
        namespace_depth,
        arc_list_index,
    };
    forwarding.copy_composed(store, out, cycles, &mut nodes, &mapping, provenance_remap);

    cycles.exit();
}

/// Maps the target paths an opinion authors (a metadata path list op, or a
/// property's connection or relationship target paths) from `src_root` into
/// `dest_root`, in place.
///
/// Spec: AOUSD Core §10 (arcs map paths authored inside the arc's target
/// namespace into the destination namespace).
fn remap_opinion_target_paths(
    store: &mut dyn LayerStore,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
    value: &mut OpinionValue,
) {
    let list = match value {
        OpinionValue::Field(FieldValue::PathListOp(list)) => list,
        OpinionValue::Property(spec) => match spec.targets.as_mut() {
            Some(list) => list,
            None => return,
        },
        OpinionValue::Field(_) => return,
    };
    let remap = |store: &mut dyn LayerStore, items: &mut Vec<TargetPath>| {
        for item in items.iter_mut() {
            *item = remap_target_path(store, dest_root, src_root, *item);
        }
    };
    if let Some(explicit) = list.explicit.as_mut() {
        remap(store, explicit);
    }
    remap(store, &mut list.prepend);
    remap(store, &mut list.append);
    remap(store, &mut list.delete);
}

fn remap_target_path(
    store: &mut dyn LayerStore,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
    path: TargetPath,
) -> TargetPath {
    match path {
        TargetPath::Prim(path) => TargetPath::Prim(remap_path_id(store, dest_root, src_root, path)),
        TargetPath::Property(path) => {
            TargetPath::Property(remap_property_path(store, dest_root, src_root, path))
        }
    }
}

fn remap_property_path(
    store: &mut dyn LayerStore,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
    path: PropertyPath,
) -> PropertyPath {
    PropertyPath::new(
        remap_path_id(store, dest_root, src_root, path.prim_path()),
        path.property(),
    )
}

fn remap_path_id(
    store: &mut dyn LayerStore,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
    path: PathId,
) -> PathId {
    let rel: Option<Vec<_>> = {
        let p = store.paths().resolve(path);
        p.strip_prefix(src_root).map(<[_]>::to_vec)
    };
    if let Some(rel) = rel {
        return store.paths_mut().intern(dest_root.join(&rel));
    }

    path
}

fn spec_path_selection_sites(
    store: &mut dyn LayerStore,
    spec_path: &SpecPath,
) -> Vec<VariantSelectionSite> {
    let mut sites = Vec::new();
    let mut prefix = Vec::new();
    for component in spec_path.components().iter().copied() {
        match component {
            crate::spec_path::SpecComponent::Prim(segment) => prefix.push(segment),
            crate::spec_path::SpecComponent::VariantSelection { set, variant } => {
                let host_path = store
                    .paths_mut()
                    .intern(crate::path::Path::root().join(&prefix));
                sites.push(VariantSelectionSite {
                    host_path,
                    set,
                    variant,
                });
            }
        }
    }
    sites
}

fn remap_spec_path(
    store: &mut dyn LayerStore,
    spec_path: &SpecPath,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
) -> SpecPath {
    let prim_path = remap_path_id(store, dest_root, src_root, spec_path.prim_path());
    let selection_sites = spec_path_selection_sites(store, spec_path)
        .into_iter()
        .map(|site| VariantSelectionSite {
            host_path: remap_path_id(store, dest_root, src_root, site.host_path),
            set: site.set,
            variant: site.variant,
        })
        .collect::<Vec<_>>();
    let mut out = if selection_sites.is_empty() {
        SpecPath::from_prim_path(prim_path, store.paths())
    } else {
        SpecPath::from_variant_selection_sites(prim_path, &selection_sites, store.paths())
    };
    if let Some(property) = spec_path.property() {
        out = out.with_property(property);
    }
    out
}

fn add_reference_edge_opinions(
    store: &mut dyn LayerStore,
    stage_stack: &LayerStack,
    dest_root: PathId,
    reference: Reference,
    outer_arc_kind: Option<ArcKind>,
    // Specializes nodes the arc is expanded in.
    specializes: &[SpecializesOrigin],
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut HashSet<(PathId, PathId)>,
    visited_specializes: &mut HashSet<(PathId, PathId)>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    provenance_remap: Option<(PathId, PathId)>,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Arcs nested inside another arc stay in the outer arc's strength
    // bucket: the target site's own opinions (and its variants) are stronger
    // than arcs authored at that site. A nested reference is therefore ranked as
    // `(outer, Some(References))`, never as a direct arc of the root layer stack.
    //
    // Spec: AOUSD Core §10.4 (LIVERPS strength ordering is applied recursively
    // within each arc's target prim index). OpenUSD makes this explicit by
    // ranking a node above all of its descendants and comparing siblings below
    // the common ancestor (`pxr/usd/pcp/strengthOrdering.cpp:309`).
    //
    // TODO(graph): NestedArcDepth. The node's strength records only the
    // outermost arc and one nested kind, so arcs nested two or more levels
    // deep share this bucket and are ordered by the remaining tie-breakers,
    // not by their position in the prim's graph.
    let (edge_arc_kind, edge_direct_nested, edge_variant_nested) = match outer_arc_kind {
        Some(outer) => (outer, Some(ArcKind::References), Some(ArcKind::References)),
        None => (ArcKind::References, None, Some(ArcKind::Variants)),
    };
    if !out.contains_key(&dest_root) {
        return;
    }
    let Some(reference_path) = resolve_arc_target(
        store,
        &reference,
        dest_root,
        ArcKind::References,
        cycles,
        deps.as_deref_mut(),
    ) else {
        return;
    };
    // An arc that would close a cycle is a composition error and is skipped
    // (AOUSD Core §10.6; OpenUSD `_CheckForCycle`).
    if cycles.closes_cycle(
        store.paths_mut(),
        dest_root,
        reference.layer,
        reference_path,
        ArcKind::References,
    ) {
        return;
    }
    // TODO(graph): CollapsedNodes. A site reached twice (a diamond, or an
    // arc listed twice with different offsets) is one arc path per
    // occurrence in OpenUSD, each with its own node; this expands it once.
    if !visited.insert((dest_root, reference.layer, reference_path)) {
        return;
    }
    cycles.enter(
        reference.layer,
        reference_path,
        dest_root,
        ArcKind::References,
    );

    // `reference.layer` is the root layer of the arc's target layer stack.
    // Arc resolution anchors an internal arc (no asset path) to the root of
    // the layer stack containing the node that authors it, so it reads that
    // whole stack (AOUSD Core §10.3.2.1; OpenUSD `_EvalRefOrPayloadArcs` in
    // `pxr/usd/pcp/primIndex.cpp`, see `anchor_internal_arcs`).
    let remote_stack = cycles.gather_layer_stack(store, reference.layer);
    let combined_stack = LayerStack {
        layers: stage_stack
            .layers
            .iter()
            .copied()
            .chain(remote_stack.layers.iter().copied())
            .collect(),
        offsets: stage_stack
            .offsets
            .iter()
            .copied()
            .chain(remote_stack.offsets.iter().copied())
            .collect(),
    };
    let target_root = store.paths().resolve(reference_path).clone();
    let dest_root_path = store.paths().resolve(dest_root).clone();
    let mut nodes = ArcNodes::new(
        parent,
        ArcStep {
            arc_kind: ArcKind::References,
            layer_stack: reference.layer,
            target: StepTarget::Namespace {
                dest_root,
                target_root: reference_path,
            },
            namespace_depth,
            sibling_index: arc_list_index,
            implied: false,
            strength: arc_strength(
                specializes,
                edge_arc_kind,
                edge_direct_nested,
                namespace_depth,
                arc_list_index,
            ),
        },
    );

    let mut remote_paths: Vec<PathId> = remote_stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| layer.prims.keys().copied())
        .collect();
    remote_paths.sort_by(|a, b| {
        store
            .paths()
            .resolve(*a)
            .cmp_with_tokens(store.paths().resolve(*b), store.tokens())
    });
    remote_paths.dedup();

    // TODO(graph): AncestralArcs. The arc maps only the target and its
    // namespace descendants. OpenUSD computes a subroot target's prim index
    // from its parent's (`_BuildInitialPrimIndexFromAncestor` in
    // `pxr/usd/pcp/primIndex.cpp`), so arcs and variant selections authored
    // on the target's ancestors contribute nodes beneath this one.
    let mut mapping: Vec<(PathId, PathId)> = Vec::new();
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&target_root) else {
                continue;
            };
            rel.to_vec()
        };
        let dest_path_id = store.paths_mut().intern(dest_root_path.join(&rel));
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }

    // Compose the reference's own offset with each remote layer's sublayer offset.
    // This gives the total time remapping for opinions from each layer in the
    // referenced layer stack.
    //
    // Spec: §12.3.2.1 (sublayer offsets compose when nested).
    let mut host_selection_cache = HashMap::new();
    for (layer_strength_idx, remote_layer_id) in remote_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let ref_offset = reference
            .layer_offset
            .compose(remote_stack.offset_at(layer_strength_idx));
        let Some(remote_layer) = store.layer(remote_layer_id).cloned() else {
            continue;
        };

        let mut pending_sources = Vec::new();
        let mut pending_fields: Vec<(
            PathId,
            TokenId,
            OpinionKey,
            OpinionValue,
            Option<PropertyType>,
            LayerOffset,
        )> = Vec::new();
        for (remote_path_id, dest_path_id) in &mapping {
            for remote_spec in remote_layer.prim_specs(*remote_path_id) {
                if let Some(d) = deps.as_deref_mut() {
                    d.add_layer_opinion(remote_layer_id, *dest_path_id);
                }
                let node =
                    nodes.spec_node(store, out, *dest_path_id, &remote_spec.outer_variant_sites);
                let base_key = OpinionKey {
                    node,
                    layer_strength,
                    layer_id: remote_layer_id,
                    lookup_path: *remote_path_id,
                    spec_path: normalized_prim_spec_path(
                        store,
                        *remote_path_id,
                        &remote_spec.outer_variant_sites,
                        provenance_remap,
                    ),
                };
                pending_sources.push((*dest_path_id, base_key.clone()));

                for entry in composed_entries(&remote_spec.fields, &remote_spec.properties) {
                    pending_fields.push((
                        *dest_path_id,
                        entry.name(),
                        base_key
                            .clone()
                            .with_spec_path(normalized_property_spec_path(
                                store,
                                *remote_path_id,
                                &remote_spec.outer_variant_sites,
                                entry.name(),
                                provenance_remap,
                            )),
                        entry.value(),
                        entry.property_type().cloned(),
                        ref_offset,
                    ));
                }

                if let Some(order) = &remote_spec.prim_order {
                    prim_order_out.entry(*dest_path_id).or_default().push((
                        OpinionKey {
                            node,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: prim_spec_path(
                                store,
                                *remote_path_id,
                                &remote_spec.outer_variant_sites,
                            ),
                        },
                        order.clone(),
                    ));
                }

                if !remote_spec.authored_children.is_empty() {
                    authored_children_out
                        .entry(*dest_path_id)
                        .or_default()
                        .push((
                            OpinionKey {
                                node,
                                layer_strength,
                                layer_id: remote_layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: prim_spec_path(
                                    store,
                                    *remote_path_id,
                                    &remote_spec.outer_variant_sites,
                                ),
                            },
                            remote_spec.authored_children.clone(),
                        ));
                }

                let selections = resolve_forwarded_variant_selections(
                    store,
                    stage_stack,
                    *dest_path_id,
                    &remote_stack,
                    *remote_path_id,
                );
                for (set, selected) in &selections {
                    if let Some(set_spec) = remote_spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected)
                    {
                        let branch_selections = combined_variant_sites(
                            &variant_spec.outer_variant_sites,
                            VariantSelectionSite {
                                host_path: *remote_path_id,
                                set: *set,
                                variant: *selected,
                            },
                        );
                        let branch_path = normalized_variant_spec_path(
                            store,
                            *remote_path_id,
                            &branch_selections,
                            provenance_remap,
                        );
                        let variant_node = nodes.variant_node(
                            store,
                            out,
                            *dest_path_id,
                            &branch_selections,
                            edge_variant_nested,
                        );
                        pending_sources.push((
                            *dest_path_id,
                            OpinionKey {
                                node: variant_node,
                                layer_strength,
                                layer_id: remote_layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: branch_path.clone(),
                            },
                        ));

                        for entry in
                            composed_entries(&variant_spec.fields, &variant_spec.properties)
                        {
                            let key = OpinionKey {
                                node: variant_node,
                                layer_strength,
                                layer_id: remote_layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_variant_property_spec_path(
                                    store,
                                    *remote_path_id,
                                    &branch_selections,
                                    entry.name(),
                                    provenance_remap,
                                ),
                            };
                            let index = out.get_mut(dest_path_id).expect("path exists");
                            if let Some(property_type) = entry.property_type() {
                                index.add_property_type(
                                    entry.name(),
                                    key.clone(),
                                    property_type.clone(),
                                );
                            }
                            index.add_opinion(Opinion {
                                key,
                                field: entry.name(),
                                value: entry.value(),
                                layer_offset: ref_offset,
                            });
                        }
                    }
                }
            }
        }

        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }
        for (dest_path_id, field, key, value, property_type, offset) in pending_fields {
            let mut value = value;
            remap_opinion_target_paths(store, &dest_root_path, &target_root, &mut value);
            let index = out.get_mut(&dest_path_id).expect("path exists");
            if let Some(property_type) = property_type {
                index.add_property_type(field, key.clone(), property_type);
            }
            index.add_opinion(Opinion {
                key,
                field,
                value,
                layer_offset: offset,
            });
        }
    }

    for &(remote_path_id, dest_path_id) in &mapping {
        let arcs = admitted_arcs(
            store,
            out,
            stage_stack,
            &remote_stack,
            reference_path,
            remote_path_id,
            dest_path_id,
            &mut host_selection_cache,
            reference.layer,
        );
        let inherits = arcs.inherits;
        for (inherit_index, (inherited_root, sites)) in inherits.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, edge_variant_nested);
            let inherit_index = u16::try_from(inherit_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // Inherit paths authored inside referenced content are translated
            // into the destination namespace (so local opinions on the
            // destination path participate).
            //
            // Spec: AOUSD Core §10 (references/inherits), via path translation
            // into the referencing namespace.
            let translated = remap_path_id(store, &dest_root_path, &target_root, inherited_root);
            let ref_remap = Some((&dest_root_path, &target_root));
            if translated != inherited_root {
                add_inherit_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    translated,
                    cycles.stage_layer_stack(),
                    Some(edge_arc_kind),
                    specializes,
                    namespace_depth,
                    inherit_index,
                    ArcParent::nested(&branch).implied(),
                    out,
                    visited_inherits,
                    visited_specializes,
                    visited,
                    prim_order_out,
                    authored_children_out,
                    ref_remap,
                    None,
                    reference.layer_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }

            add_inherit_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                inherited_root,
                reference.layer,
                Some(edge_arc_kind),
                specializes,
                namespace_depth,
                inherit_index,
                ArcParent::nested(&branch),
                out,
                visited_inherits,
                visited_specializes,
                visited,
                prim_order_out,
                authored_children_out,
                ref_remap,
                None,
                reference.layer_offset,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Direct references, references on the prim's selected branch
        // headers, and references authored for it inside its parent's
        // selected branches.
        let all_nested = arcs.references;
        for (nested_index, (nested_ref, sites)) in all_nested.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, edge_variant_nested);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_reference_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_ref,
                Some(edge_arc_kind),
                specializes,
                namespace_depth,
                nested_index,
                ArcParent::nested(&branch),
                out,
                visited,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Handle nested payloads inside referenced content.
        let nested_payloads = arcs.payloads;
        for (nested_index, (nested_payload, sites)) in nested_payloads.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, edge_variant_nested);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_payload,
                Some(edge_arc_kind),
                specializes,
                namespace_depth,
                nested_index,
                ArcParent::nested(&branch),
                out,
                visited,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Handle nested specializes inside referenced content. Their
        // opinions are weaker than every other opinion of the prim, not only
        // than this reference's: each specializes node is propagated to the
        // root with its placeholder here as its origin, and the specialized
        // path mapped into the referencing namespace is implied there.
        //
        // Spec: AOUSD Core §10.4.1; OpenUSD `_EvalImpliedSpecializes` in
        // `pxr/usd/pcp/primIndex.cpp`.
        for (spec_index, (specialized_root, sites)) in arcs.specializes.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, edge_variant_nested);
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            let chain = |implied| {
                specializes_chain(
                    specializes,
                    SpecializesOrigin {
                        namespace_depth,
                        arc_kind: edge_arc_kind,
                        nested_arc_kind: edge_direct_nested.or(Some(ArcKind::Specializes)),
                        arc_list_index,
                        specializes_index: spec_index,
                        implied,
                    },
                )
            };

            let translated = remap_path_id(store, &dest_root_path, &target_root, specialized_root);
            if translated != specialized_root {
                add_specializes_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    dest_path_id,
                    translated,
                    cycles.stage_layer_stack(),
                    &chain(true),
                    namespace_depth,
                    spec_index,
                    ArcParent::nested(&branch).implied(),
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    reference.layer_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }

            add_specializes_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                remote_path_id,
                specialized_root,
                reference.layer,
                &chain(false),
                namespace_depth,
                spec_index,
                ArcParent::nested(&branch),
                out,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                reference.layer_offset,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }

    // What the target's composed index holds beyond this expansion (see
    // `Forwarding`).
    let forwarding = Forwarding {
        specializes,
        arc_kind: edge_arc_kind,
        nested_arc_kind: edge_direct_nested,
        namespace_depth,
        arc_list_index,
    };
    forwarding.copy_composed(store, out, cycles, &mut nodes, &mapping, provenance_remap);

    // Post-process: remap any PathListOp values in opinions on mapped
    // dest prims that still reference the source namespace. This covers
    // field values brought in by nested arcs (inherits, nested references)
    // within this reference context.
    for (_, dest_path_id) in &mapping {
        let Some(index) = out.get_mut(dest_path_id) else {
            continue;
        };
        for opinions in index.opinions_by_field.values_mut() {
            for opinion in opinions.iter_mut() {
                remap_opinion_target_paths(
                    store,
                    &dest_root_path,
                    &target_root,
                    &mut opinion.value,
                );
            }
        }
    }
    cycles.exit();
}

fn add_payload_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Spec: AOUSD Core §10 (payloads arc, §5.1.22). Payloads are structurally
    // identical to references for composition purposes but sit at a weaker
    // position in LIVERPS (between References and Specializes).
    let mut visited: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_specializes: HashSet<(PathId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        cycles.begin(dest_root);
        // Internal arcs authored in the stage's layer stack target it.
        let anchor = cycles.stage_layer_stack();
        let payloads =
            resolve_payloads_for_prim(store, local_stack, dest_root, SelectionScope::Stack, anchor);
        // Also resolve variant branch-level payloads.
        let branch_payloads =
            resolve_variant_branch_payloads(store, local_stack, local_stack, dest_root, anchor);
        let all_payloads = payloads.into_iter().chain(branch_payloads);
        let selections = resolve_full_variant_selections(store, local_stack, dest_root);
        for (arc_list_index, payload) in all_payloads.enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            // An unresolved target is reported when the arc is followed.
            if let Some(d) = deps.as_deref_mut()
                && let Some(payload_path) = payload.target_path(store)
            {
                d.add_arc(ArcDependency {
                    source: payload_path,
                    target: dest_root,
                    arc_kind: ArcKind::Payloads,
                    layer: payload.layer,
                });
            }
            let sites = ArcAuthoring {
                store,
                stack: local_stack,
                prim: dest_root,
                selections: &selections,
                scope: SelectionScope::Stack,
            }
            .reference_sites(
                &payload,
                |spec| &spec.payloads,
                |branch| &branch.payloads,
                anchor,
            );
            let branch = local_variant_steps(root_layer_stack(out, dest_root), &sites);
            add_payload_edge_opinions(
                store,
                local_stack,
                dest_root,
                payload,
                None,
                &[],
                namespace_depth,
                arc_list_index,
                ArcParent::nested(&branch),
                out,
                &mut visited,
                &mut visited_inherits,
                &mut visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }
}

fn add_payload_edge_opinions(
    store: &mut dyn LayerStore,
    stage_stack: &LayerStack,
    dest_root: PathId,
    reference: Reference,
    outer_arc_kind: Option<ArcKind>,
    // Specializes nodes the arc is expanded in.
    specializes: &[SpecializesOrigin],
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut HashSet<(PathId, PathId)>,
    visited_specializes: &mut HashSet<(PathId, PathId)>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    provenance_remap: Option<(PathId, PathId)>,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Arcs nested inside another arc stay in the outer arc's strength
    // bucket: the target site's own opinions (and its variants) are stronger
    // than arcs authored at that site. A nested payload is therefore ranked as
    // `(outer, Some(Payloads))`, never as a direct arc of the root layer stack.
    //
    // Spec: AOUSD Core §10.4 (LIVERPS strength ordering is applied recursively
    // within each arc's target prim index). OpenUSD makes this explicit by
    // ranking a node above all of its descendants and comparing siblings below
    // the common ancestor (`pxr/usd/pcp/strengthOrdering.cpp:309`).
    //
    // TODO(graph): NestedArcDepth. The node's strength records only the
    // outermost arc and one nested kind, so arcs nested two or more levels
    // deep share this bucket and are ordered by the remaining tie-breakers,
    // not by their position in the prim's graph.
    let (edge_arc_kind, edge_direct_nested, edge_variant_nested) = match outer_arc_kind {
        Some(outer) => (outer, Some(ArcKind::Payloads), Some(ArcKind::Payloads)),
        None => (ArcKind::Payloads, None, Some(ArcKind::Variants)),
    };
    // Payloads mirror reference edge opinions with ArcKind::Payloads.
    if !out.contains_key(&dest_root) {
        return;
    }
    let Some(reference_path) = resolve_arc_target(
        store,
        &reference,
        dest_root,
        ArcKind::Payloads,
        cycles,
        deps.as_deref_mut(),
    ) else {
        return;
    };
    // An arc that would close a cycle is a composition error and is skipped
    // (AOUSD Core §10.6; OpenUSD `_CheckForCycle`).
    if cycles.closes_cycle(
        store.paths_mut(),
        dest_root,
        reference.layer,
        reference_path,
        ArcKind::Payloads,
    ) {
        return;
    }
    // TODO(graph): CollapsedNodes. A site reached twice (a diamond, or an
    // arc listed twice with different offsets) is one arc path per
    // occurrence in OpenUSD, each with its own node; this expands it once.
    if !visited.insert((dest_root, reference.layer, reference_path)) {
        return;
    }
    cycles.enter(
        reference.layer,
        reference_path,
        dest_root,
        ArcKind::Payloads,
    );

    // `reference.layer` is the root layer of the arc's target layer stack.
    // Arc resolution anchors an internal arc (no asset path) to the root of
    // the layer stack containing the node that authors it, so it reads that
    // whole stack (AOUSD Core §10.3.2.1; OpenUSD `_EvalRefOrPayloadArcs` in
    // `pxr/usd/pcp/primIndex.cpp`, see `anchor_internal_arcs`).
    let remote_stack = cycles.gather_layer_stack(store, reference.layer);
    let combined_stack = LayerStack {
        layers: stage_stack
            .layers
            .iter()
            .copied()
            .chain(remote_stack.layers.iter().copied())
            .collect(),
        offsets: stage_stack
            .offsets
            .iter()
            .copied()
            .chain(remote_stack.offsets.iter().copied())
            .collect(),
    };
    let target_root = store.paths().resolve(reference_path).clone();
    let dest_root_path = store.paths().resolve(dest_root).clone();
    let mut nodes = ArcNodes::new(
        parent,
        ArcStep {
            arc_kind: ArcKind::Payloads,
            layer_stack: reference.layer,
            target: StepTarget::Namespace {
                dest_root,
                target_root: reference_path,
            },
            namespace_depth,
            sibling_index: arc_list_index,
            implied: false,
            strength: arc_strength(
                specializes,
                edge_arc_kind,
                edge_direct_nested,
                namespace_depth,
                arc_list_index,
            ),
        },
    );

    let mut remote_paths: Vec<PathId> = remote_stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| layer.prims.keys().copied())
        .collect();
    remote_paths.sort_by(|a, b| {
        store
            .paths()
            .resolve(*a)
            .cmp_with_tokens(store.paths().resolve(*b), store.tokens())
    });
    remote_paths.dedup();

    // TODO(graph): AncestralArcs. The arc maps only the target and its
    // namespace descendants. OpenUSD computes a subroot target's prim index
    // from its parent's (`_BuildInitialPrimIndexFromAncestor` in
    // `pxr/usd/pcp/primIndex.cpp`), so arcs and variant selections authored
    // on the target's ancestors contribute nodes beneath this one.
    let mut mapping: Vec<(PathId, PathId)> = Vec::new();
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&target_root) else {
                continue;
            };
            rel.to_vec()
        };
        let dest_path_id = store.paths_mut().intern(dest_root_path.join(&rel));
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }

    let mut host_selection_cache = HashMap::new();
    for (layer_strength_idx, remote_layer_id) in remote_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let payload_offset = reference
            .layer_offset
            .compose(remote_stack.offset_at(layer_strength_idx));
        let Some(remote_layer) = store.layer(remote_layer_id).cloned() else {
            continue;
        };

        let mut pending_sources = Vec::new();
        for (remote_path_id, dest_path_id) in &mapping {
            for remote_spec in remote_layer.prim_specs(*remote_path_id) {
                if let Some(d) = deps.as_deref_mut() {
                    d.add_layer_opinion(remote_layer_id, *dest_path_id);
                }
                let node =
                    nodes.spec_node(store, out, *dest_path_id, &remote_spec.outer_variant_sites);
                pending_sources.push((
                    *dest_path_id,
                    OpinionKey {
                        node,
                        layer_strength,
                        layer_id: remote_layer_id,
                        lookup_path: *remote_path_id,
                        spec_path: normalized_prim_spec_path(
                            store,
                            *remote_path_id,
                            &remote_spec.outer_variant_sites,
                            provenance_remap,
                        ),
                    },
                ));

                for entry in composed_entries(&remote_spec.fields, &remote_spec.properties) {
                    let key = OpinionKey {
                        node,
                        layer_strength,
                        layer_id: remote_layer_id,
                        lookup_path: *remote_path_id,
                        spec_path: normalized_property_spec_path(
                            store,
                            *remote_path_id,
                            &remote_spec.outer_variant_sites,
                            entry.name(),
                            provenance_remap,
                        ),
                    };
                    let index = out.get_mut(dest_path_id).expect("path exists");
                    if let Some(property_type) = entry.property_type() {
                        index.add_property_type(entry.name(), key.clone(), property_type.clone());
                    }
                    index.add_opinion(Opinion {
                        key,
                        field: entry.name(),
                        value: entry.value(),
                        layer_offset: payload_offset,
                    });
                }

                if let Some(order) = &remote_spec.prim_order {
                    prim_order_out.entry(*dest_path_id).or_default().push((
                        OpinionKey {
                            node,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_prim_spec_path(
                                store,
                                *remote_path_id,
                                &remote_spec.outer_variant_sites,
                                provenance_remap,
                            ),
                        },
                        order.clone(),
                    ));
                }

                if !remote_spec.authored_children.is_empty() {
                    authored_children_out
                        .entry(*dest_path_id)
                        .or_default()
                        .push((
                            OpinionKey {
                                node,
                                layer_strength,
                                layer_id: remote_layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_prim_spec_path(
                                    store,
                                    *remote_path_id,
                                    &remote_spec.outer_variant_sites,
                                    provenance_remap,
                                ),
                            },
                            remote_spec.authored_children.clone(),
                        ));
                }

                let selections = resolve_forwarded_variant_selections(
                    store,
                    stage_stack,
                    *dest_path_id,
                    &remote_stack,
                    *remote_path_id,
                );
                for (set, selected) in &selections {
                    if let Some(set_spec) = remote_spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected)
                    {
                        let branch_selections = combined_variant_sites(
                            &variant_spec.outer_variant_sites,
                            VariantSelectionSite {
                                host_path: *remote_path_id,
                                set: *set,
                                variant: *selected,
                            },
                        );
                        let branch_path = normalized_variant_spec_path(
                            store,
                            *remote_path_id,
                            &branch_selections,
                            provenance_remap,
                        );
                        let variant_node = nodes.variant_node(
                            store,
                            out,
                            *dest_path_id,
                            &branch_selections,
                            edge_variant_nested,
                        );
                        pending_sources.push((
                            *dest_path_id,
                            OpinionKey {
                                node: variant_node,
                                layer_strength,
                                layer_id: remote_layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: branch_path.clone(),
                            },
                        ));

                        for entry in
                            composed_entries(&variant_spec.fields, &variant_spec.properties)
                        {
                            let key = OpinionKey {
                                node: variant_node,
                                layer_strength,
                                layer_id: remote_layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_variant_property_spec_path(
                                    store,
                                    *remote_path_id,
                                    &branch_selections,
                                    entry.name(),
                                    provenance_remap,
                                ),
                            };
                            let index = out.get_mut(dest_path_id).expect("path exists");
                            if let Some(property_type) = entry.property_type() {
                                index.add_property_type(
                                    entry.name(),
                                    key.clone(),
                                    property_type.clone(),
                                );
                            }
                            index.add_opinion(Opinion {
                                key,
                                field: entry.name(),
                                value: entry.value(),
                                layer_offset: payload_offset,
                            });
                        }
                    }
                }
            }
        }

        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }
    }

    // Handle nested arcs inside payload targets.
    for (remote_path_id, dest_path_id) in mapping {
        let arcs = admitted_arcs(
            store,
            out,
            stage_stack,
            &remote_stack,
            reference_path,
            remote_path_id,
            dest_path_id,
            &mut host_selection_cache,
            reference.layer,
        );
        let inherits = arcs.inherits;
        for (inherit_index, (inherited_root, sites)) in inherits.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, edge_variant_nested);
            let inherit_index = u16::try_from(inherit_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            let translated = remap_path_id(store, &dest_root_path, &target_root, inherited_root);
            let ref_remap = Some((&dest_root_path, &target_root));
            if translated != inherited_root {
                add_inherit_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    translated,
                    cycles.stage_layer_stack(),
                    Some(edge_arc_kind),
                    specializes,
                    namespace_depth,
                    inherit_index,
                    ArcParent::nested(&branch).implied(),
                    out,
                    visited_inherits,
                    visited_specializes,
                    visited,
                    prim_order_out,
                    authored_children_out,
                    ref_remap,
                    None,
                    reference.layer_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }

            add_inherit_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                inherited_root,
                reference.layer,
                Some(edge_arc_kind),
                specializes,
                namespace_depth,
                inherit_index,
                ArcParent::nested(&branch),
                out,
                visited_inherits,
                visited_specializes,
                visited,
                prim_order_out,
                authored_children_out,
                ref_remap,
                None,
                reference.layer_offset,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Direct references and those authored for this prim inside its own
        // or its parent's selected variant branches.
        let nested = arcs.references;
        for (nested_index, (nested_ref, sites)) in nested.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, edge_variant_nested);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_reference_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_ref,
                Some(edge_arc_kind),
                specializes,
                namespace_depth,
                nested_index,
                ArcParent::nested(&branch),
                out,
                visited,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Handle nested payloads inside payload targets.
        let nested_payloads = arcs.payloads;
        for (nested_index, (nested_payload, sites)) in nested_payloads.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, edge_variant_nested);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_payload,
                Some(edge_arc_kind),
                specializes,
                namespace_depth,
                nested_index,
                ArcParent::nested(&branch),
                out,
                visited,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Handle nested specializes inside payload targets, placed as for
        // references (AOUSD Core §10.4.1).
        for (spec_index, (specialized_root, sites)) in arcs.specializes.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, edge_variant_nested);
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            let chain = |implied| {
                specializes_chain(
                    specializes,
                    SpecializesOrigin {
                        namespace_depth,
                        arc_kind: edge_arc_kind,
                        nested_arc_kind: edge_direct_nested.or(Some(ArcKind::Specializes)),
                        arc_list_index,
                        specializes_index: spec_index,
                        implied,
                    },
                )
            };

            let translated = remap_path_id(store, &dest_root_path, &target_root, specialized_root);
            if translated != specialized_root {
                add_specializes_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    dest_path_id,
                    translated,
                    cycles.stage_layer_stack(),
                    &chain(true),
                    namespace_depth,
                    spec_index,
                    ArcParent::nested(&branch).implied(),
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    reference.layer_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }

            add_specializes_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                remote_path_id,
                specialized_root,
                reference.layer,
                &chain(false),
                namespace_depth,
                spec_index,
                ArcParent::nested(&branch),
                out,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                reference.layer_offset,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }
    cycles.exit();
}

fn add_specializes_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Spec: AOUSD Core §10 (specializes arc, §5.1.33). Specializes mirrors
    // inherits but sits at the weakest position in LIVERPS.
    let mut visited: HashSet<(PathId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        cycles.begin(dest_root);
        let specializes =
            resolve_specializes_for_prim(store, local_stack, dest_root, SelectionScope::Stack);
        let selections = resolve_full_variant_selections(store, local_stack, dest_root);
        for (arc_list_index, specialized_root) in specializes.into_iter().enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            if let Some(d) = deps.as_deref_mut() {
                d.add_arc(ArcDependency {
                    source: specialized_root,
                    target: dest_root,
                    arc_kind: ArcKind::Specializes,
                    layer: local_stack.layers[0],
                });
            }
            let sites = ArcAuthoring {
                store,
                stack: local_stack,
                prim: dest_root,
                selections: &selections,
                scope: SelectionScope::Stack,
            }
            .sites(
                &specialized_root,
                |spec| &spec.specializes,
                |branch| &branch.specializes,
            );
            let branch = local_variant_steps(root_layer_stack(out, dest_root), &sites);
            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_root,
                dest_root,
                specialized_root,
                cycles.stage_layer_stack(),
                &[SpecializesOrigin {
                    namespace_depth,
                    arc_kind: ArcKind::Specializes,
                    nested_arc_kind: None,
                    arc_list_index,
                    specializes_index: arc_list_index,
                    implied: false,
                }],
                namespace_depth,
                arc_list_index,
                ArcParent::nested(&branch),
                out,
                &mut visited,
                prim_order_out,
                authored_children_out,
                None,
                LayerOffset::IDENTITY,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }
}

/// Specializes edge opinions mirror inherits but use [`ArcKind::Specializes`].
///
/// Every opinion the arc introduces, including those of arcs authored inside
/// the specialized prim, belongs to the specializes node `specializes` ends
/// with, which ranks after every other opinion of the prim wherever the arc
/// is authored (AOUSD Core §10.4.1; OpenUSD `_EvalImpliedSpecializes` in
/// `pxr/usd/pcp/primIndex.cpp`).
fn add_specializes_edge_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    dest_root: PathId,
    selection_root: PathId,
    specialized_root: PathId,
    // Root layer of the layer stack the specializes arc is authored in.
    arc_stack: LayerId,
    // The specializes arcs of the opinions this arc introduces, ending with
    // this one.
    specializes: &[SpecializesOrigin],
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, PathId)>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    provenance_remap: Option<(PathId, PathId)>,
    // Accumulated offset from outer arcs (references/payloads).
    base_offset: LayerOffset,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // An arc that would close a cycle is a composition error and is skipped
    // (AOUSD Core §10.6; OpenUSD `_CheckForCycle`).
    if cycles.closes_cycle(
        store.paths_mut(),
        dest_root,
        arc_stack,
        specialized_root,
        ArcKind::Specializes,
    ) {
        return;
    }
    if !visited.insert((dest_root, specialized_root)) {
        return;
    }
    cycles.enter(arc_stack, specialized_root, dest_root, ArcKind::Specializes);

    let base_path = store.paths().resolve(dest_root).clone();
    let selection_base_path = store.paths().resolve(selection_root).clone();
    let specialized_path = store.paths().resolve(specialized_root).clone();

    let mut remote_paths: Vec<PathId> = local_stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| layer.prims.keys().copied())
        .collect();
    remote_paths.sort_by(|a, b| {
        store
            .paths()
            .resolve(*a)
            .cmp_with_tokens(store.paths().resolve(*b), store.tokens())
    });
    remote_paths.dedup();

    let mut mapping: Vec<(PathId, PathId)> = Vec::new();
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&specialized_path) else {
                continue;
            };
            rel.to_vec()
        };
        let dest_path_id = store.paths_mut().intern(base_path.join(&rel));
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }

    let mut host_selection_cache = HashMap::new();
    // Branch-only source prims whose branch is not selected for the
    // destination take no part in this arc.
    let unselected = {
        let pairs: Vec<(PathId, PathId)> = mapping
            .iter()
            .map(|&(remote, _)| {
                let rel = store
                    .paths()
                    .resolve(remote)
                    .strip_prefix(&specialized_path)
                    .expect("mapping source should stay under specialized root")
                    .to_vec();
                let selection = store
                    .paths()
                    .lookup(&selection_base_path.join(&rel))
                    .unwrap_or(selection_root);
                (remote, selection)
            })
            .collect();
        unselected_branch_prims(
            store,
            out,
            local_stack,
            local_stack,
            specialized_root,
            &pairs,
            &mut host_selection_cache,
        )
    };
    mapping.retain(|(remote, _)| !is_at_or_under(store, *remote, &unselected));

    // The specialized prim's own opinions are the specializes node's; arcs
    // authored inside it nest under that node.
    let arc_kind = ArcKind::Specializes;
    let nested_arc_kind: Option<ArcKind> = None;
    // TODO(graph): SpecializesPlacement. OpenUSD propagates a specializes
    // node to the root of the prim index, with the node where the arc is
    // authored as its origin (`_EvalImpliedSpecializes` in
    // `pxr/usd/pcp/primIndex.cpp`); this keeps the node where the arc is
    // authored and ranks it globally weaker through its strength's
    // specializes chain instead.
    let mut nodes = ArcNodes::new(
        parent,
        ArcStep {
            arc_kind: ArcKind::Specializes,
            layer_stack: arc_stack,
            target: StepTarget::Namespace {
                dest_root,
                target_root: specialized_root,
            },
            namespace_depth,
            sibling_index: arc_list_index,
            implied: false,
            strength: arc_strength(
                specializes,
                arc_kind,
                nested_arc_kind,
                namespace_depth,
                arc_list_index,
            ),
        },
    );
    let forwarding = Forwarding {
        specializes,
        arc_kind,
        nested_arc_kind,
        namespace_depth,
        arc_list_index,
    };

    for (layer_strength_idx, layer_id) in local_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let layer_offset = base_offset.compose(local_stack.offset_at(layer_strength_idx));
        let mut pending: Vec<PendingOpinion> = Vec::new();
        let mut pending_sources = Vec::new();
        {
            let Some(layer) = store.layer(layer_id).cloned() else {
                continue;
            };

            for (remote_path_id, dest_path_id) in &mapping {
                for spec in layer.prim_specs(*remote_path_id) {
                    let selection_path_id = {
                        let rel = store
                            .paths()
                            .resolve(*remote_path_id)
                            .strip_prefix(&specialized_path)
                            .expect("mapping source should stay under specialized root")
                            .to_vec();
                        store
                            .paths()
                            .lookup(&selection_base_path.join(&rel))
                            .unwrap_or(selection_root)
                    };
                    if let Some(d) = deps.as_deref_mut() {
                        d.add_layer_opinion(layer_id, *dest_path_id);
                    }
                    let node =
                        nodes.spec_node(store, out, *dest_path_id, &spec.outer_variant_sites);
                    if let Some(order) = &spec.prim_order {
                        prim_order_out.entry(*dest_path_id).or_default().push((
                            OpinionKey {
                                node,
                                layer_strength,
                                layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_prim_spec_path(
                                    store,
                                    *remote_path_id,
                                    &spec.outer_variant_sites,
                                    provenance_remap,
                                ),
                            },
                            order.clone(),
                        ));
                    }

                    if !spec.authored_children.is_empty() {
                        authored_children_out
                            .entry(*dest_path_id)
                            .or_default()
                            .push((
                                OpinionKey {
                                    node,
                                    layer_strength,
                                    layer_id,
                                    lookup_path: *remote_path_id,
                                    spec_path: normalized_prim_spec_path(
                                        store,
                                        *remote_path_id,
                                        &spec.outer_variant_sites,
                                        provenance_remap,
                                    ),
                                },
                                spec.authored_children.clone(),
                            ));
                    }

                    pending_sources.push((
                        *dest_path_id,
                        OpinionKey {
                            node,
                            layer_strength,
                            layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_prim_spec_path(
                                store,
                                *remote_path_id,
                                &spec.outer_variant_sites,
                                provenance_remap,
                            ),
                        },
                    ));
                    for entry in composed_entries(&spec.fields, &spec.properties) {
                        pending.push((
                            *dest_path_id,
                            *remote_path_id,
                            normalized_property_spec_path(
                                store,
                                *remote_path_id,
                                &spec.outer_variant_sites,
                                entry.name(),
                                provenance_remap,
                            ),
                            entry.name(),
                            entry.value(),
                            entry.property_type().cloned(),
                            node,
                        ));
                    }

                    // Forward variant opinions from selected variants through specializes.
                    let spec_selections = resolve_forwarded_variant_selections(
                        store,
                        local_stack,
                        selection_path_id,
                        local_stack,
                        *remote_path_id,
                    );
                    for (set, selected) in &spec_selections {
                        if let Some(set_spec) = spec.variant_sets.get(set)
                            && let Some(variant_spec) = set_spec.variants.get(selected)
                        {
                            let branch_selections = combined_variant_sites(
                                &variant_spec.outer_variant_sites,
                                VariantSelectionSite {
                                    host_path: *remote_path_id,
                                    set: *set,
                                    variant: *selected,
                                },
                            );
                            let branch_path = normalized_variant_spec_path(
                                store,
                                *remote_path_id,
                                &branch_selections,
                                provenance_remap,
                            );
                            let variant_node = nodes.variant_node(
                                store,
                                out,
                                *dest_path_id,
                                &branch_selections,
                                nested_arc_kind.or(Some(ArcKind::Variants)),
                            );
                            // TODO(graph): NestedArcDepth. The branch's
                            // opinions rank with the specializes node's own
                            // opinions, not with the branch's variant node,
                            // which OpenUSD ranks beneath the specialized
                            // site (`PcpCompareSiblingNodeStrength`).
                            let branch_opinions_node = nodes.variant_node(
                                store,
                                out,
                                *dest_path_id,
                                &branch_selections,
                                nested_arc_kind,
                            );
                            pending_sources.push((
                                *dest_path_id,
                                OpinionKey {
                                    node: variant_node,
                                    layer_strength,
                                    layer_id,
                                    lookup_path: *remote_path_id,
                                    spec_path: branch_path,
                                },
                            ));
                            for entry in
                                composed_entries(&variant_spec.fields, &variant_spec.properties)
                            {
                                pending.push((
                                    *dest_path_id,
                                    *remote_path_id,
                                    normalized_variant_property_spec_path(
                                        store,
                                        *remote_path_id,
                                        &branch_selections,
                                        entry.name(),
                                        provenance_remap,
                                    ),
                                    entry.name(),
                                    entry.value(),
                                    entry.property_type().cloned(),
                                    branch_opinions_node,
                                ));
                            }
                        }
                    }
                }
            }
        }

        let redundant = retain_new_class_sites(out, &mut pending_sources);
        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }

        for (dest_path_id, remote_path_id, spec_path, field, value, property_type, node) in pending
        {
            if redundant.contains(&(dest_path_id, layer_id, spec_path.prim_spec())) {
                continue;
            }
            let mut value = value;
            remap_opinion_target_paths(store, &base_path, &specialized_path, &mut value);
            let key = OpinionKey {
                node,
                layer_strength,
                layer_id,
                lookup_path: remote_path_id,
                spec_path,
            };
            let index = out.get_mut(&dest_path_id).expect("path exists");
            if let Some(property_type) = property_type {
                index.add_property_type(field, key.clone(), property_type);
            }
            index.add_opinion(Opinion {
                key,
                field,
                value,
                layer_offset,
            });
        }
    }

    // Handle nested specializes arcs.
    for &(remote_path_id, dest_path_id) in &mapping {
        let selection_path_id = {
            let rel = store
                .paths()
                .resolve(remote_path_id)
                .strip_prefix(&specialized_path)
                .expect("mapping source should stay under specialized root")
                .to_vec();
            store
                .paths()
                .lookup(&selection_base_path.join(&rel))
                .unwrap_or(selection_root)
        };
        let arcs = admitted_arcs(
            store,
            out,
            local_stack,
            local_stack,
            specialized_root,
            remote_path_id,
            selection_path_id,
            &mut host_selection_cache,
            arc_stack,
        );
        let nested_specializes = arcs.specializes;
        for (nested_index, (nested, sites)) in nested_specializes.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, nested_arc_kind.or(Some(ArcKind::Variants)));
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            // A specializes authored inside the specialized prim is its own
            // node, weaker than this one (AOUSD Core §10.4.1); the mapped
            // paths below are its implied copies.
            let chain = |implied| {
                specializes_chain(
                    specializes,
                    SpecializesOrigin {
                        namespace_depth,
                        arc_kind,
                        nested_arc_kind: Some(ArcKind::Specializes),
                        arc_list_index,
                        specializes_index: nested_index,
                        implied,
                    },
                )
            };

            let translated = remap_path_id(store, &base_path, &specialized_path, nested);
            if translated != nested {
                add_specializes_edge_opinions(
                    store,
                    local_stack,
                    dest_path_id,
                    selection_path_id,
                    translated,
                    arc_stack,
                    &chain(true),
                    namespace_depth,
                    nested_index,
                    ArcParent::nested(&branch).implied(),
                    out,
                    visited,
                    prim_order_out,
                    authored_children_out,
                    None,
                    base_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }

            // Parent-level remap for sibling specializes targets.
            if let (Some(base_parent), Some(specialized_parent)) =
                (base_path.parent(), specialized_path.parent())
            {
                let parent_translated =
                    remap_path_id(store, &base_parent, &specialized_parent, nested);
                if parent_translated != translated && parent_translated != nested {
                    add_specializes_edge_opinions(
                        store,
                        local_stack,
                        dest_path_id,
                        selection_path_id,
                        parent_translated,
                        arc_stack,
                        &chain(true),
                        namespace_depth,
                        nested_index,
                        ArcParent::nested(&branch).implied(),
                        out,
                        visited,
                        prim_order_out,
                        authored_children_out,
                        None,
                        base_offset,
                        cycles,
                        deps.as_deref_mut(),
                    );
                }
            }

            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                selection_path_id,
                nested,
                arc_stack,
                &chain(false),
                namespace_depth,
                nested_index,
                ArcParent::nested(&branch),
                out,
                visited,
                prim_order_out,
                authored_children_out,
                None,
                base_offset,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }

    // Propagate inherits from the specialized class.
    //
    // A class the specialized prim inherits is an inherits arc inside the
    // specializes node: weaker than the specialized prim's own opinions and
    // than its variants, stronger than its references, and weaker than every
    // opinion outside the node.
    //
    // Spec: AOUSD Core §10.4.1 ("opinions from composition arcs that are
    // introduced by prim B").
    let mut visited_refs: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    for &(remote_path_id, dest_path_id) in &mapping {
        let selection_path_id = {
            let rel = store
                .paths()
                .resolve(remote_path_id)
                .strip_prefix(&specialized_path)
                .expect("mapping source should stay under specialized root")
                .to_vec();
            store
                .paths()
                .lookup(&selection_base_path.join(&rel))
                .unwrap_or(selection_root)
        };
        let arcs = admitted_arcs(
            store,
            out,
            local_stack,
            local_stack,
            specialized_root,
            remote_path_id,
            selection_path_id,
            &mut host_selection_cache,
            arc_stack,
        );
        let nested_inherits = arcs.inherits;
        for (nested_index, (inherited, sites)) in nested_inherits.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, nested_arc_kind.or(Some(ArcKind::Variants)));
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // The inherited path mapped into the destination namespace, and
            // relative to the parent mapping site for a class that is a
            // sibling of the specialized class (e.g. /Looks/Metal
            // specializes, /Looks/Material inherited — they share /Looks),
            // then the authored path.
            let translated = remap_path_id(store, &base_path, &specialized_path, inherited);
            let parent_translated = match (base_path.parent(), specialized_path.parent()) {
                (Some(base_parent), Some(specialized_parent)) => Some(remap_path_id(
                    store,
                    &base_parent,
                    &specialized_parent,
                    inherited,
                )),
                _ => None,
            };
            // Each target, and whether it is an implied copy of the class.
            let mut targets = Vec::with_capacity(3);
            if translated != inherited {
                targets.push((translated, true));
            }
            if let Some(parent_translated) = parent_translated
                && parent_translated != translated
                && parent_translated != inherited
            {
                targets.push((parent_translated, true));
            }
            targets.push((inherited, false));
            for (target, implied) in targets {
                add_inherit_edge_opinions(
                    store,
                    local_stack,
                    dest_path_id,
                    target,
                    arc_stack,
                    Some(arc_kind),
                    specializes,
                    namespace_depth,
                    nested_index,
                    if implied {
                        ArcParent::nested(&branch).implied()
                    } else {
                        ArcParent::nested(&branch)
                    },
                    out,
                    &mut visited_inherits,
                    visited,
                    &mut visited_refs,
                    prim_order_out,
                    authored_children_out,
                    None,
                    None,
                    base_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }
        }
    }

    // Propagate references from the specialized class.
    //
    // When a specialized class references other prims, those referenced
    // opinions propagate at specializes strength. This handles cases like
    // ShinyPlastic_BlueShinyPlastic specializes ShinyPlastic which
    // references ShinyPlasticLook.
    //
    // Spec: AOUSD Core §10 (specializes propagation through all arcs).
    for &(remote_path_id, dest_path_id) in &mapping {
        let selection_path_id = {
            let rel = store
                .paths()
                .resolve(remote_path_id)
                .strip_prefix(&specialized_path)
                .expect("mapping source should stay under specialized root")
                .to_vec();
            store
                .paths()
                .lookup(&selection_base_path.join(&rel))
                .unwrap_or(selection_root)
        };
        let arcs = admitted_arcs(
            store,
            out,
            local_stack,
            local_stack,
            specialized_root,
            remote_path_id,
            selection_path_id,
            &mut host_selection_cache,
            arc_stack,
        );
        for (ref_index, (reference, sites)) in arcs.references.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, nested_arc_kind.or(Some(ArcKind::Variants)));
            let ref_index = u16::try_from(ref_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            add_reference_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                reference,
                Some(arc_kind),
                specializes,
                namespace_depth,
                ref_index,
                ArcParent::nested(&branch),
                out,
                &mut visited_refs,
                &mut visited_inherits,
                visited,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }

        // Payloads authored in the specialized class propagate like its
        // references.
        for (payload_index, (payload, sites)) in arcs.payloads.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites, nested_arc_kind.or(Some(ArcKind::Variants)));
            let payload_index = u16::try_from(payload_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                payload,
                Some(arc_kind),
                specializes,
                namespace_depth,
                payload_index,
                ArcParent::nested(&branch),
                out,
                &mut visited_refs,
                &mut visited_inherits,
                visited,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }

    // What the specialized prim's composed index holds beyond this expansion
    // (see `Forwarding`).
    forwarding.copy_composed(store, out, cycles, &mut nodes, &mapping, provenance_remap);
    cycles.exit();
}

/// Orders each prim's children from the `authored_children` and
/// `prim_order` opinions of its sources, whose keys name nodes of the
/// prim's graph in `prims`.
fn apply_child_order(
    store: &dyn LayerStore,
    prims: &HashMap<PathId, PrimIndex>,
    authored_children: &HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    prim_order: &HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    children: &mut HashMap<PathId, Vec<PathId>>,
) {
    for (parent, list) in children.iter_mut() {
        let Some(graph) = prims.get(parent).map(|index| &index.graph) else {
            continue;
        };
        if let Some(opinions) = authored_children.get(parent) {
            apply_authored_children_base_order(store, graph, list, opinions);
        }
        if let Some(opinions) = prim_order.get(parent) {
            apply_prim_order_chain(store, graph, list, opinions);
        };
    }
}

fn apply_authored_children_base_order(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    children: &mut Vec<PathId>,
    opinions: &[(OpinionKey, Vec<TokenId>)],
) {
    // Builds child ordering by processing opinions weakest-first (the weakest
    // source establishes the baseline child order; stronger sources append
    // unique children).
    //
    // The ordering groups opinions by layer, using the combined-stack position
    // derived from inherit/specializes opinions (which walk the full combined
    // stack and carry the correct `layer_strength`). Within each layer group,
    // local opinions come first, then direct opinions, then nested (inherit)
    // opinions.
    //
    // Spec: AOUSD Core §11 (stage population) and supplemental suite composition
    // fixtures that rely on authoring order in referenced layers.
    let mut by_name = HashMap::<TokenId, PathId>::new();
    for child in children.iter().copied() {
        if let Some(name) = store.paths().resolve(child).leaf() {
            by_name.insert(name, child);
        }
    }

    // Build a layer ordering map from inherit/specializes opinions. These
    // opinions walk the combined stack and their `layer_strength` reflects the
    // correct position of each layer in the unified composition order.
    //
    // References and payloads nested inside another arc keep a node strength
    // of `nested_arc_kind = Some(References | Payloads)` for strength ordering,
    // but they do not walk the combined stack and so are not a reliable layer
    // position source; treat them like direct opinions here.
    let walks_combined_stack = |key: &OpinionKey| {
        matches!(
            graph.strength(key.node).nested_arc_kind,
            Some(kind) if !matches!(kind, ArcKind::References | ArcKind::Payloads)
        )
    };
    let mut layer_position: HashMap<LayerId, u16> = HashMap::new();
    for (key, _) in opinions {
        if walks_combined_stack(key) {
            layer_position
                .entry(key.layer_id)
                .and_modify(|pos| *pos = (*pos).min(key.layer_strength))
                .or_insert(key.layer_strength);
        }
    }

    // For layers not seen in inherit opinions, assign a position based on
    // namespace_depth (deeper introduction site = more deeply nested reference
    // = weaker, i.e. higher position number).
    let max_inherit_pos = layer_position.values().copied().max().unwrap_or(0);
    for (key, _) in opinions {
        layer_position.entry(key.layer_id).or_insert_with(|| {
            let strength = graph.strength(key.node);
            if strength.is_local {
                0
            } else {
                // Place after all inherit-discovered layers, offset by
                // namespace_depth to preserve relative ordering among
                // layers that only have direct reference opinions.
                max_inherit_pos + 1 + strength.namespace_depth
            }
        });
    }

    // Sort opinions strongest-first: by layer position (lower = stronger),
    // then local > direct > nested within the same layer.
    let mut sorted: Vec<_> = opinions.iter().collect();
    sorted.sort_by(|a, b| {
        let pos_a = layer_position
            .get(&a.0.layer_id)
            .copied()
            .unwrap_or(u16::MAX);
        let pos_b = layer_position
            .get(&b.0.layer_id)
            .copied()
            .unwrap_or(u16::MAX);
        let pos = pos_a.cmp(&pos_b);
        if pos != Ordering::Equal {
            return pos;
        }
        // Within the same layer: local first, then direct, then nested.
        match (
            graph.strength(a.0.node).is_local,
            graph.strength(b.0.node).is_local,
        ) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        match (walks_combined_stack(&a.0), walks_combined_stack(&b.0)) {
            (false, true) => return Ordering::Less,
            (true, false) => return Ordering::Greater,
            _ => {}
        }
        a.0.layer_strength.cmp(&b.0.layer_strength)
    });

    let mut out = Vec::new();
    let mut seen = HashSet::<PathId>::new();

    // Process weakest-first.
    for (_key, names) in sorted.iter().rev() {
        for name in names.iter() {
            let Some(child_id) = by_name.get(name).copied() else {
                continue;
            };
            if seen.insert(child_id) {
                out.push(child_id);
            }
        }
    }

    // Append remaining children not covered by any opinion.
    for child_id in children.iter().copied() {
        if seen.insert(child_id) {
            out.push(child_id);
        }
    }

    *children = out;
}

fn apply_prim_order_chain(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    children: &mut Vec<PathId>,
    opinions: &[(OpinionKey, Vec<TokenId>)],
) {
    // `reorder nameChildren = [...]` composes as a chain of reorder operations
    // across the prim stack (weak-to-strong), rather than as a single strongest
    // scalar field.
    //
    // This matches the supplemental composition fixtures (e.g.
    // `BasicListEditing_root`).
    let mut sorted = opinions.to_vec();
    sorted.sort_by(|a, b| graph.cmp_keys(&a.0, &b.0));
    for (_, order) in sorted.into_iter().rev() {
        apply_reorder_op(store, children, &order);
    }
}

fn apply_reorder_op(store: &dyn LayerStore, children: &mut Vec<PathId>, order: &[TokenId]) {
    let mut by_name = HashMap::<TokenId, PathId>::new();
    for child in children.iter().copied() {
        if let Some(name) = store.paths().resolve(child).leaf() {
            by_name.insert(name, child);
        }
    }

    let order: Vec<TokenId> = order
        .iter()
        .copied()
        .filter(|name| by_name.contains_key(name))
        .collect();
    let Some((&first, rest)) = order.split_first() else {
        return;
    };

    let order_set: HashSet<TokenId> = order.iter().copied().collect();
    let mut prefix = Vec::new();
    let mut segments: HashMap<TokenId, Vec<PathId>> = HashMap::new();
    let mut current = None;
    for child in children.iter().copied() {
        let Some(name) = store.paths().resolve(child).leaf() else {
            continue;
        };
        if order_set.contains(&name) {
            segments.entry(name).or_default();
            current = Some(name);
        } else if let Some(owner) = current {
            segments.entry(owner).or_default().push(child);
        } else {
            prefix.push(child);
        }
    }

    let mut out = Vec::with_capacity(children.len());
    out.push(by_name[&first]);
    out.extend(prefix);
    if let Some(seg) = segments.get(&first) {
        out.extend(seg.iter().copied());
    }

    for name in rest {
        out.push(by_name[name]);
        if let Some(seg) = segments.get(name) {
            out.extend(seg.iter().copied());
        }
    }

    // Preserve any remaining children (shouldn't happen if `prefix+segments`
    // covered everything, but keep deterministic behavior for partial lists).
    let mut seen = HashSet::<PathId>::new();
    for id in out.iter().copied() {
        seen.insert(id);
    }
    for id in children.iter().copied() {
        if seen.insert(id) {
            out.push(id);
        }
    }

    *children = out;
}

#[cfg(test)]
mod child_order_tests {
    extern crate std;
    use super::*;
    use crate::doc::{InMemoryStore, Layer, PrimSpec, VariantSetSpec, VariantSpec};
    use crate::path::Path;
    use crate::prim_index::OpinionKey;
    use alloc::vec;

    #[test]
    fn test_layer_grouped_sort() {
        let mut store = InMemoryStore::default();
        let root_layer = LayerId(1);
        let set_layer = LayerId(2);
        let prop_layer = LayerId(3);

        let from_root = store.tokens.intern("From_root");
        let from_set = store.tokens.intern("From_set");
        let from_class_root = store.tokens.intern("From_class_in_root");
        let from_class_set = store.tokens.intern("From_class_in_set");
        let geom_tok = store.tokens.intern("geom");

        let c_geom = store
            .paths
            .intern(Path::parse_absolute("/P/I/geom", &mut store.tokens).unwrap());
        let c_fr = store
            .paths
            .intern(Path::parse_absolute("/P/I/From_root", &mut store.tokens).unwrap());
        let c_fs = store
            .paths
            .intern(Path::parse_absolute("/P/I/From_set", &mut store.tokens).unwrap());
        let c_fcr = store
            .paths
            .intern(Path::parse_absolute("/P/I/From_class_in_root", &mut store.tokens).unwrap());
        let c_fcs = store
            .paths
            .intern(Path::parse_absolute("/P/I/From_class_in_set", &mut store.tokens).unwrap());

        let sp_local = store
            .paths
            .intern(Path::parse_absolute("/P/I", &mut store.tokens).unwrap());
        let sp_set = store
            .paths
            .intern(Path::parse_absolute("/S/I", &mut store.tokens).unwrap());
        let sp_prop = store
            .paths
            .intern(Path::parse_absolute("/Prop", &mut store.tokens).unwrap());
        let sp_class = store
            .paths
            .intern(Path::parse_absolute("/_C", &mut store.tokens).unwrap());

        // The prim's local node, a reference at `/P` and one at `/P/I`, and
        // an inherit inside the reference at `/P/I`.
        let reference = |nested_arc_kind, namespace_depth| NodeStrength {
            is_local: false,
            arc_kind: ArcKind::References,
            nested_arc_kind,
            ..NodeStrength::local(namespace_depth)
        };
        let graph = PrimIndexGraph::from_strengths(
            prim_spec_path(&store, sp_local, &[]),
            NodeStrength::local(2),
            [
                reference(None, 1),
                reference(None, 2),
                reference(Some(ArcKind::Inherits), 2),
            ],
        );
        let ids: Vec<NodeId> = graph.nodes().map(|(id, _)| id).collect();
        let opinions: Vec<(OpinionKey, Vec<TokenId>)> = vec![
            (
                OpinionKey {
                    node: ids[0],
                    layer_strength: 0,
                    layer_id: root_layer,
                    lookup_path: sp_local,
                    spec_path: prim_spec_path(&store, sp_local, &[]),
                },
                vec![from_root, geom_tok],
            ),
            (
                OpinionKey {
                    node: ids[1],
                    layer_strength: 0,
                    layer_id: set_layer,
                    lookup_path: sp_set,
                    spec_path: prim_spec_path(&store, sp_set, &[]),
                },
                vec![from_set, geom_tok],
            ),
            (
                OpinionKey {
                    node: ids[2],
                    layer_strength: 0,
                    layer_id: prop_layer,
                    lookup_path: sp_prop,
                    spec_path: prim_spec_path(&store, sp_prop, &[]),
                },
                vec![geom_tok],
            ),
            (
                OpinionKey {
                    node: ids[3],
                    layer_strength: 0,
                    layer_id: root_layer,
                    lookup_path: sp_class,
                    spec_path: prim_spec_path(&store, sp_class, &[]),
                },
                vec![from_class_root, geom_tok],
            ),
            (
                OpinionKey {
                    node: ids[3],
                    layer_strength: 1,
                    layer_id: set_layer,
                    lookup_path: sp_class,
                    spec_path: prim_spec_path(&store, sp_class, &[]),
                },
                vec![from_class_set, geom_tok],
            ),
        ];

        let mut children = vec![c_geom, c_fr, c_fs, c_fcr, c_fcs];
        apply_authored_children_base_order(&store, &graph, &mut children, &opinions);

        let result: Vec<&str> = children
            .iter()
            .map(|c| {
                store
                    .tokens
                    .resolve(store.paths.resolve(*c).leaf().unwrap())
            })
            .collect();

        assert_eq!(
            result,
            vec![
                "geom",
                "From_class_in_set",
                "From_set",
                "From_class_in_root",
                "From_root"
            ]
        );
    }

    /// Test that deeply nested variant children are ordered correctly:
    /// children from deeper nesting levels (more enclosing branches of the
    /// same prim in their spec's `outer_variant_sites`) come before shallower
    /// ones.
    #[test]
    fn test_nested_variant_child_ordering() {
        let mut store = InMemoryStore::default();
        let layer_id = LayerId(1);

        // Intern tokens.
        let standin = store.tokens.intern("standin");
        let shading = store.tokens.intern("shadingVariant");
        let anim = store.tokens.intern("anim");
        let spooky = store.tokens.intern("spooky");

        let sphere = store.tokens.intern("anim_spooky_sphere");
        let anim_sphere = store.tokens.intern("anim_spooky_anim_sphere");

        // Create paths.
        let parent_path = store
            .paths
            .intern(Path::parse_absolute("/D", &mut store.tokens).unwrap());
        let child_sphere = store
            .paths
            .intern(Path::parse_absolute("/D/anim_spooky_sphere", &mut store.tokens).unwrap());
        let child_anim_sphere = store
            .paths
            .intern(Path::parse_absolute("/D/anim_spooky_anim_sphere", &mut store.tokens).unwrap());

        // Build PrimSpec with nested variant sets.
        let mut d_spec = PrimSpec {
            variant_set_order: vec![standin, shading],
            ..PrimSpec::default()
        };
        d_spec.variant_selections.insert(standin, anim);
        d_spec.variant_selections.insert(shading, spooky);

        let site = |set, variant| VariantSelectionSite {
            host_path: parent_path,
            set,
            variant,
        };

        // shadingVariant=spooky, nested in standin=anim: anim_spooky_sphere.
        let mut shading_spooky = VariantSpec::default();
        shading_spooky.authored_children.push(sphere);

        let mut shading_set = VariantSetSpec::default();
        shading_set.variants.insert(spooky, shading_spooky);
        d_spec.variant_sets.insert(shading, shading_set);

        // standin=anim, nested in standin=anim and shadingVariant=spooky:
        // anim_spooky_anim_sphere.
        let mut standin_anim = VariantSpec::default();
        standin_anim.authored_children.push(anim_sphere);

        let mut standin_set = VariantSetSpec::default();
        standin_set.variants.insert(anim, standin_anim);
        d_spec.variant_sets.insert(standin, standin_set);

        // Build layer.
        let mut layer = Layer::new(layer_id);
        layer.prims.insert(parent_path, d_spec);

        // Add the children's branch specs.
        layer.insert_prim(
            child_sphere,
            PrimSpec {
                outer_variant_sites: vec![site(standin, anim), site(shading, spooky)],
                ..PrimSpec::def()
            },
        );
        layer.insert_prim(
            child_anim_sphere,
            PrimSpec {
                outer_variant_sites: vec![
                    site(standin, anim),
                    site(shading, spooky),
                    site(standin, anim),
                ],
                ..PrimSpec::def()
            },
        );

        store.insert_layer(layer);

        // Build prim index.
        let mut prims = HashMap::new();
        prims.insert(
            parent_path,
            PrimIndex {
                sources: vec![OpinionKey {
                    node: NodeId::ROOT,
                    layer_strength: 0,
                    layer_id,
                    lookup_path: parent_path,
                    spec_path: prim_spec_path(&store, parent_path, &[]),
                }],
                ..PrimIndex::new(PrimIndexGraph::from_strengths(
                    prim_spec_path(&store, parent_path, &[]),
                    NodeStrength::local(1),
                    [],
                ))
            },
        );

        // Children list (initial order from population).
        let mut children = HashMap::new();
        children.insert(parent_path, vec![child_sphere, child_anim_sphere]);

        filter_variant_children(&store, &prims, &mut children);

        let result: Vec<&str> = children[&parent_path]
            .iter()
            .map(|c| {
                store
                    .tokens
                    .resolve(store.paths.resolve(*c).leaf().unwrap())
            })
            .collect();

        // Deeper nesting (depth 2) should come before shallower (depth 1).
        assert_eq!(
            result,
            vec!["anim_spooky_anim_sphere", "anim_spooky_sphere"]
        );
    }
}

#[cfg(test)]
mod instancing_tests {
    use super::*;
    use crate::{
        array_edit::{ArrayEdit, ArrayEditOp},
        doc::{InMemoryStore, Layer, PrimSpec, Value},
        listop::ListOp,
        path::PropertyPath,
        property::{PropertySpec, PropertyType},
        stage::ResolvedValue,
    };
    use alloc::vec;

    /// An instance descendant drops the site reached through an arc above the
    /// instance together with its declaration, so the composed type comes
    /// from the instance's own asset: `double[]`, not the discarded `int[]`.
    ///
    /// Spec: AOUSD Core §11.3.3 (scene graph instancing).
    #[test]
    fn instance_descendant_keeps_contributing_declaration_type() {
        let mut store = InMemoryStore::default();
        let x = store.tokens.intern("x");
        let reference = |layer: u64, path| ListOp {
            explicit: Some(vec![Reference::new(LayerId(layer), path)]),
            ..ListOp::default()
        };

        // root.usda: def "P" (references = @group@</Group>) {}
        let p = store.path("/P");
        let group = store.path("/Group");
        let mut root = Layer::new(LayerId(1));
        root.insert_prim(
            p,
            PrimSpec {
                references: reference(2, group),
                ..PrimSpec::def()
            },
        );
        store.insert_layer(root);

        // group.usda: /Group/I is an instance of @asset@</Asset> and authors
        // `int[] x = [99]` on its child `C`.
        let instance = store.path("/Group/I");
        let group_c = store.path("/Group/I/C");
        let asset = store.path("/Asset");
        let i_tok = store.tokens.intern("I");
        let c_tok = store.tokens.intern("C");
        let mut group_layer = Layer::new(LayerId(2));
        group_layer.insert_prim(
            group,
            PrimSpec {
                authored_children: vec![i_tok],
                ..PrimSpec::def()
            },
        );
        group_layer.insert_prim(
            instance,
            PrimSpec {
                instanceable: Some(true),
                references: reference(3, asset),
                authored_children: vec![c_tok],
                ..PrimSpec::def()
            },
        );
        let over_c = PrimSpec::over().with_property(
            x,
            PropertySpec::typed_attribute(PropertyType::new("int", true, Value::Int(0)))
                .with_default(Value::Array(vec![Value::Int(99)])),
        );
        group_layer.insert_prim(group_c, over_c);
        store.insert_layer(group_layer);

        // asset.usda: def "C" { double[] x = edit [resize 2] }
        let asset_c = store.path("/Asset/C");
        let mut asset_layer = Layer::new(LayerId(3));
        asset_layer.insert_prim(
            asset,
            PrimSpec {
                authored_children: vec![c_tok],
                ..PrimSpec::def()
            },
        );
        let def_c = PrimSpec::def().with_property(
            x,
            PropertySpec::typed_attribute(PropertyType::new("double", true, Value::Double(0.0)))
                .with_default(Value::ArrayEdit(ArrayEdit {
                    ops: vec![ArrayEditOp::Resize { len: 2 }],
                })),
        );
        asset_layer.insert_prim(asset_c, def_c);
        store.insert_layer(asset_layer);

        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let c = store.path("/P/I/C");
        let stack: Vec<_> = stage
            .explain_property_path(PropertyPath::new(c, x))
            .expect("x has opinions")
            .iter()
            .map(|opinion| (opinion.key.layer_id, opinion.key.lookup_path))
            .collect();
        assert_eq!(stack, [(LayerId(3), asset_c)], "only the asset contributes");
        let sources: Vec<_> = stage
            .explain_prim(c)
            .expect("C is composed")
            .iter()
            .map(|key| (key.layer_id, key.lookup_path))
            .collect();
        assert_eq!(sources, [(LayerId(3), asset_c)]);
        assert_eq!(
            stage
                .resolve_property_path(PropertyPath::new(c, x))
                .map(|resolved| resolved.value),
            Some(ResolvedValue::Scalar(Value::Array(vec![
                Value::Double(0.0),
                Value::Double(0.0)
            ])))
        );
    }
}

/// References and payloads with no authored prim path, which target the
/// `defaultPrim` of their layer.
///
/// Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim`), §10.3.2.1 (references),
/// §10.3.2.2 (payloads). Expectations follow OpenUSD 26.08; the conformance
/// crate's `default_prim` test replays the same cases from files.
#[cfg(test)]
mod default_prim_tests {
    use super::*;
    use crate::{
        composition_error::UnresolvedDefaultPrim,
        doc::{InMemoryStore, Layer, PrimSpec},
    };
    use alloc::{string::String, vec};

    const ROOT: LayerId = LayerId(1);
    const ASSET: LayerId = LayerId(2);
    const INNER: LayerId = LayerId(3);

    /// A layer defining `/Model/Geo/Mesh` and `/Other/OtherChild`, with the
    /// given `defaultPrim`.
    fn asset(store: &mut InMemoryStore, id: LayerId, default_prim: Option<&str>) -> Layer {
        let mut layer = Layer::new(id);
        layer.default_prim = default_prim.map(|name| store.tokens.intern(name));
        for (path, child) in [
            ("/Model", Some("Geo")),
            ("/Model/Geo", Some("Mesh")),
            ("/Model/Geo/Mesh", None),
            ("/Other", Some("OtherChild")),
            ("/Other/OtherChild", None),
        ] {
            let children = child.map(|c| store.tokens.intern(c)).into_iter().collect();
            let path = store.path(path);
            layer.insert_prim(path, PrimSpec::def().with_children(children));
        }
        layer
    }

    /// Inserts a root layer holding `prims`, plus `layers`, and composes it.
    fn compose(
        store: &mut InMemoryStore,
        default_prim: Option<&str>,
        prims: Vec<(&str, PrimSpec)>,
        layers: Vec<Layer>,
    ) -> Stage {
        let mut root = Layer::new(ROOT);
        root.default_prim = default_prim.map(|name| store.tokens.intern(name));
        for (path, spec) in prims {
            let id = store.path(path);
            root.insert_prim(id, spec);
        }
        store.insert_layer(root);
        for layer in layers {
            store.insert_layer(layer);
        }
        Stage::compose(store, ROOT, StageOptions::default())
    }

    /// The child names of `prim`, or `None` when it is not composed.
    fn children(stage: &Stage, store: &mut InMemoryStore, prim: &str) -> Option<Vec<String>> {
        let prim = store.path(prim);
        if !stage.has_prim(prim) {
            return None;
        }
        Some(
            stage
                .children_of(prim)
                .unwrap_or(&[])
                .iter()
                .map(|child| {
                    let leaf = store.paths.resolve(*child).leaf().expect("child name");
                    String::from(store.tokens.resolve(leaf))
                })
                .collect(),
        )
    }

    /// The `(layer, path)` sites of `prim`'s prim stack.
    fn sites(stage: &Stage, store: &mut InMemoryStore, prim: &str) -> Vec<(LayerId, PathId)> {
        let prim = store.path(prim);
        stage
            .explain_prim(prim)
            .expect("prim is composed")
            .iter()
            .map(|key| (key.layer_id, key.lookup_path))
            .collect()
    }

    fn unresolved(
        store: &mut InMemoryStore,
        prim: &str,
        arc: ArcKind,
        layer: LayerId,
        path: Option<&str>,
    ) -> CompositionError {
        CompositionError::UnresolvedDefaultPrim(UnresolvedDefaultPrim {
            prim: store.path(prim),
            arc,
            layer,
            path: path.map(|path| store.path(path)),
        })
    }

    #[test]
    fn two_placements_compose_only_the_default_prim() {
        let mut store = InMemoryStore::default();
        let asset = asset(&mut store, ASSET, Some("Model"));
        let other = store.path("/Other");
        let stage = compose(
            &mut store,
            None,
            vec![
                (
                    "/A",
                    PrimSpec::def().with_reference(Reference::to_default_prim(ASSET)),
                ),
                (
                    "/B",
                    PrimSpec::def().with_payload(Reference::to_default_prim(ASSET)),
                ),
                (
                    "/Explicit",
                    PrimSpec::def().with_reference(Reference::new(ASSET, other)),
                ),
            ],
            vec![asset],
        );

        let model = store.path("/Model");
        for placement in ["/A", "/B"] {
            assert_eq!(
                children(&stage, &mut store, placement),
                Some(vec!["Geo".into()]),
                "{placement} composes the default prim's children only"
            );
            let root_site = store.path(placement);
            assert_eq!(
                sites(&stage, &mut store, placement),
                [(ROOT, root_site), (ASSET, model)],
                "{placement} shows the selected source"
            );
        }
        assert_eq!(
            children(&stage, &mut store, "/A/Geo"),
            Some(vec!["Mesh".into()])
        );
        assert_eq!(children(&stage, &mut store, "/A/OtherChild"), None);
        assert_eq!(children(&stage, &mut store, "/Other"), None);
        assert_eq!(
            children(&stage, &mut store, "/Explicit"),
            Some(vec!["OtherChild".into()]),
            "an explicit target ignores `defaultPrim`"
        );
        assert_eq!(stage.composition_errors(), []);
    }

    #[test]
    fn default_prim_may_name_a_subroot_prim() {
        let mut store = InMemoryStore::default();
        let asset = asset(&mut store, ASSET, Some("Model/Geo"));
        let stage = compose(
            &mut store,
            None,
            vec![(
                "/A",
                PrimSpec::def().with_reference(Reference::to_default_prim(ASSET)),
            )],
            vec![asset],
        );
        assert_eq!(
            children(&stage, &mut store, "/A"),
            Some(vec!["Mesh".into()])
        );
        assert_eq!(stage.composition_errors(), []);
    }

    #[test]
    fn internal_arc_targets_the_stack_roots_default_prim() {
        let mut store = InMemoryStore::default();
        let local_child = store.tokens.intern("LocalChild");
        let stage = compose(
            &mut store,
            Some("Local"),
            vec![
                ("/Local", PrimSpec::def().with_children(vec![local_child])),
                ("/Local/LocalChild", PrimSpec::def()),
                (
                    "/A",
                    PrimSpec::def().with_reference(Reference::to_default_prim(ROOT)),
                ),
            ],
            vec![],
        );
        assert_eq!(
            children(&stage, &mut store, "/A"),
            Some(vec!["LocalChild".into()])
        );
        assert_eq!(stage.composition_errors(), []);
    }

    /// An internal arc authored in a sublayer targets the whole layer stack:
    /// its node is in the stack rooted at the root layer, it reads the root
    /// layer's specs, and `<>` names the root layer's `defaultPrim`.
    ///
    /// Spec: AOUSD Core §10.3.2.1. OpenUSD: `_EvalRefOrPayloadArcs` in
    /// `pxr/usd/pcp/primIndex.cpp`.
    #[test]
    fn internal_arc_in_a_sublayer_targets_the_containing_stack() {
        let mut store = InMemoryStore::default();
        let mut sub = asset(&mut store, INNER, Some("Other"));
        let a = store.path("/A");
        let b = store.path("/B");
        let model = store.path("/Model");
        sub.insert_prim(
            a,
            PrimSpec::def().with_reference(Reference::to_default_prim(INNER)),
        );
        sub.insert_prim(
            b,
            PrimSpec::def().with_payload(Reference::new(INNER, model)),
        );
        let stage = {
            let mut root = Layer::new(ROOT);
            root.default_prim = Some(store.tokens.intern("Model"));
            root.sublayers.push(INNER.into());
            root.insert_prim(model, PrimSpec::over());
            store.insert_layer(root);
            store.insert_layer(sub);
            Stage::compose(&mut store, ROOT, StageOptions::default())
        };

        for (placement, root_site) in [("/A", a), ("/B", b)] {
            assert_eq!(
                sites(&stage, &mut store, placement),
                [(INNER, root_site), (ROOT, model), (INNER, model)],
                "{placement} reads the root layer stack at `/Model`"
            );
            let prim = store.path(placement);
            let graph = stage.explain_prim_graph(prim).expect("graph");
            assert!(
                graph.nodes().all(|(_, node)| node.layer_stack() == ROOT),
                "{placement}'s nodes are in the root layer stack"
            );
        }
        assert_eq!(stage.composition_errors(), []);
    }

    #[test]
    fn missing_or_invalid_default_prim_is_reported() {
        for (default_prim, arc, path) in [
            (None, ArcKind::References, None),
            (Some("Model.attr"), ArcKind::Payloads, None),
            (Some("1Model"), ArcKind::References, None),
            (Some("Missing"), ArcKind::Payloads, Some("/Missing")),
            (
                Some("Model/Missing"),
                ArcKind::References,
                Some("/Model/Missing"),
            ),
        ] {
            let mut store = InMemoryStore::default();
            let asset = asset(&mut store, ASSET, default_prim);
            let reference = Reference::to_default_prim(ASSET);
            let spec = match arc {
                ArcKind::Payloads => PrimSpec::def().with_payload(reference),
                _ => PrimSpec::def().with_reference(reference),
            };
            let stage = compose(&mut store, None, vec![("/A", spec)], vec![asset]);

            assert_eq!(
                children(&stage, &mut store, "/A"),
                Some(vec![]),
                "defaultPrim {default_prim:?}: the arc contributes nothing"
            );
            let a = store.path("/A");
            assert_eq!(sites(&stage, &mut store, "/A"), [(ROOT, a)]);
            let expected = unresolved(&mut store, "/A", arc, ASSET, path);
            assert_eq!(
                stage.composition_errors(),
                [expected],
                "defaultPrim {default_prim:?}"
            );
        }
    }

    #[test]
    fn unresolved_default_prim_inside_an_asset_is_reported_on_the_placement() {
        // `/A` references the asset's `/Model` explicitly; `/Model` in turn
        // references an inner layer without a `defaultPrim`.
        let mut store = InMemoryStore::default();
        let mut outer = asset(&mut store, ASSET, None);
        let model = store.path("/Model");
        let inner = asset(&mut store, INNER, None);
        outer
            .prims
            .get_mut(&model)
            .expect("asset defines /Model")
            .add_reference(Reference::to_default_prim(INNER));
        let stage = compose(
            &mut store,
            None,
            vec![(
                "/A",
                PrimSpec::def().with_reference(Reference::new(ASSET, model)),
            )],
            vec![outer, inner],
        );
        assert_eq!(children(&stage, &mut store, "/A"), Some(vec!["Geo".into()]));
        let expected = unresolved(&mut store, "/A", ArcKind::References, INNER, None);
        assert_eq!(stage.composition_errors(), [expected]);
    }

    /// An arc to an asset that could not be resolved contributes nothing and
    /// is reported, even though the authoring layer has a `defaultPrim` (and
    /// the explicit target's path) it could otherwise fall back to.
    #[test]
    fn unresolved_asset_never_falls_back_to_the_authoring_layer() {
        let mut store = InMemoryStore::default();
        let value = store.tokens.intern("value");
        let local = store.path("/Local");
        let arcs = [
            ("/Ref", ArcKind::References, ReferenceTarget::DefaultPrim),
            ("/Pay", ArcKind::Payloads, ReferenceTarget::DefaultPrim),
            (
                "/RefExplicit",
                ArcKind::References,
                ReferenceTarget::Prim(local),
            ),
            (
                "/PayExplicit",
                ArcKind::Payloads,
                ReferenceTarget::Prim(local),
            ),
        ];
        let mut prims = vec![(
            "/Local",
            PrimSpec::def()
                .with_property(value, crate::PropertySpec::attribute().with_default(7_i64)),
        )];
        for (path, arc, target) in &arcs {
            let reference =
                Reference::unresolved("./absent.usda", target.clone(), LayerOffset::IDENTITY);
            assert!(reference.is_unresolved());
            let spec = match arc {
                ArcKind::Payloads => PrimSpec::def().with_payload(reference),
                _ => PrimSpec::def().with_reference(reference),
            };
            prims.push((path, spec));
        }
        let stage = compose(&mut store, Some("Local"), prims, vec![]);

        let mut expected = Vec::new();
        for (path, arc, _) in arcs {
            let prim = store.path(path);
            assert_eq!(sites(&stage, &mut store, path), [(ROOT, prim)]);
            assert!(
                stage
                    .resolve_field_path(PropertyPath::new(prim, value))
                    .is_none(),
                "{path} resolves no value"
            );
            expected.push(CompositionError::UnresolvedAsset(UnresolvedAsset {
                prim,
                arc,
                asset: "./absent.usda".into(),
            }));
        }
        let errors = stage.composition_errors();
        assert_eq!(errors.len(), expected.len(), "{errors:?}");
        for error in &expected {
            assert!(errors.contains(error), "missing {error:?} in {errors:?}");
        }
    }
}
