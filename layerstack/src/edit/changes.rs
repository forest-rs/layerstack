// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a composed stage reports as changed: [`Changes`].

use alloc::vec::Vec;
use core::cmp::Ordering;

use hashbrown::HashMap;

use super::PrimField;
use crate::{
    interner::{TokenId, TokenInterner},
    path::{PathId, PathInterner, PropertyPath},
    sort,
};

/// Composed changes made by one successful live transaction, or by any
/// other change a [`LiveStage`](crate::LiveStage) reports.
///
/// As OpenUSD's `UsdNotice::ObjectsChanged` does, it tells prims from
/// properties, and resyncs from info-only changes:
///
/// - [`Self::resynced`] and [`Self::resynced_properties`] are resyncs:
///   everything of such a prim and its subtree, or of such a property, may
///   have changed, its existence included. Resynced prims are minimal
///   subtree roots; [`Self::created`] and [`Self::removed`] are exact
///   inventories of the prims that came and went, beneath them.
/// - [`Self::changed_info_only`] lists the prims whose own information
///   changed (their metadata, or their spec stacks) without a resync, with
///   the fields it changed in [`Self::prim_fields`]. Their children and
///   properties did not change for it.
/// - [`Self::changed_properties`] lists the properties changed info-only,
///   each with each field changed. A property's prim is not listed for it.
///
/// So a consumer can tell "this prim's own metadata changed, and these of
/// its properties" apart from "only these properties changed": a draw
/// list that reads a mesh's `points` refreshes that mesh when
/// `points` is among its changed properties, and skips it when only
/// `documentation` changed on it.
///
/// Info-only paths, prims and properties, never lie beneath a resynced
/// prim, and a changed property is never a resynced one. A changed field
/// names an authored operation, not a difference of resolved values: a
/// changed opinion may be masked by a stronger one. A conservative full
/// rebuild reports the pseudo-root as resynced, rather than every survivor
/// as changed.
///
/// A property spec added or removed by a transaction resyncs that property
/// alone, unless the transaction otherwise changes its prim's composition
/// or namespace (by creating the prim spec, or a namespace edit, or an
/// applied-schema change), which resyncs the prim. A consumer caching
/// anything a property contributes, its prim's property names included,
/// reads [`Self::resynced_properties`] as well as the prim lists. External
/// opinion notifications lack edit details and conservatively resync their
/// affected prims, including other prims updated in the same batch.
///
/// A prim created or removed is resynced alone: its parent, whose child
/// list gained or lost it, is not listed, as OpenUSD does not list it (a
/// parent's `primChildren` records no field change,
/// `SdfLayer::_PrimPushChild`). A consumer caching a prim's children
/// updates them from the resyncs (and [`Self::created`],
/// [`Self::removed`]) of its children. Reordering children (`primOrder`)
/// resyncs the parent; a spec added to or removed from a weaker layer can
/// move a child too, as child names compose weakest layer first, and
/// resyncs that child.
///
/// An inert spec (an `over` with no opinions of its own) added or removed
/// for a prim that composes before and after changes only its prim's spec
/// stack, and that of each prim composing from it through an arc: they
/// are listed info-only with no field, as OpenUSD lists them. So is a prim
/// whose new `over` holds only property specs, which are resynced, or
/// metadata that composes nothing. A layer read again from its source
/// ([`Transaction::mark_reloaded`](crate::Transaction::mark_reloaded))
/// lists the prims composing a spec at a child of its pseudo-root (the
/// pseudo-root for a layer of the stage's own layer stack) info-only with
/// no field, beside the specs it changed, as OpenUSD announces the reload
/// (`SdfChangeList::DidReloadLayerContent`). A sublayer inserted or
/// removed lists the pseudo-root, with the `subLayers` and
/// `subLayerOffsets` fields, when the stage's own layer stack holds the
/// edited layer, or else each prim composing from the edited layer's layer
/// stack, with those fields, as any change of a layer's own fields lists
/// them; and the prims whose spec stacks gained or lost only inert specs
/// (`over`s, properties aside), in whichever layer stack it joins or
/// leaves; the prims it defines, or whose arcs it changes, are resynced. Reordering a layer's sublayers counts as removing and
/// inserting every one of them, as OpenUSD announces it. A change to stage
/// metadata that composes nothing (`documentation`, `startTimeCode`,
/// `metersPerUnit` and the like) lists the pseudo-root with that field.
///
/// Three differences from OpenUSD are deliberate: a sublayer that moves a
/// child among its siblings resyncs that child, where OpenUSD reports it
/// info-only and keeps a stale child order (OpenUSD bug B037); inserting a
/// muted sublayer lists `/` alone, where OpenUSD resyncs the prims it
/// defines, which compose neither before nor after (OpenUSD bug B038); and
/// for a layer a reference reaches that is a sublayer too, a reorder of
/// its sublayer list resyncs the referencing prims as well (the spec
/// stacks compared do not record the layer stack a spec came through), and
/// its removal does not resync them, where OpenUSD does though they
/// compose the same.
///
/// Every list is in namespace order ([`PathInterner::cmp_paths`]) and
/// contains no duplicates; [`Self::prim_fields`] follows its prims, then
/// field order, and [`Self::changed_properties`] and
/// [`Self::resynced_properties`] order by prim, then property name (then
/// field). The field lists are flat, one allocation each however many
/// prims or properties change. The per-prim queries
/// ([`Self::properties_of`], [`Self::fields_of_prim`] and the like)
/// binary-search these orders, which is why they take the interners: a
/// query costs O(log n) path comparisons however large the notice, and a
/// list edited out of its order answers them wrongly.
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
/// assert!(added.changes.is_resynced(rock, store.paths()));
/// let undone = live.apply(&mut store, &added.inverse).unwrap();
/// assert_eq!(undone.changes.removed, [rock]);
/// ```
///
/// OpenUSD: `UsdNotice::ObjectsChanged`: `GetResyncedPaths` (prims and
/// properties), `GetChangedInfoOnlyPaths` (prims and properties) and
/// `GetChangedFields`, with the same subtree versus exact-path
/// distinction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[doc(alias = "UsdNotice::ObjectsChanged")]
#[doc(alias = "ObjectsChanged")]
#[non_exhaustive]
pub struct Changes {
    /// Prim paths present after the change but absent before it.
    pub created: Vec<PathId>,
    /// Prim paths present before the change but absent after it.
    pub removed: Vec<PathId>,
    /// Minimal roots whose composed structure may have changed: every
    /// prim and property beneath one may have changed in any way.
    ///
    /// OpenUSD: the prim paths of `UsdNotice::ObjectsChanged::GetResyncedPaths`.
    pub resynced: Vec<PathId>,
    /// Properties whose composed spec stacks may have changed, on prims
    /// that are not themselves resynced: a property spec was added,
    /// removed or renamed. Everything about such a property may have
    /// changed, its existence and declaration included; its prim's other
    /// properties and its prim's own fields did not. Never beneath a path
    /// in [`Self::resynced`].
    ///
    /// OpenUSD: `UsdNotice::ObjectsChanged::GetResyncedPaths` lists such a
    /// property's path, not its prim's (`UsdStage::_HandleLayersDidChange`
    /// resyncs the property paths of `SdfChangeList` entries that add or
    /// remove property specs).
    pub resynced_properties: Vec<PropertyPath>,
    /// Surviving prims whose own information changed without a resync:
    /// their metadata, or their spec stacks. The fields changed are in
    /// [`Self::prim_fields`]; a prim listed with none changed its spec
    /// stack (an inert spec came or went) or was read again from its
    /// source. Its children and its properties did not change for it.
    ///
    /// OpenUSD: the prim paths of
    /// `UsdNotice::ObjectsChanged::GetChangedInfoOnlyPaths`.
    pub changed_info_only: Vec<PathId>,
    /// The fields changed of the prims in [`Self::changed_info_only`], one
    /// entry per prim and field.
    ///
    /// OpenUSD: `UsdNotice::ObjectsChanged::GetChangedFields` of a prim path.
    pub prim_fields: Vec<PrimFieldChange>,
    /// Properties changed info-only, one entry per property and field,
    /// on prims neither resynced nor beneath a resync. A property listed
    /// here is not in [`Self::resynced_properties`].
    ///
    /// OpenUSD: the property paths of
    /// `UsdNotice::ObjectsChanged::GetChangedInfoOnlyPaths`, each with its
    /// `GetChangedFields`.
    pub changed_properties: Vec<PropertyFieldChange>,
}

