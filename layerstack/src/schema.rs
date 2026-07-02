// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Schema registry and fallback value resolution.
//!
//! Schemas define property sets with fallback values for prim types.
//! Typed schemas (`IsA`) form a single-inheritance hierarchy; applied schemas
//! (`HasA`) are mix-in schemas that add additional properties.
//!
//! Spec: AOUSD Core §13 (schemas), §13.3 (schema types and ordering),
//! §13.3.2.4 (fallback value resolution).

use alloc::vec::Vec;

use hashbrown::HashMap;

use crate::{doc::FieldValue, interner::TokenId};

/// A property defined by a schema, with a fallback value.
///
/// Spec: AOUSD Core §13.3 — schema properties include a name, type, and
/// fallback value.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyDefinition {
    /// The interned property (field) name.
    pub name: TokenId,
    /// The fallback value for this property when no opinion is authored.
    pub fallback: FieldValue,
}

/// A schema definition: typed (`IsA`) or applied (`HasA`).
///
/// Typed schemas define prim types with an optional single-inheritance parent.
/// Applied schemas are mix-ins that can be applied to any prim via `apiSchemas`.
///
/// Spec: AOUSD Core §13.3 (typed and applied schemas).
#[derive(Clone, Debug)]
pub struct SchemaDefinition {
    /// The schema type name token (e.g. `Mesh`, `Xform`, `CollectionAPI`).
    pub name: TokenId,
    /// `true` for typed (`IsA`) schemas, `false` for applied (`HasA`) schemas.
    pub is_typed: bool,
    /// `true` for abstract schemas that cannot be directly instantiated.
    ///
    /// Spec: AOUSD Core §13.3.1 (abstract typed schemas).
    pub is_abstract: bool,
    /// `true` for multiple-apply API schemas (instance name required).
    ///
    /// Spec: AOUSD Core §13.3.2.2 (multiple-apply schemas).
    pub is_multi_apply: bool,
    /// Parent schema for `IsA` single inheritance (typed schemas only).
    ///
    /// Spec: AOUSD Core §13.3.1 (typed schema inheritance).
    pub parent: Option<TokenId>,
    /// Built-in API schemas that are automatically part of this schema's
    /// prim definition.
    ///
    /// Spec: AOUSD Core §13.3.2.1 (schema inclusions — built-ins).
    pub built_in_api_schemas: Vec<TokenId>,
    /// Properties defined by this schema, each with a fallback value.
    pub properties: Vec<PropertyDefinition>,
}

impl SchemaDefinition {
    /// Creates a new typed schema with the given name.
    pub fn typed(name: TokenId) -> Self {
        Self {
            name,
            is_typed: true,
            is_abstract: false,
            is_multi_apply: false,
            parent: None,
            built_in_api_schemas: Vec::new(),
            properties: Vec::new(),
        }
    }

    /// Creates a new single-apply API schema with the given name.
    pub fn api(name: TokenId) -> Self {
        Self {
            name,
            is_typed: false,
            is_abstract: false,
            is_multi_apply: false,
            parent: None,
            built_in_api_schemas: Vec::new(),
            properties: Vec::new(),
        }
    }

    /// Creates a new multiple-apply API schema with the given name.
    pub fn multi_apply_api(name: TokenId) -> Self {
        Self {
            name,
            is_typed: false,
            is_abstract: false,
            is_multi_apply: true,
            parent: None,
            built_in_api_schemas: Vec::new(),
            properties: Vec::new(),
        }
    }

    /// Sets the parent schema (builder, consuming).
    pub fn with_parent(mut self, parent: TokenId) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Marks this schema as abstract (builder, consuming).
    pub fn with_abstract(mut self, is_abstract: bool) -> Self {
        self.is_abstract = is_abstract;
        self
    }

    /// Adds a built-in API schema (builder, consuming).
    pub fn with_built_in_api(mut self, api: TokenId) -> Self {
        self.built_in_api_schemas.push(api);
        self
    }

    /// Adds a property with a fallback value (builder, consuming).
    pub fn with_property(mut self, name: TokenId, fallback: impl Into<FieldValue>) -> Self {
        self.properties.push(PropertyDefinition {
            name,
            fallback: fallback.into(),
        });
        self
    }
}

/// A registry of schema definitions.
///
/// Maps schema type name tokens to their definitions and supports lookup of
/// fallback values through the `IsA` inheritance chain and applied API schemas.
///
/// Spec: AOUSD Core §13 (schemas).
#[derive(Clone, Debug, Default)]
pub struct SchemaRegistry {
    schemas: HashMap<TokenId, SchemaDefinition>,
    /// Auto-apply rules: maps a typed schema token to API schemas that
    /// automatically apply to prims of that type.
    ///
    /// Spec: AOUSD Core §13.3.2.1 (schema inclusions — auto-applies).
    auto_apply: HashMap<TokenId, Vec<TokenId>>,
}

