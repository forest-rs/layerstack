// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authoring: atomic, invertible spec edits, written through composition.
//!
//! - An [`EditTarget`] says where stage-level edits go: a layer, a map
//!   from stage paths to spec paths in it (including variant-qualified
//!   ones), and a layer offset. It comes from a layer, a variant branch,
//!   or any node of a prim's index graph.
//! - A [`Transaction`] lists spec edits, each addressed through a target
//!   or by spec path ([`Address`]), and optional preconditions: layer
//!   generations and expected authored values.
//! - Applying a transaction checks its preconditions, validates and
//!   applies every edit or none, and returns the inverse transaction,
//!   which restores each slot only while it holds what was written there.
//!   [`LiveStage::apply`](crate::LiveStage::apply) also recomposes the
//!   prims the edits affect.
//!
//! ```
//! use layerstack::{
//!     ArcKind, EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec,
//!     PropertyType, Reference, StageOptions, Transaction, Value,
//! };
//!
//! // `/World/Rock` references `/Rock` of a rock asset, 10 frames later.
//! let mut store = InMemoryStore::default();
//! let spin = store.tokens.intern("spin");
//! let (asset_rock, rock) = (store.path("/Rock"), store.path("/World/Rock"));
//! let double = PropertyType::new("double", false, Value::Double(0.0));
//! let mut asset = Layer::new(LayerId(2));
//! asset.insert_prim(
//!     asset_rock,
//!     PrimSpec::def().with_property(spin, PropertySpec::typed_attribute(double)),
//! );
//! store.insert_layer(asset);
//! let mut reference = Reference::new(LayerId(2), asset_rock);
//! reference.layer_offset.offset = 10.0;
//! let mut scene = Layer::new(LayerId(1));
//! scene.insert_prim(rock, PrimSpec::def().with_reference(reference));
//! store.insert_layer(scene);
//! let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
//!
//! // Target the reference node: edits of `/World/Rock` go to the asset's
//! // `/Rock`, and stage frame 14 is frame 4 there.
//! let graph = live.stage().explain_prim_graph(rock).unwrap();
//! let (node, _) = graph
//!     .nodes()
//!     .find(|(_, node)| node.arc_kind() == ArcKind::References)
//!     .unwrap();
//! let target = EditTarget::for_node(live.stage(), rock, node).unwrap();
//! let spin_path = store.property_path("/World/Rock.spin");
//! let mut edit = Transaction::new();
//! edit.set_time_sample(target.property(spin_path), 14.0, Value::Double(70.0));
//! let applied = live.apply(&mut store, &edit).unwrap();
//!
//! let asset_spin = store.property_path("/Rock.spin");
//! let samples = |store: &InMemoryStore| {
//!     store.layers[&LayerId(2)].property(asset_spin).unwrap().time_samples.clone()
//! };
//! assert_eq!(samples(&store), Some(vec![(4.0, Value::Double(70.0))]));
//! let at_14 = live.stage().resolve_property_path_at_time(spin_path, 14.0, Default::default());
//! assert_eq!(at_14.unwrap().value, Value::Double(70.0));
//!
//! // The inverse removes the sample it created.
//! live.apply(&mut store, &applied.inverse).unwrap();
//! assert_eq!(samples(&store), None);
//! ```
//!
//! OpenUSD: `UsdEditTarget`, `UsdEditContext` and `UsdStage::SetEditTarget`
//! pick where `UsdAttribute::Set` and the other authoring calls write;
//! `SdfChangeBlock` batches `SdfLayer` / `SdfPrimSpec` edits. Unlike an
//! `SdfChangeBlock`, a transaction rolls back when an edit fails, and it
//! records its inverse.
//!
//! Spec: AOUSD Core §7 (scene description), §8 (spec paths), §10.3.1.1 and
//! §12.3.2.1 (layer offsets), §10.5 (variant selection).

mod apply;
mod error;
mod same;
mod spec;
mod target;
mod transaction;

#[cfg(test)]
mod tests;

use alloc::vec::Vec;

use crate::path::PathId;

pub use error::{EditError, Rejection, Slot};
pub use target::{Address, EditTarget};
pub use transaction::Transaction;

pub(crate) use apply::{PropertyValueEdit, SourceNamespace, apply};

