// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition-aware namespace moves and deletes lowered to ordinary atomic
//! transactions.
//!
//! AOUSD Core §8 (namespace), §10 (composition arcs), §12.4 (path list ops).
//! OpenUSD: `UsdNamespaceEditor` validates all required layer edits before
//! changing specs and repairs relationship targets and attribute connections.

use crate::sort;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::fmt;

use crate::diagnostic::{Describe, Names, with_store};

mod coordinated;
mod delete;
#[cfg(test)]
mod differential_tests;
mod retained;
mod unrepaired;

pub use unrepaired::{PathField, UnrepairedPath};

use super::{
    EditTarget, Transaction,
    apply::{Guarded, Raw},
    same::Same,
    spec::{Loc, SpecRef, spec_at, spec_at_mut},
    transaction::Op,
};
use crate::target_translation::{
    is_relational, relational_has_prefix, replace_relational_target_prefix, replace_text_prefix,
    text_has_prefix,
};
use crate::{
    FieldEntry, FieldValue, Layer, LayerId, LayerStack, LayerStore, ListOp, Path, PathId, PrimSpec,
    PropertyEntry, PropertyPath, ReadOnlyReason, Reference, ReferenceTarget, SpecPath, Stage,
    TargetPath, TokenId, Value, VariantSets, VariantSpec, spec_path::VariantSelectionSite,
};

/// A validated namespace move or delete, including dependent authored path
/// repairs.
///
/// Prepare against a stage reflecting the current store, inspect the affected
/// layers and specs, then pass [`Self::transaction`] to [`crate::LiveStage::apply`].
/// The transaction checks every inspected layer's generation and returns the
/// usual guarded inverse, which guards each affected prim slot in full.
/// Preparation changes no layer (it may intern paths).
///
/// Against the snapshot of a [`crate::LiveStage`], preparation costs about
/// what the edit touches: the stage retains which layer generations it
/// reflects, which prims its arcs reach each site from, and which specs
/// author paths, so preparing proves the snapshot current, finds dependent
/// namespace and repairs specs without visiting the stage, and composes
/// only the prims the edit can change. While it composes them, the edited
/// layers hold the edit in place, through [`LayerStore::layer_mut`], and
/// get their own content back, generations unchanged, before it returns.
/// Against a stage composed on its own, preparation composes and visits
/// the whole stage; both find the same edit.
///
/// Root-stack edits coordinate all contributing local opinions and author
/// relocates for prim namespace introduced through ancestral arcs. Explicit
/// dependent snapshots can participate through [`Self::prepare_with_dependents`].
/// Mapped source-asset edit targets retain their single-source behavior; their
/// split opinions and dependent arc namespaces remain explicit errors.
/// A prim moves with its variant sets and the specs inside every variant;
/// a prim composed from an ancestor's selected variant moves by a relocate.
/// Paths are repaired only in the variants a composed prim index selects:
/// specs in a variant no prim selects keep naming the old paths, as
/// `UsdNamespaceEditor` leaves them, and [`Self::unrepaired_paths`] lists
/// them. Edits through a variant's edit target,
/// instance proxies, inactive composition, unloaded assets and ambiguous
/// mappings are rejected before layer mutation.
/// Relationships, shader connections, all path-list-op buckets, reference and
/// payload targets, inherits, specializes and literal path expressions are
/// repaired in their own namespace. Relative expressions retain their original
/// authoring anchor. External asset namespaces are preserved unless that asset
/// is the explicitly edited source. Asset-variable expressions require resolved
/// dependency editing and are rejected.
#[derive(Clone, Debug, PartialEq)]
#[doc(alias = "UsdNamespaceEditor")]
pub struct NamespaceEdit {
    transaction: Transaction,
    source: TargetPath,
    destination: Option<TargetPath>,
    layers: Vec<LayerId>,
    specs: Vec<(LayerId, SpecPath)>,
    namespace_moves: Vec<(LayerId, TargetPath, TargetPath)>,
    relocates: Vec<(LayerId, Vec<crate::Relocate>)>,
    unrepaired: Vec<UnrepairedPath>,
}

/// Why a namespace move or delete cannot be prepared. No layer has been
/// modified.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum NamespaceError {
    /// Paths must be distinct non-root prims, or distinct properties.
    InvalidPaths,
    /// The source object has no composed or editable authored spec.
    NoSuchObject,
    /// The destination is already occupied in the stage or source layer.
    Collision,
    /// The destination parent is absent from the required stage/source domain.
    MissingParent,
    /// A prim cannot move beneath itself.
    Cycle,
    /// An object beneath an instance root cannot be edited independently.
    InstanceProxy,
    /// The edit target does not map both paths.
    Unmappable,
    /// A mapped source-asset move requires additional source-layer edits.
    SplitOpinions {
        /// Source layer that contributes the unsupported opinion.
        layer: LayerId,
        /// Authored spec that would require another namespace edit.
        spec: SpecPath,
    },
    /// A composition case cannot yet be handled without changing its meaning.
    UnsupportedComposition(&'static str),
    /// The edit would change a layer that cannot be saved where it was
    /// read ([`LayerStore::layer_read_only`]): an asset's package member
    /// whose arc back into the stage it would repair, say. Changing it in
    /// memory would leave an edit that no save can keep, so preparation
    /// refuses the whole edit, as it refuses any other, before any layer
    /// changes. Author the change in a layer that can be saved, or export
    /// the read-only layer and compose the copy in its place.
    ///
    /// A deliberate divergence from OpenUSD: `UsdNamespaceEditor`
    /// (`pxr/usd/usd/namespaceEditor.cpp`) edits such a layer in memory
    /// without a word from `CanApplyEdits`, and its save then fails in
    /// `SdfLayer::_CheckFormatWritability` (`pxr/usd/sdf/layer.cpp`).
    ReadOnlyLayer {
        /// The object the edit moves or deletes.
        source: TargetPath,
        /// Where a move takes it; `None` for a delete.
        destination: Option<TargetPath>,
        /// The read-only layer the edit would change: of several, the
        /// one with the smallest id.
        layer: LayerId,
        /// Why the layer cannot be saved.
        reason: ReadOnlyReason,
    },
}

impl NamespaceError {
    /// The error in words, its layer and spec written as text with
    /// `store`, the store the edit was prepared against ("moving split
    /// source opinions requires coordinated layer edits: /Asset/Rock in
    /// @./rock.usda@"). `Display` writes the same without a store, with
    /// ids.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl fmt::Display + 'a {
        with_store(self, store)
    }
}

impl Describe for NamespaceError {
    fn describe(&self, f: &mut fmt::Formatter<'_>, names: Names<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPaths => "expected two non-root prim paths or two property paths",
            Self::NoSuchObject => "no composed and authored source object",
            Self::Collision => "the destination is occupied",
            Self::MissingParent => "the destination parent has no editable authored spec",
            Self::Cycle => "a prim cannot move beneath itself",
            Self::InstanceProxy => "cannot edit an instance proxy",
            Self::Unmappable => "the edit target does not map both paths",
            Self::SplitOpinions { layer, spec } => {
                return write!(
                    f,
                    "moving split source opinions requires coordinated layer edits: {} in {}",
                    names.spec(spec),
                    names.layer(*layer)
                );
            }
            Self::UnsupportedComposition(reason) => reason,
            Self::ReadOnlyLayer {
                source,
                destination,
                layer,
                reason,
            } => {
                match destination {
                    Some(destination) => write!(
                        f,
                        "cannot move {} to {}",
                        names.target(*source),
                        names.target(*destination)
                    )?,
                    None => write!(f, "cannot delete {}", names.target(*source))?,
                }
                return write!(
                    f,
                    ": the edit would change {}, which cannot be saved ({reason})",
                    names.layer(*layer)
                );
            }
        })
    }
}

impl fmt::Display for NamespaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.describe(f, Names(None))
    }
}
impl core::error::Error for NamespaceError {}

