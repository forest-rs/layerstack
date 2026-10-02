// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Plugin-defined metadata declarations, independent of prim schemas.

use crate::{TokenId, Value};
use alloc::{sync::Arc, vec::Vec};

/// The spec kind on which a registered metadata field is valid.
///
/// OpenUSD: `SdfSchema::RegisterPluginMetadata`, `SdfMetadata.appliesTo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetadataTarget {
    /// Root-layer metadata, exposed as stage metadata.
    Layer,
    /// Metadata on prim specs.
    Prim,
    /// Metadata on attribute specs.
    Attribute,
    /// Metadata on relationship specs.
    Relationship,
}

/// A plugin's metadata declaration, shared by every applicable spec.
///
/// This is distinct from a schema-defined attribute: it does not create a
/// property on a prim. `properties` in `appliesTo` expands to both property
/// kinds. Registered defaults describe Sdf fields; property getters do not
/// automatically return them (OpenUSD `UsdObject::GetMetadata`).
///
/// Spec: AOUSD Core §7.4 (metadata), §12.2 (metadata resolution).
#[derive(Clone, Debug, PartialEq)]
pub struct MetadataDefinition {
    /// The field name, interned in the registry's token interner.
    pub name: TokenId,
    /// Sdf's registered type name, including `dictionary` and list-op types.
    pub type_name: Arc<str>,
    /// The spec kinds on which the field is valid.
    pub targets: Vec<MetadataTarget>,
    /// The registered default, if the plugin supplies one.
    pub default: Option<Value>,
    /// The plugin's documentation, empty when absent.
    pub documentation: Arc<str>,
}

impl MetadataDefinition {
    /// Whether the field is registered for `target`.
    #[must_use]
    pub fn applies_to(&self, target: MetadataTarget) -> bool {
        self.targets.contains(&target)
    }
}

/// A second declaration disagrees with an already registered metadata field.
/// The original declaration is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataConflict {
    /// The conflicting field's name.
    pub name: TokenId,
}

impl core::fmt::Display for MetadataConflict {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "conflicting metadata declaration for {:?}", self.name)
    }
}
impl core::error::Error for MetadataConflict {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SchemaRegistry, TokenInterner};

    #[test]
    fn identical_registration_is_idempotent_and_conflicts_preserve_the_first() {
        let mut tokens = TokenInterner::default();
        let name = tokens.intern("customWeight");
        let definition = MetadataDefinition {
            name,
            type_name: Arc::from("float"),
            targets: alloc::vec![MetadataTarget::Prim, MetadataTarget::Attribute],
            default: Some(Value::Float(0.0)),
            documentation: Arc::from("A weight"),
        };
        let mut builder = SchemaRegistry::builder();
        builder.register_metadata(definition.clone()).unwrap();
        let mut reordered = definition.clone();
        reordered.targets.reverse();
        reordered.targets.push(MetadataTarget::Prim);
        builder.register_metadata(reordered).unwrap();
        let mut conflicting = definition.clone();
        conflicting.default = Some(Value::Float(1.0));
        assert_eq!(
            builder.register_metadata(conflicting).unwrap_err(),
            MetadataConflict { name }
        );
        let registry = builder.build(&mut tokens);
        assert_eq!(registry.metadata_definitions().count(), 1);
        assert_eq!(registry.metadata(name), Some(&definition));
        assert!(
            registry
                .metadata(name)
                .unwrap()
                .applies_to(MetadataTarget::Prim)
        );
        assert!(
            !registry
                .metadata(name)
                .unwrap()
                .applies_to(MetadataTarget::Layer)
        );
    }
}
