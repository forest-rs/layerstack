// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The layer stacks composition reads, computed once each.
//!
//! A layer stack is identified by its root layers and the expression
//! variables it is reached with: one root layer reached from two contexts
//! whose variables differ is two layer stacks. Each is gathered once, with
//! its relocations, and shared by every node that reads it.
//!
//! Spec: AOUSD Core §10.3.1 (sublayers), §10.3.2.6 (relocates are computed
//! per layer stack). OpenUSD: `PcpLayerStack`, `PcpLayerStackRegistry`
//! (`pxr/usd/pcp/layerStack.cpp`).

use alloc::{rc::Rc, sync::Arc, vec::Vec};

use hashbrown::HashMap;

use crate::{
    composition_error::CompositionError,
    doc::{LayerId, LayerOffset, LayerStore},
    expression_variables::{EXPRESSION_VARIABLES, VariableReads},
    layer_stack::{LayerStack, LayerStackIdentifier},
    path::PathId,
    relocates::RelocationTable,
};

/// Identifies a computed layer stack within one composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct StackId(pub(crate) u32);

/// One computed layer stack.
#[derive(Debug)]
pub(crate) struct ComputedStack {
    pub(crate) identifier: LayerStackIdentifier,
    /// The root layers of the layer stacks whose expression variables this
    /// one composes, outermost first, ending with its own.
    pub(crate) chain: Rc<[LayerStackIdentifier]>,
    /// The resident layers, strongest first, and their offsets.
    pub(crate) layers: Vec<LayerId>,
    pub(crate) offsets: Vec<LayerOffset>,
    /// `layers` with their offsets, shared by the stage's nodes of this
    /// layer stack ([`crate::prim_index_graph::PrimNode::stack_layers`]).
    pub(crate) layer_offsets: Arc<[(LayerId, LayerOffset)]>,
    /// Valid relocations, source to target and back.
    pub(crate) relocations: RelocationTable,
    /// Relocation sources mapped to their targets, each source traced back
    /// through the relocations of its ancestors to where it was authored
    /// (OpenUSD's combined relocates map).
    pub(crate) combined_source_to_target: Vec<(PathId, Option<PathId>)>,
}

impl ComputedStack {
    pub(crate) fn has_relocates(&self) -> bool {
        !self.relocations.is_empty()
    }

    /// The offset of the layer at `strength` within the stack.
    pub(crate) fn offset_at(&self, strength: usize) -> LayerOffset {
        self.offsets
            .get(strength)
            .copied()
            .unwrap_or(LayerOffset::IDENTITY)
    }
}

/// Every layer stack computed so far.
#[derive(Debug, Default)]
pub(crate) struct LayerStacks {
    stacks: Vec<ComputedStack>,
    by_key: HashMap<(LayerStackIdentifier, Rc<[LayerStackIdentifier]>), StackId>,
    /// Errors found gathering layer stacks.
    pub(crate) errors: Vec<CompositionError>,
    /// Expression variables sublayer paths read.
    pub(crate) reads: VariableReads,
}

