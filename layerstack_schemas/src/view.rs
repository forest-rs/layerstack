// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What every schema view reads and every schema edit handle writes.
//!
//! A view reads a composed prim through a [`Scene`]: the [`Stage`] and the
//! store it was composed from, which together resolve values (schema
//! fallbacks included) and name tokens. Views of typed schemas deref to the
//! view of the schema they inherit from, down to [`PrimView`]; views of API
//! schemas deref to [`PrimView`] directly.
//!
//! Default-time getters skip incompatible dense opinions and read the first
//! compatible value or schema fallback, as OpenUSD's typed `Get<T>` does.
//! Numeric-time getters convert the selected source without retrying weaker
//! opinions. They return `None` when no compatible value resolves. For
//! anything a view does not offer, use the generic handle
//! ([`PrimView::prim`]) and the property's USD name:
//! [`Attribute::get`](layerstack::Attribute::get) returns the raw
//! `Resolved<Value>` with its provenance, and
//! [`Attribute::explain`](layerstack::Attribute::explain) says why.

use core::fmt;

use alloc::{string::String, vec::Vec};

use layerstack::{
    LayerStore, ListPosition, PathId, PrimPathArg, PropertyPath, Stage, StageView, TargetPath,
    Time, TokenId, TokenInterner, Value,
};

use crate::edit::SchemaEdit;
use crate::kind::KindRegistry;

/// A composed stage and the store it was composed from: what every schema
/// view reads.
///
/// The stage must be composed with this crate's schemas
/// ([`crate::openusd`] or [`crate::registry`], as
/// `StageOptions::schemas`); without schemas no prim is of any schema.
#[derive(Clone, Copy)]
pub struct Scene<'a> {
    stage: &'a Stage,
    store: &'a dyn LayerStore,
    kinds: &'a KindRegistry,
}

impl fmt::Debug for Scene<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scene").finish_non_exhaustive()
    }
}

impl<'a> Scene<'a> {
    /// The scene of `stage`, composed from `store`, with OpenUSD's kinds
    /// ([`KindRegistry::openusd`]).
    #[must_use]
    pub fn new(stage: &'a Stage, store: &'a dyn LayerStore) -> Self {
        Self {
            stage,
            store,
            kinds: KindRegistry::openusd(),
        }
    }

    /// The scene with the kinds `kinds` instead.
    #[must_use]
    pub fn with_kinds(self, kinds: &'a KindRegistry) -> Self {
        Self { kinds, ..self }
    }

