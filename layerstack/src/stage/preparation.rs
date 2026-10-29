// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a live stage retains so a namespace edit can be prepared against
//! its snapshot without composing or scanning the stage.
//!
//! [`crate::NamespaceEdit::prepare`] must prove the snapshot current, find
//! the prims whose composition graphs reach the moved namespace, and find
//! the authored specs a move or delete repairs. Composing the stage again
//! and visiting every prim and spec answers all three, at the cost of the
//! stage. A [`crate::LiveStage`] already keeps its snapshot in step with
//! its layers, so it keeps these answers too, as its composition changes:
//!
//! - the generations of the layers the snapshot reflects, recorded only
//!   while nothing is pending, so a snapshot is current exactly when they
//!   all still match the store;
//! - the composed prims of each site a composition arc other than the
//!   local one reaches, and of each namespace above such a site;
//! - the layers each prim's opinions come from ([`Prim::prim_stack`](crate::Prim::prim_stack));
//! - for each layer, the prim specs that author a path or a composition
//!   arc ([`slot_names_paths`]), and the layers their references and
//!   payloads name ([`LayerArcs`]), as of a generation of the layer, found
//!   by the first preparation that asks for them: an open that never
//!   edits namespace never visits every spec for them.
//!
//! A stage composed by [`Stage::compose`] keeps none of this, and
//! preparation then composes and scans it as before.
//!
//! Spec: AOUSD Core §8 (namespace), §10 (composition arcs).

use crate::sort;
use alloc::{sync::Arc, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::{
    ListOp,
    doc::{
        FieldEntry, FieldValue, Layer, LayerId, LayerStore, PrimSpec, Reference, Value, VariantSets,
    },
    path::{PathId, PathInterner},
    prim_index::{ArcKind, PrimIndex},
    prim_indexing::ChildIndex,
    property::PropertyEntry,
    sync::Memo,
};

use super::Stage;

/// Retained evidence for preparing namespace edits against a live stage's
/// snapshot. See the [module docs](self).
#[derive(Debug)]
pub(crate) struct PreparationIndex {
    /// The generation of every layer the snapshot reads, `None` for one
    /// the store did not hold, when the snapshot reflects them all and no
    /// recomposition is pending; `None` otherwise.
    generations: Option<Vec<(LayerId, Option<u64>)>>,
    /// The live stage's index of the child specs of its layers.
    children: Arc<ChildIndex>,
    /// The prim specs of each layer that author paths, by generation.
    links: HashMap<LayerId, LayerLinks>,
    /// What the composed prims draw on, found from the snapshot's prim
    /// indexes when a preparation first asks ([`Stage::preparation`]) and
    /// kept current from then on: an open that never prepares a namespace
    /// edit never finds it.
    composed: Memo<ComposedEvidence>,
}

/// What a snapshot's composed prims draw on, for a preparation to find the
/// prims an edit reaches.
#[derive(Debug, Default)]
struct ComposedEvidence {
    /// Each composed prim's opinion layers, sorted.
    prim_stack_layers: HashMap<PathId, Vec<LayerId>>,
    /// How many composed prims draw opinions from each layer.
    stack_layer_counts: HashMap<LayerId, usize>,
    /// The site prim paths of each composed prim's non-local nodes, sorted.
    prim_node_sites: HashMap<PathId, Vec<PathId>>,
    /// Site prim path → composed prims with a non-local node there.
    node_sites: HashMap<PathId, HashSet<PathId>>,
    /// Path → composed prims with a non-local node at or beneath it.
    nodes_beneath: HashMap<PathId, HashSet<PathId>>,
}

/// The prim specs of one layer that author a path or an arc
/// ([`slot_names_paths`]), at one generation of the layer: found when first
/// asked for, then kept up to date as the layer changes.
#[derive(Debug)]
struct LayerLinks {
    generation: u64,
    slots: Memo<LinkSlots>,
}

/// A layer's prim slots that author paths ([`slot_names_paths`]), and
/// the layers its references and payloads name ([`LayerArcs`]).
#[derive(Debug, Default, PartialEq)]
struct LinkSlots {
    paths: HashSet<PathId>,
    arcs: LayerArcs,
}

/// The references and payloads a layer's prim slots author, outside
/// variant branches or in one, by the layers they name: what namespace
/// preparation reads of every layer a stage reaches, to follow its arcs,
/// to reject those that do not resolve, and to find those targeting an
/// edited layer. The layer an arc the importer resolved names is part of
/// the layer, so it is kept; an arc the store resolves (a deferred one)
/// or composition evaluates (an asset expression) is only marked, for
/// preparation to read from its spec.
///
/// Spec: AOUSD Core §10.3.2.2 (references), §10.3.2.3 (payloads), §10.5
/// (variants).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LayerArcs {
    /// Each slot authoring a reference or payload, with what it names.
    slots: HashMap<PathId, SlotArcs>,
    /// The layers the slots' resolved arcs name, sorted.
    layers: Vec<LayerId>,
    /// The layers the resolved arcs of the slots' specs outside variant
    /// branches name, sorted.
    main_layers: Vec<LayerId>,
    /// The slots authoring a deferred or expression arc, sorted.
    unresolved: Vec<PathId>,
}

