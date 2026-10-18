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
//! - the layers each prim's opinions come from ([`Stage::prim_stack`]);
//! - for each layer, the prim specs that author a path or a composition
//!   arc ([`slot_names_paths`]), as of a generation of the layer, found by
//!   the first preparation that asks for them: an open that never edits
//!   namespace never visits every spec for them.
//!
//! A stage composed by [`Stage::compose`] keeps none of this, and
//! preparation then composes and scans it as before.
//!
//! Spec: AOUSD Core §8 (namespace), §10 (composition arcs).

use alloc::{sync::Arc, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::{
    doc::{FieldEntry, FieldValue, Layer, LayerId, LayerStore, PrimSpec, Value},
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
    slots: Memo<HashSet<PathId>>,
}

impl PreparationIndex {
    /// An empty index sharing `children` with its live stage.
    pub(crate) fn new(children: Arc<ChildIndex>) -> Self {
        Self {
            generations: None,
            children,
            links: HashMap::new(),
            prim_stack_layers: HashMap::new(),
            stack_layer_counts: HashMap::new(),
            prim_node_sites: HashMap::new(),
            node_sites: HashMap::new(),
            nodes_beneath: HashMap::new(),
        }
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
                        slots.insert(path);
                    } else {
                        slots.remove(&path);
                    }
                }
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
        self.links
            .get(&layer.id)
            .filter(|links| links.generation == layer.generation())
            .map(|links| links.slots.get_or_init(|| scan_links(layer)))
    }

    /// Every layer some composed prim's opinions come from.
    pub(crate) fn stack_layers(&self) -> impl Iterator<Item = LayerId> + '_ {
        self.stack_layer_counts.keys().copied()
    }

    /// The composed prims with a non-local node at a site at or beneath
    /// `path`.
    pub(crate) fn prims_with_nodes_beneath(
        &self,
        path: PathId,
    ) -> impl Iterator<Item = PathId> + '_ {
        self.nodes_beneath.get(&path).into_iter().flatten().copied()
    }

    /// The composed prims with a non-local node at the site `path`.
    pub(crate) fn prims_with_nodes_at(&self, path: PathId) -> impl Iterator<Item = PathId> + '_ {
        self.node_sites.get(&path).into_iter().flatten().copied()
    }

    /// Replaces `prim`'s entries with those of its composed `index`, or
    /// removes them when it is no longer composed.
    pub(crate) fn reindex(
        &mut self,
        paths: &PathInterner,
        prim: PathId,
        index: Option<&PrimIndex>,
    ) {
        let mut layers: Vec<LayerId> = index
            .map(|index| index.sources.iter().map(|key| key.layer_id).collect())
            .unwrap_or_default();
        layers.sort_unstable();
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
        sites.sort_unstable();
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
        assert_eq!(self.prim_stack_layers, fresh.prim_stack_layers);
        assert_eq!(self.stack_layer_counts, fresh.stack_layer_counts);
        assert_eq!(self.prim_node_sites, fresh.prim_node_sites);
        assert_eq!(self.node_sites, fresh.node_sites);
        assert_eq!(self.nodes_beneath, fresh.nodes_beneath);
        for (id, links) in &self.links {
            if let Some(layer) = store.layer(*id) {
                assert_eq!(links.generation, layer.generation());
                assert_eq!(
                    self.links(layer),
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

/// The prim slots of `layer` that author paths ([`slot_names_paths`]).
fn scan_links(layer: &Layer) -> HashSet<PathId> {
    layer
        .prims
        .keys()
        .chain(layer.variant_prims.keys())
        .copied()
        .filter(|path| slot_names_paths(layer, *path))
        .collect()
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
        None => alloc::borrow::Cow::Owned(scan_links(layer)),
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
        || !spec.inherits.is_empty()
        || !spec.specializes.is_empty()
        || !spec.variant_sets.is_empty()
        || fields_name_paths(&spec.fields)
        || spec.properties.iter().any(property_names_paths)
}

fn property_names_paths(entry: &PropertyEntry) -> bool {
    let spec = &entry.spec;
    spec.targets.is_some()
        || spec.default.as_ref().is_some_and(value_names_paths)
        || spec
            .time_samples
            .as_ref()
            .is_some_and(|samples| samples.as_slice().iter().any(|(_, v)| value_names_paths(v)))
        || fields_name_paths(&spec.metadata)
}

fn fields_name_paths(fields: &[FieldEntry]) -> bool {
    fields.iter().any(|field| match &field.value {
        FieldValue::Value(value) => value_names_paths(value),
        FieldValue::PathListOp(_) => true,
        _ => false,
    })
}

fn value_names_paths(value: &Value) -> bool {
    match value {
        Value::PathExpression(_) => true,
        Value::Array(values) => values.iter().any(value_names_paths),
        Value::Dictionary(values) => values.iter().any(|(_, v)| value_names_paths(v)),
        _ => false,
    }
}

impl Stage {
    /// Whether the snapshot composes an instance.
    pub(crate) fn composes_instances(&self) -> bool {
        !self.instances.is_empty()
    }

    /// The evidence a live stage retains for preparing namespace edits
    /// against this snapshot; `None` for a stage composed on its own.
    pub(crate) fn preparation(&self) -> Option<&PreparationIndex> {
        self.preparation.as_deref()
    }

    /// Starts retaining preparation evidence, sharing `children`, for
    /// every composed prim.
    pub(crate) fn retain_preparation(&mut self, children: Arc<ChildIndex>, paths: &PathInterner) {
        let mut index = PreparationIndex::new(children);
        for (&prim, prim_index) in &self.prims {
            index.reindex(paths, prim, Some(prim_index));
        }
        self.preparation = Some(alloc::boxed::Box::new(index));
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
        InMemoryStore, Layer, LayerId, LayerStore, ListOp, PathId, PrimSpec, PropertySpec,
        TargetPath, prim_indexing::ChildIndex,
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
        assert_eq!(index.links(layer), Some(&super::scan_links(layer)));
    }
}
