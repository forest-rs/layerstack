// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Metadata fields: what each field is declared to hold, and on which kinds
//! of spec it is metadata.
//!
//! Every field is declared once, by Sdf itself, by one of the plugins
//! usd-core 26.08 is built with (`interpolation` is usdGeom's), or by a
//! plugin of one's own (a [`MetadataDefinition`] a
//! [`SchemaRegistryBuilder`](crate::SchemaRegistryBuilder) registers, read
//! from a `plugInfo.json`'s `SdfMetadata`). [`MetadataField::lookup`] finds
//! a field among all three, and readers, writers and edits ask it alike, so
//! a plugin's field behaves as a built-in one does:
//!
//! - its [`MetadataType`] converts and checks values: the USDA reader
//!   converts `irIsInvertible = 0` to `false` for a `bool` field and
//!   rejects a literal the type cannot hold;
//! - its [`MetadataTarget`]s say where it is metadata: on another kind of
//!   spec the same name is no field of the registry's, and OpenUSD keeps
//!   its value as text it does not interpret;
//! - its fallback ([`MetadataField::fallback`]) is the value a layer's
//!   unauthored field reads as.
//!
//! Spec: AOUSD Core §7.4 (metadata), §7.6 (the fields of each spec type).
//! OpenUSD: `SdfSchemaBase::_RegisterStandardFields` and
//! `_UpdateMetadataFromPlugins` (`pxr/usd/sdf/schema.cpp`).

mod builtin;

use crate::sort;
use alloc::{boxed::Box, format, sync::Arc, vec::Vec};
use core::fmt;

use crate::doc::{FieldValue, Value};
use crate::interner::{TokenId, TokenInterner};
use crate::value_type::ValueType;
use crate::{SchemaRegistry, TypedArray};

pub use builtin::MetadataOrigin;

/// A kind of spec a metadata field may be registered for.
///
/// OpenUSD: the spec types `SdfSchema::RegisterPluginMetadata` extends
/// (`SdfMetadata.appliesTo`: `layers`, `prims`, `attributes`,
/// `relationships`, `properties` for both of those, and `variants`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetadataTarget {
    /// A layer's own metadata (its pseudo-root), which a stage's root
    /// layer gives the stage.
    Layer,
    /// Prim specs. A field registered for prims is registered for variants
    /// too ([`MetadataDefinition::applies_to`]).
    Prim,
    /// Variant specs (`appliesTo: "variants"`).
    Variant,
    /// Attribute specs.
    Attribute,
    /// Relationship specs.
    Relationship,
}

impl MetadataTarget {
    /// The name of the spec type, as OpenUSD's messages give it
    /// (`SdfSpecTypeAttribute`).
    #[must_use]
    pub const fn spec_type_name(self) -> &'static str {
        match self {
            Self::Layer => "SdfSpecTypePseudoRoot",
            Self::Prim => "SdfSpecTypePrim",
            Self::Variant => "SdfSpecTypeVariant",
            Self::Attribute => "SdfSpecTypeAttribute",
            Self::Relationship => "SdfSpecTypeRelationship",
        }
    }
}

impl fmt::Display for MetadataTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Layer => "layer",
            Self::Prim => "prim",
            Self::Variant => "variant",
            Self::Attribute => "attribute",
            Self::Relationship => "relationship",
        })
    }
}

/// The item type of a list-op metadata field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListOpItem {
    /// `tokenlistop`.
    Token,
    /// `stringlistop`.
    String,
    /// `intlistop`.
    Int,
    /// `uintlistop`.
    UInt,
    /// `int64listop`.
    Int64,
    /// `uint64listop`.
    UInt64,
}

impl ListOpItem {
    /// The value type of one item (`token` for `tokenlistop`).
    #[must_use]
    pub const fn value_type(self) -> ValueType {
        match self {
            Self::Token => ValueType::Token,
            Self::String => ValueType::String,
            Self::Int => ValueType::Int,
            Self::UInt => ValueType::UInt,
            Self::Int64 => ValueType::Int64,
            Self::UInt64 => ValueType::UInt64,
        }
    }
}

