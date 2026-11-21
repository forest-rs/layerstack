// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use hashbrown::{HashMap, HashSet, hash_map::Entry};
use layerstack::{
    CannotApply, ListOp, PrimDefinition, PrimSpec, PropertyDefinition, PropertyEntry, PropertyKind,
    PropertySpec, PropertyType, SchemaRegistry, TargetPath, TokenId, TokenInterner, Value,
    Variability,
};

use super::{
    BatchError,
    index::{ApiMemberships, AppliedIndex, template_parts},
};

// Comparing a property against applied templates costs more than checking a
// list of applied names or authored property slots for duplicates.
const INDEX_API_DECLARATIONS_AT: usize = 8;
const INDEX_API_NAMES_AT: usize = 16;
const INDEX_PROPERTY_SLOTS_AT: usize = 32;

/// A new prim being defined in a [`LayerBatch`](super::LayerBatch).
/// Properties use the prim's schema and any APIs applied to it. Only
/// explicitly written values and relationships get authored specs.
///
/// Spec: AOUSD Core §7.6 (specs), §13.3.2.3 (schema property declarations).
#[derive(Debug)]
pub struct BatchPrim<'a> {
    spec: PrimSpec,
    schemas: &'a SchemaRegistry,
    tokens: &'a TokenInterner,
    typed: &'a PrimDefinition,
    applied: Vec<TokenId>,
    name_buffer: String,
    applied_index: Option<Box<AppliedIndex<'a>>>,
    memberships: Option<Box<ApiMemberships>>,
    property_slots: Option<HashMap<TokenId, usize>>,
}

impl<'a> BatchPrim<'a> {
    pub(super) fn new(
        type_name: TokenId,
        schemas: &'a SchemaRegistry,
        tokens: &'a TokenInterner,
    ) -> Self {
        Self {
            spec: PrimSpec::def().with_type_name(type_name),
            schemas,
            tokens,
            typed: schemas
                .schema_definition(type_name)
                .expect("registered concrete schema"),
            applied: Vec::new(),
            name_buffer: String::new(),
            applied_index: None,
            memberships: None,
            property_slots: None,
        }
    }

    pub(super) fn into_spec(mut self) -> PrimSpec {
        if !self.applied.is_empty() {
            self.spec.set_field(
                self.tokens.intern("apiSchemas"),
                ListOp::prepended(self.applied),
            );
        }
        self.spec
    }

    /// Authors a default for the declared attribute `name`. A subsequent
    /// call replaces that default. Shared arrays are moved into the spec.
    ///
    /// # Errors
    ///
    /// Returns an error when the prim's schemas do not declare the
    /// attribute with a registered type that accepts `value`.
    pub fn set(&mut self, name: &str, value: impl Into<Value>) -> Result<(), BatchError> {
        let name = self.property_name(name)?;
        self.set_value(name, value.into())
    }

    /// Defines and sets a default for a typed attribute, including names that
    /// the registry cannot declare in advance, such as user-named primvars.
    /// Registered attributes must match the supplied type and keep their
    /// declared variability. Other attributes are varying, non-custom specs.
    ///
    /// # Errors
    ///
    /// Rejects an invalid name, a name already authored on this prim,
    /// an unregistered value type or a value that does not match the type.
    /// Rejects conflicting schema declarations.
    ///
    /// Spec: AOUSD Core §7.6.4 (attribute specs), §13.3.2.3 (schema declarations).
    pub fn define_attribute(
        &mut self,
        name: &str,
        type_name: PropertyType,
        value: impl Into<Value>,
    ) -> Result<(), BatchError> {
        let id = self.property_name(name)?;
        let present = match &self.property_slots {
            Some(slots) => slots.contains_key(&id),
            None => self.spec.property(id).is_some(),
        };
        if present {
            return Err(BatchError::DuplicateProperty(name.into()));
        }
        let variability = match self
            .typed
            .property(id)
            .or_else(|| self.applied_property(id))
        {
            Some(declaration) if declaration.type_name.as_ref() != Some(&type_name) => {
                return Err(BatchError::AttributeDeclarationConflict(name.into()));
            }
            Some(declaration) => declaration.variability,
            None => Variability::Varying,
        };
        self.set_attribute_value(id, type_name, variability, value.into())
    }

