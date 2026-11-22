// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Roll back the source records an incremental read touched if it is rejected.
//! Layers and asset bindings are already staged in the reader's batch.

use super::{Arc, BTreeMap, Catalog, HashMap, LayerId, LayerRecord, PackageCacheStats, Vec};
use alloc::boxed::Box;

#[derive(Clone, Debug, Default)]
pub(super) struct Undo {
    pub(super) records: BTreeMap<LayerId, Option<LayerRecord>>,
    identifiers: HashMap<Arc<str>, Option<LayerId>>,
    pub(super) reservations: Vec<LayerId>,
    #[cfg(feature = "std")]
    retained: BTreeMap<LayerId, Option<layerstack_usdc::RetainedValues>>,
    reserved_at: Option<(layerstack::StoreIdentity, u64)>,
    next: u64,
    work: PackageCacheStats,
}

pub(super) struct ReadTransaction<'a> {
    pub(super) catalog: &'a mut Catalog,
}

impl Catalog {
    pub(super) fn reserve_id(&mut self, id: LayerId) -> bool {
        let inserted = self.reserved.insert(id);
        if inserted && let Some(undo) = &mut self.undo {
            undo.reservations.push(id);
        }
        inserted
    }

    pub(super) fn reservations_current(&self, store: &layerstack::InMemoryStore) -> bool {
        use layerstack::LayerStore;
        self.reserved_at.as_ref().is_some_and(|(identity, count)| {
            *count == store.layer_insertions() && *identity == store.identity()
        })
    }

    pub(super) fn begin_read(&mut self, store: &layerstack::InMemoryStore) -> ReadTransaction<'_> {
        debug_assert!(
            self.undo.is_none(),
            "a catalog read already owns the journal"
        );
        self.undo = Some(Box::new(Undo {
            reserved_at: self.reserved_at.clone(),
            next: self.next,
            work: self.cache_work,
            ..Undo::default()
        }));
        if !self.reservations_current(store) {
            use layerstack::LayerStore;
            for id in store.layer_ids() {
                self.reserve_id(id);
            }
            self.reserved_at = Some((store.identity(), store.layer_insertions()));
        }
        ReadTransaction { catalog: self }
    }

    pub(super) fn identify_layer(&mut self, identifier: Arc<str>, id: LayerId) {
        if let Some(undo) = &mut self.undo {
            undo.identifiers
                .entry(Arc::clone(&identifier))
                .or_insert_with(|| self.ids.get(&identifier).copied());
        }
        self.ids.insert(identifier, id);
    }

    #[cfg(feature = "std")]
    pub(super) fn retain_values(&mut self, id: LayerId, values: layerstack_usdc::RetainedValues) {
        let previous = self.retained_values.insert(id, values);
        if let Some(undo) = &mut self.undo {
            undo.retained.entry(id).or_insert(previous);
        }
    }
}

impl ReadTransaction<'_> {
    pub(super) fn publish(
        self,
        batch: super::ReadBatch,
        store: &mut layerstack::InMemoryStore,
    ) -> super::LoadReport {
        self.catalog.undo = None;
        batch.commit(store, self.catalog)
    }
}

impl Drop for ReadTransaction<'_> {
    fn drop(&mut self) {
        let catalog = &mut self.catalog;
        let Some(undo) = catalog.undo.take() else {
            return;
        };
        catalog.reserved_at = undo.reserved_at;
        catalog.next = undo.next;
        catalog.cache_work = undo.work;
        for id in undo.reservations {
            catalog.reserved.remove(&id);
        }
        for (name, previous) in undo.identifiers {
            if let Some(id) = previous {
                catalog.ids.insert(name, id);
            } else {
                catalog.ids.remove(&name);
            }
        }
        for (id, previous) in undo.records {
            if let Some(record) = previous {
                catalog.layers.insert(id, record);
            } else {
                catalog.layers.remove(&id);
            }
        }
        #[cfg(feature = "std")]
        for (id, previous) in undo.retained {
            if let Some(values) = previous {
                catalog.retained_values.insert(id, values);
            } else {
                catalog.retained_values.remove(&id);
            }
        }
    }
}
