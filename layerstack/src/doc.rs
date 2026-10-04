// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Document model: layers, prim specs, and composition arcs.
//!
//! Spec: AOUSD Core §6–§7 (scene description data model and opinions), plus §10
//! for arc-related fields (variants/references).

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::fmt;

use hashbrown::HashMap;

use crate::{
    array_edit::ArrayEdit,
    interner::TokenId,
    interner::TokenInterner,
    listop::ListOp,
    path::{Path, PathId, PathInterner, PropertyPath, TargetPath},
    prim_index::OpinionValue,
    property::{
        PropertyEntry, PropertySpec, PropertyType, get_property, get_property_mut, remove_property,
        set_property_vec,
    },
    spec_path::{SpecComponent, SpecPath, VariantSelectionSite},
};

/// Identifies a layer by stable ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayerId(pub u64);

impl LayerId {
    /// The layer of a reference or payload whose asset path could not be
    /// resolved (see [`Reference::unresolved`]).
    ///
    /// No layer store holds a layer with this ID: such an arc targets no
    /// layer stack, contributes nothing, and composition reports it as
    /// [`CompositionError::UnresolvedAsset`]. Importers use it so that a
    /// failed resolution keeps the arc instead of dropping it or retargeting
    /// it at another layer.
    ///
    /// [`CompositionError::UnresolvedAsset`]: crate::CompositionError::UnresolvedAsset
    pub const UNRESOLVED: Self = Self(u64::MAX);
}

/// Prim specifier: determines how a prim spec contributes to composition.
///
/// Spec: AOUSD Core §7.6 (specifier field), §12.2.1 (specifier resolution).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Specifier {
    /// Concretely defining (`def`). The prim is fully defined.
    Def,
    /// Non-defining (`over`). Provides opinions without defining a new prim.
    Over,
    /// Abstractly defining (`class`). Defines a prim template not meant for
    /// direct use.
    Class,
}

/// A plain value that can be resolved by the kernel.
///
/// Covers scalar types (§6.2) and dimensioned types (§6.3): vectors,
/// matrices, and quaternions.
///
/// Spec: AOUSD Core §6.2–§6.3 (scene description data types), §16.3.10
/// (value type encoding).
#[derive(Clone, Debug)]
pub enum Value {
    /// Blocks weaker time samples, splines and clips while allowing weaker
    /// defaults. OpenUSD `SdfAnimationBlock`, `UsdAttribute::BlockAnimation`.
    AnimationBlock,
    /// No value.
    Null,
    /// A boolean (`bool`). Spec: §6.2.
    Bool(bool),
    /// An unsigned 8-bit integer (`uchar`). Spec: §6.2.
    UChar(u8),
    /// A signed 32-bit integer (`int`). Spec: §6.2.
    Int(i32),
    /// An unsigned 32-bit integer (`uint`). Spec: §6.2.
    UInt(u32),
    /// A signed 64-bit integer (`int64`). Spec: §6.2.
    Int64(i64),
    /// An unsigned 64-bit integer (`uint64`). Spec: §6.2.
    UInt64(u64),
    /// An IEEE 754 half-precision float (`half`), stored as raw bits.
    ///
    /// Spec: §6.2, §16.3.10.8 (IEEE 754-2008).
    Half(u16),
    /// A 32-bit float (`float`). Spec: §6.2.
    Float(f32),
    /// A 64-bit float (`double`). Spec: §6.2.
    Double(f64),
    /// A UTF-8 string (`string`). Spec: §6.2.
    String(Arc<str>),
    /// A token (interned string, `token`). Spec: §6.2.
    Token(TokenId),
    /// An asset path, distinct from a plain string.
    ///
    /// Asset paths undergo variable substitution and resolution (§9).
    /// They are used for layer references, texture paths, and other
    /// external resource identifiers.
    ///
    /// Spec: §6.2 (asset type), §9 (asset resolution).
    Asset(Arc<str>),
    /// A path expression (`pathExpression`), as its text.
    ///
    /// OpenUSD's `SdfPathExpression` (used for example by
    /// `CollectionAPI`'s `membershipExpression`) is not evaluated here.
    /// Composition anchors each opinion's relative patterns at the prim that
    /// authors it and maps its paths into the stage namespace, and a `%_`
    /// splices in the next weaker opinion's expression; the resolved text is
    /// written as `SdfPathExpression::GetText` writes it.
    ///
    /// Spec: AOUSD Core §16.3.10.14 (crate encoding); the type itself is an
    /// OpenUSD extension (`pxr/usd/sdf/pathExpression.h`).
    PathExpression(Arc<str>),
    /// A time code value (`timecode`), semantically a time in frames.
    ///
    /// Spec: §6.2.
    TimeCode(f64),

    // ── Vectors (§6.3) ─────────────────────────────────────────────────
    //
    // Row vectors that pre-multiply matrices. Stored inline.
    /// 2-component `f64` vector (`double2`). Spec: §6.3.
    Vec2d([f64; 2]),
    /// 3-component `f64` vector (`double3`). Spec: §6.3.
    Vec3d([f64; 3]),
    /// 4-component `f64` vector (`double4`). Spec: §6.3.
    Vec4d([f64; 4]),
    /// 2-component `f32` vector (`float2`). Spec: §6.3.
    Vec2f([f32; 2]),
    /// 3-component `f32` vector (`float3`). Spec: §6.3.
    Vec3f([f32; 3]),
    /// 4-component `f32` vector (`float4`). Spec: §6.3.
    Vec4f([f32; 4]),
    /// 2-component half vector (`half2`), stored as raw bits. Spec: §6.3.
    Vec2h([u16; 2]),
    /// 3-component half vector (`half3`), stored as raw bits. Spec: §6.3.
    Vec3h([u16; 3]),
    /// 4-component half vector (`half4`), stored as raw bits. Spec: §6.3.
    Vec4h([u16; 4]),
    /// 2-component `i32` vector (`int2`). Spec: §6.3.
    Vec2i([i32; 2]),
    /// 3-component `i32` vector (`int3`). Spec: §6.3.
    Vec3i([i32; 3]),
    /// 4-component `i32` vector (`int4`). Spec: §6.3.
    Vec4i([i32; 4]),

    // ── Matrices (§6.3) ────────────────────────────────────────────────
    //
    // Row-major, `f64` only. Translations live in the last row.
    // Boxed to avoid bloating the enum (matrix4d = 128 bytes).
    /// 2×2 `f64` matrix (`matrix2d`), row-major. Spec: §6.3.
    Matrix2d(Box<[f64; 4]>),
    /// 3×3 `f64` matrix (`matrix3d`), row-major. Spec: §6.3.
    Matrix3d(Box<[f64; 9]>),
    /// 4×4 `f64` matrix (`matrix4d`), row-major. Spec: §6.3.
    Matrix4d(Box<[f64; 16]>),

    // ── Quaternions (§6.3) ─────────────────────────────────────────────
    //
    // Storage order is (imaginary, real) = (i, j, k, r).
    // Display order per §16.3.10.22 is (r, i, j, k).
    /// `f64` quaternion (`quatd`), stored as `[i, j, k, r]`. Spec: §6.3.
    Quatd([f64; 4]),
    /// `f32` quaternion (`quatf`), stored as `[i, j, k, r]`. Spec: §6.3.
    Quatf([f32; 4]),
    /// Half quaternion (`quath`), stored as `[i, j, k, r]` in raw bits. Spec: §6.3.
    Quath([u16; 4]),