    /// Sets the `interpolation` metadata token on an authored attribute,
    /// for example a normal or a primvar.
    ///
    /// # Errors
    ///
    /// Rejects a missing attribute or a relationship.
    ///
    /// Spec: AOUSD Core §7.6.4 (attribute specs).
    #[cfg(feature = "usd-geom")]
    pub fn set_interpolation(
        &mut self,
        name: &str,
        interpolation: crate::primvar::PrimvarInterpolation,
    ) -> Result<(), BatchError> {
        let id = self.property_name(name)?;
        let spec = match &self.property_slots {
            Some(slots) => slots
                .get(&id)
                .map(|&index| Arc::make_mut(&mut self.spec.properties[index].spec)),
            None => self.spec.property_mut(id),
        }
        .ok_or_else(|| BatchError::UnknownProperty(name.into()))?;
        if spec.kind != PropertyKind::Attribute {
            return Err(BatchError::WrongPropertyKind {
                name: name.into(),
                expected: PropertyKind::Attribute,
            });
        }
        spec.set_metadata(
            self.tokens.intern("interpolation"),
            Value::Token(self.tokens.intern(interpolation.token())),
        );
        Ok(())
    }

    /// Interns a token into this batch's store for use in an authored value.
    #[must_use]
    pub fn token(&self, value: &str) -> TokenId {
        self.tokens.intern(value)
    }

    /// Replaces the declared relationship's targets in authored order. An
    /// empty list explicitly authors no targets.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown property, an attribute, or a
    /// repeated target (AOUSD Core §6.6.3.2, §7.6.5).
    pub fn set_targets(&mut self, name: &str, targets: Vec<TargetPath>) -> Result<(), BatchError> {
        let name = self.property_name(name)?;
        self.set_relationship(name, targets)
    }

    /// Applies a registered API and returns a handle to its declared
    /// properties. Supply an instance only for a multiple-apply API.
    /// Applying the same API and instance again preserves its position in
    /// the authored `apiSchemas` prepend list.
    ///
    /// # Errors
    ///
    /// Returns the registry's application error for an unknown API, an
    /// invalid instance or a prim outside the API's allowed types.
    ///
    /// Spec: AOUSD Core §13.2.1.2 (`apiSchemas`), §13.3.2 (applied schemas).
    pub fn apply(
        &mut self,
        schema: &str,
        instance: Option<&str>,
    ) -> Result<BatchApi<'_, 'a>, BatchError> {
        let rejected = |reason| BatchError::CannotApply {
            schema: schema.into(),
            reason,
        };
        let schema_id = self
            .tokens
            .lookup(schema)
            .filter(|&id| self.tokens.resolve(id) == schema)
            .ok_or_else(|| rejected(CannotApply::NotAnAppliedSchema))?;
        let definition = self
            .schemas
            .schema_definition(schema_id)
            .ok_or_else(|| rejected(CannotApply::NotAnAppliedSchema))?;
        self.check_api_application(schema_id, instance)
            .map_err(rejected)?;
        let entry = match instance {
            None => schema_id,
            Some(instance) => {
                self.name_buffer.clear();
                self.name_buffer.push_str(schema);
                self.name_buffer.push(':');
                self.name_buffer.push_str(instance);
                self.tokens.intern(&self.name_buffer)
            }
        };
        let instance = instance.map(|name| self.tokens.intern(name));
        self.record_api(entry);
        Ok(BatchApi {
            prim: self,
            definition,
            instance,
        })
    }

