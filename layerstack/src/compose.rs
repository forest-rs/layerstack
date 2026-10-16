// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition entry points.
//!
//! Composes a stage's prim indexes with [`crate::prim_indexing`], checks
//! the scene description they bring in ([`crate::composition_checks`]),
//! and wraps the result in a [`crate::stage::Stage`].
//!
//! Spec: AOUSD Core §9–§12 (layer stacks, arcs/strength ordering, population, and resolution).

use alloc::{borrow::Cow, collections::BTreeSet, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::variant_fallbacks::{VariantFallbacks, apply_variant_fallbacks};
use crate::{
    composition_checks::{
        Findings, drop_inconsistent_property_kinds, drop_instance_targets, target_error_applies,
    },
    doc::{LayerId, LayerStore},
    expression_variables::{SiteContext, read_selections, site_selections},
    interner::TokenId,
    layer_stack::LayerStack,
    path::PathId,
    prim_index::{OpinionKey, PrimIndex},
    prim_index_graph::PrimIndexGraph,
    stage::{Stage, StageOptions},
};

/// Shares opinion composition and finalization with local namespace updates.
/// `Some` requires the generation-checked `LocalNamespace` contract: one layer,
/// no arcs, variants, relocates or instances, and complete authored ancestors.
/// That proof permits skipping arc discovery as well as population discovery.
pub(crate) fn compose_stage_with_paths(
    store: &mut dyn LayerStore,
    root: LayerId,
    options: StageOptions,
    populated: Option<BTreeSet<PathId>>,
) -> Stage {
    compose_stage_selected(store, root, options, populated, false)
}

/// Exact masks are internal recomposition selections, not USD population masks.
pub(crate) fn compose_stage_selected(
    store: &mut dyn LayerStore,
    root: LayerId,
    options: StageOptions,
    populated: Option<BTreeSet<PathId>>,
    exact_mask: bool,
) -> Stage {
    compose_stage_retained(store, root, options, populated, exact_mask)
}

pub(crate) fn compose_stage_retained(
    store: &mut dyn LayerStore,
    root: LayerId,
    options: StageOptions,
    populated: Option<BTreeSet<PathId>>,
    exact_mask: bool,
) -> Stage {
    let listed: Option<HashSet<PathId>> = populated.map(|paths| paths.into_iter().collect());
    compose_indexed(store, root, options, exact_mask, listed.as_ref())
}

/// Composes the stage rooted at `root` by indexing each prim from its
/// parent's index ([`crate::prim_indexing`]).
fn compose_indexed(
    store: &mut dyn LayerStore,
    root: LayerId,
    options: StageOptions,
    exact_mask: bool,
    listed: Option<&HashSet<PathId>>,
) -> Stage {
    let scope = match (listed, options.mask.as_ref()) {
        (Some(paths), _) => crate::prim_indexing::Scope::listed(store.paths_mut(), paths),
        (None, None) => crate::prim_indexing::Scope::All,
        (None, Some(mask)) if exact_mask => crate::prim_indexing::Scope::Exact(&mask.include),
        (None, Some(mask)) => crate::prim_indexing::Scope::Mask(mask),
    };
    let mut checks = Findings::default();
    let mut composed = crate::prim_indexing::compose(store, root, &options, scope, &mut checks);
    let mut errors = core::mem::take(&mut composed.errors);
    let mut prims = core::mem::take(&mut composed.prims);
    for (path, prim) in &mut prims {
        drop_inconsistent_property_kinds(*path, prim, &mut checks);
    }
    drop_instance_targets(store, &mut prims, &mut checks);
    let (selection_errors, reads) = read_selections(store, &prims);
    errors.extend(selection_errors);
    let check_errors = checks.into_errors();
    errors.extend(check_errors);
    let mut reads_all = core::mem::take(&mut composed.reads);
    reads_all.extend(reads);
    let dependencies = composed.deps.take().map(|mut builder| {
        builder.retain_prims(&prims);
        builder.add_expression_variable_reads(reads_all);
        builder.finish()
    });
    let errors = errors
        .into_iter()
        .filter(|error| error.prim().is_none_or(|prim| prims.contains_key(&prim)))
        .filter(|error| target_error_applies(error, &prims))
        .collect();
    let asset_requests = composed
        .asset_requests
        .into_iter()
        .filter(|request| prims.contains_key(&request.prim))
        .collect();
    let used_layers = composed.used_layers.into_iter().collect();
    Stage::from_parts(
        prims,
        composed.children,
        options.with_provenance,
        dependencies,
    )
    .with_loadable(composed.loadable)
    .with_layer_inventory(
        composed.local_layers,
        used_layers,
        composed.used_layer_sites,
    )
    .with_inactive(composed.inactive, composed.inactive_children)
    .with_composition_work(crate::stage::CompositionWork {
        composed_prim_indexes: composed.composed_prim_indexes,
        reused_prim_indexes: composed.reused_prim_indexes,
        inspected_source_paths: 0,
        indexed_source_paths: 0,
    })
    .with_composition_errors(store, errors)
    .with_asset_requests(asset_requests)
    .with_instances(composed.instances)
    .with_variant_fallbacks(options.variant_fallbacks.clone())
}

/// Resolves the variant selections that govern a composed prim, in strength
/// order.
///
/// The authored selections come from
/// [`authored_strength_ordered_variant_selections`], which evaluates the
/// variant sets one at a time, as OpenUSD does; sets it leaves without a
/// selection take the variant resolver. A branch counts only once an authored selection
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
/// stays unselected, for the resolver. The sets no site declares then take
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

/// Applies `resolver` to `selections`, every authored selection of a
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

/// The variant selections `sources`, sites of the prim index whose graph
/// is `graph`, author, before variant fallbacks apply: the strongest
/// selection of each set, then those authored inside the branches they
/// select, until no branch adds another.
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

#[cfg(test)]
mod child_order_tests {
    use crate::{
        doc::{LayerOffset, Reference},
        prim_index::ArcKind,
        spec_path::{SpecPath, VariantSelectionSite},
    };
    extern crate std;
    use super::*;
    use crate::doc::{InMemoryStore, Layer, PrimSpec, VariantSetSpec, VariantSpec};
    use crate::path::Path;

    use alloc::vec;

    /// The selected branch of a referenced prim's variant set is read with
    /// the reference's offset, and its descendants, which no branch
    /// authors, keep no variant node: an ancestral node without specs is
    /// culled, as OpenUSD culls it (`_NodeCanBeCulled`), so the unselected
    /// branch never contributes either.
    #[test]
    fn inherited_variant_nodes_without_specs_are_culled() {
        let mut store = InMemoryStore::default();
        let [
            model,
            child,
            grandchild,
            world,
            world_child,
            world_grandchild,
        ] = [
            "/Model",
            "/Model/Q",
            "/Model/Q/R",
            "/World",
            "/World/Q",
            "/World/Q/R",
        ]
        .map(|path| store.path(path));
        let mode = store.tokens.intern("mode");
        let a = store.tokens.intern("a");
        let b = store.tokens.intern("b");
        let marker = store.tokens.intern("marker");
        let branch = |value| VariantSpec {
            fields: vec![crate::FieldEntry {
                name: marker,
                value: crate::Value::Int(value).into(),
            }],
            ..VariantSpec::default()
        };
        let mut parent = PrimSpec::def();
        parent.variant_selections.insert(mode, a);
        parent.variant_set_order.push(mode);
        parent.variant_sets.insert(
            mode,
            VariantSetSpec {
                variants: HashMap::from([(a, branch(1)), (b, branch(2))]),
            },
        );
        let mut model_layer = Layer::new(LayerId(2));
        model_layer.insert_prim(model, parent);
        model_layer.insert_prim(child, PrimSpec::def());
        model_layer.insert_prim(grandchild, PrimSpec::def());
        store.insert_layer(model_layer);
        let offset = LayerOffset {
            offset: 20.,
            scale: 2.,
        };
        let mut reference = Reference::new(LayerId(2), model);
        reference.layer_offset = offset;
        let mut root = Layer::new(LayerId(1));
        root.insert_prim(world, PrimSpec::def().with_reference(reference));
        store.insert_layer(root);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let graph = stage.explain_prim_graph(world).unwrap();
        let variants: Vec<_> = graph
            .nodes()
            .filter(|(_, node)| node.arc_kind() == ArcKind::Variants)
            .collect();
        assert_eq!(variants.len(), 1, "only the selected branch");
        assert_eq!(
            variants[0].1.site(),
            &SpecPath::from_variant_selection_sites(
                model,
                &[VariantSelectionSite {
                    host_path: model,
                    set: mode,
                    variant: a
                }],
                &store.paths
            )
        );
        assert_eq!(variants[0].1.layer_offset(), offset);
        assert_eq!(
            stage.resolve_field(world, marker).map(|v| v.value),
            Some(crate::Value::Int(1))
        );
        for path in [world_child, world_grandchild] {
            let graph = stage.explain_prim_graph(path).unwrap();
            assert!(
                graph
                    .nodes()
                    .all(|(_, node)| node.arc_kind() != ArcKind::Variants),
                "spec-less ancestral variant nodes are culled"
            );
            assert!(
                stage
                    .explain_prim(path)
                    .unwrap()
                    .iter()
                    .all(|source| { !source.spec_path.has_variant_selections() }),
                "no variant spec is added to the prim stack"
            );
            assert_eq!(stage.resolve_field(path, marker), None);
        }
    }

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
}

#[cfg(test)]
mod instancing_tests {
    use super::*;
    use crate::doc::Reference;
    use crate::{
        array_edit::{ArrayEdit, ArrayEditOp},
        doc::{InMemoryStore, Layer, PrimSpec, Value},
        listop::ListOp,
        path::PropertyPath,
        property::{PropertySpec, PropertyType},
        stage::ResolvedValue,
    };
    use alloc::vec;

    #[test]
    fn untyped_blocks_keep_weaker_declarations_through_edits() {
        use crate::{LiveStage, SublayerEntry};

        let mut store = InMemoryStore::default();
        let prim = store.path("/Rock");
        let x = store.tokens.intern("x");
        let mut strong = Layer::new(LayerId(1));
        strong.sublayers.push(SublayerEntry::new(LayerId(2)));
        strong.insert_prim(
            prim,
            PrimSpec::over()
                .with_property(x, PropertySpec::attribute().with_default(Value::Blocked)),
        );
        let mut middle = Layer::new(LayerId(2));
        middle.sublayers.push(SublayerEntry::new(LayerId(3)));
        middle.insert_prim(
            prim,
            PrimSpec::over().with_property(
                x,
                PropertySpec::typed_attribute(PropertyType::new(
                    "double",
                    false,
                    Value::Double(0.0),
                )),
            ),
        );
        let mut weak = Layer::new(LayerId(3));
        weak.insert_prim(
            prim,
            PrimSpec::def().with_property(
                x,
                PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                    .with_default(Value::Int(7)),
            ),
        );
        for layer in [strong, middle, weak] {
            store.insert_layer(layer);
        }
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(
            live.stage()
                .resolve_property_declaration(prim, x)
                .unwrap()
                .type_name
                .unwrap()
                .type_name
                .as_ref(),
            "double"
        );
        assert!(
            live.stage()
                .resolve_property_path(PropertyPath::new(prim, x))
                .is_none()
        );

        // Removing just the middle declaration restores the weak declaration,
        // while the stronger value block remains effective (Core §12.2.2).
        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .prims
            .get_mut(&prim)
            .unwrap()
            .properties
            .clear();
        live.notify_layer_prim_edits(LayerId(2), &[prim]);
        live.recompose(&mut store);
        let declaration = live.stage().resolve_property_declaration(prim, x).unwrap();
        assert_eq!(
            declaration.type_name.as_ref().unwrap().type_name.as_ref(),
            "int"
        );
        assert!(
            live.stage()
                .resolve_property_path(PropertyPath::new(prim, x))
                .is_none()
        );
        let clean = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(
            clean.resolve_property_declaration(prim, x),
            Some(declaration)
        );
    }

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
    use crate::{
        composition_error::{CompositionError, UnresolvedAsset},
        doc::{LayerOffset, Reference, ReferenceTarget},
        prim_index::ArcKind,
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
                graph
                    .nodes()
                    .all(|(_, node)| node.layer_stack_identifier() == ROOT.into()),
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

#[cfg(test)]
mod property_snapshot_tests {
    use crate::edit::{EditTarget, Transaction};
    use crate::{
        InMemoryStore, InterpolationType, Layer, LayerId, ListOp, LiveStage, OpinionValue,
        PrimSpec, PropertyEntry, PropertyPath, PropertySpec, PropertyType, Reference, Stage,
        StageOptions, TargetPath, Value,
    };
    use alloc::{sync::Arc, vec};

    fn payload(stage: &Stage, path: PropertyPath) -> &Arc<PropertySpec> {
        match &stage.explain_property_path(path).unwrap()[0].value {
            OpinionValue::Property(spec) => spec,
            OpinionValue::Field(_) => panic!("property opinion"),
        }
    }

    #[test]
    fn placements_share_authored_slots_but_source_and_value_edits_are_isolated() {
        let mut store = InMemoryStore::default();
        let source = store.path("/Emitter");
        let a = store.path("/A");
        let b = store.path("/B");
        let name = store.tokens.intern("rate");
        let doc = store.tokens.intern("documentation");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            source,
            PrimSpec::def().with_property(
                name,
                PropertySpec::typed_attribute(PropertyType::new("int64", false, Value::Int64(0)))
                    .with_default(1_i64)
                    .with_time_samples(vec![(0.0, Value::Int64(2)), (1.0, Value::Int64(3))])
                    .with_metadata(doc, Value::string("Shared authored data")),
            ),
        );
        for path in [a, b] {
            layer.insert_prim(
                path,
                PrimSpec::def().with_reference(Reference::new(LayerId(1), source)),
            );
        }
        store.insert_layer(layer);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let a_path = PropertyPath::new(a, name);
        let b_path = PropertyPath::new(b, name);
        let first = payload(&stage, a_path);
        let second = payload(&stage, b_path);
        assert!(Arc::ptr_eq(first, second));
        assert!(Arc::ptr_eq(
            first,
            &store.layers[&LayerId(1)].prims[&source].properties[0].spec
        ));
        let samples = first.time_samples.as_ref().unwrap().as_ptr();
        let metadata = first.metadata.as_ptr();
        let authored = store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .property_mut(PropertyPath::new(source, name))
            .unwrap();
        authored.default = Some(Value::Int64(4));
        assert_eq!(authored.time_samples.as_ref().unwrap().as_ptr(), samples);
        assert_eq!(authored.metadata.as_ptr(), metadata);
        assert_eq!(
            stage.resolve_field_path(a_path).unwrap().value,
            Value::Int64(1)
        );
        authored.time_samples.as_mut().unwrap().make_mut()[0].1 = Value::Int64(9);
        authored.set_metadata(doc, Value::string("Changed source"));
        assert_eq!(first.time_samples.as_deref().unwrap()[0].1, Value::Int64(2));
        assert_eq!(
            second.metadata(doc),
            Some(&Value::string("Shared authored data").into())
        );
        let fresh = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(
            fresh.resolve_field_path(a_path).unwrap().value,
            Value::Int64(4)
        );
        assert_eq!(
            fresh
                .resolve_property_path_at_time(b_path, 0.0, InterpolationType::Linear)
                .unwrap()
                .value,
            Value::Int64(9)
        );
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let before = payload(live.stage(), a_path);
        let sample_buffer = before.time_samples.as_ref().unwrap().as_ptr();
        let metadata_buffer = before.metadata.as_ptr();
        let target = EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(source, name));
        let mut edit = Transaction::new();
        edit.set_default(target.clone(), Value::Int64(5));
        let applied = live.apply(&mut store, &edit).unwrap();
        let after = payload(live.stage(), a_path);
        assert!(Arc::ptr_eq(after, payload(live.stage(), b_path)));
        assert_eq!(after.time_samples.as_ref().unwrap().as_ptr(), sample_buffer);
        assert_eq!(after.metadata.as_ptr(), metadata_buffer);
        assert_eq!(after.default, Some(Value::Int64(5)));
        live.apply(&mut store, &applied.inverse).unwrap();
        assert_eq!(payload(live.stage(), a_path).default, Some(Value::Int64(4)));
        let mut sample_edit = Transaction::new();
        sample_edit.set_time_sample(target, 0.0, Value::Int64(10));
        let applied = live.apply(&mut store, &sample_edit).unwrap();
        let after = payload(live.stage(), a_path);
        assert!(Arc::ptr_eq(after, payload(live.stage(), b_path)));
        let authored = store.layers[&LayerId(1)]
            .property(PropertyPath::new(source, name))
            .unwrap();
        assert_eq!(
            after.time_samples.as_ref().unwrap().as_ptr(),
            authored.time_samples.as_ref().unwrap().as_ptr()
        );
        assert_eq!(after.metadata.as_ptr(), metadata_buffer);
        assert_eq!(
            payload(&fresh, a_path).time_samples.as_deref().unwrap()[0].1,
            Value::Int64(9)
        );
        live.apply(&mut store, &applied.inverse).unwrap();
        assert_eq!(
            payload(live.stage(), a_path)
                .time_samples
                .as_deref()
                .unwrap()[0]
                .1,
            Value::Int64(9)
        );
    }

    #[test]
    fn distinct_source_edits_do_not_merge_aliased_property_payloads() {
        let mut store = InMemoryStore::default();
        let source = store.path("/Emitter");
        let instance = store.path("/Copy");
        let first = store.tokens.intern("first");
        let second = store.tokens.intern("second");
        let shared = Arc::new(
            PropertySpec::typed_attribute(PropertyType::new("int64", false, Value::Int64(0)))
                .with_default(1_i64),
        );
        let mut spec = PrimSpec::def();
        spec.properties = vec![
            PropertyEntry {
                name: first,
                spec: Arc::clone(&shared),
            },
            PropertyEntry {
                name: second,
                spec: shared,
            },
        ];
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(source, spec);
        layer.insert_prim(
            instance,
            PrimSpec::def().with_reference(Reference::new(LayerId(1), source)),
        );
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let target = EditTarget::for_layer(LayerId(1));
        let mut edit = Transaction::new();
        edit.set_default(
            target.property(PropertyPath::new(source, first)),
            Value::Int64(2),
        );
        edit.set_default(
            target.property(PropertyPath::new(source, second)),
            Value::Int64(3),
        );
        let applied = live.apply(&mut store, &edit).unwrap();
        for prim in [source, instance] {
            assert_eq!(
                payload(live.stage(), PropertyPath::new(prim, first)).default,
                Some(Value::Int64(2))
            );
            assert_eq!(
                payload(live.stage(), PropertyPath::new(prim, second)).default,
                Some(Value::Int64(3))
            );
        }
        live.apply(&mut store, &applied.inverse).unwrap();
        for prim in [source, instance] {
            for name in [first, second] {
                assert_eq!(
                    payload(live.stage(), PropertyPath::new(prim, name)).default,
                    Some(Value::Int64(1))
                );
            }
        }
    }

    #[test]
    fn mapped_targets_detach_without_copying_untouched_metadata() {
        let mut store = InMemoryStore::default();
        let source = store.path("/Emitter");
        let leaf = store.path("/Emitter/Leaf");
        let a = store.path("/A");
        let b = store.path("/B");
        let name = store.tokens.intern("link");
        let doc = store.tokens.intern("documentation");
        let target = TargetPath::Prim(leaf);
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            source,
            PrimSpec::def().with_property(
                name,
                PropertySpec::relationship()
                    .with_targets(ListOp::explicit(vec![target]))
                    .with_metadata(doc, Value::string("Keep shared")),
            ),
        );
        layer.insert_prim(leaf, PrimSpec::def());
        for path in [a, b] {
            layer.insert_prim(
                path,
                PrimSpec::def().with_reference(Reference::new(LayerId(1), source)),
            );
        }
        store.insert_layer(layer);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let authored = store.layers[&LayerId(1)]
            .property(PropertyPath::new(source, name))
            .unwrap();
        let a_spec = payload(&stage, PropertyPath::new(a, name));
        let b_spec = payload(&stage, PropertyPath::new(b, name));
        assert!(!Arc::ptr_eq(a_spec, b_spec));
        assert_eq!(a_spec.metadata.as_ptr(), authored.metadata.as_ptr());
        assert_eq!(b_spec.metadata.as_ptr(), authored.metadata.as_ptr());
        assert_eq!(authored.targets, Some(ListOp::explicit(vec![target])));
        let a_leaf = store.path("/A/Leaf");
        let b_leaf = store.path("/B/Leaf");
        assert_eq!(
            a_spec.targets,
            Some(ListOp::explicit(vec![TargetPath::Prim(a_leaf)]))
        );
        assert_eq!(
            b_spec.targets,
            Some(ListOp::explicit(vec![TargetPath::Prim(b_leaf)]))
        );
    }
}