impl NamespaceEdit {
    /// Prepares a prim rename/reparent or a property rename/reparent.
    ///
    /// Both paths are in stage namespace and map through `target`. The
    /// destination parent must exist in the composed stage. A root-stack target
    /// delegates to [`Self::prepare_with_dependents`] with no additional stages;
    /// missing local ancestors become inert overs and ancestral arc namespace
    /// uses relocates. A mapped source-asset target edits that asset directly
    /// and requires its destination parent to have an authored spec.
    ///
    /// Only child and property lists change: every spec's `primOrder` and
    /// `propertyOrder` stay as authored, so the moved object's old name
    /// stays listed and its new one is not, as `UsdNamespaceEditor` leaves
    /// them (unlike `SdfPrimSpec::SetName`, which renames the parent's
    /// `primOrder` entry).
    ///
    /// ```
    /// use layerstack::{EditTarget, InMemoryStore, Layer, LayerId, LiveStage,
    ///     NamespaceEdit, PrimSpec, StageOptions, TargetPath};
    /// let mut store = InMemoryStore::default();
    /// let old = store.path("/Old");
    /// let new = store.path("/New");
    /// let mut layer = Layer::new(LayerId(1));
    /// layer.insert_prim(old, PrimSpec::def());
    /// store.insert_layer(layer);
    /// let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let edit = NamespaceEdit::prepare(live.stage(), &mut store,
    ///     &EditTarget::for_layer(LayerId(1)), TargetPath::Prim(old), TargetPath::Prim(new)).unwrap();
    /// let applied = live.apply(&mut store, edit.transaction()).unwrap();
    /// assert!(live.view(&store).prim(new).is_some());
    /// live.apply(&mut store, &applied.inverse).unwrap();
    /// assert!(live.view(&store).prim(old).is_some());
    /// ```
    pub fn prepare(
        stage: &Stage,
        store: &mut dyn LayerStore,
        target: &EditTarget,
        source: TargetPath,
        destination: TargetPath,
    ) -> Result<Self, NamespaceError> {
        validate_paths(store, source, destination)?;
        validate_stage(stage, store, source, destination)?;
        let mapped_source = map(target, store, source)?;
        let mapped_destination = map(target, store, destination)?;
        if stage.layer_stack().contains(&target.layer())
            && spec_target(&mapped_source) == source
            && spec_target(&mapped_destination) == destination
        {
            // Through a variant of the root stack too, which
            // `prepare_with_dependents` refuses as OpenUSD does.
            return Self::prepare_with_dependents(stage, &[], store, target, source, destination);
        }
        let from = mapped_source;
        let to = mapped_destination;
        if from.has_variant_selections() || to.has_variant_selections() {
            return Err(NamespaceError::UnsupportedComposition(
                "variant-qualified namespace moves are not supported",
            ));
        }
        let id = target.layer();
        let root = stage.root_layer().ok_or(NamespaceError::NoSuchObject)?;
        let root_layers = LayerStack::gather(store, root).layers;
        let layers = reachable_layers(stage, store, root);
        if !layers.contains(&id) {
            return Err(NamespaceError::NoSuchObject);
        }
        validate_opinions(stage, store, source, id, &from)?;
        let before = store.layer(id).ok_or(NamespaceError::NoSuchObject)?.clone();
        let mut moved = before.clone();
        move_specs(store, &mut moved, &from, &to)?;
        let source_move = PathMove::new(spec_target(&from), spec_target(&to));
        let stage_move = PathMove::new(source, destination);
        let mut transaction = Transaction::new();
        let mut edited_layers = Vec::new();
        let mut specs = Vec::new();
        for layer_id in layers {
            let original = store
                .layer(layer_id)
                .ok_or(NamespaceError::NoSuchObject)?
                .clone();
            transaction.expect_generation(layer_id, original.generation());
            let mut edited = if layer_id == id {
                moved.clone()
            } else {
                original.clone()
            };
            if !edited.relocates().is_empty() {
                return Err(NamespaceError::UnsupportedComposition(
                    "namespace moves with authored relocates are not supported",
                ));
            }
            if layer_id != id && root_layers.contains(&layer_id) {
                // The edit target's layer has moved its specs already.
                repair_path_defaults(
                    store,
                    &[stage],
                    &mut edited,
                    source,
                    Some(destination),
                    &Dormant::default(),
                );
            }
            let local_move = if layer_id == id {
                Some(&source_move)
            } else if root_layers.contains(&layer_id) {
                Some(&stage_move)
            } else {
                None
            };
            for (path, spec) in &mut edited.prims {
                repair_prim(
                    store,
                    spec,
                    *path,
                    layer_id,
                    id,
                    local_move,
                    &source_move,
                    &Dormant::default(),
                )?;
            }
            for (path, branches) in &mut edited.variant_prims {
                for spec in branches {
                    repair_prim(
                        store,
                        spec,
                        *path,
                        layer_id,
                        id,
                        local_move,
                        &source_move,
                        &Dormant::default(),
                    )?;
                }
            }
            let pseudo_root = store_root_path(store);
            repair_fields(store, &mut edited.metadata, local_move, pseudo_root, false)?;
            if let Some(mapping) = local_move.filter(|m| m.from.property_path().is_none())
                && let Some(path) = original.default_prim_path(store.tokens())
            {
                let old = store.paths().intern(path);
                let new = mapping.prim(store, old);
                if old != new {
                    let path = store.paths().resolve(new);
                    let text = if path.segments().len() == 1 {
                        alloc::string::String::from(store.tokens().resolve(path.leaf().unwrap()))
                    } else {
                        path.display(store.tokens())
                    };
                    edited.default_prim = Some(store.tokens().intern(&text));
                }
            }

            let previous_len = transaction.len();
            append_delta(&original, &edited, &mut transaction, &mut specs);
            if original.default_prim != edited.default_prim
                || !original.metadata.same(&edited.metadata)
            {
                push(
                    &mut transaction,
                    Raw::LayerFields {
                        layer: layer_id,
                        default_prim: edited.default_prim,
                        metadata: edited.metadata.clone(),
                    },
                    Raw::LayerFields {
                        layer: layer_id,
                        default_prim: original.default_prim,
                        metadata: original.metadata.clone(),
                    },
                );
            }
            if transaction.len() > previous_len {
                edited_layers.push(layer_id);
            }
        }
        sort::sort_unstable(&mut edited_layers);
        sort::sort_unstable_by(&mut specs, |a, b| {
            a.0.cmp(&b.0).then(a.1.prim_path().cmp(&b.1.prim_path()))
        });
        Self {
            transaction,
            source,
            destination: Some(destination),
            layers: edited_layers,
            specs,
            namespace_moves: alloc::vec![(root, source, destination)],
            relocates: Vec::new(),
            // Every variant of the source asset is repaired.
            unrepaired: Vec::new(),
        }
        .writable(store)
    }

    /// The edit, once no layer it changes is read-only
    /// ([`NamespaceError::ReadOnlyLayer`]): the last step of every
    /// preparation, so a refused edit has changed nothing.
    ///
    /// OpenUSD diverges here: `UsdNamespaceEditor`
    /// (`pxr/usd/usd/namespaceEditor.cpp`) does not ask whether a layer
    /// it edits can be saved, so it repairs an arc in a package member in
    /// memory and `SdfLayer::_CheckFormatWritability`
    /// (`pxr/usd/sdf/layer.cpp`) refuses the save. Refusing the edit
    /// instead keeps every edit this type prepares savable; explicit
    /// edits of such a layer ([`Transaction`]) stay allowed, as in
    /// OpenUSD, since their caller chose that layer.
    fn writable(self, store: &dyn LayerStore) -> Result<Self, NamespaceError> {
        let read_only = self
            .layers
            .iter()
            .filter_map(|&layer| Some((layer, store.layer_read_only(layer)?)))
            .min_by_key(|&(layer, _)| layer);
        match read_only {
            Some((layer, reason)) => Err(NamespaceError::ReadOnlyLayer {
                source: self.source,
                destination: self.destination,
                layer,
                reason,
            }),
            None => Ok(self),
        }
    }

