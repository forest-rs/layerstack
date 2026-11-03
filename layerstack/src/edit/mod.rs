// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authoring: atomic, invertible spec edits, written through composition.
//!
//! - An [`EditTarget`] says where stage-level edits go: a layer, a map
//!   from stage paths to spec paths in it (including variant-qualified
//!   ones), and a layer offset. It comes from a layer, a variant branch,
//!   or any node of a prim's index graph.
//! - A [`NamespaceEdit`] prepares a composed prim/property move or delete
//!   and dependent authored path repairs as a generation-guarded
//!   transaction. Its preview identifies every affected layer and prim
//!   slot.
//! - A [`Transaction`] lists spec edits, each addressed through a target
//!   or by spec path ([`Address`]), and optional preconditions: layer
//!   generations and expected authored values. Besides values and
//!   metadata, it authors relationship targets and attribute connections,
//!   whole or one target at a time ([`Transaction::add_target`]), the
//!   prim fields kept in dedicated members
//!   ([`PrimField`]): the specifier, type name, `active`, `instanceable`,
//!   child and property order, the composition arcs, whose list ops it
//!   also edits item by item ([`ListPosition`]), and `variantSetNames`,
//!   with variant sets added and removed
//!   ([`Transaction::add_variant_set`]); and the layer fields
//!   kept in dedicated members, the sublayers with their offsets and the
//!   relocates, checked against what composition can use; and it copies
//!   prim and property specs, remapping the paths they author
//!   ([`Transaction::copy_spec`]).
//! - Applying a transaction checks its preconditions, validates and
//!   applies every edit or none, and returns the inverse transaction,
//!   which restores each slot only while it holds what was written there.
//!   [`LiveStage::apply`](crate::LiveStage::apply) also recomposes the
//!   prims the edits affect.
//! - An [`UndoHistory`] keeps applied transactions' inverses as undo and
//!   redo steps, grouping several transactions into one step, and reports
//!   a step that an edit it did not record made stale.
//!
//! ```
//! use layerstack::{
//!     ArcKind, EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec,
//!     PropertyType, Reference, StageOptions, Time, TimeSamples, Transaction, Value,
//! };
//!
//! // `/World/Rock` references `/Rock` of a rock asset, 10 frames later.
//! let mut store = InMemoryStore::default();
//! let spin = store.tokens().intern("spin");
//! let (asset_rock, rock) = (store.path("/Rock"), store.path("/World/Rock"));
//! let double = PropertyType::parse("double").unwrap();
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
//! let graph = live.stage().prim(rock, &store).unwrap().composition_graph();
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
//!     store.layer(LayerId(2)).unwrap().property(asset_spin).unwrap().time_samples.clone()
//! };
//! assert_eq!(samples(&store), vec![(4.0, Value::Double(70.0))].into());
//! let spin = live.view(&store).attribute(spin_path).unwrap();
//! assert_eq!(spin.value::<f64>(Time::at(14.0)), Some(70.0));
//!
//! // The inverse removes the sample it created.
//! live.apply(&mut store, &applied.inverse).unwrap();
//! assert_eq!(samples(&store), TimeSamples::none());
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
mod changes;
pub(crate) use apply::{ArgumentInput, ArgumentName};
mod copy;
mod error;
mod history;
mod layer_fields;
mod namespace;
mod prim_fields;
pub(crate) mod same;
mod spec;
mod target;
mod transaction;
mod transfer;
mod variant_sets;

#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod namespace_tests;
#[cfg(test)]
mod tests;

use alloc::vec::Vec;

use crate::path::PathId;

pub(crate) use changes::PropertyChange;
pub use changes::{Changes, PrimFieldChange, PrimInfoField, PropertyField, PropertyFieldChange};
pub use error::{EditError, Rejection, Slot};
pub use history::{HistoryError, HistoryMark, HistoryStep, Recorded, UndoHistory};
pub use namespace::{NamespaceEdit, NamespaceError, PathField, UnrepairedPath};
pub use prim_fields::{ListPosition, PrimField};
pub use target::{Address, EditTarget};
pub use transaction::Transaction;

pub(crate) use apply::{Outcome, PropertyValueEdit, SourceNamespace, apply};

/// What [`LiveStage::apply`](crate::LiveStage::apply) did.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Applied {
    /// The transaction that undoes the applied one (see [`Transaction`]).
    pub inverse: Transaction,
    /// Composed existence and invalidation changes caused by the transaction.
    pub changes: Changes,
    /// The composed prims updated by this transaction. Existing attribute
    /// value edits may refresh opinions without rebuilding prim graphs;
    /// eligible local structural edits replace only their subtrees and
    /// boundary child lists. Use [`Self::changes`] to invalidate consumers;
    /// this list includes unchanged survivors after a full rebuild. In
    /// namespace order ([`crate::PathInterner::cmp_paths`]).
    pub recomposed: Vec<PathId>,
    /// The asset requests the transaction introduced: those the stage lists
    /// after it ([`Stage::asset_requests`](crate::Stage::asset_requests))
    /// but did not list before, in that order.
    ///
    /// Each names an arc that contributes nothing until the host loads its
    /// asset and composes again: for example a deferred payload the
    /// transaction added ([`crate::Reference::deferred`]), a sublayer it
    /// adds by asset path ([`crate::SublayerEntry::unresolved`]), a variant
    /// selection that reaches one, or a changed input of a dynamic file
    /// format's arguments ([`crate::dynamic_format`]). Requests pending
    /// before the transaction are not repeated here. A document host loads
    /// them with `layerstack_io::StageDocument::load_requested_assets`, or
    /// applies through `StageDocument::apply`, which does both.
    pub asset_requests: Vec<crate::AssetRequest>,
}