    fn check_api_application(
        &mut self,
        schema: TokenId,
        instance: Option<&str>,
    ) -> Result<(), CannotApply> {
        let checked = self
            .schemas
            .can_apply(self.typed, schema, instance, self.tokens);
        if !matches!(checked, Err(CannotApply::NotATarget { .. })) || self.applied.is_empty() {
            return checked;
        }
        let memberships = self.memberships.get_or_insert_with(|| {
            let mut memberships = Box::new(ApiMemberships::new(self.typed));
            for &entry in &self.applied {
                memberships.insert(entry, self.schemas, self.tokens);
            }
            memberships
        });
        self.schemas.can_apply_with_apis(
            self.typed,
            schema,
            instance,
            self.tokens,
            |schema, instance| memberships.contains(schema, instance),
        )
    }

    fn record_api(&mut self, entry: TokenId) {
        if self.applied_index.is_none() && self.applied.len() >= INDEX_API_NAMES_AT {
            self.applied_index = Some(Box::new(AppliedIndex::new(&self.applied)));
        }
        let new = match &mut self.applied_index {
            Some(index) => index.insert(entry, self.applied.len(), self.schemas, self.tokens),
            None => !self.applied.contains(&entry),
        };
        if !new {
            return;
        }
        self.applied.push(entry);
        if let Some(memberships) = &mut self.memberships {
            memberships.insert(entry, self.schemas, self.tokens);
        }
    }

    fn property_name(&self, name: &str) -> Result<TokenId, BatchError> {
        // A valid instance's expanded property may not yet have been
        // interned. Do not let token interning truncate a misspelled name.
        if !name.split(':').all(layerstack::ident::is_identifier) {
            return Err(BatchError::UnknownProperty(name.into()));
        }
        Ok(self.tokens.intern(name))
    }

    fn declaration(
        &mut self,
        name: TokenId,
        expected: PropertyKind,
    ) -> Result<&'a PropertyDefinition, BatchError> {
        let definition = self
            .property_definition(name)
            .ok_or_else(|| BatchError::UnknownProperty(self.tokens.resolve(name).into()))?;
        if definition.kind != expected {
            return Err(BatchError::WrongPropertyKind {
                name: self.tokens.resolve(name).into(),
                expected,
            });
        }
        Ok(definition)
    }

    fn property_definition(&mut self, name: TokenId) -> Option<&'a PropertyDefinition> {
        self.typed
            .property(name)
            .or_else(|| self.applied_property(name))
    }

    fn applied_property(&mut self, name: TokenId) -> Option<&'a PropertyDefinition> {
        if self.applied.len() < INDEX_API_DECLARATIONS_AT {
            return self.schemas.strongest_property_definition(
                None,
                &self.applied,
                name,
                self.tokens,
            );
        }
        let index = self
            .applied_index
            .get_or_insert_with(|| Box::new(AppliedIndex::new(&self.applied)));
        index.property(name, &self.applied, self.schemas, self.tokens)
    }

    fn set_value(&mut self, name: TokenId, value: Value) -> Result<(), BatchError> {
        let def = self.declaration(name, PropertyKind::Attribute)?;
        let ty = def
            .type_name
            .as_ref()
            .ok_or_else(|| BatchError::UndeclaredType(self.tokens.resolve(name).into()))?;
        self.set_attribute_value(name, ty.clone(), def.variability, value)
    }

    fn set_attribute_value(
        &mut self,
        name: TokenId,
        ty: PropertyType,
        variability: Variability,
        value: Value,
    ) -> Result<(), BatchError> {
        if ty.value_type().is_none() {
            return Err(BatchError::UndeclaredType(self.tokens.resolve(name).into()));
        }
        if !ty.accepts_value(&value) {
            return Err(BatchError::ValueTypeMismatch {
                name: self.tokens.resolve(name).into(),
                expected: ty,
            });
        }
        let mut spec = PropertySpec::typed_attribute(ty).with_default(value);
        spec.variability = variability;
        self.set_property(name, spec);
        Ok(())
    }

    fn set_relationship(
        &mut self,
        name: TokenId,
        targets: Vec<TargetPath>,
    ) -> Result<(), BatchError> {
        let def = self.declaration(name, PropertyKind::Relationship)?;
        if targets.len() > 1 {
            let mut seen = HashSet::with_capacity(targets.len());
            if let Some(target) = targets.iter().copied().find(|&target| !seen.insert(target)) {
                return Err(BatchError::DuplicateTarget {
                    name: self.tokens.resolve(name).into(),
                    target,
                });
            }
        }
        let mut spec = PropertySpec::relationship().with_targets(ListOp::explicit(targets));
        spec.variability = def.variability;
        self.set_property(name, spec);
        Ok(())
    }

    fn set_property(&mut self, name: TokenId, spec: PropertySpec) {
        let properties = &mut self.spec.properties;
        if self.property_slots.is_none() && properties.len() >= INDEX_PROPERTY_SLOTS_AT {
            self.property_slots = Some(
                properties
                    .iter()
                    .enumerate()
                    .map(|(at, property)| (property.name, at))
                    .collect(),
            );
        }
        if let Some(slots) = &mut self.property_slots {
            match slots.entry(name) {
                Entry::Occupied(entry) => properties[*entry.get()].spec = Arc::new(spec),
                Entry::Vacant(entry) => {
                    entry.insert(properties.len());
                    properties.push(PropertyEntry {
                        name,
                        spec: Arc::new(spec),
                    });
                }
            }
            return;
        }
        self.spec.set_property(name, spec);
    }
}

