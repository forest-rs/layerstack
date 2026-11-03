// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The transaction that gives a layer another layer's content, spec by spec.

use alloc::{boxed::Box, vec::Vec};

use super::{
    apply::{AnimationState, Guarded, Raw},
    same::Same,
    spec::Loc,
    transaction::{Op, Transaction},
};
use crate::{
    doc::{
        FieldEntry, FieldValue, Layer, LayerId, LayerStore, PrimSpec, Reference, Value, VariantSets,
    },
    interner::TokenId,
    listop::ListOp,
    path::PathId,
    property::PropertySpec,
};

impl Transaction {
    /// The edits that make the layer `layer` of `store` hold the content of
    /// `content`, spec by spec, or `None` when the store holds no layer
    /// `layer`. Applied, the layer's sublayers, relocates, metadata and
    /// prim specs equal `content`'s; its id, generations and history stay
    /// its own. `content` must use `store`'s interners, as a layer read with
    /// them does; its own id is not read.
    ///
    /// An arc or sublayer the layer holds unresolved, which the store binds
    /// to the layer `content`'s resolves to ([`LayerStore::asset_layer`]),
    /// or the reverse, is the same arc: a layer read while its
    /// dependencies were not supplied yet holds its arcs deferred, and the
    /// same file read again once they are holds them resolved.
    ///
    /// Only what differs is edited: a layer field that changed, and the
    /// prim specs (with their variant branches) that were added, removed or
    /// changed, each written whole. So the changes a
    /// [`LiveStage`](crate::LiveStage) reports for the transaction name only
    /// those specs: a prim whose spec changed only in its properties reports
    /// those properties (field changes info-only, specs added or removed
    /// resynced), a prim added or removed is resynced, and an unchanged
    /// layer gives an empty transaction ([`Self::is_empty`]). Each edit is
    /// guarded by the content it replaces, so the transaction fails with
    /// [`EditError::StaleValue`](super::EditError::StaleValue) and changes
    /// nothing if the layer changes between this call and its application.
    /// Values compare as authored state, as guards do: an unchanged NaN is
    /// unchanged.
    ///
    /// A layer read again from its file is published this way, so that a
    /// reload reports what the file changed rather than resyncing the
    /// stage. How many `subLayerOffsets` the layer was read with is not
    /// carried when its sublayers change ([`Layer::unpaired_sublayer_offsets`]).
    ///
    /// OpenUSD: `SdfLayer::TransferContent`, and the per-spec difference
    /// `SdfLayer::_SetData` applies when `SdfLayer::Reload` reads a changed
    /// file (`pxr/usd/sdf/layer.cpp`), whose change list names each spec it
    /// adds, removes or changes. Spec: AOUSD Core §7 (layers and specs).
    #[doc(alias = "TransferContent")]
    #[must_use]
    pub fn transfer_content(
        store: &dyn LayerStore,
        layer: LayerId,
        content: &Layer,
    ) -> Option<Self> {
        let current = store.layer(layer)?;
        let normalized = bound_forms(store, layer, current, content);
        let content = normalized.as_ref().unwrap_or(content);
        let mut ops = Vec::new();
        let mut guarded = |step: Raw, written: Raw| {
            ops.push(Op::Raw(Box::new(Guarded { step, written })));
        };
        if current.sublayers != content.sublayers {
            guarded(
                Raw::Sublayers {
                    layer,
                    sublayers: content.sublayers.clone(),
                },
                Raw::Sublayers {
                    layer,
                    sublayers: current.sublayers.clone(),
                },
            );
        }
        if current.relocates != content.relocates {
            guarded(
                Raw::LayerRelocates {
                    layer,
                    relocates: content.relocates.clone(),
                },
                Raw::LayerRelocates {
                    layer,
                    relocates: current.relocates.clone(),
                },
            );
        }
        layer_fields(store, layer, current, content, &mut guarded);
        let mut paths: Vec<PathId> = current
            .prims
            .keys()
            .chain(content.prims.keys())
            .chain(current.variant_prims.keys())
            .chain(content.variant_prims.keys())
            .copied()
            .collect();
        crate::sort::sort_unstable(&mut paths);
        paths.dedup();
        for path in paths {
            let (main, branches) = (current.prims.get(&path), current.variant_prims.get(&path));
            let (new_main, new_branches) =
                (content.prims.get(&path), content.variant_prims.get(&path));
            if same(main, new_main) && same(branches, new_branches) {
                continue;
            }
            if same(branches, new_branches)
                && let (Some(old), Some(new)) = (main, new_main)
                && let Some(steps) = spec_steps(layer, path, old, new)
            {
                for (step, written) in steps {
                    guarded(step, written);
                }
                continue;
            }
            guarded(
                Raw::PrimSlots {
                    layer,
                    path,
                    main: new_main.cloned(),
                    branches: new_branches.cloned(),
                },
                Raw::PrimSlots {
                    layer,
                    path,
                    main: main.cloned(),
                    branches: branches.cloned(),
                },
            );
        }
        Some(Self {
            preconditions: Vec::new(),
            ops,
        })
    }
}

