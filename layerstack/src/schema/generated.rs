// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Schema definitions read from a generated schema layer.
//!
//! OpenUSD describes its schemas in `generatedSchema.usda` files: one
//! `class` prim per schema, named for it, holding the schema's properties
//! (a typed schema's inherited ones included), its built-ins as
//! `apiSchemas` and its override property names in
//! `customData.apiSchemaOverridePropertyNames`. A multiple-apply schema's
//! names hold [`INSTANCE_NAME_PLACEHOLDER`], and its built-ins name
//! `Other:__INSTANCE_NAME__` (by type) or `Other:__INSTANCE_NAME__:sub`
//! (by named instance). Each schema's kind, base and auto-applies live in
//! the plugin's `plugInfo.json` instead, which the caller reads and passes
//! here as [`SchemaDeclaration`]s and
//! [`SchemaRegistryBuilder::auto_apply`](super::SchemaRegistryBuilder::auto_apply)
//! calls.
//!
//! Spec: AOUSD Core §13.1 leaves how schemas are defined to the
//! implementation; this reads OpenUSD's form (`usdGenSchema`,
//! `UsdSchemaRegistry`).

use alloc::{format, string::String, vec::Vec};

use super::{INSTANCE_NAME_PLACEHOLDER, PropertyDefinition, SchemaDefinition, SchemaKind};
use crate::{
    doc::{FieldValue, Layer, Value},
    interner::{TokenId, TokenInterner},
    path::{Path, PathInterner},
    property::PropertyKind,
};

/// What a schema's plugin declares about it, which a generated schema
/// layer does not hold: its kind and the typed schema it inherits from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaDeclaration {
    /// The schema's name, which is also its prim's name in the layer.
    pub name: TokenId,
    /// The schema's kind.
    pub kind: SchemaKind,
    /// The typed schema it inherits from, if any.
    pub parent: Option<TokenId>,
}

/// Why [`read_generated_schema`] could not read a declared schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeneratedSchemaError {
    /// The layer has no root prim named for the declared schema.
    MissingSchema {
        /// The declared schema's name.
        name: TokenId,
    },
}

impl core::fmt::Display for GeneratedSchemaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingSchema { name } => {
                write!(f, "no prim defines declared schema {name:?}")
            }
        }
    }
}

impl core::error::Error for GeneratedSchemaError {}

/// Reads the definitions of the `declared` schemas from `layer`, a
/// generated schema layer, in declaration order.
///
/// Built-ins are converted from OpenUSD's template form to the form
/// [`SchemaDefinition::built_ins`] documents (`Other:__INSTANCE_NAME__`
/// becomes `Other`, `Other:__INSTANCE_NAME__:sub` becomes `Other:sub`).
/// Properties named in `apiSchemaOverridePropertyNames` become
/// [`SchemaDefinition::overrides`]; every other property's authored default
/// is its fallback.
///
/// # Errors
///
/// [`GeneratedSchemaError::MissingSchema`] when a declared schema has no
/// root prim in `layer`.
pub fn read_generated_schema(
    layer: &Layer,
    declared: &[SchemaDeclaration],
    tokens: &mut TokenInterner,
    paths: &PathInterner,
) -> Result<Vec<SchemaDefinition>, GeneratedSchemaError> {
    let api_schemas = tokens.intern("apiSchemas");
    let custom_data = tokens.intern("customData");
    declared
        .iter()
        .map(|declaration| {
            let spec = paths
                .lookup(&Path::root().join(&[declaration.name]))
                .and_then(|path| layer.prims.get(&path))
                .ok_or(GeneratedSchemaError::MissingSchema {
                    name: declaration.name,
                })?;
            let mut schema = SchemaDefinition::new(declaration.name, declaration.kind);
            schema.parent = declaration.parent;
            if let Some(FieldValue::TokenListOp(list)) = spec.field(api_schemas) {
                schema.built_ins = list
                    .apply_to(&[])
                    .into_iter()
                    .map(|name| from_template(name, tokens))
                    .collect();
            }
            let overrides = override_names(spec.field(custom_data), tokens);
            for entry in &spec.properties {
                let property = PropertyDefinition {
                    name: entry.name,
                    kind: entry.spec.kind,
                    type_name: entry.spec.type_name.clone(),
                    variability: entry.spec.variability,
                    fallback: match entry.spec.kind {
                        PropertyKind::Attribute => entry.spec.default.clone(),
                        PropertyKind::Relationship => None,
                    },
                };
                if overrides.contains(&entry.name) {
                    schema.overrides.push(property);
                } else {
                    schema.properties.push(property);
                }
            }
            Ok(schema)
        })
        .collect()
}

/// `name` without the placeholder that follows the schema name in a
/// multiple-apply template's built-in.
fn from_template(name: TokenId, tokens: &mut TokenInterner) -> TokenId {
    let text = tokens.resolve(name);
    let Some((schema, rest)) = text.split_once(':') else {
        return name;
    };
    let converted: String = if rest == INSTANCE_NAME_PLACEHOLDER {
        String::from(schema)
    } else if let Some(sub) = rest
        .strip_prefix(INSTANCE_NAME_PLACEHOLDER)
        .and_then(|rest| rest.strip_prefix(':'))
    {
        format!("{schema}:{sub}")
    } else {
        return name;
    };
    tokens.intern(converted)
}

/// The names `customData.apiSchemaOverridePropertyNames` lists.
fn override_names(custom_data: Option<&FieldValue>, tokens: &mut TokenInterner) -> Vec<TokenId> {
    let Some(FieldValue::Value(Value::Dictionary(entries))) = custom_data else {
        return Vec::new();
    };
    let Some((_, Value::Array(names))) = entries
        .iter()
        .find(|(key, _)| &**key == "apiSchemaOverridePropertyNames")
    else {
        return Vec::new();
    };
    names
        .iter()
        .filter_map(|name| match name {
            Value::Token(token) => Some(*token),
            Value::String(text) => Some(tokens.intern(&**text)),
            _ => None,
        })
        .collect()
}
