// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Preparation from the evidence a live stage retains
//! ([`crate::stage::preparation`]) rather than from the whole stage.
//!
//! Spec: AOUSD Core §8 (namespace), §10 (composition arcs).

use alloc::borrow::Cow;

use super::*;
use crate::{
    HashSet,
    stage::preparation::{PreparationIndex, layer_links},
};

/// The prim slots of `layer` that author paths, from the first of
/// `indexes` that holds the layer at its generation, otherwise found by
/// visiting every spec.
pub(super) fn links_of<'a>(
    indexes: &[&'a PreparationIndex],
    layer: &Layer,
) -> Cow<'a, HashSet<PathId>> {
    let index = indexes
        .iter()
        .copied()
        .find(|index| index.links(layer).is_some());
    layer_links(index.or(indexes.first().copied()), layer)
}

/// The layers [`reachable_layers`] finds, from a stage's retained opinion
/// layers and the specs of each layer that author arcs.
pub(super) fn reachable_layers(
    store: &dyn LayerStore,
    indexes: &[&PreparationIndex],
    index: &PreparationIndex,
    root: LayerId,
) -> Vec<LayerId> {
    let mut found = Vec::new();
    let mut pending = alloc::vec![root];
    pending.extend(index.stack_layers());
    while let Some(id) = pending.pop() {
        if found.contains(&id) {
            continue;
        }
        let Some(layer) = store.layer(id) else {
            continue;
        };
        found.push(id);
        pending.extend(layer.sublayers.iter().map(|s| s.layer));
        for path in links_of(indexes, layer).iter() {
            for spec in layer
                .prims
                .get(path)
                .into_iter()
                .chain(layer.variant_prims.get(path).into_iter().flatten())
            {
                for refs in [&spec.references, &spec.payloads] {
                    collect_ref_layers(store, refs, &mut pending);
                }
                collect_variant_layers(store, &spec.variant_sets, &mut pending);
            }
        }
    }
    found.sort_unstable();
    found
}
