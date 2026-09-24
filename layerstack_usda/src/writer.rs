// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Deterministic USDA serialization of an explicitly authored document.
//!
//! The composition model ([`layerstack::Layer`]) is built for *reading*: it
//! keeps what composition needs and drops authoring details such as
//! `custom`/`uniform` qualifiers, attribute metadata (e.g. primvar
//! `interpolation`) and most layer metadata. Formatting a composed `Value`
//! therefore cannot produce a faithful file. This module instead serializes
//! a small, owned *authored* representation — [`Document`], [`Prim`],
//! [`Attribute`], [`Metadatum`] and [`Value`] — that states every one of
//! those details explicitly.
//!
//! Output is deterministic: the same document always produces byte-identical
//! text. Items are written in the order they appear in the document (nothing
//! is sorted or deduplicated behind the caller's back); invalid input is
//! rejected with a [`WriteError`] rather than silently repaired.
//!
//! Scope: prims, attributes with default values, and metadata. Time samples,
//! relationships, connections, composition arcs and variant sets are not yet
//! representable.
//!
//! # Example
//!
//! ```
//! use layerstack_usda::writer::{Attribute, Document, Metadatum, Prim, Value};
//!
//! let mut root = Prim::def("Xform", "Root");
//! root.attributes.push(
//!     Attribute::new("xformOpOrder", "token[]", Value::TokenArray(vec![
//!         "xformOp:translate".into(),
//!     ]))
//!     .uniform(),
//! );
//! root.attributes.push(Attribute::new(
//!     "xformOp:translate",
//!     "double3",
//!     Value::Double3([0.0, 0.0, 1.5]),
//! ));
//!
//! let mut doc = Document::new();
//! doc.default_prim = Some("Root".into());
//! doc.metadata.push(Metadatum::new("upAxis", Value::Token("Z".into())));
//! doc.prims.push(root);
//!
//! let text = doc.to_usda()?;
//! assert!(text.starts_with("#usda 1.0\n(\n    defaultPrim = \"Root\"\n"));
//! assert!(text.contains("uniform token[] xformOpOrder = [\"xformOp:translate\"]"));
//! # Ok::<(), layerstack_usda::writer::WriteError>(())
//! ```
//!
//! Spec: AOUSD Core §16.2 (USDA grammar), §7.3.3 (names), §7.6 (core
//! metadata fields), §6.2–§6.5 (value types and semantic aliases).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

pub use crate::ast::Specifier;

const INDENT: &str = "    ";

// ── Document model ──────────────────────────────────────────────────────

/// A USDA layer to be written: layer metadata plus root prims.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    /// The `defaultPrim` layer metadata field. When set, it must name one of
    /// [`Self::prims`].
    ///
    /// Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim`).
    pub default_prim: Option<String>,
    /// Further layer metadata (e.g. `upAxis`, `metersPerUnit`, `doc`),
    /// written after `defaultPrim` in this order.
    pub metadata: Vec<Metadatum>,
    /// Root prims, in order.
    pub prims: Vec<Prim>,
}

impl Document {
    /// Creates an empty document.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serializes the document as USDA text.
    ///
    /// # Errors
    ///
    /// Returns a [`WriteError`] when a name is not a valid identifier, a
    /// sibling name repeats, an attribute's type name is unknown or does not
    /// match its value, an asset path cannot be quoted, or `defaultPrim` does
    /// not name a root prim. Nothing is written in that case.
    pub fn to_usda(&self) -> Result<String, WriteError> {
        let mut out = String::new();
        self.write_usda(&mut out)?;
        Ok(out)
    }

    /// Serializes the document as USDA text into `out`.
    ///
    /// Validation runs before anything is written, so on error `out` is left
    /// unchanged.
    ///
    /// # Errors
    ///
    /// See [`Self::to_usda`].
    pub fn write_usda(&self, out: &mut String) -> Result<(), WriteError> {
        self.validate()?;
        let mut w = Writer { out };
        w.document(self);
        Ok(())
    }

    fn validate(&self) -> Result<(), WriteError> {
        let mut keys: Vec<&str> = Vec::new();
        if self.default_prim.is_some() {
            keys.push("defaultPrim");
        }
        validate_metadata(&self.metadata, &mut keys, "/")?;
        if let Some(name) = &self.default_prim
            && !self.prims.iter().any(|p| &p.name == name)
        {
            return Err(WriteError::DefaultPrimNotFound { name: name.clone() });
        }
        let mut names: Vec<&str> = Vec::new();
        for prim in &self.prims {
            prim.validate("", &mut names)?;
        }
        Ok(())
    }
}

/// A prim spec with its metadata, attributes and child prims.
///
/// Spec: AOUSD Core §7.3.5 (prim specs), §16.2.17 (prim spec grammar).
#[derive(Clone, Debug, PartialEq)]
pub struct Prim {
    /// `def`, `over` or `class`.
    pub specifier: Specifier,
    /// Schema type name (e.g. `Mesh`), or `None` for a typeless prim.
    pub type_name: Option<String>,
    /// Prim name; must be a valid identifier (§7.3.3).
    pub name: String,
    /// Prim metadata (e.g. `kind`), in order.
    pub metadata: Vec<Metadatum>,
    /// Attributes, in order.
    pub attributes: Vec<Attribute>,
    /// Child prims, in order.
    pub children: Vec<Self>,
}

impl Prim {
    /// Creates a `def` prim of schema type `type_name`.
    pub fn def(type_name: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(Specifier::Def, Some(type_name.into()), name)
    }