/// What [`LiveStage::apply`](crate::LiveStage::apply) did.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    /// The transaction that undoes the applied one (see [`Transaction`]).
    pub inverse: Transaction,
    /// Composed existence and invalidation changes caused by the transaction.
    pub changes: Changes,
    /// The composed prims updated by this transaction. Existing attribute
    /// value edits may refresh opinions without rebuilding prim graphs;
    /// eligible local structural edits replace only their subtrees and
    /// boundary child lists. Use [`Self::changes`] to invalidate consumers;
    /// this list includes unchanged survivors after a full rebuild.
    pub recomposed: Vec<PathId>,
}

/// Composed changes made by one successful live transaction.
///
/// Created and removed are exact inventories. Resynced paths are subtree roots:
/// every descendant is invalidated, including created and removed descendants.
/// Info-only paths invalidate just themselves and never lie beneath a resync.
/// A changed opinion may be masked by a stronger one, so info-only notices do
/// not assert that the resolved value differs. A conservative full rebuild
/// reports the pseudo-root as resynced, rather than every survivor as changed.
/// Structural invalidation has prim granularity: a property declaration or
/// applied-schema change conservatively resyncs its owning prim. Info-only
/// reports can include complete property-field inventories in `property_changes`.
/// External opinion notifications lack edit details and conservatively resync
/// their affected prims, including other prims updated in the same batch.
/// Every list is sorted by [`PathId`] and contains no duplicates.
///
/// ```
/// use layerstack::{EditTarget, InMemoryStore, Layer, LayerId, LiveStage,
///     PrimSpec, Specifier, StageOptions, Transaction};
///
/// let mut store = InMemoryStore::default();
/// let layer = LayerId(1);
/// let world = store.path("/World");
/// let mut source = Layer::new(layer);
/// source.insert_prim(world, PrimSpec::def());
/// store.insert_layer(source);
/// let mut live = LiveStage::compose(&mut store, layer, StageOptions::default());
/// let rock = store.path("/World/Rock");
/// let mut edit = Transaction::new();
/// edit.create_prim(EditTarget::for_layer(layer).prim(rock), Specifier::Def, None);
/// let added = live.apply(&mut store, &edit).unwrap();
/// assert_eq!(added.changes.created, [rock]);
/// assert_eq!(added.changes.resynced, [rock]);
/// let undone = live.apply(&mut store, &added.inverse).unwrap();
/// assert_eq!(undone.changes.removed, [rock]);
/// ```
///
/// OpenUSD: `UsdNotice::ObjectsChanged::GetResyncedPaths` and
/// `GetChangedInfoOnlyPaths` use the same subtree versus exact-path distinction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Prim paths present after the transaction but absent before it.
    pub created: Vec<PathId>,
    /// Prim paths present before the transaction but absent after it.
    pub removed: Vec<PathId>,
    /// Minimal roots whose composed structure may have changed.
    pub resynced: Vec<PathId>,
    /// Surviving prims with opinion or child-list changes, without subtree resync.
    pub changed_info_only: Vec<PathId>,
    /// Complete property-field inventories for a subset of info-only prims.
    /// A prim absent here must be treated conservatively: any field may have
    /// changed. Resynced prims never have a precise inventory.
    pub property_changes: Vec<PrimPropertyChanges>,
}

/// An authored property field changed by a transaction.
///
/// These identify authored operations, not differences between resolved values.
/// AOUSD Core §12 (properties); OpenUSD `UsdNotice::ObjectsChanged::GetChangedFields`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PropertyField {
    /// The attribute's default value, including authored absence and blocks.
    Default,
    /// One or more authored time samples.
    TimeSamples,
    /// Attribute connections or relationship targets.
    Targets,
    /// A named property metadata field.
    Metadata(crate::TokenId),
}

/// One changed field of a composed property.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PropertyChange {
    /// Property name in the composed namespace.
    pub name: crate::TokenId,
    /// Authored field affected by the operation.
    pub field: PropertyField,
}

/// A complete inventory of changed fields for one info-only composed prim.
///
/// Consumers may ignore fields outside their dependencies only when this entry
/// is present. Inventories are sorted and deduplicated; source edits are mapped
/// through the stage's contributing source sites, including references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimPropertyChanges {
    /// The composed prim whose properties changed.
    pub prim: PathId,
    /// All potentially changed property fields on this prim.
    pub fields: Vec<PropertyChange>,
}

impl Changes {
    /// Complete changed property fields, or `None` when precision is unknown.
    #[must_use]
    pub fn properties_for(&self, prim: PathId) -> Option<&[PropertyChange]> {
        self.property_changes
            .binary_search_by_key(&prim, |p| p.prim)
            .ok()
            .map(|i| self.property_changes[i].fields.as_slice())
    }
}