/// An authored field of a prim's own information.
///
/// These identify authored operations, not differences between resolved
/// values. AOUSD Core §7.6.2 (prim spec fields); OpenUSD
/// `UsdNotice::ObjectsChanged::GetChangedFields`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PrimInfoField {
    /// A field kept in a dedicated member of the prim spec.
    Field(PrimField),
    /// A named metadata field: the prim's, or, for the pseudo-root, a
    /// layer's (`documentation`, `startTimeCode` and the like).
    Metadata(TokenId),
    /// The `subLayers` of a layer of the stage's own layer stack, reported
    /// at the pseudo-root.
    Sublayers,
    /// The `subLayerOffsets` of a layer of the stage's own layer stack,
    /// reported at the pseudo-root.
    SublayerOffsets,
}

impl PrimInfoField {
    /// OpenUSD's name for the field: its `SdfFieldKeys` token, or the
    /// metadata field's own name.
    #[must_use]
    pub fn name<'a>(&self, tokens: &'a TokenInterner) -> &'a str {
        match self {
            Self::Metadata(key) => tokens.resolve(*key),
            Self::Sublayers => "subLayers",
            Self::SublayerOffsets => "subLayerOffsets",
            Self::Field(field) => match field {
                PrimField::Specifier => "specifier",
                PrimField::TypeName => "typeName",
                PrimField::Active => "active",
                PrimField::Instanceable => "instanceable",
                PrimField::PrimOrder => "primOrder",
                PrimField::PropertyOrder => "propertyOrder",
                PrimField::References => "references",
                PrimField::Payloads => "payload",
                PrimField::Inherits => "inheritPaths",
                PrimField::Specializes => "specializes",
                PrimField::VariantSetNames => "variantSetNames",
            },
        }
    }
}

