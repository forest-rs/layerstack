// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The shape of the generated tables, and how they become
//! [`SchemaDefinition`]s.

use layerstack::{
    PropertyDefinition, PropertyKind, PropertyType, SchemaDefinition, SchemaKind, TokenInterner,
    Value, Variability,
};

/// Constructs a value, interning its tokens in the caller's interner.
pub(crate) type Construct = fn(&mut TokenInterner) -> Value;

/// A property's fallback constructor; a function so the generated tables
/// can write fallbacks as closures.
pub(crate) const fn fallback(construct: Construct) -> Option<Construct> {
    Some(construct)
}

/// A declared value type.
pub(crate) struct ValueType {
    /// The type name as OpenUSD writes it (`float3[]`).
    pub(crate) name: &'static str,
    /// Whether it is an array type.
    pub(crate) is_array: bool,
    /// One element of the type's zero value.
    pub(crate) zero: Construct,
}

/// A property a schema defines or overrides.
pub(crate) struct Property {
    pub(crate) name: &'static str,
    pub(crate) kind: PropertyKind,
    pub(crate) value_type: Option<&'static ValueType>,
    pub(crate) variability: Variability,
    pub(crate) fallback: Option<Construct>,
}

/// A schema, as `layerstack::schema::read_generated_schema` reads it.
pub(crate) struct Schema {
    pub(crate) name: &'static str,
    pub(crate) kind: SchemaKind,
    pub(crate) parent: Option<&'static str>,
    pub(crate) built_ins: &'static [&'static str],
    pub(crate) properties: &'static [Property],
    pub(crate) overrides: &'static [Property],
}

/// One domain's schemas and auto-applies.
pub(crate) struct DomainTables {
    pub(crate) schemas: &'static [Schema],
    /// `(applied schema, target)` pairs.
    pub(crate) auto_applies: &'static [(&'static str, &'static str)],
}

impl Property {
    fn definition(&self, tokens: &mut TokenInterner) -> PropertyDefinition {
        PropertyDefinition {
            name: tokens.intern(self.name),
            kind: self.kind,
            type_name: self
                .value_type
                .map(|t| PropertyType::new(t.name, t.is_array, (t.zero)(tokens))),
            variability: self.variability,
            fallback: self.fallback.map(|construct| construct(tokens)),
        }
    }
}

impl Schema {
    /// The schema's definition, with its names interned in `tokens`.
    pub(crate) fn definition(&self, tokens: &mut TokenInterner) -> SchemaDefinition {
        let mut schema = SchemaDefinition::new(tokens.intern(self.name), self.kind);
        schema.parent = self.parent.map(|parent| tokens.intern(parent));
        schema.built_ins = self.built_ins.iter().map(|b| tokens.intern(b)).collect();
        schema.properties = self
            .properties
            .iter()
            .map(|p| p.definition(tokens))
            .collect();
        schema.overrides = self
            .overrides
            .iter()
            .map(|p| p.definition(tokens))
            .collect();
        schema
    }
}
