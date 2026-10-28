// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Schema definitions as static tables: the form `layerstack_schemagen`
//! writes OpenUSD's domains in, and a schema library's own
//! (`compile --rust`), so that registering them parses nothing at run time.
//!
//! A library's [`SchemaTables`] become [`SchemaDefinition`]s, auto-applies
//! and metadata declarations of a [`SchemaRegistryBuilder`] with
//! [`SchemaTables::register`], the same definitions
//! [`layerstack::schema::read_generated_schema`] reads from the library's
//! `generatedSchema.usda`. Names are interned when they are registered, in
//! the interner the stages using the registry share. The tables are
//! generated code's to write; their fields are public so that code in
//! another crate can.
//!
//! The table types are tied to this crate's minor version, not to its
//! semver: a release may add a field or change what one means, and then
//! bumps [`TABLE_FORMAT`]. A module `layerstack_schemagen compile --rust`
//! writes asserts the format it was written in at compile time, so a
//! library built against another format fails with the remedy
//! (regenerate it with the matching `layerstack_schemagen`) rather than
//! with a missing field alone.
//!
//! Spec: AOUSD Core §13 (schemas); §13.1 leaves how schemas are defined to
//! the implementation.

use layerstack::{
    MetadataConflict, PropertyDefinition, PropertyKind, PropertyType, SchemaDefinition, SchemaKind,
    SchemaRegistryBuilder, TokenInterner, Value, Variability,
};

/// The format of the table types ([`ValueType`], [`Property`], [`Schema`],
/// [`SchemaTables`], [`Metadata`]): their fields and what they mean. It
/// changes with any of them; generated library modules check it (see the
/// [module docs](self)).
pub const TABLE_FORMAT: u32 = 1;

/// Constructs a value, interning its tokens in the interner passed in.
pub type Construct = fn(&TokenInterner) -> Value;

/// `Some(construct)`: a fallback or default constructor, which the tables
/// write as a closure.
#[must_use]
pub const fn fallback(construct: Construct) -> Option<Construct> {
    Some(construct)
}

/// A declared value type.
#[derive(Debug)]
pub struct ValueType {
    /// The type name as OpenUSD writes it (`float3[]`): a registered value
    /// type ([`layerstack::ValueType`]) or its array form.
    pub name: &'static str,
}

impl ValueType {
    /// The property type the name parses as.
    ///
    /// # Panics
    ///
    /// If the name is not a registered value type, which the generated
    /// tables never hold.
    #[must_use]
    pub fn property_type(&self) -> PropertyType {
        PropertyType::parse(self.name).expect("generated tables name registered value types")
    }
}

/// A property a schema defines or overrides.
#[derive(Debug)]
pub struct Property {
    /// Its name, with `__INSTANCE_NAME__` in a multiple-apply schema.
    pub name: &'static str,
    /// Attribute or relationship.
    pub kind: PropertyKind,
    /// An attribute's value type.
    pub value_type: Option<&'static ValueType>,
    /// Its variability.
    pub variability: Variability,
    /// Its fallback value.
    pub fallback: Option<Construct>,
    /// Its other metadata the registry keeps (`allowedTokens`, …), by
    /// field name.
    pub metadata: &'static [(&'static str, Construct)],
}

/// A schema, as `layerstack::schema::read_generated_schema` reads it.
#[derive(Debug)]
pub struct Schema {
    /// Its identifier (`Mesh`, `CollectionAPI`).
    pub name: &'static str,
    /// Its kind.
    pub kind: SchemaKind,
    /// The typed schema it inherits from.
    pub parent: Option<&'static str>,
    /// Its built-in applied schemas (`apiSchemas`).
    pub built_ins: &'static [&'static str],
    /// The properties it defines.
    pub properties: &'static [Property],
    /// The properties of its built-ins it overrides.
    pub overrides: &'static [Property],
    /// `apiSchemaCanOnlyApplyTo`.
    pub can_only_apply_to: &'static [&'static str],
    /// `apiSchemaAllowedInstanceNames`.
    pub allowed_instance_names: &'static [&'static str],
    /// The `apiSchemaCanOnlyApplyTo` of each `apiSchemaInstances` entry.
    pub instance_can_only_apply_to: &'static [(&'static str, &'static [&'static str])],
}