/// One changed field of a composed prim's own information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PrimFieldChange {
    /// The composed prim.
    pub prim: PathId,
    /// The field changed.
    pub field: PrimInfoField,
}

/// An authored property field changed by a transaction.
///
/// These identify authored operations, not differences between resolved values.
/// AOUSD Core §12 (properties); OpenUSD `UsdNotice::ObjectsChanged::GetChangedFields`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    Metadata(TokenId),
}

/// One changed field of a composed property.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PropertyFieldChange {
    /// The composed property.
    pub property: PropertyPath,
    /// The field changed.
    pub field: PropertyField,
}

/// A property field change at a source spec, before it is mapped to the
/// composed properties it reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PropertyChange {
    /// Property name.
    pub(crate) name: TokenId,
    /// Authored field affected by the operation.
    pub(crate) field: PropertyField,
}

/// The run of `list`, ordered by the prims `prim_of` gives in namespace
/// order ([`PathInterner::cmp_paths`]), whose prim is `prim`: two binary
/// searches, O(log n) path comparisons.
fn run_of<'a, T>(
    list: &'a [T],
    prim: PathId,
    prim_of: impl Fn(&T) -> PathId,
    paths: &PathInterner,
    tokens: &TokenInterner,
) -> &'a [T] {
    let start = list
        .partition_point(|entry| paths.cmp_paths(prim_of(entry), prim, tokens) == Ordering::Less);
    let len = list[start..].partition_point(|entry| prim_of(entry) == prim);
    &list[start..start + len]
}

/// Compares fields by declaration order, metadata fields by name.
fn cmp_property_fields(a: PropertyField, b: PropertyField, tokens: &TokenInterner) -> Ordering {
    match (a, b) {
        (PropertyField::Metadata(a), PropertyField::Metadata(b)) => {
            tokens.resolve(a).cmp(tokens.resolve(b))
        }
        (a, b) => a.cmp(&b),
    }
}