    /// The kinds the scene reads `kind` metadata with.
    #[must_use]
    pub fn kinds(&self) -> &'a KindRegistry {
        self.kinds
    }

    /// The prim's composed `kind` metadata, if authored and not empty.
    ///
    /// Spec: AOUSD Core §7.6.2.4.4.
    #[must_use]
    pub fn kind(&self, path: PathId) -> Option<&'a str> {
        let resolved = self.prim(path)?.metadata("kind")?.into_value()?;
        let tokens = self.store.tokens();
        let kind = match resolved.value {
            Value::Token(token) => tokens.resolve(token),
            Value::String(kind) => tokens.resolve(tokens.lookup(&kind)?),
            _ => return None,
        };
        (!kind.is_empty()).then_some(kind)
    }

    /// The prim's place in the model hierarchy: whether it is a group
    /// (`group`, `assembly`) and whether it is a model (a group, or
    /// `component` or `model`), each only under a group. The pseudo-root is
    /// both.
    fn model_flags(&self, path: PathId) -> (bool, bool) {
        let Some(parent) = self.parent(path) else {
            return (true, true);
        };
        // AOUSD Core §11.4–11.5: every proper ancestor below the
        // pseudo-root must be a group. Walk iteratively for deep namespaces.
        let mut at = parent;
        while let Some(above) = self.parent(at) {
            if !self
                .kind(at)
                .is_some_and(|kind| self.kinds.is_a(kind, crate::kind::GROUP))
            {
                return (false, false);
            }
            at = above;
        }
        let Some(kind) = self.kind(path) else {
            return (false, false);
        };
        let kinds = self.kinds;
        let group = kinds.is_a(kind, crate::kind::GROUP);
        let model = group
            || kinds.is_a(kind, crate::kind::COMPONENT)
            || kinds.is_a(kind, crate::kind::MODEL);
        (group, model)
    }

    /// Whether the prim is a model: its kind is a kind of `model` and its
    /// parent is a group.
    ///
    /// OpenUSD: `UsdPrim::IsModel` (`Usd_PrimData::_ComposeAndCacheFlags`).
    ///
    /// Spec: AOUSD Core §11.4, §11.5 (model hierarchy).
    #[must_use]
    pub fn is_model(&self, path: PathId) -> bool {
        self.model_flags(path).1
    }

    /// Whether the prim is a group: its kind is a kind of `group` and its
    /// parent is a group.
    ///
    /// OpenUSD: `UsdPrim::IsGroup`.
    ///
    /// Spec: AOUSD Core §11.4.
    #[must_use]
    pub fn is_group(&self, path: PathId) -> bool {
        self.model_flags(path).0
    }

    /// The composed stage.
    #[must_use]
    pub fn stage(&self) -> &'a Stage {
        self.stage
    }

    /// The scene's stage and store as a [`StageView`], whose generic
    /// handles explain values and report types
    /// ([`Attribute::explain`](layerstack::Attribute::explain),
    /// [`Attribute::type_name`](layerstack::Attribute::type_name)).
    #[must_use]
    pub fn view(&self) -> StageView<'a> {
        self.stage.view(self.store)
    }

    /// The interned prim path `path` names: text (`"/World/Rock"`) or an
    /// id, as a [`StageView`] query takes it. `None` for text that is not a
    /// well-formed absolute prim path, or names no interned path. Looking a
    /// path up never interns it.
    #[must_use]
    pub fn prim_path(&self, path: impl PrimPathArg) -> Option<PathId> {
        path.prim_path(self.store.paths(), self.store.tokens())
    }

    /// The store the stage was composed from.
    #[must_use]
    pub fn store(&self) -> &'a dyn LayerStore {
        self.store
    }

    fn token(&self, name: &str) -> Option<TokenId> {
        self.store.tokens().lookup(name)
    }

    /// Whether the prim at `path` is of the typed schema `schema` or a
    /// schema derived from it (`IsA`).
    ///
    /// Spec: AOUSD Core §13.3.1.
    #[must_use]
    pub fn is_a(&self, path: PathId, schema: &str) -> bool {
        self.token(schema).is_some_and(|schema| {
            self.definition(path)
                .is_some_and(|definition| definition.is_a(schema))
        })
    }

    /// Whether the applied schema `schema` is applied to the prim at
    /// `path`, with `instance` for a multiple-apply schema (`HasAPI`).
    ///
    /// Spec: AOUSD Core §13.3.2.
    #[must_use]
    pub fn has_api(&self, path: PathId, schema: &str, instance: Option<&str>) -> bool {
        let Some(schema) = self.token(schema) else {
            return false;
        };
        let Some(definition) = self.definition(path) else {
            return false;
        };
        match instance {
            None => definition.has_api(schema),
            Some(instance) => self
                .token(instance)
                .is_some_and(|instance| definition.has_api_instance(schema, instance)),
        }
    }

    /// The instance names with which the multiple-apply schema `schema` is
    /// applied to the prim at `path`, in the order the prim's definition
    /// applies them.
    #[must_use]
    pub fn instances(&self, path: PathId, schema: &str) -> Vec<&'a str> {
        let (Some(schema), Some(definition)) = (self.token(schema), self.definition(path)) else {
            return Vec::new();
        };
        let tokens = self.store.tokens();
        definition
            .applied_schemas()
            .iter()
            .filter(|applied| applied.schema == schema)
            .filter_map(|applied| applied.instance.map(|instance| tokens.resolve(instance)))
            .collect()
    }

    /// The composed prim at `path`.
    pub(crate) fn prim(&self, path: PathId) -> Option<layerstack::Prim<'a>> {
        self.stage.prim(path, self.store)
    }

    /// The prim definition of the composed prim at `path`.
    pub(crate) fn definition(&self, path: PathId) -> Option<&'a layerstack::PrimDefinition> {
        self.prim(path)?.definition()
    }

    /// The composed property at `path`, attribute or relationship.
    pub(crate) fn property(&self, path: PropertyPath) -> Option<layerstack::Property<'a>> {
        self.view().property(path)
    }

    /// The composed attribute at `path`.
    pub(crate) fn attribute(&self, path: PropertyPath) -> Option<layerstack::Attribute<'a>> {
        self.view().attribute(path)
    }

    /// The composed relationship at `path`.
    pub(crate) fn relationship(&self, path: PropertyPath) -> Option<layerstack::Relationship<'a>> {
        self.view().relationship(path)
    }

    /// The parent of the prim at `path` (`/` for a root prim), if `path` is
    /// not the pseudo-root.
    pub(crate) fn parent(&self, path: PathId) -> Option<PathId> {
        self.store.paths().parent(path)
    }

    /// The pseudo-root, `/`.
    pub(crate) fn root(&self) -> Option<PathId> {
        self.store.paths().lookup(&layerstack::Path::root())
    }
}