    /// Opaque bytes tagged with a type name.
    Opaque {
        /// The (interned) type name for these bytes.
        type_name: TokenId,
        /// The opaque payload.
        bytes: Arc<[u8]>,
    },
    /// Value block sentinel — suppresses weaker opinions.
    ///
    /// When encountered during value resolution, all weaker opinions are
    /// skipped and the fallback value is returned instead.
    ///
    /// Spec: AOUSD Core §12.3 (value blocking), §16.3.10.16 (`ValueBlock` type).
    Blocked,
    /// An ordered sequence of values (tuples and arrays).
    ///
    /// Stores both fixed-size tuples (e.g. `(1.0, 2.0, 3.0)` from a
    /// `float3` attribute) and variable-length arrays (e.g. `[1, 2, 3]`
    /// from an `int[]` attribute). Type semantics are carried externally
    /// by the attribute's type name, not by the value itself.
    ///
    /// Spec: AOUSD Core §6.2 (scene description data types).
    Array(Vec<Self>),
    /// Shared homogeneous numeric array, with native element storage.
    ///
    /// Cloning shares the buffer; mutation uses copy-on-write. USD aliases
    /// remain on the property type. See [`crate::TypedArray`].
    TypedArray(crate::TypedArray),
    /// A dictionary of string-keyed values, maintaining insertion order.
    ///
    /// Dictionary-valued fields use combining semantics during value
    /// resolution: dictionaries from multiple opinions are recursively
    /// merged rather than using strongest-wins.
    ///
    /// Spec: AOUSD Core §6.2 (dictionary type), §6.6.2.1 (dictionary
    /// combining), §12.2.5 (dictionary-valued metadata combining).
    Dictionary(Vec<(Arc<str>, Self)>),
    /// A sparse array edit composed over a weaker array opinion.
    ///
    /// Resolved attribute values never expose this directly; it exists in
    /// authored scene description and during composition.
    ArrayEdit(ArrayEdit),
    /// A shared sparse edit retaining its actual array value type.
    ///
    /// Unlike an attribute declaration, this tag survives literal-free
    /// programs and inconsistent source declarations. See [`crate::TypedArrayEdit`].
    TypedArrayEdit(Arc<crate::TypedArrayEdit>),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        if matches!(self, Self::Array(_) | Self::TypedArray(_))
            && matches!(other, Self::Array(_) | Self::TypedArray(_))
            && let (Some(a), Some(b)) = (self.array_ref(), other.array_ref())
        {
            // No elements carry a logical kind in content equality. Storage
            // kind still matters for typed reads, writing and authored guards.
            if a.is_empty() && b.is_empty() {
                return true;
            }
            if let (Some(a), Some(b)) = (a.typed(), b.typed()) {
                return a == b;
            }
            return a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| *a == *b);
        }
        match (self, other) {
            (Self::Null, Self::Null)
            | (Self::Blocked, Self::Blocked)
            | (Self::AnimationBlock, Self::AnimationBlock) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::UChar(a), Self::UChar(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::UInt(a), Self::UInt(b)) => a == b,
            (Self::Int64(a), Self::Int64(b)) => a == b,
            (Self::UInt64(a), Self::UInt64(b)) => a == b,
            (Self::Half(a), Self::Half(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a == b,
            (Self::Double(a), Self::Double(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Token(a), Self::Token(b)) => a == b,
            (Self::Asset(a), Self::Asset(b)) => a == b,
            (Self::PathExpression(a), Self::PathExpression(b)) => a == b,
            (Self::TimeCode(a), Self::TimeCode(b)) => a == b,
            (Self::Vec2f(a), Self::Vec2f(b)) => a == b,
            (Self::Vec3f(a), Self::Vec3f(b)) => a == b,
            (Self::Vec4f(a), Self::Vec4f(b)) => a == b,
            (Self::Vec2d(a), Self::Vec2d(b)) => a == b,
            (Self::Vec3d(a), Self::Vec3d(b)) => a == b,
            (Self::Vec4d(a), Self::Vec4d(b)) => a == b,
            (Self::Vec2h(a), Self::Vec2h(b)) => a == b,
            (Self::Vec3h(a), Self::Vec3h(b)) => a == b,
            (Self::Vec4h(a), Self::Vec4h(b)) => a == b,
            (Self::Vec2i(a), Self::Vec2i(b)) => a == b,
            (Self::Vec3i(a), Self::Vec3i(b)) => a == b,
            (Self::Vec4i(a), Self::Vec4i(b)) => a == b,
            (Self::Quatf(a), Self::Quatf(b)) => a == b,
            (Self::Quatd(a), Self::Quatd(b)) => a == b,
            (Self::Quath(a), Self::Quath(b)) => a == b,
            (Self::Matrix2d(a), Self::Matrix2d(b)) => a == b,
            (Self::Matrix3d(a), Self::Matrix3d(b)) => a == b,
            (Self::Matrix4d(a), Self::Matrix4d(b)) => a == b,
            (Self::Dictionary(a), Self::Dictionary(b)) => a == b,
            (Self::ArrayEdit(a), Self::ArrayEdit(b)) => a == b,
            (Self::TypedArrayEdit(a), Self::TypedArrayEdit(b)) => a == b,
            (
                Self::Opaque {
                    type_name: at,
                    bytes: ab,
                },
                Self::Opaque {
                    type_name: bt,
                    bytes: bb,
                },
            ) => at == bt && ab == bb,
            _ => false,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::UChar(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::UInt(v) => write!(f, "{v}"),
            Self::Int64(v) => write!(f, "{v}"),
            Self::UInt64(v) => write!(f, "{v}"),
            Self::Half(v) => write!(f, "half(0x{v:04x})"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Double(v) => write!(f, "{v}"),
            Self::String(v) => write!(f, "{v}"),
            Self::Token(v) => write!(f, "token({v:?})"),
            Self::Asset(v) => write!(f, "@{v}@"),
            Self::PathExpression(v) => write!(f, "pathExpression({v:?})"),
            Self::TimeCode(v) => write!(f, "{v}"),
            // Vectors
            Self::Vec2d(v) => write!(f, "({}, {})", v[0], v[1]),
            Self::Vec3d(v) => write!(f, "({}, {}, {})", v[0], v[1], v[2]),
            Self::Vec4d(v) => write!(f, "({}, {}, {}, {})", v[0], v[1], v[2], v[3]),
            Self::Vec2f(v) => write!(f, "({}, {})", v[0], v[1]),
            Self::Vec3f(v) => write!(f, "({}, {}, {})", v[0], v[1], v[2]),
            Self::Vec4f(v) => write!(f, "({}, {}, {}, {})", v[0], v[1], v[2], v[3]),
            Self::Vec2h(v) => write!(f, "(half(0x{:04x}), half(0x{:04x}))", v[0], v[1]),
            Self::Vec3h(v) => {
                write!(
                    f,
                    "(half(0x{:04x}), half(0x{:04x}), half(0x{:04x}))",
                    v[0], v[1], v[2]
                )
            }
            Self::Vec4h(v) => {
                write!(
                    f,
                    "(half(0x{:04x}), half(0x{:04x}), half(0x{:04x}), half(0x{:04x}))",
                    v[0], v[1], v[2], v[3]
                )
            }
            Self::Vec2i(v) => write!(f, "({}, {})", v[0], v[1]),
            Self::Vec3i(v) => write!(f, "({}, {}, {})", v[0], v[1], v[2]),
            Self::Vec4i(v) => write!(f, "({}, {}, {}, {})", v[0], v[1], v[2], v[3]),
            // Matrices (row-major flat array)
            Self::Matrix2d(m) => fmt_matrix(f, m.as_slice(), 2),
            Self::Matrix3d(m) => fmt_matrix(f, m.as_slice(), 3),
            Self::Matrix4d(m) => fmt_matrix(f, m.as_slice(), 4),
            // Quaternions — display order is (r, i, j, k) per §16.3.10.22.
            Self::Quatd(q) => write!(f, "({}, {}, {}, {})", q[3], q[0], q[1], q[2]),
            Self::Quatf(q) => write!(f, "({}, {}, {}, {})", q[3], q[0], q[1], q[2]),
            Self::Quath(q) => {
                write!(
                    f,
                    "(half(0x{:04x}), half(0x{:04x}), half(0x{:04x}), half(0x{:04x}))",
                    q[3], q[0], q[1], q[2]
                )
            }
            Self::Opaque { type_name, bytes } => {
                write!(f, "opaque({type_name:?}, {} bytes)", bytes.len())
            }
            Self::AnimationBlock => f.write_str("AnimationBlock"),
            Self::Blocked => write!(f, "blocked"),
            Self::TypedArray(items) => {
                write!(f, "[")?;
                for (i, v) in items.values().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Self::Array(items) => {
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Self::Dictionary(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Self::ArrayEdit(edit) => write!(f, "edit({} ops)", edit.ops.len()),
            Self::TypedArrayEdit(edit) => write!(f, "edit({} ops)", edit.edit().ops.len()),
        }
    }
}

/// Formats a row-major matrix as nested tuples: `((r0c0, r0c1), (r1c0, r1c1))`.
fn fmt_matrix(f: &mut fmt::Formatter<'_>, m: &[f64], cols: usize) -> fmt::Result {
    write!(f, "(")?;
    for row in 0..cols {
        if row > 0 {
            write!(f, ", ")?;
        }
        write!(f, "(")?;
        for col in 0..cols {
            if col > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", m[row * cols + col])?;
        }
        write!(f, ")")?;
    }
    write!(f, ")")
}

impl Value {
    /// Stores an edit with its actual array value type, independently of the
    /// attribute declaration. Clones share the immutable program and type.
    #[must_use]
    pub fn typed_array_edit(edit: ArrayEdit, value_type: PropertyType) -> Self {
        Self::TypedArrayEdit(Arc::new(crate::TypedArrayEdit::new(edit, value_type)))
    }

    /// Borrows either an untagged generic or a typed sparse edit program.
    #[must_use]
    pub fn array_edit_ref(&self) -> Option<&ArrayEdit> {
        match self {
            Self::ArrayEdit(edit) => Some(edit),
            Self::TypedArrayEdit(edit) => Some(edit.edit()),
            _ => None,
        }
    }

    /// The actual type of a tagged edit. Generic host-authored programs have
    /// no type tag and return `None`.
    #[must_use]
    pub fn array_edit_type(&self) -> Option<&PropertyType> {
        match self {
            Self::TypedArrayEdit(edit) => Some(edit.value_type()),
            _ => None,
        }
    }

    /// Packs a homogeneous numeric array into native storage. Heterogeneous,
    /// string and token arrays keep their general value representation.
    #[must_use]
    pub fn array(values: Vec<Self>) -> Self {
        Self::array_with_element(values, None)
    }

    /// Packs an array, using `element` to retain the kind of an empty buffer.
    /// Nonempty arrays keep their actual scalar kind; incompatible elements
    /// stay heterogeneous rather than being coerced or discarded.
    #[must_use]
    pub fn array_with_element(values: Vec<Self>, element: Option<&Self>) -> Self {
        Self::array_from_iter(values, element)
    }

    /// Builds an array directly from scalar elements, without retaining an
    /// intermediate heterogeneous buffer for homogeneous numeric data.
    /// `element` supplies the kind only when the iterator is empty.
    #[must_use]
    pub fn array_from_iter(values: impl IntoIterator<Item = Self>, element: Option<&Self>) -> Self {
        let mut values = values.into_iter();
        match values.next() {
            Some(first) => {
                let kind = first.clone();
                crate::TypedArray::pack_iter(&kind, core::iter::once(first).chain(values))
            }
            None => element.map_or_else(
                || Self::Array(Vec::new()),
                |kind| crate::TypedArray::pack_iter(kind, core::iter::empty()),
            ),
        }
    }

    /// Borrows an array without expanding native buffers into scalar values.
    ///
    /// Retain the resolved value, then borrow its native slice. Numeric array
    /// resolution shares the source buffer; this loop copies no points:
    ///
    /// ```
    /// use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertyPath,
    ///     PropertySpec, PropertyType, ResolvedValue, Stage, StageOptions, Value};
    /// let mut store = InMemoryStore::default();
    /// let prim = store.path("/Mesh");
    /// let points = store.tokens.intern("points");
    /// let mut layer = Layer::new(LayerId(1));
    /// layer.insert_prim(prim, PrimSpec::def().with_property(points,
    ///     PropertySpec::typed_attribute(PropertyType::new("point3f", true,
    ///         Value::Vec3f([0.0; 3])))
    ///         .with_default(Value::from(vec![[1.0_f32, 2.0, 3.0]]))));
    /// store.insert_layer(layer);
    /// let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let retained = stage.resolve_property_path(PropertyPath::new(prim, points)).unwrap();
    /// let ResolvedValue::Scalar(value) = retained.value else { panic!("points"); };
    /// let array = value.array_ref().unwrap();
    /// let points = array.typed().unwrap().as_vec3f().unwrap();
    /// assert_eq!(points[0], [1.0, 2.0, 3.0]);
    /// ```
    #[must_use]
    pub fn array_ref(&self) -> Option<crate::ArrayRef<'_>> {
        match self {
            Self::Array(values) => Some(crate::ArrayRef::Values(values)),
            Self::TypedArray(values) => values.try_materialize().ok().map(crate::ArrayRef::Typed),
            _ => None,
        }
    }

    /// Whether two values have identical representations, including float bits.
    ///
    /// Unlike `PartialEq`, unchanged NaNs compare equal and positive and negative
    /// zero remain distinct. Typed empty arrays retain their element kind here;
    /// an untyped empty array has a different authored representation. Applies recursively to arrays, dictionaries and
    /// sparse edits. Useful for retained-result comparison and authored guards.
    #[must_use]
    pub fn same_representation(&self, other: &Self) -> bool {
        crate::edit::same::Same::same(self, other)
    }

    /// Creates a string value.
    pub fn string(s: impl Into<Arc<str>>) -> Self {
        Self::String(s.into())
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(Arc::from(s))
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Self::Int(v)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Int64(v)
    }
}

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Self::Float(v)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Self::Double(v)
    }
}

/// Exposes [`Value::Dictionary`] nesting to `opinionated`'s dictionary kernel.
///
/// `layerstack` keeps its own [`Value`] representation and USD opinion
/// selection; the recursive combining algorithm itself is `opinionated`'s.
pub(crate) struct ValueDictionaries;

impl opinionated::DictionaryAdapter<Arc<str>, Value> for ValueDictionaries {
    fn entries<'v>(&self, value: &'v Value) -> Option<&'v [(Arc<str>, Value)]> {
        match value {
            Value::Dictionary(entries) => Some(entries),
            _ => None,
        }
    }

    fn dictionary(&self, entries: Vec<(Arc<str>, Value)>) -> Value {
        Value::Dictionary(entries)
    }
}

/// Combines two dictionaries per §6.6.2.1 (dictionary combining).
///
/// Rules:
/// - Keys present on only one side are kept.
/// - For a key on both sides the stronger value wins, unless both values are
///   dictionaries, which combine recursively.
/// - The result is ordered by key at every nesting level, including nested
///   dictionaries contributed by only one side, matching OpenUSD's
///   `VtDictionary`.
///
/// Delegates to [`opinionated::combine_dictionaries`].
///
/// Spec: AOUSD Core §6.6.2.1, §12.2.5.
#[must_use]
pub fn combine_dictionaries(
    stronger: &[(Arc<str>, Value)],
    weaker: &[(Arc<str>, Value)],
) -> Vec<(Arc<str>, Value)> {
    opinionated::combine_dictionaries(&ValueDictionaries, stronger, weaker)
}

/// Combines a chain of dictionary opinions in strength order (strongest first).
///
/// The chain folds strongest-first, `((d0 ∪ d1) ∪ d2) ∪ …`. Combining is not
/// associative when a key holds a dictionary in one opinion and a
/// non-dictionary in another, and the spec does not say how a chain is folded,
/// so OpenUSD governs (AOUSD Core §4.2): `MetadataValueComposer` in
/// `pxr/usd/usd/stage.cpp` composes the stronger partial result over each
/// weaker opinion via `VtDictionaryOverRecursive`. A schema fallback is the
/// last, weakest element of the chain. The result is ordered by key at every
/// nesting level, including for a single-opinion chain.
///
/// Delegates to [`opinionated::combine_dictionary_chain`].
///
/// Spec: AOUSD Core §6.6.2.1 (dictionary combining), §12.2.5.
#[must_use]
pub fn combine_dictionary_chain(
    opinions: impl IntoIterator<Item = impl AsRef<[(Arc<str>, Value)]>>,
) -> Vec<(Arc<str>, Value)> {
    opinionated::combine_dictionary_chain(&ValueDictionaries, opinions)
}

/// A named field entry: one authored metadata field on a layer, prim,
/// variant or property spec.
///
/// Properties are not fields: they are stored as [`PropertySpec`]s in
/// [`PrimSpec::properties`] and [`VariantSpec::properties`].
///
/// Spec: AOUSD Core §7.4 (metadata fields).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldEntry {
    /// The interned field name.
    pub name: TokenId,
    /// The field value.
    pub value: FieldValue,
}

/// The value of one authored metadata field.
///
/// Spec: AOUSD Core §7.4 (metadata fields), §12.2 (metadata resolution).
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    /// A plain value (strongest wins; dictionaries combine).
    Value(Value),
    /// A list-op field over tokens (resolved by chaining), such as
    /// `apiSchemas`.
    TokenListOp(ListOp<TokenId>),
    /// A list-op field over target paths (resolved by chaining).
    ///
    /// Spec: AOUSD Core §12.4 (`ListOps`), applied to path lists.
    PathListOp(ListOp<TargetPath>),
    /// A list-op field over strings (`stringlistop`), such as `clipSets`.
    StringListOp(ListOp<Arc<str>>),
    /// A list-op field over `int` values (`intlistop`).
    IntListOp(ListOp<i32>),
    /// A list-op field over `uint` values (`uintlistop`).
    UIntListOp(ListOp<u32>),
    /// A list-op field over `int64` values (`int64listop`), such as
    /// `PointInstancer`'s `inactiveIds`.
    Int64ListOp(ListOp<i64>),
    /// A list-op field over `uint64` values (`uint64listop`).
    UInt64ListOp(ListOp<u64>),
}

impl FieldValue {
    /// Returns `true` for the list-op variants.
    ///
    /// Reference and payload list ops are composition arcs and live in
    /// [`PrimSpec::references`] and [`PrimSpec::payloads`].
    #[must_use]
    pub fn is_list_op(&self) -> bool {
        !matches!(self, Self::Value(_))
    }
}

impl From<Value> for FieldValue {
    fn from(v: Value) -> Self {
        Self::Value(v)
    }
}

impl From<&str> for FieldValue {
    fn from(s: &str) -> Self {
        Self::Value(Value::string(s))
    }
}

impl From<bool> for FieldValue {
    fn from(v: bool) -> Self {
        Self::Value(Value::Bool(v))
    }
}

impl From<i32> for FieldValue {
    fn from(v: i32) -> Self {
        Self::Value(Value::Int(v))
    }
}

impl From<i64> for FieldValue {
    fn from(v: i64) -> Self {
        Self::Value(Value::Int64(v))
    }
}

impl From<f32> for FieldValue {
    fn from(v: f32) -> Self {
        Self::Value(Value::Float(v))
    }
}

impl From<f64> for FieldValue {
    fn from(v: f64) -> Self {
        Self::Value(Value::Double(v))
    }
}

/// Interpolation method for time-varying attribute resolution.
///
/// The default is [`InterpolationType::Linear`], as for an OpenUSD stage
/// (`UsdStage::GetInterpolationType` returns `UsdInterpolationTypeLinear`
/// until `SetInterpolationType` changes it; `pxr/usd/usd/stage.cpp`).
///
/// Spec: AOUSD Core §12.5 (interpolation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum InterpolationType {
    /// Step function — value holds until the next time sample.
    Held,
    /// Linearly interpolate between bracketing samples.
    /// Non-numeric types fall back to held.
    #[default]
    Linear,
}

/// A time offset/scale pair for retiming (§16.3.10.20, §12.3.2.1).
///
/// The offset places a layer's local timeline on the timeline of the layer
/// or arc that includes it: `outerTime = localTime * scale + offset`, the
/// convention of OpenUSD's `SdfLayerOffset`. [`LayerOffset::compose`]
/// concatenates offsets along a sublayer or arc chain (§10.3.1.1), and
/// [`LayerOffset::map_time`] maps a query time back into the layer.
/// The identity (no-op) is `{ offset: 0.0, scale: 1.0 }`.
///
/// `Eq` is implemented via bitwise comparison of the `f64` fields, which is
/// correct for list-op identity matching (two references with different
/// offsets are distinct list-op entries).
#[derive(Clone, Copy, Debug)]
pub struct LayerOffset {
    /// Time offset in frames.
    pub offset: f64,
    /// Time scale factor (must be positive and non-zero).
    pub scale: f64,
}