    /// Creates a prim with an explicit specifier and optional type name.
    pub fn new(specifier: Specifier, type_name: Option<String>, name: impl Into<String>) -> Self {
        Self {
            specifier,
            type_name,
            name: name.into(),
            metadata: Vec::new(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    fn validate<'a>(&'a self, parent: &str, siblings: &mut Vec<&'a str>) -> Result<(), WriteError> {
        let path = if parent.is_empty() {
            alloc::format!("/{}", self.name)
        } else {
            alloc::format!("{parent}/{}", self.name)
        };
        if !is_identifier(&self.name) {
            return Err(WriteError::InvalidName {
                path,
                name: self.name.clone(),
            });
        }
        if siblings.contains(&self.name.as_str()) {
            return Err(WriteError::Duplicate { path });
        }
        siblings.push(&self.name);
        if let Some(type_name) = &self.type_name
            && !is_identifier(type_name)
        {
            return Err(WriteError::InvalidName {
                path,
                name: type_name.clone(),
            });
        }
        validate_metadata(&self.metadata, &mut Vec::new(), &path)?;
        let mut attribute_names: Vec<&str> = Vec::new();
        for attribute in &self.attributes {
            let attr_path = alloc::format!("{path}.{}", attribute.name);
            if !is_property_name(&attribute.name) {
                return Err(WriteError::InvalidName {
                    path: attr_path,
                    name: attribute.name.clone(),
                });
            }
            if attribute_names.contains(&attribute.name.as_str()) {
                return Err(WriteError::Duplicate { path: attr_path });
            }
            attribute_names.push(&attribute.name);
            attribute.validate(&attr_path)?;
        }
        let mut child_names: Vec<&str> = Vec::new();
        for child in &self.children {
            child.validate(&path, &mut child_names)?;
        }
        Ok(())
    }
}

/// Attribute variability (the `uniform` qualifier).
///
/// Spec: AOUSD Core §16.2.16.1 (attribute declarations), §16.3.10.29.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variability {
    /// May vary over time (no qualifier).
    #[default]
    Varying,
    /// Written with the `uniform` qualifier.
    Uniform,
}

/// An attribute spec: declaration, optional default value and metadata.
///
/// Spec: AOUSD Core §16.2.16 (attribute specs).
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    /// Namespaced property name (e.g. `primvars:st`).
    pub name: String,
    /// Declared value type name, including a trailing `[]` for arrays (e.g.
    /// `texCoord2f[]`). Semantic aliases such as `point3f`, `normal3f` and
    /// `color3f` are accepted and preserved.
    pub type_name: String,
    /// Whether the declaration carries the `custom` keyword.
    pub custom: bool,
    /// Whether the declaration carries the `uniform` keyword.
    pub variability: Variability,
    /// Default value; `None` writes a bare declaration.
    pub value: Option<Value>,
    /// Attribute metadata (e.g. `interpolation`, `elementSize`), in order.
    pub metadata: Vec<Metadatum>,
}

impl Attribute {
    /// Creates a varying, non-custom attribute with a default value.
    pub fn new(name: impl Into<String>, type_name: impl Into<String>, value: Value) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            custom: false,
            variability: Variability::Varying,
            value: Some(value),
            metadata: Vec::new(),
        }
    }

    /// Marks the attribute `uniform`.
    #[must_use]
    pub fn uniform(mut self) -> Self {
        self.variability = Variability::Uniform;
        self
    }

    /// Marks the attribute `custom`.
    #[must_use]
    pub fn custom(mut self) -> Self {
        self.custom = true;
        self
    }

    /// Appends an attribute metadata entry.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.push(Metadatum::new(key, value));
        self
    }

    fn validate(&self, path: &str) -> Result<(), WriteError> {
        let Some(declared) = parse_type_name(&self.type_name) else {
            return Err(WriteError::UnknownType {
                path: path.into(),
                type_name: self.type_name.clone(),
            });
        };
        if let Some(value) = &self.value {
            if value.shape() != Some(declared) {
                return Err(WriteError::TypeMismatch {
                    path: path.into(),
                    type_name: self.type_name.clone(),
                });
            }
            validate_value(value, path)?;
        }
        validate_metadata(&self.metadata, &mut Vec::new(), path)
    }
}

/// A `key = value` metadata entry on a layer, prim or attribute.
///
/// Spec: AOUSD Core §7.4 (metadata fields), §16.2.15 (common metadata).
#[derive(Clone, Debug, PartialEq)]
pub struct Metadatum {
    /// Metadata field name; must be a plain identifier.
    pub key: String,
    /// Field value. Tokens and strings are both written quoted, as USDA
    /// metadata syntax requires.
    pub value: Value,
}

impl Metadatum {
    /// Creates a metadata entry.
    pub fn new(key: impl Into<String>, value: Value) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }
}