/// A schema library's tables: one OpenUSD domain, or a library of one's
/// own.
#[derive(Debug)]
pub struct SchemaTables {
    /// The plugin's `SdfMetadata` declarations.
    pub metadata: &'static [Metadata],
    /// Its schemas.
    pub schemas: &'static [Schema],
    /// Its auto-applies: `(applied schema, target)` pairs.
    pub auto_applies: &'static [(&'static str, &'static str)],
}

impl SchemaTables {
    /// Registers the library's metadata declarations, schemas and
    /// auto-applies with `builder`, interning their names and values in
    /// `tokens`. The schemas they build on, such as OpenUSD's, are
    /// registered separately ([`crate::register`]); a schema whose parent
    /// or built-in no registration provides is an issue of the built
    /// registry (`SchemaRegistry::issues`).
    ///
    /// # Errors
    ///
    /// A metadata declaration that conflicts with one already registered.
    /// Registrations preceding the conflict remain in the builder.
    pub fn register(
        &self,
        builder: &mut SchemaRegistryBuilder,
        tokens: &TokenInterner,
    ) -> Result<(), MetadataConflict> {
        for metadata in self.metadata {
            builder.register_metadata(metadata.definition(tokens), tokens)?;
        }
        for schema in self.schemas {
            builder.register(schema.definition(tokens));
        }
        for (schema, target) in self.auto_applies {
            builder.auto_apply(tokens.intern(schema), tokens.intern(target));
        }
        Ok(())
    }
}

/// A plugin-defined metadata field (`SdfMetadata`).
#[derive(Debug)]
pub struct Metadata {
    /// The field's name.
    pub name: &'static str,
    /// Its value type's name.
    pub type_name: &'static str,
    /// The kinds of spec it applies to.
    pub targets: &'static [layerstack::MetadataTarget],
    /// Its registered default.
    pub default: Option<Construct>,
    /// Its `displayGroup`, empty when it has none.
    pub display_group: &'static str,
    /// Its documentation.
    pub documentation: &'static str,
}

impl Metadata {
    /// The declaration, with its names interned in `tokens`.
    ///
    /// # Panics
    ///
    /// If the type name is not a metadata type, which the generated tables
    /// never hold.
    #[must_use]
    pub fn definition(&self, tokens: &TokenInterner) -> layerstack::MetadataDefinition {
        let value_type = layerstack::MetadataType::parse(self.type_name)
            .expect("generated tables name metadata types");
        let mut definition = layerstack::MetadataDefinition::new(
            tokens.intern(self.name),
            value_type,
            self.targets.iter().copied(),
        )
        .with_display_group(self.display_group)
        .with_documentation(self.documentation);
        definition.default = self.default.map(|construct| construct(tokens));
        definition
    }
}

impl Property {
    fn definition(&self, tokens: &TokenInterner) -> PropertyDefinition {
        PropertyDefinition {
            name: tokens.intern(self.name),
            kind: self.kind,
            type_name: self.value_type.map(ValueType::property_type),
            variability: self.variability,
            fallback: self.fallback.map(|construct| construct(tokens)),
            metadata: self
                .metadata
                .iter()
                .map(|(name, construct)| layerstack::FieldEntry {
                    name: tokens.intern(name),
                    value: construct(tokens).into(),
                })
                .collect(),
        }
    }
}

impl Schema {
    /// The schema's definition, with its names interned in `tokens`;
    /// [flattened](SchemaDefinition::flattened), as the generated schema
    /// layer the table is read from is.
    #[must_use]
    pub fn definition(&self, tokens: &TokenInterner) -> SchemaDefinition {
        // The tables are read from generated schema layers, whose classes
        // hold their inherited properties already.
        let mut schema = SchemaDefinition::new(tokens.intern(self.name), self.kind).flattened();
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
        schema.can_only_apply_to = self
            .can_only_apply_to
            .iter()
            .map(|t| tokens.intern(t))
            .collect();
        schema.allowed_instance_names = self
            .allowed_instance_names
            .iter()
            .map(|name| tokens.intern(name))
            .collect();
        schema.instance_can_only_apply_to = self
            .instance_can_only_apply_to
            .iter()
            .map(|(instance, types)| {
                (
                    tokens.intern(instance),
                    types.iter().map(|t| tokens.intern(t)).collect(),
                )
            })
            .collect();
        schema
    }
}