impl PartialEq for LayerOffset {
    fn eq(&self, other: &Self) -> bool {
        self.offset.to_bits() == other.offset.to_bits()
            && self.scale.to_bits() == other.scale.to_bits()
    }
}

impl Eq for LayerOffset {}

impl Default for LayerOffset {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl LayerOffset {
    /// The identity (no-op) layer offset.
    pub const IDENTITY: Self = Self {
        offset: 0.0,
        scale: 1.0,
    };

    /// Returns `true` if this is the identity (no-op) offset.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.offset == 0.0 && self.scale == 1.0
    }

    /// Maps a stage time to a layer-local time.
    ///
    /// This is the inverse of the offset's local-to-stage map:
    /// `localTime = (stageTime - offset) / scale`. OpenUSD value resolution
    /// applies `SdfLayerOffset::GetInverse()` to the query time the same way
    /// (`_GetInterpolatingTimeSamples` in `pxr/usd/usd/stage.cpp`), so a
    /// sublayer authored with `(offset = 10)` plays its local frame 0 at stage
    /// frame 10.
    ///
    /// Spec: §12.3.2.1 (layer offset and scale), §10.3.1.1 (offsets concatenate
    /// by applying the outer scale to the inner offset). The worked example in
    /// §12.3.2.1 applies the forward map to the query time instead, which
    /// contradicts OpenUSD; this follows OpenUSD.
    #[must_use]
    pub fn map_time(self, time: f64) -> f64 {
        (time - self.offset) / self.scale
    }

    /// Composes two offsets: `self` is the outer, `inner` is the inner.
    ///
    /// The result maps time as if `inner` were applied first, then `self`.
    #[must_use]
    pub fn compose(self, inner: Self) -> Self {
        Self {
            offset: self.offset + self.scale * inner.offset,
            scale: self.scale * inner.scale,
        }
    }
}

/// One entry of a layer's `layerRelocates` metadata: the prim at `source`
/// moves to `target` in the namespace of every layer stack holding the
/// layer, or is removed when `target` is `None`.
///
/// Entries are kept as authored. Composition validates them against each
/// other when it computes a layer stack's relocation table, and reports and
/// ignores the invalid ones (see [`crate::CompositionError`]).
///
/// Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`), §10.3.2.6 (relocates).
/// OpenUSD: `SdfRelocate` (`pxr/usd/sdf/types.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Relocate {
    /// The absolute prim path whose opinions move.
    pub source: PathId,
    /// The absolute prim path they move to, or `None` (authored `<>`) to
    /// remove the source prim from namespace.
    pub target: Option<PathId>,
}

/// A sublayer entry with an optional time offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SublayerEntry {
    /// The sublayer's layer ID, or [`LayerId::UNRESOLVED`] when its asset
    /// path could not be resolved (see [`SublayerEntry::unresolved`]).
    pub layer: LayerId,
    /// Time offset applied to this sublayer (§12.3.2.1).
    pub offset: LayerOffset,
    /// The authored asset path, as written in the layer's `subLayers`
    /// (not the resolved location); `None` for an entry built without one.
    /// An unresolved sublayer always has it (see
    /// [`SublayerEntry::unresolved`]).
    pub asset: Option<String>,
}

impl SublayerEntry {
    /// Creates a sublayer entry with no time offset.
    pub fn new(layer: LayerId) -> Self {
        Self::with_offset(layer, LayerOffset::IDENTITY)
    }

    /// Creates a sublayer entry with a time offset.
    pub fn with_offset(layer: LayerId, offset: LayerOffset) -> Self {
        Self {
            layer,
            offset,
            asset: None,
        }
    }

    /// Creates a sublayer entry for the authored asset path `asset`, which
    /// resolved to `layer`.
    pub fn with_asset(layer: LayerId, asset: impl Into<String>, offset: LayerOffset) -> Self {
        Self {
            layer,
            offset,
            asset: Some(asset.into()),
        }
    }

    /// Creates a sublayer entry for `asset`, whose resolution failed.
    ///
    /// The entry keeps its place and authored asset path, and its layer is
    /// [`LayerId::UNRESOLVED`]: gathering the layer stack skips it and
    /// composition reports it as [`CompositionError::UnresolvedSublayer`],
    /// keeping the rest of the layer stack.
    ///
    /// Spec: AOUSD Core §10.3.1 (sublayers), §10.6 (composition errors).
    /// OpenUSD reports it as `PcpErrorInvalidSublayerPath`
    /// (`PcpLayerStack::_BuildLayerStack`, `pxr/usd/pcp/layerStack.cpp`).
    ///
    /// [`CompositionError::UnresolvedSublayer`]: crate::CompositionError::UnresolvedSublayer
    pub fn unresolved(asset: impl Into<String>, offset: LayerOffset) -> Self {
        Self {
            layer: LayerId::UNRESOLVED,
            offset,
            asset: Some(asset.into()),
        }
    }

    /// Returns `true` when this sublayer's asset path could not be resolved
    /// (see [`SublayerEntry::unresolved`]).
    ///
    /// A sublayer whose asset path is a variable expression
    /// ([`SublayerEntry::is_expression`]) is kept unresolved by importers:
    /// gathering the layer stack evaluates it (see
    /// [`LayerStack::gather`](crate::LayerStack::gather)).
    #[must_use]
    pub fn is_unresolved(&self) -> bool {
        self.layer == LayerId::UNRESOLVED
    }

    /// Returns `true` when this sublayer's asset path is a variable
    /// expression ([`crate::variable_expression::is_expression`]).
    #[must_use]
    pub fn is_expression(&self) -> bool {
        self.asset
            .as_deref()
            .is_some_and(crate::variable_expression::is_expression)
    }
}

impl From<LayerId> for SublayerEntry {
    fn from(layer: LayerId) -> Self {
        Self::new(layer)
    }
}

/// A composition reference arc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReferenceTarget {
    /// Target a concrete prim path.
    Prim(PathId),
    /// No authored prim path: target the prim named by the referenced
    /// layer's `defaultPrim` (see [`Layer::default_prim_path`]).
    ///
    /// For an external arc (`@./asset.usda@`) that is the asset's root
    /// layer; for an internal arc (`<>`) it is the root layer of the layer
    /// stack containing the arc (see [`Reference::layer`]). When the
    /// layer has no usable `defaultPrim`, or no prim spec exists at the path
    /// it names, the arc contributes nothing and composition reports
    /// [`CompositionError::UnresolvedDefaultPrim`].
    ///
    /// Spec: AOUSD Core §10.3.2.1 (references: "If no prim path is
    /// specified, the path to the prim specified by the defaultPrim field in
    /// the specified layer is assumed"), §10.3.2.2 (payloads are references
    /// that may be unloaded). OpenUSD: `_EvalRefOrPayloadArcs` in
    /// `pxr/usd/pcp/primIndex.cpp`.
    ///
    /// [`CompositionError::UnresolvedDefaultPrim`]: crate::CompositionError::UnresolvedDefaultPrim
    DefaultPrim,
}

/// A composition reference or payload arc.
#[derive(Clone, Debug)]
pub struct Reference {
    /// The root layer of the referenced layer stack.
    ///
    /// An arc with no [`Reference::asset`] whose layer is the layer that
    /// authors it is internal (`</Prim>`, `<>`): it targets the layer stack
    /// containing the site that authors it, not that layer alone, so an
    /// internal arc authored in a sublayer reads every layer of the stack.
    /// Composition anchors it to that stack's root layer.
    ///
    /// Spec: AOUSD Core §10.3.2.1 ("the layer stack containing the
    /// reference is assumed"). OpenUSD: `_EvalRefOrPayloadArcs` in
    /// `pxr/usd/pcp/primIndex.cpp`.
    ///
    /// An arc whose [`Reference::asset`] is a variable expression
    /// ([`Reference::expression`]) names no layer until composition
    /// evaluates it; its `layer` is the layer that authors it, which the
    /// evaluated path is anchored to.
    pub layer: LayerId,
    /// The authored target within that layer/document.
    pub target: ReferenceTarget,
    /// Optional debug name / URI.
    pub asset: Option<String>,
    /// Time offset applied across this reference boundary (§12.3.2.1).
    pub layer_offset: LayerOffset,
    /// Authored reference custom data (AOUSD Core §16.2.17.5). Payloads have none.
    pub custom_data: Vec<(Arc<str>, Value)>,
}

// Reference list identity must be reflexive even for metadata NaNs, just as
// LayerOffset compares authored bits. AOUSD Core §10.3.2.1, §16.2.17.5.
impl PartialEq for Reference {
    fn eq(&self, other: &Self) -> bool {
        use crate::edit::same::Same;
        self.layer == other.layer
            && self.target == other.target
            && self.asset == other.asset
            && self.layer_offset == other.layer_offset
            && self.custom_data.len() == other.custom_data.len()
            && self.custom_data.iter().all(|(k, v)| {
                other
                    .custom_data
                    .iter()
                    .find(|(key, _)| key == k)
                    .is_some_and(|(_, value)| v.same(value))
            })
    }
}
impl Eq for Reference {}

impl Reference {
    /// Sets the authored custom data, preserving it through unresolved arcs.
    pub fn with_custom_data(mut self, custom_data: Vec<(Arc<str>, Value)>) -> Self {
        self.custom_data = custom_data;
        self
    }

    /// Creates a reference with no asset path.
    ///
    /// With `layer` set to the layer that authors it, the arc is internal
    /// (see [`Reference::layer`]).
    pub fn new(layer: LayerId, prim_path: PathId) -> Self {
        Self {
            layer,
            target: ReferenceTarget::Prim(prim_path),
            asset: None,
            layer_offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
        }
    }

    /// Creates a reference with an asset path.
    pub fn with_asset(layer: LayerId, prim_path: PathId, asset: impl Into<String>) -> Self {
        Self {
            layer,
            target: ReferenceTarget::Prim(prim_path),
            asset: Some(asset.into()),
            layer_offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
        }
    }

    /// Creates a reference that implicitly targets the referenced layer's
    /// `defaultPrim`.
    pub fn to_default_prim(layer: LayerId) -> Self {
        Self {
            layer,
            target: ReferenceTarget::DefaultPrim,
            asset: None,
            layer_offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
        }
    }

    /// Creates an asset reference that implicitly targets the asset's
    /// `defaultPrim`.
    pub fn with_asset_default_prim(layer: LayerId, asset: impl Into<String>) -> Self {
        Self {
            layer,
            target: ReferenceTarget::DefaultPrim,
            asset: Some(asset.into()),
            layer_offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
        }
    }

    /// Creates a reference or payload to `asset` whose resolution failed.
    ///
    /// The arc keeps its authored asset path, target and offset, and its
    /// layer is [`LayerId::UNRESOLVED`]: it has no target, contributes
    /// nothing, and composition reports it as
    /// [`CompositionError::UnresolvedAsset`].
    ///
    /// Spec: AOUSD Core §10.3.2.1 ("If a layer stack cannot be computed for
    /// a reference's layer asset path, it is a composition error and that
    /// reference is ignored").
    ///
    /// [`CompositionError::UnresolvedAsset`]: crate::CompositionError::UnresolvedAsset
    pub fn unresolved(
        asset: impl Into<String>,
        target: ReferenceTarget,
        layer_offset: LayerOffset,
    ) -> Self {
        Self {
            layer: LayerId::UNRESOLVED,
            target,
            asset: Some(asset.into()),
            layer_offset,
            custom_data: Vec::new(),
        }
    }

    /// Creates a reference or payload, authored in `layer`, whose asset path
    /// `asset` is a variable expression such as `` `"./${NAME}.usd"` ``.
    ///
    /// The arc targets no layer stack as authored
    /// ([`Reference::is_unresolved`]): composition evaluates `asset` with
    /// the expression variables of the layer stack that authors the arc,
    /// resolves the resulting path relative to `layer`
    /// ([`LayerStore::asset_layer`]) and follows the arc there. An
    /// expression that evaluates to nothing drops the arc; one that fails
    /// to evaluate drops it and is reported as
    /// [`CompositionError::VariableExpressionError`].
    ///
    /// Spec: AOUSD Core §10.3.2.1 (references), §10.3.2.2 (payloads).
    /// OpenUSD: `_PcpComposeSiteReferencesOrPayloads` in
    /// `pxr/usd/pcp/composeSite.cpp`.
    ///
    /// [`CompositionError::VariableExpressionError`]: crate::CompositionError::VariableExpressionError
    pub fn expression(
        layer: LayerId,
        asset: impl Into<String>,
        target: ReferenceTarget,
        layer_offset: LayerOffset,
    ) -> Self {
        Self {
            layer,
            target,
            asset: Some(asset.into()),
            layer_offset,
            custom_data: Vec::new(),
        }
    }

    /// Returns `true` when this arc's asset path is a variable expression
    /// (see [`Reference::expression`]).
    #[must_use]
    pub fn is_expression(&self) -> bool {
        self.asset
            .as_deref()
            .is_some_and(crate::variable_expression::is_expression)
    }

    /// Returns `true` when this arc targets no layer stack as authored: its
    /// asset path could not be resolved (see [`Reference::unresolved`]), or
    /// it is a variable expression that composition has yet to evaluate
    /// ([`Reference::is_expression`]).
    #[must_use]
    pub fn is_unresolved(&self) -> bool {
        self.layer == LayerId::UNRESOLVED || self.is_expression()
    }