/// A [`StageView`]'s stage and store, with OpenUSD's kinds
/// ([`Scene::new`]). A scene is a view and the kind registry it reads
/// `kind` metadata with, so the two are not one type.
impl<'a> From<StageView<'a>> for Scene<'a> {
    fn from(view: StageView<'a>) -> Self {
        Self::new(view.stage(), view.store())
    }
}

/// A composed prim, which every schema view reads through.
#[derive(Clone, Copy, Debug)]
pub struct PrimView<'a> {
    scene: Scene<'a>,
    path: PathId,
}

impl<'a> PrimView<'a> {
    /// A view of the prim at `path`, of no particular schema.
    #[must_use]
    pub fn new(scene: Scene<'a>, path: PathId) -> Self {
        Self { scene, path }
    }

    /// The scene the prim is read from.
    #[must_use]
    pub fn scene(&self) -> Scene<'a> {
        self.scene
    }

    /// The prim's path.
    #[must_use]
    pub fn path(&self) -> PathId {
        self.path
    }

    /// The generic handle of the prim, through which its attributes
    /// explain their values and report their types; `None` when the stage
    /// does not compose the prim.
    #[must_use]
    pub fn prim(&self) -> Option<layerstack::Prim<'a>> {
        self.scene.view().prim(self.path)
    }

    /// The prim's attribute named `name` (its USD name), if the stage
    /// composes one.
    pub(crate) fn attribute(&self, name: &str) -> Option<layerstack::Attribute<'a>> {
        // Straight to the property: a composed property implies its prim.
        self.scene
            .attribute(PropertyPath::new(self.path, self.scene.token(name)?))
    }

    /// The prim's property named `name` (its USD name), if the store holds
    /// that name.
    #[must_use]
    pub fn property_path(&self, name: &str) -> Option<PropertyPath> {
        Some(PropertyPath::new(self.path, self.scene.token(name)?))
    }

    /// The first compatible default value of the property `name`, schema
    /// fallback included, converted by `read`.
    ///
    /// OpenUSD: `UsdAttribute::Get<T>` at default time uses the typed
    /// `MetadataValueComposer`; incompatible dense defaults are skipped.
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §13.3.2.4 (fallbacks).
    pub fn read_value<T>(
        &self,
        name: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        let tokens = self.scene.store.tokens();
        self.attribute(name)?
            .read(Time::Default, |value| read(value, tokens))
            .map(|resolved| resolved.value)
    }

    /// [`PrimView::read_value`] at `time`: at a time code, the strongest
    /// source's value converted, without retrying weaker opinions; at
    /// [`Time::Default`], exactly [`PrimView::read_value`].
    ///
    /// Spec: AOUSD Core §12.3.2 (time-based resolution), §12.5
    /// (interpolation).
    pub fn read_value_at<T>(
        &self,
        name: &str,
        time: Time,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        let tokens = self.scene.store.tokens();
        self.attribute(name)?
            .read(time, |value| read(value, tokens))
            .map(|resolved| resolved.value)
    }

    /// Reads a typed attribute at `time`, preserving deferred decode failures.
    /// `Ok(None)` means missing, blocked or incompatible; `Err` means corrupt
    /// numeric storage. Schema fallbacks and typed source selection match the
    /// ordinary getters. Prefer generated `try_*` array getters when available.
    /// Spec: AOUSD Core §12.3–12.5 (attribute values and interpolation).
    pub fn try_read_value<T>(
        &self,
        name: &str,
        time: Time,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Result<Option<T>, layerstack::ArrayReadError> {
        let Some(attribute) = self.attribute(name) else {
            return Ok(None);
        };
        let tokens = self.scene.store.tokens();
        attribute
            .try_read(time, |value| read(value, tokens))
            .map(|result| result.map(|resolved| resolved.value))
    }

    /// The resolved value of the attribute `name` at `time`, schema
    /// fallback included, as the stage holds it.
    pub(crate) fn raw_value(&self, name: &str, time: Time) -> Option<Value> {
        self.attribute(name)?
            .get(time)
            .map(|resolved| resolved.value)
    }

    // Conservative temporal dependency, not a competing value resolver.
    // Include masked samples, splines and single samples: default time can
    // differ even when all numeric times agree (AOUSD Core §12.3, §12.5).
    pub(crate) fn property_might_vary(&self, name: &str) -> bool {
        let Some(attribute) = self.attribute(name) else {
            return false;
        };
        if attribute.clip_source(Time::held(0.0)).is_some() {
            return true;
        }
        attribute.opinions().iter().any(|opinion| {
            opinion.value.as_property().is_some_and(|spec| {
                spec.spline.is_some()
                    || spec
                        .time_samples
                        .as_ref()
                        .is_some_and(|samples| !samples.is_empty())
            })
        })
    }

    /// Whether an attribute has a resolved value source, including a schema
    /// fallback or animation-only source. A block without a fallback has none.
    /// OpenUSD: `UsdAttribute::HasValue`; AOUSD Core §12.3.
    #[must_use]
    pub fn has_value(&self, name: &str) -> bool {
        self.has_attribute(name)
            && (self.has_authored_value(name) || self.raw_value(name, Time::Default).is_some())
    }
    /// Declared attributes whose names equal `namespace` or start with that
    /// namespace followed by `:`. An empty namespace selects all attributes.
    /// A trailing `:` is accepted; partial token prefixes do not match.
    /// Includes schema-only declarations, sorted by name.
    #[must_use]
    pub fn attributes_in_namespace(&self, namespace: &str) -> Vec<PropertyPath> {
        self.namespace_attributes(namespace, false)
    }
    /// Authored attributes in a namespace, excluding schema-only declarations.
    /// Matching follows `attributes_in_namespace`, with results sorted by name.
    #[must_use]
    pub fn authored_attributes_in_namespace(&self, namespace: &str) -> Vec<PropertyPath> {
        self.namespace_attributes(namespace, true)
    }
    fn namespace_attributes(&self, namespace: &str, authored: bool) -> Vec<PropertyPath> {
        let namespace = namespace.trim_end_matches(':');
        let Some(prim) = self.prim() else {
            return Vec::new();
        };
        let mut names = if authored {
            prim.authored_property_names()
        } else {
            prim.property_names()
        };
        names.retain(|name| {
            namespace.is_empty()
                || *name == namespace
                || name
                    .strip_prefix(namespace)
                    .is_some_and(|tail| tail.starts_with(':'))
        });
        names.sort_unstable();
        names
            .into_iter()
            .filter_map(|name| prim.attribute(name).map(|attribute| attribute.path()))
            .collect()
    }

    /// Whether an opinion authors a value for the attribute `name`, at any
    /// time: the strongest opinion with a spline, time samples or a
    /// default decides, and a default block authors none. Attribute reads
    /// resolve the value at the requested time: an attribute with only
    /// time samples still reads its fallback at the default time.
    ///
    /// OpenUSD: `UsdAttribute::HasAuthoredValue` (`UsdResolveInfo` with no
    /// time).
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §12.3.6 (blocks).
    pub fn has_authored_value(&self, name: &str) -> bool {
        let Some(property) = self.prim().and_then(|prim| prim.property(name)) else {
            return false;
        };
        if property
            .as_attribute()
            .is_some_and(|attribute| attribute.clip_source(Time::held(0.0)).is_some())
        {
            return true;
        }
        for spec in property
            .opinions()
            .iter()
            .filter_map(|o| o.value.as_property())
        {
            if spec.spline.is_some() || spec.time_samples.as_ref().is_some_and(|s| !s.is_empty()) {
                return true;
            }
            match &spec.default {
                Some(Value::Blocked) => return false,
                Some(_) => return true,
                None => {}
            }
        }
        false
    }

    /// Whether the prim has an attribute `name`: one an opinion declares or
    /// its schemas define.
    ///
    /// OpenUSD: `UsdPrim::GetAttribute(name)` is valid.
    pub fn has_attribute(&self, name: &str) -> bool {
        self.attribute(name).is_some()
    }

    /// The composed targets of the relationship `name` (its USD name);
    /// empty when it has none.
    ///
    /// Spec: AOUSD Core §12.4.
    #[must_use]
    pub fn read_targets(&self, name: &str) -> Vec<TargetPath> {
        self.scene
            .token(name)
            .and_then(|token| self.scene.relationship(PropertyPath::new(self.path, token)))
            .map(layerstack::Relationship::targets)
            .unwrap_or_default()
    }
}