/// An authored value.
///
/// Numeric element types are explicit so the declared attribute type can be
/// checked against the value (§6.5.1, type and alias agreement). Arrays of
/// small vectors are stored packed for large mesh buffers.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `bool`.
    Bool(bool),
    /// `int`.
    Int(i32),
    /// `uint`.
    UInt(u32),
    /// `int64`.
    Int64(i64),
    /// `float`.
    Float(f32),
    /// `double` (also accepted for `timecode`).
    Double(f64),
    /// `string`.
    String(String),
    /// `token`.
    Token(String),
    /// `asset`; the path may not contain `@` or line breaks.
    Asset(String),
    /// `float2` and its semantic aliases (e.g. `texCoord2f`).
    Float2([f32; 2]),
    /// `float3` and its semantic aliases (e.g. `point3f`, `color3f`).
    Float3([f32; 3]),
    /// `float4` and its semantic aliases (e.g. `color4f`).
    Float4([f32; 4]),
    /// `double2` and its semantic aliases.
    Double2([f64; 2]),
    /// `double3` and its semantic aliases.
    Double3([f64; 3]),
    /// `double4` and its semantic aliases.
    Double4([f64; 4]),
    /// `int2`.
    Int2([i32; 2]),
    /// `int3`.
    Int3([i32; 3]),
    /// `int4`.
    Int4([i32; 4]),
    /// `matrix4d` (or `frame4d`), row-major with the translation in the
    /// last row, as USD authors it (row vectors, §6.3).
    Matrix4d([[f64; 4]; 4]),
    /// `bool[]`.
    BoolArray(Vec<bool>),
    /// `int[]`.
    IntArray(Vec<i32>),
    /// `uint[]`.
    UIntArray(Vec<u32>),
    /// `int64[]`.
    Int64Array(Vec<i64>),
    /// `float[]`.
    FloatArray(Vec<f32>),
    /// `double[]`.
    DoubleArray(Vec<f64>),
    /// `string[]`.
    StringArray(Vec<String>),
    /// `token[]`.
    TokenArray(Vec<String>),
    /// `asset[]`.
    AssetArray(Vec<String>),
    /// `float2[]` and aliases (e.g. `texCoord2f[]`).
    Float2Array(Vec<[f32; 2]>),
    /// `float3[]` and aliases (e.g. `point3f[]`, `normal3f[]`).
    Float3Array(Vec<[f32; 3]>),
    /// `float4[]` and aliases.
    Float4Array(Vec<[f32; 4]>),
    /// `double2[]` and aliases.
    Double2Array(Vec<[f64; 2]>),
    /// `double3[]` and aliases.
    Double3Array(Vec<[f64; 3]>),
    /// `double4[]` and aliases.
    Double4Array(Vec<[f64; 4]>),
    /// `int2[]`.
    Int2Array(Vec<[i32; 2]>),
    /// `int3[]`.
    Int3Array(Vec<[i32; 3]>),
    /// `int4[]`.
    Int4Array(Vec<[i32; 4]>),
    /// A dictionary: string keys to values, written in order with each
    /// entry's canonical type name (§6.6.2).
    ///
    /// Dictionaries are *metadata* values (e.g. `customData`). They are not
    /// an attribute value type in USD (`Sdf` registers no `dictionary`
    /// attribute type), so an [`Attribute`] cannot hold or declare one.
    Dictionary(Vec<(String, Self)>),
}

// ── Errors ──────────────────────────────────────────────────────────────

/// Why a [`Document`] could not be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// A prim name, property name, type name or metadata key is not a valid
    /// identifier (§7.3.3).
    InvalidName {
        /// Path of the offending object.
        path: String,
        /// The rejected name.
        name: String,
    },
    /// Two siblings (prims, attributes or metadata keys on one owner) share
    /// a name.
    Duplicate {
        /// Path of the second occurrence (metadata keys appended after `#`).
        path: String,
    },
    /// An attribute's type name is not a USD attribute value type (this
    /// includes `dictionary`, which is only valid for metadata).
    UnknownType {
        /// Attribute path.
        path: String,
        /// The rejected type name.
        type_name: String,
    },
    /// An attribute's value does not have the element type, arity or
    /// array-ness its declared type requires.
    TypeMismatch {
        /// Attribute path.
        path: String,
        /// The declared type name.
        type_name: String,
    },
    /// An asset path contains `@` or a line break and cannot be quoted.
    InvalidAssetPath {
        /// Path of the owning object.
        path: String,
        /// The rejected asset path.
        asset: String,
    },
    /// A string, token or dictionary key contains NUL, which USD text
    /// cannot represent.
    NulInString {
        /// Path of the owning object.
        path: String,
    },
    /// `defaultPrim` does not name a root prim of the document.
    DefaultPrimNotFound {
        /// The `defaultPrim` value.
        name: String,
    },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { path, name } => write!(f, "{path}: invalid identifier {name:?}"),
            Self::Duplicate { path } => write!(f, "{path}: duplicate name"),
            Self::UnknownType { path, type_name } => {
                write!(f, "{path}: {type_name:?} is not a USD attribute value type")
            }
            Self::TypeMismatch { path, type_name } => {
                write!(
                    f,
                    "{path}: value does not match declared type {type_name:?}"
                )
            }
            Self::InvalidAssetPath { path, asset } => {
                write!(f, "{path}: asset path {asset:?} cannot be quoted")
            }
            Self::NulInString { path } => write!(f, "{path}: string contains NUL"),
            Self::DefaultPrimNotFound { name } => {
                write!(f, "defaultPrim {name:?} does not name a root prim")
            }
        }
    }
}

impl core::error::Error for WriteError {}

// ── Validation helpers ──────────────────────────────────────────────────

/// `([XID_Start] / '_') [XID_Continue]*` — §7.3.3 (`PrimName`), with the
/// Unicode XID tables the lexer also uses, so written names always re-parse
/// here and in OpenUSD (`pxr/usd/sdf/path.cpp`, `_IsValidIdentifier`).
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(crate::ident::is_start) && chars.all(crate::ident::is_continue)
}

/// Colon-joined identifiers — §7.3.3 (`PropertyName`).
fn is_property_name(name: &str) -> bool {
    name.split(':').all(is_identifier)
}

fn validate_metadata<'a>(
    entries: &'a [Metadatum],
    seen: &mut Vec<&'a str>,
    path: &str,
) -> Result<(), WriteError> {
    for entry in entries {
        if !is_identifier(&entry.key) {
            return Err(WriteError::InvalidName {
                path: path.into(),
                name: entry.key.clone(),
            });
        }
        if seen.contains(&entry.key.as_str()) {
            return Err(WriteError::Duplicate {
                path: alloc::format!("{path}#{}", entry.key),
            });
        }
        seen.push(&entry.key);
        validate_value(&entry.value, path)?;
    }
    Ok(())
}