    /// Returns the prim path this arc targets in the layer stack rooted at
    /// [`Reference::layer`]: the authored path, or the path named by that
    /// layer's `defaultPrim` for a [`ReferenceTarget::DefaultPrim`] target.
    ///
    /// Returns `None` when the arc's asset is unresolved
    /// ([`Reference::is_unresolved`]), or when the target is `DefaultPrim`
    /// and the layer is not in `store` or has no usable `defaultPrim` (see
    /// [`Layer::default_prim_path`]). A returned path need not have a prim
    /// spec.
    ///
    /// Spec: AOUSD Core §10.3.2.1 (an omitted prim path assumes the
    /// `defaultPrim` of the specified layer).
    pub fn target_path(&self, store: &mut dyn LayerStore) -> Option<PathId> {
        if self.is_unresolved() {
            return None;
        }
        match self.target {
            ReferenceTarget::Prim(path) => Some(path),
            ReferenceTarget::DefaultPrim => {
                let default_prim = store.layer(self.layer)?.default_prim?;
                let value = String::from(store.tokens().resolve(default_prim));
                let segments: Vec<TokenId> = default_prim_names(&value)?
                    .into_iter()
                    .map(|name| store.tokens_mut().intern(name))
                    .collect();
                Some(store.paths_mut().intern(Path::root().join(&segments)))
            }
        }
    }
}

/// Opinions for a variant branch.
///
/// A variant spec holds the opinions the branch authors for the prim hosting
/// its variant set, and the variant sets nested in the branch: like
/// OpenUSD's `SdfVariantSpec`, whose prim spec owns the branch's variant
/// sets and namespace children (`pxr/usd/sdf/variantSpec.h`,
/// `pxr/usd/sdf/variantSetSpec.h`), each branch is a spec of its own,
/// addressed by its variant-qualified path (`/P{a=x}`, and `/P{a=x}{b=y}`
/// for a set nested in it). A variant set of one name nested under two
/// branches is two sets. Prims authored inside the branch are not held
/// here: each of them is a [`PrimSpec`] of its own, stored in the [`Layer`]
/// with the branch recorded in its [`PrimSpec::outer_variant_sites`].
///
/// Spec: AOUSD Core §7.3.6 (variant specs may contain any spec a prim spec
/// contains, variant set specs included), §7.6.7 (variant specs),
/// §10.3.2.5 (variants arc).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VariantSpec {
    /// Authored metadata fields on the prim hosting the variant set, within
    /// this variant.
    pub fields: Vec<FieldEntry>,
    /// Authored property specs on the prim hosting the variant set, within
    /// this variant, in authored order.
    ///
    /// Spec: AOUSD Core §7.6.7 (variant specs contribute prim spec fields).
    pub properties: Vec<PropertyEntry>,
    /// Child prim names introduced by this variant branch
    /// (`primChildren`).
    ///
    /// Each child's prim spec is stored in the [`Layer`] at its namespace
    /// path, with this branch as the innermost of its
    /// [`PrimSpec::outer_variant_sites`] (see [`Layer::branch_prim_specs`]).
    /// These children are only populated when this variant is selected.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs contain prim specs).
    pub authored_children: Vec<TokenId>,
    /// References arcs on this variant branch itself.
    ///
    /// When a variant branch header includes composition arcs
    /// (e.g. `"full" (add references = @...@) { ... }`), those arcs apply
    /// to the prim owning the variant set when this variant is selected.
    ///
    /// Spec: AOUSD Core §10.5 (variant arcs).
    pub references: ListOp<Reference>,
    /// Inherits arcs on this variant branch itself.
    pub inherits: ListOp<PathId>,
    /// Specializes arcs on this variant branch itself.
    pub specializes: ListOp<PathId>,
    /// Payloads on this variant branch itself.
    pub payloads: ListOp<Reference>,
    /// Variant selections authored within this variant branch.
    ///
    /// When a variant branch header includes `variants = { string v2 = "b" }`,
    /// those selections apply to the owning prim when this variant is selected.
    pub variant_selections: HashMap<TokenId, TokenId>,
    /// The variant sets nested in this branch (`/P{a=x}{b=}`), keyed by
    /// set name.
    ///
    /// They are sets of the prim hosting this branch, available only while
    /// this branch is selected.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs).
    pub variant_sets: HashMap<TokenId, VariantSetSpec>,
    /// Ordered names of the variant sets this branch declares (its
    /// `variantSets` metadata, then the sets it authors), as
    /// [`PrimSpec::variant_set_order`] is for a prim spec.
    pub variant_set_order: Vec<TokenId>,
    /// Property ordering authored inside this branch (`reorder properties`).
    ///
    /// Spec: AOUSD Core §7.6.7 (variant specs contribute prim spec fields).
    pub property_order: Option<Vec<TokenId>>,
}

/// One variant spec of a prim spec, with the branches of the same prim spec
/// that enclose it (see [`PrimSpec::variant_branches`]).
///
/// The branch's variant-qualified path is the prim spec's path followed by
/// [`VariantBranch::chain`]: `/P{a=x}{b=y}` for the branch `b=y` of a set
/// nested in the branch `a=x` of `/P`.
#[derive(Clone, Debug, PartialEq)]
pub struct VariantBranch<'a> {
    /// The enclosing branches of the same prim spec, outermost first, as
    /// `(set, variant)`: empty for a set declared on the prim spec itself.
    pub enclosing: Vec<(TokenId, TokenId)>,
    /// The variant set name.
    pub set: TokenId,
    /// The variant name.
    pub variant: TokenId,
    /// The variant spec.
    pub spec: &'a VariantSpec,
}

impl VariantBranch<'_> {
    /// Every selection of the branch's path on its prim spec, outermost
    /// first, ending with its own.
    pub fn chain(&self) -> impl Iterator<Item = (TokenId, TokenId)> + '_ {
        self.enclosing
            .iter()
            .copied()
            .chain(core::iter::once((self.set, self.variant)))
    }

    /// The number of enclosing branches of the same prim spec.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.enclosing.len()
    }

    /// Whether `selections` (set → variant) selects this branch and every
    /// branch enclosing it.
    #[must_use]
    pub fn is_selected(&self, selections: &HashMap<TokenId, TokenId>) -> bool {
        self.chain()
            .all(|(set, variant)| selections.get(&set) == Some(&variant))
    }

    /// The variant selection sites naming this branch, for the prim spec at
    /// `host` whose own branch context is `outer` (its
    /// [`PrimSpec::outer_variant_sites`]): `outer`, then this branch's
    /// [`VariantBranch::chain`] hosted on `host`.
    #[must_use]
    pub fn sites(&self, outer: &[VariantSelectionSite], host: PathId) -> Vec<VariantSelectionSite> {
        let mut sites = Vec::with_capacity(outer.len() + self.enclosing.len() + 1);
        sites.extend_from_slice(outer);
        sites.extend(self.chain().map(|(set, variant)| VariantSelectionSite {
            host_path: host,
            set,
            variant,
        }));
        sites
    }
}

/// The variant spec reached from `sets` through `chain`, outermost first.
fn variant_spec_in<'a>(
    sets: &'a HashMap<TokenId, VariantSetSpec>,
    chain: &[(TokenId, TokenId)],
) -> Option<&'a VariantSpec> {
    let ((set, variant), rest) = chain.split_first()?;
    let spec = sets.get(set)?.variants.get(variant)?;
    if rest.is_empty() {
        Some(spec)
    } else {
        variant_spec_in(&spec.variant_sets, rest)
    }
}

/// The variant spec reached from `sets` through `chain`, mutably.
fn variant_spec_in_mut<'a>(
    sets: &'a mut HashMap<TokenId, VariantSetSpec>,
    chain: &[(TokenId, TokenId)],
) -> Option<&'a mut VariantSpec> {
    let ((set, variant), rest) = chain.split_first()?;
    let spec = sets.get_mut(set)?.variants.get_mut(variant)?;
    if rest.is_empty() {
        Some(spec)
    } else {
        variant_spec_in_mut(&mut spec.variant_sets, rest)
    }
}

/// Iterates the variant specs of a prim spec, each before those nested in
/// it (see [`PrimSpec::variant_branches`] and
/// [`PrimSpec::selected_variant_branches`]).
#[derive(Clone, Debug)]
pub struct VariantBranches<'a> {
    /// Branches still to visit.
    pending: Vec<VariantBranch<'a>>,
    /// When set, only the branches it selects are visited.
    selections: Option<&'a HashMap<TokenId, TokenId>>,
}

impl<'a> VariantBranches<'a> {
    fn new(
        sets: &'a HashMap<TokenId, VariantSetSpec>,
        selections: Option<&'a HashMap<TokenId, TokenId>>,
    ) -> Self {
        let mut branches = Self {
            pending: Vec::new(),
            selections,
        };
        branches.push_sets(&[], sets);
        branches
    }

    fn push_sets(
        &mut self,
        enclosing: &[(TokenId, TokenId)],
        sets: &'a HashMap<TokenId, VariantSetSpec>,
    ) {
        for (set, set_spec) in sets {
            match self.selections {
                Some(selections) => {
                    let Some(variant) = selections.get(set) else {
                        continue;
                    };
                    if let Some(spec) = set_spec.variants.get(variant) {
                        self.pending.push(VariantBranch {
                            enclosing: enclosing.to_vec(),
                            set: *set,
                            variant: *variant,
                            spec,
                        });
                    }
                }
                None => {
                    for (variant, spec) in &set_spec.variants {
                        self.pending.push(VariantBranch {
                            enclosing: enclosing.to_vec(),
                            set: *set,
                            variant: *variant,
                            spec,
                        });
                    }
                }
            }
        }
    }
}

impl<'a> Iterator for VariantBranches<'a> {
    type Item = VariantBranch<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let branch = self.pending.pop()?;
        if !branch.spec.variant_sets.is_empty() {
            let chain: Vec<(TokenId, TokenId)> = branch.chain().collect();
            self.push_sets(&chain, &branch.spec.variant_sets);
        }
        Some(branch)
    }
}

/// One authored entry of a spec that composition turns into an opinion: a
/// metadata field or a property.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ComposedEntry<'a> {
    Field(&'a FieldEntry),
    Property(&'a PropertyEntry),
}

impl<'a> ComposedEntry<'a> {
    /// The field or property name.
    pub(crate) fn name(self) -> TokenId {
        match self {
            Self::Field(entry) => entry.name,
            Self::Property(entry) => entry.name,
        }
    }

    /// The opinion payload for this entry.
    pub(crate) fn value(self) -> OpinionValue {
        match self {
            Self::Field(entry) => OpinionValue::Field(entry.value.clone()),
            Self::Property(entry) => OpinionValue::Property(Arc::clone(&entry.spec)),
        }
    }
}

/// Iterates the metadata fields, then the properties, of one spec.
pub(crate) fn composed_entries<'a>(
    fields: &'a [FieldEntry],
    properties: &'a [PropertyEntry],
) -> impl Iterator<Item = ComposedEntry<'a>> {
    fields
        .iter()
        .map(ComposedEntry::Field)
        .chain(properties.iter().map(ComposedEntry::Property))
}

/// A variant set: a named collection of variants.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VariantSetSpec {
    /// Variants keyed by variant name.
    pub variants: HashMap<TokenId, VariantSpec>,
}

/// Opinions for a prim at a path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrimSpec {
    /// The prim specifier (`def`, `over`, or `class`).
    ///
    /// Spec: AOUSD Core §7.6 (specifier field), §12.2.1 (specifier resolution).
    pub specifier: Option<Specifier>,
    /// The prim type name (e.g. `Xform`, `Mesh`, `Scope`).
    ///
    /// Type name resolution uses strongest-defining-opinion-wins: the first
    /// opinion (in strength order) with a non-`None` type name determines the
    /// composed type.
    ///
    /// Spec: AOUSD Core §7.6 (typeName field), §12.2.3 (type name resolution).
    pub type_name: Option<TokenId>,
    /// Authored metadata fields not modeled by a dedicated member, in
    /// authored order: for example `kind`, `apiSchemas`, `hidden`,
    /// `customData`, `assetInfo`, `displayName`, `documentation` and
    /// `comment`.
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields).
    pub fields: Vec<FieldEntry>,
    /// Authored property specs, in authored order (`propertyChildren`).
    ///
    /// Spec: AOUSD Core §7.6.2.2.2 (`propertyChildren`), §7.3.7.
    pub properties: Vec<PropertyEntry>,
    /// Authored property ordering (`reorder properties = [...]`).
    ///
    /// OpenUSD sorts composed property names and then applies the strongest
    /// `propertyOrder` opinion (`UsdPrim::ApplyPropertyOrder` in
    /// `pxr/usd/usd/prim.cpp`).
    pub property_order: Option<Vec<TokenId>>,
    /// Variant selections required to reach this concrete prim spec in its
    /// defining layer.
    ///
    /// This preserves composed provenance for prims authored beneath variant
    /// branches so descendant local opinions can retain their variant-qualified
    /// source identity.
    pub outer_variant_sites: Vec<VariantSelectionSite>,
    /// Authored child prim names in this layer, in file order.
    ///
    /// This is used as a deterministic baseline for child ordering. Child
    /// ordering is then further refined by applying `prim_order` (`reorder
    /// nameChildren`) opinions across the prim stack.
    ///
    /// Spec: AOUSD Core §11 (stage population) and supplemental suite child
    /// ordering tests (e.g. `BasicListEditingWithInherits_root`).
    pub authored_children: Vec<TokenId>,
    /// Authored variant selections (set -> chosen variant).
    pub variant_selections: HashMap<TokenId, TokenId>,
    /// Authored variant sets of this prim spec (`/P{v=}`), keyed by set
    /// name. Sets nested in a branch belong to that branch's
    /// [`VariantSpec::variant_sets`]; [`PrimSpec::variant_branches`] walks
    /// them all.
    ///
    /// Spec: AOUSD Core §7.3.6 (prim specs contain variant set specs).
    pub variant_sets: HashMap<TokenId, VariantSetSpec>,
    /// Ordered names of the variant sets this prim spec declares (its
    /// `variantSets` metadata, then the sets it authors).
    ///
    /// This determines the evaluation order for variant children.
    /// Children from later variant sets appear before earlier ones.
    pub variant_set_order: Vec<TokenId>,
    /// Variant set names this spec's `variantSets` list op deletes: a
    /// weaker opinion's declaration of them is removed, as a list op
    /// removes deleted items (so composition does not evaluate their
    /// selections there).
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §12.4 (list ops).
    /// OpenUSD: `PcpComposeSiteVariantSets` in `pxr/usd/pcp/composeSite.cpp`.
    pub deleted_variant_sets: Vec<TokenId>,
    /// References arcs (a `ListOp` chain across the layer stack).
    pub references: ListOp<Reference>,
    /// Inherits arcs (a `ListOp` chain across the layer stack).
    ///
    /// Spec: AOUSD Core §10 (inherits arc), with ordering via §12.4 (`ListOps`).
    pub inherits: ListOp<PathId>,
    /// Specializes arcs (a `ListOp` chain across the layer stack).
    ///
    /// Specializes is similar to inherits but sits at the weakest position in
    /// LIVERPS. Unlike references, specializes propagates through all levels of
    /// referencing.
    ///
    /// Spec: AOUSD Core §10 (specializes arc, §5.1.33), with ordering via §12.4 (`ListOps`).
    pub specializes: ListOp<PathId>,
    /// Payloads arcs (a `ListOp` chain across the layer stack).
    ///
    /// Payloads are structurally identical to references but support deferred
    /// loading. When loaded, they behave like references with the same namespace
    /// mapping. Their position in LIVERPS is between References and Specializes.
    ///
    /// Spec: AOUSD Core §10 (payloads arc, §5.1.22).
    pub payloads: ListOp<Reference>,
    /// Optional child ordering (aka `primOrder` in the supplemental suite).
    ///
    /// This is used during stage population to produce deterministic, authored
    /// child ordering (rather than purely lexicographic ordering).
    ///
    /// Spec: AOUSD Core §11 (stage population), plus the supplemental
    /// parser’s `primOrder` field (`reorder nameChildren = [...]`).
    pub prim_order: Option<Vec<TokenId>>,
    /// Whether this prim is instanceable.
    ///
    /// When `true` and the prim has composition arcs (references, payloads),
    /// descendant local opinions are stripped — only opinions from composition
    /// arc targets survive. This enables prototype sharing for identical
    /// composition structures.
    ///
    /// Spec: AOUSD Core §11 (instancing), §5.1.14 (instanceable).
    pub instanceable: Option<bool>,
    /// Whether this prim is active.
    ///
    /// When `false`, the prim and all its namespace descendants are excluded
    /// from the composed stage. The strongest opinion wins.
    ///
    /// Spec: AOUSD Core §7.6 (active metadata), §11 (stage population).
    pub active: Option<bool>,
}

