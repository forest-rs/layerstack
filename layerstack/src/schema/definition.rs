// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prim definitions: the schemas a prim has and the properties they define.

use alloc::{format, vec::Vec};

use hashbrown::HashMap;

use super::{INSTANCE_NAME_PLACEHOLDER, Names, PropertyDefinition};
use crate::interner::{TokenId, TokenInterner};

/// An applied schema in a [`PrimDefinition`].
///
/// Spec: AOUSD Core §13.3.2 (single and multiple applied schemas).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AppliedSchema {
    /// The applied name, as `apiSchemas` lists it: `LabelAPI`, or
    /// `SlotAPI:left` for an instance of a multiple-apply schema.
    pub name: TokenId,
    /// The schema's name (`SlotAPI`).
    pub schema: TokenId,
    /// The instance name of a multiple-apply schema (`left`), which may
    /// contain `:`; `None` for a single-apply schema.
    pub instance: Option<TokenId>,
}

/// The schemas a prim has, and the properties they define: its type, its
/// applied schemas in strength order, and for each defined property the
/// definition its strongest schema gives, with a fallback a weaker one
/// fills in when the stronger has none.
///
/// Built by [`crate::SchemaRegistry::prim_definition`] for a composed prim
/// (or [`crate::Stage::prim_definition`]), and for each schema itself by
/// [`crate::SchemaRegistry::schema_definition`].
///
/// Spec: AOUSD Core §13.3.2.3 (the prim definition), §13.3.2.4 (fallback
/// values in its order).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrimDefinition {
    /// The typed schema, if the prim has one.
    pub(super) type_name: Option<TokenId>,
    /// The typed schema and its ancestors, nearest first.
    pub(super) type_chain: Vec<TokenId>,
    applied: Vec<AppliedSchema>,
    properties: Vec<PropertyDefinition>,
    index: HashMap<TokenId, usize>,
}

impl PrimDefinition {
    /// The prim's typed schema; `None` when the prim is typeless: it has no
    /// type name, or one that names an abstract or unknown schema.
    ///
    /// Spec: AOUSD Core §13.3.1.
    #[must_use]
    pub fn type_name(&self) -> Option<TokenId> {
        self.type_name
    }

    /// Whether the prim's type is `schema` or inherits from it, abstract
    /// ancestors included. Always `false` for a typeless prim.
    ///
    /// Spec: AOUSD Core §13.3.1 ("a prim is of type T if ...").
    #[must_use]
    pub fn is_a(&self, schema: TokenId) -> bool {
        self.type_chain.contains(&schema)
    }

    /// The applied schemas, strongest first: the type's built-ins and
    /// auto-applies, then each authored applied schema followed by its own,
    /// each schema once.
    ///
    /// OpenUSD: `UsdPrim::GetAppliedSchemas`.
    #[must_use]
    pub fn applied_schemas(&self) -> &[AppliedSchema] {
        &self.applied
    }

    /// Whether the applied schema `schema` is applied: a single-apply
    /// schema, or any instance of a multiple-apply one.
    ///
    /// Spec: AOUSD Core §13.3.2 ("has an" applied schema).
    #[must_use]
    pub fn has_api(&self, schema: TokenId) -> bool {
        self.applied.iter().any(|applied| applied.schema == schema)
    }

    /// Whether the multiple-apply schema `schema` is applied with the
    /// instance name `instance`.
    ///
    /// Spec: AOUSD Core §13.3.2.
    #[must_use]
    pub fn has_api_instance(&self, schema: TokenId, instance: TokenId) -> bool {
        self.applied
            .iter()
            .any(|applied| applied.schema == schema && applied.instance == Some(instance))
    }

    /// The definition of the property `name`, if a schema of the prim
    /// defines it.
    #[must_use]
    pub fn property(&self, name: TokenId) -> Option<&PropertyDefinition> {
        self.index.get(&name).map(|&i| &self.properties[i])
    }