impl SchemaRegistry {
    /// Creates an empty schema registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a schema definition.
    pub fn register(&mut self, schema: SchemaDefinition) {
        self.schemas.insert(schema.name, schema);
    }

    /// Registers an auto-apply rule: the given API schema automatically
    /// applies to prims of the given typed schema.
    ///
    /// Spec: AOUSD Core §13.3.2.1 (auto-applies).
    pub fn add_auto_apply(&mut self, typed_schema: TokenId, api_schema: TokenId) {
        self.auto_apply
            .entry(typed_schema)
            .or_default()
            .push(api_schema);
    }

    /// Returns the schema definition for a type name, if registered.
    pub fn get(&self, name: TokenId) -> Option<&SchemaDefinition> {
        self.schemas.get(&name)
    }

    /// Resolves the fallback value for a field on a prim with the given type
    /// and applied API schemas.
    ///
    /// Resolution order per §13.3.2.4:
    /// 1. Typed schema (walking the `IsA` inheritance chain, strongest first)
    /// 2. Applied API schemas in order (including built-ins and auto-applies)
    /// 3. `None` if no fallback is found
    ///
    /// Spec: AOUSD Core §13.3.2.4 (fallback value resolution).
    pub fn resolve_fallback(
        &self,
        type_name: Option<TokenId>,
        applied_api_schemas: &[TokenId],
        field: TokenId,
    ) -> Option<FieldValue> {
        // 1. Walk the typed schema inheritance chain (strongest = leaf type).
        if let Some(tn) = type_name
            && let Some(fallback) = self.resolve_typed_fallback(tn, field)
        {
            return Some(fallback);
        }

        // 2. Walk applied API schemas in authored order.
        for api_name in applied_api_schemas {
            if let Some(fallback) = self.property_fallback(*api_name, field) {
                return Some(fallback);
            }
        }

        // 3. Walk built-in API schemas of the typed schema chain.
        if let Some(tn) = type_name
            && let Some(fallback) = self.resolve_builtin_api_fallback(tn, field)
        {
            return Some(fallback);
        }

        // 4. Walk auto-apply API schemas.
        if let Some(tn) = type_name
            && let Some(auto_apis) = self.auto_apply.get(&tn)
        {
            for api_name in auto_apis {
                if let Some(fallback) = self.property_fallback(*api_name, field) {
                    return Some(fallback);
                }
            }
        }

        None
    }

    /// Walks the `IsA` inheritance chain looking for a fallback for `field`.
    fn resolve_typed_fallback(&self, type_name: TokenId, field: TokenId) -> Option<FieldValue> {
        let mut current = Some(type_name);
        // Guard against infinite loops from misconfigured schemas.
        let mut depth = 0;
        while let Some(tn) = current {
            if depth > 64 {
                break;
            }
            depth += 1;

            if let Some(fallback) = self.property_fallback(tn, field) {
                return Some(fallback);
            }
            current = self.schemas.get(&tn).and_then(|s| s.parent);
        }
        None
    }

    /// Walks built-in API schemas for the typed schema inheritance chain.
    fn resolve_builtin_api_fallback(
        &self,
        type_name: TokenId,
        field: TokenId,
    ) -> Option<FieldValue> {
        let mut current = Some(type_name);
        let mut depth = 0;
        while let Some(tn) = current {
            if depth > 64 {
                break;
            }
            depth += 1;

            if let Some(schema) = self.schemas.get(&tn) {
                for api_name in &schema.built_in_api_schemas {
                    if let Some(fallback) = self.property_fallback(*api_name, field) {
                        return Some(fallback);
                    }
                }
                current = schema.parent;
            } else {
                break;
            }
        }
        None
    }