impl PrimSpec {
    /// Creates a prim spec with `specifier = def`.
    pub fn def() -> Self {
        Self {
            specifier: Some(Specifier::Def),
            ..Self::default()
        }
    }

    /// Creates a prim spec with `specifier = over`.
    pub fn over() -> Self {
        Self {
            specifier: Some(Specifier::Over),
            ..Self::default()
        }
    }

    /// Creates a prim spec with `specifier = class`.
    pub fn class() -> Self {
        Self {
            specifier: Some(Specifier::Class),
            ..Self::default()
        }
    }

    /// Sets the prim type name (builder, consuming).
    ///
    /// Spec: AOUSD Core §7.6 (typeName field).
    pub fn with_type_name(mut self, type_name: TokenId) -> Self {
        self.type_name = Some(type_name);
        self
    }

    /// Inserts or replaces a field value, returning `&mut Self` for chaining.
    pub fn set_field(&mut self, token: TokenId, value: impl Into<FieldValue>) -> &mut Self {
        set_field_vec(&mut self.fields, token, value.into());
        self
    }

    /// Inserts or replaces a field value (builder, consuming).
    pub fn with_field(mut self, token: TokenId, value: impl Into<FieldValue>) -> Self {
        set_field_vec(&mut self.fields, token, value.into());
        self
    }

    /// Returns an authored metadata field, if present.
    #[must_use]
    pub fn field(&self, token: TokenId) -> Option<&FieldValue> {
        get_field(&self.fields, &token)
    }

    /// Inserts or replaces a property spec, returning `&mut Self` for
    /// chaining. A replaced property keeps its authored position.
    pub fn set_property(&mut self, name: TokenId, spec: PropertySpec) -> &mut Self {
        set_property_vec(&mut self.properties, name, spec);
        self
    }

    /// Inserts or replaces a property spec (builder, consuming).
    pub fn with_property(mut self, name: TokenId, spec: PropertySpec) -> Self {
        set_property_vec(&mut self.properties, name, spec);
        self
    }

    /// Returns the authored property named `name`, if present.
    #[must_use]
    pub fn property(&self, name: TokenId) -> Option<&PropertySpec> {
        get_property(&self.properties, name)
    }

    /// Returns the authored property named `name` mutably, if present.
    pub fn property_mut(&mut self, name: TokenId) -> Option<&mut PropertySpec> {
        get_property_mut(&mut self.properties, name)
    }

    /// Removes the property named `name`, returning its spec.
    pub fn remove_property(&mut self, name: TokenId) -> Option<PropertySpec> {
        remove_property(&mut self.properties, name)
    }

    /// Appends a reference arc.
    pub fn add_reference(&mut self, reference: Reference) -> &mut Self {
        self.references.append.push(reference);
        self
    }

    /// Appends a reference arc (builder, consuming).
    pub fn with_reference(mut self, reference: Reference) -> Self {
        self.references.append.push(reference);
        self
    }

    /// Appends an inherit arc.
    pub fn add_inherit(&mut self, path: PathId) -> &mut Self {
        self.inherits.append.push(path);
        self
    }

    /// Appends an inherit arc (builder, consuming).
    pub fn with_inherit(mut self, path: PathId) -> Self {
        self.inherits.append.push(path);
        self
    }

    /// Appends a payload arc.
    pub fn add_payload(&mut self, payload: Reference) -> &mut Self {
        self.payloads.append.push(payload);
        self
    }

    /// Appends a payload arc (builder, consuming).
    pub fn with_payload(mut self, payload: Reference) -> Self {
        self.payloads.append.push(payload);
        self
    }

    /// Appends a specialize arc.
    pub fn add_specialize(&mut self, path: PathId) -> &mut Self {
        self.specializes.append.push(path);
        self
    }

    /// Appends a specialize arc (builder, consuming).
    pub fn with_specialize(mut self, path: PathId) -> Self {
        self.specializes.append.push(path);
        self
    }

    /// Sets the authored children list (builder, consuming).
    pub fn with_children(mut self, children: Vec<TokenId>) -> Self {
        self.authored_children = children;
        self
    }

    /// Marks this prim as instanceable (or not).
    pub fn with_instanceable(mut self, instanceable: bool) -> Self {
        self.instanceable = Some(instanceable);
        self
    }

    /// Marks this prim as active (or not).
    ///
    /// When `false`, the prim and all its namespace descendants are excluded
    /// from the composed stage.
    ///
    /// Spec: AOUSD Core §7.6 (active metadata).
    pub fn with_active(mut self, active: bool) -> Self {
        self.active = Some(active);
        self
    }

    /// Iterates every variant spec of this prim spec, those of the variant
    /// sets nested in a branch included, each before the sets nested in
    /// it.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs).
    pub fn variant_branches(&self) -> VariantBranches<'_> {
        VariantBranches::new(&self.variant_sets, None)
    }

    /// Iterates the variant specs `selections` (set → variant) selects:
    /// the selected branch of each of this prim spec's sets, then of each
    /// set nested in a selected branch. A branch whose enclosing branch is
    /// not selected is not visited, whatever its own set selects.
    ///
    /// Spec: AOUSD Core §10.3.2.5 (only the selected variant contributes).
    pub fn selected_variant_branches<'a>(
        &'a self,
        selections: &'a HashMap<TokenId, TokenId>,
    ) -> VariantBranches<'a> {
        VariantBranches::new(&self.variant_sets, Some(selections))
    }

    /// Returns the variant spec at `chain` (outermost first) below this
    /// prim spec: `[(a, x), (b, y)]` names `{a=x}{b=y}`. `None` for an
    /// empty chain or one this prim spec does not author.
    #[must_use]
    pub fn variant_spec(&self, chain: &[(TokenId, TokenId)]) -> Option<&VariantSpec> {
        variant_spec_in(&self.variant_sets, chain)
    }

    /// Returns the variant spec at `chain` mutably (see
    /// [`PrimSpec::variant_spec`]).
    pub fn variant_spec_mut(&mut self, chain: &[(TokenId, TokenId)]) -> Option<&mut VariantSpec> {
        variant_spec_in_mut(&mut self.variant_sets, chain)
    }

    /// Returns the variant sets this prim spec declares under `selections`
    /// (set → variant), in `variantSets` order: its own, then those each
    /// selected branch nests, visiting the branches in their sets' declared
    /// order.
    ///
    /// Only selected branches count, each in turn, so two branches that
    /// declare the same nested sets in different orders do not interfere.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs), §10.3.2.5 (variant sets are evaluated in `variantSetNames`
    /// order).
    pub(crate) fn selected_variant_set_order(
        &self,
        selections: &HashMap<TokenId, TokenId>,
    ) -> Vec<TokenId> {
        fn visit(
            sets: &HashMap<TokenId, VariantSetSpec>,
            order: &[TokenId],
            selections: &HashMap<TokenId, TokenId>,
            out: &mut Vec<TokenId>,
        ) {
            for set in order {
                if !out.contains(set) {
                    out.push(*set);
                }
            }
            for set in order {
                let branch = selections
                    .get(set)
                    .and_then(|variant| sets.get(set)?.variants.get(variant));
                if let Some(branch) = branch {
                    visit(
                        &branch.variant_sets,
                        &branch.variant_set_order,
                        selections,
                        out,
                    );
                }
            }
        }
        let mut sets = Vec::new();
        visit(
            &self.variant_sets,
            &self.variant_set_order,
            selections,
            &mut sets,
        );
        sets
    }

    /// Returns the variant sets declared at `chain` below this prim spec,
    /// with their `variantSets` order: this prim spec's own for an empty
    /// chain, otherwise those nested in the variant spec it names.
    #[must_use]
    pub fn variant_sets_in(
        &self,
        chain: &[(TokenId, TokenId)],
    ) -> Option<(&HashMap<TokenId, VariantSetSpec>, &[TokenId])> {
        if chain.is_empty() {
            return Some((&self.variant_sets, &self.variant_set_order));
        }
        let spec = self.variant_spec(chain)?;
        Some((&spec.variant_sets, &spec.variant_set_order))
    }
}

/// Inserts or replaces a field in a `Vec<FieldEntry>` by name.
///
/// If a field with the given name already exists, its value is replaced in
/// place. Otherwise a new entry is appended.
pub fn set_field_vec(fields: &mut Vec<FieldEntry>, name: TokenId, value: FieldValue) {
    if let Some(entry) = fields.iter_mut().find(|e| e.name == name) {
        entry.value = value;
    } else {
        fields.push(FieldEntry { name, value });
    }
}

/// Returns a shared reference to the value of a field, if present.
pub fn get_field<'a>(fields: &'a [FieldEntry], name: &TokenId) -> Option<&'a FieldValue> {
    fields.iter().find(|e| &e.name == name).map(|e| &e.value)
}

/// Returns a mutable reference to the value of a field, if present.
pub fn get_field_mut<'a>(
    fields: &'a mut [FieldEntry],
    name: &TokenId,
) -> Option<&'a mut FieldValue> {
    fields
        .iter_mut()
        .find(|e| &e.name == name)
        .map(|e| &mut e.value)
}

/// Splits a `defaultPrim` value into the prim names of the path it names,
/// or returns `None` when it does not name a prim path (see
/// [`Layer::default_prim_path`]).
pub(crate) fn default_prim_names(value: &str) -> Option<Vec<&str>> {
    let relative = value.strip_prefix('/').unwrap_or(value);
    if relative.is_empty() {
        return None;
    }
    let names: Vec<&str> = relative.split('/').collect();
    names.iter().all(|name| is_prim_name(name)).then_some(names)
}

/// Returns `true` when `name` can be a prim name: non-empty, not starting
/// with an ASCII digit, and free of whitespace and of ASCII punctuation
/// other than `_`. A structural check of the identifier grammar (AOUSD Core
/// §7.3.3) that accepts other non-ASCII characters.
fn is_prim_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let allowed =
        |c: char| c == '_' || c.is_ascii_alphanumeric() || (!c.is_ascii() && !c.is_whitespace());
    !first.is_ascii_digit() && allowed(first) && chars.all(allowed)
}

/// Inserts a field only if no entry with the same name exists.
pub fn insert_field_if_absent(fields: &mut Vec<FieldEntry>, name: TokenId, value: FieldValue) {
    if !fields.iter().any(|e| e.name == name) {
        fields.push(FieldEntry { name, value });
    }
}

/// Removes a field by name, returning its value.
pub fn remove_field(fields: &mut Vec<FieldEntry>, name: TokenId) -> Option<FieldValue> {
    let index = fields.iter().position(|e| e.name == name)?;
    Some(fields.remove(index).value)
}