/// The references and payloads one prim slot authors ([`LayerArcs`]).
#[derive(Clone, Debug, PartialEq)]
struct SlotArcs {
    /// The layers its resolved arcs name, sorted.
    layers: Vec<LayerId>,
    /// Those named by its specs outside variant branches, sorted.
    main_layers: Vec<LayerId>,
    /// Whether it authors a deferred or expression arc.
    unresolved: bool,
}

impl SlotArcs {
    /// The arcs `layer` authors at the prim path `path`, if any.
    fn of(layer: &Layer, path: PathId) -> Option<Self> {
        fn visit(refs: &ListOp<Reference>, layers: &mut Vec<LayerId>, unresolved: &mut bool) {
            for r in refs.items() {
                if r.deferred || r.is_expression() {
                    *unresolved = true;
                } else {
                    layers.push(r.layer);
                }
            }
        }
        fn variants(sets: &VariantSets, layers: &mut Vec<LayerId>, unresolved: &mut bool) {
            for v in sets.values().flat_map(|set| set.variants.values()) {
                visit(&v.references, layers, unresolved);
                visit(&v.payloads, layers, unresolved);
                variants(&v.variant_sets, layers, unresolved);
            }
        }
        let (mut layers, mut main_layers, mut unresolved) = (Vec::new(), Vec::new(), false);
        let mut any = false;
        let specs = layer
            .prims
            .get(&path)
            .into_iter()
            .map(|spec| (spec, true))
            .chain(
                layer
                    .variant_prims
                    .get(&path)
                    .into_iter()
                    .flatten()
                    .map(|spec| (spec, false)),
            );
        for (spec, main) in specs {
            any |= !spec.references.is_empty() || !spec.payloads.is_empty();
            if main && spec.outer_variant_sites().is_empty() {
                visit(&spec.references, &mut main_layers, &mut unresolved);
                visit(&spec.payloads, &mut main_layers, &mut unresolved);
            }
            visit(&spec.references, &mut layers, &mut unresolved);
            visit(&spec.payloads, &mut layers, &mut unresolved);
            let before = layers.len();
            variants(spec.variant_sets(), &mut layers, &mut unresolved);
            any |= layers.len() != before;
        }
        any |= unresolved;
        if !any {
            return None;
        }
        for list in [&mut layers, &mut main_layers] {
            sort::sort_unstable(list);
            list.dedup();
        }
        Some(Self {
            layers,
            main_layers,
            unresolved,
        })
    }
}

impl LayerArcs {
    /// Records the arcs `layer` authors at `path` now, replacing those it
    /// recorded there.
    fn refresh(&mut self, layer: &Layer, path: PathId) {
        match SlotArcs::of(layer, path) {
            Some(arcs) => self.slots.insert(path, arcs),
            None => self.slots.remove(&path),
        };
    }

