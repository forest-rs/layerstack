// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Registered metadata reads over composed prims, properties and the root layer.
//!
//! Each target has one handle: [`PrimView::metadata`] ([`PrimMetadata`]),
//! [`PrimView::property_metadata`] ([`PropertyMetadata`]) and
//! [`Scene::metadata`] ([`StageMetadata`]). Each reads a metadata `field`
//! by its USD name (`kind`, `customData`, …), and a dictionary entry by
//! the field and a colon-separated `key_path` (`"a:b"`), as OpenUSD's
//! `GetMetadataByDictKey` takes it. The writers on [`PrimView`] take the
//! same names.

use crate::{PrimView, Scene};
use layerstack::{
    FieldValue, MetadataTarget, PathId, PropertyKind, PropertyPath, ResolvedValue, TokenInterner,
    Value,
};

/// Metadata on a composed prim: the fields registered for prims, built in
/// (`kind`, `customData`, `hidden`, …) or in the stage's schema registry.
/// Readers compose authored opinions; dictionaries and list ops combine
/// through the stage's metadata resolver (OpenUSD `UsdObject::GetMetadata`).
#[derive(Clone, Copy, Debug)]
pub struct PrimMetadata<'a> {
    prim: PrimView<'a>,
}

/// Metadata on an existing composed property, including schema-defined properties.
/// Readers compose authored opinions; Sdf's registered field defaults are not
/// property metadata fallbacks (OpenUSD `UsdObject::GetMetadata`).
#[derive(Clone, Copy, Debug)]
pub struct PropertyMetadata<'a> {
    scene: Scene<'a>,
    path: PropertyPath,
    target: MetadataTarget,
}

/// Metadata on the stage's root layer, with registered layer defaults.
/// Sublayer metadata does not participate (AOUSD Core §12.2.7).
#[derive(Clone, Copy, Debug)]
pub struct StageMetadata<'a> {
    scene: Scene<'a>,
}

/// The entry at the colon-separated `key_path` of a nested dictionary
/// value; a path with an empty part names none.
pub(crate) fn dictionary_entry<'v>(value: &'v Value, key_path: &str) -> Option<&'v FieldValue> {
    if key_path.split(':').any(str::is_empty) {
        return None;
    }
    dictionary_entry_at(value, key_path)
}

/// The entry at the colon-separated `key_path` of a nested dictionary
/// value, an empty part naming an entry named `""`, as
/// `VtDictionary::GetValueAtPath` splits a path.
pub(crate) fn dictionary_entry_at<'v>(value: &'v Value, key_path: &str) -> Option<&'v FieldValue> {
    let Value::Dictionary(entries) = value else {
        return None;
    };
    let mut entries = entries.as_slice();
    let mut keys = key_path.split(':').peekable();
    loop {
        let key = keys.next()?;
        let entry = &entries.iter().find(|(name, _)| name.as_ref() == key)?.1;
        if keys.peek().is_none() {
            return Some(entry);
        }
        let FieldValue::Value(Value::Dictionary(nested)) = entry else {
            return None;
        };
        entries = nested;
    }
}

fn scalar(value: ResolvedValue) -> Option<Value> {
    match value {
        ResolvedValue::Scalar(value) => Some(value),
        ResolvedValue::Dictionary(entries) => Some(Value::Dictionary(entries)),
        ResolvedValue::TokenList(tokens) => Some(Value::array_from_iter(
            tokens.into_iter().map(Value::Token),
            None,
        )),
        ResolvedValue::ValueList(values) => Some(Value::array_from_iter(values, None)),
        ResolvedValue::PathList(_) => None,
        _ => None,
    }
}

impl<'a> Scene<'a> {
    /// The stage's root-layer metadata and registered layer defaults.
    #[must_use]
    pub fn metadata(&self) -> StageMetadata<'a> {
        StageMetadata { scene: *self }
    }
}

impl<'a> PrimView<'a> {
    /// The prim's metadata.
    #[must_use]
    pub fn metadata(&self) -> PrimMetadata<'a> {
        PrimMetadata { prim: *self }
    }

    /// Reads metadata on the declared or schema-defined property
    /// `property`, of either kind. Missing prims and properties have no
    /// metadata view.
    #[must_use]
    pub fn property_metadata(&self, property: &str) -> Option<PropertyMetadata<'a>> {
        let scene = self.scene();
        let property = self.prim()?.property(property)?;
        let kind = property
            .declaration()
            .map_or_else(|| property.kind(), |declaration| declaration.kind);
        Some(PropertyMetadata {
            scene,
            path: property.path(),
            target: match kind {
                PropertyKind::Attribute => MetadataTarget::Attribute,
                PropertyKind::Relationship => MetadataTarget::Relationship,
            },
        })
    }

    pub(crate) fn read_metadata<T>(
        &self,
        field: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        read(
            &self.metadata().value(field)?,
            self.scene().store().tokens(),
        )
    }
}