#[cfg(test)]
mod sparse_tests;

/// References and payloads an importer deferred ([`Reference::deferred`]),
/// which composition resolves through the store.
///
/// Spec: AOUSD Core §10.3.2.1 (references), §10.3.2.2 (payloads),
/// §10.3.2.5 (only the selected variant contributes).
#[cfg(test)]
mod deferred_arc_tests {
    use super::*;
    use crate::{
        AssetRequest,
        doc::{InMemoryStore, Layer, PrimSpec, VariantSetSpec, VariantSpec},
    };
    use crate::{
        composition_error::{CompositionError, UnresolvedAsset},
        doc::{LayerOffset, Reference, ReferenceTarget},
        prim_index::ArcKind,
    };
    use alloc::vec;

    const ROOT: LayerId = LayerId(1);
    const ASSET: LayerId = LayerId(2);

    #[test]
    fn deferred_arcs_follow_the_store_and_ask_for_unexamined_assets() {
        let mut store = InMemoryStore::default();
        let value = store.tokens.intern("value");
        let target = store.path("/Target");
        let deferred = |asset: &str| {
            Reference::deferred(
                ROOT,
                asset,
                ReferenceTarget::Prim(target),
                LayerOffset::IDENTITY,
            )
        };
        let mut asset = Layer::new(ASSET);
        asset.insert_prim(
            target,
            PrimSpec::def()
                .with_property(value, crate::PropertySpec::attribute().with_default(3_i64)),
        );
        store.insert_layer(asset);
        store.insert_asset_layer(ROOT, "./loaded.usda", ASSET);
        store.mark_asset_missing(ROOT, "./missing.usda");

        // A branch no selection names asks for nothing.
        let set = store.tokens.intern("look");
        let (x, y) = (store.tokens.intern("x"), store.tokens.intern("y"));
        let mut host = PrimSpec::def();
        let branch = VariantSpec {
            references: crate::ListOp::prepended(vec![deferred("./never.usda")]),
            ..VariantSpec::default()
        };
        host.variant_sets.insert(
            set,
            VariantSetSpec {
                variants: [(x, branch), (y, VariantSpec::default())]
                    .into_iter()
                    .collect(),
            },
        );
        host.variant_set_order.push(set);
        host.variant_selections.insert(set, y);

        let mut root = Layer::new(ROOT);
        for (path, spec) in [
            (
                "/Loaded",
                PrimSpec::def().with_reference(deferred("./loaded.usda")),
            ),
            (
                "/Missing",
                PrimSpec::def().with_reference(deferred("./missing.usda")),
            ),
            (
                "/Pending",
                PrimSpec::def().with_payload(deferred("./pending.usda")),
            ),
            ("/Unselected", host),
        ] {
            let path = store.path(path);
            root.insert_prim(path, spec);
        }
        store.insert_layer(root);
        let stage = Stage::compose(&mut store, ROOT, StageOptions::default());

        let loaded = store.path("/Loaded");
        assert_eq!(
            stage
                .resolve_field_path(crate::path::PropertyPath::new(loaded, value))
                .map(|resolved| resolved.value),
            Some(crate::Value::Int64(3))
        );
        let missing = store.path("/Missing");
        assert_eq!(
            stage.composition_errors(),
            [CompositionError::UnresolvedAsset(UnresolvedAsset {
                prim: missing,
                arc: ArcKind::References,
                asset: "./missing.usda".into(),
            })]
        );
        let pending = store.path("/Pending");
        assert_eq!(
            stage.asset_requests(),
            [AssetRequest {
                anchor: ROOT,
                asset_path: "./pending.usda".into(),
                prim: pending,
                arc: ArcKind::Payloads,
            }]
        );
        assert!(stage.used_layers(false).contains(&ASSET));
    }

