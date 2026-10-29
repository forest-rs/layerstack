// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Borrowed generic USD views and caller-owned retained attribute evaluation.
//! AOUSD Core §11–§13; OpenUSD `UsdPrim`, `UsdProperty`, `UsdAttribute`,
//! `UsdRelationship`, `UsdAttributeQuery`. Views borrow immutable snapshots;
//! queries never hide synchronization or keep a mutable stage borrowed
//! between evaluations.
use alloc::collections::BTreeMap;

use super::*;
use crate::{DynamicDependency, FromValue, NameArg};

/// A generic object in a composed snapshot.
#[derive(Clone, Copy, Debug)]
pub enum Object<'a> {
    /// A populated prim.
    Prim(Prim<'a>),
    /// An authored or schema-defined attribute.
    Attribute(Attribute<'a>),
    /// An authored or schema-defined relationship.
    Relationship(Relationship<'a>),
}
/// A composed prim of a stage, bound to the store the stage was composed
/// from: the handle every prim query goes through.
///
/// Get one from a [`StageView`] (`view.prim("/World")`), or from
/// [`Stage::prim`]. It is `Copy` and borrows the stage and store; names go
/// in as text or tokens ([`NameArg`]) and come out as text.
#[derive(Clone, Copy)]
#[doc(alias = "UsdPrim")]
pub struct Prim<'a> {
    stage: &'a Stage,
    store: &'a dyn LayerStore,
    path: PathId,
    /// The prim's index, found once when the handle is made, so the
    /// queries that read it look nothing up again.
    index: &'a PrimIndex,
}
impl core::fmt::Debug for Prim<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Prim")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
/// A composed property of a prim, attribute or relationship: what the two
/// have in common (its name, declaration, schema definition, metadata and
/// opinions).
///
/// [`Attribute`] and [`Relationship`] deref to it; [`Self::as_attribute`]
/// and [`Self::as_relationship`] go the other way. A property is
/// composed when an opinion declares it or a schema of its prim defines it.
#[derive(Clone, Copy)]
#[doc(alias = "UsdProperty")]
pub struct Property<'a> {
    stage: &'a Stage,
    store: &'a dyn LayerStore,
    path: PropertyPath,
}
impl core::fmt::Debug for Property<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Property")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
/// A composed attribute. Creation checks its composed property kind; the
/// queries every property has are [`Property`]'s, which it derefs to.
#[derive(Clone, Copy)]
#[doc(alias = "UsdAttribute")]
pub struct Attribute<'a> {
    property: Property<'a>,
}
impl core::fmt::Debug for Attribute<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Attribute")
            .field("path", &self.property.path)
            .finish_non_exhaustive()
    }
}
/// A composed relationship. Creation checks its composed property kind;
/// the queries every property has are [`Property`]'s, which it derefs to.
#[derive(Clone, Copy)]
#[doc(alias = "UsdRelationship")]
pub struct Relationship<'a> {
    property: Property<'a>,
}
impl core::fmt::Debug for Relationship<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Relationship")
            .field("path", &self.property.path)
            .finish_non_exhaustive()
    }
}
impl<'a> core::ops::Deref for Attribute<'a> {
    type Target = Property<'a>;
    fn deref(&self) -> &Property<'a> {
        &self.property
    }
}
impl<'a> core::ops::Deref for Relationship<'a> {
    type Target = Property<'a>;
    fn deref(&self) -> &Property<'a> {
        &self.property
    }
}
impl<'a> From<Attribute<'a>> for Property<'a> {
    fn from(attribute: Attribute<'a>) -> Self {
        attribute.property
    }
}
impl<'a> From<Relationship<'a>> for Property<'a> {
    fn from(relationship: Relationship<'a>) -> Self {
        relationship.property
    }
}
/// The prim's path (`/World/Arm`).
impl core::fmt::Display for Prim<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.store.paths().display(self.path, self.store.tokens()))
    }
}
/// The property's path (`/World/Arm.points`).
impl core::fmt::Display for Property<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.path.display(self.store.paths(), self.store.tokens()))
    }
}
/// The attribute's path (`/World/Arm.points`).
impl core::fmt::Display for Attribute<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.property.fmt(f)
    }
}
/// The relationship's path (`/World/Arm.material:binding`).
impl core::fmt::Display for Relationship<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.property.fmt(f)
    }
}
/// The object's path.
impl core::fmt::Display for Object<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Prim(prim) => prim.fmt(f),
            Self::Attribute(attribute) => attribute.fmt(f),
            Self::Relationship(relationship) => relationship.fmt(f),
        }
    }
}
impl Stage {
    /// Captured token/path domain affinity. Public composition always supplies
    /// this identity; internal synthetic snapshots may have no source store.
    pub fn store_identity(&self) -> Option<&crate::StoreIdentity> {
        self.store_identity.as_ref()
    }
    /// The composed prim at `path`, active or not; `None` when the stage
    /// composes no prim there. [`StageView::prim`] also takes text.
    #[doc(alias = "UsdStage::GetPrimAtPath")]
    #[doc(alias = "GetPrimAtPath")]
    #[inline]
    pub fn prim<'a>(&'a self, path: PathId, store: &'a dyn LayerStore) -> Option<Prim<'a>> {
        let index = self.prims.get(&path)?;
        Some(Prim {
            stage: self,
            store,
            path,
            index,
        })
    }
    /// The composed prim, attribute or relationship at `path`.
    #[doc(alias = "UsdStage::GetObjectAtPath")]
    #[doc(alias = "GetObjectAtPath")]
    pub fn object<'a>(&'a self, path: TargetPath, store: &'a dyn LayerStore) -> Option<Object<'a>> {
        match path {
            TargetPath::Prim(path) => self.prim(path, store).map(Object::Prim),
            TargetPath::Property(path) => self.property(path, store).map(Property::into_object),
        }
    }
    /// The composed property at `path`: one an opinion declares or a
    /// schema of its prim defines.
    #[inline]
    pub(crate) fn property<'a>(
        &'a self,
        path: PropertyPath,
        store: &'a dyn LayerStore,
    ) -> Option<Property<'a>> {
        self.property_kind(path).map(|_| Property {
            stage: self,
            store,
            path,
        })
    }
}
impl<'a> Prim<'a> {
    /// Concrete stage path.
    #[doc(alias = "UsdPrim::GetPath")]
    #[doc(alias = "GetPath")]
    #[inline]
    pub fn path(self) -> PathId {
        self.path
    }
    /// The prim's name (`Arm`); empty for the pseudo-root.
    #[doc(alias = "UsdPrim::GetName")]
    #[doc(alias = "GetName")]
    #[inline]
    pub fn name(self) -> &'a str {
        let path = self.store.paths().resolve(self.path);
        path.leaf()
            .map_or("", |name| self.store.tokens().resolve(name))
    }
    /// The parent prim; `None` for the pseudo-root.
    #[doc(alias = "UsdPrim::GetParent")]
    #[doc(alias = "GetParent")]
    pub fn parent(self) -> Option<Self> {
        let parent = self.store.paths().parent(self.path)?;
        self.stage.prim(parent, self.store)
    }
    /// The stage and store this prim belongs to, as a view.
    pub fn view(self) -> StageView<'a> {
        self.stage.view(self.store)
    }
    /// Composed status, captured by this view's immutable snapshot.
    pub fn status(self) -> PrimStatus {
        self.stage
            .prim_status(self.path, self.store)
            .expect("populated view")
    }
    /// Whether the prim is active. An inactive prim stays on the stage
    /// to be inspected, without its descendants.
    ///
    /// Spec: AOUSD Core §11.3.1.
    #[doc(alias = "UsdPrim::IsActive")]
    #[doc(alias = "IsActive")]
    #[inline]
    pub fn is_active(self) -> bool {
        self.stage.is_active(self.path)
    }
    /// Whether the prim is active and every payload on it and its
    /// ancestors is included by the stage's load rules.
    ///
    /// Spec: AOUSD Core §10.3.2.7, §11.3.
    #[doc(alias = "UsdPrim::IsLoaded")]
    #[doc(alias = "IsLoaded")]
    pub fn is_loaded(self) -> bool {
        self.stage.is_loaded(self.path, self.store.paths())
    }
    /// Whether the prim and all its ancestors resolve a defining specifier
    /// (`def` or `class`).
    ///
    /// Spec: AOUSD Core §11.5.
    #[doc(alias = "UsdPrim::IsDefined")]
    #[doc(alias = "IsDefined")]
    pub fn is_defined(self) -> bool {
        self.stage.is_defined(self.path, self.store)
    }
    /// Whether the prim or one of its ancestors resolves the specifier
    /// `class`.
    ///
    /// Spec: AOUSD Core §11.5.
    #[doc(alias = "UsdPrim::IsAbstract")]
    #[doc(alias = "IsAbstract")]
    pub fn is_abstract(self) -> bool {
        self.stage.is_abstract(self.path, self.store)
    }
    /// Whether the prim is a native instance root: its children are
    /// instance proxies of its prototype ([`Self::prototype`]).
    ///
    /// Spec: AOUSD Core §11.3.3 (scene-graph instancing).
    #[doc(alias = "UsdPrim::IsInstance")]
    #[doc(alias = "IsInstance")]
    #[inline]
    pub fn is_instance(self) -> bool {
        self.stage.is_instance(self.path)
    }
    /// Whether the prim is beneath a native instance root.
    #[doc(alias = "UsdPrim::IsInstanceProxy")]
    #[doc(alias = "IsInstanceProxy")]
    pub fn is_instance_proxy(self) -> bool {
        self.stage.is_instance_proxy(self.path, self.store.paths())
    }
    /// The prototype of an instance root; `None` for any other prim.
    #[doc(alias = "UsdPrim::GetPrototype")]
    #[doc(alias = "GetPrototype")]
    pub fn prototype(self) -> Option<PrototypeId> {
        self.stage.instance_prototype(self.path)
    }
    /// The resolved specifier: `def` when the strongest defining opinion
    /// is a `def`, `class` when it is a `class`, `over` when no opinion
    /// defines the prim. A `class` reached through an inherit of the prim
    /// itself is weaker than every other defining opinion. The pseudo-root
    /// is `def`.
    ///
    /// OpenUSD: `UsdPrim::GetSpecifier` (`_GetPrimSpecifierImpl`).
    ///
    /// Spec: AOUSD Core §12.2.1 (specifier resolution).
    #[doc(alias = "UsdPrim::GetSpecifier")]
    #[doc(alias = "GetSpecifier")]
    #[inline]
    pub fn specifier(self) -> Specifier {
        Stage::specifier_of(self.path, self.index, self.store)
    }
    /// Authored type name (`Xform`); fallback mapping does not change it.
    /// An empty type name and `__AnyType__` are no type name.
    ///
    /// Spec: AOUSD Core §7.6 (`typeName`), §12.2.3.
    #[doc(alias = "UsdPrim::GetTypeName")]
    #[doc(alias = "GetTypeName")]
    pub fn type_name(self) -> Option<&'a str> {
        let name = self.type_name_token()?;
        Some(self.store.tokens().resolve(name))
    }
    /// The token of [`Self::type_name`], for looking the type up in a
    /// [`SchemaRegistry`], which names schemas by token.
    #[inline]
    pub fn type_name_token(self) -> Option<TokenId> {
        self.index.type_info.as_ref()?.identity.type_name
    }
    /// Effective schema type, including `fallbackPrimTypes` mapping.
    pub fn schema_type_name(self) -> Option<&'a str> {
        let identity = &self.index.type_info.as_ref()?.identity;
        let name = identity.mapped_type_name.or(identity.type_name)?;
        Some(self.store.tokens().resolve(name))
    }
    /// The prim definition: the prim's typed schema (or none: it is
    /// typeless), its applied schemas in strength order, and the
    /// properties they define. Empty when the stage has no schemas
    /// ([`StageOptions::schemas`]). `None` only while a partial composition
    /// has not yet typed the prim.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use layerstack::{
    ///     InMemoryStore, Layer, LayerId, PrimSpec, PropertyDefinition, SchemaDefinition,
    ///     SchemaRegistry, Stage, StageOptions,
    /// };
    ///
    /// let mut store = InMemoryStore::default();
    /// let (tile, width) = (store.tokens().intern("Tile"), store.tokens().intern("width"));
    /// let floor = store.path("/Floor");
    /// let mut layer = Layer::new(LayerId(1));
    /// layer.insert_prim(floor, PrimSpec::def().with_type_name(tile));
    /// store.insert_layer(layer);
    ///
    /// let mut builder = SchemaRegistry::builder();
    /// builder.register(SchemaDefinition::typed(tile).with_property(
    ///     PropertyDefinition::attribute(width).with_fallback(1.0_f32),
    /// ));
    /// let options = StageOptions {
    ///     schemas: Some(Arc::new(builder.build(store.tokens()))),
    ///     ..StageOptions::default()
    /// };
    /// let stage = Stage::compose(&mut store, LayerId(1), options);
    ///
    /// let prim = stage.view(&store).prim("/Floor").unwrap();
    /// let definition = prim.definition().expect("typed at composition");
    /// assert!(definition.is_a(tile));
    /// assert!(definition.property(width).is_some());
    /// ```
    ///
    /// OpenUSD: `UsdPrim::GetPrimDefinition`.
    ///
    /// Spec: AOUSD Core §13.3.1 (typeless prims), §13.3.2.3 (the prim
    /// definition).
    #[doc(alias = "UsdPrim::GetPrimDefinition")]
    #[doc(alias = "GetPrimDefinition")]
    #[inline]
    pub fn definition(self) -> Option<&'a PrimDefinition> {
        Some(&self.index.type_info.as_ref()?.definition)
    }
    /// Whether the applied schema `schema`, with `instance` for a
    /// multiple-apply schema, may be applied to the prim, given its type and
    /// the applied schemas it has, and why not.
    ///
    /// # Errors
    ///
    /// [`CannotApply::NotAnAppliedSchema`] when the stage has no schemas or
    /// none of that name, and otherwise what [`SchemaRegistry::can_apply`]
    /// reports.
    ///
    /// OpenUSD: `UsdPrim::CanApplyAPI`.
    #[doc(alias = "UsdPrim::CanApplyAPI")]
    #[doc(alias = "CanApplyAPI")]
    pub fn can_apply(
        self,
        schema: impl NameArg,
        instance: Option<&str>,
    ) -> Result<(), CannotApply> {
        let schema = schema
            .token(self.store.tokens())
            .ok_or(CannotApply::NotAnAppliedSchema)?;
        self.stage
            .can_apply(self.path, schema, instance, self.store)
    }
    /// The composed prim metadata field `key` (`kind`, `customData`,
    /// `apiSchemas`): the strongest authored value, dictionaries combined,
    /// list ops chained. Never a property: a prim may author a field and a
    /// property of the same name, and the two never stand in for each
    /// other. `None` when no opinion authors the field.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution), §12.2.5, §12.2.6.
    #[doc(alias = "UsdObject::GetMetadata")]
    #[doc(alias = "GetMetadata")]
    #[inline]
    pub fn metadata(self, key: impl NameArg) -> Option<Resolved<ResolvedValue>> {
        let key = key.token(self.store.tokens())?;
        self.stage.resolve_value(self.path, key)
    }
    /// Why the metadata field `key` has its value (see
    /// [`ValueExplanation`]); `None` when no opinion authors it.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution), §12.2.5, §12.2.6.
    pub fn explain_metadata(
        self,
        key: impl NameArg,
    ) -> Option<ValueExplanation<'a, ResolvedValue>> {
        let key = key.token(self.store.tokens())?;
        let stage: &'a Stage = self.stage;
        stage.explain_value(self.path, key)
    }
    /// The opinions of the metadata field `key`, strongest first, each
    /// with its source ([`OpinionRef`]); empty when none authors it.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering), §12.
    pub fn metadata_opinions(self, key: impl NameArg) -> Opinions<'a> {
        let stage: &'a Stage = self.stage;
        key.token(self.store.tokens())
            .and_then(|key| stage.explain_field(self.path, key))
            .unwrap_or_default()
    }
    /// Generic attribute with this USD name, including schema-defined properties.
    #[doc(alias = "UsdPrim::GetAttribute")]
    #[doc(alias = "GetAttribute")]
    #[inline]
    pub fn attribute(self, name: impl NameArg) -> Option<Attribute<'a>> {
        self.property(name)?.as_attribute()
    }
    /// Generic relationship with this USD name.
    #[doc(alias = "UsdPrim::GetRelationship")]
    #[doc(alias = "GetRelationship")]
    #[inline]
    pub fn relationship(self, name: impl NameArg) -> Option<Relationship<'a>> {
        self.property(name)?.as_relationship()
    }
    /// The attribute or relationship with this USD name, one an opinion
    /// declares or a schema of the prim defines.
    #[doc(alias = "UsdPrim::GetProperty")]
    #[doc(alias = "GetProperty")]
    #[inline]
    pub fn property(self, name: impl NameArg) -> Option<Property<'a>> {
        let name = name.token(self.store.tokens())?;
        self.stage
            .property(PropertyPath::new(self.path, name), self.store)
    }
    /// The names of the prim's properties, attributes and relationships
    /// together: those any opinion authors and those its schemas define.
    ///
    /// They are in dictionary order (letters ignoring case, runs of digits
    /// by value), then the names the prim's `reorder properties` lists
    /// ([`Self::property_order`]) move to the front in its order, as
    /// OpenUSD's `UsdPrim::GetPropertyNames` orders them.
    ///
    /// Spec: AOUSD Core §7.3.3 (a prim's properties share one name space),
    /// §12 (the composed prim holds every property any opinion authors),
    /// §13.3.2.3 (and every property its prim definition defines).
    #[doc(alias = "UsdPrim::GetPropertyNames")]
    #[doc(alias = "GetPropertyNames")]
    pub fn property_names(self) -> Vec<&'a str> {
        let tokens = self.store.tokens();
        Stage::property_names_of(self.index, self.store)
            .into_iter()
            .map(|name| tokens.resolve(name))
            .collect()
    }
    /// The names of the properties some opinion of the prim authors, in
    /// the order of [`Self::property_names`], leaving out those only its
    /// schemas define.
    ///
    /// Spec: AOUSD Core §12 (the composed prim holds every property any
    /// opinion authors).
    #[doc(alias = "UsdPrim::GetAuthoredPropertyNames")]
    #[doc(alias = "GetAuthoredPropertyNames")]
    pub fn authored_property_names(self) -> Vec<&'a str> {
        let tokens = self.store.tokens();
        Stage::authored_property_names_of(self.index, self.store)
            .into_iter()
            .map(|name| tokens.resolve(name))
            .collect()
    }
    /// The prim's properties, attributes and relationships together, in
    /// the order of [`Self::property_names`].
    #[doc(alias = "UsdPrim::GetProperties")]
    #[doc(alias = "GetProperties")]
    pub fn properties(self) -> Vec<Property<'a>> {
        self.property_handles(Stage::property_names_of(self.index, self.store))
    }
    /// The properties some opinion of the prim authors, in the order of
    /// [`Self::property_names`], leaving out those only its schemas define.
    #[doc(alias = "UsdPrim::GetAuthoredProperties")]
    #[doc(alias = "GetAuthoredProperties")]
    pub fn authored_properties(self) -> Vec<Property<'a>> {
        self.property_handles(Stage::authored_property_names_of(self.index, self.store))
    }
    /// The authored properties inside `namespace`: those whose names start
    /// with it and a `:` (`material:binding` holds
    /// `material:binding:preview`, not `material:binding` itself), in the
    /// order of [`Self::property_names`]. The empty namespace holds every
    /// authored property. Only the names inside are sorted, so this is
    /// cheaper than filtering [`Self::authored_properties`].
    ///
    /// OpenUSD: `UsdPrim::GetAuthoredPropertiesInNamespace`.
    #[doc(alias = "UsdPrim::GetAuthoredPropertiesInNamespace")]
    #[doc(alias = "GetAuthoredPropertiesInNamespace")]
    pub fn authored_properties_in_namespace(self, namespace: &str) -> Vec<Property<'a>> {
        self.property_handles(Stage::authored_property_names_in(
            self.index, namespace, self.store,
        ))
    }
    /// Handles for names the prim composes: each names a property spec an
    /// opinion authors (property opinions are always specs) or one its
    /// definition defines, so none needs its kind checked again.
    fn property_handles(self, names: Vec<TokenId>) -> Vec<Property<'a>> {
        let (stage, store) = (self.stage, self.store);
        names
            .into_iter()
            .map(|name| {
                let path = PropertyPath::new(self.path, name);
                debug_assert!(stage.property_kind(path).is_some(), "a composed property");
                Property { stage, store, path }
            })
            .collect()
    }
    /// The strongest authored property ordering (`reorder properties`);
    /// `None` when no opinion authors one.
    ///
    /// Spec: AOUSD Core §7.6.2.2.2 (`propertyChildren`), §12.2 (strongest
    /// opinion).
    #[doc(alias = "UsdPrim::GetPropertyOrder")]
    #[doc(alias = "GetPropertyOrder")]
    pub fn property_order(self) -> Option<Vec<&'a str>> {
        let tokens = self.store.tokens();
        let order = self.stage.resolve_property_order(self.path, self.store)?;
        Some(order.into_iter().map(|name| tokens.resolve(name)).collect())
    }
    /// The active children, in composed order.
    ///
    /// Inactive children are left out ([`Self::all_children`] has them).
    /// No other flag filters them: unlike OpenUSD's `UsdPrim::GetChildren`,
    /// undefined and abstract children are included, and so are an
    /// instance's children, its instance proxies.
    /// [`Self::filtered_children`] applies a predicate as OpenUSD does.
    ///
    /// Spec: AOUSD Core §11 (stage population).
    pub fn children(self) -> impl DoubleEndedIterator<Item = Prim<'a>> + ExactSizeIterator + 'a {
        self.child_handles(self.stage.children_of(self.path))
    }
    /// The paths of [`Self::children`], in the same order, without making a
    /// handle for each: for walking the namespace by path.
    #[inline]
    pub fn child_paths(self) -> &'a [PathId] {
        let stage: &'a Stage = self.stage;
        stage.children_of(self.path).unwrap_or_default()
    }
    /// All the populated children in composed order, inactive ones
    /// included (their own descendants are not populated).
    #[doc(alias = "UsdPrim::GetAllChildren")]
    #[doc(alias = "GetAllChildren")]
    pub fn all_children(
        self,
    ) -> impl DoubleEndedIterator<Item = Prim<'a>> + ExactSizeIterator + 'a {
        self.child_handles(self.stage.all_children_of(self.path))
    }
    /// The children `predicate` admits, in composed order. An instance's
    /// children are instance proxies and need
    /// [`PrimPredicate::instance_proxies`], unless this prim is an instance
    /// proxy itself.
    ///
    /// OpenUSD: `UsdPrim::GetFilteredChildren`; `PrimPredicate::DEFAULT`
    /// gives `UsdPrim::GetChildren`.
    #[doc(alias = "UsdPrim::GetFilteredChildren")]
    #[doc(alias = "GetFilteredChildren")]
    pub fn filtered_children(
        self,
        predicate: PrimPredicate,
    ) -> impl Iterator<Item = Prim<'a>> + 'a {
        let predicate = PrimPredicate {
            instance_proxies: predicate.instance_proxies || self.is_instance_proxy(),
            ..predicate
        };
        self.all_children()
            .filter(move |child| predicate.matches(child.status()))
    }
    fn child_handles(
        self,
        children: Option<&'a [PathId]>,
    ) -> impl DoubleEndedIterator<Item = Prim<'a>> + ExactSizeIterator + 'a {
        let (stage, store) = (self.stage, self.store);
        children
            .unwrap_or_default()
            .iter()
            .map(move |&path| stage.prim(path, store).expect("a composed child"))
    }
    /// The prim's variant sets, each with the variant composition selected
    /// for it (`None` when nothing selects one): the sets each node of its
    /// prim index lists, nodes in strength order, each node's in its
    /// composed `variantSetNames` order, a set listed by several nodes
    /// once. A set listed only in a variant that is not selected is not
    /// one, nor is a variant set spec no list names. A selection may name
    /// a variant no layer defines.
    ///
    /// OpenUSD: `UsdVariantSets::GetNames`, with
    /// `UsdVariantSet::GetVariantSelection` for each set.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs), §10.5 (variant selection).
    #[doc(alias = "UsdPrim::GetVariantSets")]
    #[doc(alias = "GetVariantSets")]
    pub fn variant_sets(self) -> Vec<(&'a str, Option<&'a str>)> {
        let tokens = self.store.tokens();
        self.stage
            .variant_sets(self.path, self.store)
            .into_iter()
            .map(|(set, selection)| {
                (
                    tokens.resolve(set),
                    selection.map(|variant| tokens.resolve(variant)),
                )
            })
            .collect()
    }
    /// The names of the variants of the variant set `set` that the prim's
    /// composition defines: a variant any site of its prim index (any
    /// layer, any arc) authors for the set, sorted by name and each once.
    /// A selection may name a variant that is not among them. Empty for a
    /// set the prim has no variants of.
    ///
    /// OpenUSD: `UsdVariantSet::GetVariantNames` (over
    /// `PcpComposeSiteVariantSetOptions` for each node, as a sorted set).
    ///
    /// Spec: AOUSD Core §7.3.6 (variant set specs), §10.5 (variant selection).
    #[doc(alias = "UsdVariantSet::GetVariantNames")]
    #[doc(alias = "GetVariantNames")]
    pub fn variant_names(self, set: impl NameArg) -> Vec<&'a str> {
        let Some(set) = set.token(self.store.tokens()) else {
            return Vec::new();
        };
        self.stage.variant_names(self.path, set, self.store)
    }
    /// The variant selections that govern the prim, by variant set name:
    /// for each set, the strongest selection authored on any site of the
    /// prim's index, selections inside selected variants included, and for
    /// a declared set without one, the variant the stage's fallbacks
    /// select ([`StageOptions::variant_fallbacks`]). An authored selection
    /// is reported whether or not the variant it names exists.
    ///
    /// OpenUSD: `UsdVariantSet::GetVariantSelection`, which reports the
    /// variant composition selected, fallbacks included.
    ///
    /// Spec: AOUSD Core §10.5 (variant selection).
    #[doc(alias = "UsdVariantSets::GetAllVariantSelections")]
    #[doc(alias = "GetAllVariantSelections")]
    pub fn variant_selections(self) -> BTreeMap<&'a str, &'a str> {
        let tokens = self.store.tokens();
        self.stage
            .variant_selections(self.path, self.store)
            .into_iter()
            .map(|(set, variant)| (tokens.resolve(set), tokens.resolve(variant)))
            .collect()
    }
    /// The prim stack: each `(layer, spec)` site that contributes to the
    /// prim, strongest first, once, at its strongest position.
    /// [`Self::sources`] keeps the sites reached through several arcs.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering), §11.
    #[doc(alias = "UsdPrim::GetPrimStack")]
    #[doc(alias = "GetPrimStack")]
    pub fn prim_stack(self) -> Vec<(LayerId, SpecPath)> {
        self.stage.prim_stack(self.path).unwrap_or_default()
    }
    /// Every source that contributes to the prim, strongest first: one
    /// [`OpinionSource`] per contributing `(arc, layer, spec)` site, with
    /// the layer offset its opinions are read through, a site reached
    /// through two arcs (a reference diamond, or a layer that appears twice
    /// in a layer stack) twice, as OpenUSD's `PcpPrimIndex::GetPrimStack`
    /// lists them. An [`Opinion::source`] is a position in this list.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering), §12.3.2.1 (layer
    /// offsets).
    pub fn sources(self) -> &'a [OpinionSource] {
        let stage: &'a Stage = self.stage;
        stage.explain_prim(self.path).unwrap_or_default()
    }
    /// The composition graph of the prim: one node per arc expansion that
    /// contributes to it, with its arc kind, layer stack, site and parent.
    /// Every [`OpinionKey::node`] of the prim's opinions and sources names
    /// one of its nodes; see [`PrimIndexGraph`] for how it relates to
    /// strength order.
    ///
    /// An instance proxy's graph is the one its own path composes, with the
    /// prototype's nodes and, as the nodes that are not instanceable, its
    /// own instance's sites, which contribute nothing beneath an instance.
    /// OpenUSD's `UsdPrim::GetPrimIndex` reports the prim index of the
    /// prototype's source instance for every proxy instead, whichever
    /// instance that is: the two agree but for those inert nodes, and on
    /// every opinion and source.
    ///
    /// OpenUSD: `PcpPrimIndex::GetGraph`.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering), §11.3.3 (scene graph
    /// instancing).
    #[doc(alias = "UsdPrim::GetPrimIndex")]
    #[doc(alias = "GetPrimIndex")]
    pub fn composition_graph(self) -> &'a PrimIndexGraph {
        let stage: &'a Stage = self.stage;
        stage.explain_prim_graph(self.path).expect("populated view")
    }
    /// What the arguments of the payloads to dynamic file formats the
    /// prim's index follows were composed from: the fields and attributes
    /// read, and each format's dependency data. `None` when the index
    /// composes no dynamic arguments; the payloads of the prim's ancestors
    /// belong to their own indexes.
    ///
    /// See [`crate::dynamic_format`]. OpenUSD:
    /// `PcpCache::GetDynamicFileFormatArgumentDependencyData`.
    pub fn dynamic_dependency(self) -> Option<&'a DynamicDependency> {
        let stage: &'a Stage = self.stage;
        stage.dynamic_dependency(self.path)
    }
}
impl<'a> Property<'a> {
    /// Concrete property path.
    #[doc(alias = "UsdProperty::GetPath")]
    #[doc(alias = "GetPath")]
    #[inline]
    pub fn path(self) -> PropertyPath {
        self.path
    }
    /// The property's name (`inputs:intensity`).
    #[doc(alias = "UsdProperty::GetName")]
    #[doc(alias = "GetName")]
    #[inline]
    pub fn name(self) -> &'a str {
        self.store.tokens().resolve(self.path.property())
    }
    /// The prim the property belongs to.
    #[doc(alias = "UsdProperty::GetPrim")]
    #[doc(alias = "GetPrim")]
    #[inline]
    pub fn prim(self) -> Prim<'a> {
        self.stage
            .prim(self.path.prim_path(), self.store)
            .expect("a composed property's prim")
    }
    /// Attribute or relationship: what the prim's schemas define it as,
    /// else what its strongest opinion declares.
    #[inline]
    pub fn kind(self) -> PropertyKind {
        self.stage
            .property_kind(self.path)
            .expect("composed property")
    }
    /// The attribute this is, if it is one.
    #[inline]
    pub fn as_attribute(self) -> Option<Attribute<'a>> {
        (self.kind() == PropertyKind::Attribute).then_some(Attribute { property: self })
    }
    /// The relationship this is, if it is one.
    #[inline]
    pub fn as_relationship(self) -> Option<Relationship<'a>> {
        (self.kind() == PropertyKind::Relationship).then_some(Relationship { property: self })
    }
    fn into_object(self) -> Object<'a> {
        match self.kind() {
            PropertyKind::Attribute => Object::Attribute(Attribute { property: self }),
            PropertyKind::Relationship => Object::Relationship(Relationship { property: self }),
        }
    }
    /// Whether an opinion declares the property `custom`: one no schema of
    /// its prim is expected to define. `false` for a property only a
    /// schema defines.
    ///
    /// Spec: AOUSD Core §12.2.4 (`custom` is true if any opinion says so).
    #[doc(alias = "UsdProperty::IsCustom")]
    #[doc(alias = "IsCustom")]
    pub fn is_custom(self) -> bool {
        self.declaration().is_some_and(|declared| declared.custom)
    }
    /// How the opinions declare the property: its kind, type, variability
    /// and `custom` qualifier, without consulting a schema. `None` when no
    /// opinion declares it and only a schema defines it
    /// ([`Self::definition`]).
    ///
    /// - The kind and type come from the strongest opinion that authors
    ///   them.
    /// - `custom` is `true` if any opinion authors it.
    /// - Variability comes from the weakest opinion.
    ///
    /// Spec: AOUSD Core §12.2.2–§12.2.4.
    #[inline]
    pub fn declaration(self) -> Option<PropertyDeclaration> {
        self.stage
            .resolve_property_declaration(self.path.prim_path(), self.path.property())
    }
    /// How a schema of the prim defines the property: its kind, type,
    /// variability, fallback and metadata. `None` when no schema defines
    /// it.
    ///
    /// OpenUSD: `UsdPrimDefinition::GetPropertyDefinition`.
    ///
    /// Spec: AOUSD Core §13.3.2.3–§13.3.2.4.
    #[inline]
    pub fn definition(self) -> Option<&'a PropertyDefinition> {
        let stage: &'a Stage = self.stage;
        stage.property_definition_ref(self.path.prim_path(), self.path.property())
    }
    /// The composed property metadata field `key`, the schema's fallback
    /// included. Authored scalar values (empty tokens too) win; a block
    /// hides weaker opinions and the fallback. Dictionaries combine across
    /// opinions and the fallback; list ops chain. A schema-only value has
    /// no layer provenance.
    ///
    /// Spec: AOUSD Core §12.2.5–§12.2.6, §13.3.2.3–§13.3.2.4.
    #[doc(alias = "UsdObject::GetMetadata")]
    #[doc(alias = "GetMetadata")]
    #[inline]
    pub fn metadata(self, key: impl NameArg) -> Option<Resolved<ResolvedValue>> {
        let key = key.token(self.store.tokens())?;
        self.stage
            .resolve_property_metadata(self.path.prim_path(), self.path.property(), key)
    }
    /// Why the property metadata field `key` has its value (see
    /// [`ValueExplanation`]): the value [`Self::metadata`] reads, the
    /// schema's fallback included, with each opinion's part. A
    /// dictionary's entries name the opinion that supplied them
    /// ([`Contribution::Dictionary`]), so
    /// a host anchors an asset path among them at that opinion's layer, as
    /// OpenUSD resolves each entry against the layer that authors it.
    /// `None` when no opinion authors the field and the schema has no
    /// fallback for it.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution), §12.2.5, §9.4 (asset
    /// paths anchor at their layer).
    pub fn explain_metadata(
        self,
        key: impl NameArg,
    ) -> Option<ValueExplanation<'a, ResolvedValue>> {
        let key = key.token(self.store.tokens())?;
        let stage: &'a Stage = self.stage;
        stage.explain_property_metadata(self.path.prim_path(), self.path.property(), key)
    }
    /// The property metadata field `key` as opinions author it, without the
    /// schema's fallback. A block reads as `None`, hiding weaker values.
    ///
    /// Spec: AOUSD Core §12.2.5–§12.2.6.
    #[doc(alias = "UsdObject::HasAuthoredMetadata")]
    pub fn authored_metadata(self, key: impl NameArg) -> Option<Resolved<ResolvedValue>> {
        let key = key.token(self.store.tokens())?;
        self.stage.resolve_authored_property_metadata(
            self.path.prim_path(),
            self.path.property(),
            key,
        )
    }
    /// The property's opinions, strongest first, each with its source
    /// ([`OpinionRef`]): its layer, spec path, node and layer offset. Empty
    /// when only a schema defines it.
    ///
    /// OpenUSD: `UsdProperty::GetPropertyStackWithLayerOffsets`, as
    /// opinions.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering), §12, §12.3.2.1 (layer
    /// offsets).
    #[doc(alias = "UsdProperty::GetPropertyStack")]
    #[doc(alias = "GetPropertyStack")]
    #[doc(alias = "UsdProperty::GetPropertyStackWithLayerOffsets")]
    #[inline]
    pub fn opinions(self) -> Opinions<'a> {
        let stage: &'a Stage = self.stage;
        stage.explain_property_path(self.path).unwrap_or_default()
    }
    /// Whether an opinion declares the property: `false` when only a
    /// schema defines it.
    #[doc(alias = "UsdProperty::IsAuthored")]
    #[doc(alias = "IsAuthored")]
    #[inline]
    pub fn is_authored(self) -> bool {
        !self.opinions().is_empty()
    }
}
impl<'a> Attribute<'a> {
    /// The value type name (`float`, `color3f[]`): the type the prim's
    /// schemas define the attribute with, else the strongest type an
    /// opinion declares. `None` when neither declares one. It names the
    /// Rust type [`Self::value`] converts to: a `float` reads as `f32`.
    ///
    /// ```
    /// # use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage, StageOptions, Variability};
    /// # let mut store = InMemoryStore::default();
    /// # let (world, radius) = (store.path("/World"), store.tokens().intern("radius"));
    /// # let mut layer = Layer::new(LayerId(1));
    /// # layer.insert_prim(world, PrimSpec::def().with_property(radius,
    /// #     PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())));
    /// # store.insert_layer(layer);
    /// # let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let radius = stage.view(&store).attribute("/World.radius").unwrap();
    /// assert_eq!(radius.type_name(), Some("double"));
    /// assert_eq!(radius.variability(), Variability::Varying);
    /// ```
    ///
    /// OpenUSD: `UsdAttribute::GetTypeName`, which consults the prim
    /// definition before authored opinions (`UsdStage::_GetAttrTypeImpl`).
    ///
    /// Spec: AOUSD Core §7.6.4.1.1 (`typeName`), §12.2.2 (the strongest
    /// opinion), §13.3.2 (prim definitions).
    #[doc(alias = "UsdAttribute::GetTypeName")]
    #[doc(alias = "GetTypeName")]
    pub fn type_name(self) -> Option<&'a str> {
        let path = self.property.path;
        let (prim, name) = (path.prim_path(), path.property());
        let stage: &'a Stage = self.property.stage;
        stage
            .property_definition_ref(prim, name)
            .and_then(|defined| defined.type_name.as_ref())
            .or_else(|| stage.prims.get(&prim)?.property_type_for(&name))
            .map(PropertyType::type_name)
    }
    /// Whether the value may vary over time: the variability the prim's
    /// schemas define the attribute with, else the weakest opinion's.
    ///
    /// OpenUSD: `UsdAttribute::GetVariability`
    /// (`UsdStage::_GetAttrVariabilityImpl`).
    ///
    /// Spec: AOUSD Core §7.6.4.1.2 (`variability`), §12.2.3 (the weakest
    /// opinion decides).
    #[doc(alias = "UsdAttribute::GetVariability")]
    #[doc(alias = "GetVariability")]
    pub fn variability(self) -> Variability {
        if let Some(defined) = self.definition() {
            return defined.variability;
        }
        self.declaration()
            .map(|declared| declared.variability)
            .unwrap_or_default()
    }
    /// The composed value at `time`, with the source it came from when the
    /// stage records provenance ([`StageOptions::with_provenance`]). Numeric
    /// arrays stay retained until the caller materializes them.
    ///
    /// The schema's fallback answers when nothing authored answers at
    /// `time` (a spec without a value included) and when a block is in
    /// effect, default or sampled; an array fallback is the weakest dense
    /// seed that sparse array edits compose over. Otherwise authored values
    /// resolve as without a schema ([`Self::authored_value`]). OpenUSD
    /// 26.08 resolves no value after a block instead: the named divergences
    /// `default-time-block-hides-fallback` and `sampled-block-drops-fallback`
    /// (`docs/generic-sparse-composition.md`).
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §12.3.5 (fallback values),
    /// §12.3.6 (blocks), §13.3.2.4 (fallback value resolution).
    #[doc(alias = "UsdAttribute::Get")]
    #[doc(alias = "Get")]
    #[inline]
    pub fn get(self, time: Time) -> Option<Resolved<Value>> {
        self.property
            .stage
            .read_property(self.property.path, time, |v| Some(v.clone()))
    }
    /// The value at `time` as a `T`, schema fallback included. At the
    /// default time this is the strongest default that converts
    /// ([`FromValue`]); at a time code, the strongest source's value if it
    /// converts. `None` when nothing converts, the value is blocked, or
    /// `time`'s samples hold none.
    ///
    /// ```
    /// # use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage, StageOptions, Time, Value};
    /// # let mut store = InMemoryStore::default();
    /// # let (world, radius) = (store.path("/World"), store.tokens().intern("radius"));
    /// # let double = PropertyType::parse("double").unwrap();
    /// # let mut layer = Layer::new(LayerId(1));
    /// # layer.insert_prim(world, PrimSpec::def().with_property(radius,
    /// #     PropertySpec::typed_attribute(double).with_default(Value::Double(2.5))));
    /// # store.insert_layer(layer);
    /// # let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let attribute = stage.view(&store).attribute("/World.radius").unwrap();
    /// assert_eq!(attribute.value::<f64>(Time::Default), Some(2.5));
    /// assert_eq!(attribute.value::<f32>(Time::Default), None);
    /// ```
    ///
    /// OpenUSD: `UsdAttribute::Get<T>`.
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §13.3.2.4 (fallbacks).
    #[inline]
    pub fn value<T: FromValue>(self, time: Time) -> Option<T> {
        let tokens = self.property.store.tokens();
        self.read(time, |v| T::from_value(v, tokens))
            .map(|resolved| resolved.value)
    }
    /// The value at `time` converted by `convert`, schema fallback
    /// included, as [`Self::value`] converts to a [`FromValue`] type, with
    /// the winning source when the stage records provenance
    /// ([`StageOptions::with_provenance`]).
    ///
    /// At [`Time::Default`] a default `convert` rejects is skipped for a
    /// weaker one; at a time code the source is selected first, and a
    /// value it rejects reads as nothing. `convert` should reject only
    /// values of another type: check shape or range afterwards, so
    /// malformed data never turns into a weaker value. Sparse array edits
    /// may also call it on candidate bases.
    ///
    /// OpenUSD: `UsdAttribute::Get<T>`.
    ///
    /// Spec: AOUSD Core §12.3 (values), §12.3.2.1 (layer offsets),
    /// §13.3.2.4 (schema fallbacks).
    #[inline]
    pub fn read<T>(self, time: Time, convert: impl Fn(&Value) -> Option<T>) -> Option<Resolved<T>> {
        self.property
            .stage
            .read_property(self.property.path, time, convert)
    }
    /// [`Self::read`], always with the winning authored source, whatever
    /// the stage's provenance policy; a schema fallback still has none.
    /// For asset paths, which resolve against the layer that authors them.
    ///
    /// Spec: AOUSD Core §9.4 (asset paths), §12.3, §13.3.2.4.
    pub fn read_with_provenance<T>(
        self,
        time: Time,
        convert: impl Fn(&Value) -> Option<T>,
    ) -> Option<Resolved<T>> {
        self.property
            .stage
            .read_property_with_provenance(self.property.path, time, convert)
    }
    /// [`Self::read`], materializing the selected value: a retained or
    /// deferred array that does not decode is an error, never an absent
    /// value or a weaker one.
    ///
    /// # Errors
    ///
    /// The [`ArrayReadError`](crate::ArrayReadError) of the selected
    /// value, or of a schema fallback a sparse fold read.
    pub fn try_read<T>(
        self,
        time: Time,
        convert: impl Fn(&Value) -> Option<T>,
    ) -> Result<Option<Resolved<T>>, crate::ArrayReadError> {
        self.property
            .stage
            .try_read_property(self.property.path, time, convert)
    }
    /// The value at `time` beneath `layer`, as a `T`, with the source it
    /// came from: what the attribute would read without `layer`'s opinions
    /// and those stronger than them.
    ///
    /// The cut is in the composed opinion stack, so the arcs `layer`
    /// authors still shape it: opinions a reference, payload, inherit or
    /// sublayer of `layer` brings in are weaker than `layer`'s own and
    /// count as beneath it. When `layer` authors no opinion here and
    /// belongs to the stage's own layer stack, the cut is beneath the
    /// stack's layers stronger than it. The schema fallback answers when
    /// nothing beneath holds a value.
    ///
    /// Spec: AOUSD Core §12.3 (value resolution in strength order).
    pub fn value_below<T: FromValue>(self, layer: LayerId, time: Time) -> Option<Resolved<T>> {
        let tokens = self.property.store.tokens();
        self.property
            .stage
            .read_property_below(self.property.path, layer, time, |v| {
                T::from_value(v, tokens)
            })
    }
    /// The value opinions author at `time`, without the schema fallback:
    /// at [`Time::Default`] the strongest authored default (sparse array
    /// edits composed over it), at a time code the time samples, spline or
    /// default that answer it. `None` when nothing authored answers, or a
    /// block is in effect.
    ///
    /// Spec: AOUSD Core §12.3.1 (default values), §12.3.2 (time samples,
    /// then splines, then the default), §12.3.6 (blocks).
    pub fn authored_value(self, time: Time) -> Option<Resolved<Value>> {
        self.property
            .stage
            .resolve_property_path_at_time(self.property.path, time)
    }
    /// Reads and materializes the selected numeric array, exposing decode failure.
    ///
    /// # Errors
    ///
    /// The [`ArrayReadError`](crate::ArrayReadError) of a selected value
    /// that does not decode.
    #[doc(alias = "UsdAttribute::Get")]
    #[doc(alias = "Get")]
    pub fn try_get(self, time: Time) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
        checked_read(self.property.stage, self.property.path, time)
    }
    /// Raw composed attribute connections, independent of its value.
    #[doc(alias = "UsdAttribute::GetConnections")]
    #[doc(alias = "GetConnections")]
    pub fn connections(self) -> Vec<TargetPath> {
        self.property
            .stage
            .resolve_target_list_path(self.property.path)
            .map(|r| r.value)
            .unwrap_or_default()
    }
    /// Why the attribute has its value at `time`: the value [`Self::get`]
    /// reads, schema fallback included, with every opinion resolution
    /// consulted and the part it played (see [`ValueExplanation`]).
    /// `None` when no opinion authors a value and the schema has no
    /// fallback.
    ///
    /// ```
    /// # use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage, StageOptions, Time, Value, ValueSource};
    /// # let mut store = InMemoryStore::default();
    /// # let (world, radius) = (store.path("/World"), store.tokens().intern("radius"));
    /// # let double = PropertyType::parse("double").unwrap();
    /// # let mut layer = Layer::new(LayerId(1));
    /// # layer.insert_prim(world, PrimSpec::def().with_property(radius,
    /// #     PropertySpec::typed_attribute(double).with_default(Value::Double(2.5))
    /// #         .with_time_samples(vec![(1.0, Value::Double(1.0)), (3.0, Value::Double(3.0))])));
    /// # store.insert_layer(layer);
    /// # let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let radius = stage.view(&store).attribute("/World.radius").unwrap();
    /// let at_two = radius.explain(Time::at(2.0)).unwrap();
    /// assert_eq!(at_two.value, Some(Value::Double(2.0)));
    /// assert!(matches!(at_two.source, ValueSource::TimeSamples { lower: 1.0, upper: 3.0, .. }));
    /// let default = radius.explain(Time::Default).unwrap();
    /// assert_eq!(default.value, Some(Value::Double(2.5)));
    /// assert_eq!(default.contributors().count(), 1);
    /// ```
    ///
    /// OpenUSD: `UsdAttribute::GetResolveInfo`, which names the source
    /// alone.
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §13.3.2.4 (fallbacks).
    #[doc(alias = "UsdAttribute::GetResolveInfo")]
    #[doc(alias = "GetResolveInfo")]
    pub fn explain(self, time: Time) -> Option<ValueExplanation<'a, Value>> {
        let path = self.property.path;
        let stage: &'a Stage = self.property.stage;
        stage.explain_value_with_schema(path.prim_path(), path.property(), time)
    }
    /// Why the connections are what they are: each opinion's paths, and
    /// those composition dropped, with why (see [`TargetsExplanation`]).
    pub fn explain_connections(self) -> Option<TargetsExplanation> {
        self.property
            .stage
            .explain_targets(self.property.path, self.property.store)
    }
    /// Caller-owned retained query for this concrete attribute path.
    pub fn query(self) -> AttributeQuery {
        AttributeQuery::new(self.property.path)
    }
    /// Distinct composed sample times in stage time, including value clips.
    /// Dense defaults and blocks mask weaker animation; sparse array edits
    /// keep weaker grids. Clip times include activations and mapping
    /// discontinuities; splines have no discrete sample times.
    ///
    /// Spec: AOUSD Core §12.3.2 (effective stage-time samples).
    #[doc(alias = "UsdAttribute::GetTimeSamples")]
    #[doc(alias = "GetTimeSamples")]
    pub fn sample_times(self) -> Vec<f64> {
        let path = self.property.path;
        self.property
            .stage
            .property_sample_times(path.prim_path(), path.property())
    }
    /// Effective sample times within open, closed or unbounded stage-time bounds.
    /// Filters the composed grid; does not add evaluation times or brackets.
    /// NaN bounds and reversed intervals select no samples.
    #[doc(alias = "UsdAttribute::GetTimeSamplesInInterval")]
    #[doc(alias = "GetTimeSamplesInInterval")]
    pub fn sample_times_in_range(self, interval: impl core::ops::RangeBounds<f64>) -> Vec<f64> {
        let path = self.property.path;
        self.property.stage.property_sample_times_in_range(
            path.prim_path(),
            path.property(),
            interval,
        )
    }
    /// Conservative whether a numeric-time query can vary with time: a
    /// contributing source with several samples, or a spline, counts as
    /// varying even when its values agree. When the strongest value is a
    /// sparse edit composing over weaker sources it is
    /// [`Self::value_source_might_be_time_varying`], as in OpenUSD, so a
    /// sparse edit over a single weaker time sample might vary.
    ///
    /// Spec: AOUSD Core §12.3.2 (value sources).
    #[doc(alias = "UsdAttribute::ValueMightBeTimeVarying")]
    #[doc(alias = "ValueMightBeTimeVarying")]
    pub fn might_be_time_varying(self) -> bool {
        let path = self.property.path;
        self.property
            .stage
            .property_might_be_time_varying(path.prim_path(), path.property())
    }
    /// Conservative whether the attribute's value source might vary with
    /// time: time samples (even one), a spline or value clips supply its
    /// value, directly or under sparse edits that compose over them. Unlike
    /// [`Self::might_be_time_varying`] it is `true` whenever a numeric-time
    /// read and a [`Time::Default`] read may differ, such as for a single
    /// time sample; `false` means the value is certainly the same at every
    /// time, the default time included.
    ///
    /// Spec: AOUSD Core §12.3.2, §12.3.4 (value clips).
    #[doc(alias = "UsdAttributeQuery::ValueSourceMightBeTimeVarying")]
    #[doc(alias = "UsdResolveInfo::ValueSourceMightBeTimeVarying")]
    #[doc(alias = "ValueSourceMightBeTimeVarying")]
    pub fn value_source_might_be_time_varying(self) -> bool {
        let path = self.property.path;
        self.property
            .stage
            .property_value_source_might_be_time_varying(path.prim_path(), path.property())
    }
    /// The value clip that supplies the value at `time`, with its mapped
    /// bracketing samples; `None` when source selection ends before clips,
    /// no manifest makes the attribute eligible, or at [`Time::Default`]
    /// (clips hold time samples only). Under held interpolation a stronger
    /// dense lower sample may supply the whole result even when the chain
    /// reaches a clip; explanations name actual contributions.
    ///
    /// Spec: AOUSD Core §12.3, §12.5 (value clips).
    pub fn clip_source(self, time: Time) -> Option<crate::value_clips::ClipValueSource> {
        let path = self.property.path;
        self.property
            .stage
            .property_clip_source(path.prim_path(), path.property(), time)
    }
    /// The error the selected clip's evaluator met at `time`, without
    /// exposing weaker values: an invalid query time, or an evaluation
    /// failure. Preparation errors are [`Stage::clip_issues`]. Loads and
    /// changes nothing; `None` at [`Time::Default`].
    pub fn clip_evaluation_error(self, time: Time) -> Option<crate::value_clips::ClipEvalError> {
        let path = self.property.path;
        self.property
            .stage
            .property_clip_evaluation_error(path.prim_path(), path.property(), time)
    }
}
impl Relationship<'_> {
    /// Ordered composed targets without forwarding.
    #[doc(alias = "UsdRelationship::GetTargets")]
    #[doc(alias = "GetTargets")]
    pub fn targets(self) -> Vec<TargetPath> {
        self.property
            .stage
            .resolve_target_list_path(self.property.path)
            .map(|r| r.value)
            .unwrap_or_default()
    }
    /// Why the targets are what they are: each opinion's paths, and those
    /// composition dropped, with why (see [`TargetsExplanation`]).
    pub fn explain_targets(self) -> Option<TargetsExplanation> {
        self.property
            .stage
            .explain_targets(self.property.path, self.property.store)
    }
    /// Ordered terminal targets after recursively forwarding relationships.
    /// A cycle contributes no terminal target along its cyclic edge;
    /// missing prim and attribute targets stay terminal, as in OpenUSD.
    #[doc(alias = "UsdRelationship::GetForwardedTargets")]
    #[doc(alias = "GetForwardedTargets")]
    pub fn forwarded_targets(self) -> Vec<TargetPath> {
        self.property
            .stage
            .forwarded_relationship_targets(self.property.path)
    }
}
/// Work performed by one retained query, independent of stage counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AttributeQueryWork {
    /// Uncached evaluations, including absence, failures and schema fallbacks.
    pub evaluations: u64,
    /// Same-time evaluations reused from an unchanged snapshot identity.
    pub cache_hits: u64,
}

