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
//! Getters return the resolved value converted to Rust, or `None` when
//! there is none (nothing authored and no fallback, a value block, or a
//! value of another type). For anything a view does not offer, read the
//! stage directly with [`PrimView::path`] and the property's USD name:
//! [`Stage::resolve_value_with_schema`] returns the raw
//! `Resolved<ResolvedValue>` with its provenance.

use core::fmt;

use alloc::{string::String, vec::Vec};

use layerstack::{
    InterpolationType, LayerStore, PathId, PropertyKind, PropertyPath, ResolvedValue, Stage,
    TargetPath, TokenId, TokenInterner, Value,
};

use crate::edit::SchemaEdit;
use crate::kind::KindRegistry;

/// When a computation reads the stage: the default time, or a time code
/// with the interpolation between its time samples.
///
/// OpenUSD: `UsdTimeCode` with the stage's `UsdInterpolationType`; linear
/// is OpenUSD's default.
///
/// Spec: AOUSD Core §12.3 (default values and time samples), §12.5
/// (interpolation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Time {
    /// The default time: only default values, never time samples.
    Default,
    /// A time code, interpolating between time samples as `interpolation`
    /// says.
    At {
        /// The time code.
        code: f64,
        /// How values between time samples are interpolated.
        interpolation: InterpolationType,
    },
}

impl Time {
    /// The time code `code`, interpolating linearly (OpenUSD's default).
    #[must_use]
    pub fn at(code: f64) -> Self {
        Self::At {
            code,
            interpolation: InterpolationType::Linear,
        }
    }