/// An API applied to a [`BatchPrim`]. Pass property names as the API
/// declares them, including the `__INSTANCE_NAME__` segment of a
/// multiple-apply property template.
#[derive(Debug)]
pub struct BatchApi<'p, 'a> {
    prim: &'p mut BatchPrim<'a>,
    definition: &'a PrimDefinition,
    instance: Option<TokenId>,
}

impl BatchApi<'_, '_> {
    /// Authors a default for `template`, expanding this API's instance name.
    ///
    /// # Errors
    ///
    /// Returns an error when the API does not declare `template`, or
    /// [`BatchPrim::set`] rejects the attribute or value.
    pub fn set(&mut self, template: &str, value: impl Into<Value>) -> Result<(), BatchError> {
        let name = self.property_name(template)?;
        self.prim.set_value(name, value.into())
    }

    /// Authors the explicit targets of `template`, expanding this API's
    /// instance name.
    ///
    /// # Errors
    ///
    /// Returns an error when the API does not declare `template`, or
    /// [`BatchPrim::set_targets`] rejects the relationship or targets.
    pub fn set_targets(
        &mut self,
        template: &str,
        targets: Vec<TargetPath>,
    ) -> Result<(), BatchError> {
        let name = self.property_name(template)?;
        self.prim.set_relationship(name, targets)
    }