fn validate_value(value: &Value, path: &str) -> Result<(), WriteError> {
    let bad_asset = |asset: &String| {
        (asset.contains('@') || asset.contains('\n') || asset.contains('\r')).then(|| {
            WriteError::InvalidAssetPath {
                path: path.into(),
                asset: asset.clone(),
            }
        })
    };
    // USD strings cannot carry NUL: OpenUSD truncates at it, silently
    // dropping the rest of the value.
    let bad_text = |text: &String| {
        text.contains('\0')
            .then(|| WriteError::NulInString { path: path.into() })
    };
    match value {
        Value::String(text) | Value::Token(text) => bad_text(text).map_or(Ok(()), Err),
        Value::StringArray(texts) | Value::TokenArray(texts) => {
            texts.iter().find_map(bad_text).map_or(Ok(()), Err)
        }
        Value::Asset(asset) => bad_asset(asset).map_or(Ok(()), Err),
        Value::AssetArray(assets) => assets.iter().find_map(bad_asset).map_or(Ok(()), Err),
        Value::Dictionary(entries) => {
            for (key, v) in entries {
                if let Some(error) = bad_text(key) {
                    return Err(error);
                }
                validate_value(v, path)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// ── Type table ──────────────────────────────────────────────────────────

/// Element storage of a value type, for type/value agreement checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Elem {
    Bool,
    Int,
    UInt,
    Int64,
    Float,
    Double,
    String,
    Token,
    Asset,
    Dictionary,
    /// A known type this writer has no [`Value`] variant for (e.g. `half`,
    /// quaternions). It can be declared but not given a value.
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    elem: Elem,
    arity: u8,
    array: bool,
}

/// Maps a USD attribute value type name to its storage shape.
///
/// The accepted names are a subset of the OpenUSD `Sdf` value type registry
/// (`pxr/usd/sdf/schema.cpp`, `_RegisterStandardTypes`; `pxr/usd/sdf/types.h`,
/// `SDF_VALUE_TYPES` plus the role aliases), each optionally with `[]`.
/// Deliberately excluded: `dictionary` (metadata only, never an attribute
/// type), `opaque`/`group`/`pathExpression`, and the legacy capitalized
/// aliases (`Vec3f`, `PointFloat`, ...). Registered types without a
/// [`Value`] variant (e.g. `half`, quaternions) may be declared without a
/// default.
///
/// Spec: AOUSD Core §6.2 (scalar types), §6.3 (dimensioned types), §6.5
/// (semantic aliases).
fn parse_type_name(type_name: &str) -> Option<Shape> {
    let (base, array) = match type_name.strip_suffix("[]") {
        Some(base) => (base, true),
        None => (type_name, false),
    };
    let (elem, arity) = match base {
        "bool" => (Elem::Bool, 1),
        "int" => (Elem::Int, 1),
        "uint" => (Elem::UInt, 1),
        "int64" => (Elem::Int64, 1),
        "float" => (Elem::Float, 1),
        "double" | "timecode" => (Elem::Double, 1),
        "string" => (Elem::String, 1),
        "token" => (Elem::Token, 1),
        "asset" => (Elem::Asset, 1),
        "int2" => (Elem::Int, 2),
        "int3" => (Elem::Int, 3),
        "int4" => (Elem::Int, 4),
        "float2" | "texCoord2f" => (Elem::Float, 2),
        "float3" | "point3f" | "normal3f" | "vector3f" | "color3f" | "texCoord3f" => {
            (Elem::Float, 3)
        }
        "float4" | "color4f" => (Elem::Float, 4),
        "double2" | "texCoord2d" => (Elem::Double, 2),
        "double3" | "point3d" | "normal3d" | "vector3d" | "color3d" | "texCoord3d" => {
            (Elem::Double, 3)
        }
        "double4" | "color4d" => (Elem::Double, 4),
        "matrix4d" | "frame4d" => (Elem::Double, 16),
        "uchar" | "uint64" | "half" | "half2" | "half3" | "half4" | "texCoord2h" | "texCoord3h"
        | "point3h" | "normal3h" | "vector3h" | "color3h" | "color4h" | "matrix2d" | "matrix3d"
        | "quath" | "quatf" | "quatd" => (Elem::Unsupported, 0),
        _ => return None,
    };
    Some(Shape { elem, arity, array })
}

impl Value {
    fn shape(&self) -> Option<Shape> {
        let (elem, arity, array) = match self {
            Self::Bool(_) => (Elem::Bool, 1, false),
            Self::Int(_) => (Elem::Int, 1, false),
            Self::UInt(_) => (Elem::UInt, 1, false),
            Self::Int64(_) => (Elem::Int64, 1, false),
            Self::Float(_) => (Elem::Float, 1, false),
            Self::Double(_) => (Elem::Double, 1, false),
            Self::String(_) => (Elem::String, 1, false),
            Self::Token(_) => (Elem::Token, 1, false),
            Self::Asset(_) => (Elem::Asset, 1, false),
            Self::Float2(_) => (Elem::Float, 2, false),
            Self::Float3(_) => (Elem::Float, 3, false),
            Self::Float4(_) => (Elem::Float, 4, false),
            Self::Double2(_) => (Elem::Double, 2, false),
            Self::Double3(_) => (Elem::Double, 3, false),
            Self::Double4(_) => (Elem::Double, 4, false),
            Self::Int2(_) => (Elem::Int, 2, false),
            Self::Int3(_) => (Elem::Int, 3, false),
            Self::Int4(_) => (Elem::Int, 4, false),
            Self::Matrix4d(_) => (Elem::Double, 16, false),
            Self::BoolArray(_) => (Elem::Bool, 1, true),
            Self::IntArray(_) => (Elem::Int, 1, true),
            Self::UIntArray(_) => (Elem::UInt, 1, true),
            Self::Int64Array(_) => (Elem::Int64, 1, true),
            Self::FloatArray(_) => (Elem::Float, 1, true),
            Self::DoubleArray(_) => (Elem::Double, 1, true),
            Self::StringArray(_) => (Elem::String, 1, true),
            Self::TokenArray(_) => (Elem::Token, 1, true),
            Self::AssetArray(_) => (Elem::Asset, 1, true),
            Self::Float2Array(_) => (Elem::Float, 2, true),
            Self::Float3Array(_) => (Elem::Float, 3, true),
            Self::Float4Array(_) => (Elem::Float, 4, true),
            Self::Double2Array(_) => (Elem::Double, 2, true),
            Self::Double3Array(_) => (Elem::Double, 3, true),
            Self::Double4Array(_) => (Elem::Double, 4, true),
            Self::Int2Array(_) => (Elem::Int, 2, true),
            Self::Int3Array(_) => (Elem::Int, 3, true),
            Self::Int4Array(_) => (Elem::Int, 4, true),
            Self::Dictionary(_) => (Elem::Dictionary, 1, false),
        };
        Some(Shape { elem, arity, array })
    }

    /// The canonical (alias-free) USD type name of this value, as used for
    /// typed dictionary entries.
    pub fn canonical_type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::UInt(_) => "uint",
            Self::Int64(_) => "int64",
            Self::Float(_) => "float",
            Self::Double(_) => "double",
            Self::String(_) => "string",
            Self::Token(_) => "token",
            Self::Asset(_) => "asset",
            Self::Float2(_) => "float2",
            Self::Float3(_) => "float3",
            Self::Float4(_) => "float4",
            Self::Double2(_) => "double2",
            Self::Double3(_) => "double3",
            Self::Double4(_) => "double4",
            Self::Int2(_) => "int2",
            Self::Int3(_) => "int3",
            Self::Int4(_) => "int4",
            Self::Matrix4d(_) => "matrix4d",
            Self::BoolArray(_) => "bool[]",
            Self::IntArray(_) => "int[]",
            Self::UIntArray(_) => "uint[]",
            Self::Int64Array(_) => "int64[]",
            Self::FloatArray(_) => "float[]",
            Self::DoubleArray(_) => "double[]",
            Self::StringArray(_) => "string[]",
            Self::TokenArray(_) => "token[]",
            Self::AssetArray(_) => "asset[]",
            Self::Float2Array(_) => "float2[]",
            Self::Float3Array(_) => "float3[]",
            Self::Float4Array(_) => "float4[]",
            Self::Double2Array(_) => "double2[]",
            Self::Double3Array(_) => "double3[]",
            Self::Double4Array(_) => "double4[]",
            Self::Int2Array(_) => "int2[]",
            Self::Int3Array(_) => "int3[]",
            Self::Int4Array(_) => "int4[]",
            Self::Dictionary(_) => "dictionary",
        }
    }
}

// ── Text emission ───────────────────────────────────────────────────────

/// Infallible text sink over a validated document. `fmt::Write` for
/// `String` never fails, so results are discarded deliberately.
struct Writer<'o> {
    out: &'o mut String,
}