/// What a metadata field is declared to hold: a value type or its array
/// form, a dictionary, or a list op.
///
/// A role type (`color3f`) holds the values of its storage type (`float3`),
/// and OpenUSD registers the field with that storage type's fallback, so
/// the role makes no difference to what the field holds.
///
/// Spec: AOUSD Core §6.2–§6.5 (value types), §7.4 (metadata field types).
/// OpenUSD: a plugin field's `type` (`SdfSchema::FindType`, `dictionary`,
/// and the six list-op types of `_GetDefaultValueForListOp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetadataType {
    /// A scalar of a value type (`float`, `color3f`).
    Value(ValueType),
    /// An array of a value type (`token[]`).
    Array(ValueType),
    /// A dictionary (`dictionary`).
    Dictionary,
    /// A list op (`tokenlistop`).
    ListOp(ListOpItem),
}

impl MetadataType {
    /// The type named `name` as a `plugInfo.json` names it: a value type
    /// or its array form (`float`, `color3f[]`), `dictionary`, or a list-op
    /// type (`tokenlistop`); `None` for any other name, which OpenUSD does
    /// not register (`"…" is not a registered value type`), including
    /// `path` and capitalized aliases (`Float`).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "dictionary" => Self::Dictionary,
            "tokenlistop" => Self::ListOp(ListOpItem::Token),
            "stringlistop" => Self::ListOp(ListOpItem::String),
            "intlistop" => Self::ListOp(ListOpItem::Int),
            "uintlistop" => Self::ListOp(ListOpItem::UInt),
            "int64listop" => Self::ListOp(ListOpItem::Int64),
            "uint64listop" => Self::ListOp(ListOpItem::UInt64),
            name => match ValueType::parse(name)? {
                (value_type, false) => Self::Value(value_type),
                (value_type, true) => Self::Array(value_type),
            },
        })
    }

    /// The type's name, as [`Self::parse`] reads it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Value(value_type) => value_type.name(),
            Self::Array(value_type) => value_type
                .array_name()
                .expect("an array type has an array name"),
            Self::Dictionary => "dictionary",
            Self::ListOp(ListOpItem::Token) => "tokenlistop",
            Self::ListOp(ListOpItem::String) => "stringlistop",
            Self::ListOp(ListOpItem::Int) => "intlistop",
            Self::ListOp(ListOpItem::UInt) => "uintlistop",
            Self::ListOp(ListOpItem::Int64) => "int64listop",
            Self::ListOp(ListOpItem::UInt64) => "uint64listop",
        }
    }

    /// The type without a role (`float3` for `color3f`): what the field
    /// holds and converts with.
    #[must_use]
    pub fn storage(self) -> Self {
        match self {
            Self::Value(value_type) => Self::Value(value_type.storage()),
            Self::Array(value_type) => Self::Array(value_type.storage()),
            other => other,
        }
    }

    /// Whether `value` is a value of this type: of its storage type, an
    /// array whose elements all are, a dictionary, or a list op of the
    /// item type. `opaque` and `group` fields hold no value.
    #[must_use]
    pub fn accepts(self, value: &FieldValue) -> bool {
        use core::mem::discriminant;
        let element = |value_type: ValueType| {
            let zero = value_type.zero();
            (!matches!(zero, Value::Null)).then_some(zero)
        };
        match (self, value) {
            (Self::Dictionary, FieldValue::Value(value)) => {
                matches!(value.materialized(), Value::Dictionary(_))
            }
            (Self::Value(value_type), FieldValue::Value(value)) => element(value_type)
                .is_some_and(|zero| discriminant(value.materialized()) == discriminant(&zero)),
            (Self::Array(value_type), FieldValue::Value(value)) => {
                let Some(zero) = element(value_type) else {
                    return false;
                };
                match value.materialized() {
                    Value::Array(items) => items
                        .iter()
                        .all(|item| discriminant(item) == discriminant(&zero)),
                    Value::TypedArray(items) => {
                        discriminant(&items.element_kind()) == discriminant(&zero)
                    }
                    _ => false,
                }
            }
            (Self::ListOp(item), value) => matches!(
                (item, value),
                (ListOpItem::Token, FieldValue::TokenListOp(_))
                    | (ListOpItem::String, FieldValue::StringListOp(_))
                    | (ListOpItem::Int, FieldValue::IntListOp(_))
                    | (ListOpItem::UInt, FieldValue::UIntListOp(_))
                    | (ListOpItem::Int64, FieldValue::Int64ListOp(_))
                    | (ListOpItem::UInt64, FieldValue::UInt64ListOp(_))
            ),
            _ => false,
        }
    }

    /// The value a field of this type falls back to when its declaration
    /// gives no default: the value type's default as Sdf's value type
    /// registry holds it (the identity for matrices and quaternions, zero
    /// otherwise, the empty token for `token`), an empty array, an empty
    /// dictionary or an empty list op. `None` for `opaque` and `group`.
    ///
    /// OpenUSD: `SdfValueTypeName::GetDefaultValue`, and
    /// `_GetDefaultMetadataValue` (`pxr/usd/sdf/schema.cpp`).
    #[must_use]
    pub fn fallback(self) -> Option<FieldValue> {
        Some(match self.storage() {
            Self::Value(value_type) => FieldValue::Value(registry_default(value_type)?),
            Self::Array(value_type) => FieldValue::Value(TypedArray::pack_iter(
                &value_type.zero(),
                &mut core::iter::empty(),
            )),
            Self::Dictionary => FieldValue::Value(Value::Dictionary(Vec::new())),
            Self::ListOp(ListOpItem::Token) => FieldValue::TokenListOp(Box::default()),
            Self::ListOp(ListOpItem::String) => FieldValue::StringListOp(Box::default()),
            Self::ListOp(ListOpItem::Int) => FieldValue::IntListOp(Box::default()),
            Self::ListOp(ListOpItem::UInt) => FieldValue::UIntListOp(Box::default()),
            Self::ListOp(ListOpItem::Int64) => FieldValue::Int64ListOp(Box::default()),
            Self::ListOp(ListOpItem::UInt64) => FieldValue::UInt64ListOp(Box::default()),
        })
    }
}