impl<'a> PrimMetadata<'a> {
    /// The prim whose metadata is read.
    #[must_use]
    pub fn path(&self) -> PathId {
        self.prim.path()
    }

    /// The prim field `field`'s composed authored value, for a field
    /// registered for prims, built in or in the stage's schema registry
    /// ([`layerstack::MetadataField::lookup`]).
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution).
    #[doc(alias = "GetMetadata")]
    #[must_use]
    pub fn value(&self, field: &str) -> Option<Value> {
        let scene = self.prim.scene();
        let tokens = scene.store().tokens();
        let key = tokens.lookup(field)?;
        if !layerstack::MetadataField::lookup(field, scene.stage().schemas(), tokens)?
            .applies_to(MetadataTarget::Prim)
        {
            return None;
        }
        scalar(self.prim.prim()?.metadata(key)?.value)
    }

    /// The entry at the colon-separated `key_path` of the prim's composed
    /// dictionary field `field` (`customData`, `assetInfo`, or a
    /// dictionary field a plugin registers), as [`Self::value`] composes
    /// it: `"a:b"` names the entry `b` of the dictionary `a`. `None` when
    /// the field holds no such entry or is not a dictionary.
    ///
    /// OpenUSD: `UsdObject::GetMetadataByDictKey`. AOUSD Core §12.2.5.
    #[doc(alias = "GetMetadataByDictKey")]
    #[doc(alias = "GetCustomDataByKey")]
    #[doc(alias = "GetAssetInfoByKey")]
    #[must_use]
    pub fn value_by_key(&self, field: &str, key_path: &str) -> Option<FieldValue> {
        dictionary_entry(&self.value(field)?, key_path).cloned()
    }
}

impl<'a> PropertyMetadata<'a> {
    /// The property whose metadata is read.
    #[must_use]
    pub fn path(&self) -> PropertyPath {
        self.path
    }

    /// Whether any property opinion authors this metadata field, including a
    /// block. Registered defaults and schema-only declarations do not count.
    /// This presence query also accepts application-defined metadata keys.
    /// OpenUSD: `UsdObject::HasAuthoredMetadata`; AOUSD Core §7.4.
    #[must_use]
    pub fn has_authored(&self, field: &str) -> bool {
        let key = self.scene.store().tokens().lookup(field);
        self.scene.property(self.path).is_some_and(|property| {
            property
                .opinions()
                .iter()
                .filter_map(|o| o.value().as_property())
                .any(|p| match field {
                    "typeName" => p.type_name.is_some(),
                    "custom" | "variability" => true,
                    "default" => p.default.is_some(),
                    "timeSamples" => p.time_samples.is_authored(),
                    "spline" => p.spline.is_some(),
                    "connectionPaths" => p.kind == PropertyKind::Attribute && p.targets.is_some(),
                    "targetPaths" => p.kind == PropertyKind::Relationship && p.targets.is_some(),
                    _ => key.is_some_and(|key| p.metadata(key).is_some()),
                })
        })
    }

    /// The entry at the colon-separated `key_path` of the property's
    /// composed dictionary field `field`, as [`PrimMetadata::value_by_key`]
    /// reads a prim's.
    ///
    /// OpenUSD: `UsdObject::GetMetadataByDictKey`. AOUSD Core §12.2.5.
    #[doc(alias = "GetMetadataByDictKey")]
    #[doc(alias = "GetCustomDataByKey")]
    #[must_use]
    pub fn value_by_key(&self, field: &str, key_path: &str) -> Option<FieldValue> {
        dictionary_entry(&self.value(field)?, key_path).cloned()
    }

    /// The field `field`'s composed authored value, for a field registered
    /// for the property's kind, built in or in the stage's schema registry.
    /// No value is synthesized from the field's registered default.
    ///
    /// Spec: AOUSD Core §12.2.5–12.2.6 (dictionary and list-op combining).
    #[doc(alias = "GetMetadata")]
    #[must_use]
    pub fn value(&self, field: &str) -> Option<Value> {
        let tokens = self.scene.store().tokens();
        let key = tokens.lookup(field)?;
        if !layerstack::MetadataField::lookup(field, self.scene.stage().schemas(), tokens)?
            .applies_to(self.target)
        {
            return None;
        }
        scalar(
            self.scene
                .property(self.path)?
                .authored_metadata(key)?
                .value,
        )
    }

    pub(crate) fn read_metadata<T>(
        &self,
        field: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        read(&self.value(field)?, self.scene.store().tokens())
    }
}

impl<'a> StageMetadata<'a> {
    /// The root layer's authored field `field`, or its registered layer
    /// default.
    #[must_use]
    pub fn value(&self, field: &str) -> Option<Value> {
        let key = self.scene.store().tokens().lookup(field)?;
        self.scene.stage().layer_metadata(key, self.scene.store())
    }

    pub(crate) fn read_metadata<T>(
        &self,
        field: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        read(&self.value(field)?, self.scene.store().tokens())
    }
}