    /// Gathers the layers and slots of every recorded slot.
    fn gather(&mut self) {
        self.layers.clear();
        self.main_layers.clear();
        self.unresolved.clear();
        for (path, arcs) in &self.slots {
            self.layers.extend(&arcs.layers);
            self.main_layers.extend(&arcs.main_layers);
            if arcs.unresolved {
                self.unresolved.push(*path);
            }
        }
        for list in [&mut self.layers, &mut self.main_layers] {
            sort::sort_unstable(list);
            list.dedup();
        }
        sort::sort_unstable(&mut self.unresolved);
    }

    /// The layers the resolved arcs name, sorted.
    pub(crate) fn layers(&self) -> &[LayerId] {
        &self.layers
    }

    /// The layers the resolved arcs of specs outside variant branches
    /// name, sorted.
    pub(crate) fn main_layers(&self) -> &[LayerId] {
        &self.main_layers
    }

    /// The slots authoring a deferred or expression arc, whose layers the
    /// store or composition resolves, sorted.
    pub(crate) fn unresolved(&self) -> &[PathId] {
        &self.unresolved
    }

    /// The slots whose resolved arcs name one of `layers`, unsorted.
    pub(crate) fn naming<'a>(&'a self, layers: &'a [LayerId]) -> impl Iterator<Item = PathId> + 'a {
        let any = self.layers.iter().any(|id| layers.contains(id));
        self.slots
            .iter()
            .filter(move |(_, arcs)| any && arcs.layers.iter().any(|id| layers.contains(id)))
            .map(|(path, _)| *path)
    }
}

impl PreparationIndex {
    /// An empty index sharing `children` with its live stage.
    pub(crate) fn new(children: Arc<ChildIndex>) -> Self {
        Self {
            generations: None,
            children,
            links: HashMap::new(),
            composed: Memo::default(),
        }
    }

    /// The composed evidence, which [`Stage::preparation`] finds before
    /// lending the index.
    fn composed(&self) -> &ComposedEvidence {
        self.composed
            .get()
            .expect("found when the stage lends the index")
    }

    /// The live stage's child index.
    pub(crate) fn children(&self) -> &ChildIndex {
        &self.children
    }

    /// Whether the snapshot reflects the store: every layer it reads
    /// still has the generation it was composed at, and nothing was
    /// pending when it was recorded.
    pub(crate) fn is_current(&self, store: &dyn LayerStore) -> bool {
        self.generations.as_ref().is_some_and(|generations| {
            generations
                .iter()
                .all(|(id, seen)| store.layer(*id).map(Layer::generation) == *seen)
        })
    }

    /// Records the layer generations the snapshot reflects, or `None`
    /// while a recomposition is pending, and brings the link inventory of
    /// each recorded layer up to its generation.
    pub(crate) fn record_generations(
        &mut self,
        store: &dyn LayerStore,
        generations: Option<Vec<(LayerId, Option<u64>)>>,
    ) {
        if let Some(generations) = &generations {
            let mut refreshed = 0;
            for (id, seen) in generations {
                if let (Some(_), Some(layer)) = (seen, store.layer(*id)) {
                    self.refresh_links(layer);
                    refreshed += 1;
                }
            }
            // Forget the layers the snapshot no longer reads.
            if self.links.len() > refreshed {
                let read: HashSet<LayerId> = generations
                    .iter()
                    .filter(|(_, seen)| seen.is_some())
                    .map(|(id, _)| *id)
                    .collect();
                self.links.retain(|id, _| read.contains(id));
            }
        }
        self.generations = generations;
    }

    /// Brings `layer`'s link inventory up to its generation: one found
    /// already is patched from the prim paths the layer recorded as changed
    /// when its history reaches back that far; any other is left for the
    /// first preparation that asks for it ([`Self::links`]) to find.
    fn refresh_links(&mut self, layer: &Layer) {
        let generation = layer.generation();
        if let Some(links) = self.links.get_mut(&layer.id) {
            if links.generation == generation {
                return;
            }
            let Some(slots) = links.slots.get_mut() else {
                links.generation = generation;
                return;
            };
            if let Some(changed) = layer.changed_paths_since(links.generation) {
                for path in changed {
                    if slot_names_paths(layer, path) {
                        slots.paths.insert(path);
                    } else {
                        slots.paths.remove(&path);
                    }
                    slots.arcs.refresh(layer, path);
                }
                slots.arcs.gather();
                links.generation = generation;
                return;
            }
        }
        self.links.insert(
            layer.id,
            LayerLinks {
                generation,
                slots: Memo::default(),
            },
        );
    }

