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
mod variant_sets;

#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod namespace_tests;
#[cfg(test)]
mod tests;

use crate::sort;
use alloc::vec::Vec;

use crate::path::PathId;

pub use error::{EditError, Rejection, Slot};
pub use history::{HistoryError, HistoryMark, HistoryStep, Recorded, UndoHistory};
pub use namespace::{NamespaceEdit, NamespaceError, PathField, UnrepairedPath};
pub use prim_fields::{ListPosition, PrimField};
pub use target::{Address, EditTarget};
pub use transaction::Transaction;

pub(crate) use apply::{PropertyValueEdit, SourceNamespace, apply};

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

/// Composed changes made by one successful live transaction.
///
/// Created and removed are exact inventories. Resynced paths are subtree roots:
/// every descendant is invalidated, including created and removed descendants.
/// Info-only paths invalidate just themselves and never lie beneath a resync.
/// A changed opinion may be masked by a stronger one, so info-only notices do
/// not assert that the resolved value differs. A conservative full rebuild
/// reports the pseudo-root as resynced, rather than every survivor as changed.
/// A property spec added or removed by a transaction whose other edits are
/// all property field edits resyncs just that property
/// (`resynced_properties`): its prim is neither resynced nor, unless its
/// other properties changed too, info-only. A consumer caching anything a
/// property contributes, its prim's property names included, must read
/// `resynced_properties` as well as the prim lists. Other edits that add or
/// remove property specs, such as one creating a prim spec too or a
/// namespace edit, and applied-schema changes, resync the owning prim.
/// Info-only reports can include complete property-field inventories in
/// `property_changes`.
/// External opinion notifications lack edit details and conservatively resync
/// their affected prims, including other prims updated in the same batch.
/// Every list is in namespace order ([`crate::PathInterner::cmp_paths`]) and
/// contains no duplicates; `property_changes` follows the order of its
/// prims.
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
#[doc(alias = "UsdNotice::ObjectsChanged")]
#[doc(alias = "ObjectsChanged")]
#[non_exhaustive]
pub struct Changes {
    /// Prim paths present after the transaction but absent before it.
    pub created: Vec<PathId>,
    /// Prim paths present before the transaction but absent after it.
    pub removed: Vec<PathId>,
    /// Minimal roots whose composed structure may have changed.
    pub resynced: Vec<PathId>,
    /// Properties whose composed spec stacks may have changed, on prims
    /// that are not themselves resynced: a property spec was added,
    /// removed or renamed. Everything about such a property may have
    /// changed, its existence and declaration included; its prim's other
    /// properties and its prim's own fields did not. Never beneath a path
    /// in [`Self::resynced`]. In namespace order: by prim, then by
    /// property name.
    ///
    /// OpenUSD: `UsdNotice::ObjectsChanged::GetResyncedPaths` lists such a
    /// property's path, not its prim's (`UsdStage::_HandleLayersDidChange`
    /// resyncs the property paths of `SdfChangeList` entries that add or
    /// remove property specs).
    pub resynced_properties: Vec<crate::PropertyPath>,
    /// Surviving prims with opinion changes, without subtree resync.
    ///
    /// A prim created or removed is resynced alone: its parent, whose
    /// child list gained or lost it, is not listed here unless its own
    /// opinions changed too, as OpenUSD does not list it (a parent's
    /// `primChildren` records no field change, `SdfLayer::_PrimPushChild`).
    /// A consumer caching a prim's children updates them from the
    /// resyncs (and [`Self::created`], [`Self::removed`]) of its children.
    /// Reordering children (`primOrder`) resyncs the parent; a spec added
    /// to or removed from a weaker layer can move a child too, as child
    /// names compose weakest layer first, and resyncs that child.
    ///
    /// An inert spec (an `over` with no opinions of its own) added or
    /// removed for a prim that composes before and after changes only its
    /// prim's spec stack, and that of each prim composing from it through
    /// an arc: they are listed here, as OpenUSD lists them. So is a prim
    /// whose new `over` holds only property specs, which are resynced in
    /// [`Self::resynced_properties`], or metadata that composes nothing.
    /// A spec rewritten with only its properties changed, as a namespace
    /// edit rewrites one whose relationship targets it repairs, is listed
    /// here with the fields it changed in [`Self::property_changes`], and
    /// the properties it adds, removes or renames are resynced. A sublayer
    /// inserted or removed lists the pseudo-root, and the prims whose spec
    /// stacks gained or lost only inert specs (`over`s, properties aside).
    /// A change to stage metadata that composes nothing (`documentation`,
    /// `startTimeCode`, `metersPerUnit` and the like) lists the
    /// pseudo-root.
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
#[non_exhaustive]
pub enum PropertyField {
    /// The attribute's default value, including authored absence and blocks.
    Default,
    /// Replacement or clearing of the time-sample and spline slots together.
    Animation,
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

impl PropertyChange {
    /// Compares by property name, then field, names compared as strings.
    fn cmp_names(&self, other: &Self, tokens: &crate::TokenInterner) -> core::cmp::Ordering {
        let names = tokens.resolve(self.name).cmp(tokens.resolve(other.name));
        names.then_with(|| match (self.field, other.field) {
            (PropertyField::Metadata(a), PropertyField::Metadata(b)) => {
                tokens.resolve(a).cmp(tokens.resolve(b))
            }
            (a, b) => a.cmp(&b),
        })
    }
}

/// A complete inventory of changed fields for one info-only composed prim.
///
/// Consumers may ignore fields outside their dependencies only when this entry
/// is present. Inventories hold no duplicates and are ordered by property
/// name, then field ([`PropertyField`] declaration order, metadata fields by
/// name); source edits are mapped through the stage's contributing source
/// sites, including references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimPropertyChanges {
    /// The composed prim whose properties changed.
    pub prim: PathId,
    /// All potentially changed property fields on this prim.
    pub fields: Vec<PropertyChange>,
}

impl Changes {
    pub(crate) fn history_vector_bytes(&self, allocated: bool) -> usize {
        fn bytes<T>(values: &Vec<T>, allocated: bool) -> usize {
            (if allocated {
                values.capacity()
            } else {
                values.len()
            }) * size_of::<T>()
        }
        bytes(&self.created, allocated)
            + bytes(&self.removed, allocated)
            + bytes(&self.resynced, allocated)
            + bytes(&self.resynced_properties, allocated)
            + bytes(&self.changed_info_only, allocated)
            + bytes(&self.property_changes, allocated)
            + self
                .property_changes
                .iter()
                .map(|p| bytes(&p.fields, allocated))
                .sum::<usize>()
    }
    /// Complete changed property fields, or `None` when precision is unknown.
    #[must_use]
    pub fn properties_for(&self, prim: PathId) -> Option<&[PropertyChange]> {
        self.property_changes
            .iter()
            .find(|p| p.prim == prim)
            .map(|p| p.fields.as_slice())
    }

    /// Puts every list in the order [`Changes`] documents, and removes
    /// duplicates.
    ///
    /// OpenUSD reports changed paths in `SdfPath` order
    /// (`UsdNotice::ObjectsChanged`, a map keyed by `SdfPath`).
    pub(crate) fn sort(&mut self, paths: &crate::PathInterner, tokens: &crate::TokenInterner) {
        for list in [
            &mut self.created,
            &mut self.removed,
            &mut self.resynced,
            &mut self.changed_info_only,
        ] {
            paths.sort_paths(list, tokens);
        }
        sort::sort_by(&mut self.resynced_properties, |a, b| {
            paths
                .cmp_paths(a.prim_path(), b.prim_path(), tokens)
                .then_with(|| {
                    tokens
                        .resolve(a.property())
                        .cmp(tokens.resolve(b.property()))
                })
        });
        self.resynced_properties.dedup();
        sort::sort_by(&mut self.property_changes, |a, b| {
            paths.cmp_paths(a.prim, b.prim, tokens)
        });
        for prim in &mut self.property_changes {
            sort::sort_unstable_by(&mut prim.fields, |a, b| a.cmp_names(b, tokens));
            prim.fields.dedup();
        }
    }
}