/// A prim a schema edit handle authors: the root every edit handle derefs
/// to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PrimEdit {
    path: PathId,
}

impl PrimEdit {
    /// A handle authoring the prim at `path` through `edit`, if the prim
    /// exists: on the edit's stage, or defined earlier in `edit`.
    #[must_use]
    pub fn new(edit: &SchemaEdit<'_>, path: impl PrimPathArg) -> Option<Self> {
        let path = edit.prim_path(path)?;
        edit.exists(path).then_some(Self { path })
    }

    /// A handle authoring the prim at `path`, without checking that it
    /// exists: a setter on a prim that does not authors an `over` for it,
    /// which [`Self::new`] never lets happen. For a prim known to exist,
    /// such as the prim of a schema view had from its `new` or `get`.
    #[must_use]
    pub fn from_path_unchecked(path: PathId) -> Self {
        Self { path }
    }

    /// The prim's path.
    #[must_use]
    pub fn path(&self) -> PathId {
        self.path
    }

    /// Authors `value` as the default of the attribute `name` (its USD
    /// name). An attribute no layer authors yet is created as the prim's
    /// schemas define it: the type and variability of their definition.
    ///
    /// OpenUSD: `UsdAttribute::Set` at the default time, after
    /// `UsdPrim::CreateAttribute` for a schema attribute.
    ///
    /// Spec: AOUSD Core §7.6.4.1.3 (`default`), §12.3 (attribute values).
    pub fn write_value(&self, edit: &mut SchemaEdit<'_>, name: &str, value: Value) {
        edit.set_value(self.path, name, None, value);
    }