/// The steps that give the prim spec `old` at `path` the metadata,
/// properties and child names of `new`, one per metadata field, property
/// field, property spec or child name that differs, or `None` when the
/// specs differ otherwise (specifier, type, arcs, variants), or the
/// steps would leave properties or children in another order: the slot is
/// then written whole. A property whose declaration (kind, `custom`,
/// variability, type) changed is written whole.
///
/// OpenUSD records a property field change as an info change of the
/// property's own path, and a property spec added or removed as a resync of
/// that path (`SdfChangeList`, `UsdStage::_HandleLayersDidChange`).
fn spec_steps(
    layer: LayerId,
    path: PathId,
    old: &PrimSpec,
    new: &PrimSpec,
) -> Option<Vec<(Raw, Raw)>> {
    let bare = |spec: &PrimSpec| {
        let mut spec = spec.clone();
        spec.authored_children.clear();
        spec.properties.clear();
        spec.fields.clear();
        spec
    };
    if !bare(old).same(&bare(new)) {
        return None;
    }
    let loc = Loc::Prim {
        path,
        sites: old.outer_variant_sites().to_vec(),
    };
    let mut steps = Vec::new();
    metadata_steps(layer, &loc, None, &old.fields, &new.fields, &mut steps)?;
    // Properties: removed, then added or changed in `new`'s order.
    fn find(spec: &PrimSpec, name: TokenId) -> Option<(usize, &PropertySpec)> {
        let at = spec
            .properties
            .iter()
            .position(|entry| entry.name == name)?;
        Some((at, &*spec.properties[at].spec))
    }
    let mut names: Vec<TokenId> = old.properties.iter().map(|entry| entry.name).collect();
    for entry in &old.properties {
        if find(new, entry.name).is_none() {
            let index = names.iter().position(|n| *n == entry.name)?;
            names.remove(index);
            steps.push(property_step(
                layer,
                &loc,
                entry.name,
                None,
                Some(&entry.spec),
                index,
            ));
        }
    }
    for (index, entry) in new.properties.iter().enumerate() {
        let Some((at, before)) = find(old, entry.name) else {
            names.insert(index.min(names.len()), entry.name);
            steps.push(property_step(
                layer,
                &loc,
                entry.name,
                Some(&entry.spec),
                None,
                index,
            ));
            continue;
        };
        if before.same(&entry.spec) {
            continue;
        }
        let after = &*entry.spec;
        if (before.kind, before.custom, before.variability)
            != (after.kind, after.custom, after.variability)
            || !before.type_name.same(&after.type_name)
        {
            steps.push(property_step(
                layer,
                &loc,
                entry.name,
                Some(after),
                Some(before),
                at,
            ));
            continue;
        }
        field_steps(layer, &loc, entry.name, before, after, &mut steps)?;
    }
    if names
        .iter()
        .ne(new.properties.iter().map(|entry| &entry.name))
    {
        return None;
    }
    // Child names: removed, then added at their places in `new`.
    let mut children = old.authored_children.clone();
    for name in &old.authored_children {
        if !new.authored_children.contains(name) {
            let index = children.iter().position(|n| n == name)?;
            children.remove(index);
            steps.push((
                Raw::Child {
                    layer,
                    parent: loc.clone(),
                    name: *name,
                    index: None,
                },
                Raw::Child {
                    layer,
                    parent: loc.clone(),
                    name: *name,
                    index: Some(index),
                },
            ));
        }
    }
    for (index, name) in new.authored_children.iter().enumerate() {
        if !old.authored_children.contains(name) {
            children.insert(index.min(children.len()), *name);
            steps.push((
                Raw::Child {
                    layer,
                    parent: loc.clone(),
                    name: *name,
                    index: Some(index),
                },
                Raw::Child {
                    layer,
                    parent: loc.clone(),
                    name: *name,
                    index: None,
                },
            ));
        }
    }
    (children == new.authored_children).then_some(steps)
}