    /// Original composed object path.
    #[must_use]
    pub fn source(&self) -> TargetPath {
        self.source
    }
    /// New composed object path; `None` for a delete
    /// ([`Self::prepare_delete`]).
    #[must_use]
    pub fn destination(&self) -> Option<TargetPath> {
        self.destination
    }
    /// Sorted source and dependency layers whose authored content changes.
    #[must_use]
    pub fn layers_to_edit(&self) -> &[LayerId] {
        &self.layers
    }
    /// Prim slots affected by moves and dependent path repairs.
    ///
    /// Each entry includes all variants and properties held by that slot.
    /// Layer metadata changes are reported by [`Self::layers_to_edit`].
    #[must_use]
    pub fn specs_to_edit(&self) -> &[(LayerId, SpecPath)] {
        &self.specs
    }
    /// Namespace moves discovered in each supplied stage, as
    /// `(stage_root_layer, source, destination)` in that stage's namespace.
    /// Direct reference mounts remain fixed and do not appear as moves.
    #[must_use]
    pub fn namespace_moves(&self) -> &[(LayerId, TargetPath, TargetPath)] {
        &self.namespace_moves
    }
    /// Complete replacement relocate tables for affected local layers.
    /// An empty table removes prior authored relocates. Source assets are
    /// excluded unless they are the explicit mapped source edit target.
    #[must_use]
    pub fn relocates_to_author(&self) -> &[(LayerId, Vec<crate::Relocate>)] {
        &self.relocates
    }
    /// The authored paths the edit leaves naming the moved or deleted
    /// object, or something beneath it, sorted by layer, then spec,
    /// field and path: those in the specs no composed prim uses, such as
    /// the variants no prim selects, which the edit moves or removes with
    /// their prim without repairing, as `UsdNamespaceEditor` does. Empty
    /// when every path naming the object is repaired.
    ///
    /// Each names the location the object had before the edit, so a
    /// later edit that composes its spec (selecting its variant) finds a
    /// reference, target or connection to nothing there. The edit is
    /// written as OpenUSD writes it, whatever this reports; a caller that
    /// wants these repaired can edit them through the
    /// [`UnrepairedPath::spec`] each names, after applying this edit.
    ///
    /// A mapped source-asset edit ([`Self::prepare`] through a target
    /// outside the root layer stack) repairs every variant and reports
    /// none.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use layerstack::{EditTarget, InMemoryStore, Layer, LayerId, ListOp, LiveStage,
    ///     NamespaceEdit, PathField, PrimSpec, PropertyEntry, PropertySpec, StageOptions,
    ///     TargetPath, VariantSetSpec, VariantSpec};
    /// let mut store = InMemoryStore::default();
    /// let (world, lamp) = (store.path("/World"), store.path("/World/Lamp"));
    /// let (light, on, off) = (store.tokens().intern("light"), store.tokens().intern("on"),
    ///     store.tokens().intern("off"));
    /// // `/World` selects `light=on`; its `off` variant targets the lamp.
    /// let mut targets = VariantSpec::default();
    /// targets.properties.push(PropertyEntry {
    ///     name: store.tokens().intern("lamp"),
    ///     spec: Arc::new(PropertySpec::relationship()
    ///         .with_targets(ListOp::explicit(vec![TargetPath::Prim(lamp)]))),
    /// });
    /// let mut spec = PrimSpec::def();
    /// let rare = spec.rare_mut();
    /// rare.variant_sets.insert(light, VariantSetSpec {
    ///     variants: [(on, VariantSpec::default()), (off, targets)].into_iter().collect(),
    /// });
    /// rare.variant_set_names.make_mut().prepend.push(light);
    /// rare.variant_selections.insert(light, on);
    /// let mut layer = Layer::new(LayerId(1));
    /// layer.insert_prim(world, spec);
    /// layer.insert_prim(lamp, PrimSpec::def());
    /// store.insert_layer(layer);
    /// let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let lamp2 = store.path("/World/Lamp2");
    /// let edit = NamespaceEdit::prepare(live.stage(), &mut store,
    ///     &EditTarget::for_layer(LayerId(1)), TargetPath::Prim(lamp),
    ///     TargetPath::Prim(lamp2)).unwrap();
    /// let [left] = edit.unrepaired_paths() else { panic!("one path left") };
    /// assert_eq!((left.field, left.path), (PathField::Targets, TargetPath::Prim(lamp)));
    /// assert_eq!(left.display(&store).to_string(),
    ///     "/World/Lamp in the relationship targets of /World{light=off}.lamp in layer 1");
    /// ```
    #[must_use]
    pub fn unrepaired_paths(&self) -> &[UnrepairedPath] {
        &self.unrepaired
    }
    /// Atomic, generation-guarded edit; apply through [`crate::LiveStage::apply`].
    #[must_use]
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }
    /// Takes the prepared transaction, preserving its generation guards.
    #[must_use]
    pub fn into_transaction(self) -> Transaction {
        self.transaction
    }

    /// Prepares the delete of the composed prim or property `object`, and
    /// of every authored path that names it, as one atomic transaction.
    ///
    /// Every layer of the stage's root layer stack loses its specs of the
    /// object: for a prim, every spec at or beneath it, with its name in its
    /// parent's children (specs inside an ancestor's variant branches belong
    /// to that arc and stay). A prim that still composes from across an arc
    /// (a reference, payload, variant branch or relocate) is removed from
    /// namespace with a relocate to `<>`, authored in `target`'s layer, or
    /// by retargeting a relocate that moves a prim to it; a property with
    /// opinions across an arc cannot be deleted
    /// ([`NamespaceError::UnsupportedComposition`]). The root stack's
    /// authored paths that name the object, and for a prim anything beneath
    /// it, are then dropped: relationship targets, attribute connections,
    /// inherits, specializes, internal references and payloads, path
    /// expression terms (`/A /B` without `/A` is `/B`) and `defaultPrim`;
    /// those in a variant no composed prim index selects stay, as
    /// `UsdNamespaceEditor` leaves them ([`Self::unrepaired_paths`] lists
    /// them). The local specs that compose over
    /// the deleted namespace through an internal arc authored at an
    /// ancestor (`/X/Kid` when `/X` inherits `/B` and `/B/Kid` goes), or
    /// over a deleted property (`/X.x` for `/B.x`), go too, outside every
    /// variant and inside the selected ones holding the arc.
    ///
    /// The other layers the stage reaches lose their references and
    /// payloads into the root layer stack that name the object, or a prim
    /// beneath it, from the specs a composed prim index uses (an asset's
    /// `/Asset/Front` referencing `@scene.usda@</World/Lamp>`), and the
    /// specs that composed over the deleted namespace through such an arc,
    /// in every layer stack between it and the stage (the asset's `over
    /// "Bulb"` beneath `/Asset/Front`, the stage's beneath
    /// `/World/Shelf/Front`). The arcs of the specs no prim index uses
    /// stay, and are listed.
    ///
    /// `target` must address a layer of the root layer stack without
    /// remapping paths ([`NamespaceError::Unmappable`]). The object must be
    /// composed ([`NamespaceError::NoSuchObject`]) and not beneath an
    /// instance ([`NamespaceError::InstanceProxy`]). Preparation composes the
    /// edited layers to check that the object is gone before returning;
    /// against a live stage's snapshot, only the object and its ancestors.
    ///
    /// OpenUSD: `UsdNamespaceEditor::DeletePrimAtPath` and
    /// `DeletePropertyAtPath`, which remove arcs to the deleted namespace
    /// only where a prim index depends on it, so an inherit whose site
    /// another arc of the same prim already reaches keeps naming it.
    ///
    /// Spec: AOUSD Core §8 (namespace), §10.3.2.6 (relocates), §12.4 (list
    /// ops).
    pub fn prepare_delete(
        stage: &Stage,
        store: &mut dyn LayerStore,
        target: &EditTarget,
        object: TargetPath,
    ) -> Result<Self, NamespaceError> {
        delete::prepare(stage, store, target, object)
    }

    /// Prepares one atomic root-stack namespace edit and repairs supplied stages.
    ///
    /// Each snapshot must reflect the current store and share its token/path
    /// domains. All root-stack opinions move together. Namespace introduced by
    /// an ancestral reference or payload uses a relocate in `target.layer()`;
    /// the referenced source layers remain intact. The editor discovers related
    /// namespaces from each supplied stage's composition graph and moves their
    /// local override specs, including relationship, connection and expression
    /// repairs. Direct references to the moved root keep their mount namespace
    /// and receive a repaired source target instead.
    ///
    /// Dependents are caller-owned: undisclosed stages cannot be discovered.
    /// Masks, muted layers, unloaded payloads and ambiguous mappings are
    /// rejected explicitly, as is a `target` that maps paths into a variant
    /// ([`NamespaceError::Unmappable`]), which `UsdNamespaceEditor` refuses.
    /// Dependent mappings require non-implied arcs to sites outside variant
    /// branches: from their local stack (or authored in the local variants
    /// their prim selects, whose specs then move inside those variants), or
    /// nested in another layer stack's arc target, as an asset's arc back
    /// into the stage is. Moving that arc's target repairs the arc in the
    /// asset; moving beneath it moves the specs over the moved namespace in
    /// every layer stack between the arc and the stage, the asset's too. A
    /// reparent leaving an arc's source domain deletes
    /// the dependent specs over the moved namespace, which no longer
    /// compose there, as `UsdNamespaceEditor` does. Preparation composes the edited layers to verify the
    /// proposed namespace before returning: where every snapshot is a live
    /// stage's and current, only the prims the edit can change, unless
    /// relocates, layer metadata, dynamic payloads or value clips are
    /// involved, which compose the whole stage. All inspected layers
    /// receive generation preconditions; application and its guarded
    /// inverse are atomic. Callers
    /// refresh other live stages after applying the shared transaction.
    /// AOUSD Core §8, §10.3.2.6 (relocates); OpenUSD `UsdNamespaceEditor` and
    /// `PcpGatherDependentNamespaceEdits`.
    pub fn prepare_with_dependents(
        stage: &Stage,
        dependents: &[&Stage],
        store: &mut dyn LayerStore,
        target: &EditTarget,
        source: TargetPath,
        destination: TargetPath,
    ) -> Result<Self, NamespaceError> {
        coordinated::prepare(stage, dependents, store, target, source, destination)
    }
}