impl Changes {
    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.created.is_empty()
            && self.removed.is_empty()
            && self.resynced.is_empty()
            && self.resynced_properties.is_empty()
            && self.changed_info_only.is_empty()
            && self.changed_properties.is_empty()
    }

    /// Whether `path` is resynced: it, or an ancestor of it, is in
    /// [`Self::resynced`]. A walk up `path`'s ancestors, each looked up in
    /// the resynced roots: O(depth × resynced roots).
    ///
    /// OpenUSD: `UsdNotice::ObjectsChanged::ResyncedObject`.
    #[must_use]
    pub fn is_resynced(&self, path: PathId, paths: &PathInterner) -> bool {
        if self.resynced.is_empty() {
            return false;
        }
        let mut current = Some(path);
        while let Some(at) = current {
            if self.resynced.contains(&at) {
                return true;
            }
            current = paths.parent(at);
        }
        false
    }

    /// Whether the property `property` is resynced: it is in
    /// [`Self::resynced_properties`], or its prim is resynced
    /// ([`Self::is_resynced`]). O(resynced properties + depth × resynced
    /// roots).
    #[must_use]
    pub fn is_property_resynced(&self, property: PropertyPath, paths: &PathInterner) -> bool {
        self.resynced_properties.contains(&property)
            || self.is_resynced(property.prim_path(), paths)
    }

    /// Whether `prim`'s own information changed info-only: it is in
    /// [`Self::changed_info_only`]. A binary search of the list in
    /// namespace order, so it takes the interners: O(log n) path
    /// comparisons.
    ///
    /// OpenUSD: `UsdNotice::ObjectsChanged::ChangedInfoOnly` of a prim.
    #[must_use]
    pub fn prim_info_changed(
        &self,
        prim: PathId,
        paths: &PathInterner,
        tokens: &TokenInterner,
    ) -> bool {
        !run_of(&self.changed_info_only, prim, |at| *at, paths, tokens).is_empty()
    }

    /// The fields changed of `prim`'s own information
    /// ([`Self::prim_fields`]): none for a prim not in
    /// [`Self::changed_info_only`], or listed there without a field. A
    /// binary search of the list, ordered by prim in namespace order:
    /// O(log n) path comparisons, then one step a field.
    ///
    /// OpenUSD: `UsdNotice::ObjectsChanged::GetChangedFields` of a prim path.
    pub fn fields_of_prim(
        &self,
        prim: PathId,
        paths: &PathInterner,
        tokens: &TokenInterner,
    ) -> impl ExactSizeIterator<Item = PrimInfoField> + '_ {
        run_of(&self.prim_fields, prim, |change| change.prim, paths, tokens)
            .iter()
            .map(|change| change.field)
    }

    /// The info-only property changes of `prim`
    /// ([`Self::changed_properties`]), in order: one run of the list, as
    /// it is ordered by prim. Empty when none of its properties changed
    /// info-only. A binary search of the list in namespace order: O(log n)
    /// path comparisons, however many of the prim's properties changed.
    #[must_use]
    pub fn properties_of(
        &self,
        prim: PathId,
        paths: &PathInterner,
        tokens: &TokenInterner,
    ) -> &[PropertyFieldChange] {
        run_of(
            &self.changed_properties,
            prim,
            |change| change.property.prim_path(),
            paths,
            tokens,
        )
    }

    /// The fields changed of `property` info-only
    /// ([`Self::changed_properties`]). A binary search for its prim's
    /// changes ([`Self::properties_of`]), then for its name among them:
    /// O(log n) comparisons, then one step a field.
    ///
    /// OpenUSD: `UsdNotice::ObjectsChanged::GetChangedFields` of a property path.
    pub fn fields_of_property(
        &self,
        property: PropertyPath,
        paths: &PathInterner,
        tokens: &TokenInterner,
    ) -> impl ExactSizeIterator<Item = PropertyField> + '_ {
        let changes = self.properties_of(property.prim_path(), paths, tokens);
        let name = property.property();
        let text = tokens.resolve(name);
        let start =
            changes.partition_point(|change| tokens.resolve(change.property.property()) < text);
        let len = changes[start..].partition_point(|change| change.property.property() == name);
        changes[start..start + len]
            .iter()
            .map(|change| change.field)
    }

    /// Whether anything of `prim` may have changed: it is resynced, or it
    /// changed info-only, or a property of it did, either way. A walk up
    /// its ancestors in the resynced roots ([`Self::is_resynced`]), then a
    /// binary search of each other list: O(depth × resynced roots + log n)
    /// path comparisons.
    ///
    /// OpenUSD: `UsdNotice::ObjectsChanged::AffectedObject` of the prim
    /// or any of its properties.
    #[must_use]
    pub fn affects_prim(&self, prim: PathId, paths: &PathInterner, tokens: &TokenInterner) -> bool {
        self.is_resynced(prim, paths)
            || self.prim_info_changed(prim, paths, tokens)
            || !self.properties_of(prim, paths, tokens).is_empty()
            || !run_of(
                &self.resynced_properties,
                prim,
                |property| property.prim_path(),
                paths,
                tokens,
            )
            .is_empty()
    }

    /// Whether anything at or beneath `root` may have changed: a prim or
    /// property path in any list is `root` or beneath it, or `root` is
    /// beneath a resync.
    #[must_use]
    pub fn affects_subtree(&self, root: PathId, paths: &PathInterner) -> bool {
        let beneath = |path: PathId| {
            let mut current = Some(path);
            while let Some(at) = current {
                if at == root {
                    return true;
                }
                current = paths.parent(at);
            }
            false
        };
        self.is_resynced(root, paths)
            || self.resynced.iter().any(|p| beneath(*p))
            || self.changed_info_only.iter().any(|p| beneath(*p))
            || self
                .resynced_properties
                .iter()
                .any(|p| beneath(p.prim_path()))
            || self
                .changed_properties
                .iter()
                .any(|c| beneath(c.property.prim_path()))
    }

    /// Every prim with an info-only change, its own or a property's
    /// ([`Self::changed_info_only`] and the prims of
    /// [`Self::changed_properties`]), in namespace order, each once.
    #[must_use]
    pub fn info_only_prims(&self, paths: &PathInterner, tokens: &TokenInterner) -> Vec<PathId> {
        let mut prims = self.changed_info_only.clone();
        let mut last = None;
        for change in &self.changed_properties {
            let prim = change.property.prim_path();
            if last != Some(prim) {
                prims.push(prim);
                last = Some(prim);
            }
        }
        paths.sort_paths(&mut prims, tokens);
        prims
    }

    /// Removes the info-only entries and property resyncs of the prims
    /// `beneath` holds for (those at or beneath a resynced root), as a
    /// resync covers them.
    pub(crate) fn retain_outside(&mut self, beneath: impl Fn(PathId) -> bool) {
        self.changed_info_only.retain(|prim| !beneath(*prim));
        self.prim_fields.retain(|change| !beneath(change.prim));
        self.resynced_properties
            .retain(|path| !beneath(path.prim_path()));
        self.changed_properties
            .retain(|change| !beneath(change.property.prim_path()));
    }

    /// Lists `prim` info-only, with `fields`, unless listed already.
    pub(crate) fn add_prim_info(
        &mut self,
        prim: PathId,
        fields: impl IntoIterator<Item = PrimInfoField>,
    ) {
        if !self.changed_info_only.contains(&prim) {
            self.changed_info_only.push(prim);
        }
        self.prim_fields.extend(
            fields
                .into_iter()
                .map(|field| PrimFieldChange { prim, field }),
        );
    }

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
            + bytes(&self.prim_fields, allocated)
            + bytes(&self.changed_properties, allocated)
    }

    /// Puts every list in the order [`Changes`] documents, removes
    /// duplicates, drops the field entries of prims no longer listed, and
    /// the info-only property entries of properties resynced.
    ///
    /// OpenUSD reports changed paths in `SdfPath` order
    /// (`UsdNotice::ObjectsChanged`, a map keyed by `SdfPath`).
    pub(crate) fn sort(&mut self, paths: &PathInterner, tokens: &TokenInterner) {
        for list in [
            &mut self.created,
            &mut self.removed,
            &mut self.resynced,
            &mut self.changed_info_only,
        ] {
            paths.sort_paths(list, tokens);
        }
        // Each prim's place in namespace order, so that the flat lists sort
        // by it without comparing paths again for every entry.
        let mut prims: Vec<PathId> = self
            .resynced_properties
            .iter()
            .map(|p| p.prim_path())
            .chain(
                self.changed_properties
                    .iter()
                    .map(|c| c.property.prim_path()),
            )
            .collect();
        paths.sort_paths(&mut prims, tokens);
        let rank: HashMap<PathId, usize> = prims
            .iter()
            .enumerate()
            .map(|(at, prim)| (*prim, at))
            .collect();
        let info_rank: HashMap<PathId, usize> = self
            .changed_info_only
            .iter()
            .enumerate()
            .map(|(at, prim)| (*prim, at))
            .collect();
        let cmp_property = |a: &PropertyPath, b: &PropertyPath| {
            rank[&a.prim_path()]
                .cmp(&rank[&b.prim_path()])
                .then_with(|| {
                    tokens
                        .resolve(a.property())
                        .cmp(tokens.resolve(b.property()))
                })
        };
        sort::sort_by(&mut self.resynced_properties, cmp_property);
        self.resynced_properties.dedup();
        self.prim_fields
            .retain(|change| info_rank.contains_key(&change.prim));
        sort::sort_by(&mut self.prim_fields, |a, b| {
            info_rank[&a.prim]
                .cmp(&info_rank[&b.prim])
                .then_with(|| a.field.name(tokens).cmp(b.field.name(tokens)))
        });
        self.prim_fields.dedup();
        if !self.resynced_properties.is_empty() {
            let resynced: hashbrown::HashSet<PropertyPath> =
                self.resynced_properties.iter().copied().collect();
            self.changed_properties
                .retain(|change| !resynced.contains(&change.property));
        }
        sort::sort_by(&mut self.changed_properties, |a, b| {
            cmp_property(&a.property, &b.property)
                .then_with(|| cmp_property_fields(a.field, b.field, tokens))
        });
        self.changed_properties.dedup();
    }
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec::Vec};

    use crate::{
        EditTarget, FieldValue, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec,
        PropertyType, StageOptions, Transaction, Value,
    };

    /// The per-prim queries binary-search lists in namespace order; they
    /// answer what a scan of the lists does, for prims interned in another
    /// order than namespace order (`/P10` before `/P2`, children after
    /// their parents' siblings) and for prims the lists do not hold.
    #[test]
    fn queries_find_what_a_scan_finds() {
        let mut store = InMemoryStore::default();
        let double = PropertyType::parse("double").unwrap();
        let mut layer = Layer::new(LayerId(1));
        let mut prims = Vec::new();
        for at in (0..12).rev() {
            prims.push(store.path(&format!("/P{at}")));
        }
        for at in 0..12 {
            prims.push(store.path(&format!("/P{at}/C")));
        }
        let names: Vec<_> = ["b", "a", "c10", "c2"]
            .iter()
            .map(|name| store.tokens.intern(name))
            .collect();
        for &prim in &prims {
            let mut spec = PrimSpec::def();
            for &name in &names {
                spec = spec.with_property(
                    name,
                    PropertySpec::typed_attribute(double.clone()).with_default(Value::Double(0.0)),
                );
            }
            layer.insert_prim(prim, spec);
        }
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let documentation = store.tokens.intern("documentation");
        let comment = store.tokens.intern("comment");
        let mut edit = Transaction::new();
        for (at, &prim) in prims.iter().enumerate() {
            let target = EditTarget::for_layer(LayerId(1));
            if at % 3 != 0 {
                edit.set_metadata(
                    target.prim(prim),
                    documentation,
                    FieldValue::Value(Value::string("noted")),
                );
            }
            if at % 4 == 1 {
                edit.set_metadata(
                    target.prim(prim),
                    comment,
                    FieldValue::Value(Value::string("noted")),
                );
            }
            for (n, &name) in names.iter().enumerate() {
                if (at + n) % 2 == 0 {
                    edit.set_default(
                        target.property(crate::PropertyPath::new(prim, name)),
                        Value::Double(1.0),
                    );
                }
            }
        }
        let changes = live.apply(&mut store, &edit).unwrap().changes;
        assert!(!changes.prim_fields.is_empty() && !changes.changed_properties.is_empty());
        let (paths, tokens) = (&store.paths, &store.tokens);
        let absent = store.path("/Elsewhere");
        for &prim in prims.iter().chain([&absent]) {
            let scanned: Vec<_> = changes
                .changed_properties
                .iter()
                .filter(|change| change.property.prim_path() == prim)
                .copied()
                .collect();
            assert_eq!(changes.properties_of(prim, paths, tokens), scanned);
            let scanned: Vec<_> = changes
                .prim_fields
                .iter()
                .filter(|change| change.prim == prim)
                .map(|change| change.field)
                .collect();
            let found: Vec<_> = changes.fields_of_prim(prim, paths, tokens).collect();
            assert_eq!(found, scanned);
            assert_eq!(
                changes.prim_info_changed(prim, paths, tokens),
                changes.changed_info_only.contains(&prim)
            );
            assert_eq!(
                changes.affects_prim(prim, paths, tokens),
                prim != absent,
                "{}",
                paths.display(prim, tokens)
            );
            for &name in &names {
                let property = crate::PropertyPath::new(prim, name);
                let scanned: Vec<_> = changes
                    .changed_properties
                    .iter()
                    .filter(|change| change.property == property)
                    .map(|change| change.field)
                    .collect();
                let found: Vec<_> = changes
                    .fields_of_property(property, paths, tokens)
                    .collect();
                assert_eq!(found, scanned);
            }
        }
    }
}
