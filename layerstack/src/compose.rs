// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition entry points.
//!
//! This module is responsible for producing composition results (`PrimIndex`es)
//! which are then wrapped by [`crate::stage::Stage`].
//!
//! Spec: AOUSD Core §9–§12 (layer stacks, arcs/strength ordering, population, and resolution).

use alloc::{borrow::Cow, collections::BTreeSet, rc::Rc, vec::Vec};

use core::cmp::Ordering;

use hashbrown::{HashMap, HashSet};

use crate::{
    arc_cycle::CycleDetector,
    arcs::{
        ArcAuthoring, AuthoredReference, SelectionScope, anchor_internal_arcs,
        lookup_reference_target_path, resolve_branch_payloads_in,
        resolve_direct_references_for_prim, resolve_inherits_for_prim,
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
    prim_index_graph::{NodeArc, NodeId, PrimIndexGraph, PrimNode},
    property::PropertyType,
    relocates::RelocationTable,
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
    // Spec: AOUSD Core §10.3.2.6 (invalid relocates are composition errors
    // of the layer stack authoring them).
    let mut relocation_errors = Vec::new();
    RelocationTable::compute(store, &layer_stack, &mut relocation_errors);
    for error in relocation_errors {
        cycles.report(error);
    }
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
                skips_duplicates: false,
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

    for (path, prim) in &mut prims {
        drop_skipped_duplicates(prim);
        prune_skipped_nodes(
            prim,
            [
                prim_order_opinions.get_mut(path),
                authored_children_opinions.get_mut(path),
            ],
        );
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

    let mut instances = strip_instance_descendants(
        store,
        &mut prims,
        &mut children,
        &authored_children_opinions,
    );

    prune_deactivated(store, &mut prims, &mut children);

    // Runs last so the ordering passes above see the populated child lists;
    // removal only drops entries.
    remove_prims_without_specs(store, &mut prims, &mut children);
    instances.retain(|instance| prims.contains_key(instance));
    crate::path_expression::anchor_opinions(store, &mut prims);

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
        .with_instances(instances)
}

/// The arc path of `node` (see [`PrimIndexGraph::arc_path`]) without the
/// variant branches of the composed prim's own layer stack that enclose the
/// arcs beneath them.
///
/// For the child-ordering and instancing passes, which ask which kinds of
/// arc introduce a site rather than how it ranks.
fn introducing_arcs(graph: &PrimIndexGraph, node: NodeId) -> Vec<&PrimNode> {
    let mut path = graph.arc_path(node);
    let local_variants = path
        .iter()
        .take(path.len().saturating_sub(1))
        .take_while(|node| node.arc_kind() == ArcKind::Variants)
        .count();
    path.drain(..local_variants);
    path
}

/// The arc whose target holds `node`: `node` itself, or for a variant
/// branch the arc hosting its variant set; `None` for the root and its own
/// variant branches.
fn enclosing_arc(graph: &PrimIndexGraph, node: NodeId) -> Option<&PrimNode> {
    let mut cursor = graph.node(node)?;
    while cursor.arc_kind() == ArcKind::Variants {
        cursor = graph.node(cursor.parent()?)?;
    }
    cursor.parent().map(|_| cursor)
}

/// Drops the registrations that nodes skipping duplicates (see
/// [`NodeArc::skips_duplicates`]) made of sites another node registers,
/// and every such registration of a site but the strongest.
///
/// OpenUSD adds no node for a site the prim index already uses while it
/// builds the recursive index of a class arc's ancestors, whichever arc
/// reaches the site first in its evaluation order; composition here does
/// not expand arcs in that order, so it adds those nodes and drops their
/// duplicate registrations once the graph is complete.
fn drop_skipped_duplicates(prim: &mut PrimIndex) {
    let registration = |key: &OpinionKey| OpinionKey {
        spec_path: key.spec_path.prim_spec(),
        ..key.clone()
    };
    prim.graph.rank();
    let dropped: HashSet<OpinionKey> = {
        let graph = &prim.graph;
        let skips = |node: NodeId| {
            graph
                .node(node)
                .is_some_and(|node| node.arc.skips_duplicates)
        };
        let mut sources: Vec<&OpinionKey> = prim.sources.iter().collect();
        sources.sort_by(|a, b| graph.cmp_keys(a, b));
        sources.dedup();
        let mut kept: HashSet<(LayerId, &SpecPath)> = sources
            .iter()
            .filter(|key| !skips(key.node))
            .map(|key| (key.layer_id, &key.spec_path))
            .collect();
        sources
            .into_iter()
            .filter(|key| skips(key.node) && !kept.insert((key.layer_id, &key.spec_path)))
            .cloned()
            .collect()
    };
    let graph = &prim.graph;
    let skips = |node: NodeId| {
        graph
            .node(node)
            .is_some_and(|node| node.arc.skips_duplicates)
    };
    let mut seen = HashSet::new();
    prim.sources
        .retain(|key| !skips(key.node) || (!dropped.contains(key) && seen.insert(key.clone())));
    for opinions in prim.opinions_by_field.values_mut() {
        let mut seen = HashSet::new();
        opinions.retain(|opinion| {
            !skips(opinion.key.node)
                || (!dropped.contains(&registration(&opinion.key))
                    && seen.insert(opinion.key.clone()))
        });
    }
    prim.opinions_by_field
        .retain(|_, opinions| !opinions.is_empty());
}

/// The child-order opinions (`reorder nameChildren`, or authored children)
/// of one composed prim, each with its key.
type ChildOrderOpinions = Vec<(OpinionKey, Vec<TokenId>)>;

/// Removes from the prim's graph the nodes skipping duplicates (see
/// [`drop_skipped_duplicates`]) that no opinion, source or declaration
/// names any more, with none beneath them that one does, and renumbers
/// every key: the prim's own and `extra`, the child-order opinions composed
/// beside it.
fn prune_skipped_nodes(prim: &mut PrimIndex, extra: [Option<&mut ChildOrderOpinions>; 2]) {
    let mut used = alloc::vec![false; prim.graph.len()];
    let mut mark = |key: &OpinionKey| used[key.node.index()] = true;
    prim.sources.iter().for_each(&mut mark);
    prim.opinions_by_field
        .values()
        .flatten()
        .for_each(|opinion| mark(&opinion.key));
    prim.property_types_by_field
        .values()
        .flatten()
        .for_each(|(key, _)| mark(key));
    for opinions in extra.iter().flatten() {
        opinions.iter().for_each(|(key, _)| mark(key));
    }
    if !prim
        .graph
        .nodes()
        .any(|(id, node)| node.arc.skips_duplicates && !used[id.index()])
    {
        return;
    }
    let remap = prim
        .graph
        .retain_nodes(|id, node| !node.arc.skips_duplicates || used[id.index()]);
    let renumber = |key: &mut OpinionKey| {
        key.node = remap[key.node.index()].expect("a used node is kept");
    };
    prim.sources.iter_mut().for_each(renumber);
    prim.opinions_by_field
        .values_mut()
        .flatten()
        .for_each(|opinion| renumber(&mut opinion.key));
    prim.property_types_by_field
        .values_mut()
        .flatten()
        .for_each(|(key, _)| renumber(key));
    for opinions in extra.into_iter().flatten() {
        opinions.iter_mut().for_each(|(key, _)| renumber(key));
    }
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
                        key.node,
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
    // The node of the composed prim's graph that reads the spec.
    node: NodeId,
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

        let host_path = crate::path::Path::root().join(&host_segments);
        let host = paths.lookup(&host_path);
        let composed_host =
            stage_host_path(store, &prims[&prim_path].graph, prim_path, node, host_path);
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

/// The stage prim path the prim path `host`, in the namespace of the site of
/// `node` in the graph of the composed prim `dest`, maps to through the arcs
/// above the node; `None` when an arc that maps only its target stands in
/// the way.
///
/// A node introduced at namespace depth `d` maps the ancestor of its site
/// `depth(dest) - d` levels up, the target its arc names (an ancestral arc
/// of a subroot target, see [`AncestralArcs`], names an ancestor of it), to
/// the same ancestor of its parent's site. A class arc maps every other path
/// to itself, as an internal reference does, and a variant branch keeps its
/// host's namespace.
///
/// OpenUSD translates the host of a variant set toward the root this way to
/// find the strongest site that selects it
/// (`Pcp_TranslatePathFromNodeToRootOrClosestNode`, used by
/// `_ComposeVariantSelection` in `pxr/usd/pcp/primIndex.cpp`).
fn stage_host_path(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    dest: PathId,
    node: NodeId,
    host: crate::path::Path,
) -> Option<crate::path::Path> {
    let paths = store.paths();
    let dest_depth = paths.resolve(dest).depth();
    let mut host = host;
    let mut cursor = graph.node(node)?;
    while let Some(parent_id) = cursor.parent() {
        let parent = graph.node(parent_id)?;
        if cursor.arc_kind() == ArcKind::Variants {
            cursor = parent;
            continue;
        }
        let levels = dest_depth.saturating_sub(usize::from(cursor.namespace_depth()));
        let ancestor = |path: &crate::path::Path| {
            let depth = path.depth().checked_sub(levels)?;
            Some(crate::path::Path::root().join(&path.segments()[..depth]))
        };
        let site = paths.resolve(cursor.site().prim_path());
        let parent_site = paths.resolve(parent.site().prim_path());
        let mapped = ancestor(site)
            .zip(ancestor(parent_site))
            .and_then(|(target, source)| Some(source.join(host.strip_prefix(&target)?)));
        host = match mapped {
            Some(mapped) => mapped,
            None if matches!(cursor.arc_kind(), ArcKind::Inherits | ArcKind::Specializes)
                || cursor.layer_stack() == parent.layer_stack() =>
            {
                host
            }
            None => return None,
        };
        cursor = parent;
    }
    Some(host)
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
                        let arc_list_index = enclosing_arc(&prim_index.graph, source.node)
                            .map_or(0, PrimNode::sibling_index);
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
            let class_based = introducing_arcs(&prim_index.graph, source.node)
                .iter()
                .take(2)
                .any(|node| matches!(node.arc_kind(), ArcKind::Inherits | ArcKind::Specializes));
            if class_based {
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
        // `fold_child_order` already established the
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
///    `Pcp_ChildNodeIsInstanceable`). An arc authored beneath the instance
///    survives only when the node it is authored at does, so the local
///    opinions' own arcs are stripped with them (see
///    [`contributes_beneath_instance`]).
///
/// Returns the effective instances.
///
/// Spec: AOUSD Core §11.3.3 (scene graph instancing: only opinions brought
/// in by the instance's composition arcs are used), §5.1.14 (instanceable).
fn strip_instance_descendants(
    store: &dyn LayerStore,
    prims: &mut HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
    authored_children_opinions: &HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
) -> HashSet<PathId> {
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
            introducing_arcs(&index.graph, s.node)
                .first()
                .is_some_and(|node| {
                    matches!(
                        node.arc_kind(),
                        ArcKind::References | ArcKind::Payloads | ArcKind::Inherits
                    )
                })
        });
        if !has_arcs {
            continue;
        }

        // Collect identity paths: local sources and sources with instanceable=true.
        let mut identity_paths: Vec<(LayerId, PathId)> = Vec::new();
        for source in &index.sources {
            if source.node == NodeId::ROOT {
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
                if key.node != NodeId::ROOT {
                    return contributes_beneath_instance(graph, key.node, instance_depth);
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

    instance_identity
        .into_iter()
        .map(|(instance_path, _)| instance_path)
        .filter(|instance_path| prims.contains_key(instance_path))
        .collect()
}

/// Whether `node` of an instance descendant's graph still contributes
/// opinions, for an instance at namespace depth `instance_depth`.
///
/// Beneath an instance, OpenUSD marks the root node inert, along with every
/// node reached through an arc authored above the instance: they hold
/// opinions local to the instance, which are not part of its prototype.
/// An inert node's own arcs are never evaluated, so an arc authored beneath
/// the instance survives only when the node it is authored at does. An arc
/// authored at the instance itself survives: the instance's own arcs bring
/// in the prototype.
///
/// Spec: AOUSD Core §11.3.3 (scene graph instancing). OpenUSD:
/// `Pcp_ChildNodeIsInstanceable` in `pxr/usd/pcp/instancing.h`, and
/// `_ConvertNodeForChild` in `pxr/usd/pcp/primIndex.cpp`, which marks the
/// other nodes inert.
fn contributes_beneath_instance(graph: &PrimIndexGraph, node: NodeId, instance_depth: u16) -> bool {
    let mut cursor = node;
    loop {
        let Some(current) = graph.node(cursor) else {
            return false;
        };
        let Some(parent) = current.parent() else {
            // The root node is inert beneath an instance.
            return false;
        };
        match current.namespace_depth().cmp(&instance_depth) {
            Ordering::Less => return false,
            Ordering::Equal => return true,
            Ordering::Greater => cursor = parent,
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
/// above `arc_target` lie outside the arc, so only `ancestor_stack` selects
/// their variants: the target layer stack, or for a class arc, which maps
/// every path outside the class to itself, the layers of the stronger layer
/// stacks the class is implied into as well.
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
    ancestor_stack: &LayerStack,
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
                            index.graph.sort_keys(&mut sources);
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
                None => resolve_full_variant_selections(store, ancestor_stack, host),
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
/// A reference or payload carries the offset of the layer that authors it
/// (see [`ArcAuthoring::authored_reference`]).
#[derive(Debug, Default)]
struct AdmittedArcs {
    inherits: Vec<(PathId, Vec<VariantSelectionSite>)>,
    specializes: Vec<(PathId, Vec<VariantSelectionSite>)>,
    references: Vec<(AuthoredReference, Vec<VariantSelectionSite>)>,
    payloads: Vec<(AuthoredReference, Vec<VariantSelectionSite>)>,
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
    // Selects the variants of hosts above `arc_target` (see
    // `enclosing_variant_selections`).
    ancestor_stack: &LayerStack,
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
        ancestor_stack,
        arc_target,
        remote_path,
        dest_path,
        cache,
    );
    arcs_admitted_by(store, data_stack, remote_path, &enclosing, anchor)
}

/// The arcs authored for `remote_path` in `data_stack`, admitted for the
/// selections `enclosing` names for the variant hosts enclosing it (see
/// [`admitted_arcs`]).
fn arcs_admitted_by(
    store: &dyn LayerStore,
    data_stack: &LayerStack,
    remote_path: PathId,
    enclosing: &HashMap<PathId, HashMap<TokenId, TokenId>>,
    anchor: LayerId,
) -> AdmittedArcs {
    let selections = enclosing.get(&remote_path).cloned().unwrap_or_default();
    let parent_selections = store
        .paths()
        .resolve(remote_path)
        .parent()
        .and_then(|parent| store.paths().lookup(&parent))
        .and_then(|parent| enclosing.get(&parent).cloned())
        .unwrap_or_default();
    let scope = SelectionScope::Composed(enclosing);

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
                authoring.authored_reference(
                    item,
                    |spec| &spec.references,
                    |b| &b.references,
                    anchor,
                )
            })
            .collect(),
        payloads: payloads
            .into_iter()
            .map(|item| {
                authoring.authored_reference(item, |spec| &spec.payloads, |b| &b.payloads, anchor)
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
    // Selects the variants of hosts above `arc_target` (see
    // `enclosing_variant_selections`).
    ancestor_stack: &LayerStack,
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
            ancestor_stack,
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
    let mut visited_inherits: VisitedClasses = VisitedClasses::new();
    let mut visited_specializes = VisitedClasses::new();
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
            let (reference, sites) = ArcAuthoring {
                store,
                stack: local_stack,
                prim: dest_root,
                selections: &selections,
                scope: SelectionScope::Stack,
            }
            .authored_reference(
                reference,
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
    let mut visited: VisitedClasses = VisitedClasses::new();
    let mut visited_specializes = VisitedClasses::new();
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
                local_stack,
                dest_root,
                inherited_root,
                cycles.stage_layer_stack(),
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
    /// node ranks with the prim's local variant opinions, at the namespace
    /// depth of the prim hosting the variant set, whatever the step's
    /// `namespace_depth`: an ancestor's variant set is introduced at that
    /// ancestor's depth (OpenUSD `_AddAncestralVariantArc` in
    /// `pxr/usd/pcp/primIndex.cpp`, `PcpNode_GetNonVariantPathElementCount`
    /// of the variant set's path).
    LocalVariant(VariantSelectionSite),
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
    /// For an implied class arc, the arc path of the node it is implied
    /// from (see [`PrimNode::origin`]).
    origin: Option<Rc<[Self]>>,
    /// The offset the layers of the step's layer stack are read with,
    /// before their sublayer offsets.
    layer_offset: LayerOffset,
    /// The layers whose offsets `layer_offset` includes: the layer that
    /// authors each reference or payload on the way to the step, outermost
    /// first. Every prim the step reaches depends on them (see
    /// [`record_offset_layers`]).
    offset_layers: Rc<[LayerId]>,
    /// `true` for a step added beneath an ancestral arc of a class arc's
    /// target, where a node duplicating a site of the graph is skipped (see
    /// [`NodeArc::skips_duplicates`]).
    skips_duplicates: bool,
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
        let (site, namespace_depth) = match self.target {
            StepTarget::Namespace {
                dest_root,
                target_root,
            } => {
                cursor.prim = map_namespace(store, dest, dest_root, target_root);
                cursor.variants.clear();
                (
                    SpecPath::from_prim_path(cursor.prim, store.paths()),
                    self.namespace_depth,
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
                if let StepTarget::LocalVariant(local) = self.target {
                    let depth =
                        u16::try_from(paths.resolve(local.host_path).depth()).unwrap_or(u16::MAX);
                    (site, depth)
                } else {
                    (site, self.namespace_depth)
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
            skips_duplicates: self.skips_duplicates,
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
fn intern_steps(
    store: &mut dyn LayerStore,
    out: &mut HashMap<PathId, PrimIndex>,
    dest: PathId,
    steps: &[ArcStep],
    cursor: &mut PathCursor,
) {
    for step in steps {
        let Some(arc) = step.arc(store, dest, cursor) else {
            continue;
        };
        let graph = &mut out.get_mut(&dest).expect("path exists").graph;
        cursor.node = graph.intern_child(cursor.node, arc);
        if let Some(origin) = &step.origin {
            let node = cursor.node;
            if graph.node(node).and_then(PrimNode::origin).is_none() {
                let mut origin_cursor = PathCursor::root(dest);
                intern_steps(store, out, dest, origin, &mut origin_cursor);
                let graph = &mut out.get_mut(&dest).expect("path exists").graph;
                graph.set_origin(node, origin_cursor.node);
            }
        }
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
            origin: None,
            layer_offset: LayerOffset::IDENTITY,
            offset_layers: Rc::from([]),
            skips_duplicates: false,
        })
        .collect()
}

/// The arcs an arc expansion is nested in, outermost first, and whether the
/// expansion is a class arc implied into a stronger layer stack.
#[derive(Clone, Debug)]
struct ArcParent<'a> {
    steps: &'a [ArcStep],
    implied: bool,
    /// For an implied class arc, the arc path of the node it is implied
    /// from.
    origin: Option<Rc<[ArcStep]>>,
    /// `true` when the arc, and every arc beneath it, skips a node that
    /// duplicates a site of the graph (see [`NodeArc::skips_duplicates`]).
    skips_duplicates: bool,
}

impl<'a> ArcParent<'a> {
    /// An arc authored at a site the arcs `steps` reach.
    fn nested(steps: &'a [ArcStep]) -> Self {
        Self {
            steps,
            implied: false,
            origin: None,
            skips_duplicates: false,
        }
    }

    /// The reference or payload `arc`, authored at the site the arcs
    /// `steps` reach, with its layer offset composed beneath the offset that
    /// site's layers are read with: the offsets and scales of every arc
    /// above it apply to its opinions, each once. `arc` already carries the
    /// offset of the layer that authors it within that site's layer stack
    /// (see [`ArcAuthoring::authored_reference`]).
    ///
    /// Spec: AOUSD Core §12.3.2.1 (layer offsets compose across arcs);
    /// OpenUSD composes them through the nodes' map expressions
    /// (`PcpNodeRef::GetMapToRoot`, `PcpMapExpression::Compose`).
    fn within_offset(&self, arc: Reference) -> Reference {
        let outer = self
            .steps
            .last()
            .map_or(LayerOffset::IDENTITY, |step| step.layer_offset);
        Reference {
            layer_offset: outer.compose(arc.layer_offset),
            ..arc
        }
    }

    /// The layers whose offsets the site the arcs `steps` reach is read
    /// with (see [`ArcStep::offset_layers`]).
    fn offset_layers(&self) -> Rc<[LayerId]> {
        self.steps
            .last()
            .map_or_else(|| Rc::from([]), |step| step.offset_layers.clone())
    }

    /// [`Self::offset_layers`], then `authored_in`, the layer that authors
    /// a reference or payload at that site: the layers whose offsets the
    /// arc's target is read with.
    fn offset_layers_within(&self, authored_in: Option<LayerId>) -> Rc<[LayerId]> {
        let outer = self.offset_layers();
        match authored_in {
            Some(layer) => outer.iter().copied().chain([layer]).collect(),
            None => outer,
        }
    }

    /// The same arc, skipping the nodes that duplicate a site of the graph.
    fn skipping_duplicates(self) -> Self {
        Self {
            skips_duplicates: true,
            ..self
        }
    }

    /// A class arc implied beneath the node `steps` reach from the class
    /// node at the arc path `origin` (see [`implied_classes`]).
    fn implied_from(steps: &'a [ArcStep], origin: Rc<[ArcStep]>) -> Self {
        Self {
            steps,
            implied: true,
            origin: Some(origin),
            skips_duplicates: false,
        }
    }
}

/// A class arc implied into a stronger layer stack: the arc path of the
/// node it is implied beneath, and the implied arc.
struct ImpliedClass {
    parent: Vec<ArcStep>,
    step: ArcStep,
    /// The arcs whose namespace mappings carry the class path into the
    /// implied arc's layer stack, innermost first.
    transfers: Vec<Transfer>,
}

/// One arc a class path is mapped across (see [`map_across`]).
#[derive(Clone, Copy)]
struct Transfer {
    /// The namespace mapping of the site that authors the arc (see
    /// [`outer_namespace`]).
    outer: Option<(PathId, PathId)>,
    arc_dest: PathId,
    arc_target: PathId,
}

/// Where the class arc `class` (an inherits or specializes arc), authored at
/// the site the arcs `steps` reach, is implied: beneath the nearest nodes
/// above it whose layer stack or namespace differs, with its class path
/// mapped there; empty when no such node exists.
///
/// A reference or payload maps its target to the site that authors it, and
/// maps every path outside the target to itself, so a root class maps to
/// the same path in the stronger layer stack. A class nested in another
/// class's namespace maps across that class arc into the inheriting prim's
/// namespace. Either way, the class hierarchy beneath a class arc is
/// implied with it: a class authored at a class arc's site is also implied
/// beneath each node that class arc is implied as. Variant branches keep
/// the layer stack and namespace; OpenUSD adds an implied class beneath them
/// that contributes no opinions, so this looks past them. Each implied arc
/// is implied further by its own expansion, so the class reaches every
/// stronger layer stack on the way to the root.
///
/// A specializes node propagated to the root (see [`nest_step`]) stands
/// in for its placeholder: the classes beneath it are implied from where the
/// placeholder sits, and a class implied beneath a specializes placeholder
/// goes beneath that placeholder's propagated node.
///
/// Spec: AOUSD Core §10.4.1, §10.4.2.4 (implied class arcs). OpenUSD:
/// `_EvalImpliedClasses`, `_EvalImpliedClassTree`,
/// `_FindStartingNodeForImpliedClasses` and `_DetermineInheritPath` in
/// `pxr/usd/pcp/primIndex.cpp`.
fn implied_classes(
    store: &mut dyn LayerStore,
    stage_layer_stack: LayerId,
    steps: &[ArcStep],
    class: &ArcStep,
) -> Vec<ImpliedClass> {
    let StepTarget::Namespace {
        dest_root,
        target_root: path,
    } = class.target
    else {
        return Vec::new();
    };
    // The arc that brings the class's site into the prim, past variant
    // branches.
    let Some(len) = steps
        .iter()
        .rposition(|step| matches!(step.target, StepTarget::Namespace { .. }))
        .map(|at| at + 1)
    else {
        return Vec::new();
    };
    let step = &steps[len - 1];
    if let Some(placeholder) = propagated_from(step) {
        let mut steps_at_placeholder = placeholder.to_vec();
        steps_at_placeholder.extend_from_slice(&steps[len..]);
        return implied_classes(store, stage_layer_stack, &steps_at_placeholder, class);
    }
    let StepTarget::Namespace {
        dest_root: arc_dest,
        target_root: arc_target,
    } = step.target
    else {
        unreachable!("found a namespace step");
    };
    let parent = &steps[..len - 1];
    let class_based = matches!(step.arc_kind, ArcKind::Inherits | ArcKind::Specializes);
    let mut implied = Vec::new();
    if !class_based && !matches!(step.arc_kind, ArcKind::References | ArcKind::Payloads) {
        return implied;
    }
    // A class authored on the class prim itself belongs to the hierarchy
    // that class arc starts, and so does a class outside the inherited
    // class's namespace; OpenUSD implies them only beneath that class arc's
    // implied nodes (`_FindStartingNodeForImpliedClasses`).
    let in_hierarchy = class_based
        && (step.namespace_depth == class.namespace_depth || {
            let paths = store.paths();
            paths
                .resolve(path)
                .strip_prefix(paths.resolve(arc_target))
                .is_none()
        });
    if !in_hierarchy {
        let outer = outer_namespace(parent);
        let mapped = map_across(store, outer, arc_dest, arc_target, path);
        let level = parent
            .iter()
            .rev()
            .find(|step| matches!(step.target, StepTarget::Namespace { .. }));
        let layer_stack = level.map_or(stage_layer_stack, |level| level.layer_stack);
        if mapped == path && layer_stack == step.layer_stack {
            // The same site: OpenUSD adds a node that contributes no
            // opinions and only carries the class further up.
            implied = implied_classes(store, stage_layer_stack, parent, class);
        } else {
            let host = ArcStep {
                layer_stack,
                layer_offset: level.map_or(LayerOffset::IDENTITY, |level| level.layer_offset),
                offset_layers: level
                    .map_or_else(|| Rc::from([]), |level| level.offset_layers.clone()),
                ..step.clone()
            };
            implied.push(ImpliedClass {
                parent: parent.to_vec(),
                step: implied_step(class, &host, dest_root, mapped),
                transfers: alloc::vec![Transfer {
                    outer,
                    arc_dest,
                    arc_target,
                }],
            });
        }
    }
    if class_based {
        for hierarchy in implied_classes(store, stage_layer_stack, parent, step) {
            let mapped = hierarchy.transfers.iter().fold(path, |path, transfer| {
                map_across(
                    store,
                    transfer.outer,
                    transfer.arc_dest,
                    transfer.arc_target,
                    path,
                )
            });
            let ImpliedClass {
                parent,
                step: host,
                transfers,
            } = hierarchy;
            let step = implied_step(class, &host, dest_root, mapped);
            implied.push(ImpliedClass {
                parent: nest_step(parent, host),
                step,
                transfers,
            });
        }
    }
    implied
}

/// The arc path of the placeholder `step` was propagated from, for a
/// specializes node propagated to the root (see [`nest_step`]).
fn propagated_from(step: &ArcStep) -> Option<&[ArcStep]> {
    match &step.origin {
        Some(placeholder) if step.arc_kind == ArcKind::Specializes && !step.implied => {
            Some(placeholder)
        }
        _ => None,
    }
}

/// The arc path of the node `step`, authored at the site the arcs `parent`
/// reach, adds: `parent` then `step`, except that a specializes arc
/// authored beneath the root leaves an inert placeholder there and its node
/// is propagated to the root, with that placeholder as its origin.
///
/// Arcs authored inside the specialized prim nest under the propagated
/// node, so they rank after every other arc of the prim (AOUSD Core
/// §10.4.1; `_EvalImpliedSpecializes` and `_PropagateNodeToRoot` in
/// `pxr/usd/pcp/primIndex.cpp`).
fn nest_step(mut parent: Vec<ArcStep>, step: ArcStep) -> Vec<ArcStep> {
    if step.arc_kind != ArcKind::Specializes || parent.is_empty() {
        parent.push(step);
        return parent;
    }
    parent.push(step.clone());
    alloc::vec![ArcStep {
        implied: false,
        origin: Some(parent.into()),
        ..step
    }]
}

/// The implied copy of the class arc `class`, targeting `target_root` in the
/// layer stack of `host`.
fn implied_step(
    class: &ArcStep,
    host: &ArcStep,
    dest_root: PathId,
    target_root: PathId,
) -> ArcStep {
    ArcStep {
        arc_kind: class.arc_kind,
        layer_stack: host.layer_stack,
        target: StepTarget::Namespace {
            dest_root,
            target_root,
        },
        namespace_depth: class.namespace_depth,
        sibling_index: class.sibling_index,
        implied: true,
        origin: None,
        layer_offset: host.layer_offset,
        offset_layers: host.offset_layers.clone(),
        skips_duplicates: false,
    }
}

/// The namespace mapping of the last namespace step of `parent`: from the
/// composed prim's namespace into the namespace of the site the arcs
/// `parent` reach; `None` for the composed prim's own site.
fn outer_namespace(parent: &[ArcStep]) -> Option<(PathId, PathId)> {
    parent.iter().rev().find_map(|step| match step.target {
        StepTarget::Namespace {
            dest_root,
            target_root,
        } => Some((dest_root, target_root)),
        _ => None,
    })
}

/// Maps `path` across the arc from `arc_dest` to `arc_target` authored at
/// the site whose namespace mapping is `outer` (see [`outer_namespace`]),
/// into that site's namespace; a path outside `arc_target` maps to itself.
///
/// OpenUSD: the arc's `PcpNodeRef::GetMapToParent` with
/// `PcpMapExpression::AddRootIdentity`, as `_EvalImpliedClasses` applies it.
fn map_across(
    store: &mut dyn LayerStore,
    outer: Option<(PathId, PathId)>,
    arc_dest: PathId,
    arc_target: PathId,
    path: PathId,
) -> PathId {
    let paths = store.paths();
    let Some(rel) = paths
        .resolve(path)
        .strip_prefix(paths.resolve(arc_target))
        .map(<[_]>::to_vec)
    else {
        return path;
    };
    let joined = paths.resolve(arc_dest).join(&rel);
    let stage_path = store.paths_mut().intern(joined);
    // Arc steps map from the composed prim's namespace; the outer mapping
    // maps into the namespace of the authoring site.
    match outer {
        Some((dest_root, target_root)) => map_namespace(store, stage_path, dest_root, target_root),
        None => stage_path,
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
    /// The nodes of the arc `step` authored at a site `parent` reaches (see
    /// [`nest_step`]).
    fn new(parent: ArcParent<'_>, step: ArcStep) -> Self {
        let skips_duplicates = parent.skips_duplicates
            || parent
                .steps
                .last()
                .is_some_and(|step| step.skips_duplicates);
        let authored = ArcStep {
            implied: parent.implied,
            origin: parent.origin,
            skips_duplicates,
            ..step
        };
        Self {
            path: nest_step(parent.steps.to_vec(), authored),
            nodes: HashMap::new(),
        }
    }

    /// The arc path to this arc's selected branches `sites`, for arcs
    /// authored inside them (see [`Self::variant_node`]); to this arc for
    /// arcs authored outside every branch.
    fn branch_path(&self, sites: &[VariantSelectionSite]) -> Cow<'_, [ArcStep]> {
        if sites.is_empty() {
            return Cow::Borrowed(&self.path);
        }
        let mut path = self.path.clone();
        path.extend(self.variant_steps(sites));
        Cow::Owned(path)
    }

    fn step(&self) -> &ArcStep {
        self.path.last().expect("an arc path ends with its arc")
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
    /// target.
    fn variant_steps(&self, sites: &[VariantSelectionSite]) -> impl Iterator<Item = ArcStep> {
        let step = self.step();
        let (layer_stack, namespace_depth, layer_offset, skips_duplicates) = (
            step.layer_stack,
            step.namespace_depth,
            step.layer_offset,
            step.skips_duplicates,
        );
        let offset_layers = step.offset_layers.clone();
        sites.iter().map(move |site| ArcStep {
            arc_kind: ArcKind::Variants,
            layer_stack,
            target: StepTarget::Variant(*site),
            namespace_depth,
            sibling_index: 0,
            implied: false,
            origin: None,
            layer_offset,
            offset_layers: offset_layers.clone(),
            skips_duplicates,
        })
    }

    /// The node of the selected variant branches `sites` of the arc's
    /// target, outermost first, in the graph of the composed prim `dest`.
    /// Each branch is a node beneath the node of the branch enclosing it, or
    /// the arc's node.
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
    ) -> NodeId {
        let mut cursor = self.cursor(store, out, dest);
        let steps: Vec<ArcStep> = self.variant_steps(sites).collect();
        intern_steps(store, out, dest, &steps, &mut cursor);
        cursor.node
    }

    /// The node of a spec of the arc's target authored inside the branches
    /// `sites` (its [`crate::doc::PrimSpec::outer_variant_sites`]), in the graph of the
    /// composed prim `dest`: the arc's node for a spec outside every branch.
    fn spec_node(
        &mut self,
        store: &mut dyn LayerStore,
        out: &mut HashMap<PathId, PrimIndex>,
        dest: PathId,
        sites: &[VariantSelectionSite],
    ) -> NodeId {
        self.variant_node(store, out, dest, sites)
    }
}

/// The inherits arcs already expanded: `(destination, first layer read,
/// class path, implied)`.
///
/// An implied class and an authored one at the same site are expanded
/// separately, whichever comes first; [`retain_new_class_sites`] keeps the
/// stronger registration of each site.
type VisitedClasses = HashSet<(PathId, LayerId, PathId, bool)>;

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

/// The arcs authored on the namespace ancestors of an arc's subroot target,
/// expanded beneath the arc's node.
///
/// A subroot target's prim index starts from its parent's: the arcs and
/// variant selections authored on the target's ancestors in the target
/// layer stack reach the target, with the target's name appended to each
/// of their sites (AOUSD Core §10.2, §10.4; OpenUSD `_AddArc` builds the
/// target site's index with `includeAncestralOpinions`, starting from
/// `_BuildInitialPrimIndexFromAncestor` in `pxr/usd/pcp/primIndex.cpp`).
/// An arc from the ancestor `/T` to `/C` reaches the target `/T/B` as an arc
/// to `/C/B`, whose own expansion follows the ancestors of `/C/B` in turn,
/// so the target's ancestral graph is built from the target layer stack
/// alone, whatever the stage has composed so far.
///
/// An ancestor's variant selections come from the strongest site it maps
/// to (see [`Self::host_selections`]): a reference or payload maps no path
/// above its target, so the target layer stack selects them, while a class
/// arc maps the ancestor to itself.
///
/// Each ancestral arc ranks after the arcs of its kind authored deeper in
/// namespace, as OpenUSD compares sibling nodes by namespace depth
/// (`PcpCompareSiblingNodeStrength` in `pxr/usd/pcp/strengthOrdering.cpp`).
struct AncestralArcs<'a> {
    /// The layers of the target layer stack, which author the ancestors'
    /// arcs.
    data_stack: &'a LayerStack,
    /// The layers the arcs nested in the arc read variant selections from.
    selection_stack: &'a LayerStack,
    /// The layers that select the variants of the target's ancestors: the
    /// target layer stack, or for a class arc, which maps every path outside
    /// the class to itself, the stronger layer stacks as well (see
    /// [`enclosing_variant_selections`]).
    ancestor_stack: &'a LayerStack,
    /// Root layer of the target layer stack; internal arcs target it.
    arc_stack: LayerId,
    dest_root: PathId,
    target: PathId,
    /// The offset of the arc, applied to class arcs nested in it.
    layer_offset: LayerOffset,
    /// The namespace the arc maps, for class arcs nested in it (see
    /// `add_inherit_edge_opinions`).
    ref_remap: Option<(&'a crate::path::Path, &'a crate::path::Path)>,
    /// `true` for a class arc, whose ancestral arcs add no node for a site
    /// the prim index uses already (see [`Self::used_sites`]).
    class_arc: bool,
}

impl AncestralArcs<'_> {
    /// The sites of `nodes`' destination graph an ancestral arc of a class
    /// arc does not add again: every non-variant node's site, and for an
    /// implied or propagated class, the sites on the arc paths of the nodes
    /// it comes from, whose ancestral arcs lead back to them.
    ///
    /// OpenUSD adds class arcs with `skipDuplicateNodes`, which holds in the
    /// recursive index of the target's ancestors as well (`_AddArc` and
    /// `_AddClassBasedArc` in `pxr/usd/pcp/primIndex.cpp`).
    fn used_sites(
        &self,
        store: &mut dyn LayerStore,
        nodes: &ArcNodes,
        out: &HashMap<PathId, PrimIndex>,
    ) -> HashSet<(LayerId, PathId)> {
        let mut used: HashSet<(LayerId, PathId)> = out[&self.dest_root]
            .graph
            .nodes()
            .filter(|(_, node)| node.arc_kind() != ArcKind::Variants)
            .map(|(_, node)| (node.layer_stack(), node.site().prim_path()))
            .collect();
        let mut pending: Vec<&[ArcStep]> = nodes
            .path
            .iter()
            .filter_map(|step| step.origin.as_deref())
            .collect();
        while let Some(path) = pending.pop() {
            for step in path {
                if let StepTarget::Namespace {
                    dest_root,
                    target_root,
                } = step.target
                {
                    let site = map_namespace(store, self.dest_root, dest_root, target_root);
                    used.insert((step.layer_stack, site));
                }
                pending.extend(step.origin.as_deref());
            }
        }
        used
    }

    /// The parent of an ancestral arc authored at the site the arcs
    /// `branch` reach: beneath a class arc, it skips the nodes that duplicate
    /// a site of the graph, as every arc of the recursive index OpenUSD
    /// builds for a class's ancestors does (`_AddArc` inherits
    /// `skipDuplicateNodes` from `previousFrame`).
    fn parent<'b>(&self, branch: &'b [ArcStep]) -> ArcParent<'b> {
        if self.class_arc {
            ArcParent::nested(branch).skipping_duplicates()
        } else {
            ArcParent::nested(branch)
        }
    }

    /// The arcs authored for `ancestor` in the target layer stack, admitted
    /// for the selections of the variant hosts enclosing it (see
    /// [`Self::host_selections`]).
    fn arcs_of(
        &self,
        store: &dyn LayerStore,
        nodes: &ArcNodes,
        out: &HashMap<PathId, PrimIndex>,
        ancestor: PathId,
    ) -> AdmittedArcs {
        let mut enclosing = HashMap::new();
        let mut host = Some(ancestor);
        while let Some(path) = host {
            enclosing.insert(path, self.host_selections(store, nodes, out, path));
            host = store
                .paths()
                .resolve(path)
                .parent()
                .filter(|parent| parent.depth() > 0)
                .and_then(|parent| store.paths().lookup(&parent));
        }
        arcs_admitted_by(store, self.data_stack, ancestor, &enclosing, self.arc_stack)
    }

    /// The variant selections for `host`, an ancestor of the target in the
    /// target layer stack.
    ///
    /// OpenUSD composes the selection from the node of the strongest site
    /// `host` maps to on the way to the root (`_ComposeVariantSelection`
    /// and `Pcp_TranslatePathFromNodeToRootOrClosestNode`): a class arc maps
    /// every path outside the class to itself, as an internal reference
    /// does, while another arc maps only its target. A host that reaches
    /// the stage takes the selections of the stage prim it maps to, as
    /// composed so far, strongest first; `ancestor_stack` supplies the rest.
    fn host_selections(
        &self,
        store: &dyn LayerStore,
        nodes: &ArcNodes,
        out: &HashMap<PathId, PrimIndex>,
        host: PathId,
    ) -> HashMap<TokenId, TokenId> {
        let mut selections = self
            .stage_host(store, nodes, out, host)
            .and_then(|stage| out.get(&stage))
            .map(|index| {
                let mut sources = index.sources.clone();
                index.graph.sort_keys(&mut sources);
                let so_far = PrimIndex {
                    sources,
                    ..PrimIndex::default()
                };
                strength_ordered_variant_selections(store, &so_far)
            })
            .unwrap_or_default();
        for (set, variant) in resolve_full_variant_selections(store, self.ancestor_stack, host) {
            selections.entry(set).or_insert(variant);
        }
        selections
    }

    /// The stage prim `host` maps to through the arcs `nodes` follow from
    /// the composed prim, innermost first; `None` when an arc that maps only
    /// its target stands in the way.
    fn stage_host(
        &self,
        store: &dyn LayerStore,
        nodes: &ArcNodes,
        out: &HashMap<PathId, PrimIndex>,
        host: PathId,
    ) -> Option<PathId> {
        let paths = store.paths();
        let host_path = paths.resolve(host);
        let root_stack = root_layer_stack(out, self.dest_root);
        for (index, step) in nodes.path.iter().enumerate().rev() {
            let StepTarget::Namespace {
                dest_root,
                target_root,
            } = step.target
            else {
                continue;
            };
            if let Some(rel) = host_path.strip_prefix(paths.resolve(target_root)) {
                return paths.lookup(&paths.resolve(dest_root).join(rel));
            }
            let authoring_stack = nodes.path[..index]
                .iter()
                .rev()
                .find(|step| matches!(step.target, StepTarget::Namespace { .. }))
                .map_or(root_stack, |step| step.layer_stack);
            let identity = matches!(step.arc_kind, ArcKind::Inherits | ArcKind::Specializes)
                || step.layer_stack == authoring_stack;
            if !identity {
                return None;
            }
        }
        Some(host)
    }

    /// The reference or payload `arc`, authored on an ancestor of the target,
    /// retargeted to the target's path beneath its target: `rel` appended;
    /// `None` when it does not resolve, or when a class arc's ancestral
    /// arc reaches a site of `used`.
    ///
    /// The authored target is resolved as a direct arc's is, so an omitted
    /// target records its dependency on the target layer's `defaultPrim`
    /// and an unresolved asset or `defaultPrim` is reported for the
    /// destination (see [`resolve_arc_target`]).
    fn retarget(
        &self,
        store: &mut dyn LayerStore,
        arc: AuthoredReference,
        rel: &[TokenId],
        used: &HashSet<(LayerId, PathId)>,
        kind: ArcKind,
        cycles: &mut CycleDetector,
        deps: Option<&mut DependencyBuilder>,
    ) -> Option<AuthoredReference> {
        let reference = &arc.reference;
        let path = resolve_arc_target(store, reference, self.dest_root, kind, cycles, deps)?;
        let joined = store.paths().resolve(path).join(rel);
        let path = store.paths_mut().intern(joined);
        if used.contains(&(reference.layer, path)) {
            return None;
        }
        Some(AuthoredReference {
            reference: Reference {
                target: ReferenceTarget::Prim(path),
                ..arc.reference
            },
            ..arc
        })
    }

    /// Expands the arcs of every ancestor of the target beneath `nodes`,
    /// the arc's nodes.
    fn expand(
        &self,
        store: &mut dyn LayerStore,
        nodes: &ArcNodes,
        out: &mut HashMap<PathId, PrimIndex>,
        visited_refs: &mut HashSet<(PathId, LayerId, PathId)>,
        visited_inherits: &mut VisitedClasses,
        visited_specializes: &mut VisitedClasses,
        prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        cycles: &mut CycleDetector,
        mut deps: Option<&mut DependencyBuilder>,
    ) {
        let used = if self.class_arc {
            self.used_sites(store, nodes, out)
        } else {
            HashSet::new()
        };
        let target_path = store.paths().resolve(self.target).clone();
        let dest_depth = store.paths().resolve(self.dest_root).depth();
        let mut ancestors = Vec::new();
        let mut cursor = target_path.parent();
        while let Some(path) = cursor {
            if path.depth() == 0 {
                break;
            }
            cursor = path.parent();
            ancestors.push(path);
        }
        for ancestor_path in ancestors {
            let Some(ancestor) = store.paths().lookup(&ancestor_path) else {
                continue;
            };
            let rel = target_path
                .strip_prefix(&ancestor_path)
                .expect("an ancestor prefixes its descendant")
                .to_vec();
            // The ancestor's depth, measured in the destination's namespace
            // as the depth of the arcs authored at the target is: the arcs
            // of ancestors further above than the destination is deep
            // share depth 0.
            let namespace_depth = u16::try_from(
                (dest_depth + ancestor_path.depth()).saturating_sub(target_path.depth()),
            )
            .unwrap_or(u16::MAX);
            let arcs = self.arcs_of(store, nodes, out, ancestor);
            let mapped = |store: &mut dyn LayerStore, path: PathId| {
                let joined = store.paths().resolve(path).join(&rel);
                store.paths_mut().intern(joined)
            };
            for (index, (reference, sites)) in arcs.references.into_iter().enumerate() {
                let Some(reference) = self.retarget(
                    store,
                    reference,
                    &rel,
                    &used,
                    ArcKind::References,
                    cycles,
                    deps.as_deref_mut(),
                ) else {
                    continue;
                };
                let branch = nodes.branch_path(&sites);
                add_reference_edge_opinions(
                    store,
                    self.selection_stack,
                    self.dest_root,
                    reference,
                    namespace_depth,
                    u16::try_from(index).unwrap_or(u16::MAX),
                    self.parent(&branch),
                    out,
                    visited_refs,
                    visited_inherits,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    cycles,
                    deps.as_deref_mut(),
                );
            }
            for (index, (payload, sites)) in arcs.payloads.into_iter().enumerate() {
                let Some(payload) = self.retarget(
                    store,
                    payload,
                    &rel,
                    &used,
                    ArcKind::Payloads,
                    cycles,
                    deps.as_deref_mut(),
                ) else {
                    continue;
                };
                let branch = nodes.branch_path(&sites);
                add_payload_edge_opinions(
                    store,
                    self.selection_stack,
                    self.dest_root,
                    payload,
                    namespace_depth,
                    u16::try_from(index).unwrap_or(u16::MAX),
                    self.parent(&branch),
                    out,
                    visited_refs,
                    visited_inherits,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    cycles,
                    deps.as_deref_mut(),
                );
            }
            for (index, (class, sites)) in arcs.inherits.into_iter().enumerate() {
                let class = mapped(store, class);
                if used.contains(&(self.arc_stack, class)) {
                    continue;
                }
                let branch = nodes.branch_path(&sites);
                add_inherit_edge_opinions(
                    store,
                    self.data_stack,
                    self.selection_stack,
                    self.dest_root,
                    class,
                    self.arc_stack,
                    namespace_depth,
                    u16::try_from(index).unwrap_or(u16::MAX),
                    self.parent(&branch),
                    out,
                    visited_inherits,
                    visited_specializes,
                    visited_refs,
                    prim_order_out,
                    authored_children_out,
                    self.ref_remap,
                    None,
                    self.layer_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }
            for (index, (specialized, sites)) in arcs.specializes.into_iter().enumerate() {
                let specialized = mapped(store, specialized);
                if used.contains(&(self.arc_stack, specialized)) {
                    continue;
                }
                let branch = nodes.branch_path(&sites);
                let index = u16::try_from(index).unwrap_or(u16::MAX);
                add_specializes_edge_opinions(
                    store,
                    self.selection_stack,
                    self.dest_root,
                    self.dest_root,
                    specialized,
                    self.arc_stack,
                    namespace_depth,
                    index,
                    self.parent(&branch),
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    self.layer_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }
        }
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
    // Rank only the graphs that register one of the pending sites already.
    for (dest, key) in pending.iter() {
        let index = out.get_mut(dest).expect("path exists");
        if index
            .sources
            .iter()
            .any(|known| known.layer_id == key.layer_id && known.spec_path == key.spec_path)
        {
            index.graph.rank();
        }
    }
    pending.retain(|(dest, key)| {
        let index = &out[dest];
        let graph = &index.graph;
        let mut registered = false;
        for known in &index.sources {
            if known.layer_id != key.layer_id || known.spec_path != key.spec_path {
                continue;
            }
            // Registrations of nodes skipping duplicates give way (see
            // `drop_skipped_duplicates`).
            if graph
                .node(known.node)
                .is_some_and(|node| node.arc.skips_duplicates)
            {
                continue;
            }
            let class_based = graph.node(known.node).is_some_and(|node| {
                matches!(node.arc_kind(), ArcKind::Inherits | ArcKind::Specializes)
            });
            let stronger = graph.cmp_nodes(key.node, known.node).is_lt();
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
    // The layer stack of the class: the layers the arc reads.
    local_stack: &LayerStack,
    // The layers whose variant selections apply to the destination, strongest
    // first.
    selection_stack: &LayerStack,
    dest_root: PathId,
    inherited_root: PathId,
    // Root layer of the layer stack the inherit is authored in.
    arc_stack: LayerId,
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut VisitedClasses,
    visited_specializes: &mut VisitedClasses,
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
    // One expansion per class site and layer stack, authored or implied.
    let layers_read = local_stack.layers.first().copied().unwrap_or(arc_stack);
    if !visited.insert((dest_root, layers_read, inherited_root, parent.implied)) {
        return;
    }
    let step = ArcStep {
        arc_kind: ArcKind::Inherits,
        layer_stack: arc_stack,
        target: StepTarget::Namespace {
            dest_root,
            target_root: inherited_root,
        },
        namespace_depth,
        sibling_index: arc_list_index,
        implied: false,
        origin: None,
        layer_offset: base_offset,
        offset_layers: parent.offset_layers(),
        skips_duplicates: false,
    };
    let implied = implied_classes(store, cycles.stage_layer_stack(), parent.steps, &step);
    // The class implied into the next stronger layer stacks or namespaces,
    // with this arc's node as its origin; its own expansion implies it
    // further (AOUSD Core §10.4.2.4; `_EvalImpliedClasses`).
    let origin: Rc<[ArcStep]> = {
        let mut path = parent.steps.to_vec();
        path.push(ArcStep {
            implied: parent.implied,
            origin: parent.origin.clone(),
            ..step.clone()
        });
        path.into()
    };
    for implied in implied {
        let StepTarget::Namespace {
            target_root: implied_root,
            ..
        } = implied.step.target
        else {
            unreachable!("an implied class maps a namespace");
        };
        let implied_stack = cycles.gather_layer_stack(store, implied.step.layer_stack);
        add_inherit_edge_opinions(
            store,
            &implied_stack,
            selection_stack,
            dest_root,
            implied_root,
            implied.step.layer_stack,
            namespace_depth,
            arc_list_index,
            ArcParent::implied_from(&implied.parent, origin.clone()),
            out,
            visited,
            visited_specializes,
            visited_refs,
            prim_order_out,
            authored_children_out,
            None,
            None,
            implied.step.layer_offset,
            cycles,
            deps.as_deref_mut(),
        );
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
            selection_stack,
            local_stack,
            selection_stack,
            inherited_root,
            &pairs,
            &mut host_selection_cache,
        )
    };
    mapping.retain(|(remote, _)| !is_at_or_under(store, *remote, &unselected));

    let mut nodes = ArcNodes::new(parent, step);
    record_offset_layers(deps.as_deref_mut(), &nodes.step().offset_layers, &mapping);

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
                        selection_stack,
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
                            let variant_node =
                                nodes.variant_node(store, out, *dest_path_id, &branch_selections);
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
                                    variant_node,
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
            selection_stack,
            local_stack,
            selection_stack,
            inherited_root,
            remote_path_id,
            dest_path_id,
            &mut host_selection_cache,
            arc_stack,
        );
        for (nested_index, (nested, sites)) in nested_inherits.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // A class nested in the inherited class's namespace is implied
            // into the destination's namespace by its own expansion (see
            // `implied_classes`).
            add_inherit_edge_opinions(
                store,
                local_stack,
                selection_stack,
                dest_path_id,
                nested,
                arc_stack,
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

        // Specializes authored in the inherited class: each leaves a
        // placeholder beneath this class's node and is propagated to the
        // root, and is implied like this class (see `implied_classes`).
        //
        // Spec: AOUSD Core §10.4.1 (the specializes node ranks after every
        // other opinion of the prim), §10.4.2.4.
        for (spec_index, (specialized, sites)) in nested_specializes.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites);
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_specializes_edge_opinions(
                store,
                selection_stack,
                dest_path_id,
                dest_path_id,
                specialized,
                arc_stack,
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
            let branch = nodes.branch_path(&sites);
            let ref_index = u16::try_from(ref_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_reference_edge_opinions(
                store,
                selection_stack,
                dest_path_id,
                nested_ref,
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
            let branch = nodes.branch_path(&sites);
            let payload_index = u16::try_from(payload_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                selection_stack,
                dest_path_id,
                nested_payload,
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

    // The arcs the class's ancestors author (see `AncestralArcs`).
    AncestralArcs {
        data_stack: local_stack,
        selection_stack,
        ancestor_stack: selection_stack,
        arc_stack,
        dest_root,
        target: inherited_root,
        layer_offset: base_offset,
        ref_remap,
        class_arc: true,
    }
    .expand(
        store,
        &nodes,
        out,
        visited_refs,
        visited,
        visited_specializes,
        prim_order_out,
        authored_children_out,
        cycles,
        deps,
    );

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

/// Records that each destination prim of `mapping`, which an arc's node
/// composes, depends on `layers`, the layers that author the references
/// and payloads on the way to that node ([`ArcStep::offset_layers`]).
///
/// A layer's offset in its layer stack retimes the opinions of every arc
/// it authors, and of every arc nested beneath one: every prim that such
/// an arc's subtree reaches depends on it, not only the one whose spec
/// authors it, including prims only a nested arc, an ancestral arc or a
/// class arc beneath it supplies.
///
/// Spec: AOUSD Core §12.3.2.1. OpenUSD folds the layer's offset into the
/// map functions of the arcs it authors, and resyncs every prim index that
/// depends on a layer whose sublayer offsets change (`PcpChanges::DidChange`
/// in `pxr/usd/pcp/changes.cpp`).
fn record_offset_layers(
    deps: Option<&mut DependencyBuilder>,
    layers: &[LayerId],
    mapping: &[(PathId, PathId)],
) {
    let Some(deps) = deps else {
        return;
    };
    for &layer in layers {
        for &(_, dest) in mapping {
            deps.add_layer_opinion(layer, dest);
        }
    }
}

fn add_reference_edge_opinions(
    store: &mut dyn LayerStore,
    stage_stack: &LayerStack,
    dest_root: PathId,
    arc: AuthoredReference,
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut VisitedClasses,
    visited_specializes: &mut VisitedClasses,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    provenance_remap: Option<(PathId, PathId)>,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // The reference's node sits beneath the node whose site authors it, so its
    // target, with the arcs authored there, ranks beneath that site and
    // before that site's weaker arcs.
    //
    // Spec: AOUSD Core §10.4 (LIVERPS strength ordering is applied recursively
    // within each arc's target prim index); OpenUSD ranks a node above all of
    // its descendants and compares siblings below their common ancestor
    // (`PcpCompareNodeStrength` in `pxr/usd/pcp/strengthOrdering.cpp`).
    if !out.contains_key(&dest_root) {
        return;
    }
    let reference = parent.within_offset(arc.reference);
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
    let offset_layers = parent.offset_layers_within(arc.layer);
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
            origin: None,
            layer_offset: reference.layer_offset,
            offset_layers,
            skips_duplicates: false,
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

    // The arc maps the target and its namespace descendants; the arcs of
    // the target's ancestors follow (see `AncestralArcs`).
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
    record_offset_layers(deps.as_deref_mut(), &nodes.step().offset_layers, &mapping);

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
                        let variant_node =
                            nodes.variant_node(store, out, *dest_path_id, &branch_selections);
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
            &remote_stack,
            reference_path,
            remote_path_id,
            dest_path_id,
            &mut host_selection_cache,
            reference.layer,
        );
        let inherits = arcs.inherits;
        for (inherit_index, (inherited_root, sites)) in inherits.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites);
            let inherit_index = u16::try_from(inherit_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // The class is implied into each stronger layer stack by its own
            // expansion (see `implied_classes`).
            let ref_remap = Some((&dest_root_path, &target_root));
            add_inherit_edge_opinions(
                store,
                &remote_stack,
                &combined_stack,
                dest_path_id,
                inherited_root,
                reference.layer,
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
            let branch = nodes.branch_path(&sites);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_reference_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_ref,
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
            let branch = nodes.branch_path(&sites);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_payload,
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

        // Specializes authored in the referenced content. Their opinions
        // are weaker than every other opinion of the prim, not only than
        // this reference's: each leaves a placeholder beneath this
        // reference's node and is propagated to the root, and is implied into
        // each stronger layer stack (see `nest_step` and `implied_classes`).
        //
        // Spec: AOUSD Core §10.4.1, §10.4.2.4; OpenUSD
        // `_EvalImpliedSpecializes` in `pxr/usd/pcp/primIndex.cpp`.
        for (spec_index, (specialized_root, sites)) in arcs.specializes.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites);
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_specializes_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                remote_path_id,
                specialized_root,
                reference.layer,
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

    // The arcs the target's ancestors author (see `AncestralArcs`).
    AncestralArcs {
        data_stack: &remote_stack,
        selection_stack: &combined_stack,
        ancestor_stack: &remote_stack,
        arc_stack: reference.layer,
        dest_root,
        target: reference_path,
        layer_offset: reference.layer_offset,
        ref_remap: Some((&dest_root_path, &target_root)),
        class_arc: false,
    }
    .expand(
        store,
        &nodes,
        out,
        visited,
        visited_inherits,
        visited_specializes,
        prim_order_out,
        authored_children_out,
        cycles,
        deps,
    );

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
    let mut visited_inherits: VisitedClasses = VisitedClasses::new();
    let mut visited_specializes = VisitedClasses::new();
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
            let (payload, sites) = ArcAuthoring {
                store,
                stack: local_stack,
                prim: dest_root,
                selections: &selections,
                scope: SelectionScope::Stack,
            }
            .authored_reference(
                payload,
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
    arc: AuthoredReference,
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut VisitedClasses,
    visited_specializes: &mut VisitedClasses,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    provenance_remap: Option<(PathId, PathId)>,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // The payload's node sits beneath the node whose site authors it, so its
    // target, with the arcs authored there, ranks beneath that site and
    // before that site's weaker arcs.
    //
    // Spec: AOUSD Core §10.4 (LIVERPS strength ordering is applied recursively
    // within each arc's target prim index); OpenUSD ranks a node above all of
    // its descendants and compares siblings below their common ancestor
    // (`PcpCompareNodeStrength` in `pxr/usd/pcp/strengthOrdering.cpp`).
    // Payloads mirror reference edge opinions with ArcKind::Payloads.
    if !out.contains_key(&dest_root) {
        return;
    }
    let reference = parent.within_offset(arc.reference);
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
    let offset_layers = parent.offset_layers_within(arc.layer);
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
            origin: None,
            layer_offset: reference.layer_offset,
            offset_layers,
            skips_duplicates: false,
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

    // The arc maps the target and its namespace descendants; the arcs of
    // the target's ancestors follow (see `AncestralArcs`).
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
    record_offset_layers(deps.as_deref_mut(), &nodes.step().offset_layers, &mapping);

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
                        let variant_node =
                            nodes.variant_node(store, out, *dest_path_id, &branch_selections);
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
            &remote_stack,
            reference_path,
            remote_path_id,
            dest_path_id,
            &mut host_selection_cache,
            reference.layer,
        );
        let inherits = arcs.inherits;
        for (inherit_index, (inherited_root, sites)) in inherits.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites);
            let inherit_index = u16::try_from(inherit_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // The class is implied into each stronger layer stack by its own
            // expansion (see `implied_classes`).
            let ref_remap = Some((&dest_root_path, &target_root));
            add_inherit_edge_opinions(
                store,
                &remote_stack,
                &combined_stack,
                dest_path_id,
                inherited_root,
                reference.layer,
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
            let branch = nodes.branch_path(&sites);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_reference_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_ref,
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
            let branch = nodes.branch_path(&sites);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_payload,
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
            let branch = nodes.branch_path(&sites);
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_specializes_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                remote_path_id,
                specialized_root,
                reference.layer,
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
    // The arcs the target's ancestors author (see `AncestralArcs`).
    AncestralArcs {
        data_stack: &remote_stack,
        selection_stack: &combined_stack,
        ancestor_stack: &remote_stack,
        arc_stack: reference.layer,
        dest_root,
        target: reference_path,
        layer_offset: reference.layer_offset,
        ref_remap: Some((&dest_root_path, &target_root)),
        class_arc: false,
    }
    .expand(
        store,
        &nodes,
        out,
        visited,
        visited_inherits,
        visited_specializes,
        prim_order_out,
        authored_children_out,
        cycles,
        deps,
    );

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
    let mut visited = VisitedClasses::new();
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
/// The arc reads the specialized prim in the layer stack it is authored in.
/// Authored beneath the root, it leaves an inert placeholder there, and its
/// node is propagated to the root with the arcs authored inside the
/// specialized prim beneath it, so every opinion it introduces ranks after
/// every other opinion of the prim (see [`nest_step`]). It is implied into
/// each stronger layer stack like an inherit (see [`implied_classes`]).
///
/// Spec: AOUSD Core §10.4.1, §10.4.2.4. OpenUSD: `_EvalImpliedSpecializes`
/// and `_EvalImpliedClasses` in `pxr/usd/pcp/primIndex.cpp`.
fn add_specializes_edge_opinions(
    store: &mut dyn LayerStore,
    // The layers whose variant selections apply to the destination, strongest
    // first.
    selection_stack: &LayerStack,
    dest_root: PathId,
    selection_root: PathId,
    specialized_root: PathId,
    // Root layer of the layer stack the specializes arc is authored in: the
    // layers the arc reads.
    arc_stack: LayerId,
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut VisitedClasses,
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
    if !visited.insert((dest_root, arc_stack, specialized_root, parent.implied)) {
        return;
    }
    let local_stack = cycles.gather_layer_stack(store, arc_stack);
    let local_stack = &local_stack;
    let step = ArcStep {
        arc_kind: ArcKind::Specializes,
        layer_stack: arc_stack,
        target: StepTarget::Namespace {
            dest_root,
            target_root: specialized_root,
        },
        namespace_depth,
        sibling_index: arc_list_index,
        implied: false,
        origin: None,
        layer_offset: base_offset,
        offset_layers: parent.offset_layers(),
        skips_duplicates: false,
    };
    // The specializes implied into the next stronger layer stacks or
    // namespaces, with the node this arc is authored as (its placeholder,
    // beneath the root) as their origin; each is implied further by its own
    // expansion (AOUSD Core §10.4.2.4; `_EvalImpliedClasses`).
    let implied = implied_classes(store, cycles.stage_layer_stack(), parent.steps, &step);
    let origin: Rc<[ArcStep]> = {
        let mut path = parent.steps.to_vec();
        path.push(ArcStep {
            implied: parent.implied,
            origin: parent.origin.clone(),
            ..step.clone()
        });
        path.into()
    };
    for implied in implied {
        let StepTarget::Namespace {
            target_root: implied_root,
            ..
        } = implied.step.target
        else {
            unreachable!("an implied class maps a namespace");
        };
        add_specializes_edge_opinions(
            store,
            selection_stack,
            dest_root,
            dest_root,
            implied_root,
            implied.step.layer_stack,
            namespace_depth,
            arc_list_index,
            ArcParent::implied_from(&implied.parent, origin.clone()),
            out,
            visited,
            prim_order_out,
            authored_children_out,
            None,
            implied.step.layer_offset,
            cycles,
            deps.as_deref_mut(),
        );
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
            selection_stack,
            local_stack,
            selection_stack,
            specialized_root,
            &pairs,
            &mut host_selection_cache,
        )
    };
    mapping.retain(|(remote, _)| !is_at_or_under(store, *remote, &unselected));

    // The specialized prim's own opinions are the specializes node's; arcs
    // authored inside it nest under that node.
    let mut nodes = ArcNodes::new(parent, step);
    record_offset_layers(deps.as_deref_mut(), &nodes.step().offset_layers, &mapping);

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
                        selection_stack,
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
                            let variant_node =
                                nodes.variant_node(store, out, *dest_path_id, &branch_selections);
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
                                    variant_node,
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
            selection_stack,
            local_stack,
            selection_stack,
            specialized_root,
            remote_path_id,
            selection_path_id,
            &mut host_selection_cache,
            arc_stack,
        );
        // A specializes authored inside the specialized prim leaves a
        // placeholder beneath this node and is propagated to the root, where
        // it ranks by that placeholder (AOUSD Core §10.4.1).
        for (nested_index, (nested, sites)) in arcs.specializes.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_specializes_edge_opinions(
                store,
                selection_stack,
                dest_path_id,
                selection_path_id,
                nested,
                arc_stack,
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
    let mut visited_inherits: VisitedClasses = VisitedClasses::new();
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
            selection_stack,
            local_stack,
            selection_stack,
            specialized_root,
            remote_path_id,
            selection_path_id,
            &mut host_selection_cache,
            arc_stack,
        );
        let nested_inherits = arcs.inherits;
        for (nested_index, (inherited, sites)) in nested_inherits.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites);
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // A class nested in the specialized class's namespace is implied
            // into the destination's namespace by its own expansion (see
            // `implied_classes`).
            add_inherit_edge_opinions(
                store,
                local_stack,
                selection_stack,
                dest_path_id,
                inherited,
                arc_stack,
                namespace_depth,
                nested_index,
                ArcParent::nested(&branch),
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
            selection_stack,
            local_stack,
            selection_stack,
            specialized_root,
            remote_path_id,
            selection_path_id,
            &mut host_selection_cache,
            arc_stack,
        );
        for (ref_index, (reference, sites)) in arcs.references.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites);
            let ref_index = u16::try_from(ref_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            add_reference_edge_opinions(
                store,
                selection_stack,
                dest_path_id,
                reference,
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
            let branch = nodes.branch_path(&sites);
            let payload_index = u16::try_from(payload_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                selection_stack,
                dest_path_id,
                payload,
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

    // The arcs the specialized prim's ancestors author (see
    // `AncestralArcs`).
    AncestralArcs {
        data_stack: local_stack,
        selection_stack,
        ancestor_stack: selection_stack,
        arc_stack,
        dest_root,
        target: specialized_root,
        layer_offset: base_offset,
        ref_remap: None,
        class_arc: true,
    }
    .expand(
        store,
        &nodes,
        out,
        &mut visited_refs,
        &mut visited_inherits,
        visited,
        prim_order_out,
        authored_children_out,
        cycles,
        deps,
    );
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
        let Some(index) = prims.get(parent) else {
            continue;
        };
        fold_child_order(
            store,
            &index.graph,
            list,
            &index.sources,
            authored_children.get(parent).map_or(&[], Vec::as_slice),
            prim_order.get(parent).map_or(&[], Vec::as_slice),
        );
    }
}

/// Folds the `authored_children` and `prim_order` opinions of one prim's
/// sources into its child order, weakest first.
///
/// Each spec appends the children it names that are not yet listed, then
/// its `reorder nameChildren` reorders the names gathered so far, from its
/// own spec and every weaker one. A spec that lists no children (one
/// authored through `Layer::insert_prim`, whose hierarchy comes from prim
/// paths) names the children its layer holds prim specs for, whatever
/// other specs list. Children no spec names follow in their population
/// order.
///
/// Spec: AOUSD Core §11 (stage population). OpenUSD:
/// `PcpPrimIndex::ComputePrimChildNames`, which walks the prim index's
/// nodes from weakest to strongest, and `PcpComposeSiteChildNames` in
/// `pxr/usd/pcp/composeSite.cpp`, which walks each node's layers from
/// weakest to strongest and applies each layer's `primOrder` as it goes.
fn fold_child_order(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    children: &mut Vec<PathId>,
    sources: &[OpinionKey],
    authored: &[(OpinionKey, Vec<TokenId>)],
    orders: &[(OpinionKey, Vec<TokenId>)],
) {
    if authored.is_empty() && orders.is_empty() {
        return;
    }
    let mut by_name = HashMap::<TokenId, PathId>::new();
    for child in children.iter().copied() {
        if let Some(name) = store.paths().resolve(child).leaf() {
            by_name.insert(name, child);
        }
    }

    // A spec that lists no children names those its layer holds prim specs
    // for. Each spec's list stands on its own: a stronger spec's explicit
    // list never hides a child from a weaker spec.
    let explicit: HashSet<&OpinionKey> = authored.iter().map(|(key, _)| key).collect();
    let names: Vec<TokenId> = children
        .iter()
        .filter_map(|child| store.paths().resolve(*child).leaf())
        .collect();
    let derived: Vec<(&OpinionKey, Vec<TokenId>)> = sources
        .iter()
        .filter(|key| !explicit.contains(key))
        .map(|key| (key, spec_children(store, key, &names)))
        .filter(|(_, names)| !names.is_empty())
        .collect();

    // A spec's children come before its reorder: `false` sorts first.
    let mut steps: Vec<(&OpinionKey, bool, &[TokenId])> = authored
        .iter()
        .map(|(key, names)| (key, false, names.as_slice()))
        .chain(
            derived
                .iter()
                .map(|(key, names)| (*key, false, names.as_slice())),
        )
        .chain(
            orders
                .iter()
                .map(|(key, order)| (key, true, order.as_slice())),
        )
        .collect();
    steps.sort_by(|a, b| graph.cmp_keys(b.0, a.0).then(a.1.cmp(&b.1)));

    let mut out = Vec::with_capacity(children.len());
    let mut seen = HashSet::<PathId>::new();
    for (_, is_order, names) in steps {
        if is_order {
            apply_reorder_op(store, &mut out, names);
            continue;
        }
        for name in names {
            let Some(child_id) = by_name.get(name).copied() else {
                continue;
            };
            if seen.insert(child_id) {
                out.push(child_id);
            }
        }
    }
    for child_id in children.iter().copied() {
        if seen.insert(child_id) {
            out.push(child_id);
        }
    }
    *children = out;
}

/// The names among `candidates`, in their order, that the spec `key`
/// names has a child prim spec for.
fn spec_children(store: &dyn LayerStore, key: &OpinionKey, candidates: &[TokenId]) -> Vec<TokenId> {
    let Some(layer) = store.layer(key.layer_id) else {
        return Vec::new();
    };
    let paths = store.paths();
    let lookup = paths.resolve(key.lookup_path);
    let spec_prim = paths.resolve(key.spec_path.prim_path());
    candidates
        .iter()
        .copied()
        .filter(|name| {
            let Some(child_lookup) = paths.lookup(&lookup.join(&[*name])) else {
                return false;
            };
            let Some(child_prim) = paths.lookup(&spec_prim.join(&[*name])) else {
                return false;
            };
            let spec = key.spec_path.prim_spec().child(*name, child_prim);
            layer.source_prim_spec(child_lookup, &spec, paths).is_some()
        })
        .collect()
}

/// Reorders `children` by one `reorder nameChildren` list.
///
/// The children before the first one `order` names keep their place in
/// front. Each named child then follows in `order`'s order, and carries
/// along the unnamed children that follow it. Names of absent children are
/// ignored.
///
/// OpenUSD: `SdfApplyListOrdering` in `pxr/usd/sdf/listOp.cpp`.
fn apply_reorder_op(store: &dyn LayerStore, children: &mut Vec<PathId>, order: &[TokenId]) {
    let mut by_name = HashMap::<TokenId, PathId>::new();
    for child in children.iter().copied() {
        if let Some(name) = store.paths().resolve(child).leaf() {
            by_name.insert(name, child);
        }
    }

    let mut order_set = HashSet::<TokenId>::new();
    let order: Vec<TokenId> = order
        .iter()
        .copied()
        .filter(|name| by_name.contains_key(name) && order_set.insert(*name))
        .collect();
    if order.is_empty() {
        return;
    }

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
    out.extend(prefix);
    for name in &order {
        out.push(by_name[name]);
        if let Some(seg) = segments.get(name) {
            out.extend(seg.iter().copied());
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

    /// A hierarchy authored through `Layer::insert_prim` leaves
    /// `authored_children` empty; a `reorder nameChildren` still orders the
    /// children population finds, in every layer of the stack.
    ///
    /// Spec: AOUSD Core §11 (stage population).
    #[test]
    fn reorder_applies_to_children_no_spec_lists() {
        let mut store = InMemoryStore::default();
        let [p, a, b, c] = ["/P", "/P/A", "/P/B", "/P/C"].map(|path| {
            store
                .paths
                .intern(Path::parse_absolute(path, &mut store.tokens).unwrap())
        });
        let name = |store: &InMemoryStore, path: PathId| store.paths.resolve(path).leaf().unwrap();
        let (a_tok, b_tok, c_tok) = (name(&store, a), name(&store, b), name(&store, c));

        let mut weak = Layer::new(LayerId(2));
        weak.insert_prim(
            p,
            PrimSpec {
                prim_order: Some(vec![b_tok, a_tok]),
                ..PrimSpec::def()
            },
        );
        weak.insert_prim(a, PrimSpec::def());
        weak.insert_prim(b, PrimSpec::def());
        store.insert_layer(weak);
        let stage = Stage::compose(&mut store, LayerId(2), StageOptions::default());
        assert_eq!(stage.children_of(p), Some(&[b, a][..]));

        // A stronger layer adds `C` and moves it first.
        let mut strong = Layer::new(LayerId(1));
        strong.sublayers = vec![crate::SublayerEntry::new(LayerId(2))];
        strong.insert_prim(
            p,
            PrimSpec {
                prim_order: Some(vec![c_tok, b_tok]),
                ..PrimSpec::over()
            },
        );
        strong.insert_prim(c, PrimSpec::def());
        store.insert_layer(strong);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(stage.children_of(p), Some(&[c, b, a][..]));
    }

    /// Each spec contributes its own children, listed in `authored_children`
    /// or inferred from its layer's prim specs, then applies its reorder,
    /// weakest spec first: a stronger spec's explicit list never hides a
    /// child from a weaker spec's inferred list or reorder. Each case runs
    /// with every mix of explicit and inferred lists (an inferred list
    /// follows population order, so only a list in that order is inferred);
    /// the expected orders are OpenUSD 26.08's, which always lists a
    /// layer's children.
    ///
    /// Spec: AOUSD Core §11 (stage population). OpenUSD:
    /// `PcpComposeSiteChildNames` in `pxr/usd/pcp/composeSite.cpp`.
    #[test]
    fn explicit_and_inferred_child_lists_fold_per_spec() {
        // (weak children, weak reorder, strong children, strong reorder,
        // composed order)
        let cases = [
            ("A B", "B A", "B", "", "B A"),
            ("A B", "B A", "C D", "D A", "B D A C"),
            ("A B C", "", "C A", "", "A B C"),
            ("A B C", "", "C A D", "", "A B C D"),
            ("C B A", "", "B D", "D C", "D C B A"),
        ];
        let sorted = |names: &str| names.split_whitespace().is_sorted();
        for (weak_names, weak_order, strong_names, strong_order, expected) in cases {
            for (weak_explicit, strong_explicit) in
                [(false, false), (false, true), (true, false), (true, true)]
            {
                if (!weak_explicit && !sorted(weak_names))
                    || (!strong_explicit && !sorted(strong_names))
                {
                    continue;
                }
                let mut store = InMemoryStore::default();
                let p = store
                    .paths
                    .intern(Path::parse_absolute("/P", &mut store.tokens).unwrap());
                let mut tokens = |names: &str| -> Vec<TokenId> {
                    names
                        .split_whitespace()
                        .map(|name| store.tokens.intern(name))
                        .collect()
                };
                let layers = [
                    (LayerId(2), weak_names, weak_order, weak_explicit),
                    (LayerId(1), strong_names, strong_order, strong_explicit),
                ]
                .map(|(id, names, order, explicit)| (id, tokens(names), tokens(order), explicit));
                for (id, names, order, explicit) in layers {
                    let mut layer = Layer::new(id);
                    if id == LayerId(1) {
                        layer.sublayers = vec![crate::SublayerEntry::new(LayerId(2))];
                    }
                    let mut parent = if id == LayerId(1) {
                        PrimSpec::over()
                    } else {
                        PrimSpec::def()
                    };
                    parent.prim_order = (!order.is_empty()).then_some(order);
                    if explicit {
                        parent.authored_children.clone_from(&names);
                    }
                    layer.insert_prim(p, parent);
                    for name in names {
                        let child = store.paths.intern(store.paths.resolve(p).join(&[name]));
                        layer.insert_prim(child, PrimSpec::over());
                    }
                    store.insert_layer(layer);
                }
                let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
                let composed: Vec<&str> = stage
                    .children_of(p)
                    .unwrap_or_default()
                    .iter()
                    .map(|child| {
                        let name = store.paths.resolve(*child).leaf().unwrap();
                        store.tokens.resolve(name)
                    })
                    .collect();
                let composed = composed.join(" ");
                assert_eq!(
                    composed, expected,
                    "weak {weak_names} reorder {weak_order:?} (explicit: {weak_explicit}), \
                     strong {strong_names} reorder {strong_order:?} (explicit: {strong_explicit})"
                );
            }
        }
    }

    /// One `reorder nameChildren` keeps the children before the first name
    /// it lists in front, and each listed child carries along the unlisted
    /// children that follow it (checked against OpenUSD 26.08).
    ///
    /// OpenUSD: `SdfApplyListOrdering` in `pxr/usd/sdf/listOp.cpp`.
    #[test]
    fn reorder_keeps_the_leading_children_in_front() {
        let mut store = InMemoryStore::default();
        let mut reorder = |children: &str, order: &str| {
            let names: Vec<TokenId> = children
                .chars()
                .map(|c| store.tokens.intern(alloc::format!("{c}")))
                .collect();
            let mut paths: Vec<PathId> = names
                .iter()
                .map(|name| store.paths.intern(Path::root().join(&[*name])))
                .collect();
            let order: Vec<TokenId> = order
                .chars()
                .map(|c| store.tokens.intern(alloc::format!("{c}")))
                .collect();
            apply_reorder_op(&store, &mut paths, &order);
            paths
                .iter()
                .map(|path| {
                    let name = store.paths.resolve(*path).leaf().unwrap();
                    alloc::string::String::from(store.tokens.resolve(name))
                })
                .collect::<alloc::string::String>()
        };
        assert_eq!(reorder("cbad", "ad"), "cbad");
        assert_eq!(reorder("abcd", "db"), "adbc");
        assert_eq!(reorder("abcde", "eb"), "aebcd");
        assert_eq!(reorder("abcde", "ca"), "cdeab");
        assert_eq!(reorder("abc", "xba"), "bca");
    }

    #[test]
    fn authored_children_compose_weakest_node_first() {
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

        // The prim's local node; a reference at `/P` to `/S`, whose `/S/I`
        // references `/Prop` and inherits `/_C`; and that inherit implied
        // into the root layer stack beneath the root node, with the
        // authored inherit as its origin.
        let mut graph = PrimIndexGraph::from_arcs(
            &prim_spec_path(&store, sp_local, &[]),
            2,
            [
                (NodeId::ROOT, ArcKind::References, 1),
                (NodeId::from_raw(1), ArcKind::References, 2),
                (NodeId::from_raw(1), ArcKind::Inherits, 2),
                (NodeId::ROOT, ArcKind::Inherits, 2),
            ],
        );
        graph.set_origin(NodeId::from_raw(4), NodeId::from_raw(3));
        graph.rank();
        let ids: Vec<NodeId> = graph.nodes().map(|(id, _)| id).collect();
        let key = |node: NodeId, layer_id: LayerId, path: PathId| OpinionKey {
            node,
            layer_strength: 0,
            layer_id,
            lookup_path: path,
            spec_path: prim_spec_path(&store, path, &[]),
        };
        let opinions: Vec<(OpinionKey, Vec<TokenId>)> = vec![
            (key(ids[0], root_layer, sp_local), vec![from_root, geom_tok]),
            (key(ids[1], set_layer, sp_set), vec![from_set, geom_tok]),
            (key(ids[2], prop_layer, sp_prop), vec![geom_tok]),
            (
                key(ids[3], set_layer, sp_class),
                vec![from_class_set, geom_tok],
            ),
            (
                key(ids[4], root_layer, sp_class),
                vec![from_class_root, geom_tok],
            ),
        ];

        let mut children = vec![c_geom, c_fr, c_fs, c_fcr, c_fcs];
        fold_child_order(&store, &graph, &mut children, &[], &opinions, &[]);

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
                ..PrimIndex::new(PrimIndexGraph::from_arcs(
                    &prim_spec_path(&store, parent_path, &[]),
                    1,
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
