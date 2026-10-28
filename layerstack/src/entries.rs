// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lookups over the storage representation of a spec's fields and
//! properties (`Vec<FieldEntry>`, `Vec<PropertyEntry>`), shared with this
//! repository's format crates. They are public only with the
//! `unstable-internals` feature and follow no semver promise: read specs
//! through [`crate::PrimSpec::field`], [`crate::PrimSpec::property`] and
//! [`crate::PropertySpec::metadata`].

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::doc::{FieldEntry, FieldValue};
use crate::interner::TokenId;
use crate::property::{PropertyEntry, PropertyKind, PropertySpec};

/// Inserts or replaces a field in a `Vec<FieldEntry>` by name.
///
/// If a field with the given name already exists, its value is replaced in
/// place. Otherwise a new entry is appended.
pub fn set_field_vec(fields: &mut Vec<FieldEntry>, name: TokenId, value: FieldValue) {
    if let Some(entry) = fields.iter_mut().find(|e| e.name == name) {
        entry.value = value;
    } else {
        fields.push(FieldEntry { name, value });
    }
}

/// Returns a shared reference to the value of a field, if present.
pub fn get_field<'a>(fields: &'a [FieldEntry], name: &TokenId) -> Option<&'a FieldValue> {
    fields.iter().find(|e| &e.name == name).map(|e| &e.value)
}

/// Returns a mutable reference to the value of a field, if present.
pub fn get_field_mut<'a>(
    fields: &'a mut [FieldEntry],
    name: &TokenId,
) -> Option<&'a mut FieldValue> {
    fields
        .iter_mut()
        .find(|e| &e.name == name)
        .map(|e| &mut e.value)
}

/// Inserts a field only if no entry with the same name exists.
pub fn insert_field_if_absent(fields: &mut Vec<FieldEntry>, name: TokenId, value: FieldValue) {
    if !fields.iter().any(|e| e.name == name) {
        fields.push(FieldEntry { name, value });
    }
}

/// Removes a field by name, returning its value.
pub fn remove_field(fields: &mut Vec<FieldEntry>, name: TokenId) -> Option<FieldValue> {
    let index = fields.iter().position(|e| e.name == name)?;
    Some(fields.remove(index).value)
}

/// Returns the property named `name`, if present.
#[must_use]
pub fn get_property(properties: &[PropertyEntry], name: TokenId) -> Option<&PropertySpec> {
    properties.iter().find(|e| e.name == name).map(|e| &*e.spec)
}

/// Returns the property named `name` mutably, if present.
#[must_use]
pub fn get_property_mut(
    properties: &mut [PropertyEntry],
    name: TokenId,
) -> Option<&mut PropertySpec> {
    properties
        .iter_mut()
        .find(|e| e.name == name)
        .map(|e| Arc::make_mut(&mut e.spec))
}

/// Inserts or replaces the property named `name`, keeping the position of a
/// replaced property (authored order).
pub fn set_property_vec(properties: &mut Vec<PropertyEntry>, name: TokenId, spec: PropertySpec) {
    if let Some(existing) = properties.iter_mut().find(|entry| entry.name == name) {
        existing.spec = Arc::new(spec);
    } else {
        properties.push(PropertyEntry {
            name,
            spec: Arc::new(spec),
        });
    }
}

/// Returns the property named `name`, creating an empty spec of `kind` (see
/// [`PropertySpec::of_kind`]) at the end of the list when absent.
pub fn property_entry(
    properties: &mut Vec<PropertyEntry>,
    name: TokenId,
    kind: PropertyKind,
) -> &mut PropertySpec {
    let index = match properties.iter().position(|e| e.name == name) {
        Some(index) => index,
        None => {
            properties.push(PropertyEntry {
                name,
                spec: Arc::new(PropertySpec::of_kind(kind)),
            });
            properties.len() - 1
        }
    };
    Arc::make_mut(&mut properties[index].spec)
}

/// Removes the property named `name`, returning its spec.
pub fn remove_property(properties: &mut Vec<PropertyEntry>, name: TokenId) -> Option<PropertySpec> {
    let index = properties.iter().position(|e| e.name == name)?;
    Some(Arc::unwrap_or_clone(properties.remove(index).spec))
}