    /// [`Self::write_value`] as a time sample at the time code `time`,
    /// mapped through the edit target.
    ///
    /// OpenUSD: `UsdAttribute::Set` at a time code.
    ///
    /// Spec: AOUSD Core §7.6.4.1.4 (`timeSamples`), §12.3.2 (time-based
    /// resolution).
    pub fn write_value_at(&self, edit: &mut SchemaEdit<'_>, name: &str, time: f64, value: Value) {
        edit.set_value(self.path, name, Some(time), value);
    }

    /// Authors `targets`, stage paths, as the explicit targets of the
    /// relationship `name` (its USD name), mapped through the edit target.
    /// A relationship no layer authors and no schema defines is created
    /// `custom`, as C++ creates it; a schema's own edits, such as
    /// `MaterialBindingApiEdit::bind`, create theirs not `custom`.
    ///
    /// OpenUSD: `UsdRelationship::SetTargets`.
    ///
    /// Spec: AOUSD Core §12.4 (relationships).
    pub fn write_targets(&self, edit: &mut SchemaEdit<'_>, name: &str, targets: &[TargetPath]) {
        edit.set_targets(self.path, name, targets);
    }

    /// Adds `target`, a stage path mapped through the edit target, to the
    /// targets of the relationship `name` (its USD name) at `position`. It
    /// edits the list op the edit target's spec authors, where
    /// [`Self::write_targets`] replaces it with an explicit list, so the
    /// targets weaker layers author stay.
    ///
    /// OpenUSD: `UsdRelationship::AddTarget`.
    ///
    /// Spec: AOUSD Core §7.6.5.1.1 (`targetPaths`), §12.4 (list ops).
    pub fn add_target(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        target: TargetPath,
        position: ListPosition,
    ) {
        edit.add_target(self.path, name, target, position);
    }