impl Writer<'_> {
    fn document(&mut self, doc: &Document) {
        // §16.2.18.1: the layer header.
        self.out.push_str("#usda 1.0\n");
        if doc.default_prim.is_some() || !doc.metadata.is_empty() {
            self.out.push_str("(\n");
            if let Some(name) = &doc.default_prim {
                self.out.push_str(INDENT);
                self.out.push_str("defaultPrim = ");
                self.string(name);
                self.out.push('\n');
            }
            self.metadata_entries(&doc.metadata, 1);
            self.out.push_str(")\n");
        }
        for prim in &doc.prims {
            self.out.push('\n');
            self.prim(prim, 0);
        }
    }

    fn indent(&mut self, depth: usize) {
        for _ in 0..depth {
            self.out.push_str(INDENT);
        }
    }

    fn metadata_entries(&mut self, entries: &[Metadatum], depth: usize) {
        for entry in entries {
            self.indent(depth);
            self.out.push_str(&entry.key);
            self.out.push_str(" = ");
            self.value(&entry.value, depth);
            self.out.push('\n');
        }
    }

    /// §16.2.17: `specifier [type] "name" [( metadata )] { body }`.
    fn prim(&mut self, prim: &Prim, depth: usize) {
        self.indent(depth);
        self.out.push_str(match prim.specifier {
            Specifier::Def => "def ",
            Specifier::Over => "over ",
            Specifier::Class => "class ",
        });
        if let Some(type_name) = &prim.type_name {
            self.out.push_str(type_name);
            self.out.push(' ');
        }
        self.string(&prim.name);
        if !prim.metadata.is_empty() {
            self.out.push_str(" (\n");
            self.metadata_entries(&prim.metadata, depth + 1);
            self.indent(depth);
            self.out.push(')');
        }
        self.out.push('\n');
        self.indent(depth);
        self.out.push_str("{\n");
        for attribute in &prim.attributes {
            self.attribute(attribute, depth + 1);
        }
        for (i, child) in prim.children.iter().enumerate() {
            if i > 0 || !prim.attributes.is_empty() {
                self.out.push('\n');
            }
            self.prim(child, depth + 1);
        }
        self.indent(depth);
        self.out.push_str("}\n");
    }

    /// §16.2.16.1: `[custom] [uniform] type name [= value] [( metadata )]`.
    fn attribute(&mut self, attribute: &Attribute, depth: usize) {
        self.indent(depth);
        if attribute.custom {
            self.out.push_str("custom ");
        }
        if attribute.variability == Variability::Uniform {
            self.out.push_str("uniform ");
        }
        self.out.push_str(&attribute.type_name);
        self.out.push(' ');
        self.out.push_str(&attribute.name);
        if let Some(value) = &attribute.value {
            self.out.push_str(" = ");
            self.value(value, depth);
        }
        if !attribute.metadata.is_empty() {
            self.out.push_str(" (\n");
            self.metadata_entries(&attribute.metadata, depth + 1);
            self.indent(depth);
            self.out.push(')');
        }
        self.out.push('\n');
    }

    fn value(&mut self, value: &Value, depth: usize) {
        match value {
            Value::Bool(v) => self.out.push_str(if *v { "true" } else { "false" }),
            Value::Int(v) => self.display(v),
            Value::UInt(v) => self.display(v),
            Value::Int64(v) => self.display(v),
            Value::Float(v) => self.f32(*v),
            Value::Double(v) => self.f64(*v),
            Value::String(v) | Value::Token(v) => self.string(v),
            Value::Asset(v) => self.asset(v),
            Value::Float2(v) => self.tuple(v, Self::f32),
            Value::Float3(v) => self.tuple(v, Self::f32),
            Value::Float4(v) => self.tuple(v, Self::f32),
            Value::Double2(v) => self.tuple(v, Self::f64),
            Value::Double3(v) => self.tuple(v, Self::f64),
            Value::Double4(v) => self.tuple(v, Self::f64),
            Value::Int2(v) => self.tuple(v, Self::i32),
            Value::Int3(v) => self.tuple(v, Self::i32),
            Value::Int4(v) => self.tuple(v, Self::i32),
            Value::Matrix4d(rows) => {
                self.out.push_str("( ");
                for (i, row) in rows.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.tuple(row, Self::f64);
                }
                self.out.push_str(" )");
            }
            Value::BoolArray(v) => self.array(v, |w, b| {
                w.out.push_str(if *b { "true" } else { "false" });
            }),
            Value::IntArray(v) => self.array(v, |w, x| w.display(x)),
            Value::UIntArray(v) => self.array(v, |w, x| w.display(x)),
            Value::Int64Array(v) => self.array(v, |w, x| w.display(x)),
            Value::FloatArray(v) => self.array(v, |w, x| w.f32(*x)),
            Value::DoubleArray(v) => self.array(v, |w, x| w.f64(*x)),
            Value::StringArray(v) | Value::TokenArray(v) => self.array(v, |w, s| w.string(s)),
            Value::AssetArray(v) => self.array(v, |w, s| w.asset(s)),
            Value::Float2Array(v) => self.array(v, |w, t| w.tuple(t, Self::f32)),
            Value::Float3Array(v) => self.array(v, |w, t| w.tuple(t, Self::f32)),
            Value::Float4Array(v) => self.array(v, |w, t| w.tuple(t, Self::f32)),
            Value::Double2Array(v) => self.array(v, |w, t| w.tuple(t, Self::f64)),
            Value::Double3Array(v) => self.array(v, |w, t| w.tuple(t, Self::f64)),
            Value::Double4Array(v) => self.array(v, |w, t| w.tuple(t, Self::f64)),
            Value::Int2Array(v) => self.array(v, |w, t| w.tuple(t, Self::i32)),
            Value::Int3Array(v) => self.array(v, |w, t| w.tuple(t, Self::i32)),
            Value::Int4Array(v) => self.array(v, |w, t| w.tuple(t, Self::i32)),
            Value::Dictionary(entries) => {
                // §6.6.2, §16.2.15: typed entries with quoted keys.
                self.out.push_str("{\n");
                for (key, v) in entries {
                    self.indent(depth + 1);
                    self.out.push_str(v.canonical_type_name());
                    self.out.push(' ');
                    self.string(key);
                    self.out.push_str(" = ");
                    self.value(v, depth + 1);
                    self.out.push('\n');
                }
                self.indent(depth);
                self.out.push('}');
            }
        }
    }

    fn display(&mut self, v: &impl fmt::Display) {
        let _ = write!(self.out, "{v}");
    }

    fn i32(&mut self, v: i32) {
        self.display(&v);
    }

    /// Shortest representation that parses back to the same `f32` (Rust's
    /// `Display`), with the USDA spellings for non-finite values (§16.2.5).
    fn f32(&mut self, v: f32) {
        if v.is_finite() {
            self.display(&v);
        } else {
            self.non_finite(v.is_nan(), v.is_sign_negative());
        }
    }

    fn f64(&mut self, v: f64) {
        if v.is_finite() {
            self.display(&v);
        } else {
            self.non_finite(v.is_nan(), v.is_sign_negative());
        }
    }

    fn non_finite(&mut self, nan: bool, negative: bool) {
        self.out.push_str(match (nan, negative) {
            (true, _) => "nan",
            (false, false) => "inf",
            (false, true) => "-inf",
        });
    }

    fn tuple<T>(&mut self, items: &[T], mut item: impl FnMut(&mut Self, T))
    where
        T: Copy,
    {
        self.out.push('(');
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            item(self, *x);
        }
        self.out.push(')');
    }

    fn array<T>(&mut self, items: &[T], mut item: impl FnMut(&mut Self, &T)) {
        self.out.push('[');
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            item(self, x);
        }
        self.out.push(']');
    }

    /// Double-quoted string with the escapes of §16.2.5. The grammar has no
    /// single-character escape for backslash, so it uses the hex form.
    fn string(&mut self, s: &str) {
        self.out.push('"');
        for c in s.chars() {
            match c {
                '"' => self.out.push_str("\\\""),
                '\\' => self.out.push_str("\\x5C"),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                '\t' => self.out.push_str("\\t"),
                c if u32::from(c) < 0x20 || c == '\u{7F}' => {
                    let _ = write!(self.out, "\\x{:02X}", u32::from(c));
                }
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    /// `@path@`; validation guarantees the path has no `@` or line break.
    fn asset(&mut self, path: &str) {
        self.out.push('@');
        self.out.push_str(path);
        self.out.push('@');
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;

    use super::*;
    use crate::ast;
    use crate::parser::parse;

    fn mesh_doc() -> Document {
        let mut mesh = Prim::def("Mesh", "Tri");
        mesh.attributes.push(Attribute::new(
            "points",
            "point3f[]",
            Value::Float3Array(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.1, -2.5e-3]]),
        ));
        mesh.attributes.push(Attribute::new(
            "faceVertexCounts",
            "int[]",
            Value::IntArray(vec![3]),
        ));
        mesh.attributes.push(
            Attribute::new(
                "primvars:st",
                "texCoord2f[]",
                Value::Float2Array(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            )
            .with_metadata("interpolation", Value::Token("vertex".into())),
        );
        mesh.attributes.push(
            Attribute::new("subdivisionScheme", "token", Value::Token("none".into())).uniform(),
        );
        mesh.attributes.push(
            Attribute::new("exedra:label", "string", Value::String("say \"hi\"".into())).custom(),
        );
        let mut root = Prim::def("Xform", "Root");
        root.metadata
            .push(Metadatum::new("kind", Value::Token("component".into())));
        root.attributes.push(Attribute::new(
            "xformOp:transform",
            "matrix4d",
            Value::Matrix4d([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [2.0, 3.0, 4.0, 1.0],
            ]),
        ));
        root.children.push(mesh);
        Document {
            default_prim: Some("Root".into()),
            metadata: vec![
                Metadatum::new("metersPerUnit", Value::Double(1.0)),
                Metadatum::new("upAxis", Value::Token("Z".into())),
            ],
            prims: vec![root],
        }
    }

    #[test]
    fn golden_text() {
        let text = mesh_doc().to_usda().unwrap();
        let expected = r#"#usda 1.0
(
    defaultPrim = "Root"
    metersPerUnit = 1
    upAxis = "Z"
)

def Xform "Root" (
    kind = "component"
)
{
    matrix4d xformOp:transform = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (2, 3, 4, 1) )

    def Mesh "Tri"
    {
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 0.1, -0.0025)]
        int[] faceVertexCounts = [3]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (0, 1)] (
            interpolation = "vertex"
        )
        uniform token subdivisionScheme = "none"
        custom string exedra:label = "say \"hi\""
    }
}
"#;
        assert_eq!(text, expected, "writer output");
        assert_eq!(text, mesh_doc().to_usda().unwrap(), "deterministic");
    }

    #[test]
    fn output_reparses_with_authored_details() {
        let text = mesh_doc().to_usda().unwrap();
        let parsed = parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let layer = &parsed.layer;
        let meta: Vec<(&str, String)> = layer
            .metadata
            .iter()
            .filter_map(|m| match m {
                ast::LayerMeta::Custom(entry) => {
                    Some((entry.key, alloc::format!("{:?}", entry.value)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(meta.len(), 3, "layer metadata entries: {meta:?}");
        assert_eq!(meta[0].0, "defaultPrim", "first layer metadata key");
        assert_eq!(meta[2].0, "upAxis", "last layer metadata key");

        let root = &layer.prims[0];
        assert_eq!(root.type_name, Some("Xform"), "root type");
        let ast::PrimChild::Prim(mesh) = &root.children[1] else {
            panic!("expected child prim");
        };
        let attrs: Vec<&ast::Attribute<'_>> = mesh
            .children
            .iter()
            .filter_map(|c| match c {
                ast::PrimChild::Attribute(a) => Some(a),
                _ => None,
            })
            .collect();
        let st = attrs.iter().find(|a| a.name == "primvars:st").unwrap();
        assert_eq!(st.type_name, "texCoord2f", "st type");
        assert!(st.is_array, "st is an array");
        assert_eq!(st.metadata.len(), 1, "st metadata");
        assert_eq!(st.metadata[0].key, "interpolation", "st metadata key");
        let scheme = attrs
            .iter()
            .find(|a| a.name == "subdivisionScheme")
            .unwrap();
        assert!(
            scheme.uniform && !scheme.custom,
            "subdivisionScheme qualifiers"
        );
        let label = attrs.iter().find(|a| a.name == "exedra:label").unwrap();
        assert!(
            label.custom && !label.uniform,
            "custom attribute qualifiers"
        );
    }

    #[test]
    fn rejects_invalid_documents() {
        let mut doc = mesh_doc();
        doc.default_prim = Some("Missing".into());
        assert_eq!(
            doc.to_usda(),
            Err(WriteError::DefaultPrimNotFound {
                name: "Missing".into()
            }),
            "defaultPrim must name a root prim"
        );

        let mut doc = mesh_doc();
        doc.prims[0].children[0].attributes[0].type_name = "normal3f".into();
        assert_eq!(
            doc.to_usda(),
            Err(WriteError::TypeMismatch {
                path: "/Root/Tri.points".into(),
                type_name: "normal3f".into()
            }),
            "array value needs an array type"
        );

        let mut doc = mesh_doc();
        doc.prims[0].children[0].attributes[0].type_name = "vec3f[]".into();
        assert!(
            matches!(doc.to_usda(), Err(WriteError::UnknownType { .. })),
            "unknown type"
        );

        let mut doc = mesh_doc();
        doc.prims[0].children[0].name = "1bad".into();
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidName { .. })),
            "prim names are identifiers"
        );

        let mut doc = mesh_doc();
        let dup = doc.prims[0].children[0].attributes[0].clone();
        doc.prims[0].children[0].attributes.push(dup);
        assert_eq!(
            doc.to_usda(),
            Err(WriteError::Duplicate {
                path: "/Root/Tri.points".into()
            }),
            "duplicate attribute"
        );

        let mut doc = mesh_doc();
        doc.metadata
            .push(Metadatum::new("defaultPrim", Value::Token("Root".into())));
        assert!(
            matches!(doc.to_usda(), Err(WriteError::Duplicate { .. })),
            "defaultPrim cannot be repeated as generic metadata"
        );

        let mut doc = mesh_doc();
        doc.prims[0].children[0].attributes.push(Attribute::new(
            "tex",
            "asset",
            Value::Asset("a@b.png".into()),
        ));
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidAssetPath { .. })),
            "unquotable asset path"
        );
    }

    #[test]
    fn scalar_spellings() {
        let mut prim = Prim::new(Specifier::Over, None, "P");
        prim.metadata.push(Metadatum::new(
            "customData",
            Value::Dictionary(vec![
                ("a:b".to_string(), Value::Int(1)),
                (
                    "nested".to_string(),
                    Value::Dictionary(vec![("s".to_string(), Value::String("x\\y".into()))]),
                ),
            ]),
        ));
        prim.attributes.push(Attribute::new(
            "f",
            "float[]",
            Value::FloatArray(vec![f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 0.1, -0.0]),
        ));
        prim.attributes.push(Attribute::new(
            "tex",
            "asset",
            Value::Asset("textures/a.png".into()),
        ));
        let doc = Document {
            prims: vec![prim],
            ..Document::new()
        };
        let text = doc.to_usda().unwrap();
        let expected = r#"#usda 1.0

over "P" (
    customData = {
        int "a:b" = 1
        dictionary "nested" = {
            string "s" = "x\x5Cy"
        }
    }
)
{
    float[] f = [inf, -inf, nan, 0.1, -0]
    asset tex = @textures/a.png@
}
"#;
        assert_eq!(text, expected, "scalar spellings");
        let parsed = parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    }

    fn doc_with(attribute: Attribute) -> Document {
        let mut prim = Prim::def("Xform", "Root");
        prim.attributes.push(attribute);
        Document {
            prims: vec![prim],
            ..Document::new()
        }
    }

    #[test]
    fn dictionaries_are_metadata_not_attribute_types() {
        let dict = Value::Dictionary(vec![("k".to_string(), Value::Int(1))]);
        let with_default = Attribute::new("data", "dictionary", dict.clone()).custom();
        assert_eq!(
            doc_with(with_default).to_usda(),
            Err(WriteError::UnknownType {
                path: "/Root.data".into(),
                type_name: "dictionary".into()
            }),
            "dictionary attribute with a default"
        );
        let declaration = Attribute {
            value: None,
            ..Attribute::new("data", "dictionary", Value::Int(0))
        };
        assert!(
            matches!(
                doc_with(declaration).to_usda(),
                Err(WriteError::UnknownType { .. })
            ),
            "dictionary attribute declaration"
        );
        let smuggled = Attribute::new("data", "int", dict);
        assert!(
            matches!(
                doc_with(smuggled).to_usda(),
                Err(WriteError::TypeMismatch { .. })
            ),
            "dictionary value under another type"
        );
        // Legacy capitalized aliases and non-attribute registry entries are
        // not admitted either.
        for type_name in [
            "Vec3f",
            "PointFloat",
            "opaque",
            "pathExpression",
            "dictionary[]",
        ] {
            let declaration = Attribute {
                value: None,
                ..Attribute::new("x", type_name, Value::Int(0))
            };
            assert!(
                matches!(
                    doc_with(declaration).to_usda(),
                    Err(WriteError::UnknownType { .. })
                ),
                "{type_name} rejected"
            );
        }
    }

    #[test]
    fn identifiers_follow_xid_tables() {
        // U+00B2 is alphanumeric but not XID_Continue; OpenUSD rejects it.
        let superscript = Attribute::new("x\u{b2}", "int", Value::Int(1)).custom();
        assert!(
            matches!(
                doc_with(superscript).to_usda(),
                Err(WriteError::InvalidName { .. })
            ),
            "x\u{b2} rejected"
        );
        let mut doc = doc_with(Attribute::new("a", "int", Value::Int(1)));
        doc.prims[0].name = "x\u{b2}".into();
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidName { .. })),
            "prim name x\u{b2} rejected"
        );
        // U+0301 (combining acute) is XID_Continue.
        let combining = "cafe\u{301}";
        let mut doc = doc_with(Attribute::new(
            alloc::format!("ns:{combining}"),
            "int",
            Value::Int(1),
        ));
        doc.prims[0].name = combining.into();
        let text = doc.to_usda().expect("combining marks are valid");
        let parsed = parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.layer.prims[0].name, combining, "prim name re-parses");
    }

    #[test]
    fn rejects_nul_in_strings() {
        let nul = Attribute::new("s", "string", Value::String("a\0b".into()));
        assert_eq!(
            doc_with(nul).to_usda(),
            Err(WriteError::NulInString {
                path: "/Root.s".into()
            }),
            "NUL would be truncated by readers"
        );
    }

    #[test]
    fn nested_dictionary_metadata_is_written() {
        let mut prim = Prim::def("Xform", "Root");
        prim.metadata.push(Metadatum::new(
            "customData",
            Value::Dictionary(vec![
                ("exedra:path".to_string(), Value::String("a/b".into())),
                (
                    "nested".to_string(),
                    Value::Dictionary(vec![("n".to_string(), Value::Int(1))]),
                ),
            ]),
        ));
        let text = Document {
            prims: vec![prim],
            ..Document::new()
        }
        .to_usda()
        .unwrap();
        assert!(
            text.contains(
                "        dictionary \"nested\" = {\n            int \"n\" = 1\n        }"
            ),
            "{text}"
        );
        assert!(parse(&text).diagnostics.is_empty(), "re-parses");
    }
}
