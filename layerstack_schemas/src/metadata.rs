// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Registered metadata reads over composed prims, properties and the root layer.

use crate::{PrimView, Scene};
use layerstack::{MetadataTarget, PropertyKind, PropertyPath, ResolvedValue, TokenInterner, Value};

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
    /// A prim field's composed authored value: a field registered for
    /// prims, built in or in the stage's schema registry
    /// ([`layerstack::MetadataField::lookup`]). Dictionaries and list ops
    /// combine through the stage's ordinary metadata resolver.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution).
    #[must_use]
    pub fn metadata_value(&self, name: &str) -> Option<Value> {
        let scene = self.scene();
        let tokens = scene.store().tokens();
        let key = tokens.lookup(name)?;
        if !layerstack::MetadataField::lookup(name, scene.stage().schemas(), tokens)?
            .applies_to(MetadataTarget::Prim)
        {
            return None;
        }
        scalar(self.prim()?.metadata(key)?.value)
    }

    /// Reads metadata on a declared or schema-defined property, of either kind.
    /// Missing prims and properties have no metadata view.
    #[must_use]
    pub fn property_metadata(&self, name: &str) -> Option<PropertyMetadata<'a>> {
        let scene = self.scene();
        let property = self.prim()?.property(name)?;
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
        name: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        read(&self.metadata_value(name)?, self.scene().store().tokens())
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
    pub fn has_authored(&self, name: &str) -> bool {
        let key = self.scene.store().tokens().lookup(name);
        self.scene.property(self.path).is_some_and(|property| {
            property
                .opinions()
                .iter()
                .filter_map(|o| o.value.as_property())
                .any(|p| match name {
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

    /// A field's composed authored value, for a field registered for the
    /// property's kind, built in or in the stage's schema registry.
    /// No value is synthesized from the field's registered default.
    ///
    /// Spec: AOUSD Core §12.2.5–12.2.6 (dictionary and list-op combining).
    #[must_use]
    pub fn value(&self, name: &str) -> Option<Value> {
        let tokens = self.scene.store().tokens();
        let key = tokens.lookup(name)?;
        if !layerstack::MetadataField::lookup(name, self.scene.stage().schemas(), tokens)?
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
        name: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        read(&self.value(name)?, self.scene.store().tokens())
    }
}

impl<'a> StageMetadata<'a> {
    /// An authored root-layer field or its registered layer default.
    #[must_use]
    pub fn value(&self, name: &str) -> Option<Value> {
        let key = self.scene.store().tokens().lookup(name)?;
        self.scene.stage().layer_metadata(key, self.scene.store())
    }

    pub(crate) fn read_metadata<T>(
        &self,
        name: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        read(&self.value(name)?, self.scene.store().tokens())
    }
}