    /// Removes `target`, a stage path mapped through the edit target, from
    /// the targets of the relationship `name` (its USD name): the edit
    /// target's list op deletes it, which removes it from the targets
    /// weaker layers author too, or an explicit list drops it.
    ///
    /// OpenUSD: `UsdRelationship::RemoveTarget`.
    ///
    /// Spec: AOUSD Core §7.6.5.1.1 (`targetPaths`), §12.4 (list ops).
    pub fn remove_target(&self, edit: &mut SchemaEdit<'_>, name: &str, target: TargetPath) {
        edit.remove_target(self.path, name, target);
    }

    /// Authors `value` as the metadata field `key` of the property `name`
    /// (both USD names), such as `documentation` (USDA `doc`),
    /// `displayName`, `customData`, `bindMaterialAs` on a binding
    /// relationship, `connectability` on a shading input, `weight` on a
    /// blend-shape inbetween, or a field a custom schema's plugin registers.
    /// A schema-defined property no layer authors yet gets a spec from its
    /// definition, as C++ creates one. Read it back with
    /// [`PrimView::property_metadata`].
    ///
    /// The field must be registered for the property's kind, by Sdf or by
    /// a plugin's `SdfMetadata` in the stage's schema registry, and `value`
    /// must be of its registered type: a `token` field takes
    /// [`Value::Token`], a `string` field [`Value::String`], a `float` field
    /// [`Value::Float`]. Fields of the spec itself (`typeName`, `custom`,
    /// `default`, …) have their own edits and are refused. A refusal
    /// collects nothing.
    ///
    /// OpenUSD: `UsdObject::SetMetadata`.
    ///
    /// Spec: AOUSD Core §7.4 (metadata), §12.2 (metadata resolution).
    ///
    /// # Errors
    ///
    /// Why the field cannot be authored on that property.
    #[doc(alias = "SetMetadata")]
    #[doc(alias = "UsdObject::SetMetadata")]
    pub fn write_property_metadata(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        key: &str,
        value: Value,
    ) -> Result<(), crate::PropertyMetadataError> {
        edit.author_property_metadata(self.path, name, key, Some(value))
    }

    /// Clears the metadata field `key` of the property `name` in the edit
    /// target, so weaker opinions or the field's fallback show through. The
    /// field is checked as [`Self::write_property_metadata`] checks it; a
    /// property with no spec in the edit target has nothing to clear.
    ///
    /// OpenUSD: `UsdObject::ClearMetadata`.
    ///
    /// # Errors
    ///
    /// Why the field cannot be authored on that property.
    #[doc(alias = "ClearMetadata")]
    #[doc(alias = "UsdObject::ClearMetadata")]
    pub fn clear_property_metadata(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        key: &str,
    ) -> Result<(), crate::PropertyMetadataError> {
        edit.author_property_metadata(self.path, name, key, None)
    }
}

/// One instance of a multiple-apply schema on a composed prim: the root
/// every multiple-apply view derefs to. Its properties are named for the
/// instance (`collection:<instance>:includes`).
///
/// Spec: AOUSD Core §13.3.2 (instance names form property names).
#[derive(Clone, Copy, Debug)]
pub struct InstanceView<'a> {
    prim: PrimView<'a>,
    instance: &'a str,
}

impl<'a> core::ops::Deref for InstanceView<'a> {
    type Target = PrimView<'a>;

    fn deref(&self) -> &Self::Target {
        &self.prim
    }
}

impl<'a> InstanceView<'a> {
    /// A view of the instance `instance` of a multiple-apply schema on
    /// `prim`, of no particular schema, as [`PrimView::new`] is.
    #[must_use]
    pub fn new(prim: PrimView<'a>, instance: &'a str) -> Self {
        Self { prim, instance }
    }

    /// The instance name.
    #[must_use]
    pub fn instance(&self) -> &'a str {
        self.instance
    }

    /// [`PrimView::read_value`] of the instance's property `template`: a
    /// multiple-apply schema's property name, whose `__INSTANCE_NAME__`
    /// segment is the instance name (`collection:__INSTANCE_NAME__:includes`
    /// reads `collection:<instance>:includes`); a name without that segment
    /// is read as it is.
    pub fn read_value<T>(
        &self,
        template: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        self.prim
            .read_value(&instance_name(template, self.instance), read)
    }

    /// [`PrimView::read_value_at`] of the instance's property `template`
    /// (see [`Self::read_value`]).
    pub fn read_value_at<T>(
        &self,
        template: &str,
        time: Time,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        self.prim
            .read_value_at(&instance_name(template, self.instance), time, read)
    }

    /// Checked typed read of this instance's namespaced attribute.
    /// Missing, blocked or incompatible values return `Ok(None)`; deferred
    /// numeric decode errors are returned unchanged.
    pub fn try_read_value<T>(
        &self,
        template: &str,
        time: Time,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Result<Option<T>, layerstack::ArrayReadError> {
        self.prim
            .try_read_value(&instance_name(template, self.instance), time, read)
    }

    /// [`PrimView::read_targets`] of the instance's relationship `template`
    /// (see [`Self::read_value`]).
    #[must_use]
    pub fn read_targets(&self, template: &str) -> Vec<TargetPath> {
        self.prim
            .read_targets(&instance_name(template, self.instance))
    }
}

