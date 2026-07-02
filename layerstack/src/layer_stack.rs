// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Layer stack gathering.
//!
//! A [`LayerStack`] is an ordered list of layers from strongest to weakest,
//! gathered recursively by following `sublayers`.
//!
//! Spec: AOUSD Core §9 (Layer stacks).

use alloc::vec::Vec;

use hashbrown::HashSet;

use crate::doc::{LayerId, LayerOffset, LayerStore};

/// An ordered set of layers gathered recursively from sublayers.
///
/// The order is strongest → weakest, and a layer is always stronger than any of
/// its sublayers.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerStack {
    /// Ordered strongest → weakest.
    pub layers: Vec<LayerId>,
    /// Accumulated time offset for each layer in `layers` (same length,
    /// parallel indexing). The root layer always has [`LayerOffset::IDENTITY`].
    ///
    /// Spec: §12.3.2.1 (sublayer offsets compose when nested).
    pub offsets: Vec<LayerOffset>,
}

impl LayerStack {
    /// Gathers the layer stack rooted at `root`.
    ///
    /// Cycles are treated as non-fatal and are ignored for the purposes of
    /// gathering.
    #[must_use]
    pub fn gather(store: &dyn LayerStore, root: LayerId) -> Self {
        fn visit(
            store: &dyn LayerStore,
            id: LayerId,
            accumulated: LayerOffset,
            visiting: &mut HashSet<LayerId>,
            out: &mut Vec<LayerId>,
            offsets: &mut Vec<LayerOffset>,
        ) {
            if !visiting.insert(id) {
                return;
            }

            out.push(id);
            offsets.push(accumulated);
            if let Some(layer) = store.layer(id) {
                for sub in &layer.sublayers {
                    let child_offset = accumulated.compose(sub.offset);
                    visit(store, sub.layer, child_offset, visiting, out, offsets);
                }
            }

            visiting.remove(&id);
        }

        let mut layers = Vec::new();
        let mut offsets = Vec::new();
        visit(
            store,
            root,
            LayerOffset::IDENTITY,
            &mut HashSet::new(),
            &mut layers,
            &mut offsets,
        );
        Self { layers, offsets }
    }

    /// Returns the accumulated offset for a layer at the given index in the stack.
    #[must_use]
    pub fn offset_at(&self, index: usize) -> LayerOffset {
        self.offsets
            .get(index)
            .copied()
            .unwrap_or(LayerOffset::IDENTITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashMap;
    use crate::doc::{InMemoryStore, Layer, SublayerEntry};
    use alloc::vec;

    /// Shorthand for sublayer entries without offsets.
    fn subs(ids: &[u64]) -> Vec<SublayerEntry> {
        ids.iter()
            .map(|&id| SublayerEntry::new(LayerId(id)))
            .collect()
    }

    #[test]
    fn duplicates_are_preserved_in_layer_stack() {
        // Mirrors the idea in the supplemental composition test
        // `BasicDuplicateSublayer_root`: the same layer can appear multiple
        // times in the layer stack.
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer {
            id: LayerId(1),
            sublayers: subs(&[2, 3]),
            default_prim: None,
            prims: HashMap::new(),
        });
        store.insert_layer(Layer {
            id: LayerId(2),
            sublayers: subs(&[3]),
            default_prim: None,
            prims: HashMap::new(),
        });
        store.insert_layer(Layer {
            id: LayerId(3),
            sublayers: vec![],
            default_prim: None,
            prims: HashMap::new(),
        });

        let stack = LayerStack::gather(&store, LayerId(1));
        assert_eq!(
            stack.layers,
            vec![LayerId(1), LayerId(2), LayerId(3), LayerId(3)]
        );
    }

    #[test]
    fn cycles_do_not_infinite_loop() {
        // Mirrors the idea in the supplemental composition test
        // `ErrorSublayerCycle_root`: cycles are non-fatal and must not loop.
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer {
            id: LayerId(1),
            sublayers: subs(&[2]),
            default_prim: None,
            prims: HashMap::new(),
        });
        store.insert_layer(Layer {
            id: LayerId(2),
            sublayers: subs(&[3]),
            default_prim: None,
            prims: HashMap::new(),
        });
        store.insert_layer(Layer {
            id: LayerId(3),
            sublayers: subs(&[2]),
            default_prim: None,
            prims: HashMap::new(),
        });

        let stack = LayerStack::gather(&store, LayerId(1));
        assert_eq!(stack.layers, vec![LayerId(1), LayerId(2), LayerId(3)]);
    }

    #[test]
    fn sublayer_offsets_accumulate() {
        // Root → (offset=10) → A → (offset=20) → B
        // Root has identity offset, A has offset=10, B has offset=30 (10+20).
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer {
            id: LayerId(1),
            sublayers: vec![SublayerEntry {
                layer: LayerId(2),
                offset: LayerOffset {
                    offset: 10.0,
                    scale: 1.0,
                },
            }],
            default_prim: None,
            prims: HashMap::new(),
        });
        store.insert_layer(Layer {
            id: LayerId(2),
            sublayers: vec![SublayerEntry {
                layer: LayerId(3),
                offset: LayerOffset {
                    offset: 20.0,
                    scale: 1.0,
                },
            }],
            default_prim: None,
            prims: HashMap::new(),
        });
        store.insert_layer(Layer {
            id: LayerId(3),
            sublayers: vec![],
            default_prim: None,
            prims: HashMap::new(),
        });

        let stack = LayerStack::gather(&store, LayerId(1));
        assert_eq!(stack.layers, vec![LayerId(1), LayerId(2), LayerId(3)]);
        assert_eq!(stack.offset_at(0), LayerOffset::IDENTITY);
        assert_eq!(
            stack.offset_at(1),
            LayerOffset {
                offset: 10.0,
                scale: 1.0
            }
        );
        assert_eq!(
            stack.offset_at(2),
            LayerOffset {
                offset: 30.0,
                scale: 1.0
            }
        );
    }

    #[test]
    fn sublayer_offsets_compose_with_scale() {
        // Root → (offset=10, scale=2) → A → (offset=5, scale=3) → B
        // A's accumulated: offset=10, scale=2
        // B's accumulated: compose(10,2; 5,3) = offset=10+2*5=20, scale=2*3=6
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer {
            id: LayerId(1),
            sublayers: vec![SublayerEntry {
                layer: LayerId(2),
                offset: LayerOffset {
                    offset: 10.0,
                    scale: 2.0,
                },
            }],
            default_prim: None,
            prims: HashMap::new(),
        });
        store.insert_layer(Layer {
            id: LayerId(2),
            sublayers: vec![SublayerEntry {
                layer: LayerId(3),
                offset: LayerOffset {
                    offset: 5.0,
                    scale: 3.0,
                },
            }],
            default_prim: None,
            prims: HashMap::new(),
        });
        store.insert_layer(Layer {
            id: LayerId(3),
            sublayers: vec![],
            default_prim: None,
            prims: HashMap::new(),
        });

        let stack = LayerStack::gather(&store, LayerId(1));
        assert_eq!(
            stack.offset_at(2),
            LayerOffset {
                offset: 20.0,
                scale: 6.0
            }
        );
    }
}
