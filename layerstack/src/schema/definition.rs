// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prim definitions: the schemas a prim has and the properties they define.

use crate::sort;
#[cfg(feature = "std")]
use crate::sync::Memo;
use alloc::{borrow::Cow, boxed::Box, format, sync::Arc, vec::Vec};
use core::fmt;

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

impl AppliedSchema {
    pub(super) fn instantiate(
        self,
        instance: Option<TokenId>,
        names: &mut Names<'_>,
    ) -> Option<Self> {
        let name = names.instantiate(self.name, instance)?;
        let instance = match self.instance {
            Some(template) => Some(names.instantiate(template, instance)?),
            None => None,
        };
        Some(Self {
            name,
            schema: self.schema,
            instance,
        })
    }
}

/// The schemas a prim has, and the properties they define: its type, its
/// applied schemas in strength order, and for each defined property the
/// definition its strongest schema gives, with missing fallbacks and metadata
/// filled by weaker matching definitions and dictionary metadata merged.
///
/// Built by [`crate::SchemaRegistry::prim_definition`] for a composed prim
/// (or [`crate::Prim::definition`]), and for each schema itself by
/// [`crate::SchemaRegistry::schema_definition`].
///
/// Spec: AOUSD Core §13.3.2.3 (the prim definition), §13.3.2.4 (fallback
/// values in its order).
#[derive(Clone, Default)]
pub struct PrimDefinition {
    /// The typed schema, if the prim has one.
    pub(super) type_name: Option<TokenId>,
    /// The typed schema and its ancestors, in C3 order.
    pub(super) ancestors: Vec<TokenId>,
    applied: Vec<AppliedSchema>,
    properties: Vec<PropertyDefinition>,
    index: HashMap<TokenId, usize>,
    base: Option<Box<SharedBase>>,
}

/// The typed definition has its own property order and name index. A
/// composed prim keeps only new properties and replacements for base slots.
struct SharedBase {
    definition: Arc<PrimDefinition>,
    /// Changed base properties in order of their slots in `definition`.
    replacements: Vec<(usize, PropertyDefinition)>,
    #[cfg(feature = "std")]
    flattened: Memo<Vec<PropertyDefinition>>,
}

impl Clone for SharedBase {
    fn clone(&self) -> Self {
        Self {
            definition: Arc::clone(&self.definition),
            replacements: self.replacements.clone(),
            #[cfg(feature = "std")]
            flattened: Memo::default(),
        }
    }
}

impl PartialEq for PrimDefinition {
    fn eq(&self, other: &Self) -> bool {
        self.type_name == other.type_name
            && self.ancestors() == other.ancestors()
            && self.applied == other.applied
            && self.iter_properties().eq(other.iter_properties())
    }
}

impl fmt::Debug for PrimDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Properties<'a>(&'a PrimDefinition);
        impl fmt::Debug for Properties<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_list().entries(self.0.iter_properties()).finish()
            }
        }
        f.debug_struct("PrimDefinition")
            .field("type_name", &self.type_name)
            .field("ancestors", &self.ancestors())
            .field("applied", &self.applied)
            .field("properties", &Properties(self))
            .finish_non_exhaustive()
    }
}

impl PrimDefinition {
    /// Starts a composed definition based on the registry's concrete type.
    #[cfg(feature = "std")]
    pub(super) fn with_shared_base(base: Arc<Self>) -> Self {
        debug_assert!(base.base.is_none(), "the registry stores flat definitions");
        Self {
            type_name: base.type_name,
            applied: base.applied.clone(),
            base: Some(Box::new(SharedBase {
                definition: base,
                replacements: Vec::new(),
                flattened: Memo::default(),
            })),
            ..Self::default()
        }
    }

    /// Without `std`, keep the owned slice and the definition's `Send` and
    /// `Sync` guarantees. A lazy borrowed slice needs a thread-safe cell.
    #[cfg(not(feature = "std"))]
    pub(super) fn with_shared_base(base: Arc<Self>) -> Self {
        base.as_ref().clone()
    }