    /// The defined properties, in the order their schemas compose.
    #[must_use]
    pub fn properties(&self) -> &[PropertyDefinition] {
        &self.properties
    }

    pub(super) fn property_mut(&mut self, name: TokenId) -> Option<&mut PropertyDefinition> {
        self.index.get(&name).map(|&i| &mut self.properties[i])
    }

    pub(super) fn push_applied(&mut self, applied: AppliedSchema) {
        if !self.applied.iter().any(|a| a.name == applied.name) {
            self.applied.push(applied);
        }
    }

    /// Adds `property` as weaker than the properties already defined: a
    /// new name is added; an existing one keeps its definition, taking the
    /// fallback when it has none and the types agree.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (`compose_prim_definition`). OpenUSD:
    /// `UsdPrimDefinition::_AddOrComposeProperty`.
    pub(super) fn add_property(&mut self, property: PropertyDefinition) {
        match self.index.get(&property.name) {
            Some(&i) => self.properties[i].compose_weaker(&property),
            None => {
                self.index.insert(property.name, self.properties.len());
                self.properties.push(property);
            }
        }
    }

    /// Composes `weaker` into this definition as a weaker one: its applied
    /// schemas follow these, and its properties are added as weaker, with
    /// `instance` filling in the placeholder of a multiple-apply template.
    /// An entry whose instantiated name `names` cannot form is left out.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (`compose_prim_definition`). OpenUSD:
    /// `UsdPrimDefinition::_ComposeWeakerAPIPrimDefinition`.
    pub(super) fn compose_weaker(
        &mut self,
        weaker: &Self,
        instance: Option<TokenId>,
        names: &mut impl Names,
    ) {
        for applied in &weaker.applied {
            let Some(name) = names.instantiate(applied.name, instance) else {
                continue;
            };
            let applied_instance = match applied.instance {
                None => None,
                Some(template) => match names.instantiate(template, instance) {
                    Some(name) => Some(name),
                    None => continue,
                },
            };
            self.push_applied(AppliedSchema {
                name,
                schema: applied.schema,
                instance: applied_instance,
            });
        }
        for property in &weaker.properties {
            if let Some(name) = names.instantiate(property.name, instance) {
                self.add_property(PropertyDefinition {
                    name,
                    ..property.clone()
                });
            }
        }
    }

    /// The template properties of this multiple-apply definition that
    /// applying it as `instance` names `name`, in definition order.
    pub(super) fn instance_properties<'d>(
        &'d self,
        name: &str,
        instance: &str,
        tokens: &TokenInterner,
    ) -> Vec<&'d PropertyDefinition> {
        let mut found: Vec<usize> = Vec::new();
        let mut consider = |candidate: &str| {
            if let Some(&i) = tokens
                .lookup(candidate)
                .and_then(|token| self.index.get(&token))
                && !found.contains(&i)
            {
                found.push(i);
            }
        };
        consider(name);
        let mut start = 0;
        loop {
            let end = start + instance.len();
            if name[start..].starts_with(instance)
                && (end == name.len() || name.as_bytes()[end] == b':')
            {
                consider(&format!(
                    "{}{INSTANCE_NAME_PLACEHOLDER}{}",
                    &name[..start],
                    &name[end..]
                ));
            }
            match name[start..].find(':') {
                Some(colon) => start += colon + 1,
                None => break,
            }
        }
        found.sort_unstable();
        found.into_iter().map(|i| &self.properties[i]).collect()
    }
}

impl PropertyDefinition {
    /// Composes a weaker definition of this property into it: the fallback
    /// fills in when this one has none and the types agree.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (`compose_prim_definition` fills in
    /// `default`). OpenUSD: `_CreateComposedPrimOrPropertyIfNeeded`.
    pub(super) fn compose_weaker(&mut self, weaker: &Self) {
        if self.fallback.is_none() && self.has_type_of(weaker) {
            self.fallback.clone_from(&weaker.fallback);
        }
    }
}