fn map(
    target: &EditTarget,
    store: &mut dyn LayerStore,
    path: TargetPath,
) -> Result<SpecPath, NamespaceError> {
    let prim = target
        .map_to_spec_path(path.prim_path(), store.paths())
        .ok_or(NamespaceError::Unmappable)?;
    Ok(match path {
        TargetPath::Prim(_) => prim,
        TargetPath::Property(p) => prim.with_property(p.property()),
    })
}

fn spec_target(path: &SpecPath) -> TargetPath {
    match path.property() {
        Some(name) => TargetPath::Property(PropertyPath::new(path.prim_path(), name)),
        None => TargetPath::Prim(path.prim_path()),
    }
}

fn validate_paths(
    store: &dyn LayerStore,
    from: TargetPath,
    to: TargetPath,
) -> Result<(), NamespaceError> {
    if core::mem::discriminant(&from) != core::mem::discriminant(&to)
        || from == to
        || store
            .paths()
            .resolve(from.prim_path())
            .segments()
            .is_empty()
        || store.paths().resolve(to.prim_path()).segments().is_empty()
    {
        return Err(NamespaceError::InvalidPaths);
    }
    let valid = |p: TargetPath| {
        store
            .paths()
            .resolve(p.prim_path())
            .segments()
            .iter()
            .all(|n| crate::ident::is_identifier(store.tokens().resolve(*n)))
            && p.property_path().is_none_or(|p| {
                store
                    .tokens()
                    .resolve(p.property())
                    .split(':')
                    .all(crate::ident::is_identifier)
            })
    };
    if !valid(from) || !valid(to) {
        return Err(NamespaceError::InvalidPaths);
    }
    if matches!(from, TargetPath::Prim(_))
        && store
            .paths()
            .resolve(from.prim_path())
            .is_prefix_of(store.paths().resolve(to.prim_path()))
    {
        return Err(NamespaceError::Cycle);
    }
    Ok(())
}
fn validate_stage(
    stage: &Stage,
    store: &mut dyn LayerStore,
    from: TargetPath,
    to: TargetPath,
) -> Result<(), NamespaceError> {
    let exists = |p| match p {
        TargetPath::Prim(p) => stage.has_prim(p),
        TargetPath::Property(p) => stage.has_property_path(p),
    };
    if !exists(from) {
        return Err(NamespaceError::NoSuchObject);
    }
    if exists(to) {
        return Err(NamespaceError::Collision);
    }
    let parent = match to {
        TargetPath::Prim(p) => store
            .paths()
            .resolve(p)
            .parent()
            .ok_or(NamespaceError::InvalidPaths)?,
        TargetPath::Property(p) => store.paths().resolve(p.prim_path()).clone(),
    };
    let parent = store.paths().intern(parent);
    if !store.paths().resolve(parent).segments().is_empty() && !stage.has_prim(parent) {
        return Err(NamespaceError::MissingParent);
    }
    for object in [from, to] {
        let mut current = Some(store.paths().resolve(object.prim_path()).clone());
        let mut first = true;
        while let Some(path) = current {
            let at = store.paths().intern(path.clone());
            if inactive_prim(stage, store, at) {
                return Err(NamespaceError::UnsupportedComposition(
                    "namespace moves of or beneath inactive roots require dormant descendant coordination",
                ));
            }
            if stage.is_instance(at) && !first {
                return Err(NamespaceError::InstanceProxy);
            }
            first = false;
            current = path.parent();
        }
    }
    Ok(())
}
fn validate_opinions(
    stage: &Stage,
    store: &dyn LayerStore,
    from: TargetPath,
    layer: LayerId,
    source: &SpecPath,
) -> Result<(), NamespaceError> {
    validate_authored_subtree(stage, store, from, layer, source)?;
    // The snapshot inventory includes retained inactive roots. Default
    // traversal intentionally omits them and is insufficient for validation.
    for prim in stage.prim_paths() {
        let inside = store
            .paths()
            .resolve(from.prim_path())
            .is_prefix_of(store.paths().resolve(prim));
        if inside && from.property_path().is_none() && inactive_prim(stage, store, prim) {
            return Err(NamespaceError::UnsupportedComposition(
                "namespace moves containing inactive roots require dormant descendant coordination",
            ));
        }
        let Some(stack) = stage.prim_stack(prim) else {
            continue;
        };
        if inside && (from.property_path().is_none() || prim == from.prim_path()) {
            for (l, spec) in &stack {
                let relevant = from.property_path().is_none()
                    || store
                        .layer(*l)
                        .and_then(|l| spec_at(l, &Loc::of(spec)))
                        .is_some_and(|s| {
                            s.properties()
                                .iter()
                                .any(|p| Some(p.name) == source.property())
                        });
                if relevant && (*l != layer || spec.has_variant_selections()) {
                    return Err(NamespaceError::SplitOpinions {
                        layer: *l,
                        spec: spec.clone(),
                    });
                }
            }
        } else if stack.iter().any(|(l, p)| {
            if *l != layer {
                return false;
            }
            if let Some(property) = source.property() {
                p.prim_path() == source.prim_path()
                    && store
                        .layer(*l)
                        .and_then(|l| spec_at(l, &Loc::of(p)))
                        .is_some_and(|s| s.properties().iter().any(|p| p.name == property))
            } else {
                store
                    .paths()
                    .resolve(source.prim_path())
                    .is_prefix_of(store.paths().resolve(p.prim_path()))
            }
        }) && !alias_preserves_namespace(stage, store, prim, from, layer, source)
        {
            return Err(NamespaceError::UnsupportedComposition(
                "dependent namespace through another composition arc requires coordinated edits",
            ));
        }
    }
    Ok(())
}

// AOUSD Core §7.6 (active), §11.3.1 (inactive descendants are unpopulated).
// Read the strongest authored active opinion, including selected variants,
// without requiring the stage-controls API from the next stacked branch.
fn inactive_prim(stage: &Stage, store: &dyn LayerStore, prim: PathId) -> bool {
    let active = store.tokens().lookup("active");
    stage
        .prim_stack(prim)
        .unwrap_or_default()
        .into_iter()
        .find_map(|(layer, path)| {
            let spec = spec_at(store.layer(layer)?, &Loc::of(&path))?;
            if let SpecRef::Prim(prim) = spec
                && prim.active.is_some()
            {
                return prim.active;
            }
            active
                .and_then(|key| crate::doc::get_field(spec.fields(), &key))
                .and_then(|field| match field {
                    FieldValue::Value(Value::Bool(value)) => Some(*value),
                    _ => None,
                })
        })
        == Some(false)
}