    /// The prim slots of `layer` that author paths, when the inventory
    /// holds the layer at its generation, found by visiting every spec of
    /// the layer the first time they are asked for.
    pub(crate) fn links(&self, layer: &Layer) -> Option<&HashSet<PathId>> {
        self.link_slots(layer).map(|slots| &slots.paths)
    }

    /// The references and payloads of `layer` ([`LayerArcs`]), found
    /// with [`Self::links`].
    pub(crate) fn arcs(&self, layer: &Layer) -> Option<&LayerArcs> {
        self.link_slots(layer).map(|slots| &slots.arcs)
    }

    fn link_slots(&self, layer: &Layer) -> Option<&LinkSlots> {
        self.links
            .get(&layer.id)
            .filter(|links| links.generation == layer.generation())
            .map(|links| links.slots.get_or_init(|| scan_links(layer)))
    }

    /// Every layer some composed prim's opinions come from.
    pub(crate) fn stack_layers(&self) -> impl Iterator<Item = LayerId> + '_ {
        self.composed().stack_layer_counts.keys().copied()
    }

    /// The composed prims with a non-local node at a site at or beneath
    /// `path`.
    pub(crate) fn prims_with_nodes_beneath(
        &self,
        path: PathId,
    ) -> impl Iterator<Item = PathId> + '_ {
        let beneath = &self.composed().nodes_beneath;
        beneath.get(&path).into_iter().flatten().copied()
    }

    /// The composed prims with a non-local node at the site `path`.
    pub(crate) fn prims_with_nodes_at(&self, path: PathId) -> impl Iterator<Item = PathId> + '_ {
        let sites = &self.composed().node_sites;
        sites.get(&path).into_iter().flatten().copied()
    }

    /// Replaces `prim`'s entries with those of its composed `index`, or
    /// removes them when it is no longer composed, once the evidence is
    /// found.
    pub(crate) fn reindex(
        &mut self,
        paths: &PathInterner,
        prim: PathId,
        index: Option<&PrimIndex>,
    ) {
        if let Some(composed) = self.composed.get_mut() {
            composed.reindex(paths, prim, index);
        }
    }
}

impl ComposedEvidence {
    /// The evidence of every prim of `prims`.
    fn of(paths: &PathInterner, prims: &HashMap<PathId, PrimIndex>) -> Self {
        let mut composed = Self::default();
        for (&prim, index) in prims {
            composed.reindex(paths, prim, Some(index));
        }
        composed
    }

    /// Replaces `prim`'s entries with those of its composed `index`, or
    /// removes them when it is no longer composed.
    fn reindex(&mut self, paths: &PathInterner, prim: PathId, index: Option<&PrimIndex>) {
        let mut layers: Vec<LayerId> = index
            .map(|index| {
                index
                    .sources
                    .iter()
                    .map(|source| source.key.layer_id)
                    .collect()
            })
            .unwrap_or_default();
        sort::sort_unstable(&mut layers);
        layers.dedup();
        let old = if layers.is_empty() {
            self.prim_stack_layers.remove(&prim)
        } else {
            self.prim_stack_layers.insert(prim, layers.clone())
        };
        if old.as_ref() != Some(&layers) {
            for layer in old.into_iter().flatten() {
                if let Some(count) = self.stack_layer_counts.get_mut(&layer) {
                    *count -= 1;
                    if *count == 0 {
                        self.stack_layer_counts.remove(&layer);
                    }
                }
            }
            for &layer in &layers {
                *self.stack_layer_counts.entry(layer).or_default() += 1;
            }
        }

        let mut sites: Vec<PathId> = index
            .map(|index| {
                index
                    .graph
                    .nodes()
                    .filter(|(_, node)| node.arc_kind() != ArcKind::Local)
                    .map(|(_, node)| node.site().prim_path())
                    .collect()
            })
            .unwrap_or_default();
        sort::sort_unstable(&mut sites);
        sites.dedup();
        let old = if sites.is_empty() {
            self.prim_node_sites.remove(&prim)
        } else {
            self.prim_node_sites.insert(prim, sites.clone())
        };
        if old.as_ref() == Some(&sites) {
            return;
        }
        for site in old.into_iter().flatten() {
            remove_from(&mut self.node_sites, site, prim);
            for path in at_or_above(paths, site) {
                remove_from(&mut self.nodes_beneath, path, prim);
            }
        }
        for &site in &sites {
            self.node_sites.entry(site).or_default().insert(prim);
            for path in at_or_above(paths, site) {
                self.nodes_beneath.entry(path).or_default().insert(prim);
            }
        }
    }
}

