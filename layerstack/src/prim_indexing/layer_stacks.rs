// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The layer stacks composition reads, computed once each.
//!
//! A layer stack is identified by its root layers and the expression
//! variables it is reached with: one root layer reached from two contexts
//! whose variables differ is two layer stacks. Each is gathered once, with
//! its relocations, and shared by every node that reads it, from any
//! number of threads: a stack is read by id without locking, and two
//! threads gathering one stack at once keep the first one recorded.
//!
//! Spec: AOUSD Core §10.3.1 (sublayers), §10.3.2.6 (relocates are computed
//! per layer stack). OpenUSD: `PcpLayerStack`, `PcpLayerStackRegistry`
//! (`pxr/usd/pcp/layerStack.cpp`).

use crate::sort;
use alloc::{boxed::Box, collections::BTreeSet, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

use hashbrown::HashMap;

use crate::{
    composition_error::CompositionError,
    doc::{Layer, LayerId, LayerOffset, LayerStore},
    expression_variables::{EXPRESSION_VARIABLES, VariableReads},
    layer_stack::{LayerStack, LayerStackIdentifier, SublayerRequest, awaits},
    path::PathId,
    prim_indexing::site::ChildListing,
    relocates::RelocationTable,
    sync::{ReadLock, Slots},
};

/// Identifies a computed layer stack within one composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct StackId(pub(crate) u32);

/// One computed layer stack, of layers the store `'s` holds.
pub(crate) struct ComputedStack<'s> {
    pub(crate) identifier: LayerStackIdentifier,
    /// The root layers of the layer stacks whose expression variables this
    /// one composes, outermost first, ending with its own.
    pub(crate) chain: Arc<[LayerStackIdentifier]>,
    /// The resident layers, strongest first, and their offsets.
    pub(crate) layers: Vec<LayerId>,
    /// Each of `layers`, as the store holds it: the store does not change
    /// while a composition reads it, so its layers are looked up once.
    pub(crate) resolved: Box<[&'s Layer]>,
    pub(crate) offsets: Vec<LayerOffset>,
    /// `layers` with their offsets, shared by the stage's nodes of this
    /// layer stack ([`crate::prim_index_graph::PrimNode::layers`]).
    pub(crate) layer_offsets: Arc<[(LayerId, LayerOffset)]>,
    /// The child listing of each of `layers`, as this composition reads it
    /// ([`super::site::compose_child_names`]).
    pub(crate) children: Box<[ChildListing]>,
    /// Valid relocations, source to target and back.
    pub(crate) relocations: RelocationTable,
    /// Relocation sources mapped to their targets, each source traced back
    /// through the relocations of its ancestors to where it was authored
    /// (OpenUSD's combined relocates map).
    pub(crate) combined_source_to_target: Vec<(PathId, Option<PathId>)>,
    /// Errors found gathering the stack and its relocations.
    pub(crate) errors: Vec<CompositionError>,
    /// Expression variables its sublayer paths read.
    pub(crate) reads: VariableReads,
    /// The sublayer asset paths it names that the store has not resolved,
    /// which the first prim to read the stack asks for.
    pub(crate) requests: Vec<SublayerRequest>,
}

impl core::fmt::Debug for ComputedStack<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ComputedStack")
            .field("identifier", &self.identifier)
            .field("layers", &self.layers)
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

impl ComputedStack<'_> {
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

/// The key a computed layer stack is found by: its root layers and the
/// chain of layer stacks whose expression variables it composes.
type StackKey = (LayerStackIdentifier, Arc<[LayerStackIdentifier]>);

/// Every layer stack computed so far.
#[derive(Default)]
pub(crate) struct LayerStacks<'s> {
    stacks: Slots<ComputedStack<'s>>,
    len: AtomicU32,
    by_key: ReadLock<HashMap<StackKey, StackId>>,
    /// The layers the stage mutes ([`crate::StageOptions::muted_layers`]):
    /// the store hides them, and they are not reported missing.
    muted: Option<&'s BTreeSet<LayerId>>,
}

/// Reads computed layer stacks by id, for a reader borrowing the stacks
/// for less long than their layers live.
pub(crate) trait Stacks {
    /// The stack `id`.
    fn stack(&self, id: StackId) -> &ComputedStack<'_>;
}

impl Stacks for LayerStacks<'_> {
    fn stack(&self, id: StackId) -> &ComputedStack<'_> {
        self.get(id)
    }
}

impl<'s> LayerStacks<'s> {
    /// No stacks yet, for a stage that mutes `muted`.
    pub(crate) fn muting(muted: &'s BTreeSet<LayerId>) -> Self {
        Self {
            muted: Some(muted),
            ..Self::default()
        }
    }

    /// Whether the stage mutes `layer`.
    pub(crate) fn is_muted(&self, layer: LayerId) -> bool {
        self.muted.is_some_and(|muted| muted.contains(&layer))
    }