// Every source layer in the contributing node's stack shares this namespace.
// Inspect authored slots rather than only populated opinions: weaker specs
// behind inactive roots or unselected variants must not remain at the old path.
fn validate_authored_subtree(
    stage: &Stage,
    store: &dyn LayerStore,
    from: TargetPath,
    layer: LayerId,
    source: &SpecPath,
) -> Result<(), NamespaceError> {
    let mut source_layers = LayerStack::gather(store, layer).layers;
    if let Some(graph) = stage.explain_prim_graph(from.prim_path()) {
        for (_, node) in graph.nodes() {
            if node.site().prim_path() == source.prim_path() {
                let stack = LayerStack::gather_identifier(store, node.layer_stack_identifier());
                if stack.layers.contains(&layer) {
                    source_layers.extend(stack.layers);
                }
            }
        }
    }
    sort::sort_unstable(&mut source_layers);
    source_layers.dedup();
    for id in source_layers {
        let Some(authored) = store.layer(id) else {
            continue;
        };
        for (path, spec) in authored.prims.iter().map(|(p, s)| (*p, s)).chain(
            authored
                .variant_prims
                .iter()
                .flat_map(|(p, ss)| ss.iter().map(|s| (*p, s))),
        ) {
            let relevant = if let Some(property) = source.property() {
                path == source.prim_path() && spec.properties.iter().any(|p| p.name == property)
            } else {
                store
                    .paths()
                    .resolve(source.prim_path())
                    .is_prefix_of(store.paths().resolve(path))
            };
            if !relevant {
                continue;
            }
            if id != layer {
                return Err(NamespaceError::SplitOpinions {
                    layer: id,
                    spec: Loc::Prim {
                        path,
                        sites: spec.outer_variant_sites().to_vec(),
                    }
                    .spec_path(store.paths()),
                });
            }
            if source.property().is_none() && spec.active == Some(false) {
                return Err(NamespaceError::UnsupportedComposition(
                    "namespace moves containing authored inactive roots require dormant descendant coordination",
                ));
            }
        }
    }
    Ok(())
}

// An arc targeting the moved root keeps its composed mount point. Its authored
// reference/inherit/specialize target changes, but descendants retain their
// names. Moving a descendant inside an arc needs a separate namespace map.
fn alias_preserves_namespace(
    stage: &Stage,
    store: &dyn LayerStore,
    prim: PathId,
    from: TargetPath,
    layer: LayerId,
    source: &SpecPath,
) -> bool {
    if from.property_path().is_some() {
        return false;
    }
    let mut current = Some(store.paths().resolve(prim).clone());
    while let Some(path) = current {
        if let Some(id) = store.paths().lookup(&path)
            && stage.explain_prim_graph(id).is_some_and(|graph| {
                graph.nodes().any(|(_, node)| {
                    node.arc_kind() != crate::ArcKind::Local
                        && usize::from(node.namespace_depth()) == path.segments().len()
                        && node.site().prim_path() == source.prim_path()
                        && LayerStack::gather_identifier(store, node.layer_stack_identifier())
                            .layers
                            .contains(&layer)
                })
            })
        {
            return true;
        }

        current = path.parent();
    }
    false
}

fn reachable_layers(stage: &Stage, store: &dyn LayerStore, root: LayerId) -> Vec<LayerId> {
    let mut found = Vec::new();
    let mut pending = alloc::vec![root];
    for prim in stage.prim_paths() {
        pending.extend(
            stage
                .prim_stack(prim)
                .unwrap_or_default()
                .into_iter()
                .map(|(layer, _)| layer),
        );
    }
    let mut seen = crate::HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(layer) = store.layer(id) else {
            continue;
        };
        found.push(id);
        pending.extend(layer.sublayers.iter().map(|s| s.layer));
        for spec in layer
            .prims
            .values()
            .chain(layer.variant_prims.values().flatten())
        {
            for refs in [&spec.references, &spec.payloads] {
                collect_ref_layers(store, refs, &mut pending);
            }
            collect_variant_layers(store, spec.variant_sets(), &mut pending);
        }
    }
    sort::sort_unstable(&mut found);
    found
}
fn collect_ref_layers(store: &dyn LayerStore, refs: &ListOp<Reference>, layers: &mut Vec<LayerId>) {
    layers.extend(refs.items().filter_map(|r| r.stored_target_layer(store)));
}
fn collect_variant_layers(store: &dyn LayerStore, sets: &VariantSets, layers: &mut Vec<LayerId>) {
    for set in sets.values() {
        for v in set.variants.values() {
            collect_ref_layers(store, &v.references, layers);
            collect_ref_layers(store, &v.payloads, layers);
            collect_variant_layers(store, &v.variant_sets, layers);
        }
    }
}

pub(super) fn move_specs(
    store: &mut dyn LayerStore,
    layer: &mut Layer,
    from: &SpecPath,
    to: &SpecPath,
) -> Result<(), NamespaceError> {
    let from_loc = Loc::of(from);
    let to_loc = Loc::of(to);
    if spec_at(layer, &from_loc).is_none() {
        return Err(NamespaceError::NoSuchObject);
    }
    if let Some(name) = from.property() {
        let source = spec_at(layer, &from_loc).unwrap();
        let index = source
            .properties()
            .iter()
            .position(|p| p.name == name)
            .ok_or(NamespaceError::NoSuchObject)?;
        let property = source.properties()[index].spec.clone();
        let new_name = to.property().ok_or(NamespaceError::InvalidPaths)?;
        let destination = spec_at(layer, &to_loc).ok_or(NamespaceError::MissingParent)?;
        if destination.properties().iter().any(|p| p.name == new_name) {
            return Err(NamespaceError::Collision);
        }
        spec_at_mut(layer, &from_loc)
            .unwrap()
            .properties()
            .remove(index);
        let mut destination = spec_at_mut(layer, &to_loc).unwrap();
        let new_index = if from.prim_path() == to.prim_path() {
            index
        } else {
            destination.properties().len()
        };
        destination.properties().insert(
            new_index,
            PropertyEntry {
                name: new_name,
                spec: property,
            },
        );
        return Ok(());
    }
    // The branches enclosing the moved specs that are not the moved
    // namespace's own: none for a prim spec outside its ancestors'
    // variants (`/A/B`), the ancestor's selection for one inside
    // (`/A{v=x}B`). A prim spec moves with its variant sets and every
    // spec authored inside them; specs at the same paths inside other
    // branches of an ancestor belong to that branch and stay, as
    // `SdfLayer::Apply` moves only the spec at the edit's path and its
    // namespace children (`pxr/usd/sdf/layer.cpp`). AOUSD Core §7.3.6.
    let within: Vec<VariantSelectionSite> = from.variant_selection_sites().collect();
    if to.variant_selection_sites().ne(within.iter().copied()) {
        return Err(NamespaceError::InvalidPaths);
    }
    let source_path = store.paths().resolve(from.prim_path()).clone();
    let destination_path = store.paths().resolve(to.prim_path()).clone();
    let (parent_loc, new_name) = to_loc
        .parent(store.paths())
        .ok_or(NamespaceError::InvalidPaths)?;
    let (source_parent_loc, old_name) = from_loc
        .parent(store.paths())
        .ok_or(NamespaceError::InvalidPaths)?;
    let parent_is_root = store
        .paths()
        .resolve(parent_loc.prim_path())
        .segments()
        .is_empty();
    if !parent_is_root && spec_at(layer, &parent_loc).is_none() {
        return Err(NamespaceError::MissingParent);
    }
    let path_move = PathMove::new(
        TargetPath::Prim(from.prim_path()),
        TargetPath::Prim(to.prim_path()),
    );
    // Every path with a spec has one in `prims`; the specs of other
    // branches at it are in `variant_prims` (see `Layer::prims`).
    let slots: Vec<PathId> = layer
        .prims
        .keys()
        .copied()
        .filter(|p| source_path.is_prefix_of(store.paths().resolve(*p)))
        .collect();
    let mut staying = Vec::new();
    let mut moving = Vec::new();
    for old in slots {
        let main = layer.prims.remove(&old);
        let branches = layer.variant_prims.remove(&old).unwrap_or_default();
        let suffix = store
            .paths()
            .resolve(old)
            .strip_prefix(&source_path)
            .unwrap()
            .to_vec();
        let new = store.paths().intern(destination_path.join(&suffix));
        for mut spec in main.into_iter().chain(branches) {
            if !moves_with(&*store, &source_path, &within, &spec) {
                staying.push((old, spec));
                continue;
            }
            if let Some(rare) = spec.authored_rare_mut() {
                for site in &mut rare.outer_variant_sites {
                    site.host_path = path_move.prim(store, site.host_path);
                }
            }
            moving.push((new, spec));
        }
    }
    for (path, spec) in staying {
        layer.insert_prim(path, spec);
    }
    for (path, spec) in moving {
        if layer
            .prim_spec_in(path, spec.outer_variant_sites())
            .is_some()
        {
            return Err(NamespaceError::Collision);
        }
        layer.insert_prim(path, spec);
    }
    let old_index = spec_at(layer, &source_parent_loc)
        .and_then(|s| s.children().iter().position(|n| *n == old_name));
    // Only the child lists change: the parents' `primOrder` keeps the old
    // name and lists no new one, as `SdfLayer::Apply` moves a child
    // (`Sdf_ChildrenUtils::MoveChildForBatchNamespaceEdit`,
    // `pxr/usd/sdf/childrenUtils.cpp`), which `UsdNamespaceEditor` applies
    // to each layer (`_ApplyLayerSpecMove`, `pxr/usd/usd/namespaceEditor.cpp`).
    // `SdfPrimSpec::SetName` alone renames the entry. AOUSD Core
    // §7.6.2.4.5 (`primOrder`).
    if let Some(mut s) = spec_at_mut(layer, &source_parent_loc) {
        s.children().retain(|n| *n != old_name);
    }
    let same_parent = parent_loc == source_parent_loc;
    if let Some(mut s) = spec_at_mut(layer, &parent_loc) {
        let children = s.children();
        let index = if same_parent {
            old_index.unwrap_or(children.len())
        } else {
            children.len()
        };
        children.insert(index.min(children.len()), new_name);
    }
    Ok(())
}