impl fmt::Display for MetadataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The default value of `value_type` in Sdf's value type registry, which
/// is the identity for matrices and quaternions where
/// [`ValueType::zero`] is zero.
fn registry_default(value_type: ValueType) -> Option<Value> {
    let half_one = crate::half::from_f32(1.0);
    let identity = |n: usize| {
        let mut m = alloc::vec![0.0; n * n];
        for i in 0..n {
            m[i * n + i] = 1.0;
        }
        m
    };
    Some(match value_type.storage() {
        ValueType::Matrix2d => Value::Matrix2d(Box::new(identity(2).try_into().ok()?)),
        ValueType::Matrix3d => Value::Matrix3d(Box::new(identity(3).try_into().ok()?)),
        ValueType::Matrix4d => Value::Matrix4d(Box::new(identity(4).try_into().ok()?)),
        // Stored (i, j, k, real).
        ValueType::Quatd => Value::Quatd([0.0, 0.0, 0.0, 1.0]),
        ValueType::Quatf => Value::Quatf([0.0, 0.0, 0.0, 1.0]),
        ValueType::Quath => Value::Quath([0, 0, 0, half_one]),
        other => match other.zero() {
            Value::Null => return None,
            zero => zero,
        },
    })
}

/// A plugin's metadata declaration, shared by every applicable spec.
///
/// This is distinct from a schema-defined attribute: it does not create a
/// property on a prim. Registered defaults describe Sdf fields; prim and
/// property reads do not return them for an unauthored field, as OpenUSD's
/// `UsdObject::GetMetadata` does not, while a stage's unauthored layer
/// field reads as its fallback (`UsdStage::GetMetadata`).
///
/// Spec: AOUSD Core §7.4 (metadata), §12.2 (metadata resolution).
#[derive(Clone, Debug, PartialEq)]
pub struct MetadataDefinition {
    /// The field name, interned in the registry's token interner.
    pub name: TokenId,
    /// The type the field holds.
    pub value_type: MetadataType,
    /// The spec kinds on which the field is valid; a field for prims is
    /// valid on variants too. Empty for a field registered for no spec,
    /// as a declaration whose `appliesTo` names no kind of spec is.
    pub targets: Vec<MetadataTarget>,
    /// The plugin's default, of `value_type`, if it supplies one; OpenUSD
    /// takes none for a dictionary or list-op field.
    pub default: Option<Value>,
    /// The plugin's `displayGroup`, empty when absent.
    pub display_group: Arc<str>,
    /// The plugin's documentation, empty when absent.
    pub documentation: Arc<str>,
}