/// The step writing the property `name` whole (`spec`, `None` to remove
/// it) over `before`, inserted at `index` when new.
fn property_step(
    layer: LayerId,
    loc: &Loc,
    name: TokenId,
    spec: Option<&PropertySpec>,
    before: Option<&PropertySpec>,
    index: usize,
) -> (Raw, Raw) {
    (
        Raw::Property {
            layer,
            loc: loc.clone(),
            name,
            spec: spec.cloned(),
            index,
        },
        Raw::Property {
            layer,
            loc: loc.clone(),
            name,
            spec: before.cloned(),
            index,
        },
    )
}

/// The steps that change the fields of the property `name` from `before`'s
/// to `after`'s, whose declarations agree: its default, its animation, its
/// targets and each metadata field that differs. `None` when the metadata
/// fields would end in another order.
fn field_steps(
    layer: LayerId,
    loc: &Loc,
    name: TokenId,
    before: &PropertySpec,
    after: &PropertySpec,
    steps: &mut Vec<(Raw, Raw)>,
) -> Option<()> {
    let loc = || loc.clone();
    if !before.default.same(&after.default) {
        steps.push((
            Raw::Default {
                layer,
                loc: loc(),
                name,
                value: after.default.clone(),
            },
            Raw::Default {
                layer,
                loc: loc(),
                name,
                value: before.default.clone(),
            },
        ));
    }
    if !before.time_samples.same(&after.time_samples) || !before.spline.same(&after.spline) {
        let state = |spec: &PropertySpec| {
            Box::new(AnimationState {
                samples: spec.time_samples.clone(),
                spline: spec.spline.clone(),
            })
        };
        steps.push((
            Raw::Animation {
                layer,
                loc: loc(),
                name,
                state: state(after),
            },
            Raw::Animation {
                layer,
                loc: loc(),
                name,
                state: state(before),
            },
        ));
    }
    if before.targets != after.targets {
        let targets = |spec: &PropertySpec| spec.targets.as_deref().cloned();
        steps.push((
            Raw::Targets {
                layer,
                loc: loc(),
                name,
                targets: targets(after),
            },
            Raw::Targets {
                layer,
                loc: loc(),
                name,
                targets: targets(before),
            },
        ));
    }
    metadata_steps(
        layer,
        &loc(),
        Some(name),
        &before.metadata,
        &after.metadata,
        steps,
    )
}

/// The steps that change the metadata fields `before` of the spec at
/// `loc` (of its property `property`, when given) to `after`: one per field
/// removed, added or changed, or `None` when the fields would end in
/// another order.
fn metadata_steps(
    layer: LayerId,
    loc: &Loc,
    property: Option<TokenId>,
    before: &[FieldEntry],
    after: &[FieldEntry],
    steps: &mut Vec<(Raw, Raw)>,
) -> Option<()> {
    if before.same(after) {
        return Some(());
    }
    let field = |fields: &[FieldEntry], key| fields.iter().position(|f: &FieldEntry| f.name == key);
    let step = |key, value, index| Raw::Field {
        layer,
        loc: loc.clone(),
        property,
        key,
        value,
        index,
    };
    let mut fields: Vec<FieldEntry> = before.to_vec();
    for entry in before {
        if field(after, entry.name).is_none() {
            let index = field(&fields, entry.name)?;
            fields.remove(index);
            steps.push((
                step(entry.name, None, 0),
                step(entry.name, Some(entry.value.clone()), index),
            ));
        }
    }
    for (index, entry) in after.iter().enumerate() {
        let old = field(&fields, entry.name).map(|at| (at, fields[at].value.clone()));
        if old
            .as_ref()
            .is_some_and(|(_, value)| value.same(&entry.value))
        {
            continue;
        }
        match &old {
            Some((at, _)) => fields[*at].value = entry.value.clone(),
            None => fields.insert(index.min(fields.len()), entry.clone()),
        }
        steps.push((
            step(entry.name, Some(entry.value.clone()), index),
            step(entry.name, old.map(|(_, value)| value), 0),
        ));
    }
    fields.as_slice().same(after).then_some(())
}