/// Whether the prim spec `spec`, at or beneath `source`, moves with it:
/// whether the branches enclosing it are `within`, then only branches
/// hosted at or beneath `source`.
fn moves_with(
    store: &dyn LayerStore,
    source: &Path,
    within: &[VariantSelectionSite],
    spec: &PrimSpec,
) -> bool {
    let sites = spec.outer_variant_sites();
    sites.len() >= within.len()
        && sites[..within.len()] == *within
        && sites[within.len()..]
            .iter()
            .all(|site| source.is_prefix_of(store.paths().resolve(site.host_path)))
}

#[derive(Clone, PartialEq)]
pub(super) struct PathMove {
    from: TargetPath,
    to: TargetPath,
    /// Whose path fixing this is.
    fix: PathFix,
}
/// Whose path fixing a [`PathMove`] mirrors; they differ for the paths in
/// a relational attribute's or mapper's brackets.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum PathFix {
    /// `UsdNamespaceEditor`'s: an authored path is fixed where the moved
    /// path is its prefix, and the relationship's `default` with it.
    NamespaceEdit,
    /// `SdfCopySpec`'s (`SdfShouldCopyValue`, `pxr/usd/sdf/copyUtils.cpp`):
    /// a target or connection is mapped by `SdfPath::ReplacePrefix` alone,
    /// and other fields are copied as they are.
    Copy,
}
impl PathMove {
    pub(super) fn new(from: TargetPath, to: TargetPath) -> Self {
        Self {
            from,
            to,
            fix: PathFix::NamespaceEdit,
        }
    }
    /// The paths a copy of `from` to `to` remaps ([`PathFix::Copy`]).
    pub(super) fn for_copy(from: TargetPath, to: TargetPath) -> Self {
        Self {
            from,
            to,
            fix: PathFix::Copy,
        }
    }
    fn prim(&self, store: &mut dyn LayerStore, path: PathId) -> PathId {
        if self.from.property_path().is_some() {
            return path;
        }
        let Some(suffix) = store
            .paths()
            .resolve(path)
            .strip_prefix(store.paths().resolve(self.from.prim_path()))
            .map(<[_]>::to_vec)
        else {
            return path;
        };
        let result = store.paths().resolve(self.to.prim_path()).join(&suffix);
        store.paths().intern(result)
    }
    /// `path`, an authored relationship target or connection, after the
    /// move: as `_PathListOpNamespaceEdit` (`pxr/usd/usd/namespaceEditor.cpp`)
    /// edits a list op's item, replaced only where the moved path is its
    /// prefix (`SdfPath::HasPrefix`, which never looks inside brackets), and
    /// then by `SdfPath::ReplacePrefix`, which fixes the paths in a
    /// relational attribute's or mapper's brackets too (`fixTargetPaths`).
    /// A copy ([`PathFix::Copy`]) replaces the prefix without the test, as
    /// `SdfShouldCopyValue` does, so it fixes a bracket of any path.
    /// AOUSD Core §8 (paths).
    fn target(&self, store: &mut dyn LayerStore, path: TargetPath) -> TargetPath {
        if let TargetPath::Property(property) = path
            && is_relational(store.tokens(), property.property())
        {
            let (tokens, paths) = (store.tokens(), store.paths());
            if self.fix == PathFix::NamespaceEdit
                && !relational_has_prefix(tokens, paths, property, self.from)
            {
                return path;
            }
            let replaced =
                replace_relational_target_prefix(tokens, paths, property, self.from, self.to);
            return TargetPath::Property(replaced);
        }
        if self.from.property_path().is_some() {
            return if path == self.from { self.to } else { path };
        }
        let prim = self.prim(store, path.prim_path());
        match path {
            TargetPath::Prim(_) => TargetPath::Prim(prim),
            TargetPath::Property(p) => TargetPath::Property(PropertyPath::new(prim, p.property())),
        }
    }
}
fn map_list<T>(list: &mut ListOp<T>, mut map: impl FnMut(&mut T)) {
    for bucket in list.lists_mut() {
        for item in bucket {
            map(item);
        }
    }
}
fn repair_refs(
    store: &mut dyn LayerStore,
    refs: &mut ListOp<Reference>,
    author: LayerId,
    edited: LayerId,
    local: Option<&PathMove>,
    source: &PathMove,
) -> Result<(), NamespaceError> {
    if refs.items().any(Reference::is_expression) {
        return Err(NamespaceError::UnsupportedComposition(
            "reference/payload asset expressions require resolved dependency editing",
        ));
    }
    map_list(refs, |r| {
        let target = r.stored_target_layer(&*store);
        let mapping = if r.layer == author && r.asset.is_none() {
            local.or_else(|| (r.layer == edited).then_some(source))
        } else if target == Some(edited) {
            Some(source)
        } else {
            None
        };
        if let (Some(mapping), ReferenceTarget::Prim(path)) = (mapping, r.target.clone()) {
            r.target = ReferenceTarget::Prim(mapping.prim(store, path));
        }
    });
    Ok(())
}
fn repair_properties(
    store: &mut dyn LayerStore,
    properties: &mut [PropertyEntry],
    local: Option<&PathMove>,
    anchor: PathId,
    owner_moves: bool,
) -> Result<(), NamespaceError> {
    for entry in properties {
        let moved_property = local.filter(|m| {
            m.to.property_path()
                .is_some_and(|p| p.prim_path() == anchor && p.property() == entry.name)
        });
        let entry_anchor = moved_property.map_or(anchor, |m| m.from.prim_path());
        let entry_moves =
            owner_moves || moved_property.is_some_and(|m| m.from.prim_path() != m.to.prim_path());
        let spec = Arc::make_mut(&mut entry.spec);
        if let (Some(mapping), Some(targets)) = (local, &mut spec.targets) {
            map_list(targets, |p| *p = mapping.target(store, *p));
        }
        if let Some(value) = &mut spec.default {
            repair_value(store, value, local, entry_anchor, entry_moves)?;
        }
        if let Some(samples) = spec.time_samples.get_mut() {
            for (_, value) in samples {
                repair_value(store, value, local, entry_anchor, entry_moves)?;
            }
        }
        repair_fields(
            store,
            spec.metadata.make_mut(),
            local,
            entry_anchor,
            entry_moves,
        )?;
    }
    Ok(())
}
/// Whether `prefix`, an edited prim or property, is a prefix of the
/// authored path `path` as `SdfPath::HasPrefix` counts it: of its prim's
/// path, or for a property the property itself or a relational attribute
/// or mapper it owns (`/A.b` of `/A.b[/C].d`), but never of a path in
/// brackets. AOUSD Core §8 (paths).
pub(super) fn has_prefix(store: &dyn LayerStore, path: TargetPath, prefix: TargetPath) -> bool {
    match (prefix, path) {
        (TargetPath::Prim(prim), _) => store
            .paths()
            .resolve(prim)
            .is_prefix_of(store.paths().resolve(path.prim_path())),
        (TargetPath::Property(_), TargetPath::Prim(_)) => false,
        (TargetPath::Property(_), TargetPath::Property(property)) => {
            path == prefix
                || (is_relational(store.tokens(), property.property())
                    && relational_has_prefix(store.tokens(), store.paths(), property, prefix))
        }
    }
}