    /// Returns the fallback value for a field directly on a single schema.
    fn property_fallback(&self, schema_name: TokenId, field: TokenId) -> Option<FieldValue> {
        let schema = self.schemas.get(&schema_name)?;
        schema
            .properties
            .iter()
            .find(|p| p.name == field)
            .map(|p| p.fallback.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Value;
    use crate::interner::TokenInterner;

    fn setup_interner() -> (
        TokenInterner,
        TokenId,
        TokenId,
        TokenId,
        TokenId,
        TokenId,
        TokenId,
        TokenId,
    ) {
        let mut t = TokenInterner::default();
        let mesh = t.intern("Mesh");
        let gprim = t.intern("Gprim");
        let extent = t.intern("extent");
        let double_sided = t.intern("doubleSided");
        let visibility = t.intern("visibility");
        let collection_api = t.intern("CollectionAPI");
        let includes = t.intern("includes");
        (
            t,
            mesh,
            gprim,
            extent,
            double_sided,
            visibility,
            collection_api,
            includes,
        )
    }

    #[test]
    fn typed_schema_fallback() {
        let (_t, mesh, _gprim, extent, double_sided, _vis, _capi, _inc) = setup_interner();

        let mut reg = SchemaRegistry::new();
        reg.register(
            SchemaDefinition::typed(mesh)
                .with_property(extent, Value::Null)
                .with_property(double_sided, Value::Bool(false)),
        );

        assert_eq!(
            reg.resolve_fallback(Some(mesh), &[], extent),
            Some(FieldValue::Value(Value::Null))
        );
        assert_eq!(
            reg.resolve_fallback(Some(mesh), &[], double_sided),
            Some(FieldValue::Value(Value::Bool(false)))
        );
    }

    #[test]
    fn isa_inheritance_fallback() {
        let (_t, mesh, gprim, extent, double_sided, visibility, _capi, _inc) = setup_interner();

        let mut reg = SchemaRegistry::new();
        reg.register(
            SchemaDefinition::typed(gprim)
                .with_property(double_sided, Value::Bool(false))
                .with_property(visibility, Value::from("inherited")),
        );
        reg.register(
            SchemaDefinition::typed(mesh)
                .with_parent(gprim)
                .with_property(extent, Value::Null),
        );

        // Mesh's own property.
        assert_eq!(
            reg.resolve_fallback(Some(mesh), &[], extent),
            Some(FieldValue::Value(Value::Null))
        );
        // Inherited from Gprim.
        assert_eq!(
            reg.resolve_fallback(Some(mesh), &[], double_sided),
            Some(FieldValue::Value(Value::Bool(false)))
        );
        assert_eq!(
            reg.resolve_fallback(Some(mesh), &[], visibility),
            Some(FieldValue::Value(Value::from("inherited")))
        );
    }

    #[test]
    fn applied_api_schema_fallback() {
        let (_t, mesh, _gprim, _extent, _ds, _vis, collection_api, includes) = setup_interner();

        let mut reg = SchemaRegistry::new();
        reg.register(SchemaDefinition::typed(mesh));
        reg.register(SchemaDefinition::api(collection_api).with_property(includes, Value::Null));

        // No authored API schemas → no fallback.
        assert_eq!(reg.resolve_fallback(Some(mesh), &[], includes), None);
        // With applied API schema → fallback found.
        assert_eq!(
            reg.resolve_fallback(Some(mesh), &[collection_api], includes),
            Some(FieldValue::Value(Value::Null))
        );
    }

    #[test]
    fn typed_beats_api_for_same_field() {
        let (_t, mesh, _gprim, extent, _ds, _vis, collection_api, _inc) = setup_interner();

        let mut reg = SchemaRegistry::new();
        reg.register(SchemaDefinition::typed(mesh).with_property(extent, Value::Double(1.0)));
        reg.register(
            SchemaDefinition::api(collection_api).with_property(extent, Value::Double(99.0)),
        );

        // Typed schema's fallback is stronger than applied API schema's.
        assert_eq!(
            reg.resolve_fallback(Some(mesh), &[collection_api], extent),
            Some(FieldValue::Value(Value::Double(1.0)))
        );
    }

    #[test]
    fn builtin_api_schemas() {
        let mut t = TokenInterner::default();
        let mesh = t.intern("Mesh");
        let some_api = t.intern("SomeAPI");
        let api_field = t.intern("apiField");

        let mut reg = SchemaRegistry::new();
        reg.register(SchemaDefinition::api(some_api).with_property(api_field, Value::Int(42)));
        reg.register(SchemaDefinition::typed(mesh).with_built_in_api(some_api));

        // Built-in API schema provides fallback even without explicit apiSchemas.
        assert_eq!(
            reg.resolve_fallback(Some(mesh), &[], api_field),
            Some(FieldValue::Value(Value::Int(42)))
        );
    }

    #[test]
    fn auto_apply_api_schemas() {
        let mut t = TokenInterner::default();
        let mesh = t.intern("Mesh");
        let auto_api = t.intern("AutoAPI");
        let auto_field = t.intern("autoField");

        let mut reg = SchemaRegistry::new();
        reg.register(SchemaDefinition::typed(mesh));
        reg.register(SchemaDefinition::api(auto_api).with_property(auto_field, Value::Bool(true)));
        reg.add_auto_apply(mesh, auto_api);

        assert_eq!(
            reg.resolve_fallback(Some(mesh), &[], auto_field),
            Some(FieldValue::Value(Value::Bool(true)))
        );
    }

    #[test]
    fn no_type_no_fallback() {
        let mut t = TokenInterner::default();
        let field = t.intern("someField");

        let reg = SchemaRegistry::new();
        assert_eq!(reg.resolve_fallback(None, &[], field), None);
    }

    #[test]
    fn unregistered_type_no_fallback() {
        let mut t = TokenInterner::default();
        let unknown = t.intern("UnknownType");
        let field = t.intern("someField");

        let reg = SchemaRegistry::new();
        assert_eq!(reg.resolve_fallback(Some(unknown), &[], field), None);
    }
}