    /// A deferred arc and an importer's resolved arc to the same layer are
    /// one item for list editing once composition resolves the deferred
    /// one, so a stronger layer's deferred `delete` removes a weaker
    /// layer's resolved arc.
    #[test]
    fn resolved_deferred_arcs_list_edit_like_resolved_arcs() {
        let mut store = InMemoryStore::default();
        let value = store.tokens.intern("value");
        let target = store.path("/Target");
        let model = store.path("/Model");
        let mut asset = Layer::new(ASSET);
        asset.insert_prim(
            target,
            PrimSpec::def()
                .with_property(value, crate::PropertySpec::attribute().with_default(3_i64)),
        );
        store.insert_layer(asset);
        let weak = LayerId(3);
        let mut weak_layer = Layer::new(weak);
        weak_layer.insert_prim(
            model,
            PrimSpec::def().with_reference(Reference::with_asset(ASSET, target, "./asset.usda")),
        );
        store.insert_layer(weak_layer);
        store.insert_asset_layer(ROOT, "./asset.usda", ASSET);
        let mut root = Layer::new(ROOT);
        root.sublayers.push(crate::SublayerEntry::new(weak));
        let mut spec = PrimSpec::over();
        spec.references.delete = vec![Reference::deferred(
            ROOT,
            "./asset.usda",
            ReferenceTarget::Prim(target),
            LayerOffset::IDENTITY,
        )];
        root.insert_prim(model, spec);
        store.insert_layer(root);
        let stage = Stage::compose(&mut store, ROOT, StageOptions::default());
        assert!(
            stage
                .resolve_field_path(crate::path::PropertyPath::new(model, value))
                .is_none()
        );
        assert!(stage.asset_requests().is_empty());
    }
}