#[cfg(test)]
impl PreparationIndex {
    /// Asserts that this index holds the prims `fresh`, built from a new
    /// composition of the same layers, holds, and that it records the
    /// generations the store has.
    pub(crate) fn assert_current_as(&self, fresh: &Self, store: &dyn LayerStore) {
        assert!(self.is_current(store), "the snapshot is current");
        let (ours, theirs) = (self.composed(), fresh.composed());
        assert_eq!(ours.prim_stack_layers, theirs.prim_stack_layers);
        assert_eq!(ours.stack_layer_counts, theirs.stack_layer_counts);
        assert_eq!(ours.prim_node_sites, theirs.prim_node_sites);
        assert_eq!(ours.node_sites, theirs.node_sites);
        assert_eq!(ours.nodes_beneath, theirs.nodes_beneath);
        for (id, links) in &self.links {
            if let Some(layer) = store.layer(*id) {
                assert_eq!(links.generation, layer.generation());
                assert_eq!(
                    self.link_slots(layer),
                    Some(&scan_links(layer)),
                    "links of {id:?}"
                );
            }
        }
    }
}

fn remove_from(map: &mut HashMap<PathId, HashSet<PathId>>, key: PathId, prim: PathId) {
    if let Some(prims) = map.get_mut(&key) {
        prims.remove(&prim);
        if prims.is_empty() {
            map.remove(&key);
        }
    }
}

/// `path` and its ancestors, the pseudo-root excluded.
fn at_or_above(paths: &PathInterner, path: PathId) -> impl Iterator<Item = PathId> + '_ {
    core::iter::successors(Some(path), |path| paths.parent(*path))
        .filter(|path| paths.parent(*path).is_some())
}

/// The prim slots of `layer` that author paths ([`slot_names_paths`]),
/// and their references and payloads ([`LayerArcs`]).
fn scan_links(layer: &Layer) -> LinkSlots {
    let mut slots = LinkSlots::default();
    for &path in layer.prims.keys().chain(layer.variant_prims.keys()) {
        if slot_names_paths(layer, path) {
            slots.paths.insert(path);
            slots.arcs.refresh(layer, path);
        }
    }
    slots.arcs.gather();
    slots
}

/// The prim slots of `layer` that author paths ([`slot_names_paths`]):
/// the retained inventory of `index` when it holds the layer at its
/// generation, otherwise found by visiting every spec.
pub(crate) fn layer_links<'a>(
    index: Option<&'a PreparationIndex>,
    layer: &Layer,
) -> alloc::borrow::Cow<'a, HashSet<PathId>> {
    match index.and_then(|index| index.links(layer)) {
        Some(links) => alloc::borrow::Cow::Borrowed(links),
        None => alloc::borrow::Cow::Owned(scan_links(layer).paths),
    }
}

/// The references and payloads of `layer` ([`LayerArcs`]): the retained
/// inventory of `index` when it holds the layer at its generation,
/// otherwise found by visiting every spec.
pub(crate) fn layer_arcs<'a>(
    index: Option<&'a PreparationIndex>,
    layer: &Layer,
) -> alloc::borrow::Cow<'a, LayerArcs> {
    match index.and_then(|index| index.arcs(layer)) {
        Some(arcs) => alloc::borrow::Cow::Borrowed(arcs),
        None => alloc::borrow::Cow::Owned(scan_links(layer).arcs),
    }
}