impl MetadataDefinition {
    /// A declaration of the field `name` holding `value_type` on
    /// `targets`, with no default, display group or documentation.
    ///
    /// ```
    /// use layerstack::{MetadataDefinition, MetadataTarget, MetadataType, TokenInterner, Value};
    ///
    /// let tokens = TokenInterner::default();
    /// let float = MetadataType::parse("float").unwrap();
    /// let weight = MetadataDefinition::new(tokens.intern("weightScale"), float, [MetadataTarget::Prim])
    ///     .with_default(Value::Float(0.5));
    /// assert!(weight.applies_to(MetadataTarget::Variant));
    /// ```
    #[must_use]
    pub fn new(
        name: TokenId,
        value_type: MetadataType,
        targets: impl IntoIterator<Item = MetadataTarget>,
    ) -> Self {
        Self {
            name,
            value_type,
            targets: targets.into_iter().collect(),
            default: None,
            display_group: Arc::from(""),
            documentation: Arc::from(""),
        }
    }

    /// The declaration with `default`, a value of its type.
    #[must_use]
    pub fn with_default(mut self, default: Value) -> Self {
        self.default = Some(default);
        self
    }

    /// The declaration with its `displayGroup`.
    #[must_use]
    pub fn with_display_group(mut self, display_group: &str) -> Self {
        self.display_group = Arc::from(display_group);
        self
    }

    /// The declaration with its documentation.
    #[must_use]
    pub fn with_documentation(mut self, documentation: &str) -> Self {
        self.documentation = Arc::from(documentation);
        self
    }

    /// Whether the field is registered for `target`; a field registered for
    /// prims is for variants too, as OpenUSD registers it.
    #[must_use]
    pub fn applies_to(&self, target: MetadataTarget) -> bool {
        self.targets.contains(&target)
            || (target == MetadataTarget::Variant && self.targets.contains(&MetadataTarget::Prim))
    }

    /// The field's fallback: its default, else its type's
    /// ([`MetadataType::fallback`]).
    #[must_use]
    pub fn fallback(&self) -> Option<FieldValue> {
        match &self.default {
            Some(value) => Some(FieldValue::Value(value.clone())),
            None => self.value_type.fallback(),
        }
    }
}

/// A metadata field, found by name among the built-in fields and a
/// registry's ([`MetadataField::lookup`]).
#[derive(Clone, Copy, Debug)]
pub struct MetadataField<'a> {
    field: Field<'a>,
}

#[derive(Clone, Copy, Debug)]
enum Field<'a> {
    Builtin(&'static builtin::Builtin),
    Registered(&'a MetadataDefinition),
}

impl<'a> MetadataField<'a> {
    /// The field `name`: a built-in one, that Sdf or one of usd-core's own
    /// plugins registers, else one `registry` holds. A registry refuses a
    /// declaration that conflicts with a built-in field
    /// ([`SchemaRegistryBuilder::register_metadata`](crate::SchemaRegistryBuilder::register_metadata)),
    /// as OpenUSD keeps the first registration of a field.
    #[must_use]
    pub fn lookup(
        name: &str,
        registry: Option<&'a SchemaRegistry>,
        tokens: &TokenInterner,
    ) -> Option<Self> {
        if let Some(field) = Self::builtin(name) {
            return Some(Self { field: field.field });
        }
        let definition = registry
            .zip(tokens.lookup(name))
            .and_then(|(registry, name)| registry.metadata(name))?;
        Some(Self {
            field: Field::Registered(definition),
        })
    }

    /// The built-in field `name`, that Sdf or one of the plugins usd-core
    /// 26.08 is built with registers.
    #[must_use]
    pub fn builtin(name: &str) -> Option<MetadataField<'static>> {
        builtin::find(name).map(|field| MetadataField {
            field: Field::Builtin(field),
        })
    }

    /// Every built-in field, in name order.
    pub fn builtins() -> impl Iterator<Item = (&'static str, MetadataField<'static>)> {
        builtin::BUILTINS.iter().map(|field| {
            (
                field.name,
                MetadataField {
                    field: Field::Builtin(field),
                },
            )
        })
    }

    /// The type the field holds.
    #[must_use]
    pub fn value_type(&self) -> MetadataType {
        match self.field {
            Field::Builtin(field) => field.value_type,
            Field::Registered(definition) => definition.value_type,
        }
    }

    /// The spec kinds the field is registered for.
    #[must_use]
    pub fn targets(&self) -> &'a [MetadataTarget] {
        match self.field {
            Field::Builtin(field) => field.targets,
            Field::Registered(definition) => &definition.targets,
        }
    }

    /// Whether the field is metadata on `target`; a prim field is on
    /// variants too.
    #[must_use]
    pub fn applies_to(&self, target: MetadataTarget) -> bool {
        let targets = self.targets();
        targets.contains(&target)
            || (target == MetadataTarget::Variant && targets.contains(&MetadataTarget::Prim))
    }

    /// The field's fallback, with a token interned in `tokens`; `None` for
    /// an `opaque` field.
    ///
    /// OpenUSD: `SdfSchemaBase::GetFallback`.
    #[must_use]
    pub fn fallback(&self, tokens: &TokenInterner) -> Option<FieldValue> {
        match self.field {
            Field::Builtin(field) => field.fallback(tokens),
            Field::Registered(definition) => definition.fallback(),
        }
    }

    /// Who registers the field.
    #[must_use]
    pub fn origin(&self) -> MetadataOrigin {
        match self.field {
            Field::Builtin(field) => field.origin,
            Field::Registered(_) => MetadataOrigin::Registry,
        }
    }

    /// The registry's declaration of the field, `None` for a built-in one.
    #[must_use]
    pub fn definition(&self) -> Option<&'a MetadataDefinition> {
        match self.field {
            Field::Builtin(_) => None,
            Field::Registered(definition) => Some(definition),
        }
    }
}