/// `content` with each sublayer and arc that is the one `current` holds,
/// in another form (resolved where `current`'s is deferred, through the
/// store's binding of its asset, or the reverse), in `current`'s form;
/// `None` when no arc needs it.
fn bound_forms(
    store: &dyn LayerStore,
    layer: LayerId,
    current: &Layer,
    content: &Layer,
) -> Option<Layer> {
    // The same arc: one resolved to the layer the store binds the other's
    // asset to.
    let same = |old: &Reference, new: &Reference| {
        if old.deferred == new.deferred {
            return false;
        }
        let (deferred, resolved) = if old.deferred { (old, new) } else { (new, old) };
        let bound = deferred.layer == layer
            && deferred
                .asset
                .as_deref()
                .and_then(|asset| store.asset_layer(layer, asset))
                == Some(resolved.layer);
        let mut as_old = new.clone();
        as_old.layer = old.layer;
        as_old.deferred = old.deferred;
        bound && *old == as_old
    };
    let mut normalized: Option<Layer> = None;
    for (at, (old, new)) in current.sublayers.iter().zip(&content.sublayers).enumerate() {
        let bound = old.asset == new.asset
            && old.offset == new.offset
            && old.is_unresolved() != new.is_unresolved()
            && old
                .asset
                .as_deref()
                .and_then(|asset| store.asset_layer(layer, asset))
                == Some(if old.is_unresolved() {
                    new.layer
                } else {
                    old.layer
                });
        if bound {
            normalized.get_or_insert_with(|| content.clone()).sublayers[at] = old.clone();
        }
    }
    for (path, old) in current.prims.iter() {
        let Some(new) = content.prims.get(path) else {
            continue;
        };
        let pairs = |a: &ListOp<Reference>, b: &ListOp<Reference>| {
            arc_pairs(a, b).any(|(old, new)| same(old, new))
        };
        let references = pairs(&old.references, &new.references);
        let payloads = pairs(&old.payloads, &new.payloads);
        let variants = variant_arcs_differ(old.variant_sets(), new.variant_sets(), &same);
        if !(references || payloads || variants) {
            continue;
        }
        let spec = normalized
            .get_or_insert_with(|| content.clone())
            .prims
            .get_mut(path)
            .expect("a spec of the content");
        keep_forms(&old.references, &mut spec.references, &same);
        keep_forms(&old.payloads, &mut spec.payloads, &same);
        if variants && let Some(rare) = spec.authored_rare_mut() {
            keep_variant_forms(old.variant_sets(), &mut rare.variant_sets, &same);
        }
    }
    normalized
}

/// The lists of `op`, in a fixed order, `None` for an explicit list not
/// authored.
fn arc_lists(op: &ListOp<Reference>) -> [Option<&Vec<Reference>>; 6] {
    [
        op.explicit.as_ref(),
        Some(&op.delete),
        Some(&op.add),
        Some(&op.prepend),
        Some(&op.append),
        Some(&op.reorder),
    ]
}

/// Each arc of `old` with the arc of `new` in the same place, list by list,
/// for lists of the same length.
fn arc_pairs<'a>(
    old: &'a ListOp<Reference>,
    new: &'a ListOp<Reference>,
) -> impl Iterator<Item = (&'a Reference, &'a Reference)> {
    arc_lists(old)
        .into_iter()
        .zip(arc_lists(new))
        .filter_map(|(old, new)| old.zip(new))
        .filter(|(old, new)| old.len() == new.len())
        .flat_map(|(old, new)| old.iter().zip(new))
}

