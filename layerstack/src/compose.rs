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

use crate::variable_expression::ExpressionVariables;
use crate::variant_fallbacks::{VariantFallbacks, apply_variant_fallbacks};
use crate::{
    arc_cycle::{ChainState, CycleDetector},
    arcs::{
        ArcAuthoring, AuthoredReference, HostSpec, NodeLists, SelectionScope, Sited,
        VariantNodeOrder, anchor_internal_arcs, arcs_of, lookup_reference_target_path,
        resolve_branch_payloads_in, resolve_direct_references_for_prim, resolve_inherits_for_prim,
        resolve_inherits_for_prim_in, resolve_payloads_for_prim, resolve_payloads_for_prim_in,
        resolve_references_for_prim_selected, resolve_specializes_for_prim,
        resolve_specializes_for_prim_in, resolve_variant_branch_payloads,
        resolve_variant_child_references, resolve_variant_references_in, selection_host_specs,
        spec_arcs_apply,
    },
    composition_checks::{
        ArcPathMap, TargetOwner, TargetSpecsCheck, drop_inconsistent_property_kinds,
        drop_instance_targets, map_arc_targets, target_error_applies,
    },
    composition_error::{
        ArcToProhibitedChild, CompositionError, UnresolvedAsset, UnresolvedDefaultPrim,
    },
    dependency_map::{ArcDependency, DependencyBuilder},
    doc::{LayerId, LayerOffset, LayerStore, Reference, ReferenceTarget, composed_entries},
    expression_variables::{
        ArcAnchor, ExpressionScope, SiteContext, composed_variables, node_chain, node_variables,
        read_selections, same_context, site_selections,
    },
    interner::TokenId,
    layer_stack::LayerStack,
    path::PathId,
    population::populate,
    prim_index::{ArcKind, Opinion, OpinionKey, OpinionValue, PrimIndex},
    prim_index_graph::{NodeArc, NodeId, PrimIndexGraph, PrimNode},
    property::PropertyType,
    relocates::{LiftedSet, Relocations, Walk},
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
    // The variant fallbacks every selection is resolved with, passed
    // explicitly to each function that resolves selections.
    let fallbacks = &options.variant_fallbacks;
    let mut cycles = CycleDetector::new(root);
    let layer_stack = cycles.gather_layer_stack(store, root);
    // Spec: AOUSD Core §10.3.2.6 (relocates are computed per layer stack;
    // invalid ones are composition errors of the layer stack authoring
    // them).
    cycles.set_relocations(Relocations::new(store, &layer_stack));
    let (paths, mut children) = populate(
        store,
        &layer_stack,
        options.mask.as_ref(),
        cycles.relocations_mut(),
    );
    for error in cycles.relocations_mut().take_errors() {
        cycles.report(error);
    }
    let stage_relocates = cycles.relocations().stage();
    cycles.report_source_opinions(store, &stage_relocates);

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
        fallbacks,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        dep_builder.as_mut(),
    );
    add_relocated_variant_opinions(
        store,
        &layer_stack,
        &stage_relocates,
        &[],
        &stage_relocates,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        &mut cycles,
    );
    add_inherit_opinions(
        store,
        fallbacks,
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
        fallbacks,
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
        fallbacks,
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
        fallbacks,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        &mut cycles,
        dep_builder.as_mut(),
    );
    add_late_variant_branches(
        store,
        fallbacks,
        &layer_stack,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        &mut cycles,
        dep_builder.as_mut(),
    );

    let blocked = cycles.relocations().blocked();
    for (path, prim) in &mut prims {
        crate::relocates::elide_blocked(
            prim,
            store.paths(),
            blocked,
            [
                prim_order_opinions.get_mut(path),
                authored_children_opinions.get_mut(path),
            ],
        );
        drop_skipped_duplicates(store, prim);
        prune_skipped_nodes(
            prim,
            [
                prim_order_opinions.get_mut(path),
                authored_children_opinions.get_mut(path),
            ],
        );
        prim.finalize();
    }
    cycles.report_relocate_node_opinions(store, &prims);

    prune_unselected_variant_specs(store, fallbacks, &layer_stack, &mut prims);

    relocated_child_names(
        store,
        cycles.relocations(),
        &prims,
        &mut authored_children_opinions,
    );
    apply_child_order(
        store,
        &prims,
        &authored_children_opinions,
        &prim_order_opinions,
        &mut children,
    );

    filter_variant_children(store, fallbacks, &prims, &mut children);

    let mut instances = strip_instance_descendants(
        store,
        &mut prims,
        &mut children,
        &authored_children_opinions,
    );

    prune_deactivated(store, &mut prims, &mut children);

    // Runs last so the ordering passes above see the populated child lists;
    // removal only drops entries.
    remove_prims_without_specs(store, &mut prims, &mut children, |path| {
        cycles.relocations().is_target(path)
    });
    remove_relocation_sources(store, cycles.relocations(), &mut prims, &mut children);
    instances.retain(|instance| prims.contains_key(instance));
    crate::path_expression::anchor_opinions(store, &mut prims);

    for (path, prim) in &mut prims {
        drop_inconsistent_property_kinds(*path, prim, &mut cycles);
    }
    drop_instance_targets(store, &mut prims, &mut cycles);
    // Variant selections authored as expressions are evaluated where they
    // are read (`site_selections`); those composition reads are errors and
    // dependencies.
    let (errors, reads) = read_selections(store, &prims);
    for error in errors {
        cycles.report(error);
    }
    cycles.add_variable_reads(reads);

    if let Some(builder) = dep_builder.as_mut() {
        builder.retain_prims(&prims);
        builder.add_relocation_layers(cycles.relocations().layers());
        builder.add_expression_variable_reads(cycles.take_variable_reads());
    }
    let dependencies = dep_builder.map(DependencyBuilder::finish);
    // Only prims of the composed stage report arc errors: population
    // over-approximates, and pruned prims are not part of the stage.
    let errors = cycles
        .into_errors()
        .into_iter()
        .filter(|error| error.prim().is_none_or(|prim| prims.contains_key(&prim)))
        // Target path errors of specs a stronger explicit list replaces.
        .filter(|error| target_error_applies(error, &prims))
        .collect();
    Stage::from_parts(prims, children, options.with_provenance, dependencies)
        .with_composition_errors(errors)
        .with_instances(instances)
        .with_variant_fallbacks(options.variant_fallbacks.clone())
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
/// and every such registration of a site but one: the one OpenUSD adds
/// first where the graph shows it (see [`PrimIndexGraph::implied_after`]),
/// else the strongest.
///
/// OpenUSD adds no node for a site the prim index already uses while it
/// builds the recursive index of a class arc's ancestors, whichever arc
/// reaches the site first in its evaluation order; composition here does
/// not expand arcs in that order, so it adds those nodes and drops their
/// duplicate registrations once the graph is complete. A class implied from
/// a node comes after that node's subtree, so a site both reach stays
/// beneath the node, however strong the implied class is.
///
/// Two registrations are of one site only when their nodes read the layer
/// with the same expression variables: a layer stack reached with other
/// variables is another layer stack
/// (`PcpLayerStackIdentifier::expressionVariablesOverrideSource`).
fn drop_skipped_duplicates(store: &dyn LayerStore, prim: &mut PrimIndex) {
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
        // A site: its layer and spec path, in its node's context.
        let mut contexts: HashMap<NodeId, ExpressionVariables> = HashMap::new();
        let mut site = |key: &OpinionKey| {
            let variables = contexts
                .entry(key.node)
                .or_insert_with(|| node_variables(store, graph, key.node))
                .clone();
            (key.layer_id, key.spec_path.clone(), variables)
        };
        let kept: HashSet<(LayerId, SpecPath, ExpressionVariables)> = sources
            .iter()
            .filter(|key| !skips(key.node))
            .map(|key| site(key))
            .collect();
        // The registration OpenUSD adds first, else the strongest.
        let mut first: HashMap<(LayerId, SpecPath, ExpressionVariables), &OpinionKey> =
            HashMap::new();
        for key in sources.iter().filter(|key| skips(key.node)) {
            let site = site(key);
            if kept.contains(&site) {
                continue;
            }
            let winner = first.entry(site).or_insert(key);
            if graph.implied_after(key.node, winner.node) {
                *winner = key;
            }
        }
        sources
            .into_iter()
            .filter(|key| {
                skips(key.node) && first.get(&site(key)).is_none_or(|winner| *winner != *key)
            })
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
pub(crate) type ChildOrderOpinions = Vec<(OpinionKey, Vec<TokenId>)>;

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
/// A relocation target is kept (`keep`) whatever its specs: a relocation
/// makes it a child of its parent (AOUSD Core §11.3.1).
///
/// Spec: AOUSD Core §11 (stage population from composed prim indexes);
/// OpenUSD only populates prims whose index has specs
/// (`PcpPrimIndex::HasSpecs`).
fn remove_prims_without_specs(
    store: &dyn LayerStore,
    prims: &mut HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
    keep: impl Fn(PathId) -> bool,
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
                    && !keep(**path)
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

/// Removes the prims at or beneath a relocation source, as lifted into the
/// stage namespace (see [`Relocations`]), with every child entry naming
/// them.
///
/// A relocated prim's source path is prohibited in the namespace of the
/// relocating layer stack, and so in every namespace that layer stack is
/// mapped into: nothing composes there, whatever other arcs bring.
///
/// Spec: AOUSD Core §10.3.2.6. OpenUSD: `_ComposeIsProhibitedPrimChild` in
/// `pxr/usd/pcp/primIndex.cpp`, and the prohibited child names of
/// `PcpPrimIndex::ComputePrimChildNames`.
fn remove_relocation_sources(
    store: &dyn LayerStore,
    relocations: &Relocations,
    prims: &mut HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
) {
    let paths = store.paths();
    prims.retain(|path, _| !relocations.is_prohibited(paths, *path));
    children.retain(|path, _| prims.contains_key(path));
    for list in children.values_mut() {
        list.retain(|child| prims.contains_key(child));
    }
}

/// Resolves the variant selections that govern a composed prim, in strength
/// order.
///
/// The authored selections come from
/// [`authored_strength_ordered_variant_selections`], which evaluates the
/// variant sets one at a time, as OpenUSD does; sets it leaves without a
/// selection take the variant fallbacks. A branch counts only once an authored selection
/// selects it: a branch a fallback selected authors selections only for
/// the sets declared after its own, which the fallback pass decides
/// ([`apply_variant_fallbacks`]), so pruning keeps every fallback branch
/// composition kept.
///
/// Spec: AOUSD Core §10.5 (the strongest variant selection opinion in the
/// prim index wins, independent of which arc introduced the variant set).
pub(crate) fn strength_ordered_variant_selections(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    prim_index: &PrimIndex,
) -> HashMap<TokenId, TokenId> {
    let (graph, sources) = (&prim_index.graph, &prim_index.sources[..]);
    let mut selections = authored_strength_ordered_variant_selections(store, graph, sources);
    apply_fallbacks_at(store, fallbacks, &mut selections, graph, sources, &[]);
    selections
}

/// The selections of [`strength_ordered_variant_selections`] before variant
/// fallbacks apply, for callers that add weaker selections first.
///
/// Each of `sources`, sites of the prim index whose graph is `graph`, is
/// a [`VariantSite`], read in its node's context: a spec outside the
/// prim's own variant branches, or the branch its spec path ends in
/// (`/P{a=x}`, or `/P{a=x}{b=y}` for a set nested in another branch), in
/// the strength order of the prim index, where a branch of the prim's own
/// set is stronger than a referenced site. [`evaluate_variant_sets`]
/// decides the sets they declare; any other selection a source authors,
/// and those of [`authored_composed_variant_selections`], fill in the rest.
fn authored_strength_ordered_variant_selections(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    sources: &[OpinionKey],
) -> HashMap<TokenId, TokenId> {
    let sites: Vec<VariantSite<'_>> = sources
        .iter()
        .filter_map(|source| {
            let spec = store.layer(source.layer_id).and_then(|layer| {
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            })?;
            let context = SiteContext::Node(graph, source.node);
            Some(VariantSite::of_spec(
                store,
                spec,
                source.spec_path.variant_chain(),
                context,
            ))
        })
        .collect();
    let mut selections = evaluate_variant_sets(&sites);
    for (set, variant) in authored_composed_variant_selections(store, graph, sources) {
        selections.entry(set).or_insert(variant);
    }
    selections
}

/// A site that may author and declare variant selections for a composed
/// prim: a prim spec, or one of the prim's own variant specs in it.
///
/// OpenUSD reads a node's site path, which ends in the node's variant
/// selections for a variant node (`_ComposeVariantSelectionAcrossNodes` and
/// `_EvalNodeVariantSets` in `pxr/usd/pcp/primIndex.cpp`).
struct VariantSite<'a> {
    /// The path of the site's variant spec on the prim spec, outermost
    /// first (`[(a, x), (b, y)]` for `/P{a=x}{b=y}`): empty for the prim
    /// spec itself.
    branch: Vec<(TokenId, TokenId)>,
    /// The selections the site authors, evaluated where it is read.
    authored: Option<Cow<'a, HashMap<TokenId, TokenId>>>,
    /// The variant sets the site declares, in `variantSets` order; `None`
    /// for a site that declares none.
    declared: Option<&'a [TokenId]>,
}

impl<'a> VariantSite<'a> {
    /// The site of `spec`, or of its variant spec at `branch` (outermost
    /// first), read in `context`.
    fn of_spec(
        store: &dyn LayerStore,
        spec: &'a crate::doc::PrimSpec,
        branch: Vec<(TokenId, TokenId)>,
        context: SiteContext<'_>,
    ) -> Self {
        let (authored, declared) = if branch.is_empty() {
            (
                Some(&spec.variant_selections),
                Some(spec.variant_set_order.as_slice()),
            )
        } else {
            match spec.variant_spec(&branch) {
                Some(variant) => (
                    Some(&variant.variant_selections),
                    Some(variant.variant_set_order.as_slice()),
                ),
                None => (None, None),
            }
        };
        Self {
            branch,
            authored: authored.map(|authored| site_selections(store, authored, context)),
            declared,
        }
    }

    /// The sites of the node whose layer stack holds `specs`, the specs of
    /// one prim with the chains reaching them, stronger layers first: each
    /// spec, then each variant branch in the node's strength order.
    fn of_node(store: &dyn LayerStore, specs: &[HostSpec<'a, '_>]) -> Vec<Self> {
        let mut sites: Vec<Self> = specs
            .iter()
            .map(|(spec, chain)| Self::of_spec(store, spec, Vec::new(), SiteContext::Chain(chain)))
            .collect();
        sites.extend(Self::branches_of_node(store, specs));
        sites
    }

    /// The sites of the variant branches of the node whose layer stack
    /// holds `specs`, the specs of one prim with the chains reaching them,
    /// stronger layers first: in the order of their variant nodes
    /// ([`VariantNodeOrder`]), each node's specs by layer.
    fn branches_of_node(store: &dyn LayerStore, specs: &[HostSpec<'a, '_>]) -> Vec<Self> {
        let order = VariantNodeOrder::new(specs.iter().map(|(spec, _)| *spec));
        // Each branch with its node's rank, then its layer.
        let mut branches: Vec<(Vec<usize>, usize, Self)> = Vec::new();
        for (layer, (spec, chain)) in specs.iter().enumerate() {
            for branch in spec.variant_branches() {
                let path: Vec<(TokenId, TokenId)> = branch.chain().collect();
                let node = order.rank(&path);
                let authored = site_selections(
                    store,
                    &branch.spec.variant_selections,
                    SiteContext::Chain(chain),
                );
                let site = Self {
                    branch: path,
                    authored: Some(authored),
                    declared: Some(branch.spec.variant_set_order.as_slice()),
                };
                branches.push((node, layer, site));
            }
        }
        // Variants of one set exclude each other, so their relative order
        // does not matter.
        branches.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        branches.into_iter().map(|(_, _, site)| site).collect()
    }

    /// Selections authored at a site that declares no variant set.
    fn authored_only(authored: &'a HashMap<TokenId, TokenId>) -> Self {
        Self {
            branch: Vec::new(),
            authored: Some(Cow::Borrowed(authored)),
            declared: None,
        }
    }

    /// Whether every branch the site lies in is selected.
    fn composed(&self, selections: &HashMap<TokenId, TokenId>) -> bool {
        self.branch
            .iter()
            .all(|(set, variant)| selections.get(set) == Some(variant))
    }

    /// The variant sets the site declares, in `variantSets` order: those of
    /// its prim spec, or those nested in its variant spec.
    fn declared(&self) -> impl Iterator<Item = TokenId> + '_ {
        self.declared.into_iter().flatten().copied()
    }
}