/// Fixes the `SdfPath`s that relationships' legacy `default`s hold
/// ([`Value::PathText`]) in `layer`, of a layer stack of `stages`, for the
/// edit of `from` (to `to`, or a delete), before any spec moves: as
/// OpenUSD does, once a composed property depends on the edit, its
/// default is replaced by `SdfPath::ReplacePrefix(from, to)` whether or not
/// `from` is its own prefix (`_PathReplacePrefix`,
/// `pxr/usd/usd/namespaceEditor.cpp`), which fixes the paths in its
/// brackets and, for a delete, empties it. The variants `dormant` lists
/// are left as they are, as no composed property reads them.
/// AOUSD Core §8 (paths).
pub(super) fn repair_path_defaults(
    store: &mut dyn LayerStore,
    stages: &[&Stage],
    layer: &mut Layer,
    from: TargetPath,
    to: Option<TargetPath>,
    dormant: &Dormant,
) {
    let holds_default = |properties: &[PropertyEntry]| {
        properties
            .iter()
            .any(|entry| matches!(entry.spec.default, Some(Value::PathText(_))))
    };
    for (path, spec) in layer.prims.iter_mut() {
        if holds_default(&spec.properties) {
            repair_defaults(store, stages, &mut spec.properties, *path, from, to);
        }
        let outer = spec.outer_variant_sites().to_vec();
        if let Some(rare) = spec.authored_rare_mut() {
            let mut sites = outer;
            repair_variant_defaults(
                store,
                stages,
                &mut rare.variant_sets,
                *path,
                (from, to),
                (&mut sites, dormant),
            );
        }
    }
    for (path, branches) in layer.variant_prims.iter_mut() {
        for spec in branches {
            let outer = spec.outer_variant_sites().to_vec();
            if dormant.contains(*path, &outer) {
                continue;
            }
            if holds_default(&spec.properties) {
                repair_defaults(store, stages, &mut spec.properties, *path, from, to);
            }
            if let Some(rare) = spec.authored_rare_mut() {
                let mut sites = outer;
                repair_variant_defaults(
                    store,
                    stages,
                    &mut rare.variant_sets,
                    *path,
                    (from, to),
                    (&mut sites, dormant),
                );
            }
        }
    }
}

/// [`repair_path_defaults`] in the variants of `sets`, held by the prim at
/// `path` inside the branches `sites`, but those `dormant` lists.
fn repair_variant_defaults(
    store: &mut dyn LayerStore,
    stages: &[&Stage],
    sets: &mut VariantSets,
    path: PathId,
    (from, to): (TargetPath, Option<TargetPath>),
    (sites, dormant): (&mut Vec<VariantSelectionSite>, &Dormant),
) {
    for (set, spec) in sets.iter_mut() {
        for (name, variant) in spec.variants.iter_mut() {
            sites.push(VariantSelectionSite {
                host_path: path,
                set: *set,
                variant: *name,
            });
            if !dormant.contains(path, sites) {
                repair_defaults(store, stages, &mut variant.properties, path, from, to);
                repair_variant_defaults(
                    store,
                    stages,
                    &mut variant.variant_sets,
                    path,
                    (from, to),
                    (sites, dormant),
                );
            }
            sites.pop();
        }
    }
}

/// [`repair_path_defaults`] for the properties of the prim at `prim`.
fn repair_defaults(
    store: &mut dyn LayerStore,
    stages: &[&Stage],
    properties: &mut [PropertyEntry],
    prim: PathId,
    from: TargetPath,
    to: Option<TargetPath>,
) {
    for entry in properties {
        let Some(Value::PathText(text)) = &entry.spec.default else {
            continue;
        };
        let composed = PropertyPath::new(prim, entry.name);
        if !default_depends(store, stages, &entry.spec, text, composed, from) {
            continue;
        }
        let (tokens, paths) = (store.tokens(), store.paths());
        let replaced = match to {
            Some(to) => replace_text_prefix(tokens, paths, text, from, to).map(Arc::from),
            None => Some(Arc::from("")),
        };
        if let Some(replaced) = replaced {
            Arc::make_mut(&mut entry.spec).default = Some(Value::PathText(replaced));
        }
    }
}

/// Whether the composed property `composed`, which `spec` (whose default
/// is `text`) contributes to, depends on the edit of `from`: whether
/// `from` is a prefix of a path any of its opinions authors, the targets
/// as composition maps them to the stage and the defaults of its stage's
/// own layer stack (`_DependencyCollector`, which visits every spec of
/// the composed property).
fn default_depends(
    store: &dyn LayerStore,
    stages: &[&Stage],
    spec: &crate::PropertySpec,
    text: &str,
    composed: PropertyPath,
    from: TargetPath,
) -> bool {
    let (tokens, paths) = (store.tokens(), store.paths());
    let authors = |spec: &crate::PropertySpec| {
        spec.targets
            .iter()
            .flat_map(|targets| targets.items())
            .any(|target| has_prefix(store, *target, from))
    };
    if text_has_prefix(tokens, paths, text, from) || authors(spec) {
        return true;
    }
    stages.iter().any(|stage| {
        stage
            .explain_property_path(composed)
            .into_iter()
            .flatten()
            .any(|opinion| {
                let Some(spec) = opinion.value().as_property() else {
                    return false;
                };
                let local = stage.layer_stack().contains(&opinion.layer_id());
                authors(spec)
                    || (local
                        && matches!(&spec.default, Some(Value::PathText(text))
                            if text_has_prefix(tokens, paths, text, from)))
            })
    })
}