/// Puts each arc of `new` that is `same` as the arc in its place in `old`
/// in `old`'s form.
fn keep_forms(
    old: &ListOp<Reference>,
    new: &mut crate::SparseListOp<Reference>,
    same: &impl Fn(&Reference, &Reference) -> bool,
) {
    let Some(new) = new.get_mut() else {
        return;
    };
    let olds = arc_lists(old);
    let explicit = new.explicit.as_mut();
    let lists = [
        explicit,
        Some(&mut new.delete),
        Some(&mut new.add),
        Some(&mut new.prepend),
        Some(&mut new.append),
        Some(&mut new.reorder),
    ];
    for (old, new) in olds.into_iter().zip(lists) {
        let (Some(old), Some(new)) = (old, new) else {
            continue;
        };
        if old.len() != new.len() {
            continue;
        }
        for (old, new) in old.iter().zip(new.iter_mut()) {
            if same(old, new) {
                *new = old.clone();
            }
        }
    }
}

/// Whether an arc of a variant of `new` is `same` as the one in its place in
/// `old`.
fn variant_arcs_differ(
    old: &VariantSets,
    new: &VariantSets,
    same: &impl Fn(&Reference, &Reference) -> bool,
) -> bool {
    old.iter().any(|(name, set)| {
        new.get(name).is_some_and(|new_set| {
            set.variants.iter().any(|(variant, old)| {
                new_set.variants.get(variant).is_some_and(|new| {
                    arc_pairs(&old.references, &new.references).any(|(a, b)| same(a, b))
                        || arc_pairs(&old.payloads, &new.payloads).any(|(a, b)| same(a, b))
                        || variant_arcs_differ(&old.variant_sets, &new.variant_sets, same)
                })
            })
        })
    })
}

/// [`keep_forms`] for every variant of `new` that `old` holds too.
fn keep_variant_forms(
    old: &VariantSets,
    new: &mut VariantSets,
    same: &impl Fn(&Reference, &Reference) -> bool,
) {
    for (name, set) in old.iter() {
        let Some(new_set) = new.get_mut(name) else {
            continue;
        };
        for (variant, old) in set.variants.iter() {
            if let Some(new) = new_set.variants.get_mut(variant) {
                keep_forms(&old.references, &mut new.references, same);
                keep_forms(&old.payloads, &mut new.payloads, same);
                keep_variant_forms(&old.variant_sets, &mut new.variant_sets, same);
            }
        }
    }
}

/// Whether two optional slots hold the same authored state.
fn same<T: Same>(a: Option<&T>, b: Option<&T>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.same(b),
        (a, b) => a.is_none() && b.is_none(),
    }
}