    fn property_name(&mut self, template: &str) -> Result<TokenId, BatchError> {
        let tokens = self.prim.tokens;
        let property = tokens
            .lookup(template)
            .filter(|&id| tokens.resolve(id) == template)
            .and_then(|id| self.definition.property(id))
            .ok_or_else(|| BatchError::UnknownProperty(template.into()))?;
        if let Some(instance) = self.instance
            && let Some((prefix, suffix)) = template_parts(template)
        {
            let buffer = &mut self.prim.name_buffer;
            buffer.clear();
            buffer.push_str(prefix);
            buffer.push_str(tokens.resolve(instance));
            buffer.push_str(suffix);
            return Ok(tokens.intern(buffer));
        }
        Ok(property.name)
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;

    use super::*;
    use layerstack::{FieldValue, InMemoryStore, PropertyType, SchemaDefinition, SchemaKind};

    #[test]
    fn apply_only_prims_index_names_without_preparing_declarations() {
        let store = InMemoryStore::default();
        let tokens = store.tokens();
        let ty = tokens.intern("Panel");
        let template = "slot:__INSTANCE_NAME__:visible";
        let attribute = |name| {
            PropertyDefinition::attribute(tokens.intern(name))
                .with_type(PropertyType::parse("bool").unwrap())
        };
        let mut builder = SchemaRegistry::builder();
        builder
            .register(SchemaDefinition::typed(ty).with_property(attribute("visible")))
            .register(
                SchemaDefinition::new(tokens.intern("SlotAPI"), SchemaKind::MultipleApplyApi)
                    .with_property(attribute(template)),
            );
        let registry = builder.build(tokens);

        let mut prim = BatchPrim::new(ty, &registry, tokens);
        for at in 0..INDEX_API_NAMES_AT {
            prim.apply("SlotAPI", Some(&format!("entry{at}"))).unwrap();
        }
        assert!(prim.applied_index.is_none());
        prim.apply("SlotAPI", Some("last")).unwrap();
        prim.apply("SlotAPI", Some("entry0")).unwrap();
        prim.set("visible", Value::Bool(true)).unwrap();
        assert!(!prim.applied_index.as_ref().unwrap().has_declarations());

        prim.set("slot:entry0:visible", Value::Bool(false)).unwrap();
        assert!(prim.applied_index.as_ref().unwrap().has_declarations());
        prim.apply("SlotAPI", Some("late"))
            .unwrap()
            .set(template, Value::Bool(true))
            .unwrap();
        let spec = prim.into_spec();
        let FieldValue::TokenListOp(applied) = spec.field(tokens.intern("apiSchemas")).unwrap()
        else {
            panic!("expected apiSchemas list op");
        };
        assert_eq!(applied.prepend.len(), INDEX_API_NAMES_AT + 2);
        assert_eq!(tokens.resolve(applied.prepend[0]), "SlotAPI:entry0");
        assert_eq!(
            tokens.resolve(*applied.prepend.last().unwrap()),
            "SlotAPI:late"
        );
        assert_eq!(
            spec.property(tokens.intern("slot:late:visible"))
                .unwrap()
                .default,
            Some(Value::Bool(true)),
        );
    }

    #[test]
    fn indexing_slots_on_a_rewrite_preserves_authored_order() {
        let store = InMemoryStore::default();
        let tokens = store.tokens();
        let ty = tokens.intern("Panel");
        let ty_int = PropertyType::parse("int").unwrap();
        let names: Vec<_> = (0..=INDEX_PROPERTY_SLOTS_AT)
            .map(|at| format!("field{at}"))
            .collect();
        let mut schema = SchemaDefinition::typed(ty);
        for name in &names {
            schema = schema.with_property(
                PropertyDefinition::attribute(tokens.intern(name)).with_type(ty_int.clone()),
            );
        }
        let mut builder = SchemaRegistry::builder();
        builder.register(schema);
        let registry = builder.build(tokens);
        let mut prim = BatchPrim::new(ty, &registry, tokens);
        for name in &names[..INDEX_PROPERTY_SLOTS_AT] {
            prim.set(name, Value::Int(1)).unwrap();
        }
        assert!(prim.property_slots.is_none());
        prim.set(&names[0], Value::Int(2)).unwrap();
        assert!(prim.property_slots.is_some());
        prim.set(&names[INDEX_PROPERTY_SLOTS_AT], Value::Int(3))
            .unwrap();
        let spec = prim.into_spec();
        assert_eq!(spec.properties.len(), names.len());
        for (property, name) in spec.properties.iter().zip(&names) {
            assert_eq!(tokens.resolve(property.name), name);
        }
        assert_eq!(spec.properties[0].spec.default, Some(Value::Int(2)));
        assert_eq!(
            spec.properties[INDEX_PROPERTY_SLOTS_AT].spec.default,
            Some(Value::Int(3))
        );
    }
}