/// A document layer.
///
/// Two layers are equal when their content is: the [`Layer::generation`]
/// and [`Layer::structural_generation`] counters are not compared.
#[derive(Clone, Debug)]
pub struct Layer {
    /// Stable identifier for this layer.
    pub id: LayerId,
    /// Ordered sublayer includes. The layer itself is always stronger than its sublayers.
    pub sublayers: Vec<SublayerEntry>,
    /// The authored `defaultPrim` token: the prim that references and
    /// payloads with no authored prim path target in this layer.
    ///
    /// The token is kept as authored, so a value that names no prim path is
    /// still visible; [`Layer::default_prim_path`] converts it.
    ///
    /// Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim`), §10.3.2.1 (references).
    pub default_prim: Option<TokenId>,
    /// All other authored layer metadata, in authored order: for example
    /// `upAxis`, `metersPerUnit`, `timeCodesPerSecond`, `framesPerSecond`,
    /// `startTimeCode`, `endTimeCode`, `customLayerData`, `documentation`
    /// and `comment`.
    ///
    /// Spec: AOUSD Core §7.6.1 (layer spec fields), §12.2.7 (layer metadata
    /// is read from the root layer, not composed).
    pub metadata: Vec<FieldEntry>,
    /// The authored `layerRelocates` entries, in authored order.
    ///
    /// Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`), §10.3.2.6
    /// (relocates are computed per layer stack from every layer's entries).
    pub relocates: Vec<Relocate>,
    /// Prim specs keyed by prim path.
    ///
    /// Each path holds one spec here: the spec authored outside any variant
    /// branch if there is one, otherwise the first spec ingested for it.
    /// Specs authored for the same path inside other variant branches are
    /// kept in [`Layer::variant_prims`]; see [`Layer::prim_specs`].
    pub prims: HashMap<PathId, PrimSpec>,
    /// Further prim specs at a path already in [`Layer::prims`], each
    /// authored inside a different variant branch and identified by its
    /// [`PrimSpec::outer_variant_sites`].
    ///
    /// Branches of one variant set may author the same descendant
    /// (`/Model{lod=high}Geom` and `/Model{lod=low}Geom`); each branch keeps
    /// its own spec so composition can use the selected one.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs contain their own prim specs),
    /// §10.5 (only the selected variant contributes).
    pub variant_prims: HashMap<PathId, Vec<PrimSpec>>,
    /// Counts the edits made to this layer (see [`Layer::generation`]).
    pub(crate) generation: u64,
    /// Counts the edits that may change namespace or arcs (see
    /// [`Layer::structural_generation`]).
    pub(crate) structure: u64,
}

impl PartialEq for Layer {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            id,
            sublayers,
            default_prim,
            metadata,
            prims,
            variant_prims,
            relocates,
            generation: _,
            structure: _,
        } = self;
        *id == other.id
            && *sublayers == other.sublayers
            && *default_prim == other.default_prim
            && *metadata == other.metadata
            && *prims == other.prims
            && *variant_prims == other.variant_prims
            && *relocates == other.relocates
    }
}

impl Layer {
    /// Creates an empty layer with no sublayers or prims.
    pub fn new(id: LayerId) -> Self {
        Self {
            id,
            sublayers: Vec::new(),
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
        }
    }

    /// The layer's time rate: authored `timeCodesPerSecond`, otherwise
    /// `framesPerSecond`, otherwise 24. This is a layer-local value; composition
    /// converts child-layer time into parent time through their rate ratio.
    ///
    /// Returns authored numerical metadata as given. Hosts can validate rates
    /// before composition; this accessor does not sanitize invalid opinions.
    /// Spec: AOUSD Core §7.6.1 (layer metadata), §12.3.2.1 (layer time).
    /// OpenUSD: `SdfLayer::GetTimeCodesPerSecond`.
    #[must_use]
    pub fn time_codes_per_second(&self, tokens: &TokenInterner) -> f64 {
        let rate = |name| {
            let field = get_field(&self.metadata, &tokens.lookup(name)?)?;
            match field {
                FieldValue::Value(Value::Double(v)) => Some(*v),
                FieldValue::Value(Value::Float(v)) => Some(f64::from(*v)),
                FieldValue::Value(Value::Int(v)) => Some(f64::from(*v)),
                _ => None,
            }
        };
        rate("timeCodesPerSecond")
            .or_else(|| rate("framesPerSecond"))
            .unwrap_or(24.0)
    }

    /// Returns this layer's generation: a counter that every edit made
    /// through the layer's own methods moves forward.
    ///
    /// A host records the generation when it prepares an edit and checks it
    /// when it applies the edit, so an edit prepared against content that
    /// has changed since fails even when the value it would overwrite is
    /// back to what it was.
    ///
    /// The count only moves forward and is not part of the layer's content:
    /// it is not saved, and [`PartialEq`] ignores it.
    ///
    /// Writes straight into the public fields ([`Layer::prims`] and the
    /// others) bypass the counter: they are the importers' building API,
    /// not an authoring API. Hosts that write a field after composition
    /// call [`Layer::touch`].
    ///
    /// OpenUSD reports the same edits as `SdfNotice::LayersDidChange`.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns this layer's structural generation: a counter that the
    /// edits made through the layer's own methods move forward when they
    /// may change more than opinion values: add or remove prim specs,
    /// child lists, composition arcs, variant sets or layer metadata.
    ///
    /// Every edit that moves it also moves [`Layer::generation`]. A stage
    /// that finds only [`Layer::generation`] moved can recompose the prims
    /// drawing on the layer; one that finds this moved must recompose its
    /// namespace.
    ///
    /// OpenUSD tells these apart in `SdfChangeList` entries: a spec added or
    /// removed, or a composition field changed, is a significant change that
    /// resyncs prims; a field value change only changes the prims' info.
    #[must_use]
    pub fn structural_generation(&self) -> u64 {
        self.structure
    }

    /// Moves [`Layer::generation`] and [`Layer::structural_generation`]
    /// forward, for an edit of unknown kind made through the public fields.
    pub fn touch(&mut self) {
        self.touch_structure();
    }

    /// Moves [`Layer::generation`] forward, for an edit of opinion values
    /// only.
    pub(crate) fn touch_values(&mut self) {
        self.generation += 1;
    }

    /// Moves both counters forward, for an edit that may change namespace
    /// or arcs.
    pub(crate) fn touch_structure(&mut self) {
        self.generation += 1;
        self.structure += 1;
    }

    /// Inserts a prim spec at the given path, replacing the spec authored in
    /// the same variant branch context ([`PrimSpec::outer_variant_sites`]).
    ///
    /// A spec from a different branch context is kept next to the existing
    /// one (see [`Layer::variant_prims`]) instead of replacing it; a spec
    /// authored outside any variant branch takes the [`Layer::prims`] slot.
    pub fn insert_prim(&mut self, path: PathId, spec: PrimSpec) {
        self.touch_structure();
        let displaced = match self.prims.get(&path) {
            Some(existing) if existing.outer_variant_sites == spec.outer_variant_sites => None,
            Some(_) if !spec.outer_variant_sites.is_empty() => {
                insert_branch_spec(self.variant_prims.entry(path).or_default(), spec);
                return;
            }
            Some(_) => self.prims.remove(&path),
            None => None,
        };
        self.prims.insert(path, spec);
        if let Some(displaced) = displaced {
            insert_branch_spec(self.variant_prims.entry(path).or_default(), displaced);
        }
    }

    /// Returns the absolute prim path named by [`Layer::default_prim`], or
    /// `None` when `defaultPrim` is not authored or is not a prim path.
    ///
    /// The token is a root prim name (`Model`) or a prim path, either
    /// absolute (`/Model/Geo`) or relative to the pseudo-root (`Model/Geo`).
    /// Every name must be an identifier, so property paths, variant
    /// selections, `.` and `..` components and the pseudo-root itself are
    /// not prim paths. Identifiers are checked structurally: a name may not
    /// start with an ASCII digit or contain whitespace or ASCII punctuation
    /// other than `_`; other non-ASCII characters are accepted.
    ///
    /// Spec: AOUSD Core §7.6.1.2.3 ("If it starts with /, it is already an
    /// absolute path. Otherwise, it's a path relative to /"). OpenUSD:
    /// `SdfLayer::ConvertDefaultPrimTokenToPath` (`pxr/usd/sdf/layer.cpp`).
    ///
    /// ```
    /// use layerstack::{Layer, LayerId, TokenInterner};
    ///
    /// let mut tokens = TokenInterner::default();
    /// let mut layer = Layer::new(LayerId(1));
    /// layer.default_prim = Some(tokens.intern("Model/Geo"));
    /// let path = layer.default_prim_path(&mut tokens).unwrap();
    /// assert_eq!(path.display(&tokens), "/Model/Geo");
    ///
    /// layer.default_prim = Some(tokens.intern("Model.attr"));
    /// assert!(layer.default_prim_path(&mut tokens).is_none());
    /// ```
    pub fn default_prim_path(&self, tokens: &mut TokenInterner) -> Option<Path> {
        let value = String::from(tokens.resolve(self.default_prim?));
        let segments: Vec<TokenId> = default_prim_names(&value)?
            .into_iter()
            .map(|name| tokens.intern(name))
            .collect();
        Some(Path::root().join(&segments))
    }

    /// Returns every prim spec this layer authors at `path`: the
    /// [`Layer::prims`] spec, then those of other variant branches.
    pub fn prim_specs(&self, path: PathId) -> impl Iterator<Item = &PrimSpec> {
        self.prims
            .get(&path)
            .into_iter()
            .chain(self.variant_prims.get(&path).into_iter().flatten())
    }

    /// Returns the prim spec at `path` authored in the variant branch
    /// context `sites` (empty for a spec outside any branch).
    #[must_use]
    pub fn prim_spec_in(&self, path: PathId, sites: &[VariantSelectionSite]) -> Option<&PrimSpec> {
        self.prim_specs(path)
            .find(|spec| spec.outer_variant_sites == sites)
    }

    /// Returns the prim specs at `path` authored directly inside the
    /// variant branch `branch`: those whose innermost enclosing selection
    /// ([`PrimSpec::outer_variant_sites`]) is `branch`, whatever branches
    /// enclose it in turn.
    ///
    /// For a child `C` of the prim hosting `branch` this is the spec at
    /// `/P{v=x}C` (and, for a variant set nested in another branch of `P`,
    /// at `/P{a=y}{v=x}C`); for a deeper descendant it is the spec at
    /// `/P{v=x}C/G`.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs contain prim specs).
    pub fn branch_prim_specs(
        &self,
        path: PathId,
        branch: VariantSelectionSite,
    ) -> impl Iterator<Item = &PrimSpec> {
        self.prim_specs(path)
            .filter(move |spec| spec.outer_variant_sites.last() == Some(&branch))
    }

    /// Returns the prim specs at `path` authored directly in a branch of
    /// `host`'s variant sets (the innermost of their
    /// [`PrimSpec::outer_variant_sites`] is hosted on `host`) whose every
    /// enclosing branch hosted on `host` `selections` (set → variant, for
    /// `host`) selects.
    ///
    /// A variant set nested in another branch of `host` may reuse its branch
    /// names under several outer branches (`/P{a=x}{b=y}C` and
    /// `/P{a=z}{b=y}C`); only the spec whose outer branches are selected
    /// too is returned, never one selected by its innermost branch alone.
    /// Branches hosted on other prims (`/A{v=x}P{a=y}C` has one on `/A`) are
    /// not checked here; callers check them against those hosts'
    /// selections.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs), §10.3.2.5 (only the selected variant contributes).
    pub fn selected_branch_prim_specs<'a>(
        &'a self,
        path: PathId,
        host: PathId,
        selections: &'a HashMap<TokenId, TokenId>,
    ) -> impl Iterator<Item = &'a PrimSpec> {
        self.prim_specs(path).filter(move |spec| {
            spec.outer_variant_sites
                .last()
                .is_some_and(|site| site.host_path == host)
                && spec
                    .outer_variant_sites
                    .iter()
                    .filter(|site| site.host_path == host)
                    .all(|site| selections.get(&site.set) == Some(&site.variant))
        })
    }

    /// Returns the prim spec a composed opinion source names: the spec at
    /// `lookup_path` whose variant branch context matches every variant
    /// selection in `spec_path` enclosing that prim.
    ///
    /// Selections on the prim itself (`/P{v=b}`) name one of that spec's
    /// variants, not another spec, so they do not take part. When no spec
    /// has exactly that context (for example a source whose spec path was
    /// remapped across an arc), the [`Layer::prims`] spec is returned.
    ///
    /// Spec: AOUSD Core §7.3.6, §10.5.
    pub(crate) fn source_prim_spec(
        &self,
        lookup_path: PathId,
        spec_path: &SpecPath,
        paths: &PathInterner,
    ) -> Option<&PrimSpec> {
        if self.variant_prims.get(&lookup_path).is_none() {
            return self.prims.get(&lookup_path);
        }
        self.branch_prim_spec(lookup_path, spec_path, paths)
            .or_else(|| self.prims.get(&lookup_path))
    }

    /// Returns the prim spec at `lookup_path` authored in exactly the
    /// variant branch context the selections in `spec_path` enclosing that
    /// prim name, or `None` when this layer authors none there.
    ///
    /// Unlike [`Layer::source_prim_spec`] this never falls back to another
    /// branch's spec: a spec path outside any branch names only a spec
    /// outside any branch, and `/P{v=b}C` only the spec the branch `v=b`
    /// of `/P` holds.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs contain their own prim
    /// specs), §10.3.2.5 (only the selected variant contributes).
    pub(crate) fn branch_prim_spec(
        &self,
        lookup_path: PathId,
        spec_path: &SpecPath,
        paths: &PathInterner,
    ) -> Option<&PrimSpec> {
        let depth = paths.resolve(lookup_path).depth();
        let mut segments = Vec::new();
        let mut sites = Vec::new();
        for component in spec_path.components() {
            match *component {
                SpecComponent::Prim(segment) => segments.push(segment),
                SpecComponent::VariantSelection { set, variant } => {
                    if segments.len() >= depth {
                        break;
                    }
                    let Some(host_path) = paths.lookup(&Path::root().join(&segments)) else {
                        continue;
                    };
                    sites.push(VariantSelectionSite {
                        host_path,
                        set,
                        variant,
                    });
                }
            }
        }
        self.prim_spec_in(lookup_path, &sites)
    }

    /// Returns the variant spec at the variant-qualified `spec_path`
    /// (`/P{a=x}`, `/P{a=x}{b=y}`, or `/A{v=x}P{a=y}` for a set on a prim
    /// inside a branch), or `None` when this layer authors none there or
    /// `spec_path` does not end in a variant selection. A property suffix
    /// is ignored.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs), §7.6.7 (variant specs). OpenUSD addresses each `SdfVariantSpec`
    /// by such a path (`SdfPath::IsPrimVariantSelectionPath`,
    /// `pxr/usd/sdf/variantSpec.h`).
    #[must_use]
    pub fn variant_spec_at(
        &self,
        spec_path: &SpecPath,
        paths: &PathInterner,
    ) -> Option<&VariantSpec> {
        let chain = spec_path.variant_chain();
        if chain.is_empty() {
            return None;
        }
        self.branch_prim_spec(spec_path.prim_path(), spec_path, paths)?
            .variant_spec(&chain)
    }

    /// Returns the variant sets of the prim or variant spec at the
    /// variant-qualified `spec_path`, with their `variantSets` order: those
    /// of the prim spec for `/P` (or `/A{v=x}P`), those nested in the
    /// variant spec for `/P{a=x}`. `None` when this layer authors no spec
    /// there.
    ///
    /// Spec: AOUSD Core §7.3.6 (prim and variant specs contain variant set
    /// specs). OpenUSD: `SdfPrimSpec::GetVariantSets` and
    /// `SdfVariantSpec::GetVariantSets`.
    #[must_use]
    pub fn variant_sets_at(
        &self,
        spec_path: &SpecPath,
        paths: &PathInterner,
    ) -> Option<(&HashMap<TokenId, VariantSetSpec>, &[TokenId])> {
        let chain = spec_path.variant_chain();
        self.branch_prim_spec(spec_path.prim_path(), spec_path, paths)?
            .variant_sets_in(&chain)
    }

    /// Returns an authored layer metadata field, if present.
    #[must_use]
    pub fn metadata(&self, key: TokenId) -> Option<&FieldValue> {
        get_field(&self.metadata, &key)
    }

    /// Returns the expression variables this layer authors in its
    /// `expressionVariables` metadata; empty when it authors none.
    ///
    /// Strings, booleans, integers and arrays of one of those convert to
    /// [`VariableValue::Value`](crate::variable_expression::VariableValue),
    /// an authored `None` to `VariableValue::None`, and any other value to
    /// `VariableValue::Unsupported`. Only the root layer of a layer stack
    /// provides its variables (see [`crate::variable_expression`]).
    ///
    /// Spec: AOUSD Core §7.6.1.7 reserves `expressionVariables`. OpenUSD:
    /// `SdfLayer::GetExpressionVariables`.
    #[must_use]
    pub fn expression_variables(
        &self,
        tokens: &TokenInterner,
    ) -> crate::variable_expression::ExpressionVariables {
        crate::expression_variables::layer_expression_variables(self, tokens)
    }

    /// Inserts or replaces a layer metadata field.
    pub fn set_metadata(&mut self, key: TokenId, value: impl Into<FieldValue>) -> &mut Self {
        self.touch_structure();
        set_field_vec(&mut self.metadata, key, value.into());
        self
    }

    /// Inserts or replaces a property spec by concrete [`PropertyPath`].
    ///
    /// If the owning prim does not yet exist in this layer, a default
    /// [`PrimSpec`] is created first, which is a structural edit (see
    /// [`Layer::structural_generation`]).
    pub fn set_property(&mut self, property_path: PropertyPath, spec: PropertySpec) -> &mut Self {
        if self.prims.contains_key(&property_path.prim_path()) {
            self.touch_values();
        } else {
            self.touch_structure();
        }
        self.prims
            .entry(property_path.prim_path())
            .or_default()
            .set_property(property_path.property(), spec);
        self
    }

    /// Returns the property spec at `property_path`, if authored in this
    /// layer.
    #[must_use]
    pub fn property(&self, property_path: PropertyPath) -> Option<&PropertySpec> {
        self.prims
            .get(&property_path.prim_path())?
            .property(property_path.property())
    }

    /// Returns the property spec at `property_path` mutably, if authored in
    /// this layer.
    ///
    /// Moves [`Layer::generation`] forward, since the caller may write
    /// through the reference.
    pub fn property_mut(&mut self, property_path: PropertyPath) -> Option<&mut PropertySpec> {
        self.touch_values();
        self.prims
            .get_mut(&property_path.prim_path())?
            .property_mut(property_path.property())
    }
}