/// Why a metadata field cannot be authored on a spec, or a value cannot be
/// authored as it: the field, the kind of spec, and the reason.
///
/// OpenUSD: `UsdObject::SetMetadata` and `UsdStage::SetMetadata`
/// (`pxr/usd/usd/stage.cpp`), whose Python binding converts a value to the
/// field's type (`UsdPythonToMetadataValue`); layerstack takes a value of
/// the type.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MetadataEditError {
    /// The field's name.
    pub field: Arc<str>,
    /// The kind of spec it is authored on.
    pub target: MetadataTarget,
    /// Why.
    pub reason: MetadataEditReason,
}

/// Why a metadata edit is refused ([`MetadataEditError`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetadataEditReason {
    /// No field of the name is built in or registered (OpenUSD:
    /// "Unregistered metadata key").
    Unregistered,
    /// The field is registered, but not for this kind of spec (OpenUSD:
    /// "… is not registered as valid metadata for spec type …").
    NotForSpec,
    /// The name is a field of the spec with an edit of its own
    /// ([`is_spec_field`]); `comment` is not refused.
    SpecField,
    /// The value is not of the field's type (OpenUSD: "Invalid value …
    /// Expected type …").
    ValueType {
        /// The field's type.
        expected: MetadataType,
    },
}

impl MetadataEditError {
    /// OpenUSD's message for the refusal, without the layer it names, and
    /// for a value of another type without the value and its C++ type.
    #[must_use]
    pub fn openusd_message(&self) -> alloc::string::String {
        let field = &self.field;
        match self.reason {
            MetadataEditReason::Unregistered => format!("Unregistered metadata key: {field}"),
            MetadataEditReason::NotForSpec if self.target == MetadataTarget::Layer => {
                format!("Metadata '{field}' is not registered as valid Layer metadata")
            }
            MetadataEditReason::NotForSpec => format!(
                "Cannot set metadata. '{field}' is not registered as valid metadata for spec type {}.",
                self.target.spec_type_name()
            ),
            MetadataEditReason::SpecField => {
                format!("\"{field}\" is registered as a non-metadata field")
            }
            MetadataEditReason::ValueType { expected } => {
                format!("Invalid value for key '{field}'. Expected type '{expected}'")
            }
        }
    }
}

impl fmt::Display for MetadataEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let field = &self.field;
        let target = self.target;
        match self.reason {
            MetadataEditReason::Unregistered => {
                write!(f, "no metadata field `{field}` is registered")
            }
            MetadataEditReason::NotForSpec => {
                write!(
                    f,
                    "metadata field `{field}` is not registered for {target} specs"
                )
            }
            MetadataEditReason::SpecField => {
                write!(
                    f,
                    "`{field}` is a field of {target} specs with its own edit"
                )
            }
            MetadataEditReason::ValueType { expected } => {
                write!(f, "metadata field `{field}` takes a `{expected}` value")
            }
        }
    }
}

