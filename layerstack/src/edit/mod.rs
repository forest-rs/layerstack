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

pub(crate) use apply::apply;

/// What [`LiveStage::apply`](crate::LiveStage::apply) did.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    /// The transaction that undoes the applied one (see [`Transaction`]).
    pub inverse: Transaction,
    /// The composed prims that were recomposed, as
    /// [`LiveStage::recompose`](crate::LiveStage::recompose) reports them.
    pub recomposed: Vec<PathId>,
}