/// Adds the steps that give `current` the default prim and metadata of
/// `content`: one per field that differs, so a change of a field that
/// composes nothing stays a change of that field alone, then one writing
/// both whole when the fields would otherwise end in another order.
fn layer_fields(
    store: &dyn LayerStore,
    layer: LayerId,
    current: &Layer,
    content: &Layer,
    guarded: &mut impl FnMut(Raw, Raw),
) {
    if current.default_prim == content.default_prim && current.metadata.same(&content.metadata) {
        return;
    }
    let field = |fields: &[FieldEntry], key| {
        fields
            .iter()
            .position(|entry| entry.name == key)
            .map(|index| (index, fields[index].value.clone()))
    };
    // The order the per-field steps leave the fields in, to check against
    // `content`'s.
    let mut fields = current.metadata.clone();
    for entry in &current.metadata {
        if field(&content.metadata, entry.name).is_none() {
            let index = field(&fields, entry.name).map_or(0, |(index, _)| index);
            fields.remove(index);
            guarded(
                Raw::LayerMetadata {
                    layer,
                    key: entry.name,
                    value: None,
                    index: 0,
                },
                Raw::LayerMetadata {
                    layer,
                    key: entry.name,
                    value: Some(entry.value.clone()),
                    index,
                },
            );
        }
    }
    for (index, entry) in content.metadata.iter().enumerate() {
        let old = field(&fields, entry.name);
        if old
            .as_ref()
            .is_some_and(|(_, value)| value.same(&entry.value))
        {
            continue;
        }
        match &old {
            Some((at, _)) => fields[*at].value = entry.value.clone(),
            None => fields.insert(index.min(fields.len()), entry.clone()),
        }
        guarded(
            Raw::LayerMetadata {
                layer,
                key: entry.name,
                value: Some(entry.value.clone()),
                index,
            },
            Raw::LayerMetadata {
                layer,
                key: entry.name,
                value: old.map(|(_, value)| value),
                index: 0,
            },
        );
    }
    if current.default_prim != content.default_prim {
        let token = |prim: Option<_>| prim.map(|t| FieldValue::Value(Value::Token(t)));
        guarded(
            Raw::LayerMetadata {
                layer,
                key: store.tokens().intern("defaultPrim"),
                value: token(content.default_prim),
                index: 0,
            },
            Raw::LayerMetadata {
                layer,
                key: store.tokens().intern("defaultPrim"),
                value: token(current.default_prim),
                index: 0,
            },
        );
    }
    if !fields.same(&content.metadata) {
        guarded(
            Raw::LayerFields {
                layer,
                default_prim: content.default_prim,
                metadata: content.metadata.clone(),
            },
            Raw::LayerFields {
                layer,
                default_prim: content.default_prim,
                metadata: fields,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertyPath, PropertySpec,
        StageOptions, Transaction, Value,
    };

    /// A layer with `/A.x = x`, and `/S` when `with_s`, built through
    /// `store`'s interners.
    fn scene(store: &InMemoryStore, x: f64, with_s: bool) -> Layer {
        let mut layer = Layer::new(LayerId(1));
        let x_name = store.tokens().intern("x");
        let a = store.path("/A");
        layer.insert_prim(
            a,
            PrimSpec::def().with_property(
                x_name,
                PropertySpec::attribute().with_default(Value::Double(x)),
            ),
        );
        let mut children = alloc::vec![store.tokens().intern("A")];
        if with_s {
            let s = store.path("/S");
            layer.insert_prim(s, PrimSpec::def());
            children.push(store.tokens().intern("S"));
        }
        let root = store.path("/");
        let pseudo = PrimSpec {
            authored_children: children,
            ..PrimSpec::default()
        };
        layer.insert_prim(root, pseudo);
        layer
    }

    fn live(x: f64, with_s: bool) -> (InMemoryStore, LiveStage) {
        let mut store = InMemoryStore::default();
        let layer = scene(&store, x, with_s);
        store.insert_layer(layer);
        let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        (store, live)
    }

    #[test]
    fn unchanged_content_transfers_nothing() {
        let (store, _) = live(1.0, true);
        let content = scene(&store, 1.0, true);
        let edit = Transaction::transfer_content(&store, LayerId(1), &content).unwrap();
        assert!(edit.is_empty());
        assert!(Transaction::transfer_content(&store, LayerId(9), &content).is_none());
    }

    #[test]
    fn a_changed_value_is_reported_info_only() {
        let (mut store, mut live) = live(1.0, true);
        let content = scene(&store, 2.0, true);
        let edit = Transaction::transfer_content(&store, LayerId(1), &content).unwrap();
        let applied = live.apply(&mut store, &edit).unwrap();
        let a = store.path("/A");
        let x = store.tokens().intern("x");
        assert_eq!(applied.changes.resynced, []);
        assert_eq!(applied.changes.resynced_properties, []);
        assert_eq!(applied.changes.changed_info_only, []);
        assert_eq!(
            applied.changes.changed_properties,
            [crate::PropertyFieldChange {
                property: PropertyPath::new(a, x),
                field: crate::PropertyField::Default,
            }]
        );
        assert_eq!(store.layer(LayerId(1)).unwrap(), &content);
        let value = live
            .view(&store)
            .attribute(PropertyPath::new(a, x))
            .and_then(|attribute| attribute.value::<f64>(crate::Time::Default));
        assert_eq!(value, Some(2.0));
    }

    #[test]
    fn a_reloaded_layer_of_the_stage_changes_the_pseudo_root() {
        let (mut store, mut live) = live(1.0, true);
        let content = scene(&store, 2.0, true);
        let mut edit = Transaction::transfer_content(&store, LayerId(1), &content).unwrap();
        edit.mark_reloaded(LayerId(1));
        let applied = live.apply(&mut store, &edit).unwrap();
        let (root, a) = (store.path("/"), store.path("/A"));
        assert_eq!(applied.changes.resynced, []);
        // The pseudo-root, read again, and `/A.x`, whose value it changed.
        assert_eq!(applied.changes.changed_info_only, [root]);
        assert_eq!(applied.changes.prim_fields, []);
        assert_eq!(applied.changes.properties_of(a).len(), 1);
        // Marked alone, it still is.
        let mut edit = Transaction::new();
        edit.mark_reloaded(LayerId(1));
        let applied = live.apply(&mut store, &edit).unwrap();
        assert_eq!(applied.changes.changed_info_only, [root]);
        assert!(applied.inverse.is_empty());
        let mut edit = Transaction::new();
        edit.mark_reloaded(LayerId(9));
        assert!(live.apply(&mut store, &edit).is_err());
    }

    #[test]
    fn a_changed_prim_field_is_reported_info_only() {
        use crate::{FieldEntry, FieldValue};
        let (mut store, mut live) = live(1.0, true);
        let mut content = scene(&store, 1.0, true);
        let a = store.path("/A");
        let documentation = store.tokens().intern("documentation");
        content.prim_mut(a).unwrap().fields.push(FieldEntry {
            name: documentation,
            value: FieldValue::Value(Value::Double(3.0)),
        });
        let edit = Transaction::transfer_content(&store, LayerId(1), &content).unwrap();
        let applied = live.apply(&mut store, &edit).unwrap();
        assert_eq!(applied.changes.resynced, []);
        assert_eq!(applied.changes.changed_info_only, [a]);
        assert_eq!(
            applied.changes.prim_fields,
            [crate::PrimFieldChange {
                prim: a,
                field: crate::PrimInfoField::Metadata(documentation),
            }]
        );
        assert_eq!(applied.changes.changed_properties, []);
        assert_eq!(store.layer(LayerId(1)).unwrap(), &content);
    }

    /// A layer read while its reference's asset was not supplied holds the
    /// arc deferred, bound once the asset arrived; read again, it holds the
    /// arc resolved. Both are the same arc.
    #[test]
    fn a_deferred_arc_bound_is_the_resolved_one() {
        use crate::{LayerOffset, Reference, ReferenceTarget};
        let mut store = InMemoryStore::default();
        let r = store.path("/R");
        let root = store.path("/");
        let name = store.tokens().intern("R");
        let layer = |reference: Reference| {
            let mut layer = Layer::new(LayerId(1));
            layer.insert_prim(r, PrimSpec::def().with_reference(reference));
            let pseudo = PrimSpec {
                authored_children: alloc::vec![name],
                ..PrimSpec::default()
            };
            layer.insert_prim(root, pseudo);
            layer
        };
        let target = ReferenceTarget::Prim(r);
        let deferred = Reference::deferred(LayerId(1), "ref.usda", target, LayerOffset::IDENTITY);
        store.insert_layer(layer(deferred));
        let mut referenced = Layer::new(LayerId(2));
        referenced.insert_prim(r, PrimSpec::def());
        store.insert_layer(referenced);
        let resolved = layer(Reference::with_asset(LayerId(2), r, "ref.usda"));
        // Unbound, the arcs differ.
        let edit = Transaction::transfer_content(&store, LayerId(1), &resolved).unwrap();
        assert!(!edit.is_empty());
        store.insert_asset_layer(LayerId(1), "ref.usda", LayerId(2));
        let edit = Transaction::transfer_content(&store, LayerId(1), &resolved).unwrap();
        assert!(edit.is_empty());
    }

    #[test]
    fn a_removed_prim_is_resynced_alone() {
        let (mut store, mut live) = live(1.0, true);
        let content = scene(&store, 1.0, false);
        let edit = Transaction::transfer_content(&store, LayerId(1), &content).unwrap();
        let applied = live.apply(&mut store, &edit).unwrap();
        let s = store.path("/S");
        assert_eq!(applied.changes.resynced, [s]);
        assert_eq!(applied.changes.removed, [s]);
        assert_eq!(applied.changes.changed_info_only, []);
        assert_eq!(store.layer(LayerId(1)).unwrap(), &content);
        // Undone, the prim is back.
        let undone = live.apply(&mut store, &applied.inverse).unwrap();
        assert_eq!(undone.changes.created, [s]);
    }
}