impl core::error::Error for MetadataEditError {}

impl MetadataField<'_> {
    /// The field `name` as authored on a spec of `target`, with `value`
    /// (`None` to clear it), checked as OpenUSD checks a `SetMetadata`
    /// call: the field must be built in or in `registry` and registered
    /// for `target`, not a field of the spec with an edit of its own, and
    /// the value must be of its type ([`MetadataType::accepts`]).
    ///
    /// # Errors
    ///
    /// [`MetadataEditError`], naming the field and why.
    pub fn check_edit<'r>(
        name: &str,
        target: MetadataTarget,
        value: Option<&FieldValue>,
        registry: Option<&'r SchemaRegistry>,
        tokens: &TokenInterner,
    ) -> Result<MetadataField<'r>, MetadataEditError> {
        let error = |reason| MetadataEditError {
            field: Arc::from(name),
            target,
            reason,
        };
        // `comment`, a field of every spec but not metadata, holds a
        // string a generic edit may author, as `SetMetadata` may.
        if name == "comment" {
            let string = MetadataType::Value(ValueType::String);
            return match value {
                Some(value) if !string.accepts(value) => {
                    Err(error(MetadataEditReason::ValueType { expected: string }))
                }
                _ => Ok(MetadataField {
                    field: Field::Builtin(&builtin::COMMENT),
                }),
            };
        }
        if is_spec_field(name, target) {
            return Err(error(MetadataEditReason::SpecField));
        }
        let Some(field) = MetadataField::lookup(name, registry, tokens) else {
            // A field of another kind of spec (a relationship's `typeName`)
            // is registered, if not for this one.
            let elsewhere = [
                MetadataTarget::Layer,
                MetadataTarget::Prim,
                MetadataTarget::Attribute,
                MetadataTarget::Relationship,
            ]
            .into_iter()
            .any(|other| is_spec_field(name, other));
            return Err(error(if elsewhere {
                MetadataEditReason::NotForSpec
            } else {
                MetadataEditReason::Unregistered
            }));
        };
        if !field.applies_to(target) {
            return Err(error(MetadataEditReason::NotForSpec));
        }
        if let Some(value) = value
            && !field.value_type().accepts(value)
        {
            return Err(error(MetadataEditReason::ValueType {
                expected: field.value_type(),
            }));
        }
        Ok(field)
    }
}

/// Whether `name` is a field of a spec of `target` that is not metadata:
/// one of the spec's own fields (`specifier`, `default`, `targetPaths`,
/// `subLayers`, …) or a metadata field with a route of its own (`typeName`,
/// `payload`), which edits author through their own calls. `permission`,
/// an enum of Sdf's own that no metadata type holds, is a field of none of
/// these: it is kept as the token it is written as.
///
/// OpenUSD: the `Field`s of `_RegisterStandardFields`' spec definitions
/// (`"…" is registered as a non-metadata field`).
#[must_use]
pub fn is_spec_field(name: &str, target: MetadataTarget) -> bool {
    builtin::is_spec_field(name, target)
}

/// A declaration of a metadata field that conflicts with one already
/// registered. The original declaration is retained.
///
/// OpenUSD: `"…" is already a registered field`
/// (`SdfSchemaBase::_UpdateMetadataFromPlugins`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MetadataConflict {
    /// The conflicting field's name.
    pub name: TokenId,
    /// Who registers the field already.
    pub registered_by: MetadataOrigin,
}

impl fmt::Display for MetadataConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "conflicting metadata declaration for {:?}, which {} registers already",
            self.name, self.registered_by
        )
    }
}
impl core::error::Error for MetadataConflict {}