/// Whether a spec of `layer` at the prim path `path`, outside variant
/// branches or in one, authors a path a namespace edit may repair or an
/// arc it must inspect: a reference, payload, inherit or specialize, a
/// variant set, a relationship target or attribute connection, a path
/// list field, or a path expression value at any depth. The repairs of a
/// namespace edit change no other spec.
pub(crate) fn slot_names_paths(layer: &Layer, path: PathId) -> bool {
    layer
        .prims
        .get(&path)
        .into_iter()
        .chain(layer.variant_prims.get(&path).into_iter().flatten())
        .any(spec_names_paths)
}

fn spec_names_paths(spec: &PrimSpec) -> bool {
    !spec.references.is_empty()
        || !spec.payloads.is_empty()
        || !spec.inherits().is_empty()
        || !spec.specializes().is_empty()
        || !spec.variant_sets().is_empty()
        || fields_name_paths(&spec.fields)
        || spec.properties.iter().any(property_names_paths)
}

fn property_names_paths(entry: &PropertyEntry) -> bool {
    let spec = &entry.spec;
    spec.targets.is_some()
        || spec.default.as_ref().is_some_and(value_names_paths)
        || spec
            .time_samples
            .get()
            .is_some_and(|samples| samples.iter().any(|(_, v)| value_names_paths(v)))
        || fields_name_paths(&spec.metadata)
}

fn fields_name_paths(fields: &[FieldEntry]) -> bool {
    fields.iter().any(|field| match &field.value {
        FieldValue::Value(value) => value_names_paths(value),
        FieldValue::PathListOp(_) => true,
        _ => false,
    })
}

/// Whether `value` holds a path a namespace edit may fix: a path
/// expression, or the `SdfPath` of a relationship's legacy `default`
/// ([`Value::PathText`]), which OpenUSD's `_PathReplacePrefix` fixes.
fn value_names_paths(value: &Value) -> bool {
    match value {
        Value::PathExpression(_) | Value::PathText(_) => true,
        Value::Array(values) => values.iter().any(value_names_paths),
        Value::Dictionary(values) => values.iter().any(|(_, v)| value_names_paths(v)),
        _ => false,
    }
}

impl Stage {
    /// The evidence a live stage retains for preparing namespace edits
    /// against this snapshot; `None` for a stage composed on its own.
    pub(crate) fn preparation(&self, paths: &PathInterner) -> Option<&PreparationIndex> {
        let index = self.preparation.as_deref()?;
        index
            .composed
            .get_or_init(|| ComposedEvidence::of(paths, &self.prims));
        Some(index)
    }

    /// Starts retaining preparation evidence, sharing `children`, for
    /// every composed prim, found when first lent ([`Self::preparation`]).
    pub(crate) fn retain_preparation(&mut self, children: Arc<ChildIndex>) {
        self.preparation = Some(alloc::boxed::Box::new(PreparationIndex::new(children)));
    }

    /// Replaces `prim`'s retained preparation evidence with its current
    /// composition.
    pub(crate) fn reindex_preparation(&mut self, paths: &PathInterner, prim: PathId) {
        let Self {
            prims, preparation, ..
        } = self;
        if let Some(index) = preparation {
            index.reindex(paths, prim, prims.get(&prim));
        }
    }

