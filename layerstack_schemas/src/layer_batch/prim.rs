// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{format, vec::Vec};
use hashbrown::HashSet;
use layerstack::{
    CannotApply, ListOp, PrimDefinition, PrimSpec, PropertyDefinition, PropertyKind, PropertySpec,
    SchemaRegistry, TargetPath, TokenId, TokenInterner, Value,
};

use super::BatchError;

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
    applied: Vec<TokenId>,
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
            applied: Vec::new(),
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
        crate::edit::check_api_application(
            self.schemas,
            self.spec.type_name,
            || self.applied.as_slice(),
            schema_id,
            instance,
            self.tokens,
        )
        .map_err(rejected)?;
        let entry = match instance {
            None => schema_id,
            Some(instance) => self.tokens.intern(format!("{schema}:{instance}")),
        };
        if !self.applied.contains(&entry) {
            self.applied.push(entry);
        }
        let instance = instance.map(|name| self.tokens.intern(name));
        Ok(BatchApi {
            prim: self,
            definition,
            instance,
        })
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
        &self,
        name: TokenId,
        expected: PropertyKind,
    ) -> Result<&'a PropertyDefinition, BatchError> {
        let definition = self
            .schemas
            .strongest_property_definition(self.spec.type_name, &self.applied, name, self.tokens)
            .ok_or_else(|| BatchError::UnknownProperty(self.tokens.resolve(name).into()))?;
        if definition.kind != expected {
            return Err(BatchError::WrongPropertyKind {
                name: self.tokens.resolve(name).into(),
                expected,
            });
        }
        Ok(definition)
    }

    fn set_value(&mut self, name: TokenId, value: Value) -> Result<(), BatchError> {
        let def = self.declaration(name, PropertyKind::Attribute)?;
        let ty = def
            .type_name
            .as_ref()
            .filter(|ty| ty.value_type().is_some())
            .ok_or_else(|| BatchError::UndeclaredType(self.tokens.resolve(name).into()))?;
        if !ty.accepts_value(&value) {
            return Err(BatchError::ValueTypeMismatch {
                name: self.tokens.resolve(name).into(),
                expected: ty.clone(),
            });
        }
        let mut spec = PropertySpec::typed_attribute(ty.clone()).with_default(value);
        spec.variability = def.variability;
        self.spec.set_property(name, spec);
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
        self.spec.set_property(name, spec);
        Ok(())
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

    fn property_name(&self, template: &str) -> Result<TokenId, BatchError> {
        let tokens = self.prim.tokens;
        let property = tokens
            .lookup(template)
            .filter(|&id| tokens.resolve(id) == template)
            .and_then(|id| self.definition.property(id))
            .ok_or_else(|| BatchError::UnknownProperty(template.into()))?;
        Ok(match self.instance {
            Some(instance) => tokens.intern(crate::view::instance_name(
                template,
                tokens.resolve(instance),
            )),
            None => property.name,
        })
    }
}