fn path_names(store: &dyn LayerStore, path: PathId) -> Vec<alloc::string::String> {
    store
        .paths()
        .resolve(path)
        .segments()
        .iter()
        .map(|n| alloc::string::String::from(store.tokens().resolve(*n)))
        .collect()
}
fn store_root_path(store: &mut dyn LayerStore) -> PathId {
    store.paths().intern(Path::root())
}
fn repair_value(
    store: &mut dyn LayerStore,
    value: &mut Value,
    local: Option<&PathMove>,
    anchor: PathId,
    owner_moves: bool,
) -> Result<(), NamespaceError> {
    match value {
        Value::PathExpression(text) => {
            if let Some(mapping) = local {
                let expression = crate::PathExpression::parse(text).map_err(|_| {
                    NamespaceError::UnsupportedComposition(
                        "invalid authored path expression prevents namespace editing",
                    )
                })?;
                let source = path_names(store, mapping.from.prim_path());
                let destination = path_names(store, mapping.to.prim_path());
                let property = mapping
                    .from
                    .property_path()
                    .map(|p| store.tokens().resolve(p.property()));
                let new_property = mapping
                    .to
                    .property_path()
                    .map(|p| store.tokens().resolve(p.property()));
                let edited = expression.edit_namespace(
                    &source,
                    property,
                    &destination,
                    new_property,
                    &path_names(store, anchor),
                    owner_moves,
                );
                if edited != expression {
                    *text = Arc::from(edited.lossless_text());
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                repair_value(store, value, local, anchor, owner_moves)?;
            }
        }
        Value::Dictionary(values) => {
            for (_, value) in values {
                if let FieldValue::Value(value) = value {
                    repair_value(store, value, local, anchor, owner_moves)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn repair_fields(
    store: &mut dyn LayerStore,
    fields: &mut [FieldEntry],
    local: Option<&PathMove>,
    anchor: PathId,
    owner_moves: bool,
) -> Result<(), NamespaceError> {
    for field in fields {
        match &mut field.value {
            FieldValue::Value(value) => repair_value(store, value, local, anchor, owner_moves)?,
            FieldValue::PathListOp(paths) => {
                if let Some(mapping) = local {
                    map_list(paths, |p| *p = mapping.target(store, *p));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Variant specs no composed prim index uses, by the path of their prim
/// and the branches enclosing them (for a variant spec, ending with its
/// own selection; none for a prim spec of a layer outside every layer
/// stack, which no arc reaches): a namespace edit moves them with their
/// prim, but the paths they author stay as they are.
///
/// OpenUSD fixes path-bearing fields only for the composed objects that
/// depend on the edited path (`_GatherPathBearingFieldEditsForStage`,
/// from the specs `Usd_Resolver` visits for every prim) and composition
/// arcs only where a prim index depends on them
/// (`PcpGatherDependentNamespaceEdits`); neither visits a variant no prim
/// index selects. AOUSD Core §10.5 (variant selection).
#[derive(Default)]
pub(super) struct Dormant(crate::HashSet<(PathId, Vec<VariantSelectionSite>)>);

impl Dormant {
    pub(super) fn insert(&mut self, path: PathId, sites: Vec<VariantSelectionSite>) {
        self.0.insert((path, sites));
    }
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(super) fn contains(&self, path: PathId, sites: &[VariantSelectionSite]) -> bool {
        !self.0.is_empty() && self.0.contains(&(path, sites.to_vec()))
    }
    /// The specs after `mapping` moves those it carries inside the
    /// branches `within` ([`move_specs`]).
    pub(super) fn after(
        self,
        store: &mut dyn LayerStore,
        mapping: &PathMove,
        within: &[VariantSelectionSite],
    ) -> Self {
        if self.0.is_empty() || mapping.from.property_path().is_some() {
            return self;
        }
        let from = store.paths().resolve(mapping.from.prim_path()).clone();
        let mut out = Self::default();
        for (path, mut sites) in self.0 {
            let carried = from.is_prefix_of(store.paths().resolve(path))
                && sites.len() >= within.len()
                && sites[..within.len()] == *within
                && sites[within.len()..]
                    .iter()
                    .all(|site| from.is_prefix_of(store.paths().resolve(site.host_path)));
            if !carried {
                out.0.insert((path, sites));
                continue;
            }
            for site in &mut sites {
                site.host_path = mapping.prim(store, site.host_path);
            }
            out.0.insert((mapping.prim(store, path), sites));
        }
        out
    }
}

#[allow(clippy::too_many_arguments, reason = "the repair's whole context")]
pub(super) fn repair_prim(
    store: &mut dyn LayerStore,
    spec: &mut PrimSpec,
    path: PathId,
    author: LayerId,
    edited: LayerId,
    local: Option<&PathMove>,
    source: &PathMove,
    dormant: &Dormant,
) -> Result<(), NamespaceError> {
    let outer = spec.outer_variant_sites();
    if dormant.contains(path, outer) {
        return Ok(());
    }
    let outer = outer.to_vec();
    let anchor = local.map_or(path, |mapping| {
        PathMove::new(mapping.to, mapping.from).prim(store, path)
    });
    let owner_moves = anchor != path;
    repair_properties(store, &mut spec.properties, local, anchor, owner_moves)?;
    repair_fields(store, &mut spec.fields, local, anchor, owner_moves)?;
    if let Some(op) = spec.references.get_mut() {
        repair_refs(store, op, author, edited, local, source)?;
    }
    if let Some(op) = spec.payloads.get_mut() {
        repair_refs(store, op, author, edited, local, source)?;
    }
    let Some(rare) = spec.authored_rare_mut() else {
        return Ok(());
    };
    if let Some(mapping) = local {
        if let Some(op) = rare.inherits.get_mut() {
            map_list(op, |p| *p = mapping.prim(store, *p));
        }
        if let Some(op) = rare.specializes.get_mut() {
            map_list(op, |p| *p = mapping.prim(store, *p));
        }
    }
    let mut sites = Branch {
        host: path,
        sites: outer,
        dormant,
    };
    repair_variants(
        store,
        &mut rare.variant_sets,
        author,
        edited,
        local,
        source,
        anchor,
        owner_moves,
        &mut sites,
    )
}
/// The branch a variant spec being repaired is authored in.
struct Branch<'a> {
    host: PathId,
    sites: Vec<VariantSelectionSite>,
    dormant: &'a Dormant,
}
#[allow(clippy::too_many_arguments, reason = "the repair's whole context")]
fn repair_variants(
    store: &mut dyn LayerStore,
    sets: &mut VariantSets,
    author: LayerId,
    edited: LayerId,
    local: Option<&PathMove>,
    source: &PathMove,
    anchor: PathId,
    owner_moves: bool,
    branch: &mut Branch<'_>,
) -> Result<(), NamespaceError> {
    for (set, spec) in sets.iter_mut() {
        for (variant, v) in spec.variants.iter_mut() {
            branch.sites.push(VariantSelectionSite {
                host_path: branch.host,
                set: *set,
                variant: *variant,
            });
            if !branch.dormant.contains(branch.host, &branch.sites) {
                repair_variant(
                    store,
                    v,
                    author,
                    edited,
                    local,
                    source,
                    anchor,
                    owner_moves,
                    branch,
                )?;
            }
            branch.sites.pop();
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments, reason = "the repair's whole context")]
fn repair_variant(
    store: &mut dyn LayerStore,
    spec: &mut VariantSpec,
    author: LayerId,
    edited: LayerId,
    local: Option<&PathMove>,
    source: &PathMove,
    anchor: PathId,
    owner_moves: bool,
    branch: &mut Branch<'_>,
) -> Result<(), NamespaceError> {
    repair_properties(store, &mut spec.properties, local, anchor, owner_moves)?;
    repair_fields(store, &mut spec.fields, local, anchor, owner_moves)?;
    if let Some(op) = spec.references.get_mut() {
        repair_refs(store, op, author, edited, local, source)?;
    }
    if let Some(op) = spec.payloads.get_mut() {
        repair_refs(store, op, author, edited, local, source)?;
    }
    if let Some(mapping) = local {
        if let Some(op) = spec.inherits.get_mut() {
            map_list(op, |p| *p = mapping.prim(store, *p));
        }
        if let Some(op) = spec.specializes.get_mut() {
            map_list(op, |p| *p = mapping.prim(store, *p));
        }
    }
    repair_variants(
        store,
        &mut spec.variant_sets,
        author,
        edited,
        local,
        source,
        anchor,
        owner_moves,
        branch,
    )
}
fn push(transaction: &mut Transaction, step: Raw, written: Raw) {
    transaction
        .ops
        .push(Op::Raw(Box::new(Guarded { step, written })));
}
fn append_delta(
    original: &Layer,
    edited: &Layer,
    transaction: &mut Transaction,
    specs: &mut Vec<(LayerId, SpecPath)>,
) {
    let mut paths: Vec<_> = original
        .prims
        .keys()
        .chain(original.variant_prims.keys())
        .chain(edited.prims.keys())
        .chain(edited.variant_prims.keys())
        .copied()
        .collect();
    sort::sort_unstable(&mut paths);
    paths.dedup();
    for path in paths {
        let before = Raw::PrimSlots {
            layer: original.id,
            path,
            main: original.prims.get(&path).cloned(),
            branches: original.variant_prims.get(&path).cloned(),
        };
        let after = Raw::PrimSlots {
            layer: edited.id,
            path,
            main: edited.prims.get(&path).cloned(),
            branches: edited.variant_prims.get(&path).cloned(),
        };
        if !original
            .prims
            .get(&path)
            .cloned()
            .same(&edited.prims.get(&path).cloned())
            || !original
                .variant_prims
                .get(&path)
                .cloned()
                .same(&edited.variant_prims.get(&path).cloned())
        {
            push(transaction, after, before);
            specs.push((original.id, SpecPath::from_prim_path(path)));
        }
    }
}
