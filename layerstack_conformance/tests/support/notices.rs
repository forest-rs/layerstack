// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A stage's change reports in the terms of OpenUSD's notices.

use std::collections::{BTreeMap, BTreeSet};

use layerstack::{ChangeNotice, Changes, PathInterner, PropertyField, PropertyPath, TokenInterner};

/// OpenUSD's name of a changed property field
/// (`UsdNotice::ObjectsChanged::GetChangedFields`): `targetPaths` for a
/// relationship's targets, `connectionPaths` for an attribute's.
pub(crate) fn property_field_name(
    field: PropertyField,
    is_relationship: bool,
    tokens: &TokenInterner,
) -> String {
    match field {
        PropertyField::Default => "default".into(),
        PropertyField::TimeSamples | PropertyField::Animation => "timeSamples".into(),
        PropertyField::Targets if is_relationship => "targetPaths".into(),
        PropertyField::Targets => "connectionPaths".into(),
        PropertyField::Metadata(key) => tokens.resolve(key).to_string(),
        other => format!("{other:?}"),
    }
}

/// The info-only paths of `changes`, prims and properties, each with the
/// names of its changed fields, as `GetChangedInfoOnlyPaths` and
/// `GetChangedFields` give them; `is_relationship` tells a property's kind.
pub(crate) fn info_fields(
    changes: &Changes,
    paths: &PathInterner,
    tokens: &TokenInterner,
    is_relationship: impl Fn(PropertyPath) -> bool,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut info: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for &prim in &changes.changed_info_only {
        let fields = changes
            .fields_of_prim(prim)
            .map(|field| field.name(tokens).to_string())
            .collect();
        info.insert(paths.display(prim, tokens), fields);
    }
    for change in &changes.changed_properties {
        let name = property_field_name(change.field, is_relationship(change.property), tokens);
        info.entry(change.property.display(paths, tokens))
            .or_default()
            .insert(name);
    }
    info
}

/// [`info_fields`] of the changes `notice` reports, a property's kind read
/// from the stage after them.
pub(crate) fn notice_info_fields(notice: &ChangeNotice<'_>) -> BTreeMap<String, BTreeSet<String>> {
    let view = notice.view();
    info_fields(
        notice.changes,
        notice.store.paths(),
        notice.store.tokens(),
        |property| {
            view.prim(property.prim_path())
                .and_then(|prim| prim.relationship(property.property()))
                .is_some()
        },
    )
}

/// The resynced paths of `changes`, prims and properties, as
/// `GetResyncedPaths` gives them.
pub(crate) fn resynced_paths(
    changes: &Changes,
    paths: &PathInterner,
    tokens: &TokenInterner,
) -> BTreeSet<String> {
    changes
        .resynced
        .iter()
        .map(|p| paths.display(*p, tokens))
        .chain(
            changes
                .resynced_properties
                .iter()
                .map(|p| p.display(paths, tokens)),
        )
        .collect()
}