/// Inserts `spec` into a list of branch specs, replacing the one from the
/// same branch context.
fn insert_branch_spec(specs: &mut Vec<PrimSpec>, spec: PrimSpec) {
    match specs
        .iter_mut()
        .find(|existing| existing.outer_variant_sites == spec.outer_variant_sites)
    {
        Some(existing) => *existing = spec,
        None => specs.push(spec),
    }
}

/// The host's explicit knowledge of an asset anchored to a layer.
///
/// Value-clip templates omit confirmed missing candidates, but must not treat
/// an asset the host has not examined as a missing file. No variant performs I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetAvailability {
    /// The host resolved the asset to this layer identifier.
    /// The layer may still need to be inserted into the store.
    Loaded(LayerId),
    /// The host resolved the asset and confirmed that it does not exist.
    Missing,
    /// The host has not supplied a resolution result.
    Unresolved,
}

/// A store for accessing layers and shared interners.
pub trait LayerStore {
    /// Returns a layer, if present.
    fn layer(&self, id: LayerId) -> Option<&Layer>;

    /// Returns a layer mutably, if present, for authoring.
    fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer>;

    /// Returns the shared token interner.
    fn tokens(&self) -> &TokenInterner;

    /// Returns the shared token interner mutably, allowing interning of additional tokens.
    fn tokens_mut(&mut self) -> &mut TokenInterner;

    /// Returns the shared path interner.
    fn paths(&self) -> &PathInterner;

    /// Returns the shared path interner mutably, allowing interning of derived paths.
    fn paths_mut(&mut self) -> &mut PathInterner;

    /// Returns the loaded layer that `asset_path`, anchored to the layer
    /// `anchor`, resolves to; `None` when it is not known.
    ///
    /// Importers resolve the asset paths they read before composition, but
    /// the asset path of a variable expression
    /// ([`crate::variable_expression`]) is only known once composition
    /// evaluates it, with the expression variables of the layer stack that
    /// authors it. Composition asks the store for the evaluated path here,
    /// and reports one that does not resolve as
    /// [`CompositionError::UnresolvedAsset`] or
    /// [`CompositionError::UnresolvedSublayer`]. A host loads the paths
    /// [`crate::asset::expression_asset_paths`] lists before composing.
    ///
    /// The default knows no asset path.
    ///
    /// Spec: AOUSD Core §9.4 (relative asset paths are anchored to the
    /// layer that authors them). OpenUSD resolves the evaluated path with
    /// `SdfComputeAssetPathRelativeToLayer` (`pxr/usd/pcp/composeSite.cpp`).
    ///
    /// [`CompositionError::UnresolvedAsset`]: crate::CompositionError::UnresolvedAsset
    /// [`CompositionError::UnresolvedSublayer`]: crate::CompositionError::UnresolvedSublayer
    fn asset_layer(&self, anchor: LayerId, asset_path: &str) -> Option<LayerId> {
        let _ = (anchor, asset_path);
        None
    }
    /// Returns the host's asset availability without resolving or loading it.
    ///
    /// The default derives known layers from [`Self::asset_layer`]. Hosts
    /// supporting clip templates should also report confirmed missing candidates.
    fn asset_availability(&self, anchor: LayerId, asset_path: &str) -> AssetAvailability {
        self.asset_layer(anchor, asset_path)
            .map_or(AssetAvailability::Unresolved, AssetAvailability::Loaded)
    }
}

/// A simple in-memory [`LayerStore`] implementation.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    /// Shared token interner for all layers in the store.
    pub tokens: TokenInterner,
    /// Shared path interner for all layers in the store.
    pub paths: PathInterner,
    /// Layers keyed by [`LayerId`].
    pub layers: HashMap<LayerId, Layer>,
    /// The layers asset paths resolve to, by the layer each path is
    /// anchored to, then by path, for [`LayerStore::asset_layer`].
    /// [`LayerId::UNRESOLVED`] records a confirmed missing asset. Use
    /// [`Self::insert_asset_layer`] and [`Self::mark_asset_missing`] to make
    /// binding changes visible to live-stage generation checks.
    pub asset_layers: HashMap<LayerId, HashMap<Arc<str>, LayerId>>,
}

impl InMemoryStore {
    /// Inserts (or replaces) a layer.
    /// Replacing a layer advances its generations so live stages detect reloads,
    /// even when the new layer happened to have the same generation counters.
    pub fn insert_layer(&mut self, mut layer: Layer) {
        debug_assert_ne!(layer.id, LayerId::UNRESOLVED, "reserved layer ID");
        if let Some(previous) = self.layers.get(&layer.id) {
            layer.generation = layer
                .generation
                .max(previous.generation)
                .checked_add(1)
                .expect("layer generation exhausted");
            layer.structure = layer
                .structure
                .max(previous.structure)
                .checked_add(1)
                .expect("layer structural generation exhausted");
        }
        self.layers.insert(layer.id, layer);
    }

    /// Records that `asset_path`, anchored to the layer `anchor`, resolves
    /// to the layer `layer` (see [`LayerStore::asset_layer`]).
    pub fn insert_asset_layer(&mut self, anchor: LayerId, asset_path: &str, layer: LayerId) {
        let previous = self
            .asset_layers
            .entry(anchor)
            .or_default()
            .insert(Arc::from(asset_path), layer);
        if previous != Some(layer)
            && let Some(source) = self.layers.get_mut(&anchor)
        {
            source.touch_structure();
        }
    }

    /// Records a host-confirmed missing asset, invalidating its anchor layer.
    ///
    /// This lets value-clip templates distinguish a filename hole from a
    /// candidate that has not yet been resolved. A later [`Self::insert_asset_layer`]
    /// replaces this result and invalidates the same anchor.
    pub fn mark_asset_missing(&mut self, anchor: LayerId, asset_path: &str) {
        self.insert_asset_layer(anchor, asset_path, LayerId::UNRESOLVED);
    }

    /// Parses and interns an absolute path, returning its [`PathId`].
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a valid absolute path (must start with `/`).
    pub fn path(&mut self, s: &str) -> PathId {
        let p = Path::parse_absolute(s, &mut self.tokens).expect("valid absolute path");
        self.paths.intern(p)
    }

    /// Parses a concrete property path, interning its prim path and property token.
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a valid property path such as `/Prim.attrName`.
    pub fn property_path(&mut self, s: &str) -> PropertyPath {
        PropertyPath::parse(s, &mut self.tokens, &mut self.paths).expect("valid property path")
    }

    /// Parses a concrete relationship or connection target path.
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a valid target path such as `/Prim` or
    /// `/Prim.attrName`.
    pub fn target_path(&mut self, s: &str) -> TargetPath {
        TargetPath::parse(s, &mut self.tokens, &mut self.paths).expect("valid target path")
    }
}