/// Retained composition identity of one prim, including absence and schema data.
///
/// Comparing this evidence never resolves values or scans geometry. A change to
/// any opinion on this prim invalidates it conservatively. Use store-local paths
/// only with their original store's interners. This is neither authored identity
/// nor geometry-content identity. AOUSD Core §10 (composition), §13.3 (schemas).
#[derive(Clone, Debug)]
pub struct PrimSnapshot {
    store_identity: Option<crate::StoreIdentity>,
    path: PathId,
    index: Option<Arc<crate::prim_index::PrimIndexData>>,
    type_info: Option<Arc<PrimTypeInfo>>,
}
impl Stage {
    /// Retains the immutable records currently describing this prim or absence.
    pub fn prim_snapshot(&self, path: PathId) -> PrimSnapshot {
        let current = self.prims.get(&path);
        PrimSnapshot {
            store_identity: self.store_identity.clone(),
            path,
            index: current.map(|i| i.data.clone()),
            type_info: current.and_then(|i| i.type_info.clone()),
        }
    }
}
impl PrimSnapshot {
    /// Whether this current stage still has the captured records and schema data.
    pub fn is_current(&self, stage: &Stage) -> bool {
        let current = stage.prims.get(&self.path);
        self.store_identity.as_ref() == stage.store_identity()
            && same_arc(self.index.as_ref(), current.map(|i| &i.data))
            && same_arc(
                self.type_info.as_ref(),
                current.and_then(|i| i.type_info.as_ref()),
            )
    }
}
#[derive(Clone, Debug)]
struct QueryStamp {
    store_identity: Option<crate::StoreIdentity>,
    index: Option<Arc<crate::prim_index::PrimIndexData>>,
    type_info: Option<Arc<PrimTypeInfo>>,
    clips: crate::value_clips::ClipQueryIdentity,
    provenance: bool,
}
impl QueryStamp {
    fn matches(&self, stage: &Stage, path: PropertyPath) -> bool {
        let current = stage.prims.get(&path.prim_path());
        self.store_identity.as_ref() == stage.store_identity()
            && same_arc(self.index.as_ref(), current.map(|i| &i.data))
            && same_arc(
                self.type_info.as_ref(),
                current.and_then(|i| i.type_info.as_ref()),
            )
            && self.provenance == stage.with_provenance
            && stage
                .clips
                .query_identity_matches(&self.clips, path.prim_path(), path.property())
    }
}
fn same_arc<T>(a: Option<&Arc<T>>, b: Option<&Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
#[derive(Clone, Debug)]
struct CachedQuery {
    time: Time,
    checked: bool,
    value: Result<Option<Resolved<Value>>, crate::ArrayReadError>,
}
/// Caller-owned same-time attribute cache, refreshed by immutable snapshot
/// identity on every read. No change notice can be missed: even a replacement
/// stage invalidates the cached value. Store-local paths must stay in one store.
/// Unrelated prim edits preserve hits; changes to this prim or prepared clips
/// invalidate conservatively. Memory is bounded to one result and its sources.
#[derive(Clone, Debug)]
#[doc(alias = "UsdAttributeQuery")]
pub struct AttributeQuery {
    path: PropertyPath,
    cached: Option<CachedQuery>,
    stamp: Option<QueryStamp>,
    work: AttributeQueryWork,
}
impl AttributeQuery {
    /// Retains a concrete path without borrowing a stage across edits.
    pub fn new(path: PropertyPath) -> Self {
        Self {
            path,
            cached: None,
            stamp: None,
            work: AttributeQueryWork::default(),
        }
    }
    /// Queried concrete attribute path.
    #[inline]
    pub fn path(&self) -> PropertyPath {
        self.path
    }
    /// Whether a checked read at this time can reuse its captured snapshot.
    /// This checks immutable identities, including clip dependencies, without
    /// resolving an attribute or materializing its numeric storage.
    pub fn is_current(&self, stage: &Stage, time: Time) -> bool {
        self.stamp
            .as_ref()
            .is_some_and(|s| s.matches(stage, self.path))
            && self
                .cached
                .as_ref()
                .is_some_and(|c| c.time == time && c.checked)
    }
    /// Evaluates against the supplied current snapshot, refreshing as needed.
    /// The caller synchronizes `LiveStage` before passing its snapshot.
    #[doc(alias = "UsdAttributeQuery::Get")]
    #[doc(alias = "Get")]
    #[inline]
    pub fn get(&mut self, stage: &Stage, time: Time) -> Option<Resolved<Value>> {
        self.evaluate(stage, time, false).ok().flatten()
    }
    /// Evaluates and materializes the selected numeric array with typed failure.
    /// Repeated reads of the same mode/time/snapshot reuse failures as well as values.
    #[doc(alias = "UsdAttributeQuery::Get")]
    #[doc(alias = "Get")]
    pub fn try_get(
        &mut self,
        stage: &Stage,
        time: Time,
    ) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
        self.evaluate(stage, time, true)
    }
    fn evaluate(
        &mut self,
        stage: &Stage,
        time: Time,
        checked: bool,
    ) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
        if self
            .stamp
            .as_ref()
            .is_some_and(|s| s.matches(stage, self.path))
            && let Some(cached) = &self.cached
            && cached.time == time
            && cached.checked == checked
        {
            self.work.cache_hits = self.work.cache_hits.saturating_add(1);
            return cached.value.clone();
        }
        let current = stage.prims.get(&self.path.prim_path());
        self.stamp = Some(QueryStamp {
            store_identity: stage.store_identity.clone(),
            index: current.map(|i| i.data.clone()),
            type_info: current.and_then(|i| i.type_info.clone()),
            clips: stage
                .clips
                .query_identity(self.path.prim_path(), self.path.property()),
            provenance: stage.with_provenance,
        });
        let value = if stage.property_kind(self.path) != Some(PropertyKind::Attribute) {
            Ok(None)
        } else if checked {
            checked_read(stage, self.path, time)
        } else {
            Ok(stage.read_property(self.path, time, |v| Some(v.clone())))
        };
        self.cached = Some(CachedQuery {
            time,
            checked,
            value: value.clone(),
        });
        self.work.evaluations = self.work.evaluations.saturating_add(1);
        value
    }
    /// Drops retained source/value state while preserving cumulative counters.
    pub fn clear(&mut self) {
        self.cached = None;
        self.stamp = None;
    }
    /// Cumulative query work for measuring reuse.
    pub fn work(&self) -> AttributeQueryWork {
        self.work
    }
}
fn materialize(
    mut value: Option<Resolved<Value>>,
) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
    if let Some(Resolved { value, .. }) = &mut value {
        materialize_selected(value)?;
    }
    Ok(value)
}

pub(super) fn check_decode(
    stage: &Stage,
    path: PropertyPath,
    time: Time,
) -> Result<(), crate::ArrayReadError> {
    stage.try_resolve_property_path_at_time(path, time)?;
    Ok(())
}
fn checked_read(
    stage: &Stage,
    path: PropertyPath,
    time: Time,
) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
    check_decode(stage, path, time)?;
    materialize(stage.read_property(path, time, |v| Some(v.clone())))
}