    pub(crate) fn get(&self, id: StackId) -> &ComputedStack<'s> {
        self.stacks.get(id.0).expect("computed layer stack")
    }

    /// The stacks computed so far, in the order they were recorded.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &ComputedStack<'s>> {
        (0..self.len.load(Ordering::Acquire)).map(|n| self.get(StackId(n)))
    }

    /// The layer stack rooted at `identifier`, reached through the layer
    /// stacks of `outer`'s chain (`None` for the stage's own), or the
    /// layers it reads that the store has not loaded yet
    /// ([`LayerStore::is_layer_pending`]).
    ///
    /// A stack that reads a pending layer is not recorded: it is gathered
    /// again, whole, once asked for after the layer loads.
    pub(crate) fn find_or_compute(
        &self,
        store: &'s dyn LayerStore,
        identifier: LayerStackIdentifier,
        outer: Option<StackId>,
    ) -> Result<StackId, Vec<LayerId>> {
        let key = (identifier, self.variable_chain(store, identifier, outer)?);
        if let Some(id) = self.by_key.read(|by_key| by_key.get(&key).copied()) {
            return Ok(id);
        }
        // Gathered without the lock, so threads gather different stacks at
        // once; of two gathering the same one, the first to record it wins.
        let computed = compute(store, identifier, key.1.clone(), self.muted)?;
        Ok(self.by_key.write(|by_key| {
            if let Some(&id) = by_key.get(&key) {
                return id;
            }
            let id = StackId(self.len.load(Ordering::Relaxed));
            self.stacks.set(id.0, computed);
            self.len.store(id.0 + 1, Ordering::Release);
            by_key.insert(key, id);
            id
        }))
    }

    /// The chain of layer stacks whose expression variables a stack rooted
    /// at `identifier` composes when reached from `outer`: only stacks
    /// whose root layers author variables matter, so when none does, every
    /// context shares one stack. A root layer the store has not loaded yet
    /// leaves the chain unknown: its pending layers are returned instead.
    fn variable_chain(
        &self,
        store: &dyn LayerStore,
        identifier: LayerStackIdentifier,
        outer: Option<StackId>,
    ) -> Result<Arc<[LayerStackIdentifier]>, Vec<LayerId>> {
        let Some(field) = store.tokens().lookup(EXPRESSION_VARIABLES) else {
            return Ok(Arc::from([]));
        };
        let mut authors = false;
        let mut pending = Vec::new();
        for layer in identifier.variable_layers() {
            match store.layer(layer) {
                Some(layer) => authors |= layer.metadata(field).is_some(),
                None if awaits(store, layer) => pending.push(layer),
                None => {}
            }
        }
        if !pending.is_empty() {
            return Err(pending);
        }
        let mut chain: Vec<LayerStackIdentifier> = outer
            .map(|outer| self.get(outer).chain.to_vec())
            .unwrap_or_default();
        if authors {
            chain.push(identifier);
        }
        Ok(Arc::from(chain))
    }
}

/// Gathers the layer stack rooted at `identifier`, reached through the
/// layer stacks of `chain`, and its relocations, or returns the layers it
/// reads that the store has not loaded yet.
fn compute<'s>(
    store: &'s dyn LayerStore,
    identifier: LayerStackIdentifier,
    chain: Arc<[LayerStackIdentifier]>,
    muted: Option<&BTreeSet<LayerId>>,
) -> Result<ComputedStack<'s>, Vec<LayerId>> {
    let mut full_chain: Vec<LayerStackIdentifier> = chain.to_vec();
    if full_chain.last() != Some(&identifier) {
        full_chain.push(identifier);
    }
    let mut errors = Vec::new();
    let mut reads = VariableReads::default();
    let mut stack = LayerStack::gather_recording(store, &full_chain, &mut errors, Some(&mut reads));
    if !stack.pending.is_empty() {
        return Err(core::mem::take(&mut stack.pending));
    }
    // A muted layer is hidden on purpose, not missing.
    if let Some(muted) = muted {
        errors.retain(|error| {
            !matches!(error, CompositionError::MissingLayer(missing) if muted.contains(&missing.layer))
        });
    }
    let requests = core::mem::take(&mut stack.requests);
    let relocations = RelocationTable::compute(store, &stack, &mut errors);
    let combined_source_to_target = combined_relocations(store, &relocations);
    let mut resolved = Vec::with_capacity(stack.layers.len());
    let (layers, offsets): (Vec<_>, Vec<_>) = stack
        .layers
        .iter()
        .copied()
        .zip(stack.offsets.iter().copied())
        .filter(|(layer, _)| {
            let held = store.layer(*layer);
            resolved.extend(held);
            held.is_some()
        })
        .unzip();
    let layer_offsets = layers
        .iter()
        .copied()
        .zip(offsets.iter().copied())
        .collect();
    let children = layers.iter().map(|_| ChildListing::default()).collect();
    Ok(ComputedStack {
        identifier,
        chain,
        children,
        resolved: resolved.into_boxed_slice(),
        layers,
        offsets,
        layer_offsets,
        relocations,
        combined_source_to_target,
        errors,
        reads,
        requests,
    })
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
    sort::sort_unstable(&mut out);
    out
}