    /// Records the layer generations the snapshot reflects (see
    /// [`PreparationIndex::record_generations`]).
    pub(crate) fn record_preparation_generations(
        &mut self,
        store: &dyn LayerStore,
        generations: Option<Vec<(LayerId, Option<u64>)>>,
    ) {
        if let Some(index) = &mut self.preparation {
            index.record_generations(store, generations);
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec, vec::Vec};

    use super::PreparationIndex;
    use crate::{
        InMemoryStore, Layer, LayerId, LayerOffset, ListOp, PathId, PrimSpec, PropertySpec,
        Reference, ReferenceTarget, TargetPath, prim_indexing::ChildIndex,
    };

    /// A prim spec with a relationship targeting `target`.
    fn linking(store: &mut InMemoryStore, target: PathId) -> PrimSpec {
        let rel = store.tokens.intern("rel");
        PrimSpec::def().with_property(
            rel,
            PropertySpec::relationship()
                .with_targets(ListOp::explicit(vec![TargetPath::Prim(target)])),
        )
    }

    /// The generations of `ids` as `store` holds them, for recording.
    fn generations(store: &InMemoryStore, ids: &[LayerId]) -> Vec<(LayerId, Option<u64>)> {
        ids.iter()
            .map(|id| (*id, store.layer(*id).map(Layer::generation)))
            .collect()
    }

    /// Recording generations visits no spec: the link inventory of a layer
    /// is found when a preparation first asks for it, then patched as the
    /// layer changes, and a layer changed before anyone asked is found as
    /// it is then.
    #[test]
    fn link_inventories_are_found_when_first_asked_for() {
        let mut store = InMemoryStore::default();
        let (a, b, c, target) = (
            store.path("/A"),
            store.path("/B"),
            store.path("/C"),
            store.path("/T"),
        );
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(a, linking(&mut store, target));
        layer.insert_prim(target, PrimSpec::def());
        store.insert_layer(layer);
        let mut index = PreparationIndex::new(Arc::new(ChildIndex::default()));
        index.record_generations(&store, Some(generations(&store, &[LayerId(1)])));
        assert!(
            index
                .links
                .get_mut(&LayerId(1))
                .and_then(|links| links.slots.get_mut())
                .is_none(),
            "not found while recording"
        );

        // Changed before anyone asked: found as the layer is now.
        let spec = linking(&mut store, target);
        store.layer_mut(LayerId(1)).unwrap().insert_prim(b, spec);
        index.record_generations(&store, Some(generations(&store, &[LayerId(1)])));
        let layer = store.layer(LayerId(1)).unwrap();
        assert_eq!(index.links(layer), Some(&[a, b].into_iter().collect()));

        // Changed after: patched from the layer's history.
        let spec = linking(&mut store, target);
        store.layer_mut(LayerId(1)).unwrap().insert_prim(c, spec);
        store
            .layer_mut(LayerId(1))
            .unwrap()
            .insert_prim(a, PrimSpec::def());
        index.record_generations(&store, Some(generations(&store, &[LayerId(1)])));
        let layer = store.layer(LayerId(1)).unwrap();
        assert_eq!(index.links(layer), Some(&[b, c].into_iter().collect()));
        assert_eq!(index.links(layer), Some(&super::scan_links(layer).paths));
    }

    /// The arc summary keeps the layers resolved arcs name, outside variant
    /// branches and in them, and marks the slots of arcs the store
    /// resolves, as the layer changes.
    #[test]
    fn arc_summaries_follow_their_layer() {
        let mut store = InMemoryStore::default();
        let (a, b, target) = (store.path("/A"), store.path("/B"), store.path("/T"));
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            a,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), target)),
        );
        store.insert_layer(layer);
        let mut index = PreparationIndex::new(Arc::new(ChildIndex::default()));
        index.record_generations(&store, Some(generations(&store, &[LayerId(1)])));
        let arcs = index.arcs(store.layer(LayerId(1)).unwrap()).unwrap();
        assert_eq!(arcs.layers(), [LayerId(2)]);
        assert_eq!(arcs.main_layers(), [LayerId(2)]);
        assert!(arcs.unresolved().is_empty());
        assert_eq!(arcs.naming(&[LayerId(2)]).collect::<Vec<_>>(), [a]);
        assert_eq!(arcs.naming(&[LayerId(3)]).count(), 0);

        // A deferred payload, then the reference removed: patched.
        let deferred = Reference::deferred(
            LayerId(1),
            "asset.usda",
            ReferenceTarget::DefaultPrim,
            LayerOffset::default(),
        );
        let spec = PrimSpec::def().with_payload(deferred);
        store.layer_mut(LayerId(1)).unwrap().insert_prim(b, spec);
        store
            .layer_mut(LayerId(1))
            .unwrap()
            .insert_prim(a, PrimSpec::def());
        index.record_generations(&store, Some(generations(&store, &[LayerId(1)])));
        let layer = store.layer(LayerId(1)).unwrap();
        let arcs = index.arcs(layer).unwrap();
        assert!(arcs.layers().is_empty());
        assert_eq!(arcs.unresolved(), [b]);
        assert_eq!(index.link_slots(layer), Some(&super::scan_links(layer)));
    }
}