    /// The prim's typed schema; `None` when the prim is typeless: it has no
    /// type name, or one that names an abstract or unknown schema.
    ///
    /// Spec: AOUSD Core §13.3.1.
    #[must_use]
    pub fn type_name(&self) -> Option<TokenId> {
        self.type_name
    }

    /// Whether the prim's type is `schema` or inherits from it, abstract
    /// ancestors and [additional bases](crate::SchemaDefinition::additional_bases)
    /// included. Always `false` for a typeless prim.
    ///
    /// Spec: AOUSD Core §13.3.1 ("a prim is of type T if ..."). OpenUSD:
    /// `UsdPrim::IsA`.
    #[must_use]
    pub fn is_a(&self, schema: TokenId) -> bool {
        self.ancestors().contains(&schema)
    }

    /// The prim's type and the typed schemas it is, in OpenUSD's order:
    /// the C3 linearization of its bases, the type first. With a single
    /// base each, this is the parent chain, nearest first. Empty for a
    /// typeless prim.
    ///
    /// A consumer that picks behaviour by type, most derived first, walks
    /// this as OpenUSD walks `TfType::GetAllAncestorTypes`, as for
    /// `UsdGeomBoundable`'s extent functions.
    #[must_use]
    pub fn ancestors(&self) -> &[TokenId] {
        self.base
            .as_ref()
            .map_or(&self.ancestors, |base| &base.definition.ancestors)
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
        if let Some(&i) = self.index.get(&name) {
            return Some(&self.properties[i]);
        }
        let base = self.base.as_ref()?;
        let &i = base.definition.index.get(&name)?;
        Some(
            match base
                .replacements
                .binary_search_by_key(&i, |(slot, _)| *slot)
            {
                Ok(at) => &base.replacements[at].1,
                Err(_) => &base.definition.properties[i],
            },
        )
    }

    /// The defined properties, in the order their schemas compose.
    ///
    /// With the `std` feature, a composed definition may copy its properties
    /// into a contiguous slice on the first call. Use [`Self::iter_properties`]
    /// to visit them directly in shared storage.
    #[must_use]
    pub fn properties(&self) -> &[PropertyDefinition] {
        #[cfg(feature = "std")]
        if let Some(base) = &self.base
            && !base.definition.properties.is_empty()
        {
            if self.properties.is_empty() && base.replacements.is_empty() {
                return &base.definition.properties;
            }
            return base
                .flattened
                .get_or_init(|| self.iter_properties().cloned().collect());
        }
        &self.properties
    }

    /// Iterates defined properties in the order their schemas compose,
    /// borrowing shared properties directly.
    pub fn iter_properties(&self) -> impl Iterator<Item = &PropertyDefinition> {
        let (properties, replacements) = self.base.as_ref().map_or((&[][..], &[][..]), |base| {
            (
                base.definition.properties.as_slice(),
                base.replacements.as_slice(),
            )
        });
        let mut replacements = replacements.iter().peekable();
        properties
            .iter()
            .enumerate()
            .map(move |(i, property)| {
                replacements
                    .next_if(|(slot, _)| *slot == i)
                    .map_or(property, |(_, replacement)| replacement)
            })
            .chain(&self.properties)
    }

    /// The registry builder edits definitions before they can be shared.
    pub(super) fn property_mut(&mut self, name: TokenId) -> Option<&mut PropertyDefinition> {
        debug_assert!(
            self.base.is_none(),
            "the registry builder edits flat definitions"
        );
        self.index.get(&name).map(|&i| &mut self.properties[i])
    }

    pub(super) fn push_applied(&mut self, applied: AppliedSchema) {
        if !self.applied.iter().any(|a| a.name == applied.name) {
            self.applied.push(applied);
        }
    }

