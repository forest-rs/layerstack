// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Layer stack gathering.
//!
//! A [`LayerStack`] is an ordered list of layers from strongest to weakest,
//! gathered recursively by following `sublayers`.
//!
//! Spec: AOUSD Core §9 (Layer stacks).

use alloc::{string::String, vec::Vec};

use hashbrown::HashSet;

use crate::{
    composition_error::{CompositionError, ExpressionContext, SublayerCycle, UnresolvedSublayer},
    doc::{LayerId, LayerOffset, LayerStore, SublayerEntry},
    expression_variables::{VariableReads, evaluate, expression_error},
};

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
    /// A sublayer that would form a cycle, or whose asset path could not be
    /// resolved, is ignored; composition reports it as a [`SublayerCycle`]
    /// or an [`UnresolvedSublayer`] through
    /// [`Stage::composition_errors`](crate::Stage::composition_errors).
    ///
    /// A sublayer asset path that is a variable expression
    /// ([`SublayerEntry::is_expression`]) is evaluated with the expression
    /// variables of `root` ([`crate::Layer::expression_variables`]),
    /// whichever layer of the stack authors it, and resolved relative to
    /// that layer ([`LayerStore::asset_layer`]); one that evaluates to
    /// nothing is skipped.
    #[must_use]
    pub fn gather(store: &dyn LayerStore, root: LayerId) -> Self {
        Self::gather_reporting(store, root, &mut Vec::new())
    }

    /// Gathers the layer stack rooted at `root`, appending an error for
    /// each sublayer it ignores to `errors`: a [`SublayerCycle`] or an
    /// [`UnresolvedSublayer`].
    ///
    /// A sublayer forms a cycle when it is already on the chain of sublayers
    /// from `root` to the layer that names it. A layer repeated elsewhere in
    /// the hierarchy is not a cycle and appears once per occurrence. A
    /// sublayer whose asset path could not be resolved
    /// ([`SublayerEntry::is_unresolved`]) contributes no layer; the rest of
    /// the stack is gathered as usual.
    ///
    /// Spec: AOUSD Core §10.3.1 (a sublayer that would form a cycle is a
    /// composition error and is ignored), §10.6. OpenUSD reports the two as
    /// `PcpErrorSublayerCycle` and `PcpErrorInvalidSublayerPath`
    /// (`PcpLayerStack::_BuildLayerStack`, `pxr/usd/pcp/layerStack.cpp`).
    ///
    /// [`SublayerEntry::is_unresolved`]: crate::SublayerEntry::is_unresolved
    pub(crate) fn gather_reporting(
        store: &dyn LayerStore,
        root: LayerId,
        errors: &mut Vec<CompositionError>,
    ) -> Self {
        Self::gather_recording(store, root, errors, None)
    }

    /// [`LayerStack::gather_reporting`], recording the expression
    /// variables that sublayer asset paths read in `reads`.
    ///
    /// A sublayer asset path that is a variable expression
    /// ([`SublayerEntry::is_expression`]) is evaluated with the expression
    /// variables of `root`, whichever layer of the stack authors it, and
    /// resolved relative to that layer ([`LayerStore::asset_layer`]). One
    /// that evaluates to nothing is skipped, one that fails to evaluate is
    /// reported as a [`CompositionError::VariableExpressionError`], and one
    /// that does not resolve as an [`UnresolvedSublayer`].
    ///
    /// OpenUSD: `PcpLayerStack::_BuildLayerStack`
    /// (`pxr/usd/pcp/layerStack.cpp`), which evaluates with the variables
    /// of the layer stack's root (and session) layer. A layer stack reached
    /// through a reference also takes variables from the referencing layer
    /// stack there; its sublayers here see only its root's.
    ///
    /// [`SublayerEntry::is_expression`]: crate::SublayerEntry::is_expression
    pub(crate) fn gather_recording(
        store: &dyn LayerStore,
        root: LayerId,
        errors: &mut Vec<CompositionError>,
        mut reads: Option<&mut VariableReads>,
    ) -> Self {
        struct Gather<'a, 'r> {
            store: &'a dyn LayerStore,
            root: LayerId,
            visiting: HashSet<LayerId>,
            layers: Vec<LayerId>,
            offsets: Vec<LayerOffset>,
            errors: &'a mut Vec<CompositionError>,
            reads: Option<&'r mut VariableReads>,
        }

        impl Gather<'_, '_> {
            /// The layer `sub`, a sublayer of `id`, resolves to, or `None`
            /// after reporting why it does not.
            fn sublayer(&mut self, id: LayerId, sub: &SublayerEntry) -> Option<LayerId> {
                let asset = sub.asset.as_deref().unwrap_or_default();
                if sub.is_unresolved() && sub.is_expression() {
                    let evaluated =
                        evaluate(self.store, &[self.root], asset, self.reads.as_deref_mut());
                    let path = match evaluated {
                        Ok(path) => path?,
                        Err(error) => {
                            self.errors.push(expression_error(
                                ExpressionContext::Sublayer,
                                id,
                                None,
                                asset,
                                error,
                            ));
                            return None;
                        }
                    };
                    let layer = self.store.asset_layer(id, &path);
                    if layer.is_none() {
                        self.errors.push(CompositionError::UnresolvedSublayer(
                            UnresolvedSublayer {
                                layer: id,
                                asset: path,
                            },
                        ));
                    }
                    return layer;
                }
                if sub.is_unresolved() {
                    self.errors
                        .push(CompositionError::UnresolvedSublayer(UnresolvedSublayer {
                            layer: id,
                            asset: String::from(asset),
                        }));
                    return None;
                }
                Some(sub.layer)
            }

            fn visit(&mut self, id: LayerId, accumulated: LayerOffset) {
                self.visiting.insert(id);
                self.layers.push(id);
                self.offsets.push(accumulated);
                if let Some(layer) = self.store.layer(id) {
                    for sub in &layer.sublayers {
                        let Some(sublayer) = self.sublayer(id, sub) else {
                            continue;
                        };
                        if self.visiting.contains(&sublayer) {
                            self.errors
                                .push(CompositionError::SublayerCycle(SublayerCycle {
                                    layer: id,
                                    sublayer,
                                }));
                            continue;
                        }
                        self.visit(sublayer, accumulated.compose(sub.offset));
                    }
                }
                self.visiting.remove(&id);
            }
        }

        let mut gather = Gather {
            store,
            root,
            visiting: HashSet::new(),
            layers: Vec::new(),
            offsets: Vec::new(),
            errors,
            reads: reads.take(),
        };
        gather.visit(root, LayerOffset::IDENTITY);
        Self {
            layers: gather.layers,
            offsets: gather.offsets,
        }
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
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });
        store.insert_layer(Layer {
            id: LayerId(2),
            sublayers: subs(&[3]),
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });
        store.insert_layer(Layer {
            id: LayerId(3),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
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
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });
        store.insert_layer(Layer {
            id: LayerId(2),
            sublayers: subs(&[3]),
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });
        store.insert_layer(Layer {
            id: LayerId(3),
            sublayers: subs(&[2]),
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });

        let mut cycles = Vec::new();
        let stack = LayerStack::gather_reporting(&store, LayerId(1), &mut cycles);
        assert_eq!(stack.layers, vec![LayerId(1), LayerId(2), LayerId(3)]);
        assert_eq!(
            cycles,
            vec![CompositionError::SublayerCycle(SublayerCycle {
                layer: LayerId(3),
                sublayer: LayerId(2),
            })]
        );
    }

    /// An unresolved sublayer contributes no layer and is reported; the
    /// sublayers around it, and their offsets, are kept.
    #[test]
    fn unresolved_sublayers_are_reported_and_skipped() {
        let mut store = InMemoryStore::default();
        let offset = LayerOffset {
            offset: 5.0,
            scale: 1.0,
        };
        let mut root = Layer::new(LayerId(1));
        root.sublayers = vec![
            SublayerEntry::unresolved("./missing.usda", offset),
            SublayerEntry::with_offset(LayerId(2), offset),
        ];
        store.insert_layer(root);
        store.insert_layer(Layer::new(LayerId(2)));

        let mut errors = Vec::new();
        let stack = LayerStack::gather_reporting(&store, LayerId(1), &mut errors);
        assert_eq!(stack.layers, [LayerId(1), LayerId(2)]);
        assert_eq!(stack.offsets, [LayerOffset::IDENTITY, offset]);
        assert_eq!(
            errors,
            [CompositionError::UnresolvedSublayer(UnresolvedSublayer {
                layer: LayerId(1),
                asset: "./missing.usda".into(),
            })]
        );
    }

    #[test]
    fn duplicate_sublayers_are_not_cycles() {
        // A layer reached along two sibling paths is repeated, not cyclic.
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer {
            id: LayerId(1),
            sublayers: subs(&[2, 2]),
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });
        store.insert_layer(Layer {
            id: LayerId(2),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });

        let mut cycles = Vec::new();
        let stack = LayerStack::gather_reporting(&store, LayerId(1), &mut cycles);
        assert_eq!(stack.layers, vec![LayerId(1), LayerId(2), LayerId(2)]);
        assert!(cycles.is_empty());
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
                asset: None,
            }],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });
        store.insert_layer(Layer {
            id: LayerId(2),
            sublayers: vec![SublayerEntry {
                layer: LayerId(3),
                offset: LayerOffset {
                    offset: 20.0,
                    scale: 1.0,
                },
                asset: None,
            }],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });
        store.insert_layer(Layer {
            id: LayerId(3),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
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
                asset: None,
            }],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });
        store.insert_layer(Layer {
            id: LayerId(2),
            sublayers: vec![SublayerEntry {
                layer: LayerId(3),
                offset: LayerOffset {
                    offset: 5.0,
                    scale: 3.0,
                },
                asset: None,
            }],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        });
        store.insert_layer(Layer {
            id: LayerId(3),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
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