/// Whether a declaration of `name` matches the built-in field of that
/// name, as the registry's tables of usd-core's own plugins declare it
/// again; `Err` with the origin when it conflicts, `Ok(false)` when there
/// is no such built-in field.
pub(crate) fn check_builtin(
    definition: &MetadataDefinition,
    name: &str,
) -> Result<bool, MetadataOrigin> {
    let Some(field) = builtin::find(name) else {
        return Ok(false);
    };
    let same = matches!(field.origin, MetadataOrigin::Plugin(_))
        && field.value_type == definition.value_type
        && {
            let mut targets = definition.targets.clone();
            sort::sort_unstable(&mut targets);
            targets.dedup();
            targets == field.targets
        }
        && field.default_matches(definition.default.as_ref());
    if same { Ok(true) } else { Err(field.origin) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SchemaRegistry, TokenInterner};

    fn definition(tokens: &TokenInterner, name: &str, ty: &str) -> MetadataDefinition {
        MetadataDefinition {
            name: tokens.intern(name),
            value_type: MetadataType::parse(ty).unwrap(),
            targets: alloc::vec![MetadataTarget::Prim, MetadataTarget::Attribute],
            default: None,
            display_group: Arc::from(""),
            documentation: Arc::from(""),
        }
    }

    #[test]
    fn identical_registration_is_idempotent_and_conflicts_preserve_the_first() {
        let tokens = TokenInterner::default();
        let name = tokens.intern("customWeight");
        let mut definition = definition(&tokens, "customWeight", "float");
        definition.default = Some(Value::Float(0.0));
        definition.documentation = Arc::from("A weight");
        let mut builder = SchemaRegistry::builder();
        builder
            .register_metadata(definition.clone(), &tokens)
            .unwrap();
        let mut reordered = definition.clone();
        reordered.targets.reverse();
        reordered.targets.push(MetadataTarget::Prim);
        builder.register_metadata(reordered, &tokens).unwrap();
        let mut conflicting = definition.clone();
        conflicting.default = Some(Value::Float(1.0));
        assert_eq!(
            builder.register_metadata(conflicting, &tokens).unwrap_err(),
            MetadataConflict {
                name,
                registered_by: MetadataOrigin::Registry
            }
        );
        let registry = builder.build(&tokens);
        assert_eq!(registry.metadata_definitions().count(), 1);
        assert_eq!(registry.metadata(name), Some(&definition));
        let field = registry.metadata_field("customWeight", &tokens).unwrap();
        assert!(field.applies_to(MetadataTarget::Prim));
        assert!(
            field.applies_to(MetadataTarget::Variant),
            "prims imply variants"
        );
        assert!(!field.applies_to(MetadataTarget::Layer));
        assert_eq!(field.origin(), MetadataOrigin::Registry);
    }

    #[test]
    fn built_in_fields_refuse_redeclaration_unless_a_bundled_plugin_declares_them_alike() {
        let tokens = TokenInterner::default();
        let mut builder = SchemaRegistry::builder();
        // Sdf's own field: always refused.
        let documentation = definition(&tokens, "documentation", "string");
        assert_eq!(
            builder
                .register_metadata(documentation, &tokens)
                .unwrap_err()
                .registered_by,
            MetadataOrigin::Sdf
        );
        // usdGeom's field, declared as usdGeom declares it: accepted.
        let mut interpolation = definition(&tokens, "interpolation", "token");
        interpolation.targets = alloc::vec![MetadataTarget::Attribute];
        interpolation.default = Some(Value::Token(tokens.intern("constant")));
        builder
            .register_metadata(interpolation.clone(), &tokens)
            .unwrap();
        // Declared otherwise: refused.
        interpolation.targets.push(MetadataTarget::Prim);
        assert_eq!(
            builder
                .register_metadata(interpolation, &tokens)
                .unwrap_err()
                .registered_by,
            MetadataOrigin::Plugin("usdGeom")
        );
    }

    #[test]
    fn types_parse_as_plug_info_names_them_and_accept_their_storage() {
        assert_eq!(
            MetadataType::parse("color3f"),
            Some(MetadataType::Value(ValueType::Color3f))
        );
        assert_eq!(
            MetadataType::parse("color3f").unwrap().storage(),
            MetadataType::Value(ValueType::Float3)
        );
        for name in ["path", "Float", "dictionary[]", "opaque[]", "pathlistop"] {
            assert_eq!(MetadataType::parse(name), None, "{name}");
        }
        for name in [
            "token[]",
            "dictionary",
            "uint64listop",
            "matrix4d",
            "timecode",
        ] {
            assert_eq!(MetadataType::parse(name).unwrap().name(), name);
        }
        let float3 = MetadataType::parse("color3f").unwrap();
        assert!(float3.accepts(&FieldValue::Value(Value::Vec3f([1.0, 2.0, 3.0]))));
        assert!(!float3.accepts(&FieldValue::Value(Value::Vec3d([1.0, 2.0, 3.0]))));
        let tokens = MetadataType::parse("token[]").unwrap();
        assert!(tokens.accepts(&FieldValue::Value(Value::Array(Vec::new()))));
        assert!(!tokens.accepts(&FieldValue::Value(Value::Token(TokenId::EMPTY))));
        let list = MetadataType::parse("int64listop").unwrap();
        assert!(list.accepts(&FieldValue::Int64ListOp(Box::default())));
        assert!(!list.accepts(&FieldValue::IntListOp(Box::default())));
        let opaque = MetadataType::parse("opaque").unwrap();
        assert!(!opaque.accepts(&FieldValue::Value(Value::Null)));
        assert_eq!(opaque.fallback(), None);
    }

    #[test]
    fn fallbacks_are_the_value_type_registry_defaults() {
        assert_eq!(
            MetadataType::parse("quatf").unwrap().fallback(),
            Some(FieldValue::Value(Value::Quatf([0.0, 0.0, 0.0, 1.0])))
        );
        let Some(FieldValue::Value(Value::Matrix2d(m))) =
            MetadataType::parse("matrix2d").unwrap().fallback()
        else {
            panic!("a matrix fallback");
        };
        assert_eq!(*m, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            MetadataType::parse("dictionary").unwrap().fallback(),
            Some(FieldValue::Value(Value::Dictionary(Vec::new())))
        );
    }

    #[test]
    fn edits_are_checked_against_the_field_and_its_kinds_of_spec() {
        let tokens = TokenInterner::default();
        let check = |name: &str, target, value: Value| {
            MetadataField::check_edit(name, target, Some(&FieldValue::Value(value)), None, &tokens)
                .map(|_| ())
                .map_err(|error| error.reason)
        };
        let token = Value::Token(tokens.intern("vertex"));
        assert_eq!(
            check("interpolation", MetadataTarget::Attribute, token.clone()),
            Ok(())
        );
        assert_eq!(
            check("interpolation", MetadataTarget::Prim, token.clone()),
            Err(MetadataEditReason::NotForSpec)
        );
        assert_eq!(
            check(
                "interpolation",
                MetadataTarget::Attribute,
                Value::string("vertex")
            ),
            Err(MetadataEditReason::ValueType {
                expected: MetadataType::Value(ValueType::Token)
            })
        );
        assert_eq!(
            check("hostWeight", MetadataTarget::Prim, Value::Float(1.0)),
            Err(MetadataEditReason::Unregistered)
        );
        assert_eq!(
            check("default", MetadataTarget::Attribute, Value::Float(1.0)),
            Err(MetadataEditReason::SpecField)
        );
        assert_eq!(
            check("typeName", MetadataTarget::Relationship, token),
            Err(MetadataEditReason::NotForSpec),
            "a field of other kinds of spec"
        );
        assert_eq!(
            check("comment", MetadataTarget::Prim, Value::string("note")),
            Ok(())
        );
    }

    #[test]
    fn builtin_fields_carry_their_spec_kinds_and_fallbacks() {
        let tokens = TokenInterner::default();
        let up_axis = MetadataField::builtin("upAxis").unwrap();
        assert_eq!(up_axis.targets(), [MetadataTarget::Layer]);
        assert_eq!(
            up_axis.fallback(&tokens),
            Some(FieldValue::Value(Value::Token(tokens.intern("Y"))))
        );
        assert_eq!(up_axis.origin(), MetadataOrigin::Plugin("usdGeom"));
        let weight = MetadataField::builtin("weight").unwrap();
        assert!(weight.applies_to(MetadataTarget::Attribute));
        assert!(!weight.applies_to(MetadataTarget::Prim));
        assert!(MetadataField::builtin("comment").is_none());
        assert!(is_spec_field("comment", MetadataTarget::Prim));
        assert!(is_spec_field("typeName", MetadataTarget::Prim));
        assert!(!is_spec_field("kind", MetadataTarget::Prim));
    }
}