/// Resolves the authored variant selections of a composed prim from its
/// `sites`, strongest first.
///
/// A branch site composes only while every branch it names is selected.
/// The variant sets are evaluated one at a time, each once: next is the
/// first set, in `variantSets` order, of the strongest composed site that
/// declares a set not yet evaluated (a set nested in a branch is declared
/// by that branch). Its selection is the one authored on the strongest
/// composed site that authors one, and selecting it composes its branch
/// sites, whose selections count for the sets evaluated after it at the
/// strength of their place among `sites`. A set no composed site selects is
/// evaluated again once a newly selected branch composes, and otherwise
/// stays unselected, for the fallbacks. The sets no site declares then take
/// the strongest selection left.
///
/// Spec: AOUSD Core §10.3.2.5.1 (computing variant selection), §10.5.
/// OpenUSD queues a task per declared set of each node
/// (`_EvalNodeVariantSets`), processes them in node strength order and
/// then set order (`Task::PriorityOrder`), resolves each with
/// `_ComposeVariantSelection`, which takes a set's prior selection or
/// searches the nodes added so far strongest first, and retries the sets
/// left unselected when a new variant arc may author selections
/// (`_AddVariantArc`, `RetryVariantTasks`), all in
/// `pxr/usd/pcp/primIndex.cpp`.
fn evaluate_variant_sets(sites: &[VariantSite<'_>]) -> HashMap<TokenId, TokenId> {
    let mut selections: HashMap<TokenId, TokenId> = HashMap::new();
    // Sets evaluated without a selection, until a new branch composes.
    let mut unselected: HashSet<TokenId> = HashSet::new();
    loop {
        let next = sites
            .iter()
            .filter(|site| site.composed(&selections))
            .flat_map(VariantSite::declared)
            .find(|set| !selections.contains_key(set) && !unselected.contains(set));
        let Some(set) = next else {
            break;
        };
        let found = sites
            .iter()
            .filter(|site| site.composed(&selections))
            .find_map(|site| site.authored.as_ref()?.get(&set).copied());
        match found {
            Some(variant) => {
                selections.insert(set, variant);
                unselected.clear();
            }
            None => {
                unselected.insert(set);
            }
        }
    }

    // Sets no composed site declares: the strongest selection, as each
    // newly selected branch composes.
    loop {
        let mut changed = false;
        for site in sites {
            if !site.composed(&selections) {
                continue;
            }
            for (set, variant) in site.authored.iter().flat_map(|authored| authored.iter()) {
                if !selections.contains_key(set) && !unselected.contains(set) {
                    selections.insert(*set, *variant);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    selections
}

/// Applies `fallbacks` to `selections`, every authored selection of a
/// prim, for the variant sets of its specs: the source specs `sources`,
/// strongest first, then the specs of each `(stack, prim)` site (see
/// [`apply_variant_fallbacks`]). Callers gather every authored selection
/// first, so a selection authored anywhere wins over a fallback.
fn apply_fallbacks_at(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    selections: &mut HashMap<TokenId, TokenId>,
    graph: &PrimIndexGraph,
    sources: &[OpinionKey],
    sites: &[(&LayerStack, PathId)],
) {
    if fallbacks.is_empty() {
        return;
    }
    let source_specs = sources.iter().filter_map(|source| {
        let spec = store.layer(source.layer_id).and_then(|layer| {
            layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
        })?;
        Some((spec, SiteContext::Node(graph, source.node)))
    });
    let site_specs = sites.iter().flat_map(|(stack, prim)| {
        stack
            .layers
            .iter()
            .filter_map(|id| store.layer(*id))
            .flat_map(move |layer| {
                let context = SiteContext::Chain(stack.chain_of(layer.id));
                layer.prim_specs(*prim).map(move |spec| (spec, context))
            })
    });
    let specs: Vec<(&crate::doc::PrimSpec, SiteContext<'_>)> =
        source_specs.chain(site_specs).collect();
    apply_variant_fallbacks(store, fallbacks, selections, &specs);
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
    fallbacks: &VariantFallbacks,
    prim_index: &PrimIndex,
) -> HashMap<TokenId, TokenId> {
    let (graph, sources) = (&prim_index.graph, &prim_index.sources[..]);
    let mut selections = authored_composed_variant_selections(store, graph, sources);
    apply_fallbacks_at(store, fallbacks, &mut selections, graph, sources, &[]);
    selections
}

/// The selections of [`composed_variant_selections`] before variant
/// fallbacks apply.
fn authored_composed_variant_selections(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    sources: &[OpinionKey],
) -> HashMap<TokenId, TokenId> {
    let mut selections: HashMap<TokenId, TokenId> = HashMap::new();
    for source in sources {
        let Some(layer) = store.layer(source.layer_id) else {
            continue;
        };
        let Some(spec) =
            layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
        else {
            continue;
        };
        let context = SiteContext::Node(graph, source.node);
        let authored = site_selections(store, &spec.variant_selections, context);
        for (set, variant) in authored.iter() {
            selections.entry(*set).or_insert(*variant);
        }
    }

    // Expand selections from within selected variant branches (chaining).
    loop {
        let mut new_sels = HashMap::new();
        for source in sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            let context = SiteContext::Node(graph, source.node);
            for branch in spec.selected_variant_branches(&selections) {
                let inner = site_selections(store, &branch.spec.variant_selections, context);
                for (inner_set, inner_variant) in inner.iter() {
                    if !selections.contains_key(inner_set) {
                        new_sels.entry(*inner_set).or_insert(*inner_variant);
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
    fallbacks: &VariantFallbacks,
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
            // A spec is checked once per expression variable context its
            // nodes read it in: its selections may differ by context.
            let mut rejected: HashSet<(LayerId, SpecPath, ExpressionVariables)> = HashSet::new();
            {
                let index = &prims[&prim_path];
                let mut checked: HashSet<(LayerId, &SpecPath, ExpressionVariables)> =
                    HashSet::new();
                let all_keys = index
                    .sources
                    .iter()
                    .chain(index.opinions_by_field.values().flatten().map(|op| &op.key));
                for key in all_keys {
                    let variables = node_variables(store, &index.graph, key.node);
                    if !checked.insert((key.layer_id, &key.spec_path, variables.clone())) {
                        continue;
                    }
                    if !spec_path_branches_selected(
                        store,
                        fallbacks,
                        stage_stack,
                        prims,
                        &mut selection_cache,
                        prim_path,
                        key.node,
                        key.layer_id,
                        &key.spec_path,
                        hosts,
                    ) {
                        rejected.insert((key.layer_id, key.spec_path.clone(), variables));
                    }
                }
            }
            if rejected.is_empty() {
                continue;
            }

            prims
                .get_mut(&prim_path)
                .expect("prim exists")
                .retain_keys(|graph, key| {
                    let variables = node_variables(store, graph, key.node);
                    !rejected.contains(&(key.layer_id, key.spec_path.clone(), variables))
                });
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
    fallbacks: &VariantFallbacks,
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
                    .or_insert_with(|| {
                        strength_ordered_variant_selections(store, fallbacks, &prims[&id])
                    })
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
                    .or_insert_with(|| {
                        strength_ordered_variant_selections(store, fallbacks, &prims[&host])
                    })
                    .get(&set)
                    .copied()
            })
            .or_else(|| {
                let authored = &store.layer(layer_id)?.prims.get(&host?)?.variant_selections;
                let context = SiteContext::Node(&prims[&prim_path].graph, node);
                site_selections(store, authored, context).get(&set).copied()
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
/// A relocate node moves a path at or beneath a relocation source of its
/// layer stack to the relocation's target, as the arcs that reach that
/// layer stack map it, and the nodes beneath it see `dest` at the
/// relocation source: at the depth of the source's site, relative to the
/// target's (AOUSD Core §10.3.2.6.1).
///
/// OpenUSD translates the host of a variant set toward the root this way to
/// find the strongest site that selects it
/// (`Pcp_TranslatePathFromNodeToRootOrClosestNode`, used by
/// `_ComposeVariantSelection` in `pxr/usd/pcp/primIndex.cpp`; the map
/// functions of the arcs reaching a relocating layer stack include its
/// relocations).
fn stage_host_path(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    dest: PathId,
    node: NodeId,
    host: crate::path::Path,
) -> Option<crate::path::Path> {
    let paths = store.paths();
    let site_depth = |node: &PrimNode| paths.resolve(node.site().prim_path()).depth();
    // The depth of `dest` as each node on the way to the root sees it,
    // nearest first.
    let mut chain = Vec::new();
    let mut at = Some(node);
    while let Some(id) = at {
        let node = graph.node(id)?;
        chain.push(node);
        at = node.parent();
    }
    let mut depths = alloc::vec![0; chain.len()];
    let mut depth = paths.resolve(dest).depth();
    for (at, node) in chain.iter().enumerate().rev() {
        depths[at] = depth;
        if node.arc_kind() == ArcKind::Relocates
            && let Some(parent) = chain.get(at + 1)
        {
            depth = (depth + site_depth(node)).saturating_sub(site_depth(parent));
        }
    }
    let mut host = host;
    for (at, pair) in chain.windows(2).enumerate() {
        let (cursor, parent) = (pair[0], pair[1]);
        if cursor.arc_kind() == ArcKind::Variants {
            continue;
        }
        if cursor.arc_kind() == ArcKind::Relocates {
            host = relocated_path(store, cursor.layer_stack(), host);
            continue;
        }
        let levels = depths[at].saturating_sub(usize::from(cursor.namespace_depth()));
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
    }
    Some(host)
}

/// `path`, in the namespace of the layer stack rooted at `layer_stack`,
/// moved by the relocation of that layer stack whose source is at or above
/// it: the path that relocation gives it, as the arcs that reach the layer
/// stack map it (AOUSD Core §10.3.2.6.1). A path no relocation moves, or
/// one a relocation removes, is kept.
fn relocated_path(
    store: &dyn LayerStore,
    layer_stack: LayerId,
    path: crate::path::Path,
) -> crate::path::Path {
    let stack = LayerStack::gather(store, layer_stack);
    let table = crate::relocates::RelocationTable::compute(store, &stack, &mut Vec::new());
    let paths = store.paths();
    table
        .iter()
        .find_map(|relocate| {
            let rel = path.strip_prefix(paths.resolve(relocate.source))?;
            Some(paths.resolve(relocate.target?).join(rel))
        })
        .unwrap_or(path)
}

/// Filters children maps by removing the children only unselected variant
/// branches author.
///
/// Population follows every branch, so a prim's child list holds the names
/// each branch of its variant sets, or of its parent's, authors. A name no
/// contributing spec of the prim authors is removed
/// ([`uncontributed_children`]); a name some contributing spec authors
/// stays, however many unselected branches author it too.
///
/// Spec: AOUSD Core §10.3.2.5 (only the selected variant contributes), §11
/// (population).
fn filter_variant_children(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    prims: &HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
) {
    use hashbrown::HashSet;

    let parent_paths: Vec<PathId> = children.keys().copied().collect();
    for parent_path in parent_paths {
        let Some(prim_index) = prims.get(&parent_path) else {
            continue;
        };

        // The children any branch of this prim's variant sets names, and
        // those of the branches its composed selections select.
        let mut all_variant_children: HashSet<TokenId> = HashSet::new();
        let mut selected_children: HashSet<TokenId> = HashSet::new();
        let mut variant_set_order: Vec<TokenId> = Vec::new();

        // First, resolve variant selections and variant set order from all opinion sources.
        let selections = composed_variant_selections(store, fallbacks, prim_index);
        for source in &prim_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) =
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            else {
                continue;
            };
            // Use the first non-empty variant set order we find: the prim
            // spec's sets, then those nested in its branches.
            variant_set_order = spec.selected_variant_set_order(&selections);
            if !variant_set_order.is_empty() {
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

            // A child of a variant set nested in other branches of this
            // prim also needs those branches selected.
            for branch in spec.variant_branches() {
                all_variant_children.extend(branch.spec.authored_children.iter().copied());
                if branch.is_selected(&selections) {
                    selected_children.extend(branch.spec.authored_children.iter().copied());
                }
            }
        }

        if all_variant_children.is_empty() {
            continue;
        }

        // The graph lacks the branches of selections made through the
        // sites ancestral arcs reach (`Cause::AncestralArcs` in
        // `composition_strict.rs`), so the children of every branch the
        // composed selections select stay as well.
        let mut unselected = uncontributed_children(store, prim_index, &all_variant_children);
        unselected.retain(|name| !selected_children.contains(name));

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
                for branch in spec.variant_branches() {
                    for child in &branch.spec.authored_children {
                        let entry = child_nesting_depth.entry(*child).or_insert(0);
                        *entry = (*entry).max(branch.depth());
                    }
                }
            }

            for set_tok in variant_set_order.iter().rev() {
                if !selections.contains_key(set_tok) {
                    continue;
                }
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
                    for branch in spec
                        .selected_variant_branches(&selections)
                        .filter(|branch| branch.set == *set_tok)
                    {
                        let arc_list_index = enclosing_arc(&prim_index.graph, source.node)
                            .map_or(0, PrimNode::sibling_index);
                        let group = arc_groups.entry(arc_list_index).or_default();
                        for child in &branch.spec.authored_children {
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
            let context = SiteContext::of_source(gp_index, source);
            for (set, variant) in site_selections(store, &spec.variant_selections, context).iter() {
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
                let context = SiteContext::of_source(gp_index, source);
                for branch in spec.selected_variant_branches(&gp_selections) {
                    let inner = site_selections(store, &branch.spec.variant_selections, context);
                    for (inner_set, inner_variant) in inner.iter() {
                        if !gp_selections.contains_key(inner_set) {
                            new_sels.entry(*inner_set).or_insert(*inner_variant);
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
                            composed_variant_selections(store, fallbacks, index)
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

        let Some(parent_index) = prims.get(&parent_path) else {
            continue;
        };
        let mut unselected_gc = uncontributed_children(store, parent_index, &all_gc);
        unselected_gc.retain(|name| !selected_gc.contains(name));
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
                                // source's filtered children → it was filtered out,
                                // unless a branch this prim selects through the
                                // class authors it (`/C{a=x}{b=y}Child`).
                                if !src_leaves.contains(&leaf)
                                    && prims.contains_key(&sc_id)
                                    && !prim_index
                                        .sources
                                        .iter()
                                        .any(|key| source_authors_child(store, key, leaf))
                                {
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

/// The names among `candidates` that no contributing spec of `index`
/// authors a child for.
///
/// A prim's children are the names its contributing specs author: the
/// specs outside any variant branch and those of the selected branches,
/// reached through any node of its graph. An unselected branch is not a
/// node of the graph, so a child it names exists only when a contributing
/// spec authors it as well. `index` must already be pruned of unselected
/// branches ([`prune_unselected_variant_specs`]).
///
/// Spec: AOUSD Core §10.3.2.5 (only the selected variant contributes), §11
/// (stage population). OpenUSD: `PcpPrimIndex::ComputePrimChildNames`,
/// which calls `PcpComposeSiteChildNames` (`pxr/usd/pcp/composeSite.cpp`)
/// for each node of the prim index.
fn uncontributed_children(
    store: &dyn LayerStore,
    index: &PrimIndex,
    candidates: &HashSet<TokenId>,
) -> HashSet<TokenId> {
    candidates
        .iter()
        .copied()
        .filter(|name| {
            !index
                .sources
                .iter()
                .any(|key| source_authors_child(store, key, *name))
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
                    let context = SiteContext::of_source(index, source);
                    let authored = site_selections(store, &spec.variant_selections, context);
                    for (set, variant) in authored.iter() {
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
                    for branch in spec.selected_variant_branches(&selections) {
                        vc.extend(branch.spec.authored_children.iter().copied());
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
            let context = SiteContext::of_source(index, source);
            for (set, variant) in site_selections(store, &spec.variant_selections, context).iter() {
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
            for branch in spec.selected_variant_branches(&selections) {
                surviving.extend(branch.spec.authored_children.iter().copied());
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
            for branch in spec.selected_variant_branches(&selections) {
                surviving.extend(branch.spec.authored_children.iter().copied());
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
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    path: PathId,
) -> HashMap<TokenId, TokenId> {
    let mut selections = authored_full_variant_selections(store, fallbacks, local_stack, path);
    apply_fallbacks_at(
        store,
        fallbacks,
        &mut selections,
        &PrimIndexGraph::default(),
        &[],
        &[(local_stack, path)],
    );
    selections
}

/// The selections of [`resolve_full_variant_selections`] before variant
/// fallbacks apply, for callers that add weaker selections first.
///
/// The sites that map to `path` are laid out in the strength order their
/// nodes take (AOUSD Core §10.4, LIVERPS): the prim's specs, then the
/// selections authored for it inside its parent's selected branch, each
/// inherit target with its branches, the prim's own branches, then each
/// reference and payload target with its branches. [`evaluate_variant_sets`]
/// resolves them as OpenUSD does, so a branch of the prim's own set that a
/// weaker reference selects still selects the sets evaluated after it.
fn authored_full_variant_selections(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    path: PathId,
) -> HashMap<TokenId, TokenId> {
    let local = selection_host_specs(store, fallbacks, local_stack, path);
    let child = resolve_variant_child_selections_for_prim(store, fallbacks, local_stack, path);
    let inherits = arcs_of(resolve_inherits_for_prim(
        store,
        fallbacks,
        local_stack,
        path,
        SelectionScope::Stack,
    ));

    // Reference and payload targets. Internal arcs target the whole stack
    // (AOUSD Core §10.3.2.1).
    let mut targets: Vec<(LayerStack, PathId)> = Vec::new();
    if let Some(&root) = local_stack.layers.first() {
        // The stack's variables, as its chain of arcs composes them,
        // evaluate its asset path expressions; composition reports what
        // they find when it follows the arcs.
        let chain = local_stack.chain_of(root).to_vec();
        let scope = ExpressionScope::new(chain.clone());
        let anchor = ArcAnchor::new(root, Some(&scope));
        let applied = |spec: &&crate::doc::PrimSpec| {
            spec_arcs_apply(
                store,
                fallbacks,
                local_stack,
                path,
                spec,
                SelectionScope::Stack,
            )
        };
        let arcs = |list: fn(&crate::doc::PrimSpec) -> &crate::listop::ListOp<Reference>| {
            let mut ops = NodeLists::new();
            for layer_id in &local_stack.layers {
                let Some(layer) = store.layer(*layer_id) else {
                    continue;
                };
                for spec in layer.prim_specs(path).filter(applied) {
                    let op = anchor_internal_arcs(store, list(spec), *layer_id, anchor);
                    ops.push(&spec.outer_variant_sites, op);
                }
            }
            arcs_of(ops.resolve())
        };
        let references = arcs(|spec| &spec.references);
        let payloads = arcs(|spec| &spec.payloads);
        for arc in references.iter().chain(&payloads) {
            let Some(target) = lookup_reference_target_path(store, arc) else {
                continue;
            };
            // The target stack as this stack's arcs reach it.
            let mut target_chain = chain.clone();
            target_chain.push(arc.layer);
            let stack = LayerStack::gather_recording(store, &target_chain, &mut Vec::new(), None);
            targets.push((stack, target));
        }
    }

    let mut sites: Vec<VariantSite<'_>> = local
        .iter()
        .map(|(spec, chain)| {
            VariantSite::of_spec(store, spec, Vec::new(), SiteContext::Chain(chain))
        })
        .collect();
    sites.push(VariantSite::authored_only(&child));
    for target in inherits.iter().copied() {
        let specs = selection_host_specs(store, fallbacks, local_stack, target);
        sites.extend(VariantSite::of_node(store, &specs));
    }
    sites.extend(VariantSite::branches_of_node(store, &local));
    for (stack, target) in &targets {
        let specs = selection_host_specs(store, fallbacks, stack, *target);
        sites.extend(VariantSite::of_node(store, &specs));
    }
    // Specializes targets are the weakest sites (AOUSD Core §10.4, the S in
    // LIVERPS); OpenUSD adds them before any variant set is evaluated, so
    // a class a prim specializes selects the prim's own sets.
    let specializes = arcs_of(resolve_specializes_for_prim(
        store,
        fallbacks,
        local_stack,
        path,
        SelectionScope::Stack,
    ));
    for target in specializes {
        let specs = selection_host_specs(store, fallbacks, local_stack, target);
        sites.extend(VariantSite::of_node(store, &specs));
    }
    evaluate_variant_sets(&sites)
}

/// Resolves the variant selections authored on `prim`'s specs inside the
/// selected branches of its parent (`/Parent{v=x}Prim (variants = ...)`).
///
/// Spec: AOUSD Core §7.3.6 (variant specs contain prim specs), §10.3.2.5.1
/// (computing variant selection).
fn resolve_variant_child_selections_for_prim(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    prim: PathId,
) -> HashMap<TokenId, TokenId> {
    let Some(parent) = store.paths().resolve(prim).parent() else {
        return HashMap::new();
    };
    let Some(parent_id) = store.paths().lookup(&parent) else {
        return HashMap::new();
    };

    let parent_selections =
        resolve_full_variant_selections(store, fallbacks, local_stack, parent_id);
    let mut selected = HashMap::new();
    for layer in local_stack.layers.iter().filter_map(|id| store.layer(*id)) {
        let context = SiteContext::Chain(local_stack.chain_of(layer.id));
        for spec in layer.selected_branch_prim_specs(prim, parent_id, &parent_selections) {
            // Branches hosted on the parent's ancestors must be selected too.
            let ancestors_selected = spec
                .outer_variant_sites
                .iter()
                .filter(|site| site.host_path != parent_id)
                .all(|site| {
                    resolve_full_variant_selections(store, fallbacks, local_stack, site.host_path)
                        .get(&site.set)
                        .is_none_or(|selected| *selected == site.variant)
                });
            if !ancestors_selected {
                continue;
            }
            let authored = site_selections(store, &spec.variant_selections, context);
            for (child_set, child_variant) in authored.iter() {
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
    fallbacks: &VariantFallbacks,
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
                // A host without variant sets in the arc's layer stack has
                // no branch to select, whatever the stronger sites select.
                Some(_) if !declares_variant_sets(store, remote_stack, host) => HashMap::new(),
                Some(dest_host) => {
                    // Every authored selection first, then the fallbacks.
                    let empty = PrimIndexGraph::default();
                    let (graph, sources) =
                        out.get(&dest_host).map_or((&empty, Vec::new()), |index| {
                            let mut sources = index.sources.clone();
                            index.graph.sort_keys(&mut sources);
                            (&index.graph, sources)
                        });
                    let mut selections =
                        authored_strength_ordered_variant_selections(store, graph, &sources);
                    let sites = [(stage_stack, dest_host), (remote_stack, host)];
                    for (stack, path) in sites {
                        for (set, variant) in
                            authored_full_variant_selections(store, fallbacks, stack, path)
                        {
                            selections.entry(set).or_insert(variant);
                        }
                    }
                    apply_fallbacks_at(store, fallbacks, &mut selections, graph, &sources, &sites);
                    selections
                }
                None => resolve_full_variant_selections(store, fallbacks, ancestor_stack, host),
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

/// Whether a spec for `path` in `stack`, in or outside a variant branch,
/// authors a variant set.
fn declares_variant_sets(store: &dyn LayerStore, stack: &LayerStack, path: PathId) -> bool {
    stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| layer.prim_specs(path))
        .any(|spec| !spec.variant_sets.is_empty())
}

/// The arcs authored for one prim of an arc's target namespace, admitted for
/// the variant selections in force at the composed destination.
///
/// This is the single place that decides which arcs nested inside another arc
/// are followed, whichever outer arc (reference, payload, inherit or
/// specialize) brought the content in.
///
/// Each arc comes with the variant branches of the prim that author it,
/// outermost first (see [`NodeLists`]); its node goes beneath theirs.
/// A reference or payload carries the offset of the layer that authors it
/// (see [`ArcAuthoring::authored_reference`]).
#[derive(Debug, Default)]
struct AdmittedArcs {
    inherits: Vec<(PathId, Vec<VariantSelectionSite>)>,
    specializes: Vec<(PathId, Vec<VariantSelectionSite>)>,
    references: Vec<(AuthoredReference, Vec<VariantSelectionSite>)>,
    payloads: Vec<(AuthoredReference, Vec<VariantSelectionSite>)>,
}

/// `arcs` without repeats: an arc of one node, read from two lists that
/// both read the node, is one arc (see [`NodeLists`]).
fn unique<T: PartialEq>(arcs: Vec<Sited<T>>) -> Vec<Sited<T>> {
    let mut kept: Vec<Sited<T>> = Vec::with_capacity(arcs.len());
    for arc in arcs {
        if !kept.contains(&arc) {
            kept.push(arc);
        }
    }
    kept
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
    fallbacks: &VariantFallbacks,
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
    // The root layer of `data_stack`, which internal arcs target.
    anchor: LayerId,
    // Its asset path expressions evaluate in the chain's scope.
    cycles: &mut CycleDetector,
) -> AdmittedArcs {
    let enclosing = enclosing_variant_selections(
        store,
        fallbacks,
        out,
        stage_stack,
        data_stack,
        ancestor_stack,
        arc_target,
        remote_path,
        dest_path,
        cache,
    );
    let scope = cycles.expression_scope();
    let arcs = arcs_admitted_by(
        store,
        fallbacks,
        data_stack,
        remote_path,
        &enclosing,
        ArcAnchor::new(anchor, Some(&scope)),
    );
    cycles.absorb(scope, dest_path);
    arcs
}

/// The arcs authored for `remote_path` in `data_stack`, admitted for the
/// selections `enclosing` names for the variant hosts enclosing it (see
/// [`admitted_arcs`]).
fn arcs_admitted_by(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    data_stack: &LayerStack,
    remote_path: PathId,
    enclosing: &HashMap<PathId, HashMap<TokenId, TokenId>>,
    anchor: ArcAnchor<'_>,
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

    let mut references = resolve_direct_references_for_prim(
        store,
        fallbacks,
        data_stack,
        remote_path,
        scope,
        anchor,
    );
    references.extend(resolve_variant_references_in(
        store,
        fallbacks,
        data_stack,
        remote_path,
        &selections,
        &parent_selections,
        scope,
        anchor,
    ));
    let mut payloads = resolve_payloads_for_prim_in(
        store,
        fallbacks,
        data_stack,
        remote_path,
        &parent_selections,
        scope,
        anchor,
    );
    payloads.extend(resolve_branch_payloads_in(
        store,
        fallbacks,
        data_stack,
        remote_path,
        &selections,
        scope,
        anchor,
    ));
    let inherits = resolve_inherits_for_prim_in(
        store,
        fallbacks,
        data_stack,
        remote_path,
        &selections,
        &parent_selections,
        scope,
    );
    let specializes = resolve_specializes_for_prim_in(
        store,
        fallbacks,
        data_stack,
        remote_path,
        &selections,
        &parent_selections,
        scope,
    );
    // The prim's specs in its parent's branches are read for both lists
    // of each kind; an arc of one node is one arc.
    references = unique(references);
    payloads = unique(payloads);
    let authoring = ArcAuthoring {
        store,
        stack: data_stack,
        prim: remote_path,
    };
    AdmittedArcs {
        inherits,
        specializes,
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
                authoring.authored_reference(
                    item,
                    |spec| &spec.payloads,
                    |b| &b.payloads,
                    anchor.payloads(),
                )
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
    fallbacks: &VariantFallbacks,
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
            fallbacks,
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
    fallbacks: &VariantFallbacks,
    stronger_stack: &LayerStack,
    selection_path: PathId,
    weaker_stack: &LayerStack,
    source_path: PathId,
) -> HashMap<TokenId, TokenId> {
    let mut selections =
        authored_full_variant_selections(store, fallbacks, stronger_stack, selection_path);
    for (set, variant) in
        authored_full_variant_selections(store, fallbacks, weaker_stack, source_path)
    {
        selections.entry(set).or_insert(variant);
    }
    apply_fallbacks_at(
        store,
        fallbacks,
        &mut selections,
        &PrimIndexGraph::default(),
        &[],
        &[
            (stronger_stack, selection_path),
            (weaker_stack, source_path),
        ],
    );
    selections
}

/// The variant selections for the variant sets of `remote_path`, the site
/// of the node `own` in the target layer stack `remote_stack`, from the
/// complete prim index of the composed prim `dest`, which must be ranked,
/// in a stage whose layer stack is `stage_stack`.
///
/// The sites are the index's sources, strongest first, with the site's own
/// variant branches ranked where their nodes go: beneath `own`, after its
/// specs and the classes it inherits, and before its other arcs (AOUSD
/// Core §10.4, LIVERPS). [`evaluate_variant_sets`] resolves them, so a
/// branch a weaker arc selects still selects the sets evaluated after it;
/// any other selection a source authors, then the fallbacks, fill in the
/// rest.
///
/// Spec: AOUSD Core §10.3.2.5 (the strongest selection in the prim index
/// wins). OpenUSD searches the whole index strongest first
/// (`_ComposeVariantSelection` in `pxr/usd/pcp/primIndex.cpp`), once every
/// arc of the prim is added.
fn late_variant_selections(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stage_stack: &LayerStack,
    out: &HashMap<PathId, PrimIndex>,
    dest: PathId,
    own: NodeId,
    remote_stack: &LayerStack,
    remote_path: PathId,
) -> HashMap<TokenId, TokenId> {
    let index = &out[&dest];
    let graph = &index.graph;
    // The specs inside an ancestor's unselected branch compose nothing
    // (see `prune_unselected_variant_specs`).
    let mut cache = HashMap::new();
    let mut sources: Vec<OpinionKey> = index
        .sources
        .iter()
        .filter(|key| {
            spec_path_branches_selected(
                store,
                fallbacks,
                stage_stack,
                out,
                &mut cache,
                dest,
                key.node,
                key.layer_id,
                &key.spec_path,
                BranchHosts::Ancestors,
            )
        })
        .cloned()
        .collect();
    graph.sort_keys(&mut sources);
    // Whether a source ranks ahead of the branches of `own`.
    let ahead = |node: NodeId| {
        let mut child = node;
        while let Some(parent) = graph.node(child).and_then(PrimNode::parent) {
            if parent == own {
                return graph
                    .node(child)
                    .is_some_and(|child| child.arc_kind() == ArcKind::Inherits);
            }
            child = parent;
        }
        node == own || graph.cmp_nodes(node, own).is_lt()
    };
    let at = sources
        .iter()
        .position(|key| !ahead(key.node))
        .unwrap_or(sources.len());
    let site_of = |key: &OpinionKey| {
        let spec = store.layer(key.layer_id).and_then(|layer| {
            layer.source_prim_spec(key.lookup_path, &key.spec_path, store.paths())
        })?;
        let context = SiteContext::Node(graph, key.node);
        Some(VariantSite::of_spec(
            store,
            spec,
            key.spec_path.variant_chain(),
            context,
        ))
    };
    // The site's own specs, read in the context of its node.
    let own_chain = node_chain(graph, own);
    let own_specs: Vec<HostSpec<'_, '_>> = remote_stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| layer.prim_specs(remote_path))
        .map(|spec| (spec, own_chain.as_slice()))
        .collect();
    let sites: Vec<VariantSite<'_>> = sources[..at]
        .iter()
        .filter_map(site_of)
        .chain(VariantSite::branches_of_node(store, &own_specs))
        .chain(sources[at..].iter().filter_map(site_of))
        .collect();
    let mut selections = evaluate_variant_sets(&sites);
    for (set, variant) in authored_composed_variant_selections(store, graph, &sources) {
        selections.entry(set).or_insert(variant);
    }
    apply_fallbacks_at(
        store,
        fallbacks,
        &mut selections,
        graph,
        &sources,
        &[(remote_stack, remote_path)],
    );
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
    store: &dyn LayerStore,
    out: &mut HashMap<PathId, PrimIndex>,
    path: PathId,
    sites: &[VariantSelectionSite],
) -> NodeId {
    let layer_stack = root_layer_stack(out, path);
    let paths = store.paths();
    let graph = &mut out.get_mut(&path).expect("path exists").graph;
    let mut node = NodeId::ROOT;
    let mut variants = Vec::new();
    // Local variants stay at this prim: unlike namespace arcs they never
    // intern a mapped path. Keep the store borrowed so callers can read
    // authored specs directly instead of cloning them to release a borrow.
    for site in sites {
        let host = paths.resolve(site.host_path);
        let hosted = paths.resolve(path).strip_prefix(host).is_some();
        debug_assert!(hosted, "a variant step is hosted at or above its site");
        if !hosted {
            continue;
        }
        let enclosing = enclosing_branches(&variants, *site);
        variants.push(*site);
        node = graph.intern_child(
            node,
            NodeArc {
                arc_kind: ArcKind::Variants,
                layer_stack,
                site: SpecPath::from_variant_selection_sites(path, &variants, paths),
                namespace_depth: u16::try_from(host.depth()).unwrap_or(u16::MAX),
                // The stage's layer stack is read with its own variables.
                sibling_index: declared_variant_set_index(store, &[layer_stack], *site, &enclosing),
                implied: false,
                skips_duplicates: false,
            },
        );
        graph.set_layer_offset(node, LayerOffset::IDENTITY);
    }
    node
}

/// The layer stack of the root node of `out[path]`'s graph: the stage's.
fn root_layer_stack(out: &HashMap<PathId, PrimIndex>, path: PathId) -> LayerId {
    out[&path]
        .graph
        .node(NodeId::ROOT)
        .expect("a prim graph has a root")
        .layer_stack()
}

/// Adds to the composed prim `dest`, beneath the relocate node `nodes`, the
/// specs `stack` authors at `source_view`, the relocation source `source`
/// extended towards `dest`, inside the selected variant branches of the
/// source's namespace ancestors.
///
/// A relocate node brings the ancestral opinions of its source, those of
/// the variant arcs of the source's ancestors among them; the source's own
/// specs, its own variant branches included, are ignored.
///
/// Spec: AOUSD Core §10.3.2.6 ("the composition algorithm is executed with
/// the layer stack and the entry's source path"). OpenUSD: the relocate arc
/// `_EvalNodeRelocations` adds includes the source's ancestral opinions
/// (`includeAncestralOpinions` in `pxr/usd/pcp/primIndex.cpp`), the variant
/// nodes of its ancestors among them. The arcs above the relocate node map
/// the target paths of those opinions (see [`TargetMap`]).
fn add_source_ancestral_variant_specs(
    store: &mut dyn LayerStore,
    stack: &LayerStack,
    nodes: &mut ArcNodes,
    out: &mut HashMap<PathId, PrimIndex>,
    dest: PathId,
    (source, source_view): (PathId, PathId),
    base_offset: LayerOffset,
    authored_children_out: &mut HashMap<PathId, ChildOrderOpinions>,
    prim_order_out: &mut HashMap<PathId, ChildOrderOpinions>,
    cycles: &mut CycleDetector,
) {
    let source_path = store.paths().resolve(source).clone();
    // A spec inside the variant branches of the source's proper ancestors.
    let ancestral = |paths: &crate::path::PathInterner, sites: &[VariantSelectionSite]| {
        !sites.is_empty()
            && sites.iter().all(|site| {
                let host = paths.resolve(site.host_path);
                host.is_prefix_of(&source_path) && *host != source_path
            })
    };
    // The node of each branch path those specs are authored in, interned
    // first so the specs are read in place.
    let mut branch_nodes: Vec<(Vec<VariantSelectionSite>, NodeId)> = Vec::new();
    for &layer_id in &stack.layers {
        let sites: Vec<Vec<VariantSelectionSite>> = store
            .layer(layer_id)
            .into_iter()
            .flat_map(|layer| layer.prim_specs(source_view))
            .filter(|spec| ancestral(store.paths(), &spec.outer_variant_sites))
            .map(|spec| spec.outer_variant_sites.to_vec())
            .collect();
        for sites in sites {
            if branch_nodes.iter().all(|(known, _)| *known != sites) {
                let node = nodes.spec_node(store, out, dest, &sites);
                branch_nodes.push((sites, node));
            }
        }
    }
    if branch_nodes.is_empty() {
        return;
    }
    for (layer_strength_idx, layer_id) in stack.layers.iter().copied().enumerate() {
        let Some(layer) = store.layer(layer_id) else {
            continue;
        };
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let layer_offset = base_offset.compose(stack.offset_at(layer_strength_idx));
        // Opinions whose target paths the arc maps once the layer is read.
        let mut pending: Vec<Opinion> = Vec::new();
        for spec in layer.prim_specs(source_view) {
            let Some(&(_, node)) = branch_nodes
                .iter()
                .find(|(sites, _)| **sites == *spec.outer_variant_sites)
            else {
                continue;
            };
            let key = OpinionKey {
                node,
                layer_strength,
                layer_id,
                lookup_path: source_view,
                spec_path: prim_spec_path(store, source_view, &spec.outer_variant_sites),
            };
            if !spec.authored_children.is_empty() {
                authored_children_out
                    .entry(dest)
                    .or_default()
                    .push((key.clone(), spec.authored_children.clone()));
            }
            if let Some(order) = &spec.prim_order {
                prim_order_out
                    .entry(dest)
                    .or_default()
                    .push((key.clone(), order.clone()));
            }
            let index = out.get_mut(&dest).expect("path exists");
            index.add_source(key.clone());
            for entry in composed_entries(&spec.fields, &spec.properties) {
                let key = key.clone().with_spec_path(property_spec_path(
                    store,
                    source_view,
                    &spec.outer_variant_sites,
                    entry.name(),
                ));
                let index = out.get_mut(&dest).expect("path exists");
                if let Some(property_type) = entry.property_type() {
                    index.add_property_type(entry.name(), key.clone(), property_type.clone());
                }
                pending.push(Opinion {
                    key,
                    field: entry.name(),
                    value: entry.value(),
                    layer_offset,
                });
            }
        }
        for mut opinion in pending {
            // The relocate node maps nothing itself; the arcs above it map
            // its opinions' targets.
            let targets = nodes.target_map(cycles.stage_layer_stack(), &[]);
            let source = store.paths().resolve(source_view).clone();
            // Errors name the nearest arc above that maps.
            let arc = nodes
                .path
                .iter()
                .rev()
                .map(|step| step.arc_kind)
                .find(|kind| !matches!(kind, ArcKind::Relocates | ArcKind::Variants))
                .unwrap_or(ArcKind::Relocates);
            map_arc_targets(
                store,
                &mut opinion.value,
                ArcPathMap {
                    arc,
                    source: &source,
                    map: &|store, path| targets.map(store, path),
                },
                TargetOwner {
                    prim: dest,
                    property: opinion.field,
                    layer: layer_id,
                    spec: opinion.key.spec_path.clone(),
                },
                cycles,
            );
            out.get_mut(&dest)
                .expect("path exists")
                .add_opinion(opinion);
        }
    }
}

/// Adds to each composed prim at or beneath the stage target of a
/// relocation of `relocates` whose source it also reaches the ancestral
/// variant opinions of that source (see
/// [`add_source_ancestral_variant_specs`]), read from `stack`, the gathered
/// relocating layer stack, beneath a relocate node under the arcs `parent`.
///
/// This serves the stage's own relocations (`parent` empty) and those of
/// the layer stack an arc reaches, lifted through it (`parent` the arc
/// path, see [`AncestralArcs::expand_relocation_sources`]); the relocate
/// node sits beneath the arc's node, so it reads its layers in that node's
/// expression context. A relocation whose source the arc does not map has
/// no stage source: its relocate node comes from [`AncestralArcs`].
fn add_relocated_variant_opinions(
    store: &mut dyn LayerStore,
    stack: &LayerStack,
    relocates: &LiftedSet,
    parent: &[ArcStep],
    stage_relocates: &Rc<LiftedSet>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, ChildOrderOpinions>,
    authored_children_out: &mut HashMap<PathId, ChildOrderOpinions>,
    cycles: &mut CycleDetector,
) {
    // Stage target → (relocating layer stack, source in its namespace).
    let by_target: HashMap<PathId, (LayerId, PathId)> = relocates
        .iter()
        .filter(|relocate| relocate.stage_source.is_some())
        .filter_map(|relocate| {
            Some((
                relocate.stage_target?,
                (relocate.layer_stack, relocate.source),
            ))
        })
        .collect();
    if by_target.is_empty() {
        return;
    }
    let (layer_offset, offset_layers) = parent.last().map_or_else(
        || (LayerOffset::default(), Rc::from([])),
        |step| (step.layer_offset, step.offset_layers.clone()),
    );
    let mut dests: Vec<PathId> = out.keys().copied().collect();
    dests.sort_unstable();
    for path in dests {
        // The nearest relocation target at or above `path`.
        let found = {
            let interner = store.paths();
            let mut cursor = Some(interner.resolve(path).clone());
            let mut found = None;
            while let Some(at) = cursor.filter(|at| at.depth() > 0) {
                if let Some(&relocate) = interner.lookup(&at).and_then(|id| by_target.get(&id)) {
                    let rel = interner
                        .resolve(path)
                        .strip_prefix(&at)
                        .expect("an ancestor prefixes its descendant")
                        .to_vec();
                    found = Some((relocate, at.depth(), rel));
                    break;
                }
                cursor = at.parent();
            }
            found
        };
        let Some(((layer_stack, source), target_depth, rel)) = found else {
            continue;
        };
        let namespace_depth = u16::try_from(target_depth).unwrap_or(u16::MAX);
        let joined = store.paths().resolve(source).join(&rel);
        let source_view = store.paths_mut().intern(joined);
        let step = ArcStep {
            arc_kind: ArcKind::Relocates,
            layer_stack,
            target: StepTarget::Namespace {
                dest_root: path,
                target_root: source_view,
            },
            namespace_depth,
            sibling_index: 0,
            implied: false,
            origin: None,
            layer_offset,
            offset_layers: offset_layers.clone(),
            skips_duplicates: false,
            relocates: None,
            ancestral: None,
            spooky: Rc::from([]),
        };
        let mut nodes = ArcNodes::new(ArcParent::nested(parent), step, Rc::clone(stage_relocates));
        add_source_ancestral_variant_specs(
            store,
            stack,
            &mut nodes,
            out,
            path,
            (source, source_view),
            layer_offset,
            authored_children_out,
            prim_order_out,
            cycles,
        );
    }
}

fn add_local_and_variant_opinions(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    mut deps: Option<&mut DependencyBuilder>,
) {
    for path in paths.iter().copied() {
        let selections = resolve_full_variant_selections(store, fallbacks, local_stack, path);

        for (layer_strength_idx, layer_id) in local_stack.layers.iter().copied().enumerate() {
            let Some(layer) = store.layer(layer_id) else {
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

                // Each selected branch is a variant spec of its own, those
                // of sets nested in a selected branch included
                // (`/P{a=x}{b=y}`), so each has its own node.
                //
                // Spec: AOUSD Core §7.3.6, §10.3.2.5.
                for branch in spec.selected_variant_branches(&selections) {
                    let variant_spec = branch.spec;
                    let branch_selections = branch.sites(&spec.outer_variant_sites, path);

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
    // Arc resolution evaluates asset path expressions
    // (`anchor_internal_arcs`); one read without an expression scope
    // targets nothing and is not an error.
    if reference.is_expression() {
        return None;
    }
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
    // The target layer stack as the arc reaches it, its sublayers evaluated
    // in that context.
    let mut has_spec = |store: &dyn LayerStore, path: PathId| {
        cycles
            .gather_layer_stack(store, reference.layer)
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
    fallbacks: &VariantFallbacks,
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
    let mut visited_inherits: VisitedClasses = VisitedClasses::new();
    let mut visited_specializes = VisitedClasses::new();
    for dest_root in paths.iter().copied() {
        cycles.begin(dest_root);
        // Internal arcs authored in the stage's layer stack target it, and
        // its variables evaluate their asset path expressions.
        let scope = cycles.expression_scope();
        let anchor = ArcAnchor::new(cycles.stage_layer_stack(), Some(&scope));
        // The prim's own branches follow the selections composed for it,
        // which its weaker arcs may author (`authored_full_variant_selections`).
        let selections = resolve_full_variant_selections(store, fallbacks, local_stack, dest_root);
        let refs = resolve_references_for_prim_selected(
            store,
            fallbacks,
            local_stack,
            dest_root,
            SelectionScope::Stack,
            anchor,
            &selections,
        );
        // Also resolve variant child references with full selection chaining.
        let variant_child_refs = resolve_variant_child_references(
            store,
            fallbacks,
            local_stack,
            local_stack,
            dest_root,
            anchor,
        );
        // Both lists read the prim's specs inside its parent's selected
        // branches; each reference of a node is one arc, listed once.
        let all_refs = unique(refs.into_iter().chain(variant_child_refs).collect());
        for (arc_list_index, reference) in all_refs.into_iter().enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            // An unresolved target is reported when the arc is followed.
            if let Some(d) = deps.as_deref_mut()
                && let Some(reference_path) = reference.0.target_path(store)
            {
                d.add_arc(ArcDependency {
                    source: reference_path,
                    target: dest_root,
                    arc_kind: ArcKind::References,
                    layer: reference.0.layer,
                });
            }
            let (reference, sites) = ArcAuthoring {
                store,
                stack: local_stack,
                prim: dest_root,
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
                fallbacks,
                local_stack,
                dest_root,
                reference,
                namespace_depth,
                arc_list_index,
                ArcParent::nested(&branch),
                out,
                &mut visited_inherits,
                &mut visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }
        cycles.absorb(scope, dest_root);
    }
}

fn add_inherit_opinions(
    store: &mut dyn LayerStore,
    fallbacks: &VariantFallbacks,
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
    for dest_root in paths.iter().copied() {
        cycles.begin(dest_root);
        let inherits = resolve_inherits_for_prim(
            store,
            fallbacks,
            local_stack,
            dest_root,
            SelectionScope::Stack,
        );
        for (arc_list_index, (inherited_root, sites)) in inherits.into_iter().enumerate() {
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
            let branch = local_variant_steps(root_layer_stack(out, dest_root), &sites);
            add_inherit_edge_opinions(
                store,
                fallbacks,
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
    /// For a namespace step, the relocations of its layer stack lifted
    /// through the arc into the stage namespace (see [`LiftedSet::lift`]);
    /// `None` when that layer stack relocates nothing the arc reaches.
    ///
    /// Spec: AOUSD Core §10.3.2.6.1.
    relocates: Option<Rc<LiftedSet>>,
    /// For an arc authored on an ancestor of another arc's target (see
    /// [`AncestralArcs`]), the arc as authored: the ancestor, in the
    /// namespace of the site authoring the arc, and the arc's authored
    /// target. The step reaches the authored target extended towards the
    /// other target, but maps the paths outside that extension, such as a
    /// class the target inherits, as the authored arc does.
    ancestral: Option<(PathId, PathId)>,
    /// For an implied class arc, the relocations lifted into the stage
    /// namespace of the weaker layer stacks it is implied out of (see
    /// [`Walk::with_spooky`]): the arc's opinions at their sources compose
    /// at their targets.
    ///
    /// Spec: AOUSD Core §10.3.2.6, §10.4.2.4.
    spooky: Rc<[Rc<LiftedSet>]>,
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
    ///
    /// `chain` is the context the step's layer stack is read in: the root
    /// layers of the layer stacks on the arcs to it, outermost first,
    /// ending with its own (see [`LayerStack::gather_recording`]).
    fn arc(
        &self,
        store: &mut dyn LayerStore,
        dest: PathId,
        cursor: &mut PathCursor,
        chain: &[LayerId],
    ) -> Option<NodeArc> {
        let mut sibling_index = self.sibling_index;
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
                let enclosing = enclosing_branches(&cursor.variants, site);
                cursor.variants.push(site);
                sibling_index = declared_variant_set_index(store, chain, site, &enclosing);
                let paths = store.paths();
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
            sibling_index,
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

/// The branches of `site`'s host among `outer`, the variant selections
/// leading to it, outermost first: those whose variant spec declares its
/// set (`/P{a=x}` for `/P{a=x}{b=y}`), as `(set, variant)`.
fn enclosing_branches(
    outer: &[VariantSelectionSite],
    site: VariantSelectionSite,
) -> Vec<(TokenId, TokenId)> {
    let mut chain: Vec<(TokenId, TokenId)> = outer
        .iter()
        .rev()
        .take_while(|enclosing| enclosing.host_path == site.host_path)
        .map(|enclosing| (enclosing.set, enclosing.variant))
        .collect();
    chain.reverse();
    chain
}

/// The position of `site`'s variant set among the variant sets declared
/// where it is authored, strongest layer first: by the host's prim spec for
/// a set of its own, or by the variant spec of the branches `enclosing`
/// (outermost first) for a set nested in them. This ranks the branches of
/// different sets at one site; sets declared nowhere follow every declared
/// one.
///
/// Each branch declares its nested sets on its own, so two branches that
/// declare the same sets in different orders rank them differently. The
/// layer stack is the one `chain` reaches (the root layers of the layer
/// stacks on the arcs to it, outermost first, ending with its own),
/// gathered with the expression variables composed along it, so a sublayer
/// that an overriding variable selects declares the order.
///
/// Spec: AOUSD Core §10.3.2.5 (variant sets are evaluated in the order of
/// the `variantSetNames` list op), §7.3.6 (variant specs may contain variant
/// set specs). OpenUSD: `PcpCompareSiblingNodeStrength` compares variant
/// siblings by `GetSiblingNumAtOrigin`, the set's index in
/// `PcpComposeSiteVariantSets` at the node the arc is added beneath
/// (`pxr/usd/pcp/strengthOrdering.cpp`, `_AddVariantArc` in
/// `pxr/usd/pcp/primIndex.cpp`); that node's layer stack is identified with
/// the source of its variables
/// (`PcpLayerStackIdentifier::expressionVariablesOverrideSource`).
fn declared_variant_set_index(
    store: &dyn LayerStore,
    chain: &[LayerId],
    site: VariantSelectionSite,
    enclosing: &[(TokenId, TokenId)],
) -> u16 {
    let mut declared: Vec<TokenId> = Vec::new();
    // Errors and variable reads of the gather are recorded where the node's
    // layer stack is gathered for its opinions, in the same context.
    let stack = LayerStack::gather_recording(store, chain, &mut Vec::new(), None);
    for layer in stack.layers.iter().filter_map(|id| store.layer(*id)) {
        for spec in layer.prim_specs(site.host_path) {
            let Some((_, order)) = spec.variant_sets_in(enclosing) else {
                continue;
            };
            for set in order {
                if !declared.contains(set) {
                    declared.push(*set);
                }
            }
        }
    }
    let index = declared
        .iter()
        .position(|set| *set == site.set)
        .unwrap_or(declared.len());
    u16::try_from(index).unwrap_or(u16::MAX)
}

/// Adds the nodes of `steps` beneath `cursor` in the graph of the composed
/// prim `dest`, moving `cursor` to the last one.
///
/// `stage_relocates` are the relocations of the stage's layer stack. Where
/// the relocations of the layer stacks above a step moved `dest` into the
/// namespace the step maps, the step's node sits beneath a relocate node
/// for each of them (see [`relocate_nodes`]).
fn intern_steps(
    store: &mut dyn LayerStore,
    out: &mut HashMap<PathId, PrimIndex>,
    dest: PathId,
    steps: &[ArcStep],
    cursor: &mut PathCursor,
    stage_relocates: &LiftedSet,
) {
    // `dest` as the steps so far see it: the relocations they took undone.
    let mut seen = dest;
    for (at, step) in steps.iter().enumerate() {
        let mut spooky_depth = None;
        let view = match step.target {
            StepTarget::Namespace { dest_root, .. } => {
                // A relocation an enclosing step took is taken once: an arc
                // authored in the relocation source's namespace sees the
                // source.
                let paths = store.paths();
                if !paths.resolve(dest_root).is_prefix_of(paths.resolve(seen)) {
                    seen = dest;
                }
                let outer = Walk::new(outer_relocates(stage_relocates, &steps[..at]), None);
                let view = relocate_nodes(store, out, dest, seen, dest_root, &outer, cursor);
                seen = view;
                // An implied class reaching `dest` through a relocation it
                // is implied across has the site of the relocation source:
                // OpenUSD implies it from the relocate node's class, in the
                // prim index of the relocation target, and adds it there
                // (`_EvalImpliedClassTree`).
                let spooky = Walk::new(step.spooky.iter().map(|set| &**set), None);
                let (taken, unmoved) = spooky.unwind(store, dest_root, view);
                match taken
                    .first()
                    .and_then(|(relocate, _)| relocate.stage_target)
                {
                    Some(target) => {
                        let depth = store.paths().resolve(target).depth();
                        spooky_depth = Some(u16::try_from(depth).unwrap_or(u16::MAX));
                        unmoved
                    }
                    None => view,
                }
            }
            _ => dest,
        };
        // A variant step ranks its set in its layer stack as read beneath
        // `cursor`: with the expression variables of the arcs reaching it.
        let chain = match step.target {
            StepTarget::Variant(_) | StepTarget::LocalVariant(_) => {
                let graph = &out[&dest].graph;
                let mut chain = node_chain(graph, cursor.node);
                if chain.last() != Some(&step.layer_stack) {
                    chain.push(step.layer_stack);
                }
                chain
            }
            StepTarget::Namespace { .. } => Vec::new(),
        };
        let Some(mut arc) = step.arc(store, view, cursor, &chain) else {
            continue;
        };
        if let Some(depth) = spooky_depth {
            arc.namespace_depth = depth;
        }
        let graph = &mut out.get_mut(&dest).expect("path exists").graph;
        cursor.node = graph.intern_child(cursor.node, arc);
        graph.set_layer_offset(cursor.node, step.layer_offset);
        if let Some(origin) = &step.origin {
            let node = cursor.node;
            if graph.node(node).and_then(PrimNode::origin).is_none() {
                let mut origin_cursor = PathCursor::root(dest);
                intern_steps(
                    store,
                    out,
                    dest,
                    origin,
                    &mut origin_cursor,
                    stage_relocates,
                );
                let graph = &mut out.get_mut(&dest).expect("path exists").graph;
                graph.set_origin(node, origin_cursor.node);
            }
        }
    }
}

/// Adds beneath `cursor`, in the graph of the composed prim `dest`, a
/// relocate node for each relocation in `outer` that moved `dest` into
/// the namespace an arc authored at the stage path `host` maps, and
/// returns `dest` as that arc sees it: the path it would have without
/// those relocations. `seen` is `dest` with the relocations the arcs
/// enclosing that arc took already undone.
///
/// A relocate node sits at the relocation source in the relocating layer
/// stack. It contributes no opinions: its source's own opinions are
/// ignored. The nodes beneath it are the source's ancestral opinions,
/// which compose at the relocation target.
///
/// Spec: AOUSD Core §10.3.2.6 ("the composition algorithm is executed with
/// the layer stack and the entry's source path to compute the opinions
/// from the relocation source"). OpenUSD adds a `PcpArcTypeRelocate` node
/// whose own specs do not contribute (`_EvalNodeRelocations` in
/// `pxr/usd/pcp/primIndex.cpp`).
fn relocate_nodes(
    store: &mut dyn LayerStore,
    out: &mut HashMap<PathId, PrimIndex>,
    dest: PathId,
    seen: PathId,
    host: PathId,
    outer: &Walk<'_>,
    cursor: &mut PathCursor,
) -> PathId {
    if outer.is_empty() {
        return seen;
    }
    let (taken, view) = outer.unwind(store, host, seen);
    for (relocate, site) in taken {
        let namespace_depth = relocate.stage_target.map_or(0, |target| {
            u16::try_from(store.paths().resolve(target).depth()).unwrap_or(u16::MAX)
        });
        let site = SpecPath::from_prim_path(site, store.paths());
        let graph = &mut out.get_mut(&dest).expect("path exists").graph;
        let arc = NodeArc {
            arc_kind: ArcKind::Relocates,
            layer_stack: relocate.layer_stack,
            site,
            namespace_depth,
            sibling_index: 0,
            implied: false,
            skips_duplicates: false,
        };
        let offset = graph.node(cursor.node).map(PrimNode::layer_offset);
        cursor.node = graph.intern_child(cursor.node, arc);
        // A relocation is read in the layer stack of the node authoring it.
        if let Some(offset) = offset {
            graph.set_layer_offset(cursor.node, offset);
        }
        cursor.variants.clear();
    }
    view
}

/// The relocations of the stage's layer stack and of the layer stacks the
/// arcs `steps` reach, lifted into the stage namespace, strongest first.
/// The authored target of `arc`, an arc of a target's ancestor retargeted
/// `depth` names beneath it (see [`AncestralArcs`]).
fn authored_target(store: &mut dyn LayerStore, arc: &Reference, depth: usize) -> Option<PathId> {
    let target = arc.target_path(store)?;
    let segments = store.paths().resolve(target).segments();
    let authored = crate::path::Path::root().join(&segments[..segments.len().checked_sub(depth)?]);
    Some(store.paths_mut().intern(authored))
}

fn outer_relocates<'a>(
    stage: &'a LiftedSet,
    steps: &'a [ArcStep],
) -> impl Iterator<Item = &'a LiftedSet> {
    core::iter::once(stage).chain(steps.iter().filter_map(|step| step.relocates.as_deref()))
}

/// The walk the opinions of an arc's target take into the stage namespace
/// (see [`Walk`]): `path` is the arc path to the arc, the arc last.
fn arc_walk<'a>(stage: &'a LiftedSet, path: &'a [ArcStep]) -> Walk<'a> {
    let walk = match path.split_last() {
        Some((own, outer)) => Walk::new(outer_relocates(stage, outer), own.relocates.as_deref()),
        None => Walk::new([stage], None),
    };
    walk.with_spooky(spooky_relocates(path))
}

/// The relocations the implied class arcs among `steps` are implied across
/// (see [`ArcStep::spooky`]).
fn spooky_relocates(steps: &[ArcStep]) -> impl Iterator<Item = &LiftedSet> {
    steps
        .iter()
        .flat_map(|step| step.spooky.iter().map(|set| &**set))
}

/// The relocations of the arc target's layer stack, rooted at
/// `layer_stack`, lifted through an arc from the stage path `dest_root` to
/// `target_root` authored at the site the arcs `parent` reach.
fn lift_arc_relocates(
    store: &mut dyn LayerStore,
    cycles: &mut CycleDetector,
    parent: &[ArcStep],
    layer_stack: LayerId,
    target_root: PathId,
    dest_root: PathId,
) -> Option<Rc<LiftedSet>> {
    let stage = cycles.relocations().stage();
    let outer = Walk::new(outer_relocates(&stage, parent), None);
    cycles.lift_relocations(store, layer_stack, target_root, dest_root, &outer)
}

/// Records that `dest`, which a relocation moved an arc's opinions to,
/// depends on the stage paths of the sites authoring that arc (`arc`) and
/// the arcs `above` it.
///
/// Those sites are not namespace ancestors of `dest`, so a population mask
/// that keeps `dest` must keep them too for the arcs to be expanded again
/// (see [`crate::LiveStage`]).
fn add_relocation_dependencies(
    deps: &mut DependencyBuilder,
    above: &[ArcStep],
    arc: (PathId, LayerId),
    dest: PathId,
) {
    let hosts = above.iter().filter_map(|step| match step.target {
        StepTarget::Namespace { dest_root, .. } => Some((dest_root, step.layer_stack)),
        _ => None,
    });
    for (source, layer) in hosts.chain([arc]) {
        if source != dest {
            deps.add_arc(ArcDependency {
                source,
                target: dest,
                arc_kind: ArcKind::Relocates,
                layer,
            });
        }
    }
}

/// Returns `true`, and records an [`ArcToProhibitedChild`], when an arc
/// from the composed prim `prim` targets `target` at or beneath a
/// relocation source of the layer stack rooted at `layer_stack`. The
/// caller must then skip the arc.
///
/// Spec: AOUSD Core §10.3.2.6. OpenUSD: `PcpErrorArcToProhibitedChild`.
fn targets_prohibited_child(
    store: &dyn LayerStore,
    cycles: &mut CycleDetector,
    prim: PathId,
    arc: ArcKind,
    layer_stack: LayerId,
    target: PathId,
) -> bool {
    let table = cycles.relocation_table(store, layer_stack);
    let Some(relocation_source) = table.source_at_or_above(store.paths(), target) else {
        return false;
    };
    cycles.report(CompositionError::ArcToProhibitedChild(
        ArcToProhibitedChild {
            prim,
            arc,
            layer_stack,
            target,
            relocation_source,
        },
    ));
    true
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
            relocates: None,
            ancestral: None,
            spooky: Rc::from([]),
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
    /// For an arc of a target's ancestor, the arc as authored (see
    /// [`ArcStep::ancestral`]).
    ancestral: Option<(PathId, PathId)>,
    /// For an implied class arc, the relocations it is implied across (see
    /// [`ArcStep::spooky`]).
    spooky: Rc<[Rc<LiftedSet>]>,
}

impl<'a> ArcParent<'a> {
    /// An arc authored at a site the arcs `steps` reach.
    fn nested(steps: &'a [ArcStep]) -> Self {
        Self {
            steps,
            implied: false,
            origin: None,
            skips_duplicates: false,
            ancestral: None,
            spooky: Rc::from([]),
        }
    }

    /// The same arc, authored on `ancestor` towards `target` (see
    /// [`ArcStep::ancestral`]).
    fn authored_on(self, ancestor: PathId, target: PathId) -> Self {
        Self {
            ancestral: Some((ancestor, target)),
            ..self
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
    /// node at the arc path `origin`, across the relocations `spooky` (see
    /// [`implied_classes`]).
    fn implied_from(
        steps: &'a [ArcStep],
        origin: Rc<[ArcStep]>,
        spooky: Rc<[Rc<LiftedSet>]>,
    ) -> Self {
        Self {
            steps,
            implied: true,
            origin: Some(origin),
            skips_duplicates: false,
            ancestral: None,
            spooky,
        }
    }

    /// The walk the opinions of the class arc `own`, authored at the site
    /// `steps` reach, take into the stage namespace (see [`arc_walk`]).
    fn class_walk<'b>(&'b self, stage: &'b LiftedSet, own: Option<&'b LiftedSet>) -> Walk<'b> {
        Walk::new(outer_relocates(stage, self.steps), own)
            .with_spooky(spooky_relocates(self.steps).chain(self.spooky.iter().map(|set| &**set)))
    }
}

/// A class arc implied into a stronger layer stack: the arc path of the
/// node it is implied beneath, and the implied arc.
struct ImpliedClass {
    parent: Vec<ArcStep>,
    step: ArcStep,
    /// The relocations of the arcs the class is implied across, which
    /// the implied arc's opinions pass (see [`ArcStep::spooky`]).
    spooky: Vec<Rc<LiftedSet>>,
    /// The arcs whose namespace mappings carry the class path into the
    /// implied arc's layer stack, innermost first.
    transfers: Vec<Transfer>,
}

/// One arc a class path is mapped across (see [`map_across`]).
#[derive(Clone)]
struct Transfer {
    /// The namespace mapping of the site that authors the arc (see
    /// [`outer_namespace`]).
    outer: Option<(PathId, PathId)>,
    arc_dest: PathId,
    arc_target: PathId,
    /// The relocations the arc maps the path through, and those above the
    /// site authoring it (see [`TransferRelocates`]).
    relocates: TransferRelocates,
    /// The arc as authored, for an arc of a target's ancestor (see
    /// [`ArcStep::ancestral`]).
    ancestral: Option<(PathId, PathId)>,
}

/// The relocations lifted into the stage namespace that a class path
/// mapped across an arc passes: `across`, those of the layer stacks above
/// the arc, which move the path as the arc maps it into the stage
/// namespace, and `above`, those above the site authoring the arc, which
/// the path is mapped back through into that site's namespace.
///
/// Spec: AOUSD Core §10.3.2.6.1 (relocates are part of an arc's namespace
/// mapping).
#[derive(Clone, Default)]
struct TransferRelocates {
    across: Vec<Rc<LiftedSet>>,
    above: Vec<Rc<LiftedSet>>,
}

impl TransferRelocates {
    /// The relocations for an arc authored at the site the arcs `parent`
    /// reach, in a stage whose layer stack relocates `stage`.
    fn new(stage: &Rc<LiftedSet>, parent: &[ArcStep]) -> Self {
        let sets = |steps: &[ArcStep]| -> Vec<Rc<LiftedSet>> {
            core::iter::once(Rc::clone(stage))
                .chain(steps.iter().filter_map(|step| step.relocates.clone()))
                .collect()
        };
        let outer_at = parent
            .iter()
            .rposition(|step| matches!(step.target, StepTarget::Namespace { .. }))
            .unwrap_or(0);
        Self {
            across: sets(parent),
            above: sets(&parent[..outer_at]),
        }
    }
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
/// stronger layer stack on the way to the root. The class path maps
/// through the relocations of the layer stacks it crosses
/// (`stage_relocates` and those of `steps`; see [`map_across`]).
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
    stage_relocates: &Rc<LiftedSet>,
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
    // A relocate node maps its source's namespace onto its parent's in the
    // same layer stack: a class beneath it is implied from its parent, as
    // OpenUSD implies past relocate nodes (`_EvalImpliedClassTree`).
    if step.arc_kind == ArcKind::Relocates {
        return implied_classes(
            store,
            stage_layer_stack,
            stage_relocates,
            &steps[..len - 1],
            class,
        );
    }
    if let Some(placeholder) = propagated_from(step) {
        let mut steps_at_placeholder = placeholder.to_vec();
        steps_at_placeholder.extend_from_slice(&steps[len..]);
        return implied_classes(
            store,
            stage_layer_stack,
            stage_relocates,
            &steps_at_placeholder,
            class,
        );
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
        let relocates = TransferRelocates::new(stage_relocates, parent);
        let mapped = map_across(
            store,
            outer,
            arc_dest,
            arc_target,
            &relocates,
            step.ancestral,
            path,
        );
        let level = parent
            .iter()
            .rev()
            .find(|step| matches!(step.target, StepTarget::Namespace { .. }));
        let layer_stack = level.map_or(stage_layer_stack, |level| level.layer_stack);
        if mapped == path && layer_stack == step.layer_stack {
            // The same site: OpenUSD adds a node that contributes no
            // opinions and only carries the class further up.
            implied = implied_classes(store, stage_layer_stack, stage_relocates, parent, class);
            for further in &mut implied {
                further.spooky.extend(step.relocates.clone());
            }
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
                spooky: step.relocates.iter().cloned().collect(),
                transfers: alloc::vec![Transfer {
                    outer,
                    arc_dest,
                    arc_target,
                    relocates,
                    ancestral: step.ancestral,
                }],
            });
        }
    }
    if class_based {
        for hierarchy in implied_classes(store, stage_layer_stack, stage_relocates, parent, step) {
            let mapped = hierarchy.transfers.iter().fold(path, |path, transfer| {
                map_across(
                    store,
                    transfer.outer,
                    transfer.arc_dest,
                    transfer.arc_target,
                    &transfer.relocates,
                    transfer.ancestral,
                    path,
                )
            });
            let ImpliedClass {
                parent,
                step: host,
                mut spooky,
                transfers,
            } = hierarchy;
            spooky.extend(step.relocates.clone());
            let implied_class = implied_step(class, &host, dest_root, mapped);
            implied.push(ImpliedClass {
                parent: nest_step(parent, host),
                step: implied_class,
                spooky,
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
        relocates: None,
        ancestral: None,
        spooky: Rc::from([]),
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
///
/// The arc maps `path` through the relocations of the layer stacks above
/// it (`stage_relocates` and those `parent` reaches), and the path lands in
/// the authoring site's namespace from before the relocations beneath that
/// site (AOUSD Core §10.3.2.6.1).
///
/// The arc maps `path` through the relocations of the layer stacks above
/// it (`relocates.across`), and the path lands in the authoring site's
/// namespace from before the relocations beneath that site
/// (`relocates.above`; AOUSD Core §10.3.2.6.1).
fn map_across(
    store: &mut dyn LayerStore,
    outer: Option<(PathId, PathId)>,
    arc_dest: PathId,
    arc_target: PathId,
    relocates: &TransferRelocates,
    ancestral: Option<(PathId, PathId)>,
    path: PathId,
) -> PathId {
    // An arc of a target's ancestor maps as authored, from its authored
    // target onto the ancestor, in the authoring site's namespace.
    if let Some((ancestor, authored)) = ancestral {
        let paths = store.paths();
        let Some(rel) = paths
            .resolve(path)
            .strip_prefix(paths.resolve(authored))
            .map(<[_]>::to_vec)
        else {
            return path;
        };
        let joined = paths.resolve(ancestor).join(&rel);
        return store.paths_mut().intern(joined);
    }
    let paths = store.paths();
    let Some(rel) = paths
        .resolve(path)
        .strip_prefix(paths.resolve(arc_target))
        .map(<[_]>::to_vec)
    else {
        return path;
    };
    // A class moved outside the namespace the arcs map keeps its path
    // beneath the destination.
    let stage_path = Walk::new(relocates.across.iter().map(|set| &**set), None)
        .map(store, arc_dest, &rel)
        .unwrap_or_else(|| {
            let joined = store.paths().resolve(arc_dest).join(&rel);
            store.paths_mut().intern(joined)
        });
    // Arc steps map from the composed prim's namespace; the outer mapping
    // maps into the namespace of the authoring site.
    match outer {
        Some((dest_root, target_root)) => {
            let above = Walk::new(relocates.above.iter().map(|set| &**set), None);
            let (_, view) = above.unwind(store, dest_root, stage_path);
            map_namespace(store, view, dest_root, target_root)
        }
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
    /// The relocations of the stage's layer stack.
    stage_relocates: Rc<LiftedSet>,
}

impl ArcNodes {
    /// The nodes of the arc `step` authored at a site `parent` reaches (see
    /// [`nest_step`]), in a stage whose layer stack relocates
    /// `stage_relocates`.
    fn new(parent: ArcParent<'_>, step: ArcStep, stage_relocates: Rc<LiftedSet>) -> Self {
        let skips_duplicates = parent.skips_duplicates
            || parent
                .steps
                .last()
                .is_some_and(|step| step.skips_duplicates);
        let authored = ArcStep {
            implied: parent.implied,
            origin: parent.origin,
            skips_duplicates,
            ancestral: parent.ancestral,
            spooky: parent.spooky,
            ..step
        };
        Self {
            path: nest_step(parent.steps.to_vec(), authored),
            nodes: HashMap::new(),
            stage_relocates,
        }
    }

    /// The arc path to this arc's selected branches `sites`, for arcs
    /// authored inside them (see [`Self::variant_node`]); to this arc for
    /// arcs authored outside every branch.
    /// How the paths this arc's opinions author map into the stage
    /// namespace (see [`TargetMap`]), in a stage whose layer stack is
    /// rooted at `stage_stack`; `relocated` as there.
    fn target_map<'a>(
        &'a self,
        stage_stack: LayerId,
        relocated: &'a [(PathId, PathId)],
    ) -> TargetMap<'a> {
        TargetMap {
            stage: &self.stage_relocates,
            stage_stack,
            steps: &self.path,
            relocated,
        }
    }

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
        intern_steps(
            store,
            out,
            dest,
            &self.path,
            &mut cursor,
            &self.stage_relocates,
        );
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
            relocates: None,
            ancestral: None,
            spooky: Rc::from([]),
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
        intern_steps(store, out, dest, &steps, &mut cursor, &self.stage_relocates);
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
///
/// A class site is the same only in the same expression variable context,
/// in which its layer stack gathers the same sublayers and its arcs
/// evaluate alike.
/// The sites a class arc's ancestral arcs do not add again (see
/// `AncestralArcs::used_sites`): a layer stack's root layer, a prim path,
/// and the expression variables the layer stack is read with.
type UsedSites = HashSet<(LayerId, PathId, ExpressionVariables)>;

/// The expression variables of the layer stack of the last of `steps`, a
/// prefix of an arc path from the composed prim, whose layer stack is
/// rooted at `root` (see `ArcChain::stacks_for`).
fn step_variables(store: &dyn LayerStore, root: LayerId, steps: &[ArcStep]) -> ExpressionVariables {
    let Some(last) = steps.last() else {
        return composed_variables(store, &[root]);
    };
    let mut chain: Vec<LayerId> = core::iter::once(root)
        .chain(steps.iter().map(|step| step.layer_stack))
        .collect();
    if let Some(end) = chain.iter().position(|stack| *stack == last.layer_stack) {
        chain.truncate(end + 1);
    }
    chain.dedup();
    composed_variables(store, &chain)
}

type VisitedClasses = HashSet<(PathId, LayerId, PathId, bool, ExpressionVariables)>;

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
#[derive(Clone, Copy)]
struct AncestralArcs<'a> {
    /// The stage's variant fallbacks.
    fallbacks: &'a VariantFallbacks,
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
    /// `true` for a class arc, whose ancestral arcs add no node for a site
    /// the prim index uses already (see [`Self::used_sites`]).
    class_arc: bool,
}

impl AncestralArcs<'_> {
    /// The sites of `nodes`' destination graph an ancestral arc of a class
    /// arc does not add again: every non-variant node's site but those of
    /// the nodes OpenUSD adds after the arc's node `own`, beneath classes
    /// implied from it (see [`PrimIndexGraph::implied_after`]), and for an
    /// implied or propagated class, the sites on the arc paths of the nodes
    /// it comes from, whose ancestral arcs lead back to them.
    ///
    /// OpenUSD adds class arcs with `skipDuplicateNodes`, which holds in the
    /// recursive index of the target's ancestors as well (`_AddArc` and
    /// `_AddClassBasedArc` in `pxr/usd/pcp/primIndex.cpp`). Classes are
    /// implied here before their origin's ancestral arcs are expanded, so
    /// the sites an implied class reached are left to
    /// [`drop_skipped_duplicates`], which keeps the origin's.
    fn used_sites(
        &self,
        store: &mut dyn LayerStore,
        nodes: &ArcNodes,
        own: NodeId,
        out: &HashMap<PathId, PrimIndex>,
    ) -> UsedSites {
        let graph = &out[&self.dest_root].graph;
        let mut used: UsedSites = graph
            .nodes()
            .filter(|(id, node)| {
                node.arc_kind() != ArcKind::Variants && !graph.implied_after(own, *id)
            })
            .map(|(id, node)| {
                (
                    node.layer_stack(),
                    node.site().prim_path(),
                    node_variables(store, graph, id),
                )
            })
            .collect();
        let root = root_layer_stack(out, self.dest_root);
        let mut pending: Vec<&[ArcStep]> = nodes
            .path
            .iter()
            .filter_map(|step| step.origin.as_deref())
            .collect();
        while let Some(path) = pending.pop() {
            for (index, step) in path.iter().enumerate() {
                if let StepTarget::Namespace {
                    dest_root,
                    target_root,
                } = step.target
                {
                    let site = map_namespace(store, self.dest_root, dest_root, target_root);
                    let variables = step_variables(store, root, &path[..=index]);
                    used.insert((step.layer_stack, site, variables));
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
        cycles: &mut CycleDetector,
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
        let scope = cycles.expression_scope();
        let arcs = arcs_admitted_by(
            store,
            self.fallbacks,
            self.data_stack,
            ancestor,
            &enclosing,
            ArcAnchor::new(self.arc_stack, Some(&scope)),
        );
        cycles.absorb(scope, self.dest_root);
        arcs
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
        // Every authored selection first, then the fallbacks.
        let empty = PrimIndexGraph::default();
        let (graph, sources) = self
            .stage_host(store, nodes, out, host)
            .and_then(|stage| out.get(&stage))
            .map_or((&empty, Vec::new()), |index| {
                let mut sources = index.sources.clone();
                index.graph.sort_keys(&mut sources);
                (&index.graph, sources)
            });
        let mut selections = authored_strength_ordered_variant_selections(store, graph, &sources);
        for (set, variant) in
            authored_full_variant_selections(store, self.fallbacks, self.ancestor_stack, host)
        {
            selections.entry(set).or_insert(variant);
        }
        apply_fallbacks_at(
            store,
            self.fallbacks,
            &mut selections,
            graph,
            &sources,
            &[(self.ancestor_stack, host)],
        );
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
        used: &UsedSites,
        kind: ArcKind,
        cycles: &mut CycleDetector,
        deps: Option<&mut DependencyBuilder>,
    ) -> Option<AuthoredReference> {
        let reference = &arc.reference;
        let path = resolve_arc_target(store, reference, self.dest_root, kind, cycles, deps)?;
        let joined = store.paths().resolve(path).join(rel);
        let path = store.paths_mut().intern(joined);
        let variables = composed_variables(store, &cycles.stacks_for(reference.layer));
        if used.contains(&(reference.layer, path, variables)) {
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
        visited_inherits: &mut VisitedClasses,
        visited_specializes: &mut VisitedClasses,
        prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        cycles: &mut CycleDetector,
        deps: Option<&mut DependencyBuilder>,
    ) {
        self.expand_from(
            store,
            nodes,
            out,
            visited_inherits,
            visited_specializes,
            prim_order_out,
            authored_children_out,
            cycles,
            deps,
            0,
        );
    }

    /// Adds, beneath relocate nodes, the ancestral opinions of the
    /// relocation sources of the target layer stack that the arc's walk
    /// does not bring to their targets.
    ///
    /// For a source inside the arc's target, the walk moves the opinions
    /// of the arcs above the source, but not the specs the relocating
    /// layer stack authors at the source inside its ancestors' variant
    /// branches: [`add_relocated_variant_opinions`] adds those, as for the
    /// stage's own relocations.
    ///
    /// For a source outside the arc's target whose relocation target lies
    /// inside, it expands the arcs of the source's ancestors (see
    /// [`Self::expand_from`]) for the prim the arc maps that target to. No
    /// path the arc maps reaches such a source, so no walk moves its
    /// ancestral opinions to the target: the relocate node brings them, as
    /// OpenUSD adds one wherever a node's site is a relocation target
    /// (`_EvalNodeRelocations` in `pxr/usd/pcp/primIndex.cpp`). The
    /// classes implied from beneath it stop at the arc, which cannot map
    /// the source (`_EvalImpliedRelocations`).
    ///
    /// Spec: AOUSD Core §10.3.2.6 ("the composition algorithm is executed
    /// with the layer stack and the entry's source path").
    fn expand_relocation_sources(
        &self,
        store: &mut dyn LayerStore,
        nodes: &ArcNodes,
        out: &mut HashMap<PathId, PrimIndex>,
        visited_inherits: &mut VisitedClasses,
        visited_specializes: &mut VisitedClasses,
        prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        cycles: &mut CycleDetector,
        mut deps: Option<&mut DependencyBuilder>,
    ) {
        let Some(own) = nodes.step().relocates.clone() else {
            return;
        };
        // The sources the arc maps: their ancestral opinions reach the
        // target through the arc, all but those of the variant branches
        // of the relocating layer stack, which the relocate node brings.
        add_relocated_variant_opinions(
            store,
            self.data_stack,
            &own,
            &nodes.path,
            &nodes.stage_relocates,
            out,
            prim_order_out,
            authored_children_out,
            cycles,
        );
        for relocate in own.iter() {
            let (Some(target), Some(dest)) = (relocate.target, relocate.stage_target) else {
                continue;
            };
            let paths = store.paths();
            if paths
                .resolve(self.target)
                .is_prefix_of(paths.resolve(relocate.source))
                || !out.contains_key(&dest)
            {
                continue;
            }
            AncestralArcs {
                dest_root: dest,
                target,
                ..*self
            }
            .expand(
                store,
                nodes,
                out,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }

    /// Expands the arcs of the ancestors of the target beneath `nodes`,
    /// past the `skip` nearest ones.
    ///
    /// At the deepest relocation target of the target layer stack at or
    /// above the target, the ancestral arcs above it give way to those of
    /// its relocation source: they are expanded beneath a relocate node at
    /// the source, for the source extended towards the target, past the
    /// source and its descendants, whose own arcs the relocation ignores.
    ///
    /// Spec: AOUSD Core §10.3.2.6 ("the composition algorithm is executed
    /// with the layer stack and the entry's source path"; "All
    /// previously-computed ancestral opinions except those due to ancestral
    /// variant arcs are removed"). OpenUSD: `_EvalNodeRelocations` in
    /// `pxr/usd/pcp/primIndex.cpp`, which adds the relocate node with
    /// `includeAncestralOpinions` and an inert source.
    fn expand_from(
        &self,
        store: &mut dyn LayerStore,
        nodes: &ArcNodes,
        out: &mut HashMap<PathId, PrimIndex>,
        visited_inherits: &mut VisitedClasses,
        visited_specializes: &mut VisitedClasses,
        prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        cycles: &mut CycleDetector,
        mut deps: Option<&mut DependencyBuilder>,
        skip: usize,
    ) {
        let used = if self.class_arc {
            // The arc's node, the origin of the classes implied from it.
            let mut cursor = PathCursor::root(self.dest_root);
            intern_steps(
                store,
                out,
                self.dest_root,
                &nodes.path,
                &mut cursor,
                &nodes.stage_relocates,
            );
            self.used_sites(store, nodes, cursor.node, out)
        } else {
            UsedSites::new()
        };
        // The context the ancestral arcs' classes are read in.
        let class_variables = composed_variables(store, &cycles.stacks_for(self.arc_stack));
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
        ancestors.drain(..skip.min(ancestors.len()));
        let relocated = self.relocation_at_or_above(store, cycles, &target_path, &ancestors, skip);
        if let Some((kept, _, _)) = relocated {
            ancestors.truncate(kept);
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
            // share depth 0. Beneath a relocate node, which composes the
            // relocation source's index in its own namespace, it is the
            // ancestor's depth there (`_EvalNodeRelocations`).
            let depth = if nodes.step().arc_kind == ArcKind::Relocates {
                ancestor_path.depth()
            } else {
                (dest_depth + ancestor_path.depth()).saturating_sub(target_path.depth())
            };
            let namespace_depth = u16::try_from(depth).unwrap_or(u16::MAX);
            let arcs = self.arcs_of(store, nodes, out, ancestor, cycles);
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
                let Some(authored) = authored_target(store, &reference.reference, rel.len()) else {
                    continue;
                };
                let branch = nodes.branch_path(&sites);
                add_reference_edge_opinions(
                    store,
                    self.fallbacks,
                    self.selection_stack,
                    self.dest_root,
                    reference,
                    namespace_depth,
                    u16::try_from(index).unwrap_or(u16::MAX),
                    self.parent(&branch).authored_on(ancestor, authored),
                    out,
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
                let Some(authored) = authored_target(store, &payload.reference, rel.len()) else {
                    continue;
                };
                let branch = nodes.branch_path(&sites);
                add_payload_edge_opinions(
                    store,
                    self.fallbacks,
                    self.selection_stack,
                    self.dest_root,
                    payload,
                    namespace_depth,
                    u16::try_from(index).unwrap_or(u16::MAX),
                    self.parent(&branch).authored_on(ancestor, authored),
                    out,
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
                let (authored, class) = (class, mapped(store, class));
                if used.contains(&(self.arc_stack, class, class_variables.clone())) {
                    continue;
                }
                let branch = nodes.branch_path(&sites);
                add_inherit_edge_opinions(
                    store,
                    self.fallbacks,
                    self.data_stack,
                    self.selection_stack,
                    self.dest_root,
                    class,
                    self.arc_stack,
                    namespace_depth,
                    u16::try_from(index).unwrap_or(u16::MAX),
                    self.parent(&branch).authored_on(ancestor, authored),
                    out,
                    visited_inherits,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    self.layer_offset,
                    cycles,
                    deps.as_deref_mut(),
                );
            }
            for (index, (specialized, sites)) in arcs.specializes.into_iter().enumerate() {
                let (authored, specialized) = (specialized, mapped(store, specialized));
                if used.contains(&(self.arc_stack, specialized, class_variables.clone())) {
                    continue;
                }
                let branch = nodes.branch_path(&sites);
                let index = u16::try_from(index).unwrap_or(u16::MAX);
                add_specializes_edge_opinions(
                    store,
                    self.fallbacks,
                    self.selection_stack,
                    self.dest_root,
                    self.dest_root,
                    specialized,
                    self.arc_stack,
                    namespace_depth,
                    index,
                    self.parent(&branch).authored_on(ancestor, authored),
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
        let Some((_, relocated_at, source)) = relocated else {
            return;
        };
        // The relocate node, at the source extended towards the target. It
        // contributes no opinions; the source's ancestral arcs nest in it.
        let rel = target_path
            .strip_prefix(store.paths().resolve(relocated_at))
            .expect("the relocation target is at or above the target")
            .to_vec();
        let joined = store.paths().resolve(source).join(&rel);
        let source_view = store.paths_mut().intern(joined);
        let arc = nodes.step();
        let step = ArcStep {
            arc_kind: ArcKind::Relocates,
            layer_stack: self.arc_stack,
            target: StepTarget::Namespace {
                dest_root: self.dest_root,
                target_root: source_view,
            },
            namespace_depth: u16::try_from(dest_depth).unwrap_or(u16::MAX),
            sibling_index: 0,
            implied: false,
            origin: None,
            layer_offset: arc.layer_offset,
            offset_layers: arc.offset_layers.clone(),
            skips_duplicates: arc.skips_duplicates,
            relocates: None,
            ancestral: None,
            spooky: Rc::from([]),
        };
        let mut relocate_nodes = ArcNodes::new(
            ArcParent::nested(&nodes.path),
            step,
            Rc::clone(&nodes.stage_relocates),
        );
        // The destination and the prims beneath it, each reading the
        // source extended as far.
        let mut dests: Vec<(PathId, PathId)> = alloc::vec![(self.dest_root, source_view)];
        {
            let dest_path = store.paths().resolve(self.dest_root).clone();
            let mut beneath: Vec<(PathId, Vec<TokenId>)> = out
                .keys()
                .filter_map(|&prim| {
                    let rel = store.paths().resolve(prim).strip_prefix(&dest_path)?;
                    (!rel.is_empty()).then(|| (prim, rel.to_vec()))
                })
                .collect();
            beneath.sort_unstable();
            for (prim, rel) in beneath {
                let joined = store.paths().resolve(source_view).join(&rel);
                dests.push((prim, store.paths_mut().intern(joined)));
            }
        }
        for (dest, view) in dests {
            add_source_ancestral_variant_specs(
                store,
                self.data_stack,
                &mut relocate_nodes,
                out,
                dest,
                (source, view),
                self.layer_offset,
                authored_children_out,
                prim_order_out,
                cycles,
            );
        }
        AncestralArcs {
            target: source_view,
            ..*self
        }
        .expand_from(
            store,
            &relocate_nodes,
            out,
            visited_inherits,
            visited_specializes,
            prim_order_out,
            authored_children_out,
            cycles,
            deps,
            rel.len(),
        );
    }

    /// The deepest relocation target of the target layer stack among the
    /// target (unless `skip` passes it) and its `ancestors`, nearest first:
    /// how many of `ancestors` are at or below it, its path and its
    /// relocation source.
    fn relocation_at_or_above(
        &self,
        store: &dyn LayerStore,
        cycles: &mut CycleDetector,
        target: &crate::path::Path,
        ancestors: &[crate::path::Path],
        skip: usize,
    ) -> Option<(usize, PathId, PathId)> {
        let table = cycles.relocation_table(store, self.arc_stack);
        if table.is_empty() {
            return None;
        }
        let paths = store.paths();
        let found = |path: &crate::path::Path| {
            let id = paths.lookup(path)?;
            Some((id, table.source_of(id)?))
        };
        if skip == 0
            && let Some((at, source)) = found(target)
        {
            return Some((0, at, source));
        }
        ancestors
            .iter()
            .enumerate()
            .find_map(|(index, path)| found(path).map(|(at, source)| (index + 1, at, source)))
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
///
/// A site is the same only in the same expression variable context: a
/// layer stack reached with other variables is another layer stack
/// (`PcpLayerStackIdentifier::expressionVariablesOverrideSource`), so its
/// class sites are other sites ([`same_context`]).
fn retain_new_class_sites(
    store: &dyn LayerStore,
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
            if known.layer_id != key.layer_id
                || known.spec_path != key.spec_path
                || !same_context(store, graph, known.node, key.node)
            {
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
    fallbacks: &VariantFallbacks,
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
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
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
    if targets_prohibited_child(
        store,
        cycles,
        dest_root,
        ArcKind::Inherits,
        arc_stack,
        inherited_root,
    ) {
        return;
    }
    // One expansion per class site and layer stack, authored or implied.
    let layers_read = local_stack.layers.first().copied().unwrap_or(arc_stack);
    let variables = composed_variables(store, &cycles.stacks_for(layers_read));
    if !visited.insert((
        dest_root,
        layers_read,
        inherited_root,
        parent.implied,
        variables,
    )) {
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
        relocates: None,
        ancestral: None,
        spooky: Rc::from([]),
    };
    let stage_relocates = cycles.relocations().stage();
    let implied = implied_classes(
        store,
        cycles.stage_layer_stack(),
        &stage_relocates,
        parent.steps,
        &step,
    );
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
            fallbacks,
            &implied_stack,
            selection_stack,
            dest_root,
            implied_root,
            implied.step.layer_stack,
            namespace_depth,
            arc_list_index,
            ArcParent::implied_from(
                &implied.parent,
                origin.clone(),
                parent
                    .spooky
                    .iter()
                    .cloned()
                    .chain(implied.spooky)
                    .collect(),
            ),
            out,
            visited,
            visited_specializes,
            prim_order_out,
            authored_children_out,
            None,
            implied.step.layer_offset,
            cycles,
            deps.as_deref_mut(),
        );
    }

    cycles.enter(arc_stack, inherited_root, dest_root, ArcKind::Inherits);
    // The relocations of the class's layer stack apply to the namespace the
    // arc maps (AOUSD Core §10.3.2.6.1).
    let step = ArcStep {
        relocates: lift_arc_relocates(
            store,
            cycles,
            parent.steps,
            arc_stack,
            inherited_root,
            dest_root,
        ),
        ..step
    };
    let stage_relocates = cycles.relocations().stage();

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
    let walk = parent.class_walk(&stage_relocates, step.relocates.as_deref());
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&inherited_path) else {
                continue;
            };
            rel.to_vec()
        };
        let Some((dest_path_id, moved)) = walk.place(store, dest_root, &rel) else {
            continue;
        };
        if moved && let Some(d) = deps.as_deref_mut() {
            add_relocation_dependencies(d, parent.steps, (dest_root, arc_stack), dest_path_id);
        }
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }
    drop(walk);

    let mut host_selection_cache = HashMap::new();
    // Branch-only source prims whose branch is not selected for the
    // destination take no part in this arc.
    let unselected = {
        let pairs: Vec<(PathId, PathId)> = mapping.clone();
        unselected_branch_prims(
            store,
            fallbacks,
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

    let mut nodes = ArcNodes::new(parent, step, stage_relocates);
    record_offset_layers(deps.as_deref_mut(), &nodes.step().offset_layers, &mapping);
    let class_relocated: Vec<(PathId, PathId)> = cycles
        .relocation_table(store, arc_stack)
        .iter()
        .filter_map(|relocate| Some((relocate.target?, relocate.source)))
        .collect();

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
                        fallbacks,
                        selection_stack,
                        *dest_path_id,
                        local_stack,
                        *remote_path_id,
                    );
                    for branch in spec.selected_variant_branches(&inherits_selections) {
                        let variant_spec = branch.spec;
                        let branch_selections =
                            branch.sites(&spec.outer_variant_sites, *remote_path_id);
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

        let redundant = retain_new_class_sites(store, out, &mut pending_sources);
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
            let targets = nodes.target_map(cycles.stage_layer_stack(), &class_relocated);
            map_arc_targets(
                store,
                &mut value,
                ArcPathMap {
                    arc: ArcKind::Inherits,
                    source: &inherited_path,
                    map: &|store, path| targets.map(store, path),
                },
                TargetOwner {
                    prim: dest_path_id,
                    property: field,
                    layer: layer_id,
                    spec: spec_path.clone(),
                },
                cycles,
            );
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
            fallbacks,
            out,
            selection_stack,
            local_stack,
            selection_stack,
            inherited_root,
            remote_path_id,
            dest_path_id,
            &mut host_selection_cache,
            arc_stack,
            cycles,
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
                fallbacks,
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
                prim_order_out,
                authored_children_out,
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
                fallbacks,
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
                fallbacks,
                selection_stack,
                dest_path_id,
                nested_ref,
                namespace_depth,
                ref_index,
                ArcParent::nested(&branch),
                out,
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
                fallbacks,
                selection_stack,
                dest_path_id,
                nested_payload,
                namespace_depth,
                payload_index,
                ArcParent::nested(&branch),
                out,
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
        fallbacks,
        data_stack: local_stack,
        selection_stack,
        ancestor_stack: selection_stack,
        arc_stack,
        dest_root,
        target: inherited_root,
        layer_offset: base_offset,
        class_arc: true,
    }
    .expand(
        store,
        &nodes,
        out,
        visited,
        visited_specializes,
        prim_order_out,
        authored_children_out,
        cycles,
        deps,
    );

    cycles.exit();
}

/// The map function of the arc at the end of `steps`, composed with those
/// of the arcs above it: how a path its specs author maps into the stage
/// namespace, once, as they are added.
///
/// Each arc maps its target, and the paths beneath it, onto its
/// destination, through the relocations of the layer stacks on the way
/// (see [`Walk::map`]): the destination is a stage path, so the path is
/// mapped. A class arc, or an internal reference or payload, maps every
/// other path to itself, in the namespace of the arc authoring it, where
/// the next arc up maps it in turn; any other arc maps nothing else.
/// Variant branches and relocate nodes map nothing themselves. A path the
/// stage's own layer stack keeps is a stage path.
///
/// An arc that maps a path to itself does not map one at or beneath its
/// destination, in the namespace the arc is authored in: that path would
/// not map back (OpenUSD's bijection check); nor, for the innermost arc,
/// one at or beneath a relocation target of its layer stack whose source
/// lies beneath the destination (`relocated`). An implied class maps its
/// paths to themselves unchecked, its destination being in another layer
/// stack.
///
/// Spec: AOUSD Core §10.3.2 (arcs map namespaces), §10.3.2.6.1 (relocates
/// add to the mapping), §12.4 (target paths map through the arcs of their
/// opinions). OpenUSD: `PcpNodeRef::GetMapToRoot`, built from each arc's
/// `_CreateMapExpressionForArc` with `AddRootIdentity` for internal and
/// class arcs (`pxr/usd/pcp/primIndex.cpp`, `pxr/usd/pcp/mapExpression.cpp`).
struct TargetMap<'a> {
    /// The relocations of the stage's layer stack.
    stage: &'a LiftedSet,
    /// Root layer of the stage's layer stack.
    stage_stack: LayerId,
    /// The arc path to the arc, the arc last.
    steps: &'a [ArcStep],
    /// The relocations of the innermost arc's layer stack, as `(target,
    /// source)` pairs.
    relocated: &'a [(PathId, PathId)],
}

impl TargetMap<'_> {
    /// Maps `path`, authored in the namespace of the innermost arc's
    /// target, into the stage namespace; `None` when the arcs map nothing
    /// there.
    fn map(&self, store: &mut dyn LayerStore, path: PathId) -> Option<PathId> {
        self.map_through(store, self.steps, path)
    }

    /// Maps `path` through the arcs `steps`, innermost last (see
    /// [`Self::map`]).
    fn map_through(
        &self,
        store: &mut dyn LayerStore,
        steps: &[ArcStep],
        mut path: PathId,
    ) -> Option<PathId> {
        let within = |store: &dyn LayerStore, path: PathId, root: PathId| {
            let paths = store.paths();
            paths
                .resolve(path)
                .strip_prefix(paths.resolve(root))
                .map(<[_]>::to_vec)
        };
        let mut innermost = true;
        for (at, step) in steps.iter().enumerate().rev() {
            // A specializes node propagated to the root maps as its
            // placeholder does, beneath the arcs that author it.
            if let Some(placeholder) = propagated_from(step) {
                return self.map_through(store, placeholder, path);
            }
            let StepTarget::Namespace {
                dest_root,
                target_root,
            } = step.target
            else {
                continue;
            };
            if step.arc_kind == ArcKind::Relocates {
                continue;
            }
            if let Some(rel) = within(store, path, target_root) {
                return arc_walk(self.stage, &steps[..=at]).map(store, dest_root, &rel);
            }
            // The arc authored on an ancestor of another arc's target maps
            // as authored, onto that ancestor (see `ArcStep::ancestral`).
            if let Some((ancestor, authored)) = step.ancestral
                && let Some(rel) = within(store, path, authored)
            {
                let joined = store.paths().resolve(ancestor).join(&rel);
                path = store.paths_mut().intern(joined);
                innermost = false;
                continue;
            }
            // The arc that authors this one, and its namespace.
            let above = steps[..at].iter().rev().find_map(|step| match step.target {
                StepTarget::Namespace {
                    dest_root,
                    target_root,
                } if step.arc_kind != ArcKind::Relocates => {
                    Some((step.layer_stack, dest_root, target_root))
                }
                _ => None,
            });
            let authoring_stack = above.map_or(self.stage_stack, |(stack, _, _)| stack);
            let identity = matches!(step.arc_kind, ArcKind::Inherits | ArcKind::Specializes)
                || step.layer_stack == authoring_stack;
            if !identity {
                return None;
            }
            if !step.implied {
                // The destination, in the namespace the arc is authored in.
                let dest = match (step.ancestral, above) {
                    (Some((ancestor, _)), _) => Some(ancestor),
                    (None, Some((_, above_dest, above_target))) => {
                        within(store, dest_root, above_dest).map(|rel| {
                            let joined = store.paths().resolve(above_target).join(&rel);
                            store.paths_mut().intern(joined)
                        })
                    }
                    (None, None) => Some(dest_root),
                };
                if let Some(dest) = dest {
                    let paths = store.paths();
                    let (resolved, dest) = (paths.resolve(path), paths.resolve(dest));
                    let moved = innermost
                        && self.relocated.iter().any(|&(target, source)| {
                            paths.resolve(target).is_prefix_of(resolved)
                                && dest.is_prefix_of(paths.resolve(source))
                        });
                    if dest.is_prefix_of(resolved) || moved {
                        return None;
                    }
                }
            }
            innermost = false;
        }
        Some(path)
    }
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

/// The opinions an arc will copy into its stage, detached from the source
/// store before namespace mapping mutates the path interner. Snapshot only
/// mapped specs, and clone each opinion once into its eventual stage owner.
struct ArcSpecSnapshot {
    outer_variant_sites: Vec<VariantSelectionSite>,
    entries: Vec<(TokenId, OpinionValue, Option<PropertyType>)>,
    prim_order: Option<Vec<TokenId>>,
    authored_children: Vec<TokenId>,
    has_variant_sets: bool,
}

fn snapshot_arc_specs(
    layer: &crate::doc::Layer,
    mapping: &[(PathId, PathId)],
) -> Vec<(PathId, PathId, ArcSpecSnapshot)> {
    mapping
        .iter()
        .flat_map(|&(source, dest)| {
            layer.prim_specs(source).map(move |spec| {
                let snapshot = ArcSpecSnapshot {
                    outer_variant_sites: spec.outer_variant_sites.clone(),
                    entries: composed_entries(&spec.fields, &spec.properties)
                        .map(|entry| (entry.name(), entry.value(), entry.property_type().cloned()))
                        .collect(),
                    prim_order: spec.prim_order.clone(),
                    authored_children: spec.authored_children.clone(),
                    has_variant_sets: !spec.variant_sets.is_empty(),
                };
                (source, dest, snapshot)
            })
        })
        .collect()
}

fn add_reference_edge_opinions(
    store: &mut dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stage_stack: &LayerStack,
    dest_root: PathId,
    arc: AuthoredReference,
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
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
    if targets_prohibited_child(
        store,
        cycles,
        dest_root,
        ArcKind::References,
        reference.layer,
        reference_path,
    ) {
        return;
    }
    let target_specs = TargetSpecsCheck::begin(
        store,
        out,
        &reference,
        dest_root,
        ArcKind::References,
        reference_path,
        namespace_depth,
    );
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
    let combined_stack = stage_stack.joined(&remote_stack);
    let target_root = store.paths().resolve(reference_path).clone();
    let offset_layers = parent.offset_layers_within(arc.layer);
    // The relocations of the target's layer stack apply to the namespace
    // the arc maps (AOUSD Core §10.3.2.6.1).
    let relocates = lift_arc_relocates(
        store,
        cycles,
        parent.steps,
        reference.layer,
        reference_path,
        dest_root,
    );
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
            relocates,
            ancestral: None,
            spooky: Rc::from([]),
        },
        cycles.relocations().stage(),
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
    let walk = arc_walk(&nodes.stage_relocates, &nodes.path);
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&target_root) else {
                continue;
            };
            rel.to_vec()
        };
        let Some((dest_path_id, moved)) = walk.place(store, dest_root, &rel) else {
            continue;
        };
        if moved && let Some(d) = deps.as_deref_mut() {
            let above = &nodes.path[..nodes.path.len() - 1];
            add_relocation_dependencies(d, above, (dest_root, reference.layer), dest_path_id);
        }
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
    let mut late_sites = Vec::new();
    let mut late_seen = HashSet::new();
    for (layer_strength_idx, remote_layer_id) in remote_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let ref_offset = reference
            .layer_offset
            .compose(remote_stack.offset_at(layer_strength_idx));
        let Some(remote_layer) = store.layer(remote_layer_id) else {
            continue;
        };

        let snapshots = snapshot_arc_specs(remote_layer, &mapping);

        let mut pending_sources = Vec::new();
        let mut pending_fields: Vec<(
            PathId,
            TokenId,
            OpinionKey,
            OpinionValue,
            Option<PropertyType>,
            LayerOffset,
        )> = Vec::new();
        for (remote_path_id, dest_path_id, remote_spec) in snapshots {
            if let Some(d) = deps.as_deref_mut() {
                d.add_layer_opinion(remote_layer_id, dest_path_id);
            }
            let node = nodes.spec_node(store, out, dest_path_id, &remote_spec.outer_variant_sites);
            let base_key = OpinionKey {
                node,
                layer_strength,
                layer_id: remote_layer_id,
                lookup_path: remote_path_id,
                spec_path: normalized_prim_spec_path(
                    store,
                    remote_path_id,
                    &remote_spec.outer_variant_sites,
                    provenance_remap,
                ),
            };
            pending_sources.push((dest_path_id, base_key.clone()));

            for (field, value, property_type) in remote_spec.entries {
                pending_fields.push((
                    dest_path_id,
                    field,
                    base_key
                        .clone()
                        .with_spec_path(normalized_property_spec_path(
                            store,
                            remote_path_id,
                            &remote_spec.outer_variant_sites,
                            field,
                            provenance_remap,
                        )),
                    value,
                    property_type,
                    ref_offset,
                ));
            }

            if let Some(order) = remote_spec.prim_order {
                prim_order_out.entry(dest_path_id).or_default().push((
                    OpinionKey {
                        node,
                        layer_strength,
                        layer_id: remote_layer_id,
                        lookup_path: remote_path_id,
                        spec_path: prim_spec_path(
                            store,
                            remote_path_id,
                            &remote_spec.outer_variant_sites,
                        ),
                    },
                    order,
                ));
            }

            if !remote_spec.authored_children.is_empty() {
                authored_children_out
                    .entry(dest_path_id)
                    .or_default()
                    .push((
                        OpinionKey {
                            node,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: remote_path_id,
                            spec_path: prim_spec_path(
                                store,
                                remote_path_id,
                                &remote_spec.outer_variant_sites,
                            ),
                        },
                        remote_spec.authored_children,
                    ));
            }

            // The spec's own variant sets are selected once the prim
            // index is complete (see `LateBranches`).
            if remote_spec.has_variant_sets && late_seen.insert((remote_path_id, dest_path_id)) {
                late_sites.push((remote_path_id, dest_path_id));
            }
        }

        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }
        let targets = nodes.target_map(cycles.stage_layer_stack(), &[]);
        for (dest_path_id, field, key, value, property_type, offset) in pending_fields {
            let mut value = value;
            map_arc_targets(
                store,
                &mut value,
                ArcPathMap {
                    arc: ArcKind::References,
                    source: &target_root,
                    map: &|store, path| targets.map(store, path),
                },
                TargetOwner {
                    prim: dest_path_id,
                    property: field,
                    layer: key.layer_id,
                    spec: key.spec_path.clone(),
                },
                cycles,
            );
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

    let nested = NestedArcs {
        remote_stack: &remote_stack,
        combined_stack: &combined_stack,
        anchor: reference.layer,
        layer_offset: reference.layer_offset,
    };
    for &(remote_path_id, dest_path_id) in &mapping {
        let arcs = admitted_arcs(
            store,
            fallbacks,
            out,
            stage_stack,
            &remote_stack,
            &remote_stack,
            reference_path,
            remote_path_id,
            dest_path_id,
            &mut host_selection_cache,
            reference.layer,
            cycles,
        );
        // The arcs of the prim's own branches follow their selection (see
        // `LateBranches`).
        nested.expand(
            store,
            fallbacks,
            &nodes,
            remote_path_id,
            dest_path_id,
            arcs,
            |sites| !in_own_branch(sites, remote_path_id),
            out,
            visited_inherits,
            visited_specializes,
            prim_order_out,
            authored_children_out,
            cycles,
            deps.as_deref_mut(),
        );
    }

    // The arcs the target's ancestors author, and those of the relocation
    // sources outside the target (see `AncestralArcs`).
    let ancestral = AncestralArcs {
        fallbacks,
        data_stack: &remote_stack,
        selection_stack: &combined_stack,
        ancestor_stack: &remote_stack,
        arc_stack: reference.layer,
        dest_root,
        target: reference_path,
        layer_offset: reference.layer_offset,
        class_arc: false,
    };
    ancestral.expand(
        store,
        &nodes,
        out,
        visited_inherits,
        visited_specializes,
        prim_order_out,
        authored_children_out,
        cycles,
        deps.as_deref_mut(),
    );
    ancestral.expand_relocation_sources(
        store,
        &nodes,
        out,
        visited_inherits,
        visited_specializes,
        prim_order_out,
        authored_children_out,
        cycles,
        deps,
    );

    if !late_sites.is_empty() {
        LateArc {
            arc: ArcKind::References,
            path: nodes.path.clone(),
            stage_relocates: Rc::clone(&nodes.stage_relocates),
            stage_stack: stage_stack.clone(),
            combined_stack: combined_stack.clone(),
            remote_stack: remote_stack.clone(),
            anchor: reference.layer,
            layer_offset: reference.layer_offset,
            target_root: reference_path,
            provenance_remap,
            chain: cycles.chain_state(),
        }
        .defer(cycles, late_sites);
    }
    if let Some(check) = target_specs {
        check.finish(out, cycles);
    }
    cycles.exit();
}

/// Whether an arc authored inside the variant branches `sites` (outermost
/// first) is authored inside a branch of `host`'s own variant sets.
fn in_own_branch(sites: &[VariantSelectionSite], host: PathId) -> bool {
    sites.iter().any(|site| site.host_path == host)
}

/// The arcs of `arcs` whose variant branches `keep` accepts, each with its
/// index among `arcs`.
fn kept<T>(
    arcs: Vec<(T, Vec<VariantSelectionSite>)>,
    keep: &impl Fn(&[VariantSelectionSite]) -> bool,
) -> Vec<(u16, T, Vec<VariantSelectionSite>)> {
    arcs.into_iter()
        .enumerate()
        .filter(|(_, (_, sites))| keep(sites))
        .map(|(index, (arc, sites))| (u16::try_from(index).unwrap_or(u16::MAX), arc, sites))
        .collect()
}

/// The arcs authored for the prims of a reference or payload target's
/// namespace, expanded beneath the arc's nodes (see [`Self::expand`]).
struct NestedArcs<'a> {
    /// The target layer stack, which authors the arcs.
    remote_stack: &'a LayerStack,
    /// The layers that select the variants of the arcs' targets: those of
    /// the stronger layer stacks, then `remote_stack`'s.
    combined_stack: &'a LayerStack,
    /// Root layer of the target layer stack; internal arcs target it.
    anchor: LayerId,
    /// The offset of the arc, applied to the class arcs nested in it.
    layer_offset: LayerOffset,
}

impl NestedArcs<'_> {
    /// Expands `arcs`, those authored for `remote_path` of the target
    /// namespace, beneath `nodes` for the composed prim `dest` it maps onto:
    /// each arc `keep` accepts by the variant branches authoring it. Each
    /// keeps its place among the arcs of its kind, the arcs `keep` rejects
    /// included.
    ///
    /// Spec: AOUSD Core §10.4 (an arc's target ranks beneath the site that
    /// authors it). OpenUSD: `_EvalRefOrPayloadArcs` and
    /// `_AddClassBasedArcs` in `pxr/usd/pcp/primIndex.cpp`.
    fn expand(
        &self,
        store: &mut dyn LayerStore,
        fallbacks: &VariantFallbacks,
        nodes: &ArcNodes,
        remote_path: PathId,
        dest: PathId,
        arcs: AdmittedArcs,
        keep: impl Fn(&[VariantSelectionSite]) -> bool,
        out: &mut HashMap<PathId, PrimIndex>,
        visited_inherits: &mut VisitedClasses,
        visited_specializes: &mut VisitedClasses,
        prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        cycles: &mut CycleDetector,
        mut deps: Option<&mut DependencyBuilder>,
    ) {
        let namespace_depth =
            u16::try_from(store.paths().resolve(dest).depth()).unwrap_or(u16::MAX);
        for (index, class, sites) in kept(arcs.inherits, &keep) {
            let branch = nodes.branch_path(&sites);
            // The class is implied into each stronger layer stack by its own
            // expansion (see `implied_classes`).
            add_inherit_edge_opinions(
                store,
                fallbacks,
                self.remote_stack,
                self.combined_stack,
                dest,
                class,
                self.anchor,
                namespace_depth,
                index,
                ArcParent::nested(&branch),
                out,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                self.layer_offset,
                cycles,
                deps.as_deref_mut(),
            );
        }
        // Direct references, references on the prim's selected branches, and
        // references authored for it inside its parent's selected branches.
        for (index, reference, sites) in kept(arcs.references, &keep) {
            let branch = nodes.branch_path(&sites);
            add_reference_edge_opinions(
                store,
                fallbacks,
                self.combined_stack,
                dest,
                reference,
                namespace_depth,
                index,
                ArcParent::nested(&branch),
                out,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }
        for (index, payload, sites) in kept(arcs.payloads, &keep) {
            let branch = nodes.branch_path(&sites);
            add_payload_edge_opinions(
                store,
                fallbacks,
                self.combined_stack,
                dest,
                payload,
                namespace_depth,
                index,
                ArcParent::nested(&branch),
                out,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }
        // Specializes authored in the target's namespace. Their opinions are
        // weaker than every other opinion of the prim, not only than this
        // arc's: each leaves a placeholder beneath this arc's node and is
        // propagated to the root, and is implied into each stronger layer
        // stack (see `nest_step` and `implied_classes`).
        //
        // Spec: AOUSD Core §10.4.1, §10.4.2.4; OpenUSD
        // `_EvalImpliedSpecializes` in `pxr/usd/pcp/primIndex.cpp`.
        for (index, specialized, sites) in kept(arcs.specializes, &keep) {
            let branch = nodes.branch_path(&sites);
            add_specializes_edge_opinions(
                store,
                fallbacks,
                self.combined_stack,
                dest,
                remote_path,
                specialized,
                self.anchor,
                namespace_depth,
                index,
                ArcParent::nested(&branch),
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

/// A reference or payload whose target sites' own variant sets are left
/// for the late variant pass (see [`LateBranches`]).
#[derive(Debug)]
struct LateArc {
    /// References or payloads.
    arc: ArcKind,
    /// The arc path to the arc, the arc last.
    path: Vec<ArcStep>,
    /// The relocations of the stage's layer stack.
    stage_relocates: Rc<LiftedSet>,
    /// The layers of the stronger layer stacks, which select the variants
    /// of the hosts enclosing the target's sites (see
    /// [`enclosing_variant_selections`]).
    stage_stack: LayerStack,
    /// `stage_stack`'s layers, then `remote_stack`'s.
    combined_stack: LayerStack,
    /// The arc's target layer stack.
    remote_stack: LayerStack,
    /// Root layer of the target layer stack; internal arcs target it.
    anchor: LayerId,
    /// The arc's offset.
    layer_offset: LayerOffset,
    /// The arc's target.
    target_root: PathId,
    /// How spec paths are recorded (see [`normalized_prim_spec_path`]).
    provenance_remap: Option<(PathId, PathId)>,
    /// The chain of arcs to the arc, the arc included.
    chain: ChainState,
}

/// The variant sets a site of an arc's target namespace declares, for the
/// composed prim it maps onto, selected once the prim index holds every
/// other arc.
///
/// An arc expansion maps its target's specs onto each composed prim, but
/// leaves the branches of their own variant sets, with the arcs authored
/// inside them, to [`add_late_variant_branches`]: a stronger site that
/// selects them may still be missing, such as a class implied across the
/// arc, or a site an arc authored deeper in namespace reaches.
///
/// Spec: AOUSD Core §10.3.2.5 (the strongest selection in the prim index
/// wins). OpenUSD evaluates a node's variant sets after the prim index's
/// other arcs and implied classes (`EvalNodeVariantSets` in `Task::Type`,
/// `_EvalNodeVariantSets` in `pxr/usd/pcp/primIndex.cpp`), searching the
/// whole index for each selection (`_ComposeVariantSelection`).
#[derive(Debug)]
pub(crate) struct LateBranches {
    arc: Rc<LateArc>,
    /// The site, in the target layer stack.
    remote_path: PathId,
    /// The composed prim the site maps onto.
    dest: PathId,
}

impl LateArc {
    /// Leaves the variant sets of each `(remote path, composed prim)` of
    /// `sites` for the late variant pass.
    fn defer(self, cycles: &mut CycleDetector, sites: Vec<(PathId, PathId)>) {
        let arc = Rc::new(self);
        for (remote_path, dest) in sites {
            cycles.defer_branches(LateBranches {
                arc: Rc::clone(&arc),
                remote_path,
                dest,
            });
        }
    }
}

impl LateBranches {
    /// Adds the branches the composed prim's index selects, with the arcs
    /// authored inside them, beneath the arc's node.
    fn add(
        &self,
        store: &mut dyn LayerStore,
        fallbacks: &VariantFallbacks,
        stage_stack: &LayerStack,
        out: &mut HashMap<PathId, PrimIndex>,
        visited_inherits: &mut VisitedClasses,
        visited_specializes: &mut VisitedClasses,
        prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
        cycles: &mut CycleDetector,
        deps: Option<&mut DependencyBuilder>,
    ) {
        let (arc, remote_path, dest) = (&*self.arc, self.remote_path, self.dest);
        if !out.contains_key(&dest) {
            return;
        }
        cycles.resume(arc.chain.clone());
        let mut nodes = ArcNodes {
            path: arc.path.clone(),
            nodes: HashMap::new(),
            stage_relocates: Rc::clone(&arc.stage_relocates),
        };
        let own = nodes.cursor(store, out, dest).node;
        out.get_mut(&dest).expect("path exists").graph.rank();
        let selections = late_variant_selections(
            store,
            fallbacks,
            stage_stack,
            out,
            dest,
            own,
            &arc.remote_stack,
            remote_path,
        );
        let target_root = store.paths().resolve(arc.target_root).clone();
        for (at, layer_id) in arc.remote_stack.layers.iter().copied().enumerate() {
            let layer_strength = u16::try_from(at).unwrap_or(u16::MAX);
            let layer_offset = arc.layer_offset.compose(arc.remote_stack.offset_at(at));
            let specs: Vec<crate::doc::PrimSpec> = store
                .layer(layer_id)
                .map(|layer| layer.prim_specs(remote_path).cloned().collect())
                .unwrap_or_default();
            for spec in &specs {
                for branch in spec.selected_variant_branches(&selections) {
                    let sites = branch.sites(&spec.outer_variant_sites, remote_path);
                    let node = nodes.variant_node(store, out, dest, &sites);
                    let key = OpinionKey {
                        node,
                        layer_strength,
                        layer_id,
                        lookup_path: remote_path,
                        spec_path: normalized_variant_spec_path(
                            store,
                            remote_path,
                            &sites,
                            arc.provenance_remap,
                        ),
                    };
                    out.get_mut(&dest)
                        .expect("path exists")
                        .add_source(key.clone());
                    for entry in composed_entries(&branch.spec.fields, &branch.spec.properties) {
                        let key =
                            key.clone()
                                .with_spec_path(normalized_variant_property_spec_path(
                                    store,
                                    remote_path,
                                    &sites,
                                    entry.name(),
                                    arc.provenance_remap,
                                ));
                        let mut value = entry.value();
                        let targets = nodes.target_map(cycles.stage_layer_stack(), &[]);
                        map_arc_targets(
                            store,
                            &mut value,
                            ArcPathMap {
                                arc: arc.arc,
                                source: &target_root,
                                map: &|store, path| targets.map(store, path),
                            },
                            TargetOwner {
                                prim: dest,
                                property: entry.name(),
                                layer: layer_id,
                                spec: key.spec_path.clone(),
                            },
                            cycles,
                        );
                        let index = out.get_mut(&dest).expect("path exists");
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
                            value,
                            layer_offset,
                        });
                    }
                }
            }
        }

        // The arcs authored inside the selected branches.
        let mut cache = HashMap::new();
        let mut enclosing = enclosing_variant_selections(
            store,
            fallbacks,
            out,
            &arc.stage_stack,
            &arc.remote_stack,
            &arc.remote_stack,
            arc.target_root,
            remote_path,
            dest,
            &mut cache,
        );
        enclosing.insert(remote_path, selections);
        let scope = cycles.expression_scope();
        let arcs = arcs_admitted_by(
            store,
            fallbacks,
            &arc.remote_stack,
            remote_path,
            &enclosing,
            ArcAnchor::new(arc.anchor, Some(&scope)),
        );
        cycles.absorb(scope, dest);
        NestedArcs {
            remote_stack: &arc.remote_stack,
            combined_stack: &arc.combined_stack,
            anchor: arc.anchor,
            layer_offset: arc.layer_offset,
        }
        .expand(
            store,
            fallbacks,
            &nodes,
            remote_path,
            dest,
            arcs,
            |sites| in_own_branch(sites, remote_path),
            out,
            visited_inherits,
            visited_specializes,
            prim_order_out,
            authored_children_out,
            cycles,
            deps,
        );
    }
}

/// Adds the variant branches left for late evaluation (see
/// [`LateBranches`]), with the arcs authored inside them, until the arcs
/// those add leave none.
///
/// Spec: AOUSD Core §10.3.2.5. OpenUSD processes the variant tasks of a
/// prim index after its arc tasks, and those of the nodes a variant arc
/// adds as they come (`Pcp_PrimIndexer` in `pxr/usd/pcp/primIndex.cpp`).
fn add_late_variant_branches(
    store: &mut dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stage_stack: &LayerStack,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    cycles: &mut CycleDetector,
    mut deps: Option<&mut DependencyBuilder>,
) {
    let mut visited_inherits = VisitedClasses::new();
    let mut visited_specializes = VisitedClasses::new();
    loop {
        let late = cycles.take_late_branches();
        if late.is_empty() {
            break;
        }
        for branches in late {
            branches.add(
                store,
                fallbacks,
                stage_stack,
                out,
                &mut visited_inherits,
                &mut visited_specializes,
                prim_order_out,
                authored_children_out,
                cycles,
                deps.as_deref_mut(),
            );
        }
    }
}

fn add_payload_opinions(
    store: &mut dyn LayerStore,
    fallbacks: &VariantFallbacks,
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
    let mut visited_inherits: VisitedClasses = VisitedClasses::new();
    let mut visited_specializes = VisitedClasses::new();
    for dest_root in paths.iter().copied() {
        cycles.begin(dest_root);
        // Internal arcs authored in the stage's layer stack target it, and
        // its variables evaluate their asset path expressions.
        let scope = cycles.expression_scope();
        let anchor = ArcAnchor::new(cycles.stage_layer_stack(), Some(&scope)).payloads();
        let payloads = resolve_payloads_for_prim(
            store,
            fallbacks,
            local_stack,
            dest_root,
            SelectionScope::Stack,
            anchor,
        );
        // Also resolve variant branch-level payloads, for the selections
        // composed for the prim (`authored_full_variant_selections`).
        let selections = resolve_full_variant_selections(store, fallbacks, local_stack, dest_root);
        let branch_payloads = resolve_variant_branch_payloads(
            store,
            fallbacks,
            local_stack,
            dest_root,
            anchor,
            &selections,
        );
        let all_payloads = unique(payloads.into_iter().chain(branch_payloads).collect());
        for (arc_list_index, payload) in all_payloads.into_iter().enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            // An unresolved target is reported when the arc is followed.
            if let Some(d) = deps.as_deref_mut()
                && let Some(payload_path) = payload.0.target_path(store)
            {
                d.add_arc(ArcDependency {
                    source: payload_path,
                    target: dest_root,
                    arc_kind: ArcKind::Payloads,
                    layer: payload.0.layer,
                });
            }
            let (payload, sites) = ArcAuthoring {
                store,
                stack: local_stack,
                prim: dest_root,
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
                fallbacks,
                local_stack,
                dest_root,
                payload,
                namespace_depth,
                arc_list_index,
                ArcParent::nested(&branch),
                out,
                &mut visited_inherits,
                &mut visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                cycles,
                deps.as_deref_mut(),
            );
        }
        cycles.absorb(scope, dest_root);
    }
}

fn add_payload_edge_opinions(
    store: &mut dyn LayerStore,
    fallbacks: &VariantFallbacks,
    stage_stack: &LayerStack,
    dest_root: PathId,
    arc: AuthoredReference,
    namespace_depth: u16,
    arc_list_index: u16,
    // The arcs this arc is authored inside, and whether it is implied.
    parent: ArcParent<'_>,
    out: &mut HashMap<PathId, PrimIndex>,
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
    if targets_prohibited_child(
        store,
        cycles,
        dest_root,
        ArcKind::Payloads,
        reference.layer,
        reference_path,
    ) {
        return;
    }
    let target_specs = TargetSpecsCheck::begin(
        store,
        out,
        &reference,
        dest_root,
        ArcKind::Payloads,
        reference_path,
        namespace_depth,
    );
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
    let combined_stack = stage_stack.joined(&remote_stack);
    let target_root = store.paths().resolve(reference_path).clone();
    let offset_layers = parent.offset_layers_within(arc.layer);
    // The relocations of the target's layer stack apply to the namespace
    // the arc maps (AOUSD Core §10.3.2.6.1).
    let relocates = lift_arc_relocates(
        store,
        cycles,
        parent.steps,
        reference.layer,
        reference_path,
        dest_root,
    );
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
            relocates,
            ancestral: None,
            spooky: Rc::from([]),
        },
        cycles.relocations().stage(),
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
    let walk = arc_walk(&nodes.stage_relocates, &nodes.path);
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&target_root) else {
                continue;
            };
            rel.to_vec()
        };
        let Some((dest_path_id, moved)) = walk.place(store, dest_root, &rel) else {
            continue;
        };
        if moved && let Some(d) = deps.as_deref_mut() {
            let above = &nodes.path[..nodes.path.len() - 1];
            add_relocation_dependencies(d, above, (dest_root, reference.layer), dest_path_id);
        }
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }
    record_offset_layers(deps.as_deref_mut(), &nodes.step().offset_layers, &mapping);

    let mut host_selection_cache = HashMap::new();
    let mut late_sites = Vec::new();
    let mut late_seen = HashSet::new();
    for (layer_strength_idx, remote_layer_id) in remote_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let payload_offset = reference
            .layer_offset
            .compose(remote_stack.offset_at(layer_strength_idx));
        let Some(remote_layer) = store.layer(remote_layer_id) else {
            continue;
        };

        let snapshots = snapshot_arc_specs(remote_layer, &mapping);

        let mut pending_sources = Vec::new();
        for (remote_path_id, dest_path_id, remote_spec) in snapshots {
            if let Some(d) = deps.as_deref_mut() {
                d.add_layer_opinion(remote_layer_id, dest_path_id);
            }
            let node = nodes.spec_node(store, out, dest_path_id, &remote_spec.outer_variant_sites);
            pending_sources.push((
                dest_path_id,
                OpinionKey {
                    node,
                    layer_strength,
                    layer_id: remote_layer_id,
                    lookup_path: remote_path_id,
                    spec_path: normalized_prim_spec_path(
                        store,
                        remote_path_id,
                        &remote_spec.outer_variant_sites,
                        provenance_remap,
                    ),
                },
            ));

            for (field, value, property_type) in remote_spec.entries {
                let key = OpinionKey {
                    node,
                    layer_strength,
                    layer_id: remote_layer_id,
                    lookup_path: remote_path_id,
                    spec_path: normalized_property_spec_path(
                        store,
                        remote_path_id,
                        &remote_spec.outer_variant_sites,
                        field,
                        provenance_remap,
                    ),
                };
                let index = out.get_mut(&dest_path_id).expect("path exists");
                if let Some(property_type) = property_type {
                    index.add_property_type(field, key.clone(), property_type);
                }
                index.add_opinion(Opinion {
                    key: key.clone(),
                    field,
                    value: {
                        let mut value = value;
                        let targets = nodes.target_map(cycles.stage_layer_stack(), &[]);
                        map_arc_targets(
                            store,
                            &mut value,
                            ArcPathMap {
                                arc: ArcKind::Payloads,
                                source: &target_root,
                                map: &|store, path| targets.map(store, path),
                            },
                            TargetOwner {
                                prim: dest_path_id,
                                property: field,
                                layer: remote_layer_id,
                                spec: key.spec_path.clone(),
                            },
                            cycles,
                        );
                        value
                    },
                    layer_offset: payload_offset,
                });
            }

            if let Some(order) = remote_spec.prim_order {
                prim_order_out.entry(dest_path_id).or_default().push((
                    OpinionKey {
                        node,
                        layer_strength,
                        layer_id: remote_layer_id,
                        lookup_path: remote_path_id,
                        spec_path: normalized_prim_spec_path(
                            store,
                            remote_path_id,
                            &remote_spec.outer_variant_sites,
                            provenance_remap,
                        ),
                    },
                    order,
                ));
            }

            if !remote_spec.authored_children.is_empty() {
                authored_children_out
                    .entry(dest_path_id)
                    .or_default()
                    .push((
                        OpinionKey {
                            node,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: remote_path_id,
                            spec_path: normalized_prim_spec_path(
                                store,
                                remote_path_id,
                                &remote_spec.outer_variant_sites,
                                provenance_remap,
                            ),
                        },
                        remote_spec.authored_children,
                    ));
            }

            // The spec's own variant sets are selected once the prim
            // index is complete (see `LateBranches`).
            if remote_spec.has_variant_sets && late_seen.insert((remote_path_id, dest_path_id)) {
                late_sites.push((remote_path_id, dest_path_id));
            }
        }

        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }
    }

    let nested = NestedArcs {
        remote_stack: &remote_stack,
        combined_stack: &combined_stack,
        anchor: reference.layer,
        layer_offset: reference.layer_offset,
    };
    for &(remote_path_id, dest_path_id) in &mapping {
        let arcs = admitted_arcs(
            store,
            fallbacks,
            out,
            stage_stack,
            &remote_stack,
            &remote_stack,
            reference_path,
            remote_path_id,
            dest_path_id,
            &mut host_selection_cache,
            reference.layer,
            cycles,
        );
        // The arcs of the prim's own branches follow their selection (see
        // `LateBranches`).
        nested.expand(
            store,
            fallbacks,
            &nodes,
            remote_path_id,
            dest_path_id,
            arcs,
            |sites| !in_own_branch(sites, remote_path_id),
            out,
            visited_inherits,
            visited_specializes,
            prim_order_out,
            authored_children_out,
            cycles,
            deps.as_deref_mut(),
        );
    }
    // The arcs the target's ancestors author, and those of the relocation
    // sources outside the target (see `AncestralArcs`).
    let ancestral = AncestralArcs {
        fallbacks,
        data_stack: &remote_stack,
        selection_stack: &combined_stack,
        ancestor_stack: &remote_stack,
        arc_stack: reference.layer,
        dest_root,
        target: reference_path,
        layer_offset: reference.layer_offset,
        class_arc: false,
    };
    ancestral.expand(
        store,
        &nodes,
        out,
        visited_inherits,
        visited_specializes,
        prim_order_out,
        authored_children_out,
        cycles,
        deps.as_deref_mut(),
    );
    ancestral.expand_relocation_sources(
        store,
        &nodes,
        out,
        visited_inherits,
        visited_specializes,
        prim_order_out,
        authored_children_out,
        cycles,
        deps,
    );

    if !late_sites.is_empty() {
        LateArc {
            arc: ArcKind::Payloads,
            path: nodes.path.clone(),
            stage_relocates: Rc::clone(&nodes.stage_relocates),
            stage_stack: stage_stack.clone(),
            combined_stack: combined_stack.clone(),
            remote_stack: remote_stack.clone(),
            anchor: reference.layer,
            layer_offset: reference.layer_offset,
            target_root: reference_path,
            provenance_remap,
            chain: cycles.chain_state(),
        }
        .defer(cycles, late_sites);
    }
    if let Some(check) = target_specs {
        check.finish(out, cycles);
    }
    cycles.exit();
}

fn add_specializes_opinions(
    store: &mut dyn LayerStore,
    fallbacks: &VariantFallbacks,
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
        let specializes = resolve_specializes_for_prim(
            store,
            fallbacks,
            local_stack,
            dest_root,
            SelectionScope::Stack,
        );
        for (arc_list_index, (specialized_root, sites)) in specializes.into_iter().enumerate() {
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
            let branch = local_variant_steps(root_layer_stack(out, dest_root), &sites);
            add_specializes_edge_opinions(
                store,
                fallbacks,
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
    fallbacks: &VariantFallbacks,
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
    if targets_prohibited_child(
        store,
        cycles,
        dest_root,
        ArcKind::Specializes,
        arc_stack,
        specialized_root,
    ) {
        return;
    }
    let variables = composed_variables(store, &cycles.stacks_for(arc_stack));
    if !visited.insert((
        dest_root,
        arc_stack,
        specialized_root,
        parent.implied,
        variables,
    )) {
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
        relocates: None,
        ancestral: None,
        spooky: Rc::from([]),
    };
    // The specializes implied into the next stronger layer stacks or
    // namespaces, with the node this arc is authored as (its placeholder,
    // beneath the root) as their origin; each is implied further by its own
    // expansion (AOUSD Core §10.4.2.4; `_EvalImpliedClasses`).
    let stage_relocates = cycles.relocations().stage();
    let implied = implied_classes(
        store,
        cycles.stage_layer_stack(),
        &stage_relocates,
        parent.steps,
        &step,
    );
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
            fallbacks,
            selection_stack,
            dest_root,
            dest_root,
            implied_root,
            implied.step.layer_stack,
            namespace_depth,
            arc_list_index,
            ArcParent::implied_from(
                &implied.parent,
                origin.clone(),
                parent
                    .spooky
                    .iter()
                    .cloned()
                    .chain(implied.spooky)
                    .collect(),
            ),
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
    // The relocations of the class's layer stack apply to the namespace the
    // arc maps (AOUSD Core §10.3.2.6.1).
    let relocates = lift_arc_relocates(
        store,
        cycles,
        parent.steps,
        arc_stack,
        specialized_root,
        dest_root,
    );
    let stage_relocates = cycles.relocations().stage();

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
    let walk = parent.class_walk(&stage_relocates, relocates.as_deref());
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&specialized_path) else {
                continue;
            };
            rel.to_vec()
        };
        let Some((dest_path_id, moved)) = walk.place(store, dest_root, &rel) else {
            continue;
        };
        if moved && let Some(d) = deps.as_deref_mut() {
            add_relocation_dependencies(d, parent.steps, (dest_root, arc_stack), dest_path_id);
        }
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }
    drop(walk);

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
            fallbacks,
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
    let mut nodes = ArcNodes::new(parent, ArcStep { relocates, ..step }, stage_relocates);
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
                        fallbacks,
                        selection_stack,
                        selection_path_id,
                        local_stack,
                        *remote_path_id,
                    );
                    for branch in spec.selected_variant_branches(&spec_selections) {
                        let variant_spec = branch.spec;
                        let branch_selections =
                            branch.sites(&spec.outer_variant_sites, *remote_path_id);
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

        let redundant = retain_new_class_sites(store, out, &mut pending_sources);
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
            let targets = nodes.target_map(cycles.stage_layer_stack(), &[]);
            map_arc_targets(
                store,
                &mut value,
                ArcPathMap {
                    arc: ArcKind::Specializes,
                    source: &specialized_path,
                    map: &|store, path| targets.map(store, path),
                },
                TargetOwner {
                    prim: dest_path_id,
                    property: field,
                    layer: layer_id,
                    spec: spec_path.clone(),
                },
                cycles,
            );
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
            fallbacks,
            out,
            selection_stack,
            local_stack,
            selection_stack,
            specialized_root,
            remote_path_id,
            selection_path_id,
            &mut host_selection_cache,
            arc_stack,
            cycles,
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
                fallbacks,
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
            fallbacks,
            out,
            selection_stack,
            local_stack,
            selection_stack,
            specialized_root,
            remote_path_id,
            selection_path_id,
            &mut host_selection_cache,
            arc_stack,
            cycles,
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
                fallbacks,
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
                prim_order_out,
                authored_children_out,
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
            fallbacks,
            out,
            selection_stack,
            local_stack,
            selection_stack,
            specialized_root,
            remote_path_id,
            selection_path_id,
            &mut host_selection_cache,
            arc_stack,
            cycles,
        );
        for (ref_index, (reference, sites)) in arcs.references.into_iter().enumerate() {
            let branch = nodes.branch_path(&sites);
            let ref_index = u16::try_from(ref_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            add_reference_edge_opinions(
                store,
                fallbacks,
                selection_stack,
                dest_path_id,
                reference,
                namespace_depth,
                ref_index,
                ArcParent::nested(&branch),
                out,
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
                fallbacks,
                selection_stack,
                dest_path_id,
                payload,
                namespace_depth,
                payload_index,
                ArcParent::nested(&branch),
                out,
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
        fallbacks,
        data_stack: local_stack,
        selection_stack,
        ancestor_stack: selection_stack,
        arc_stack,
        dest_root,
        target: specialized_root,
        layer_offset: base_offset,
        class_arc: true,
    }
    .expand(
        store,
        &nodes,
        out,
        &mut visited_inherits,
        visited,
        prim_order_out,
        authored_children_out,
        cycles,
        deps,
    );
    cycles.exit();
}

/// Adds to the authored child names of each composed prim in `prims` the
/// children relocates move beneath it, and renames the children they
/// rename within it.
///
/// A relocation that moves a prim beneath a new parent adds its name at
/// the parent's node of the relocating layer stack, before the names that
/// node's own specs author, in name order among the names added there. One
/// that renames a prim within its parent renames it in place, keeping its
/// position, among the names that the parent's node of the relocating
/// layer stack and the nodes beneath it author: of two layer stacks
/// renaming one name, the weaker renames it.
///
/// Spec: AOUSD Core §11.3.1 (relocates "rename" children in place and
/// "extend" a prim with sorted children). OpenUSD:
/// `_ComposePrimChildNamesAtNode` in `pxr/usd/pcp/primIndex.cpp`.
fn relocated_child_names(
    store: &dyn LayerStore,
    relocations: &Relocations,
    prims: &HashMap<PathId, PrimIndex>,
    authored_children: &mut HashMap<PathId, ChildOrderOpinions>,
) {
    let paths = store.paths();
    let mut added: HashMap<(PathId, NodeId), (LayerId, Vec<TokenId>)> = HashMap::new();
    // Renames: the parent, the node of the relocating layer stack, and the
    // old and new names.
    let mut renames: Vec<(PathId, Option<NodeId>, TokenId, TokenId)> = Vec::new();
    for (target, source, relocate) in relocations.targets() {
        let (target_path, source_path) = (paths.resolve(target), paths.resolve(source));
        let (Some(parent_path), Some(name)) = (target_path.parent(), target_path.leaf()) else {
            continue;
        };
        let Some(parent) = paths.lookup(&parent_path) else {
            continue;
        };
        let Some(index) = prims.get(&parent) else {
            continue;
        };
        if source_path.parent().as_ref() == Some(&parent_path) {
            let Some(old) = source_path.leaf() else {
                continue;
            };
            // A rename applies at the parent's node of the relocating layer
            // stack, to the names that node and the nodes beneath it add.
            let site = paths
                .resolve(relocate.source)
                .parent()
                .and_then(|site| paths.lookup(&site));
            let node = index
                .graph
                .nodes()
                .find(|(_, node)| {
                    node.layer_stack() == relocate.layer_stack
                        && Some(node.site().prim_path()) == site
                })
                .map(|(id, _)| id);
            renames.push((parent, node, old, name));
            continue;
        }
        // The parent's node of the relocating layer stack.
        let site = relocate
            .target
            .and_then(|target| paths.resolve(target).parent())
            .and_then(|site| paths.lookup(&site));
        let Some((node, _)) = index.graph.nodes().find(|(_, node)| {
            node.layer_stack() == relocate.layer_stack && Some(node.site().prim_path()) == site
        }) else {
            continue;
        };
        added
            .entry((parent, node))
            .or_insert_with(|| (relocate.layer_stack, Vec::new()))
            .1
            .push(name);
    }
    // Deepest node first: a name a weaker layer stack renames is renamed
    // before the stronger ones see it.
    renames.sort_by_cached_key(|(parent, node, _, _)| {
        core::cmp::Reverse(node.map(|node| prims[parent].graph.arc_path(node).len()))
    });
    for (parent, node, old, name) in renames {
        let graph = &prims[&parent].graph;
        let beneath = |key: &OpinionKey| {
            let Some(node) = node else {
                return true;
            };
            let mut cursor = Some(key.node);
            while let Some(at) = cursor {
                if at == node {
                    return true;
                }
                cursor = graph.node(at).and_then(PrimNode::parent);
            }
            false
        };
        for (key, names) in authored_children.get_mut(&parent).into_iter().flatten() {
            if !beneath(key) {
                continue;
            }
            for authored in names.iter_mut().filter(|authored| **authored == old) {
                *authored = name;
            }
        }
    }
    for ((parent, node), (layer, mut names)) in added {
        names.sort_by(|a, b| store.tokens().resolve(*a).cmp(store.tokens().resolve(*b)));
        names.dedup();
        let site = prims[&parent]
            .graph
            .node(node)
            .expect("found node")
            .site()
            .clone();
        // Weaker than every spec of the node.
        let key = OpinionKey {
            node,
            layer_strength: u16::MAX,
            layer_id: layer,
            lookup_path: parent,
            spec_path: site,
        };
        authored_children
            .entry(parent)
            .or_default()
            .push((key, names));
    }
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
    candidates
        .iter()
        .copied()
        .filter(|name| source_authors_child(store, key, *name))
        .collect()
}

/// Whether the spec the source `key` names authors a child prim spec named
/// `name`: a spec at the child's path in that spec's own variant branch
/// context.
///
/// The spec `/P{v=b}` is the branch `v=b` of `/P`, whose children are the
/// specs `/P{v=b}C`; a spec outside any branch has the children authored
/// outside any branch. A spec another branch holds at the same path is
/// never the child of either.
///
/// Spec: AOUSD Core §7.3.6 (variant specs contain prim specs), §11 (stage
/// population). OpenUSD: `PcpComposeSiteChildNames` in
/// `pxr/usd/pcp/composeSite.cpp` reads the `primChildren` of each spec at a
/// node's site, and the site of a variant node is the branch.
fn source_authors_child(store: &dyn LayerStore, key: &OpinionKey, name: TokenId) -> bool {
    let Some(layer) = store.layer(key.layer_id) else {
        return false;
    };
    let paths = store.paths();
    let Some(child_lookup) = paths.lookup(&paths.resolve(key.lookup_path).join(&[name])) else {
        return false;
    };
    let Some(child_prim) = paths.lookup(&paths.resolve(key.spec_path.prim_path()).join(&[name]))
    else {
        return false;
    };
    let spec = key.spec_path.prim_spec().child(name, child_prim);
    layer.branch_prim_spec(child_lookup, &spec, paths).is_some()
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
    /// same prim) come before shallower ones.
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
            variant_set_order: vec![standin],
            ..PrimSpec::default()
        };
        d_spec.variant_selections.insert(standin, anim);
        d_spec.variant_selections.insert(shading, spooky);

        let site = |set, variant| VariantSelectionSite {
            host_path: parent_path,
            set,
            variant,
        };

        // standin=anim, nested in standin=anim and shadingVariant=spooky:
        // anim_spooky_anim_sphere.
        let mut inner_anim = VariantSpec::default();
        inner_anim.authored_children.push(anim_sphere);
        let mut inner_standin = VariantSetSpec::default();
        inner_standin.variants.insert(anim, inner_anim);

        // shadingVariant=spooky, nested in standin=anim: anim_spooky_sphere.
        let mut shading_spooky = VariantSpec {
            variant_set_order: vec![standin],
            ..VariantSpec::default()
        };
        shading_spooky.authored_children.push(sphere);
        shading_spooky.variant_sets.insert(standin, inner_standin);
        let mut shading_set = VariantSetSpec::default();
        shading_set.variants.insert(spooky, shading_spooky);

        let mut standin_anim = VariantSpec {
            variant_set_order: vec![shading],
            ..VariantSpec::default()
        };
        standin_anim.variant_sets.insert(shading, shading_set);
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

        // Build prim index: the spec and its selected branches, the
        // sources the children are authored in.
        let branches = [
            vec![],
            vec![site(standin, anim), site(shading, spooky)],
            vec![
                site(standin, anim),
                site(shading, spooky),
                site(standin, anim),
            ],
        ];
        let mut prims = HashMap::new();
        prims.insert(
            parent_path,
            PrimIndex {
                sources: branches
                    .iter()
                    .map(|sites| OpinionKey {
                        node: NodeId::ROOT,
                        layer_strength: 0,
                        layer_id,
                        lookup_path: parent_path,
                        spec_path: prim_spec_path(&store, parent_path, sites),
                    })
                    .collect(),
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

        filter_variant_children(&store, &VariantFallbacks::default(), &prims, &mut children);

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
        let reference =
            |layer: u64, path| ListOp::explicit(vec![Reference::new(LayerId(layer), path)]);

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
                    .resolve_field_path(crate::path::PropertyPath::new(prim, value))
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