    /// Adds `property` as weaker than the properties already defined: a
    /// new name is added; an existing one keeps its definition, taking the
    /// fallback and metadata when missing, merging dictionaries when types agree.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (`compose_prim_definition`). OpenUSD:
    /// `UsdPrimDefinition::_AddOrComposeProperty`.
    pub(super) fn add_property(
        &mut self,
        name: TokenId,
        property: &PropertyDefinition,
        tokens: &TokenInterner,
    ) {
        if let Some(base) = &mut self.base {
            #[cfg(feature = "std")]
            if base.flattened.get_mut().is_some() {
                base.flattened = Memo::default();
            }
            if let Some(&slot) = base.definition.index.get(&name) {
                match base.replacements.binary_search_by_key(&slot, |(i, _)| *i) {
                    Ok(at) => base.replacements[at].1.compose_weaker(property, tokens),
                    Err(at) => {
                        let inherited = &base.definition.properties[slot];
                        if inherited.has_type_of(property) {
                            let mut replacement = inherited.clone();
                            replacement.compose_weaker(property, tokens);
                            base.replacements.insert(at, (slot, replacement));
                        }
                    }
                }
                return;
            }
        }
        match self.index.entry(name) {
            hashbrown::hash_map::Entry::Occupied(entry) => {
                self.properties[*entry.get()].compose_weaker(property, tokens);
            }
            hashbrown::hash_map::Entry::Vacant(entry) => {
                entry.insert(self.properties.len());
                self.properties.push(PropertyDefinition {
                    name,
                    ..property.clone()
                });
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
        names: &mut Names<'_>,
    ) {
        for applied in &weaker.applied {
            if let Some(applied) = applied.instantiate(instance, names) {
                self.push_applied(applied);
            }
        }
        for property in weaker.iter_properties() {
            if let Some(name) = names.instantiate(property.name, instance) {
                self.add_property(name, property, names.tokens);
            }
        }
    }

    /// The template properties of this multiple-apply definition that
    /// applying it as `instance` names `name`, in definition order: those
    /// whose name, with its placeholder filled in by `instance`, is `name`.
    pub(super) fn instance_properties<'d>(
        &'d self,
        name: &str,
        instance: &str,
        tokens: &TokenInterner,
    ) -> Vec<&'d PropertyDefinition> {
        debug_assert!(self.base.is_none(), "the registry stores flat templates");
        let mut found: Vec<usize> = Vec::new();
        let mut consider = |candidate: &str| {
            if let Some(&i) = tokens
                .lookup(candidate)
                .and_then(|token| self.index.get(&token))
                && !found.contains(&i)
                && instantiated_text(tokens.resolve(self.properties[i].name), instance) == name
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
        sort::sort_unstable(&mut found);
        found.into_iter().map(|i| &self.properties[i]).collect()
    }
}

/// `template` with its placeholder replaced by `instance`, as
/// [`super::SchemaRegistry::prim_definition`] names an instance's
/// properties; `template` itself when it has no placeholder.
fn instantiated_text(template: &str, instance: &str) -> alloc::string::String {
    match super::placeholder_offset(template) {
        None => template.into(),
        Some(offset) => format!(
            "{}{instance}{}",
            &template[..offset],
            &template[offset + INSTANCE_NAME_PLACEHOLDER.len()..]
        ),
    }
}

impl PropertyDefinition {
    /// Composes missing fallback values and metadata from matching weaker
    /// definitions, recursively merging dictionaries.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (`compose_prim_definition` fills in
    /// `default`). OpenUSD: `_CreateComposedPrimOrPropertyIfNeeded`.
    pub(super) fn compose_weaker(&mut self, weaker: &Self, tokens: &TokenInterner) {
        if !self.has_type_of(weaker) {
            return;
        }
        if self.fallback.is_none() {
            self.fallback.clone_from(&weaker.fallback);
        }
        // Core §13.3.2.3; OpenUSD `_CreateComposedPrimOrPropertyIfNeeded`:
        // missing metadata fills in, dictionaries recursively merge, while
        // custom/documentation never come from a weaker property definition.
        for field in &weaker.metadata {
            if matches!(tokens.resolve(field.name), "custom" | "documentation") {
                continue;
            }
            match crate::doc::get_field_mut(&mut self.metadata, &field.name) {
                Some(crate::FieldValue::Value(crate::Value::Dictionary(strong))) => {
                    if let crate::FieldValue::Value(crate::Value::Dictionary(weak)) = &field.value {
                        *strong = crate::doc::combine_dictionaries(strong, weak);
                    }
                }
                Some(crate::FieldValue::Value(strong))
                    if tokens.resolve(field.name) == "propertyOrder" =>
                {
                    if let (Some(names), crate::FieldValue::Value(weak)) =
                        (strong.array_ref(), &field.value)
                        && let Some(weak) = weak.array_ref()
                    {
                        // Core §13.3.2.3: propertyOrder appends unique names.
                        let mut merged: Vec<crate::Value> =
                            names.iter().map(Cow::into_owned).collect();
                        for value in weak.iter() {
                            if !merged.contains(&value) {
                                merged.push(value.into_owned());
                            }
                        }
                        *strong = crate::Value::array(merged);
                    }
                }
                Some(_) => {}
                None => self.metadata.push(field.clone()),
            }
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::Value;

    #[test]
    fn shared_base_matches_flat_composition_and_reuses_unmodified_slots() {
        let tokens = TokenInterner::default();
        let [kind, parent, a, b, c, d, e, group] =
            ["Type", "Parent", "a", "b", "c", "d", "e", "displayGroup"]
                .map(|name| tokens.intern(name));
        let mut base = PrimDefinition {
            type_name: Some(kind),
            ancestors: [kind, parent].into(),
            ..PrimDefinition::default()
        };
        for name in [a, b, c] {
            base.add_property(name, &PropertyDefinition::attribute(name), &tokens);
        }
        let mut flat = base.clone();
        let base = Arc::new(base);
        let mut shared = PrimDefinition::with_shared_base(Arc::clone(&base));
        assert!(core::ptr::eq(shared.properties(), base.properties()));
        assert!(core::ptr::eq(shared.ancestors(), base.ancestors()));
        assert!(shared.is_a(parent));

        // Overlap base slots out of their original order, and merge a new
        // property before and after another newly appended property.
        for property in [
            PropertyDefinition::attribute(c).with_fallback(Value::Int(3)),
            PropertyDefinition::attribute(d),
            PropertyDefinition::relationship(b),
            PropertyDefinition::attribute(a).with_fallback(Value::Int(1)),
            PropertyDefinition::attribute(e).with_fallback(Value::Int(5)),
            PropertyDefinition::attribute(d).with_fallback(Value::Int(4)),
            PropertyDefinition::attribute(a).with_metadata(group, Value::string("group")),
        ] {
            flat.add_property(property.name, &property, &tokens);
            shared.add_property(property.name, &property, &tokens);
        }
        assert_eq!(shared, flat);
        assert_eq!(
            shared.iter_properties().map(|p| p.name).collect::<Vec<_>>(),
            [a, b, c, d, e]
        );
        assert!(core::ptr::eq(
            shared.property(b).unwrap(),
            base.property(b).unwrap()
        ));
        assert_eq!(shared.property(d).unwrap().fallback, Some(Value::Int(4)));
        assert_eq!(base.property(a).unwrap().fallback, None);
        assert!(shared.base.as_ref().unwrap().flattened.get().is_none());

        assert_eq!(shared.properties(), flat.properties());
        assert!(core::ptr::eq(shared.properties(), shared.properties()));
        assert_eq!(shared, shared.clone());
        assert!(
            shared
                .clone()
                .base
                .as_ref()
                .unwrap()
                .flattened
                .get()
                .is_none()
        );
    }

    #[test]
    fn empty_base_keeps_new_properties_in_their_original_slice() {
        let tokens = TokenInterner::default();
        let added = tokens.intern("added");
        let mut definition = PrimDefinition::with_shared_base(Arc::default());
        assert!(definition.iter_properties().next().is_none());
        assert!(definition.properties().is_empty());
        definition.add_property(added, &PropertyDefinition::attribute(added), &tokens);
        assert!(core::ptr::eq(
            definition.properties(),
            definition.properties.as_slice()
        ));
        assert!(definition.base.as_ref().unwrap().flattened.get().is_none());
    }
}