impl LayerStacks {
    pub(crate) fn get(&self, id: StackId) -> &ComputedStack {
        &self.stacks[id.0 as usize]
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &ComputedStack> {
        self.stacks.iter()
    }

    /// The layer stack rooted at `identifier`, reached through the layer
    /// stacks of `outer`'s chain (`None` for the stage's own).
    pub(crate) fn find_or_compute(
        &mut self,
        store: &dyn LayerStore,
        identifier: LayerStackIdentifier,
        outer: Option<StackId>,
    ) -> StackId {
        let chain = self.variable_chain(store, identifier, outer);
        if let Some(&id) = self.by_key.get(&(identifier, chain.clone())) {
            return id;
        }
        let mut full_chain: Vec<LayerStackIdentifier> = chain.to_vec();
        if full_chain.last() != Some(&identifier) {
            full_chain.push(identifier);
        }
        let stack = LayerStack::gather_recording(
            store,
            &full_chain,
            &mut self.errors,
            Some(&mut self.reads),
        );
        let relocations = RelocationTable::compute(store, &stack, &mut self.errors);
        let combined_source_to_target = combined_relocations(store, &relocations);
        let (layers, offsets): (Vec<_>, Vec<_>) = stack
            .layers
            .iter()
            .copied()
            .zip(stack.offsets.iter().copied())
            .filter(|(layer, _)| store.layer(*layer).is_some())
            .unzip();
        let layer_offsets = layers
            .iter()
            .copied()
            .zip(offsets.iter().copied())
            .collect();
        let id = StackId(u32::try_from(self.stacks.len()).expect("layer stack count"));
        self.stacks.push(ComputedStack {
            identifier,
            chain: chain.clone(),
            layers,
            offsets,
            layer_offsets,
            relocations,
            combined_source_to_target,
        });
        self.by_key.insert((identifier, chain), id);
        id
    }

    /// The chain of layer stacks whose expression variables a stack rooted
    /// at `identifier` composes when reached from `outer`: only stacks
    /// whose root layers author variables matter, so when none does, every
    /// context shares one stack.
    fn variable_chain(
        &self,
        store: &dyn LayerStore,
        identifier: LayerStackIdentifier,
        outer: Option<StackId>,
    ) -> Rc<[LayerStackIdentifier]> {
        let Some(field) = store.tokens().lookup(EXPRESSION_VARIABLES) else {
            return Rc::from([]);
        };
        let authors = |stack: &LayerStackIdentifier| {
            stack.variable_layers().any(|layer| {
                store
                    .layer(layer)
                    .is_some_and(|l| l.metadata(field).is_some())
            })
        };
        let mut chain: Vec<LayerStackIdentifier> = outer
            .map(|outer| self.get(outer).chain.to_vec())
            .unwrap_or_default();
        if authors(&identifier) {
            chain.push(identifier);
        }
        Rc::from(chain)
    }
}

/// Each valid relocation source traced back through the relocations of its
/// ancestors to where it was authored, with its target: the source
/// origin, found by moving the source from the target of the nearest
/// relocation at or above it to that relocation's own origin.
/// OpenUSD: `_ComputeSourceOriginForTargetIfNeeded`
/// (`pxr/usd/pcp/layerStack.cpp`).
fn combined_relocations(
    store: &dyn LayerStore,
    table: &RelocationTable,
) -> Vec<(PathId, Option<PathId>)> {
    use crate::interner::TokenId;
    if table.is_empty() {
        return Vec::new();
    }
    let paths = store.paths();
    let relocates: Vec<(Vec<TokenId>, Option<Vec<TokenId>>)> = table
        .iter()
        .map(|r| {
            (
                paths.resolve(r.source).segments().to_vec(),
                r.target.map(|t| paths.resolve(t).segments().to_vec()),
            )
        })
        .collect();
    let mut origins: Vec<Option<Vec<TokenId>>> = alloc::vec![None; relocates.len()];
    fn origin_of(
        at: usize,
        relocates: &[(Vec<TokenId>, Option<Vec<TokenId>>)],
        origins: &mut [Option<Vec<TokenId>>],
    ) -> Vec<TokenId> {
        if let Some(origin) = &origins[at] {
            return origin.clone();
        }
        let source = relocates[at].0.clone();
        // Guards against re-entering this relocate through a cycle.
        origins[at] = Some(source.clone());
        let nearest = (1..=source.len()).rev().find_map(|len| {
            let ancestor = &source[..len];
            relocates
                .iter()
                .position(|(_, target)| target.as_deref() == Some(ancestor))
        });
        let origin = match nearest {
            Some(other) => {
                let other_origin = origin_of(other, relocates, origins);
                let target_len = relocates[other].1.as_ref().map_or(0, Vec::len);
                let mut moved = other_origin;
                moved.extend_from_slice(&source[target_len..]);
                moved
            }
            None => source,
        };
        origins[at] = Some(origin.clone());
        origin
    }
    let mut out = Vec::with_capacity(relocates.len());
    for at in 0..relocates.len() {
        let origin = origin_of(at, &relocates, &mut origins);
        let target = relocates[at]
            .1
            .as_ref()
            .map(|t| store.paths().intern_segments(t));
        out.push((store.paths().intern_segments(&origin), target));
    }
    out.sort_unstable();
    out
}
