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
    InterpolationType, LayerStore, PathId, PropertyPath, ResolvedValue, SchemaKind, Stage,
    TargetPath, TokenId, TokenInterner, Value,
};

use crate::edit::SchemaEdit;

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
}

impl fmt::Debug for Scene<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scene").finish_non_exhaustive()
    }
}

impl<'a> Scene<'a> {
    /// The scene of `stage`, composed from `store`.
    #[must_use]
    pub fn new(stage: &'a Stage, store: &'a dyn LayerStore) -> Self {
        Self { stage, store }
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
        let Some(registry) = self.stage.schemas() else {
            return false;
        };
        let Some(type_name) = self.stage.resolve_type_name(path, self.store) else {
            return false;
        };
        registry
            .schema(type_name)
            .is_some_and(|s| s.kind == SchemaKind::ConcreteTyped)
            && self
                .token(schema)
                .is_some_and(|schema| registry.is_a(type_name, schema))
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
        let Some(definition) = self.stage.prim_definition(path, self.store) else {
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
        let (Some(schema), Some(definition)) = (
            self.token(schema),
            self.stage.prim_definition(path, self.store),
        ) else {
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