/// One instance of a multiple-apply schema a handle authors: the root every
/// multiple-apply edit handle derefs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InstanceEdit {
    prim: PrimEdit,
    instance: alloc::sync::Arc<str>,
}

impl core::ops::Deref for InstanceEdit {
    type Target = PrimEdit;

    fn deref(&self) -> &Self::Target {
        &self.prim
    }
}

impl InstanceEdit {
    /// A handle authoring the instance `instance` of a multiple-apply
    /// schema on the prim `prim` authors.
    #[must_use]
    pub fn new(prim: PrimEdit, instance: &str) -> Self {
        Self {
            prim,
            instance: alloc::sync::Arc::from(instance),
        }
    }

    /// The instance name.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// [`PrimEdit::write_value`] of the instance's attribute `template`:
    /// a multiple-apply schema's property name, whose `__INSTANCE_NAME__`
    /// segment is the instance name (see [`InstanceView::read_value`]).
    pub fn write_value(&self, edit: &mut SchemaEdit<'_>, template: &str, value: Value) {
        self.prim
            .write_value(edit, &instance_name(template, &self.instance), value);
    }

    /// [`PrimEdit::write_value_at`] of the instance's attribute `template`
    /// (see [`Self::write_value`]).
    pub fn write_value_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        template: &str,
        time: f64,
        value: Value,
    ) {
        self.prim
            .write_value_at(edit, &instance_name(template, &self.instance), time, value);
    }

    /// [`PrimEdit::write_targets`] of the instance's relationship
    /// `template` (see [`Self::write_value`]).
    pub fn write_targets(&self, edit: &mut SchemaEdit<'_>, template: &str, targets: &[TargetPath]) {
        self.prim
            .write_targets(edit, &instance_name(template, &self.instance), targets);
    }

    /// [`PrimEdit::add_target`] of the instance's relationship `template`
    /// (see [`Self::write_value`]).
    pub fn add_target(
        &self,
        edit: &mut SchemaEdit<'_>,
        template: &str,
        target: TargetPath,
        position: ListPosition,
    ) {
        self.prim.add_target(
            edit,
            &instance_name(template, &self.instance),
            target,
            position,
        );
    }

    /// [`PrimEdit::remove_target`] of the instance's relationship
    /// `template` (see [`Self::write_value`]).
    pub fn remove_target(&self, edit: &mut SchemaEdit<'_>, template: &str, target: TargetPath) {
        self.prim
            .remove_target(edit, &instance_name(template, &self.instance), target);
    }
}

/// The name a multiple-apply schema's property `template` has for
/// `instance`.
pub(crate) fn instance_name(template: &str, instance: &str) -> String {
    let placeholder = layerstack::schema::INSTANCE_NAME_PLACEHOLDER;
    let mut out = String::new();
    let mut replaced = false;
    for (i, segment) in template.split(':').enumerate() {
        if i > 0 {
            out.push(':');
        }
        if !replaced && segment == placeholder {
            out.push_str(instance);
            replaced = true;
        } else {
            out.push_str(segment);
        }
    }
    out
}

/// Whether an authored declaration or schema defines a relationship on an existing prim.
#[cfg(any(
    feature = "usd-shade",
    feature = "usd-render",
    feature = "usd-skel",
    feature = "usd-vol",
    feature = "usd-lod"
))]
pub(crate) fn is_relationship(scene: &Scene<'_>, path: PropertyPath) -> bool {
    scene.relationship(path).is_some()
}