impl LayerStore for InMemoryStore {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.get(&id)
    }

    fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        self.layers.get_mut(&id)
    }

    fn tokens(&self) -> &TokenInterner {
        &self.tokens
    }

    fn tokens_mut(&mut self) -> &mut TokenInterner {
        &mut self.tokens
    }

    fn paths(&self) -> &PathInterner {
        &self.paths
    }

    fn paths_mut(&mut self) -> &mut PathInterner {
        &mut self.paths
    }

    fn asset_layer(&self, anchor: LayerId, asset_path: &str) -> Option<LayerId> {
        self.asset_layers
            .get(&anchor)?
            .get(asset_path)
            .copied()
            .filter(|id| *id != LayerId::UNRESOLVED)
    }
    fn asset_availability(&self, anchor: LayerId, asset_path: &str) -> AssetAvailability {
        match self
            .asset_layers
            .get(&anchor)
            .and_then(|assets| assets.get(asset_path))
        {
            Some(&LayerId::UNRESOLVED) => AssetAvailability::Missing,
            Some(&id) => AssetAvailability::Loaded(id),
            None => AssetAvailability::Unresolved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::sync::Arc;
    use alloc::vec;

    fn entry(key: &str, val: Value) -> (Arc<str>, Value) {
        (Arc::from(key), val)
    }

    #[test]
    fn layer_methods_move_the_generation_forward() {
        let mut store = InMemoryStore::default();
        let size = store.tokens.intern("size");
        let rock = store.path("/Rock");
        let mut layer = Layer::new(LayerId(1));
        let pristine = layer.clone();
        let mut seen = layer.generation();

        let mut seen_structure = layer.structural_generation();
        let mut step = |layer: &Layer, what: &str, structural: bool| {
            assert!(layer.generation() > seen, "{what} moves the generation");
            assert_eq!(
                layer.structural_generation() > seen_structure,
                structural,
                "{what} is structural: {structural}"
            );
            seen = layer.generation();
            seen_structure = layer.structural_generation();
        };
        layer.insert_prim(rock, PrimSpec::def());
        step(&layer, "insert_prim", true);
        layer.set_property(PropertyPath::new(rock, size), PropertySpec::attribute());
        step(&layer, "set_property on an existing prim spec", false);
        let pebble = store.path("/Rock/Pebble");
        layer.set_property(PropertyPath::new(pebble, size), PropertySpec::attribute());
        step(&layer, "set_property creating the prim spec", true);
        let _ = layer.property_mut(PropertyPath::new(rock, size));
        step(&layer, "property_mut", false);
        layer.set_metadata(size, Value::Double(1.0));
        step(&layer, "set_metadata", true);
        layer.touch();
        step(&layer, "touch", true);

        let mut same = pristine.clone();
        same.touch();
        assert_eq!(same, pristine, "equality compares content, not generations");
        assert_ne!(layer, pristine);
    }

    #[test]
    fn interpolation_defaults_to_linear() {
        assert_eq!(
            InterpolationType::default(),
            InterpolationType::Linear,
            "an OpenUSD stage interpolates linearly unless told otherwise"
        );
    }

    #[test]
    fn combine_stronger_scalar_wins() {
        let stronger = vec![entry("a", Value::Int(1))];
        let weaker = vec![entry("a", Value::Int(2))];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(result, vec![entry("a", Value::Int(1))]);
    }

    #[test]
    fn combine_weaker_only_keys_preserved() {
        let stronger = vec![entry("a", Value::Int(1))];
        let weaker = vec![entry("b", Value::Int(2))];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(
            result,
            vec![entry("a", Value::Int(1)), entry("b", Value::Int(2))]
        );
    }

    #[test]
    fn combine_nested_dictionaries_recurse() {
        let stronger = vec![entry(
            "sub",
            Value::Dictionary(vec![entry("x", Value::Int(10))]),
        )];
        let weaker = vec![entry(
            "sub",
            Value::Dictionary(vec![entry("x", Value::Int(20)), entry("y", Value::Int(30))]),
        )];
        let result = combine_dictionaries(&stronger, &weaker);
        // x=10 from stronger wins; y=30 from weaker preserved.
        assert_eq!(
            result,
            vec![entry(
                "sub",
                Value::Dictionary(vec![entry("x", Value::Int(10)), entry("y", Value::Int(30))])
            )]
        );
    }

    #[test]
    fn combine_stronger_dict_weaker_scalar_wins_stronger() {
        // When stronger has a dictionary and weaker has a scalar at the same key,
        // stronger wins (it's not a dict-dict merge).
        let stronger = vec![entry(
            "a",
            Value::Dictionary(vec![entry("x", Value::Int(1))]),
        )];
        let weaker = vec![entry("a", Value::Int(99))];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(
            result,
            vec![entry(
                "a",
                Value::Dictionary(vec![entry("x", Value::Int(1))])
            )]
        );
    }

    #[test]
    fn combine_stronger_scalar_weaker_dict_wins_stronger() {
        // When stronger is scalar and weaker is dictionary, stronger wins.
        let stronger = vec![entry("a", Value::Int(1))];
        let weaker = vec![entry(
            "a",
            Value::Dictionary(vec![entry("x", Value::Int(99))]),
        )];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(result, vec![entry("a", Value::Int(1))]);
    }

    #[test]
    fn combine_empty_stronger() {
        let stronger: Vec<(Arc<str>, Value)> = vec![];
        let weaker = vec![entry("a", Value::Int(1))];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(result, vec![entry("a", Value::Int(1))]);
    }

    #[test]
    fn combine_empty_weaker() {
        let stronger = vec![entry("a", Value::Int(1))];
        let weaker: Vec<(Arc<str>, Value)> = vec![];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(result, vec![entry("a", Value::Int(1))]);
    }

    #[test]
    fn combine_chain_three_opinions() {
        let strongest = vec![entry("a", Value::Int(1))];
        let middle = vec![entry("b", Value::Int(2))];
        let weakest = vec![entry("c", Value::Int(3)), entry("a", Value::Int(99))];
        let result = combine_dictionary_chain([strongest, middle, weakest]);
        assert_eq!(
            result,
            vec![
                entry("a", Value::Int(1)),
                entry("b", Value::Int(2)),
                entry("c", Value::Int(3)),
            ]
        );
    }

    #[test]
    fn combine_chain_empty_yields_default() {
        let result = combine_dictionary_chain(Vec::<Vec<(Arc<str>, Value)>>::new());
        assert!(result.is_empty());
    }

    #[test]
    fn combine_chain_nested_across_three_layers() {
        // Three layers all contribute to a nested dictionary.
        let strongest = vec![entry(
            "d",
            Value::Dictionary(vec![entry("x", Value::Int(1))]),
        )];
        let middle = vec![entry(
            "d",
            Value::Dictionary(vec![entry("y", Value::Int(2))]),
        )];
        let weakest = vec![entry(
            "d",
            Value::Dictionary(vec![entry("x", Value::Int(99)), entry("z", Value::Int(3))]),
        )];
        let result = combine_dictionary_chain([strongest, middle, weakest]);
        assert_eq!(
            result,
            vec![entry(
                "d",
                Value::Dictionary(vec![
                    entry("x", Value::Int(1)),
                    entry("y", Value::Int(2)),
                    entry("z", Value::Int(3)),
                ])
            )]
        );
    }

    #[test]
    fn layer_offset_identity() {
        let id = LayerOffset::IDENTITY;
        assert!(id.is_identity());
        assert_eq!(id.map_time(42.0), 42.0);
    }

    #[test]
    fn layer_offset_map_time() {
        let lo = LayerOffset {
            offset: 10.0,
            scale: 2.0,
        };
        // Local frame 5 plays at stage frame 5 * 2 + 10 = 20.
        assert_eq!(lo.map_time(20.0), 5.0);
        assert_eq!(lo.map_time(10.0), 0.0);
    }

    #[test]
    fn layer_offset_map_time_inverts_composed_offsets_innermost_last() {
        // A layer reached through `outer` then `inner` maps a stage time
        // through `outer` first, then through `inner`.
        let outer = LayerOffset {
            offset: 10.0,
            scale: 2.0,
        };
        let inner = LayerOffset {
            offset: 5.0,
            scale: 3.0,
        };
        let stage_time = 47.0;
        assert_eq!(
            outer.compose(inner).map_time(stage_time),
            inner.map_time(outer.map_time(stage_time))
        );
    }

    #[test]
    fn layer_offset_compose() {
        // outer (offset=10, scale=2) composed with inner (offset=5, scale=3)
        // composed_offset = 10 + 2*5 = 20
        // composed_scale  = 2 * 3 = 6
        let outer = LayerOffset {
            offset: 10.0,
            scale: 2.0,
        };
        let inner = LayerOffset {
            offset: 5.0,
            scale: 3.0,
        };
        let composed = outer.compose(inner);
        assert_eq!(
            composed,
            LayerOffset {
                offset: 20.0,
                scale: 6.0
            }
        );
    }

    #[test]
    fn layer_offset_compose_identity_is_noop() {
        let lo = LayerOffset {
            offset: 10.0,
            scale: 2.0,
        };
        assert_eq!(lo.compose(LayerOffset::IDENTITY), lo);
        assert_eq!(LayerOffset::IDENTITY.compose(lo), lo);
    }

    // ── Dimensioned types (§6.3) ────────────────────────────────────

    #[test]
    fn vec3f_construction_and_display() {
        let v = Value::Vec3f([1.0, 2.0, 3.0]);
        assert_eq!(format!("{v}"), "(1, 2, 3)");
    }

    #[test]
    fn vec2d_construction_and_display() {
        let v = Value::Vec2d([1.5, -2.5]);
        assert_eq!(format!("{v}"), "(1.5, -2.5)");
    }

    #[test]
    fn vec4i_construction_and_display() {
        let v = Value::Vec4i([1, 2, 3, 4]);
        assert_eq!(format!("{v}"), "(1, 2, 3, 4)");
    }

    #[test]
    fn matrix2d_display() {
        let m = Value::Matrix2d(Box::new([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(format!("{m}"), "((1, 0), (0, 1))");
    }

    #[test]
    fn matrix4d_identity_display() {
        let mut elems = [0.0_f64; 16];
        elems[0] = 1.0;
        elems[5] = 1.0;
        elems[10] = 1.0;
        elems[15] = 1.0;
        let m = Value::Matrix4d(Box::new(elems));
        let s = format!("{m}");
        assert!(s.starts_with("((1, 0, 0, 0)"));
        assert!(s.ends_with("(0, 0, 0, 1))"));
    }

    #[test]
    fn quatf_display_is_rijkr_order() {
        // Storage: [i, j, k, r] = [0.1, 0.2, 0.3, 0.9]
        // Display: (r, i, j, k) = (0.9, 0.1, 0.2, 0.3)
        let q = Value::Quatf([0.1, 0.2, 0.3, 0.9]);
        let s = format!("{q}");
        assert!(s.starts_with("(0.9,"));
    }

    #[test]
    fn vec3f_equality() {
        assert_eq!(Value::Vec3f([1.0, 2.0, 3.0]), Value::Vec3f([1.0, 2.0, 3.0]));
        assert_ne!(Value::Vec3f([1.0, 2.0, 3.0]), Value::Vec3f([1.0, 2.0, 4.0]));
    }

    #[test]
    fn vec3f_clone() {
        let v = Value::Vec3f([1.0, 2.0, 3.0]);
        let v2 = v.clone();
        assert_eq!(v, v2);
    }

    #[test]
    fn matrix4d_clone() {
        let m = Value::Matrix4d(Box::new([1.0; 16]));
        let m2 = m.clone();
        assert_eq!(m, m2);
    }

    #[test]
    fn array_of_vec3f() {
        let arr = Value::Array(vec![
            Value::Vec3f([1.0, 2.0, 3.0]),
            Value::Vec3f([4.0, 5.0, 6.0]),
        ]);
        assert_eq!(format!("{arr}"), "[(1, 2, 3), (4, 5, 6)]");
    }

    /// `defaultPrim` tokens and the prim paths they name, as recorded from
    /// OpenUSD 26.08's `SdfLayer::GetDefaultPrimAsPath` (an empty path there
    /// is `None` here).
    #[test]
    fn default_prim_path_matches_openusd() {
        let cases: &[(&str, Option<&str>)] = &[
            ("Model", Some("/Model")),
            ("Model/Geo", Some("/Model/Geo")),
            ("/Model/Geo", Some("/Model/Geo")),
            ("_x", Some("/_x")),
            ("Mödel", Some("/Mödel")),
            ("1Bad", None),
            ("Model.attr", None),
            ("/", None),
            ("", None),
            ("./Model", None),
            ("../X", None),
            ("Model{v=a}", None),
            ("Bad Name", None),
            ("Model/", None),
            ("a:b", None),
        ];
        let mut tokens = TokenInterner::default();
        let mut layer = Layer::new(LayerId(1));
        assert_eq!(layer.default_prim_path(&mut tokens), None, "not authored");
        for (token, expected) in cases {
            layer.default_prim = Some(tokens.intern(token));
            let actual = layer
                .default_prim_path(&mut tokens)
                .map(|path| path.display(&tokens));
            assert_eq!(actual.as_deref(), *expected, "defaultPrim = {token:?}");
        }
    }

    /// Variant specs nest per branch: `/P{a=x}{a=x}` is a spec of its own
    /// below `/P{a=x}`, and a set nested under two branches is two sets.
    /// Both are addressed by their variant-qualified paths.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs).
    #[test]
    fn variant_specs_nest_per_branch() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let [a, b, x, y, count] = ["a", "b", "x", "y", "count"].map(|t| tokens.intern(t));
        let p = paths.intern(Path::parse_absolute("/P", &mut tokens).unwrap());
        let variant = |value: i64, sets: Vec<(TokenId, TokenId, VariantSpec)>| {
            let mut spec = VariantSpec::default();
            spec.fields.push(FieldEntry {
                name: count,
                value: FieldValue::Value(Value::Int64(value)),
            });
            for (set, name, nested) in sets {
                spec.variant_set_order.push(set);
                spec.variant_sets
                    .entry(set)
                    .or_default()
                    .variants
                    .insert(name, nested);
            }
            spec
        };
        // `/P{a=x}` nests `a` again and `b`; `/P{a=y}` nests another `b`.
        let ax = variant(
            1,
            vec![(a, x, variant(2, vec![])), (b, y, variant(3, vec![]))],
        );
        let ay = variant(4, vec![(b, y, variant(5, vec![]))]);
        let mut prim = PrimSpec::def();
        prim.variant_set_order.push(a);
        let set = prim.variant_sets.entry(a).or_default();
        set.variants.insert(x, ax);
        set.variants.insert(y, ay);
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(p, prim);

        let count_at = |path: &str, tokens: &mut TokenInterner, paths: &mut PathInterner| {
            let path = SpecPath::parse(path, tokens, paths).unwrap();
            match layer.variant_spec_at(&path, paths)?.fields[0].value {
                FieldValue::Value(Value::Int64(value)) => Some(value),
                _ => None,
            }
        };
        assert_eq!(count_at("/P{a=x}", &mut tokens, &mut paths), Some(1));
        assert_eq!(count_at("/P{a=x}{a=x}", &mut tokens, &mut paths), Some(2));
        assert_eq!(count_at("/P{a=x}{b=y}", &mut tokens, &mut paths), Some(3));
        assert_eq!(count_at("/P{a=y}{b=y}", &mut tokens, &mut paths), Some(5));
        assert_eq!(count_at("/P{b=y}", &mut tokens, &mut paths), None);
        assert_eq!(count_at("/P", &mut tokens, &mut paths), None);

        let sets_at = |path: &str, tokens: &mut TokenInterner, paths: &mut PathInterner| {
            let path = SpecPath::parse(path, tokens, paths).unwrap();
            layer
                .variant_sets_at(&path, paths)
                .map(|(_, order)| order.to_vec())
        };
        assert_eq!(sets_at("/P", &mut tokens, &mut paths), Some(vec![a]));
        assert_eq!(
            sets_at("/P{a=x}", &mut tokens, &mut paths),
            Some(vec![a, b])
        );
        assert_eq!(sets_at("/P{a=y}", &mut tokens, &mut paths), Some(vec![b]));
        assert_eq!(sets_at("/Q", &mut tokens, &mut paths), None);

        let prim = &layer.prims[&p];
        let mut chains: Vec<Vec<(TokenId, TokenId)>> = prim
            .variant_branches()
            .map(|branch| branch.chain().collect())
            .collect();
        chains.sort();
        let mut expected = vec![
            vec![(a, x)],
            vec![(a, x), (a, x)],
            vec![(a, x), (b, y)],
            vec![(a, y)],
            vec![(a, y), (b, y)],
        ];
        expected.sort();
        assert_eq!(chains, expected, "every variant spec, nested ones included");

        // Only fully selected branches are visited.
        let selections: HashMap<TokenId, TokenId> = [(a, y), (b, y)].into_iter().collect();
        let mut selected: Vec<Vec<(TokenId, TokenId)>> = prim
            .selected_variant_branches(&selections)
            .map(|branch| branch.chain().collect())
            .collect();
        selected.sort();
        assert_eq!(selected, [vec![(a, y)], vec![(a, y), (b, y)]]);
    }
}