    /// The time code `code`, holding each time sample until the next.
    #[must_use]
    pub fn held(code: f64) -> Self {
        Self::At {
            code,
            interpolation: InterpolationType::Held,
        }
    }
}

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
        let key = self.token("kind")?;
        let resolved = self.stage.resolve_field(path, key)?;
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
            self.stage
                .prim_definition_ref(path)
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
        let Some(definition) = self.stage.prim_definition_ref(path) else {
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
        let (Some(schema), Some(definition)) =
            (self.token(schema), self.stage.prim_definition_ref(path))
        else {
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

    /// The parent of the prim at `path` (`/` for a root prim), if `path` is
    /// not the pseudo-root.
    pub(crate) fn parent(&self, path: PathId) -> Option<PathId> {
        let paths = self.store.paths();
        paths.lookup(&paths.resolve(path).parent()?)
    }

    /// The pseudo-root, `/`.
    pub(crate) fn root(&self) -> Option<PathId> {
        self.store.paths().lookup(&layerstack::Path::root())
    }

    /// `instance` as the store holds it, if any prim could apply it.
    pub(crate) fn instance_name(&self, instance: &str) -> Option<&'a str> {
        let tokens = self.store.tokens();
        tokens.lookup(instance).map(|token| tokens.resolve(token))
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

    /// The prim's property named `name` (its USD name), if the store holds
    /// that name.
    #[must_use]
    pub fn property_path(&self, name: &str) -> Option<PropertyPath> {
        Some(PropertyPath::new(self.path, self.scene.token(name)?))
    }

    /// The resolved value of the property `name`, schema fallback included,
    /// converted by `read`.
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §13.3.2.4 (fallbacks).
    pub(crate) fn read_value<T>(
        &self,
        name: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        let token = self.scene.token(name)?;
        let resolved =
            self.scene
                .stage
                .resolve_value_with_schema(self.path, token, self.scene.store)?;
        match resolved.value {
            ResolvedValue::Scalar(value) => read(&value, self.scene.store.tokens()),
            _ => None,
        }
    }

    /// [`PrimView::read_value`] at the time code `time`.
    ///
    /// Spec: AOUSD Core §12.3.2 (time-based resolution), §12.5
    /// (interpolation).
    pub(crate) fn read_value_at<T>(
        &self,
        name: &str,
        time: f64,
        interp: InterpolationType,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        let token = self.scene.token(name)?;
        let resolved = self.scene.stage.resolve_value_at_time_with_schema(
            self.path,
            token,
            time,
            interp,
            self.scene.store,
        )?;
        read(&resolved.value, self.scene.store.tokens())
    }

    /// The resolved value of the attribute `name` at `time`, schema
    /// fallback included, as the stage holds it.
    pub(crate) fn raw_value(&self, name: &str, time: Time) -> Option<Value> {
        let raw = |value: &Value, _: &TokenInterner| Some(value.clone());
        match time {
            Time::Default => self.read_value(name, raw),
            Time::At {
                code,
                interpolation,
            } => self.read_value_at(name, code, interpolation, raw),
        }
    }

    // Conservative temporal dependency, not a competing value resolver.
    // Include masked samples, splines and single samples: default time can
    // differ even when all numeric times agree (AOUSD Core §12.3, §12.5).
    pub(crate) fn property_might_vary(&self, name: &str) -> bool {
        self.property_path(name)
            .and_then(|property| self.scene.stage.explain_property_path(property))
            .is_some_and(|opinions| {
                opinions.iter().any(|opinion| {
                    opinion.value.as_property().is_some_and(|spec| {
                        spec.spline.is_some()
                            || spec
                                .time_samples
                                .as_ref()
                                .is_some_and(|samples| !samples.is_empty())
                    })
                })
            })
    }

    /// Whether an opinion authors a value for the attribute `name`, at any
    /// time: the strongest opinion with a spline, time samples or a
    /// default decides, and a default block authors none. Whether that
    /// value applies at a given time is [`PrimView::raw_value`]'s to say:
    /// an attribute with only time samples still reads its fallback at the
    /// default time.
    ///
    /// OpenUSD: `UsdAttribute::HasAuthoredValue` (`UsdResolveInfo` with no
    /// time).
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §12.3.6 (blocks).
    pub(crate) fn has_authored_value(&self, name: &str) -> bool {
        let Some(property) = self.property_path(name) else {
            return false;
        };
        let Some(opinions) = self.scene.stage.explain_property_path(property) else {
            return false;
        };
        for spec in opinions.iter().filter_map(|o| o.value.as_property()) {
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
    pub(crate) fn has_attribute(&self, name: &str) -> bool {
        let Some(token) = self.scene.token(name) else {
            return false;
        };
        let stage = self.scene.stage;
        if let Some(declared) = stage.resolve_property_declaration(self.path, token) {
            return declared.kind == PropertyKind::Attribute;
        }
        stage
            .property_definition_ref(self.path, token)
            .is_some_and(|defined| defined.kind == PropertyKind::Attribute)
    }

    /// The composed targets of the relationship `name`.
    ///
    /// Spec: AOUSD Core §12.4.
    pub(crate) fn read_targets(&self, name: &str) -> Vec<TargetPath> {
        self.scene
            .token(name)
            .and_then(|token| {
                self.scene
                    .stage
                    .resolve_target_list_path(PropertyPath::new(self.path, token))
            })
            .map(|resolved| resolved.value)
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
    /// A handle authoring the prim at `path`, which the caller has
    /// checked exists.
    pub(crate) fn new(path: PathId) -> Self {
        Self { path }
    }

    /// The prim's path.
    #[must_use]
    pub fn path(&self) -> PathId {
        self.path
    }

    /// Authors `value` for the attribute `name`, at `time` or as its
    /// default.
    pub(crate) fn write_value(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        time: Option<f64>,
        value: Value,
    ) {
        edit.set_value(self.path, name, time, value);
    }

    /// Authors `targets` as the explicit targets of the relationship `name`.
    pub(crate) fn write_targets(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        targets: &[TargetPath],
    ) {
        edit.set_targets(self.path, name, targets);
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
    pub(crate) fn new(prim: PrimView<'a>, instance: &'a str) -> Self {
        Self { prim, instance }
    }

    /// The instance name.
    #[must_use]
    pub fn instance(&self) -> &'a str {
        self.instance
    }

    pub(crate) fn read_value<T>(
        &self,
        template: &str,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        self.prim
            .read_value(&instance_name(template, self.instance), read)
    }

    pub(crate) fn read_value_at<T>(
        &self,
        template: &str,
        time: f64,
        interp: InterpolationType,
        read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    ) -> Option<T> {
        self.prim
            .read_value_at(&instance_name(template, self.instance), time, interp, read)
    }

    pub(crate) fn read_targets(&self, template: &str) -> Vec<TargetPath> {
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
    pub(crate) fn new(path: PathId, instance: &str) -> Self {
        Self {
            prim: PrimEdit::new(path),
            instance: alloc::sync::Arc::from(instance),
        }
    }

    /// The instance name.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    pub(crate) fn write_value(
        &self,
        edit: &mut SchemaEdit<'_>,
        template: &str,
        time: Option<f64>,
        value: Value,
    ) {
        self.prim
            .write_value(edit, &instance_name(template, &self.instance), time, value);
    }

    pub(crate) fn write_targets(
        &self,
        edit: &mut SchemaEdit<'_>,
        template: &str,
        targets: &[TargetPath],
    ) {
        self.prim
            .write_targets(edit, &instance_name(template, &self.instance), targets);
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

/// A typed or single-apply view's getters for one attribute: the value at
/// the default time and at a time code.
macro_rules! attribute {
    ($(#[$doc:meta])* $get:ident, $get_at:ident, $name:literal, $ty:ty, $read:expr) => {
        $(#[$doc])*
        #[must_use]
        pub fn $get(&self) -> Option<$ty> {
            self.read_value($name, $read)
        }

        #[doc = concat!(
            "[`Self::", stringify!($get), "`] at the time code `time`, ",
            "interpolated with `interp`."
        )]
        #[must_use]
        pub fn $get_at(&self, time: f64, interp: ::layerstack::InterpolationType) -> Option<$ty> {
            self.read_value_at($name, time, interp, $read)
        }
    };
}

/// A view's getter for one relationship: its composed targets.
macro_rules! relationship {
    ($(#[$doc:meta])* $get:ident, $name:literal) => {
        $(#[$doc])*
        #[must_use]
        pub fn $get(&self) -> ::alloc::vec::Vec<::layerstack::TargetPath> {
            self.read_targets($name)
        }
    };
}

/// An edit handle's setters for one attribute: its default and a time
/// sample.
macro_rules! set_attribute {
    ($(#[$doc:meta])* $set:ident, $set_at:ident, $name:literal, $ty:ty, $write:expr) => {
        $(#[$doc])*
        pub fn $set(&self, edit: &mut $crate::SchemaEdit<'_>, value: $ty) -> &Self {
            let value = ($write)(value, edit.tokens());
            self.write_value(edit, $name, None, value);
            self
        }

        #[doc = concat!(
            "[`Self::", stringify!($set), "`] as a time sample at the time code `time`."
        )]
        pub fn $set_at(&self, edit: &mut $crate::SchemaEdit<'_>, time: f64, value: $ty) -> &Self {
            let value = ($write)(value, edit.tokens());
            self.write_value(edit, $name, Some(time), value);
            self
        }
    };
}

/// An edit handle's setter for one relationship: its explicit targets.
macro_rules! set_relationship {
    ($(#[$doc:meta])* $set:ident, $name:literal) => {
        $(#[$doc])*
        pub fn $set(
            &self,
            edit: &mut $crate::SchemaEdit<'_>,
            targets: &[::layerstack::TargetPath],
        ) -> &Self {
            self.write_targets(edit, $name, targets);
            self
        }
    };
}

/// An enum of a token property's `allowedTokens`, with `Other` for any
/// token the schema does not list.
macro_rules! token_enum {
    (
        $(#[$doc:meta])* $name:ident {
            $($(#[$vdoc:meta])* $variant:ident = $token:literal,)*
        }
    ) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vdoc])* $variant,)*
            /// A token the schema does not list: OpenUSD does not enforce
            /// `allowedTokens`, so an authored value may be anything.
            Other(::alloc::sync::Arc<str>),
        }

        impl $name {
            /// The tokens the schema allows, in its order.
            pub const TOKENS: &'static [&'static str] = &[$($token),*];

            /// The value `token` names, `Other` when the schema does not
            /// list it.
            #[must_use]
            pub fn from_token(token: &str) -> Self {
                match token {
                    $($token => Self::$variant,)*
                    other => Self::Other(::alloc::sync::Arc::from(other)),
                }
            }

            /// The token this value is.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $token,)*
                    Self::Other(token) => token,
                }
            }

            pub(crate) fn read(
                value: &::layerstack::Value,
                tokens: &::layerstack::TokenInterner,
            ) -> Option<Self> {
                $crate::value::read_token(value, tokens).map(Self::from_token)
            }

            pub(crate) fn write(
                self,
                tokens: &mut ::layerstack::TokenInterner,
            ) -> ::layerstack::Value {
                $crate::value::write_token(self.as_str(), tokens)
            }
        }
    };
}

/// A typed or single-apply view's getter for one `uniform` attribute, which
/// has no time samples to read.
macro_rules! uniform_attribute {
    ($(#[$doc:meta])* $get:ident, $name:literal, $ty:ty, $read:expr) => {
        $(#[$doc])*
        #[must_use]
        pub fn $get(&self) -> Option<$ty> {
            self.read_value($name, $read)
        }
    };
}

/// An edit handle's setter for one `uniform` attribute: its default.
macro_rules! set_uniform_attribute {
    ($(#[$doc:meta])* $set:ident, $name:literal, $ty:ty, $write:expr) => {
        $(#[$doc])*
        pub fn $set(&self, edit: &mut $crate::SchemaEdit<'_>, value: $ty) -> &Self {
            let value = ($write)(value, edit.tokens());
            self.write_value(edit, $name, None, value);
            self
        }
    };
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