/// Ordered relationship targets after recursively forwarding relationship paths.
/// OpenUSD: `UsdRelationship::GetForwardedTargets`.
#[cfg(any(
    feature = "usd-shade",
    feature = "usd-render",
    feature = "usd-skel",
    feature = "usd-vol",
    feature = "usd-lod"
))]
pub(crate) fn forwarded_targets(scene: &Scene<'_>, root: PropertyPath) -> Vec<TargetPath> {
    if !is_relationship(scene, root) {
        return Vec::new();
    }
    // OpenUSD: UsdRelationship::GetForwardedTargets. Stack traversal avoids
    // recursion on adversarial relationship chains; visited edges stop cycles.
    let mut stack = alloc::vec![TargetPath::Property(root)];
    let mut seen = hashbrown::HashSet::new();
    let mut out = Vec::new();
    let mut unique = hashbrown::HashSet::new();
    while let Some(target) = stack.pop() {
        if let TargetPath::Property(path) = target
            && is_relationship(scene, path)
        {
            if !seen.insert(path) {
                continue;
            }
            if let Some(relationship) = scene.relationship(path) {
                stack.extend(relationship.targets().into_iter().rev());
            }
        } else if unique.insert(target) {
            out.push(target);
        }
    }
    out
}

#[cfg(all(test, feature = "usd-geom"))]
mod handle_tests {
    use alloc::{string::ToString, sync::Arc};

    use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, Time};

    use crate::usd_geom::{Sphere, Xform};

    use super::Scene;

    /// Views take text paths or ids, and reach the generic handles.
    #[test]
    fn views_take_text_paths_and_reach_generic_handles() {
        let mut store = InMemoryStore::default();
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(&store.tokens))),
            ..StageOptions::default()
        };
        let ball = store.path("/Ball");
        let sphere = store.tokens.intern("Sphere");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(ball, PrimSpec::def().with_type_name(sphere));
        store.insert_layer(layer);
        let stage = Stage::compose(&mut store, LayerId(1), options);
        let scene = Scene::from(stage.view(&store));
        let by_text = Sphere::new(&scene, "/Ball").expect("a sphere");
        assert_eq!(by_text.path(), ball);
        assert!(Sphere::new(&scene, ball).is_some());
        assert!(Xform::new(&scene, "/Ball").is_none(), "not an Xform");
        for absent in ["/Nowhere", "Ball", "/Ball/"] {
            assert!(Sphere::new(&scene, absent).is_none(), "{absent:?}");
        }
        let radius = by_text
            .prim()
            .and_then(|prim| prim.attribute("radius"))
            .expect("the schema's radius");
        assert_eq!(radius.type_name(), Some("double"));
        assert_eq!(radius.to_string(), "/Ball.radius");
        let explained = radius.explain(Time::Default).expect("a fallback");
        assert_eq!(explained.source, layerstack::ValueSource::Fallback);
        assert_eq!(explained.value, Some(layerstack::Value::Double(1.0)));
        assert_eq!(by_text.radius(), Some(1.0));
    }
}

#[cfg(test)]
mod model_hierarchy_tests {
    use super::*;
    use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, StageOptions};

    #[test]
    fn model_requires_group_ancestors_through_a_deep_namespace() {
        let mut store = InMemoryStore::default();
        let kind = store.tokens.intern("kind");
        let group = store.tokens.intern("group");
        let component = store.tokens.intern("component");
        let mut layer = Layer::new(LayerId(1));
        let mut name = String::new();
        for _ in 0..256 {
            name.push_str("/Group");
            let path = store.path(&name);
            layer.insert_prim(path, PrimSpec::def().with_field(kind, Value::Token(group)));
        }
        let deepest_group = store.path(&name);
        name.push_str("/Asset");
        let asset = store.path(&name);
        layer.insert_prim(
            asset,
            PrimSpec::def().with_field(kind, Value::Token(component)),
        );
        name.push_str("/NestedGroup");
        let invalid = store.path(&name);
        layer.insert_prim(
            invalid,
            PrimSpec::def().with_field(kind, Value::Token(group)),
        );
        store.insert_layer(layer);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let scene = Scene::new(&stage, &store);
        assert!(scene.is_group(deepest_group));
        assert!(scene.is_model(asset));
        assert!(!scene.is_group(asset));
        assert!(!scene.is_model(invalid));
        assert!(!scene.is_group(invalid));
    }
}
