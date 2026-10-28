// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Document model: layers, prim specs, and composition arcs.
//!
//! Spec: AOUSD Core §6–§7 (scene description data model and opinions), plus §10
//! for arc-related fields (variants/references).

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{
    fmt,
    hash::{Hash, Hasher},
};

use hashbrown::HashMap;

use crate::{
    array_edit::ArrayEdit,
    interner::TokenId,
    interner::TokenInterner,
    listop::{ListOp, SparseListOp},
    path::{
        Path, PathError, PathId, PathInterner, PropertyPath, PropertyPathError, TargetPath,
        TargetPathError,
    },
    prim_index::OpinionValue,
    property::{
        PropertyEntry, PropertySpec, PropertyType, get_property, get_property_mut, remove_property,
        set_property_vec,
    },
    spec_path::{SpecPath, SpecPathError, VariantSelectionSite},
};

/// Identifies a layer by stable ID.
///
/// IDs are split in two ranges. Below [`LayerId::FIRST_DERIVED`] are the
/// IDs hosts and stores hand out themselves, such as those of anonymous and
/// session layers. From it up are the IDs a loader derives from the
/// identifiers of the layers it reads: `layerstack_io` gives a layer it
/// names by identifier a fixed hash of that identifier, so the same
/// identifier gets the same ID in every open, on any number of threads, on
/// every platform and in every process. A host allocating its own IDs
/// should keep them below `FIRST_DERIVED`; a loader steps past any ID
/// already in use rather than reuse it, but a layer it reads may then get
/// another ID than it gets in an open without the host's layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayerId(pub u64);

impl LayerId {
    /// The first ID of the range loaders derive from layer identifiers
    /// (2^63); every ID below it is free for hosts and stores to hand out.
    pub const FIRST_DERIVED: Self = Self(1 << 63);

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
#[non_exhaustive]
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
    /// A value OpenUSD keeps as the text its USDA parser recorded, an
    /// `SdfUnregisteredValue` holding a string: a metadata value no schema
    /// registers and inference gives no type (a bare identifier, `foo`),
    /// or the value of an attribute whose type name no type registers. It
    /// is written back as that text.
    ///
    /// OpenUSD: `SdfUnregisteredValue`, `Sdf_ParserValueContext`'s recorded
    /// string (`pxr/usd/sdf/parserValueContext.cpp`).
    Unregistered(Arc<str>),
    /// An `SdfPath` value, as authored: the legacy default of a
    /// relationship (`rel r.default = </A>`), which OpenUSD's parser keeps
    /// without anchoring a relative path.
    ///
    /// OpenUSD: `SdfFieldKeys->Default` holding an `SdfPath`
    /// (`TextParserAction<PathRef>`, `pxr/usd/sdf/textFileFormatParser.cpp`).
    Path(Arc<str>),
    /// A time code value (`timecode`), semantically a time in frames.
    ///
    /// Spec: §6.2.
    TimeCode(f64),
    /// A length of time in time codes (`duration`, `GfDuration`).
    ///
    /// Held as a plain number. Value resolution reads it through a layer
    /// offset's scale but not its offset, as OpenUSD 26.11 does
    /// (`SdfLayerOffset::operator*(GfDuration)`,
    /// `Usd_ApplyLayerOffsetToValue` in `pxr/usd/usd/valueUtils.h`), and a
    /// value clip scales it by its time mapping's slope. Values carry no
    /// arithmetic, so nothing relates it to [`Value::TimeCode`] the way
    /// `GfTimeCode - GfTimeCode` gives a `GfDuration` (`pxr/base/gf/timeCode.h`).
    ///
    /// Spec: AOUSD Core §12.3.2.1 (layer offsets). §6.2 lists no duration
    /// type; this is an OpenUSD
    /// extension (`pxr/base/gf/duration.h`, `SdfValueTypeNames->Duration`).
    Duration(f64),

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
    /// Stores fixed-size tuples (e.g. `(1.0, 2.0, 3.0)` from a `float3`
    /// attribute) and arrays no native buffer holds: heterogeneous ones and
    /// arrays of asset paths or path expressions. Readers hold numeric,
    /// token and string arrays as [`Self::TypedArray`]; [`Self::array`]
    /// packs elements into one. Type semantics are carried externally by
    /// the attribute's type name, not by the value itself.
    ///
    /// Spec: AOUSD Core §6.2 (scene description data types).
    Array(Vec<Self>),
    /// Shared homogeneous numeric, token or string array, with native
    /// element storage.
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
    /// An authored value an adapter decodes on its first read (see
    /// [`crate::DeferredValue`]).
    ///
    /// Read it with [`Self::try_materialize`] or [`Self::materialized`]:
    /// the decoded value is what an eager read produces. Value resolution
    /// reads through it, so composed queries never return it unless it
    /// failed to decode, and the checked queries
    /// ([`crate::Attribute::try_get`]) report that failure.
    Deferred(crate::DeferredValue),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        if matches!(self, Self::Deferred(_)) || matches!(other, Self::Deferred(_)) {
            // A value that fails to decode equals nothing, as a failed
            // retained array does.
            return match (self.try_materialize(), other.try_materialize()) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
        }
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
            (Self::Unregistered(a), Self::Unregistered(b)) => a == b,
            (Self::Path(a), Self::Path(b)) => a == b,
            (Self::TimeCode(a), Self::TimeCode(b)) => a == b,
            (Self::Duration(a), Self::Duration(b)) => a == b,
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
            Self::Unregistered(v) => write!(f, "unregistered({v:?})"),
            Self::Path(v) => write!(f, "<{v}>"),
            Self::TimeCode(v) | Self::Duration(v) => write!(f, "{v}"),
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
            Self::Deferred(deferred) => match deferred.try_materialize() {
                Ok(value) => write!(f, "{value}"),
                Err(error) => write!(f, "undecodable({error})"),
            },
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

    /// Packs a homogeneous numeric, token or string array into native
    /// storage. Heterogeneous arrays, and asset path and path expression
    /// arrays, keep their general value representation.
    ///
    /// Spec: AOUSD Core §6.2–§6.3 (array values).
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
                crate::TypedArray::pack_iter(&kind, &mut core::iter::once(first).chain(values))
            }
            None => element.map_or_else(
                || Self::Array(Vec::new()),
                |kind| crate::TypedArray::pack_iter(kind, &mut core::iter::empty()),
            ),
        }
    }

    /// Borrows an array without expanding native buffers into scalar values.
    ///
    /// Retain the resolved value, then borrow its native slice. Numeric array
    /// resolution shares the source buffer; this loop copies no points:
    ///
    /// ```
    /// use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType,
    ///     Stage, StageOptions, Time, Value};
    /// let mut store = InMemoryStore::default();
    /// let prim = store.path("/Mesh");
    /// let points = store.tokens().intern("points");
    /// let mut layer = Layer::new(LayerId(1));
    /// layer.insert_prim(prim, PrimSpec::def().with_property(points,
    ///     PropertySpec::typed_attribute(PropertyType::parse("point3f[]").unwrap())
    ///         .with_default(Value::from(vec![[1.0_f32, 2.0, 3.0]]))));
    /// store.insert_layer(layer);
    /// let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let points = stage.view(&store).attribute("/Mesh.points").unwrap();
    /// let value = points.get(Time::Default).unwrap().value;
    /// let array = value.array_ref().unwrap();
    /// let points = array.typed().unwrap().as_vec3f().unwrap();
    /// assert_eq!(points[0], [1.0, 2.0, 3.0]);
    /// ```
    #[must_use]
    pub fn array_ref(&self) -> Option<crate::ArrayRef<'_>> {
        match self {
            Self::Array(values) => Some(crate::ArrayRef::Values(values)),
            Self::TypedArray(values) => values.try_materialize().ok().map(crate::ArrayRef::Typed),
            Self::Deferred(deferred) => deferred.try_materialize().ok()?.array_ref(),
            _ => None,
        }
    }

    /// Borrows this value, decoding a [`Self::Deferred`] one first.
    ///
    /// Every other value is returned as it is, retained numeric arrays
    /// included (see [`crate::TypedArray::try_materialize`]). A deferred
    /// value decodes once; later reads borrow the cached value.
    ///
    /// # Errors
    ///
    /// The cached decode failure of a deferred value.
    pub fn try_materialize(&self) -> Result<&Self, &crate::ArrayReadError> {
        match self {
            Self::Deferred(deferred) => deferred.try_materialize(),
            value => Ok(value),
        }
    }

    /// Borrows this value, decoding a [`Self::Deferred`] one first, as
    /// [`Self::try_materialize`] does.
    ///
    /// A deferred value that fails to decode is returned as it is: it is of
    /// no value type, so a typed read of it finds nothing, as a typed read
    /// of a failed retained array does. Use [`Self::try_materialize`] to
    /// tell that failure from a value of another type.
    #[must_use]
    pub fn materialized(&self) -> &Self {
        self.try_materialize().unwrap_or(self)
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
/// The list-op variants are boxed: most fields hold a [`Value`], and a
/// field no larger than one keeps every [`FieldEntry`] and composed
/// opinion small.
///
/// Spec: AOUSD Core §7.4 (metadata fields), §12.2 (metadata resolution).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum FieldValue {
    /// A plain value (strongest wins; dictionaries combine).
    Value(Value),
    /// A list-op field over tokens (resolved by chaining), such as
    /// `apiSchemas`.
    TokenListOp(Box<ListOp<TokenId>>),
    /// A list-op field over target paths (resolved by chaining).
    ///
    /// Spec: AOUSD Core §12.4 (`ListOps`), applied to path lists.
    PathListOp(Box<ListOp<TargetPath>>),
    /// A list-op field over strings (`stringlistop`), such as `clipSets`.
    StringListOp(Box<ListOp<Arc<str>>>),
    /// A list-op field over `int` values (`intlistop`).
    IntListOp(Box<ListOp<i32>>),
    /// A list-op field over `uint` values (`uintlistop`).
    UIntListOp(Box<ListOp<u32>>),
    /// A list-op field over `int64` values (`int64listop`), such as
    /// `PointInstancer`'s `inactiveIds`.
    Int64ListOp(Box<ListOp<i64>>),
    /// A list-op field over `uint64` values (`uint64listop`).
    UInt64ListOp(Box<ListOp<u64>>),
    /// A list-op field no schema registers, its items the text OpenUSD's
    /// parser records for each statement's list, brackets dropped
    /// (`prepend key = [1, 2.5]` holds `1, 2.5`).
    ///
    /// OpenUSD: `SdfUnregisteredValueListOp` (`_KeyValueMetadataEnd`,
    /// `pxr/usd/sdf/textParserHelpers.cpp`).
    UnregisteredListOp(Box<ListOp<Arc<str>>>),
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

impl From<ListOp<TokenId>> for FieldValue {
    fn from(op: ListOp<TokenId>) -> Self {
        Self::TokenListOp(Box::new(op))
    }
}

impl From<ListOp<TargetPath>> for FieldValue {
    fn from(op: ListOp<TargetPath>) -> Self {
        Self::PathListOp(Box::new(op))
    }
}

impl From<ListOp<Arc<str>>> for FieldValue {
    fn from(op: ListOp<Arc<str>>) -> Self {
        Self::StringListOp(Box::new(op))
    }
}

impl From<ListOp<i32>> for FieldValue {
    fn from(op: ListOp<i32>) -> Self {
        Self::IntListOp(Box::new(op))
    }
}

impl From<ListOp<u32>> for FieldValue {
    fn from(op: ListOp<u32>) -> Self {
        Self::UIntListOp(Box::new(op))
    }
}

impl From<ListOp<i64>> for FieldValue {
    fn from(op: ListOp<i64>) -> Self {
        Self::Int64ListOp(Box::new(op))
    }
}

impl From<ListOp<u64>> for FieldValue {
    fn from(op: ListOp<u64>) -> Self {
        Self::UInt64ListOp(Box::new(op))
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

/// Hashes the bits [`PartialEq`] compares.
impl Hash for LayerOffset {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.offset.to_bits().hash(state);
        self.scale.to_bits().hash(state);
    }
}

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
    /// The sublayer's layer ID, or [`LayerId::UNRESOLVED`] when no layer
    /// is resolved for its asset path (see [`SublayerEntry::unresolved`]).
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

    /// Creates a sublayer entry for `asset` with no layer resolved: the way
    /// to author a sublayer by asset path, and how an importer keeps one
    /// whose resolution failed.
    ///
    /// The entry keeps its place and authored asset path, and its layer is
    /// [`LayerId::UNRESOLVED`]. Gathering the layer stack resolves it
    /// through the store ([`LayerStore::asset_availability`]), relative to
    /// the layer naming it: a layer the store binds the path to takes its
    /// place; a path the store has no answer for is asked for
    /// ([`Stage::asset_requests`](crate::Stage::asset_requests), which
    /// `layerstack_io::StageDocument` loads at open and after
    /// `StageDocument::apply`); one the store confirmed missing
    /// ([`InMemoryStore::mark_asset_missing`], as importers record a failed
    /// resolution) is skipped and reported as
    /// [`CompositionError::UnresolvedSublayer`], keeping the rest of the
    /// layer stack.
    ///
    /// Spec: AOUSD Core §9.4 (relative asset paths), §10.3.1 (sublayers),
    /// §10.6 (composition errors). OpenUSD opens each sublayer as it
    /// computes the layer stack and reports one it cannot open as
    /// `PcpErrorInvalidSublayerPath` (`PcpLayerStack::_BuildLayerStack`,
    /// `pxr/usd/pcp/layerStack.cpp`).
    ///
    /// [`CompositionError::UnresolvedSublayer`]: crate::CompositionError::UnresolvedSublayer
    pub fn unresolved(asset: impl Into<String>, offset: LayerOffset) -> Self {
        Self {
            layer: LayerId::UNRESOLVED,
            offset,
            asset: Some(asset.into()),
        }
    }

    /// Returns `true` when no layer is resolved for this sublayer's asset
    /// path (see [`SublayerEntry::unresolved`]): gathering the layer stack
    /// resolves it through the store.
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

    /// Returns the layer this sublayer of `anchor` names as the store
    /// resolves it: its layer, or for one with no layer resolved
    /// ([`SublayerEntry::is_unresolved`]) the layer `store` binds its asset
    /// path to relative to `anchor` ([`LayerStore::asset_layer`]), as
    /// gathering the layer stack takes it. `None` while the store binds
    /// none, and for a variable expression, which only gathering the layer
    /// stack evaluates.
    ///
    /// Spec: AOUSD Core §9.4 (relative asset paths), §10.3.1 (sublayers).
    pub(crate) fn stored_layer(&self, store: &dyn LayerStore, anchor: LayerId) -> Option<LayerId> {
        if !self.is_unresolved() {
            return Some(self.layer);
        }
        if self.is_expression() {
            return None;
        }
        store.asset_layer(anchor, self.asset.as_deref()?)
    }
}

impl From<LayerId> for SublayerEntry {
    fn from(layer: LayerId) -> Self {
        Self::new(layer)
    }
}

/// A composition reference arc.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReferenceTarget {
    /// Target a concrete prim path.
    Prim(PathId),
    /// No authored prim path: target the prim named by the referenced
    /// layer's `defaultPrim` (see [`Layer::default_prim_path`]).
    ///
    /// For an external arc (`@./asset.usda@`) that is the asset's root
    /// layer; for an internal arc (`<>`) it is the root layer of the layer
    /// stack containing the arc (see [`ReferenceSource::Layer`]). When the
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

/// Where a reference or payload arc leads: the layer stack it targets, or
/// the asset path composition resolves to find it.
///
/// [`Reference::source`] reads it. Each variant names the one layer it
/// means: the layer the arc targets ([`Self::Layer`]), or the layer that
/// authors it, which a path is resolved relative to (`anchor`).
///
/// Spec: AOUSD Core §9.4 (asset paths are anchored to the layer that
/// authors them), §10.3.2.1 (references), §10.3.2.2 (payloads).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceSource<'a> {
    /// The layer stack whose root layer is `layer`, which the store holds.
    /// `asset` is the asset path the arc was authored with, which writers
    /// write back; `None` for an arc made by layer id alone.
    ///
    /// An arc with no `asset` whose `layer` is the layer that authors it is
    /// internal (`</Prim>`, `<>`): it targets the layer stack containing
    /// the site that authors it, not that layer alone, so an internal arc
    /// authored in a sublayer reads every layer of the stack. Composition
    /// anchors it to that stack's root layer.
    ///
    /// Spec: AOUSD Core §10.3.2.1 ("the layer stack containing the
    /// reference is assumed"). OpenUSD: `_EvalRefOrPayloadArcs` in
    /// `pxr/usd/pcp/primIndex.cpp`.
    Layer {
        /// The root layer of the targeted layer stack.
        layer: LayerId,
        /// The authored asset path, if the arc has one.
        asset: Option<&'a str>,
    },
    /// An asset path the importer left for composition to resolve relative
    /// to `anchor` ([`Reference::deferred`]): composition asks the store for
    /// it ([`LayerStore::asset_availability`]) and follows the arc to the
    /// layer the host loaded, or lists it in
    /// [`Stage::asset_requests`](crate::Stage::asset_requests) until then.
    Asset {
        /// The layer that authors the arc.
        anchor: LayerId,
        /// The authored asset path.
        path: &'a str,
    },
    /// A variable expression (`` `"./${NAME}.usd"` ``) composition evaluates
    /// with the authoring layer stack's expression variables, then resolves
    /// relative to `anchor` ([`Reference::expression`]).
    Expression {
        /// The layer that authors the arc.
        anchor: LayerId,
        /// The authored expression.
        text: &'a str,
    },
    /// An asset path whose resolution failed ([`Reference::unresolved`]):
    /// the arc contributes nothing, and composition reports it as
    /// [`CompositionError::UnresolvedAsset`](crate::CompositionError::UnresolvedAsset).
    Unresolved {
        /// The authored asset path.
        path: &'a str,
    },
}

/// A composition reference or payload arc.
///
/// Its [`source`](Self::source) says where it leads; the constructors make
/// each kind: [`Reference::new`] and [`Reference::to_default_prim`] (a
/// layer by id, or an internal arc), [`Reference::with_asset`] and
/// [`Reference::with_asset_default_prim`] (a layer and the asset path it
/// was authored with), [`Reference::deferred`], [`Reference::expression`]
/// and [`Reference::unresolved`].
#[derive(Clone, Debug)]
#[doc(alias = "SdfReference")]
pub struct Reference {
    /// The layer the arc's source names: the targeted layer for a resolved
    /// arc, the authoring layer for a deferred or expression arc,
    /// [`LayerId::UNRESOLVED`] for an unresolved one
    /// ([`ReferenceSource`] tells them apart).
    pub(crate) layer: LayerId,
    /// The authored target within that layer/document.
    pub target: ReferenceTarget,
    /// The authored asset path, or expression.
    pub(crate) asset: Option<String>,
    /// Time offset applied across this reference boundary (§12.3.2.1).
    pub layer_offset: LayerOffset,
    /// Authored reference custom data (AOUSD Core §16.2.17.5). Payloads have none.
    pub custom_data: Vec<(Arc<str>, Value)>,
    /// Whether the importer left `asset` for composition to resolve,
    /// relative to `layer` (see [`Reference::deferred`]).
    pub(crate) deferred: bool,
}

// Reference list identity must be reflexive even for metadata NaNs, just as
// LayerOffset compares authored bits. AOUSD Core §10.3.2.1, §16.2.17.5.
impl PartialEq for Reference {
    fn eq(&self, other: &Self) -> bool {
        use crate::edit::same::Same;
        self.layer == other.layer
            && self.deferred == other.deferred
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

/// Hashes what [`PartialEq`] compares, except custom data's entries, which
/// it compares in any order: only their count is hashed. List ops hash
/// long reference lists to apply them in linear time.
impl Hash for Reference {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.layer.hash(state);
        self.deferred.hash(state);
        self.target.hash(state);
        self.asset.hash(state);
        self.layer_offset.hash(state);
        self.custom_data.len().hash(state);
    }
}

impl Reference {
    /// Where the arc leads: the layer it targets, or the asset path or
    /// expression composition resolves relative to the layer that authors
    /// it (see [`ReferenceSource`]).
    ///
    /// ```
    /// use layerstack::{LayerId, LayerOffset, Reference, ReferenceSource, ReferenceTarget};
    ///
    /// let asset = Reference::with_asset_default_prim(LayerId(2), "./rock.usda");
    /// assert_eq!(
    ///     asset.source(),
    ///     ReferenceSource::Layer { layer: LayerId(2), asset: Some("./rock.usda") },
    /// );
    /// let deferred = Reference::deferred(
    ///     LayerId(1), "./plant.usda", ReferenceTarget::DefaultPrim, LayerOffset::IDENTITY,
    /// );
    /// assert_eq!(
    ///     deferred.source(),
    ///     ReferenceSource::Asset { anchor: LayerId(1), path: "./plant.usda" },
    /// );
    /// ```
    #[must_use]
    pub fn source(&self) -> ReferenceSource<'_> {
        let asset = self.asset.as_deref();
        if self.layer == LayerId::UNRESOLVED {
            ReferenceSource::Unresolved {
                path: asset.unwrap_or_default(),
            }
        } else if self.deferred {
            ReferenceSource::Asset {
                anchor: self.layer,
                path: asset.unwrap_or_default(),
            }
        } else if self.is_expression() {
            ReferenceSource::Expression {
                anchor: self.layer,
                text: asset.unwrap_or_default(),
            }
        } else {
            ReferenceSource::Layer {
                layer: self.layer,
                asset,
            }
        }
    }

    /// An arc from `source` to `target`, with no layer offset or custom
    /// data: the constructor that takes any [`ReferenceSource`].
    #[must_use]
    pub fn from_source(source: ReferenceSource<'_>, target: ReferenceTarget) -> Self {
        let (layer, asset, deferred) = match source {
            ReferenceSource::Layer { layer, asset } => (layer, asset, false),
            ReferenceSource::Asset { anchor, path } => (anchor, Some(path), true),
            ReferenceSource::Expression { anchor, text } => (anchor, Some(text), false),
            ReferenceSource::Unresolved { path } => (LayerId::UNRESOLVED, Some(path), false),
        };
        Self {
            layer,
            target,
            asset: asset.map(String::from),
            layer_offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
            deferred,
        }
    }

    /// The arc with its source replaced by `source`, keeping its target,
    /// layer offset and custom data: for a tool that rewrites where arcs
    /// lead, such as one that localizes assets.
    #[must_use]
    pub fn with_source(self, source: ReferenceSource<'_>) -> Self {
        let Self {
            target,
            layer_offset,
            custom_data,
            ..
        } = self;
        Self {
            layer_offset,
            custom_data,
            ..Self::from_source(source, target)
        }
    }

    /// The asset path the arc was authored with, whatever its source: the
    /// path of a resolved, deferred or unresolved arc, or the expression
    /// text. `None` for an arc made by layer id alone, or an internal one.
    #[must_use]
    pub fn asset_path(&self) -> Option<&str> {
        self.asset.as_deref()
    }

    /// Sets the layer offset applied across the arc.
    #[must_use]
    pub fn with_layer_offset(mut self, layer_offset: LayerOffset) -> Self {
        self.layer_offset = layer_offset;
        self
    }

    /// Sets the authored custom data, preserving it through unresolved arcs.
    pub fn with_custom_data(mut self, custom_data: Vec<(Arc<str>, Value)>) -> Self {
        self.custom_data = custom_data;
        self
    }

    /// Creates a reference to the prim at `prim_path` in the layer stack
    /// whose root layer is `layer`, with no asset path
    /// ([`ReferenceSource::Layer`]).
    ///
    /// With `layer` set to the layer that authors it, the arc is internal.
    pub fn new(layer: LayerId, prim_path: PathId) -> Self {
        Self {
            layer,
            target: ReferenceTarget::Prim(prim_path),
            asset: None,
            layer_offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
            deferred: false,
        }
    }

    /// Creates a reference to the prim at `prim_path` in the layer stack
    /// whose root layer is `layer`, the layer `asset` resolved to
    /// ([`ReferenceSource::Layer`]).
    pub fn with_asset(layer: LayerId, prim_path: PathId, asset: impl Into<String>) -> Self {
        Self {
            layer,
            target: ReferenceTarget::Prim(prim_path),
            asset: Some(asset.into()),
            layer_offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
            deferred: false,
        }
    }

    /// Creates a reference to the `defaultPrim` of `layer`, with no asset
    /// path ([`ReferenceSource::Layer`]); internal (`<>`) when `layer` is
    /// the layer that authors it.
    pub fn to_default_prim(layer: LayerId) -> Self {
        Self {
            layer,
            target: ReferenceTarget::DefaultPrim,
            asset: None,
            layer_offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
            deferred: false,
        }
    }

    /// Creates a reference to the `defaultPrim` of `layer`, the layer
    /// `asset` resolved to ([`ReferenceSource::Layer`]).
    pub fn with_asset_default_prim(layer: LayerId, asset: impl Into<String>) -> Self {
        Self {
            layer,
            target: ReferenceTarget::DefaultPrim,
            asset: Some(asset.into()),
            layer_offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
            deferred: false,
        }
    }

    /// Creates a reference or payload to `asset` whose resolution failed
    /// ([`ReferenceSource::Unresolved`]).
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
            deferred: false,
        }
    }

    /// Creates a reference or payload, authored in `anchor`, whose asset path
    /// `asset` is a variable expression such as `` `"./${NAME}.usd"` ``
    /// ([`ReferenceSource::Expression`]).
    ///
    /// The arc targets no layer stack as authored
    /// ([`Reference::is_unresolved`]): composition evaluates `asset` with
    /// the expression variables of the layer stack that authors the arc,
    /// resolves the resulting path relative to `anchor`
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
        anchor: LayerId,
        asset: impl Into<String>,
        target: ReferenceTarget,
        layer_offset: LayerOffset,
    ) -> Self {
        Self {
            layer: anchor,
            target,
            asset: Some(asset.into()),
            layer_offset,
            custom_data: Vec::new(),
            deferred: false,
        }
    }

    /// Creates a reference or payload, authored in `anchor`, whose asset path
    /// `asset` the importer leaves for composition to resolve
    /// ([`ReferenceSource::Asset`]).
    ///
    /// An importer may defer an arc that composition might never follow,
    /// such as one inside a variant branch, so that opening a stage reads
    /// only the layers its selections reach. The arc targets no layer stack
    /// as authored ([`Reference::is_unresolved`]): composition asks the
    /// store for `asset` relative to `anchor`
    /// ([`LayerStore::asset_availability`]) and follows the arc to the layer
    /// the host loaded. An asset the host confirmed missing is reported as
    /// [`CompositionError::UnresolvedAsset`]; one the host has not examined
    /// contributes nothing yet and is listed by
    /// [`Stage::asset_requests`](crate::Stage::asset_requests), for the host
    /// to load and compose again.
    ///
    /// Spec: AOUSD Core §10.3.2.1 (references), §10.3.2.2 (payloads),
    /// §10.3.2.5 (only the selected variant contributes). OpenUSD opens the
    /// layer of each arc it composes, in `_EvalRefOrPayloadArcs`
    /// (`pxr/usd/pcp/primIndex.cpp`).
    ///
    /// [`CompositionError::UnresolvedAsset`]: crate::CompositionError::UnresolvedAsset
    pub fn deferred(
        anchor: LayerId,
        asset: impl Into<String>,
        target: ReferenceTarget,
        layer_offset: LayerOffset,
    ) -> Self {
        Self {
            layer: anchor,
            target,
            asset: Some(asset.into()),
            layer_offset,
            custom_data: Vec::new(),
            deferred: true,
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
    /// asset path could not be resolved (see [`Reference::unresolved`]), it
    /// is a variable expression that composition has yet to evaluate
    /// ([`Reference::is_expression`]), or the importer deferred it
    /// ([`Reference::deferred`]).
    #[must_use]
    pub fn is_unresolved(&self) -> bool {
        self.layer == LayerId::UNRESOLVED || self.deferred || self.is_expression()
    }

    /// Returns the root layer of the layer stack this arc targets as the
    /// store resolves it: its layer, or for a deferred arc
    /// ([`Reference::deferred`]) the layer `store` records for its asset
    /// ([`LayerStore::asset_layer`]), `None` while it records none.
    pub(crate) fn stored_target_layer(&self, store: &dyn LayerStore) -> Option<LayerId> {
        if self.deferred {
            store.asset_layer(self.layer, self.asset.as_deref().unwrap_or_default())
        } else {
            Some(self.layer)
        }
    }

    /// Returns the prim path this arc targets in the layer stack rooted at
    /// its [`ReferenceSource::Layer`]: the authored path, or the path named by that
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
                    .map(|name| store.tokens().intern(name))
                    .collect();
                Some(store.paths().intern(Path::root().join(&segments)))
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
    pub references: SparseListOp<Reference>,
    /// Inherits arcs on this variant branch itself.
    pub inherits: SparseListOp<PathId>,
    /// Specializes arcs on this variant branch itself.
    pub specializes: SparseListOp<PathId>,
    /// Payloads on this variant branch itself.
    pub payloads: SparseListOp<Reference>,
    /// Variant selections authored within this variant branch.
    ///
    /// When a variant branch header includes `variants = { string v2 = "b" }`,
    /// those selections apply to the owning prim when this variant is selected.
    pub variant_selections: VariantSelections,
    /// The variant sets nested in this branch (`/P{a=x}{b=}`), keyed by
    /// set name, in the order they were added.
    ///
    /// They are sets of the prim hosting this branch, available only while
    /// this branch is selected.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs).
    pub variant_sets: VariantSets,
    /// The `variantSetNames` list op of this branch (`variantSets` in
    /// USDA): the nested sets it lists, as
    /// [`PrimSpec::variant_set_names`] is for a prim spec.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §7.6.7.
    pub variant_set_names: SparseListOp<TokenId>,
    /// Property ordering authored inside this branch (`reorder properties`).
    ///
    /// Spec: AOUSD Core §7.6.7 (variant specs contribute prim spec fields).
    pub property_order: Option<Vec<TokenId>>,
}

/// Variant selections: for each variant set name, the name of the variant
/// selected (`variants = { string shape = "round" }` in USDA), as a prim
/// spec ([`PrimSpec::variant_selections`]) or a variant branch
/// ([`VariantSpec::variant_selections`]) authors them.
///
/// A set has at most one selection. Iteration visits each set once, in no
/// particular order; sort by name for a stable one.
///
/// Spec: AOUSD Core §7.6.2.3.4 (`variantSelection`), §10.3.2.5 (only the
/// selected variant contributes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VariantSelections(HashMap<TokenId, TokenId>);

impl VariantSelections {
    /// No selections.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The variant selected in `set`, if any.
    #[inline]
    #[must_use]
    pub fn get(&self, set: TokenId) -> Option<TokenId> {
        self.0.get(&set).copied()
    }

    /// Whether `set` has a selection.
    #[inline]
    #[must_use]
    pub fn contains(&self, set: TokenId) -> bool {
        self.0.contains_key(&set)
    }

    /// Selects `variant` in `set`, returning the selection it replaces.
    pub fn insert(&mut self, set: TokenId, variant: TokenId) -> Option<TokenId> {
        self.0.insert(set, variant)
    }

    /// Removes the selection of `set`, returning it.
    pub fn remove(&mut self, set: TokenId) -> Option<TokenId> {
        self.0.remove(&set)
    }

    /// Removes every selection.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// The number of sets with a selection.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no set has a selection.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Each selection as `(set, variant)`, in no particular order.
    pub fn iter(&self) -> VariantSelectionsIter<'_> {
        VariantSelectionsIter(self.0.iter())
    }

    /// The map itself, for composition's own lookups.
    pub(crate) fn as_map(&self) -> &HashMap<TokenId, TokenId> {
        &self.0
    }

    /// The map itself, mutably, for composition's own updates.
    pub(crate) fn as_map_mut(&mut self) -> &mut HashMap<TokenId, TokenId> {
        &mut self.0
    }
}

impl FromIterator<(TokenId, TokenId)> for VariantSelections {
    fn from_iter<I: IntoIterator<Item = (TokenId, TokenId)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl Extend<(TokenId, TokenId)> for VariantSelections {
    fn extend<I: IntoIterator<Item = (TokenId, TokenId)>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl<'a> IntoIterator for &'a VariantSelections {
    type Item = (TokenId, TokenId);
    type IntoIter = VariantSelectionsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The selections of a [`VariantSelections`] as `(set, variant)`
/// ([`VariantSelections::iter`]), in no particular order.
#[derive(Clone, Debug)]
pub struct VariantSelectionsIter<'a>(hashbrown::hash_map::Iter<'a, TokenId, TokenId>);

impl Iterator for VariantSelectionsIter<'_> {
    type Item = (TokenId, TokenId);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(set, variant)| (*set, *variant))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl ExactSizeIterator for VariantSelectionsIter<'_> {}

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
    pub fn is_selected(&self, selections: &VariantSelections) -> bool {
        self.chain()
            .all(|(set, variant)| selections.get(set) == Some(variant))
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
    sets: &'a VariantSets,
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
    sets: &'a mut VariantSets,
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
    selections: Option<&'a VariantSelections>,
}

impl<'a> VariantBranches<'a> {
    fn new(sets: &'a VariantSets, selections: Option<&'a VariantSelections>) -> Self {
        let mut branches = Self {
            pending: Vec::new(),
            selections,
        };
        branches.push_sets(&[], sets);
        branches
    }

    fn push_sets(&mut self, enclosing: &[(TokenId, TokenId)], sets: &'a VariantSets) {
        for (set, set_spec) in sets {
            match self.selections {
                Some(selections) => {
                    let Some(variant) = selections.get(*set) else {
                        continue;
                    };
                    if let Some(spec) = set_spec.variants.get(&variant) {
                        self.pending.push(VariantBranch {
                            enclosing: enclosing.to_vec(),
                            set: *set,
                            variant,
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
    /// Variants keyed by variant name, in the order they were added.
    pub variants: Variants,
}

/// The variant specs of a variant set, keyed by variant name, in the order
/// they were added: a new variant goes last, a replaced one keeps its
/// place, and a removed one leaves the others in order.
///
/// This is OpenUSD's `variantChildren`, the order
/// `SdfVariantSetSpec::GetVariants` lists (`pxr/usd/sdf/variantSetSpec.cpp`):
/// a text layer's variants as its `variantSet` block lists them, a crate
/// file's as its `variantChildren` holds them. Writers sort variants by
/// name, as `Sdf_WriteVariantSet` does (`pxr/usd/sdf/fileIO_Common.h`), and
/// two sets holding the same variants compare equal whatever their order.
///
/// The variants are held in one vector, sized exactly by the readers:
/// lookups are linear, as a set has few variants.
///
/// Spec: AOUSD Core §7.6.6 (variant set specs), §7.6.7 (variant specs).
#[derive(Clone, Debug, Default)]
pub struct Variants {
    variants: Vec<(TokenId, VariantSpec)>,
}

impl Variants {
    /// No variants.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            variants: Vec::new(),
        }
    }

    /// No variants, with room for `capacity` of them.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            variants: Vec::with_capacity(capacity),
        }
    }

    /// The number of variants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.variants.len()
    }

    /// Whether there are no variants.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.variants.is_empty()
    }

    /// Makes room for `additional` more variants.
    pub fn reserve(&mut self, additional: usize) {
        self.variants.reserve_exact(additional);
    }

    /// Drops spare room, as a reader does once a set is complete.
    pub fn shrink_to_fit(&mut self) {
        self.variants.shrink_to_fit();
    }

    /// The place of the variant named `name` among the variants.
    #[must_use]
    pub fn index_of(&self, name: &TokenId) -> Option<usize> {
        self.variants
            .iter()
            .position(|(variant, _)| variant == name)
    }

    /// The variant named `name`.
    #[must_use]
    pub fn get(&self, name: &TokenId) -> Option<&VariantSpec> {
        self.index_of(name).map(|i| &self.variants[i].1)
    }

    /// The variant named `name`, mutably.
    #[must_use]
    pub fn get_mut(&mut self, name: &TokenId) -> Option<&mut VariantSpec> {
        self.index_of(name).map(|i| &mut self.variants[i].1)
    }

    /// Whether a variant is named `name`.
    #[must_use]
    pub fn contains_key(&self, name: &TokenId) -> bool {
        self.index_of(name).is_some()
    }

    /// Sets the variant named `name` to `variant`, in its place when there
    /// is one (returning it) and last otherwise.
    pub fn insert(&mut self, name: TokenId, variant: VariantSpec) -> Option<VariantSpec> {
        match self.index_of(&name) {
            Some(i) => Some(core::mem::replace(&mut self.variants[i].1, variant)),
            None => {
                self.variants.push((name, variant));
                None
            }
        }
    }

    /// Adds the variant `variant` named `name` at `index` among the
    /// variants, which must not have one of that name.
    ///
    /// # Panics
    ///
    /// When `index` is past the last variant, or a variant is named `name`.
    pub fn insert_at(&mut self, index: usize, name: TokenId, variant: VariantSpec) {
        assert!(!self.contains_key(&name), "a variant of that name");
        self.variants.insert(index, (name, variant));
    }

    /// The variant named `name`, added last, empty, when there is none.
    pub fn get_or_insert_default(&mut self, name: TokenId) -> &mut VariantSpec {
        let i = self.index_of(&name).unwrap_or_else(|| {
            self.variants.push((name, VariantSpec::default()));
            self.variants.len() - 1
        });
        &mut self.variants[i].1
    }

    /// Removes the variant named `name`, keeping the others in order.
    pub fn remove(&mut self, name: &TokenId) -> Option<VariantSpec> {
        self.index_of(name).map(|i| self.variants.remove(i).1)
    }

    /// Keeps the variants `keep` accepts, in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&TokenId, &mut VariantSpec) -> bool) {
        self.variants
            .retain_mut(|(name, variant)| keep(name, variant));
    }

    /// Removes every variant.
    pub fn clear(&mut self) {
        self.variants.clear();
    }

    /// The variants with their names, in order.
    pub fn iter(&self) -> VariantsIter<'_> {
        self.into_iter()
    }

    /// The variants with their names, mutably, in order.
    pub fn iter_mut(&mut self) -> VariantsIterMut<'_> {
        self.into_iter()
    }

    /// The variant names, in order.
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &TokenId> + DoubleEndedIterator {
        self.variants.iter().map(|(name, _)| name)
    }

    /// The variants, in order.
    pub fn values(&self) -> impl ExactSizeIterator<Item = &VariantSpec> + DoubleEndedIterator {
        self.variants.iter().map(|(_, variant)| variant)
    }

    /// The variants, mutably, in order.
    pub fn values_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = &mut VariantSpec> + DoubleEndedIterator {
        self.variants.iter_mut().map(|(_, variant)| variant)
    }

    /// The bytes the variants' vector holds, spare room included.
    #[must_use]
    pub fn allocation_size(&self) -> usize {
        self.variants.capacity() * size_of::<(TokenId, VariantSpec)>()
    }
}

/// Two sets of variants are equal when they hold the same variants by
/// name, whatever their order: writers sort variants by name.
impl PartialEq for Variants {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(name, variant)| other.get(name) == Some(variant))
    }
}

/// The variants of a [`Variants`] with their names, in order.
pub type VariantsIter<'a> = core::iter::Map<
    core::slice::Iter<'a, (TokenId, VariantSpec)>,
    fn(&'a (TokenId, VariantSpec)) -> (&'a TokenId, &'a VariantSpec),
>;

/// The variants of a [`Variants`] with their names, mutably, in order.
pub type VariantsIterMut<'a> = core::iter::Map<
    core::slice::IterMut<'a, (TokenId, VariantSpec)>,
    fn(&'a mut (TokenId, VariantSpec)) -> (&'a TokenId, &'a mut VariantSpec),
>;

impl<'a> IntoIterator for &'a Variants {
    type Item = (&'a TokenId, &'a VariantSpec);
    type IntoIter = VariantsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.variants.iter().map(|(name, variant)| (name, variant))
    }
}

impl<'a> IntoIterator for &'a mut Variants {
    type Item = (&'a TokenId, &'a mut VariantSpec);
    type IntoIter = VariantsIterMut<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.variants
            .iter_mut()
            .map(|(name, variant)| (&*name, variant))
    }
}

impl IntoIterator for Variants {
    type Item = (TokenId, VariantSpec);
    type IntoIter = alloc::vec::IntoIter<(TokenId, VariantSpec)>;

    fn into_iter(self) -> Self::IntoIter {
        self.variants.into_iter()
    }
}

impl FromIterator<(TokenId, VariantSpec)> for Variants {
    fn from_iter<I: IntoIterator<Item = (TokenId, VariantSpec)>>(variants: I) -> Self {
        let mut all = Self::new();
        all.extend(variants);
        all
    }
}

impl<const N: usize> From<[(TokenId, VariantSpec); N]> for Variants {
    fn from(variants: [(TokenId, VariantSpec); N]) -> Self {
        let mut all = Self::with_capacity(N);
        all.extend(variants);
        all
    }
}

impl Extend<(TokenId, VariantSpec)> for Variants {
    fn extend<I: IntoIterator<Item = (TokenId, VariantSpec)>>(&mut self, variants: I) {
        for (name, variant) in variants {
            self.insert(name, variant);
        }
    }
}

impl core::ops::Index<&TokenId> for Variants {
    type Output = VariantSpec;

    fn index(&self, name: &TokenId) -> &VariantSpec {
        self.get(name).expect("no variant of that name")
    }
}

/// The variant set specs of a prim spec or variant spec, keyed by set
/// name, in the order they were added: a new set goes last, a replaced one
/// keeps its place, and a removed one leaves the others in order.
///
/// This is OpenUSD's `variantSetChildren`, the order a text layer's
/// `variantSet` blocks are written in (`Sdf_WritePrimVariantSets`,
/// `pxr/usd/sdf/fileIO_Common.h`): set specs are created as a text layer
/// lists them in an explicit or added `variantSets` and then as its
/// blocks introduce them, as `UsdVariantSets::AddVariantSet` creates them,
/// and as a crate file's `variantSetChildren` holds them. Which sets a
/// prim composes, and in which order, is its `variantSetNames` list op
/// instead ([`PrimSpec::variant_set_names`]).
///
/// Lookups are linear: a spec has few variant sets.
///
/// Spec: AOUSD Core §7.3.6 (prim and variant specs contain variant set
/// specs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VariantSets {
    sets: Vec<(TokenId, VariantSetSpec)>,
}

impl VariantSets {
    /// No variant sets.
    #[must_use]
    pub const fn new() -> Self {
        Self { sets: Vec::new() }
    }

    /// The number of sets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    /// Whether there are no sets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// The place of the set named `name` among the sets.
    #[must_use]
    pub fn index_of(&self, name: &TokenId) -> Option<usize> {
        self.sets.iter().position(|(set, _)| set == name)
    }

    /// The set named `name`.
    #[must_use]
    pub fn get(&self, name: &TokenId) -> Option<&VariantSetSpec> {
        self.index_of(name).map(|i| &self.sets[i].1)
    }

    /// The set named `name`, mutably.
    #[must_use]
    pub fn get_mut(&mut self, name: &TokenId) -> Option<&mut VariantSetSpec> {
        self.index_of(name).map(|i| &mut self.sets[i].1)
    }

    /// Whether a set is named `name`.
    #[must_use]
    pub fn contains_key(&self, name: &TokenId) -> bool {
        self.index_of(name).is_some()
    }

    /// Sets the set named `name` to `set`, in its place when there is one
    /// (returning it) and last otherwise.
    pub fn insert(&mut self, name: TokenId, set: VariantSetSpec) -> Option<VariantSetSpec> {
        match self.index_of(&name) {
            Some(i) => Some(core::mem::replace(&mut self.sets[i].1, set)),
            None => {
                self.sets.push((name, set));
                None
            }
        }
    }

    /// Adds the set `set` named `name` at `index` among the sets, which
    /// must not have one of that name.
    ///
    /// # Panics
    ///
    /// When `index` is past the last set, or a set is named `name`.
    pub fn insert_at(&mut self, index: usize, name: TokenId, set: VariantSetSpec) {
        assert!(!self.contains_key(&name), "a variant set of that name");
        self.sets.insert(index, (name, set));
    }

    /// The set named `name`, added last, empty, when there is none.
    pub fn get_or_insert_default(&mut self, name: TokenId) -> &mut VariantSetSpec {
        let i = self.index_of(&name).unwrap_or_else(|| {
            self.sets.push((name, VariantSetSpec::default()));
            self.sets.len() - 1
        });
        &mut self.sets[i].1
    }

    /// Removes the set named `name`, keeping the others in order.
    pub fn remove(&mut self, name: &TokenId) -> Option<VariantSetSpec> {
        self.index_of(name).map(|i| self.sets.remove(i).1)
    }

    /// Keeps the sets `keep` accepts, in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&TokenId, &mut VariantSetSpec) -> bool) {
        self.sets.retain_mut(|(name, set)| keep(name, set));
    }

    /// The sets, in order.
    pub fn iter(&self) -> VariantSetsIter<'_> {
        self.into_iter()
    }

    /// The sets, mutably, in order.
    pub fn iter_mut(&mut self) -> VariantSetsIterMut<'_> {
        self.into_iter()
    }

    /// The set names, in order.
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &TokenId> + DoubleEndedIterator {
        self.sets.iter().map(|(name, _)| name)
    }

    /// The sets, in order.
    pub fn values(&self) -> impl ExactSizeIterator<Item = &VariantSetSpec> + DoubleEndedIterator {
        self.sets.iter().map(|(_, set)| set)
    }

    /// The sets, mutably, in order.
    pub fn values_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = &mut VariantSetSpec> + DoubleEndedIterator {
        self.sets.iter_mut().map(|(_, set)| set)
    }
}

/// The sets of a [`VariantSets`] with their names, in order.
pub type VariantSetsIter<'a> = core::iter::Map<
    core::slice::Iter<'a, (TokenId, VariantSetSpec)>,
    fn(&'a (TokenId, VariantSetSpec)) -> (&'a TokenId, &'a VariantSetSpec),
>;

/// The sets of a [`VariantSets`] with their names, mutably, in order.
pub type VariantSetsIterMut<'a> = core::iter::Map<
    core::slice::IterMut<'a, (TokenId, VariantSetSpec)>,
    fn(&'a mut (TokenId, VariantSetSpec)) -> (&'a TokenId, &'a mut VariantSetSpec),
>;

impl<'a> IntoIterator for &'a VariantSets {
    type Item = (&'a TokenId, &'a VariantSetSpec);
    type IntoIter = VariantSetsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.sets.iter().map(|(name, set)| (name, set))
    }
}

impl<'a> IntoIterator for &'a mut VariantSets {
    type Item = (&'a TokenId, &'a mut VariantSetSpec);
    type IntoIter = VariantSetsIterMut<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.sets.iter_mut().map(|(name, set)| (&*name, set))
    }
}

impl IntoIterator for VariantSets {
    type Item = (TokenId, VariantSetSpec);
    type IntoIter = alloc::vec::IntoIter<(TokenId, VariantSetSpec)>;

    fn into_iter(self) -> Self::IntoIter {
        self.sets.into_iter()
    }
}

impl FromIterator<(TokenId, VariantSetSpec)> for VariantSets {
    fn from_iter<I: IntoIterator<Item = (TokenId, VariantSetSpec)>>(sets: I) -> Self {
        let mut all = Self::new();
        all.extend(sets);
        all
    }
}

impl Extend<(TokenId, VariantSetSpec)> for VariantSets {
    fn extend<I: IntoIterator<Item = (TokenId, VariantSetSpec)>>(&mut self, sets: I) {
        for (name, set) in sets {
            self.insert(name, set);
        }
    }
}

impl core::ops::Index<&TokenId> for VariantSets {
    type Output = VariantSetSpec;

    fn index(&self, name: &TokenId) -> &VariantSetSpec {
        self.get(name).expect("no variant set of that name")
    }
}

/// Opinions for a prim at a path.
///
/// The fields most prim specs author are held inline. Those few author
/// (inherits and specializes, variant sets and selections, reorder
/// statements and the variant branch a spec is authored in) are held out
/// of line in a [`RarePrimFields`], allocated once one of them is
/// authored: read them through the accessors of the same names, which
/// read as empty when none is authored, and author them through
/// [`PrimSpec::rare_mut`].
#[derive(Clone, Debug, Default)]
#[doc(alias = "SdfPrimSpec")]
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
    /// Authored child prim names in this layer, in file order.
    ///
    /// This is used as a deterministic baseline for child ordering. Child
    /// ordering is then further refined by applying `prim_order` (`reorder
    /// nameChildren`) opinions across the prim stack.
    ///
    /// Spec: AOUSD Core §11 (stage population) and supplemental suite child
    /// ordering tests (e.g. `BasicListEditingWithInherits_root`).
    pub authored_children: Vec<TokenId>,
    /// References arcs (a `ListOp` chain across the layer stack).
    pub references: SparseListOp<Reference>,
    /// Payloads arcs (a `ListOp` chain across the layer stack).
    ///
    /// Payloads are structurally identical to references but support deferred
    /// loading. When loaded, they behave like references with the same namespace
    /// mapping. Their position in LIVERPS is between References and Specializes.
    ///
    /// Spec: AOUSD Core §10 (payloads arc, §5.1.22).
    pub payloads: SparseListOp<Reference>,
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
    /// The rarely authored fields, `None` until one is authored. Crate
    /// code borrows it directly where it needs another field mutably at
    /// the same time.
    pub(crate) rare: Option<Box<RarePrimFields>>,
}

/// The fields of a [`PrimSpec`] few prim specs author, held out of line
/// so that a prim spec authoring none of them does not carry their
/// storage.
///
/// A spec allocates them once one is authored ([`PrimSpec::rare_mut`]);
/// fields emptied again stay allocated and read, and compare, as
/// unauthored.
///
/// Spec: AOUSD Core §7.6.2 (prim spec fields).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RarePrimFields {
    /// Inherits arcs (a `ListOp` chain across the layer stack).
    ///
    /// Spec: AOUSD Core §10 (inherits arc), with ordering via §12.4 (`ListOps`).
    pub inherits: SparseListOp<PathId>,
    /// Specializes arcs (a `ListOp` chain across the layer stack).
    ///
    /// Specializes is similar to inherits but sits at the weakest position in
    /// LIVERPS. Unlike references, specializes propagates through all levels of
    /// referencing.
    ///
    /// Spec: AOUSD Core §10 (specializes arc, §5.1.33), with ordering via §12.4 (`ListOps`).
    pub specializes: SparseListOp<PathId>,
    /// Authored variant selections (set -> chosen variant).
    pub variant_selections: VariantSelections,
    /// Authored variant sets of this prim spec (`/P{v=}`), keyed by set
    /// name, in the order they were added (OpenUSD's
    /// `variantSetChildren`). Sets nested in a branch belong to that branch's
    /// [`VariantSpec::variant_sets`]; [`PrimSpec::variant_branches`] walks
    /// them all.
    ///
    /// Spec: AOUSD Core §7.3.6 (prim specs contain variant set specs).
    pub variant_sets: VariantSets,
    /// The `variantSetNames` list op of this prim spec (`variantSets` in
    /// USDA), as authored.
    ///
    /// Composition evaluates only the sets a site lists: the list ops of
    /// its specs applied weakest to strongest across the layer stack. A
    /// variant set spec in [`PrimSpec::variant_sets`] that no list names
    /// contributes nothing, and a listed set without one contributes no
    /// opinions. The composed order is the evaluation order of the sets.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §12.4 (list ops),
    /// §10.5 (variant selection). OpenUSD: `PcpComposeSiteVariantSets` in
    /// `pxr/usd/pcp/composeSite.cpp`.
    pub variant_set_names: SparseListOp<TokenId>,
    /// Authored property ordering (`reorder properties = [...]`).
    ///
    /// OpenUSD sorts composed property names and then applies the strongest
    /// `propertyOrder` opinion (`UsdPrim::ApplyPropertyOrder` in
    /// `pxr/usd/usd/prim.cpp`).
    pub property_order: Option<Vec<TokenId>>,
    /// Optional child ordering (aka `primOrder` in the supplemental suite).
    ///
    /// This is used during stage population to produce deterministic, authored
    /// child ordering (rather than purely lexicographic ordering).
    ///
    /// Spec: AOUSD Core §11 (stage population), plus the supplemental
    /// parser’s `primOrder` field (`reorder nameChildren = [...]`).
    pub prim_order: Option<Vec<TokenId>>,
    /// Variant selections required to reach this concrete prim spec in its
    /// defining layer.
    ///
    /// This preserves composed provenance for prims authored beneath variant
    /// branches so descendant local opinions can retain their variant-qualified
    /// source identity.
    pub outer_variant_sites: Vec<VariantSelectionSite>,
}

/// Compares the authored opinions: rare fields allocated but emptied
/// compare equal to rare fields never authored.
impl PartialEq for PrimSpec {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            specifier,
            type_name,
            fields,
            properties,
            authored_children,
            references,
            payloads,
            instanceable,
            active,
            rare,
        } = self;
        let rare_eq = match (rare.as_deref(), other.rare.as_deref()) {
            (Some(a), Some(b)) => a == b,
            (Some(only), None) | (None, Some(only)) => only.is_empty(),
            (None, None) => true,
        };
        *specifier == other.specifier
            && *type_name == other.type_name
            && *fields == other.fields
            && *properties == other.properties
            && *authored_children == other.authored_children
            && *references == other.references
            && *payloads == other.payloads
            && *instanceable == other.instanceable
            && *active == other.active
            && rare_eq
    }
}

impl RarePrimFields {
    /// Whether none of the fields is authored: each reads as it does on a
    /// prim spec that never allocated them.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let Self {
            inherits,
            specializes,
            variant_selections,
            variant_sets,
            variant_set_names,
            property_order,
            prim_order,
            outer_variant_sites,
        } = self;
        inherits.is_empty()
            && specializes.is_empty()
            && variant_selections.is_empty()
            && variant_sets.is_empty()
            && variant_set_names.is_empty()
            && property_order.is_none()
            && prim_order.is_none()
            && outer_variant_sites.is_empty()
    }
}

/// No variant sets, for a prim spec authoring none.
static NO_VARIANT_SETS: VariantSets = VariantSets::new();

/// An empty path list op, for a prim spec authoring no inherits or
/// specializes.
const NO_PATHS: &ListOp<PathId> = &ListOp::new();

/// An empty token list op, for a prim spec authoring no `variantSetNames`.
const NO_TOKENS: &ListOp<TokenId> = &ListOp::new();

impl PrimSpec {
    /// The rarely authored fields, if this spec has allocated them: `None`
    /// when it authors none of them (see [`RarePrimFields`]).
    #[inline]
    #[must_use]
    pub fn rare(&self) -> Option<&RarePrimFields> {
        self.rare.as_deref()
    }

    /// The rarely authored fields to author into, allocated first if this
    /// spec has none.
    #[inline]
    pub fn rare_mut(&mut self) -> &mut RarePrimFields {
        self.rare.get_or_insert_with(Box::default)
    }

    /// The rarely authored fields to edit in place, if this spec has
    /// allocated them: `None`, and no allocation, otherwise. Edits that
    /// only remove or rewrite opinions use this rather than
    /// [`Self::rare_mut`].
    #[inline]
    pub fn authored_rare_mut(&mut self) -> Option<&mut RarePrimFields> {
        self.rare.as_deref_mut()
    }

    /// Inherits arcs (a `ListOp` chain across the layer stack), empty when
    /// none is authored.
    ///
    /// Spec: AOUSD Core §10 (inherits arc), with ordering via §12.4 (`ListOps`).
    #[inline]
    #[must_use]
    pub fn inherits(&self) -> &ListOp<PathId> {
        self.rare.as_deref().map_or(NO_PATHS, |rare| &rare.inherits)
    }

    /// Specializes arcs (a `ListOp` chain across the layer stack), empty
    /// when none is authored.
    ///
    /// Spec: AOUSD Core §10 (specializes arc, §5.1.33), with ordering via §12.4 (`ListOps`).
    #[inline]
    #[must_use]
    pub fn specializes(&self) -> &ListOp<PathId> {
        self.rare
            .as_deref()
            .map_or(NO_PATHS, |rare| &rare.specializes)
    }

    /// The authored variant selections (set -> chosen variant), `None` when
    /// none is authored.
    ///
    /// Unlike the other accessors this does not read as an empty map: a
    /// hash map has no empty value to borrow without allocating one.
    #[inline]
    #[must_use]
    pub fn variant_selections(&self) -> Option<&VariantSelections> {
        self.rare
            .as_deref()
            .map(|rare| &rare.variant_selections)
            .filter(|selections| !selections.is_empty())
    }

    /// The variant authored as selected in `set`, if any.
    #[inline]
    #[must_use]
    pub fn variant_selection(&self, set: TokenId) -> Option<TokenId> {
        self.rare
            .as_deref()
            .and_then(|rare| rare.variant_selections.get(set))
    }

    /// The variant sets of this prim spec (see
    /// [`RarePrimFields::variant_sets`]), empty when none is authored.
    ///
    /// Spec: AOUSD Core §7.3.6 (prim specs contain variant set specs).
    #[inline]
    #[must_use]
    pub fn variant_sets(&self) -> &VariantSets {
        self.rare
            .as_deref()
            .map_or(&NO_VARIANT_SETS, |rare| &rare.variant_sets)
    }

    /// The `variantSetNames` list op of this prim spec (see
    /// [`RarePrimFields::variant_set_names`]), empty when none is authored.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`).
    #[inline]
    #[must_use]
    pub fn variant_set_names(&self) -> &ListOp<TokenId> {
        self.rare
            .as_deref()
            .map_or(NO_TOKENS, |rare| &rare.variant_set_names)
    }

    /// The authored property ordering (`reorder properties = [...]`), if
    /// any.
    ///
    /// OpenUSD sorts composed property names and then applies the strongest
    /// `propertyOrder` opinion (`UsdPrim::ApplyPropertyOrder` in
    /// `pxr/usd/usd/prim.cpp`).
    #[inline]
    #[must_use]
    pub fn property_order(&self) -> Option<&[TokenId]> {
        self.rare.as_deref()?.property_order.as_deref()
    }

    /// The authored child ordering (`reorder nameChildren = [...]`), if
    /// any.
    ///
    /// Spec: AOUSD Core §11 (stage population), plus the supplemental
    /// parser’s `primOrder` field.
    #[inline]
    #[must_use]
    pub fn prim_order(&self) -> Option<&[TokenId]> {
        self.rare.as_deref()?.prim_order.as_deref()
    }

    /// The variant selections required to reach this prim spec in its
    /// defining layer, outermost first: empty for a spec outside variant
    /// branches (see [`RarePrimFields::outer_variant_sites`]).
    #[inline]
    #[must_use]
    pub fn outer_variant_sites(&self) -> &[VariantSelectionSite] {
        self.rare
            .as_deref()
            .map_or(&[], |rare| &rare.outer_variant_sites)
    }

    /// Sets the authored child ordering (`reorder nameChildren`; builder,
    /// consuming); see [`Self::prim_order`].
    pub fn with_prim_order(mut self, order: Vec<TokenId>) -> Self {
        self.rare_mut().prim_order = Some(order);
        self
    }

    /// Sets the variant branch this prim spec is authored in (builder,
    /// consuming); see [`Self::outer_variant_sites`].
    pub fn with_outer_variant_sites(mut self, sites: Vec<VariantSelectionSite>) -> Self {
        if !sites.is_empty() || self.rare.is_some() {
            self.rare_mut().outer_variant_sites = sites;
        }
        self
    }
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
        self.references.make_mut().append.push(reference);
        self
    }

    /// Appends a reference arc (builder, consuming).
    pub fn with_reference(mut self, reference: Reference) -> Self {
        self.references.make_mut().append.push(reference);
        self
    }

    /// Appends an inherit arc.
    pub fn add_inherit(&mut self, path: PathId) -> &mut Self {
        self.rare_mut().inherits.make_mut().append.push(path);
        self
    }

    /// Appends an inherit arc (builder, consuming).
    pub fn with_inherit(mut self, path: PathId) -> Self {
        self.rare_mut().inherits.make_mut().append.push(path);
        self
    }

    /// Appends a payload arc.
    pub fn add_payload(&mut self, payload: Reference) -> &mut Self {
        self.payloads.make_mut().append.push(payload);
        self
    }

    /// Appends a payload arc (builder, consuming).
    pub fn with_payload(mut self, payload: Reference) -> Self {
        self.payloads.make_mut().append.push(payload);
        self
    }

    /// Appends a specialize arc.
    pub fn add_specialize(&mut self, path: PathId) -> &mut Self {
        self.rare_mut().specializes.make_mut().append.push(path);
        self
    }

    /// Appends a specialize arc (builder, consuming).
    pub fn with_specialize(mut self, path: PathId) -> Self {
        self.rare_mut().specializes.make_mut().append.push(path);
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
        VariantBranches::new(self.variant_sets(), None)
    }

    /// Iterates the variant specs `selections` (set → variant) selects:
    /// the selected branch of each of this prim spec's sets, then of each
    /// set nested in a selected branch. A branch whose enclosing branch is
    /// not selected is not visited, whatever its own set selects.
    ///
    /// Spec: AOUSD Core §10.3.2.5 (only the selected variant contributes).
    pub fn selected_variant_branches<'a>(
        &'a self,
        selections: &'a VariantSelections,
    ) -> VariantBranches<'a> {
        VariantBranches::new(self.variant_sets(), Some(selections))
    }

    /// Returns the variant spec at `chain` (outermost first) below this
    /// prim spec: `[(a, x), (b, y)]` names `{a=x}{b=y}`. `None` for an
    /// empty chain or one this prim spec does not author.
    #[must_use]
    pub fn variant_spec(&self, chain: &[(TokenId, TokenId)]) -> Option<&VariantSpec> {
        variant_spec_in(self.variant_sets(), chain)
    }

    /// Returns the variant spec at `chain` mutably (see
    /// [`PrimSpec::variant_spec`]).
    pub fn variant_spec_mut(&mut self, chain: &[(TokenId, TokenId)]) -> Option<&mut VariantSpec> {
        variant_spec_in_mut(&mut self.authored_rare_mut()?.variant_sets, chain)
    }

    /// Returns the variant sets held at `chain` below this prim spec, with
    /// the `variantSetNames` list op of the spec holding them: this prim
    /// spec's own for an empty chain, otherwise those nested in the variant
    /// spec it names.
    #[must_use]
    pub fn variant_sets_in(
        &self,
        chain: &[(TokenId, TokenId)],
    ) -> Option<(&VariantSets, &ListOp<TokenId>)> {
        if chain.is_empty() {
            return Some((self.variant_sets(), self.variant_set_names()));
        }
        let spec = self.variant_spec(chain)?;
        Some((&spec.variant_sets, &spec.variant_set_names))
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

/// The prim specs of a layer inside variant branches, by prim path
/// ([`Layer::variant_prims`]): for each path, one spec per branch that
/// authors it, each identified by its [`PrimSpec::outer_variant_sites`].
///
/// Paths come in no particular order; [`Layer::prim_specs_in_order`] lists
/// every spec in namespace order. Two views are equal when they hold the
/// same specs at the same paths, in the same order at each path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VariantPrims<'a>(&'a HashMap<PathId, Vec<PrimSpec>>);

impl<'a> VariantPrims<'a> {
    /// The specs at `path`, one per branch that authors it.
    #[must_use]
    pub fn get(self, path: &PathId) -> Option<&'a [PrimSpec]> {
        self.0.get(path).map(Vec::as_slice)
    }

    /// The number of paths.
    #[must_use]
    pub fn len(self) -> usize {
        self.0.len()
    }

    /// Whether no variant branch authors a prim spec.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// The paths and their specs, in no particular order.
    pub fn iter(self) -> impl ExactSizeIterator<Item = (&'a PathId, &'a [PrimSpec])> {
        self.0.iter().map(|(path, specs)| (path, specs.as_slice()))
    }

    /// The paths, in no particular order.
    pub fn keys(self) -> impl ExactSizeIterator<Item = &'a PathId> {
        self.0.keys()
    }

    /// The specs at each path, in no particular order of paths.
    pub fn values(self) -> impl ExactSizeIterator<Item = &'a [PrimSpec]> {
        self.0.values().map(Vec::as_slice)
    }
}

/// The prim specs of a layer outside variant branches, keyed by prim path
/// ([`Layer::prims`]), read and changed as a map of them.
///
/// Each spec stays where it was placed until it is removed: a layer
/// growing to more paths moves no spec, and a reader placing a spec it
/// built moves none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerPrims {
    specs: HashMap<PathId, Box<PrimSpec>>,
}

/// An iterator over the paths and specs of a [`LayerPrims`], in no
/// particular order.
#[derive(Clone, Debug)]
pub struct LayerPrimsIter<'a>(hashbrown::hash_map::Iter<'a, PathId, Box<PrimSpec>>);

impl<'a> Iterator for LayerPrimsIter<'a> {
    type Item = (&'a PathId, &'a PrimSpec);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(path, spec)| (path, &**spec))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl ExactSizeIterator for LayerPrimsIter<'_> {}

impl core::iter::FusedIterator for LayerPrimsIter<'_> {}

/// An iterator over the paths and specs of a [`LayerPrims`], for editing,
/// in no particular order.
#[derive(Debug)]
pub struct LayerPrimsIterMut<'a>(hashbrown::hash_map::IterMut<'a, PathId, Box<PrimSpec>>);

impl<'a> Iterator for LayerPrimsIterMut<'a> {
    type Item = (&'a PathId, &'a mut PrimSpec);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(path, spec)| (path, &mut **spec))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl ExactSizeIterator for LayerPrimsIterMut<'_> {}

impl core::iter::FusedIterator for LayerPrimsIterMut<'_> {}

/// The paths and specs a [`LayerPrims`] held, in no particular order.
#[derive(Debug)]
pub struct LayerPrimsIntoIter(hashbrown::hash_map::IntoIter<PathId, Box<PrimSpec>>);

impl Iterator for LayerPrimsIntoIter {
    type Item = (PathId, PrimSpec);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(path, spec)| (path, *spec))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl ExactSizeIterator for LayerPrimsIntoIter {}

impl core::iter::FusedIterator for LayerPrimsIntoIter {}

impl LayerPrims {
    /// The spec at `path`, if there is one.
    #[must_use]
    pub fn get(&self, path: &PathId) -> Option<&PrimSpec> {
        self.specs.get(path).map(|spec| &**spec)
    }

    /// The spec at `path` for editing, if there is one.
    pub fn get_mut(&mut self, path: &PathId) -> Option<&mut PrimSpec> {
        self.specs.get_mut(path).map(|spec| &mut **spec)
    }

    /// Whether a spec is at `path`.
    #[must_use]
    pub fn contains_key(&self, path: &PathId) -> bool {
        self.specs.contains_key(path)
    }

    /// The number of specs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// Whether there are no specs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// The paths and specs, in no particular order.
    pub fn iter(&self) -> LayerPrimsIter<'_> {
        LayerPrimsIter(self.specs.iter())
    }

    /// The paths and specs for editing, in no particular order.
    pub fn iter_mut(&mut self) -> LayerPrimsIterMut<'_> {
        LayerPrimsIterMut(self.specs.iter_mut())
    }

    /// The paths, in no particular order.
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &PathId> {
        self.specs.keys()
    }

    /// The specs, in no particular order.
    pub fn values(&self) -> impl ExactSizeIterator<Item = &PrimSpec> {
        self.specs.values().map(|spec| &**spec)
    }

    /// The specs for editing, in no particular order.
    pub fn values_mut(&mut self) -> impl ExactSizeIterator<Item = &mut PrimSpec> {
        self.specs.values_mut().map(|spec| &mut **spec)
    }

    /// Places `spec` at `path`, returning the spec it replaces.
    pub fn insert(&mut self, path: PathId, spec: PrimSpec) -> Option<PrimSpec> {
        self.insert_boxed(path, Box::new(spec)).map(|spec| *spec)
    }

    /// Removes the spec at `path`, returning it.
    pub fn remove(&mut self, path: &PathId) -> Option<PrimSpec> {
        self.remove_boxed(path).map(|spec| *spec)
    }

    /// Keeps only the specs `keep` accepts.
    pub fn retain(&mut self, mut keep: impl FnMut(&PathId, &mut PrimSpec) -> bool) {
        self.specs.retain(|path, spec| keep(path, spec));
    }

    /// Removes every spec.
    pub fn clear(&mut self) {
        self.specs.clear();
    }

    /// Makes room for `additional` more specs.
    pub fn reserve(&mut self, additional: usize) {
        self.specs.reserve(additional);
    }

    /// [`LayerPrims::insert`], moving the spec where it is.
    pub(crate) fn insert_boxed(
        &mut self,
        path: PathId,
        spec: Box<PrimSpec>,
    ) -> Option<Box<PrimSpec>> {
        self.specs.insert(path, spec)
    }

    /// [`LayerPrims::remove`], leaving the spec where it is.
    pub(crate) fn remove_boxed(&mut self, path: &PathId) -> Option<Box<PrimSpec>> {
        self.specs.remove(path)
    }

    /// The map entry at `path`.
    pub(crate) fn entry(
        &mut self,
        path: PathId,
    ) -> hashbrown::hash_map::Entry<'_, PathId, Box<PrimSpec>, hashbrown::DefaultHashBuilder> {
        self.specs.entry(path)
    }
}

impl<'a> IntoIterator for &'a LayerPrims {
    type Item = (&'a PathId, &'a PrimSpec);
    type IntoIter = LayerPrimsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for LayerPrims {
    type Item = (PathId, PrimSpec);
    type IntoIter = LayerPrimsIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        LayerPrimsIntoIter(self.specs.into_iter())
    }
}

impl<'a> IntoIterator for &'a mut LayerPrims {
    type Item = (&'a PathId, &'a mut PrimSpec);
    type IntoIter = LayerPrimsIterMut<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl core::ops::Index<&PathId> for LayerPrims {
    type Output = PrimSpec;

    fn index(&self, path: &PathId) -> &PrimSpec {
        self.get(path).expect("no prim spec at the path")
    }
}

/// A document layer.
///
/// Its content is read through accessors such as [`Layer::prims`] and
/// changed only through methods, each recording its change (see
/// [`Layer::generation`]), so a live stage drawing on the layer sees every
/// edit.
///
/// Two layers are equal when their content is: the [`Layer::generation`]
/// and [`Layer::structural_generation`] counters are not compared.
#[derive(Clone, Debug)]
#[doc(alias = "SdfLayer")]
pub struct Layer {
    /// Stable identifier for this layer.
    pub(crate) id: LayerId,
    /// Ordered sublayer includes. The layer itself is always stronger than its sublayers.
    pub(crate) sublayers: Vec<SublayerEntry>,
    /// How many `subLayerOffsets` the layer was read with, when that is
    /// not its number of sublayers (see [`Layer::unpaired_sublayer_offsets`]).
    pub(crate) unpaired_sublayer_offsets: Option<usize>,
    /// The authored `defaultPrim` token: the prim that references and
    /// payloads with no authored prim path target in this layer.
    ///
    /// The token is kept as authored, so a value that names no prim path is
    /// still visible; [`Layer::default_prim_path`] converts it.
    ///
    /// Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim`), §10.3.2.1 (references).
    pub(crate) default_prim: Option<TokenId>,
    /// All other authored layer metadata, in authored order: for example
    /// `upAxis`, `metersPerUnit`, `timeCodesPerSecond`, `framesPerSecond`,
    /// `startTimeCode`, `endTimeCode`, `customLayerData`, `documentation`
    /// and `comment`.
    ///
    /// Spec: AOUSD Core §7.6.1 (layer spec fields), §12.2.7 (layer metadata
    /// is read from the root layer, not composed).
    pub(crate) metadata: Vec<FieldEntry>,
    /// The authored `layerRelocates` entries, in authored order, or `None`
    /// when the layer authors no `layerRelocates` field. An authored but
    /// empty field (`relocates = {}`) is kept, as OpenUSD keeps it
    /// (`SdfLayer::HasRelocates`).
    ///
    /// Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`), §10.3.2.6
    /// (relocates are computed per layer stack from every layer's entries).
    pub(crate) relocates: Option<Vec<Relocate>>,
    /// Prim specs keyed by prim path.
    ///
    /// Each path holds one spec here: the spec authored outside any variant
    /// branch if there is one, otherwise the first spec ingested for it.
    /// Specs authored for the same path inside other variant branches are
    /// kept in [`Layer::variant_prims`]; see [`Layer::prim_specs`].
    pub(crate) prims: LayerPrims,
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
    pub(crate) variant_prims: HashMap<PathId, Vec<PrimSpec>>,
    /// Counts the edits made to this layer (see [`Layer::generation`]).
    pub(crate) generation: u64,
    /// Counts the edits that may change namespace or arcs (see
    /// [`Layer::structural_generation`]).
    pub(crate) structure: u64,
    pub(crate) change_history: crate::change_history::History<LayerChange>,
    /// The [`Layer::generation`] at which a reader found that no prim spec
    /// authors a reference or payload ([`Layer::note_no_reference_arcs`]);
    /// stale, and so unused, once the layer changes.
    pub(crate) no_reference_arcs: Option<u64>,
}

/// Bounded authored evidence, independent of each stage's synchronization cursor.
#[derive(Clone, Debug)]
pub(crate) struct LayerChange {
    generation: u64,
    paths: Option<Vec<PathId>>,
    /// Changed prims whose specs only support existing children, as an
    /// inert `over` created for an edit beneath it does: their own prim
    /// indexes change, not the namespace beneath them (see
    /// [`Layer::changed_supports_since`]).
    scope_ignored: Vec<PathId>,
}
impl crate::change_history::Record for LayerChange {
    fn revision(&self) -> u64 {
        self.generation
    }
    fn bytes(&self) -> usize {
        size_of::<Self>()
            + self
                .paths
                .as_ref()
                .map_or(0, |p| p.capacity() * size_of::<PathId>())
            + self.scope_ignored.capacity() * size_of::<PathId>()
    }
    fn items(&self) -> usize {
        self.paths.as_ref().map_or(0, Vec::len)
    }
}

impl PartialEq for Layer {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            id,
            sublayers,
            unpaired_sublayer_offsets,
            default_prim,
            metadata,
            prims,
            variant_prims,
            relocates,
            generation: _,
            structure: _,
            change_history: _,
            no_reference_arcs: _,
        } = self;
        *id == other.id
            && *sublayers == other.sublayers
            && *unpaired_sublayer_offsets == other.unpaired_sublayer_offsets
            && *default_prim == other.default_prim
            && *metadata == other.metadata
            && *prims == other.prims
            && *variant_prims == other.variant_prims
            && *relocates == other.relocates
    }
}

impl Layer {
    /// Rewrites every layer id this layer stores through `map`: its own
    /// [`Layer::id`], its sublayers', and those of the references and
    /// payloads of its prim and variant specs, nested variants included.
    ///
    /// A reader that numbers layers in another order than it reads them
    /// renumbers them with this; mapping a layer to
    /// [`LayerId::UNRESOLVED`] makes its arcs unresolved, keeping their
    /// asset paths. Records a structural change of the whole layer.
    pub fn map_layer_ids(&mut self, map: impl Fn(LayerId) -> LayerId) {
        fn arcs(op: &mut ListOp<Reference>, map: &dyn Fn(LayerId) -> LayerId) {
            for arc in op.lists_mut().flatten() {
                arc.layer = map(arc.layer);
            }
        }
        fn variant_sets(sets: &mut VariantSets, map: &dyn Fn(LayerId) -> LayerId) {
            for variant in sets.values_mut().flat_map(|set| set.variants.values_mut()) {
                if let Some(op) = variant.references.get_mut() {
                    arcs(op, map);
                }
                if let Some(op) = variant.payloads.get_mut() {
                    arcs(op, map);
                }
                variant_sets(&mut variant.variant_sets, map);
            }
        }
        // Renumbering changes no spec's arcs: a layer known to author none
        // stays known to, and its specs need no visit.
        let without_arcs = self.known_without_reference_arcs();
        self.touch_structure();
        self.id = map(self.id);
        for sublayer in &mut self.sublayers {
            sublayer.layer = map(sublayer.layer);
        }
        if without_arcs {
            self.no_reference_arcs = Some(self.generation);
            return;
        }
        let specs = self
            .prims
            .values_mut()
            .chain(self.variant_prims.values_mut().flatten());
        for spec in specs {
            if let Some(op) = spec.references.get_mut() {
                arcs(op, &map);
            }
            if let Some(op) = spec.payloads.get_mut() {
                arcs(op, &map);
            }
            if let Some(rare) = spec.authored_rare_mut() {
                variant_sets(&mut rare.variant_sets, &map);
            }
        }
    }

    /// Creates an empty layer with no sublayers or prims.
    pub fn new(id: LayerId) -> Self {
        Self {
            id,
            sublayers: Vec::new(),
            unpaired_sublayer_offsets: None,
            default_prim: None,
            metadata: Vec::new(),
            relocates: None,
            prims: LayerPrims::default(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
            no_reference_arcs: None,
        }
    }

    /// Records that no prim spec of this layer, in or out of a variant
    /// branch, authors a reference or payload, as the layer now is.
    ///
    /// A reader that ingested none says so after building the layer, so
    /// the passes that visit every spec's arcs ([`Layer::map_layer_ids`],
    /// a loader looking for asset paths authored as expressions) can skip
    /// the visit. The note lapses with the layer's next change
    /// ([`Layer::generation`]): an edit, a replaced spec, a reload.
    pub fn note_no_reference_arcs(&mut self) {
        debug_assert!(
            !self.authors_reference_arcs_in_specs(),
            "a layer noted as authoring no reference or payload authors one"
        );
        self.no_reference_arcs = Some(self.generation);
    }

    /// Whether this layer is known to author no reference or payload in
    /// any prim spec: a reader noted so ([`Layer::note_no_reference_arcs`])
    /// and the layer has not changed since. `false` says nothing either
    /// way.
    #[must_use]
    pub fn known_without_reference_arcs(&self) -> bool {
        self.no_reference_arcs == Some(self.generation)
    }

    /// Whether some prim spec, in or out of a variant branch, authors a
    /// reference or payload, found by visiting every spec.
    fn authors_reference_arcs_in_specs(&self) -> bool {
        fn arcs(references: &ListOp<Reference>, payloads: &ListOp<Reference>) -> bool {
            !references.is_empty() || !payloads.is_empty()
        }
        fn in_sets(sets: &VariantSets) -> bool {
            sets.values()
                .flat_map(|set| set.variants.values())
                .any(|variant| {
                    arcs(&variant.references, &variant.payloads) || in_sets(&variant.variant_sets)
                })
        }
        self.prims
            .values()
            .chain(self.variant_prims.values().flatten())
            .any(|spec| arcs(&spec.references, &spec.payloads) || in_sets(spec.variant_sets()))
    }

    /// Returns this layer's stable identifier in its store.
    #[must_use]
    pub fn id(&self) -> LayerId {
        self.id
    }

    /// Returns the ordered sublayer includes, strongest first. The layer
    /// itself is always stronger than its sublayers.
    ///
    /// Spec: AOUSD Core §7.6.1.2.1 (`subLayers`), §7.6.1.2.2
    /// (`subLayerOffsets`), §10.3.1 (sublayers).
    /// OpenUSD: `SdfLayer::GetSubLayerPaths` and `GetSubLayerOffsets`.
    #[must_use]
    pub fn sublayers(&self) -> &[SublayerEntry] {
        &self.sublayers
    }

    /// Returns the sublayer includes for editing.
    ///
    /// Records a structural change of the whole layer (see
    /// [`Layer::structural_generation`]): sublayers change the layer stack.
    /// Each sublayer then has its own offset again, so this clears
    /// [`Layer::unpaired_sublayer_offsets`].
    pub fn sublayers_mut(&mut self) -> &mut Vec<SublayerEntry> {
        self.touch_structure();
        self.unpaired_sublayer_offsets = None;
        &mut self.sublayers
    }

    /// How many `subLayerOffsets` the layer was read with, when that is
    /// not its number of sublayers; `None` when they pair.
    ///
    /// Only a USDC layer can author the two fields apart. OpenUSD 26.11
    /// then gives every sublayer the identity offset and reports
    /// `PcpErrorInvalidSublayerAndOffsetCount` (`_BuildLayerStack`,
    /// `pxr/usd/pcp/layerStack.cpp`); a reader does the same here, and
    /// composition reports [`InvalidSublayerAndOffsetCount`] for each layer
    /// stack holding the layer.
    ///
    /// Spec: AOUSD Core §7.6.1.2.2 (`subLayerOffsets` parallels
    /// `subLayers`), §12.3.2.1 (sublayer offsets).
    ///
    /// [`InvalidSublayerAndOffsetCount`]: crate::InvalidSublayerAndOffsetCount
    #[must_use]
    pub fn unpaired_sublayer_offsets(&self) -> Option<usize> {
        self.unpaired_sublayer_offsets
    }

    /// Records how many `subLayerOffsets` a reader found for this layer's
    /// sublayers, when that is not their number (see
    /// [`Layer::unpaired_sublayer_offsets`]); `None` when they pair.
    pub fn set_unpaired_sublayer_offsets(&mut self, offsets: Option<usize>) -> &mut Self {
        self.touch_structure();
        self.unpaired_sublayer_offsets = offsets;
        self
    }

    /// Returns the authored `defaultPrim` token: the prim that references
    /// and payloads with no authored prim path target in this layer.
    ///
    /// The token is kept as authored, so a value that names no prim path is
    /// still visible; [`Layer::default_prim_path`] converts it.
    ///
    /// Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim`), §10.3.2.1 (references).
    /// OpenUSD: `SdfLayer::GetDefaultPrim`.
    #[must_use]
    pub fn default_prim(&self) -> Option<TokenId> {
        self.default_prim
    }

    /// Authors (`Some`) or clears (`None`) the `defaultPrim` token.
    ///
    /// Records a structural change of the whole layer: arcs into this layer
    /// may target a different prim.
    ///
    /// Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim`). OpenUSD:
    /// `SdfLayer::SetDefaultPrim` and `ClearDefaultPrim`.
    pub fn set_default_prim(&mut self, default_prim: Option<TokenId>) {
        self.touch_structure();
        self.default_prim = default_prim;
    }

    /// Returns every other authored layer metadata field, in authored
    /// order: for example `upAxis`, `metersPerUnit`, `timeCodesPerSecond`,
    /// `framesPerSecond`, `startTimeCode`, `endTimeCode`,
    /// `customLayerData`, `documentation` and `comment`.
    /// [`Layer::metadata`] looks one field up.
    ///
    /// Spec: AOUSD Core §7.6.1 (layer spec fields), §12.2.7 (layer metadata
    /// is read from the root layer, not composed).
    #[must_use]
    pub fn metadata_fields(&self) -> &[FieldEntry] {
        &self.metadata
    }

    /// Returns the layer metadata fields for editing.
    ///
    /// Records a structural change of the whole layer, as
    /// [`Layer::set_metadata`] does.
    pub fn metadata_fields_mut(&mut self) -> &mut Vec<FieldEntry> {
        self.touch_structure();
        &mut self.metadata
    }

    /// Returns the authored `layerRelocates` entries, in authored order.
    ///
    /// Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`), §10.3.2.6
    /// (relocates are computed per layer stack from every layer's entries).
    /// OpenUSD: `SdfLayer::GetRelocates`.
    /// Empty when the layer authors no `layerRelocates` field, or an
    /// empty one ([`Layer::has_relocates`] tells them apart).
    #[must_use]
    pub fn relocates(&self) -> &[Relocate] {
        self.relocates.as_deref().unwrap_or_default()
    }

    /// Whether the layer authors a `layerRelocates` field, even an empty
    /// one.
    ///
    /// Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`). OpenUSD:
    /// `SdfLayer::HasRelocates`.
    #[must_use]
    pub fn has_relocates(&self) -> bool {
        self.relocates.is_some()
    }

    /// Returns the `layerRelocates` entries for editing, authoring the
    /// field (empty) when the layer has none.
    ///
    /// Records a structural change of the whole layer: relocates move
    /// namespace across the layer stack.
    pub fn relocates_mut(&mut self) -> &mut Vec<Relocate> {
        self.touch_structure();
        self.relocates.get_or_insert_with(Vec::new)
    }

    /// Removes the `layerRelocates` field. OpenUSD:
    /// `SdfLayer::ClearRelocates`.
    ///
    /// Records a structural change of the whole layer.
    pub fn clear_relocates(&mut self) {
        self.touch_structure();
        self.relocates = None;
    }

    /// Returns the prim specs keyed by prim path.
    ///
    /// Each path holds one spec here: the spec authored outside any variant
    /// branch if there is one, otherwise the first spec ingested for it.
    /// Specs authored for the same path inside other variant branches are
    /// in [`Layer::variant_prims`]; [`Layer::prim_specs`] visits both.
    #[must_use]
    pub fn prims(&self) -> &LayerPrims {
        &self.prims
    }

    /// Returns the [`Layer::prims`] spec at `path`, if this layer authors
    /// one.
    ///
    /// OpenUSD: `SdfLayer::GetPrimAtPath` for a path without variant
    /// selections.
    #[must_use]
    pub fn prim(&self, path: PathId) -> Option<&PrimSpec> {
        self.prims.get(&path)
    }

    /// Returns the [`Layer::prims`] spec at `path` for editing, if this
    /// layer authors one.
    ///
    /// Records a structural change confined to `path` when the spec exists:
    /// the caller may change its children, arcs or variant sets as well as
    /// its opinions.
    pub fn prim_mut(&mut self, path: PathId) -> Option<&mut PrimSpec> {
        if self.prims.contains_key(&path) {
            self.record_change(true, Some(alloc::vec![path]));
        }
        self.prims.get_mut(&path)
    }

    /// Removes the [`Layer::prims`] spec at `path` and returns it; specs of
    /// other variant branches at `path` ([`Layer::variant_prims`]) stay.
    ///
    /// Records a structural change confined to `path` when a spec was
    /// removed. This removes one spec only: its parent's child list and
    /// the specs beneath it are left as they are.
    pub fn remove_prim(&mut self, path: PathId) -> Option<PrimSpec> {
        let removed = self.prims.remove(&path)?;
        self.record_change(true, Some(alloc::vec![path]));
        Some(removed)
    }

    /// Returns the prim specs for bulk editing, as a reader building a
    /// layer does.
    ///
    /// Records a structural change of the whole layer, which composes a
    /// live stage drawing on it anew; [`Layer::prim_mut`],
    /// [`Layer::insert_prim`] and [`Layer::remove_prim`] record changes
    /// confined to one path.
    pub fn prims_mut(&mut self) -> &mut LayerPrims {
        self.touch_structure();
        &mut self.prims
    }

    /// Returns the further prim specs at paths already in [`Layer::prims`],
    /// each authored inside a different variant branch and identified by
    /// its [`PrimSpec::outer_variant_sites`].
    ///
    /// Branches of one variant set may author the same descendant
    /// (`/Model{lod=high}Geom` and `/Model{lod=low}Geom`); each branch keeps
    /// its own spec so composition can use the selected one.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs contain their own prim specs),
    /// §10.5 (only the selected variant contributes).
    #[must_use]
    pub fn variant_prims(&self) -> VariantPrims<'_> {
        VariantPrims(&self.variant_prims)
    }

    /// Every prim spec of this layer, those inside variant branches
    /// included, in namespace order: by path ([`PathInterner::cmp_paths`],
    /// parents before children), the spec outside any branch first at each
    /// path, then branch specs by their enclosing selections. The order
    /// depends only on the layer's content, not on how it was built. Each
    /// spec's properties keep their authored order
    /// ([`PrimSpec::properties`]); sort them by name for an order that does
    /// not depend on authoring either.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs contain their own prim
    /// specs), §8 (namespace).
    #[must_use]
    pub fn prim_specs_in_order(
        &self,
        paths: &PathInterner,
        tokens: &TokenInterner,
    ) -> Vec<(PathId, &PrimSpec)> {
        let mut specs: Vec<(PathId, &PrimSpec)> = self
            .prims
            .iter()
            .map(|(path, spec)| (*path, spec))
            .chain(
                self.variant_prims
                    .iter()
                    .flat_map(|(path, specs)| specs.iter().map(move |spec| (*path, spec))),
            )
            .collect();
        let site = |spec: &PrimSpec| {
            spec.outer_variant_sites()
                .iter()
                .map(|s| {
                    (
                        paths.resolve(s.host_path).depth(),
                        tokens.resolve(s.set),
                        tokens.resolve(s.variant),
                    )
                })
                .collect::<Vec<_>>()
        };
        specs.sort_by(|(a, x), (b, y)| {
            paths
                .cmp_paths(*a, *b, tokens)
                .then_with(|| site(x).cmp(&site(y)))
        });
        specs
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
    /// Every mutating method records its change, so no edit can bypass the
    /// counter: the `_mut` accessors record theirs before handing out the
    /// reference. Changes confined to known prim paths
    /// ([`Layer::prim_mut`], [`Layer::insert_prim`], [`Layer::property_mut`]
    /// and the like) keep those paths for independently synchronized live
    /// stages; the default budget retains up to 64 such batches and 1 MiB.
    /// Unscoped changes ([`Layer::prims_mut`], [`Layer::touch`]) and expired
    /// history conservatively invalidate the stage.
    ///
    /// OpenUSD reports the same edits as `SdfNotice::LayersDidChange`.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Changes this layer's authored-evidence retention budget and evicts old
    /// evidence immediately. A stage behind the retained history recovers through
    /// conservative recomposition when synchronized; layer content is unchanged.
    pub fn set_change_history_budget(&mut self, budget: crate::ChangeHistoryBudget) {
        self.change_history.set_budget(budget);
    }
    /// Forgets the authored evidence retained so far, as a reader does once
    /// it has built a layer from a file: its edits built the layer before
    /// any stage could see it, so no stage needs them. A stage that did see
    /// an older generation of the layer recovers through conservative
    /// recomposition, as after any history expiry; layer content and
    /// generations are unchanged.
    pub fn forget_change_history(&mut self) {
        self.change_history.clear();
    }
    /// Current authored-evidence budget.
    pub fn change_history_budget(&self) -> crate::ChangeHistoryBudget {
        self.change_history.budget()
    }
    /// Authored-evidence memory and cumulative retention work.
    pub fn change_history_stats(&self) -> crate::ChangeHistoryStats {
        self.change_history.stats()
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
    /// forward, recording an edit of unknown kind.
    ///
    /// The layer's mutating methods record their own changes; this is for
    /// a host that wants every stage drawing on the layer to compose it
    /// anew, as if all of it had changed.
    pub fn touch(&mut self) {
        self.touch_structure();
    }

    /// Moves both counters forward, for an edit that may change namespace
    /// or arcs.
    pub(crate) fn touch_structure(&mut self) {
        self.record_change(true, None);
    }

    /// Records an atomic authored batch. Unknown edits invalidate history precision.
    pub(crate) fn record_change(&mut self, structural: bool, paths: Option<Vec<PathId>>) {
        self.record_scoped_change(structural, paths, Vec::new());
    }

    /// Records an atomic authored batch whose changed prims `scope_ignored`
    /// (among `paths`) only support existing children.
    pub(crate) fn record_scoped_change(
        &mut self,
        structural: bool,
        paths: Option<Vec<PathId>>,
        scope_ignored: Vec<PathId>,
    ) {
        self.generation += 1;
        self.structure += u64::from(structural);
        self.change_history.record(LayerChange {
            generation: self.generation,
            paths,
            scope_ignored,
        });
    }

    /// Complete authored prim evidence, or None after unknown writes/history expiry.
    pub(crate) fn changed_paths_since(&self, generation: u64) -> Option<Vec<PathId>> {
        self.changed_paths(generation, false)
    }

    pub(crate) fn changed_scopes_since(&self, generation: u64) -> Option<Vec<PathId>> {
        self.changed_paths(generation, true)
    }

    /// The changed prims since `generation` whose specs only support
    /// existing children, left out of [`Self::changed_scopes_since`]: their
    /// own prim indexes change, the namespace beneath them does not. None
    /// after unknown writes or history expiry.
    pub(crate) fn changed_supports_since(&self, generation: u64) -> Option<Vec<PathId>> {
        self.changed_paths(generation, true)?;
        let mut paths: Vec<PathId> = self
            .change_history
            .iter()
            .filter(|c| c.generation > generation)
            .flat_map(|c| c.scope_ignored.iter().copied())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        Some(paths)
    }

    fn changed_paths(&self, generation: u64, scoped: bool) -> Option<Vec<PathId>> {
        if generation == self.generation {
            return Some(Vec::new());
        }
        if generation > self.generation || self.change_history.front()?.generation > generation + 1
        {
            return None;
        }
        let mut paths = Vec::new();
        for change in self
            .change_history
            .iter()
            .filter(|c| c.generation > generation)
        {
            paths.extend(
                change
                    .paths
                    .as_ref()?
                    .iter()
                    .copied()
                    .filter(|p| !scoped || !change.scope_ignored.contains(p)),
            );
        }
        paths.sort_unstable();
        paths.dedup();
        Some(paths)
    }

    /// Inserts a prim spec at the given path, replacing the spec authored in
    /// the same variant branch context ([`PrimSpec::outer_variant_sites`]).
    ///
    /// A spec from a different branch context is kept next to the existing
    /// one (see [`Layer::variant_prims`]) instead of replacing it; a spec
    /// authored outside any variant branch takes the [`Layer::prims`] slot.
    pub fn insert_prim(&mut self, path: PathId, spec: PrimSpec) {
        self.record_change(true, Some(alloc::vec![path]));
        self.place_prim(path, Box::new(spec));
    }

    /// Inserts each of `specs` as [`Layer::insert_prim`] does, as one
    /// change: the layer's generation advances once, recording every path.
    /// A format reading a whole layer inserts its specs this way, so the
    /// prim map grows once, to the number of specs, rather than as they
    /// arrive, and the layer's history holds one change for the read. A
    /// boxed spec stays where it is.
    pub fn insert_prims<S: Into<Box<PrimSpec>>>(
        &mut self,
        specs: impl IntoIterator<Item = (PathId, S)>,
    ) {
        let specs = specs.into_iter();
        // Grown once, by as many specs as may come: a reader filtering
        // its specs knows only that many.
        let (fewest, most) = specs.size_hint();
        let most = most.unwrap_or(fewest);
        self.prims.reserve(most);
        let mut paths = Vec::with_capacity(most);
        for (path, spec) in specs {
            paths.push(path);
            self.place_prim(path, spec.into());
        }
        if !paths.is_empty() {
            self.record_change(true, Some(paths));
        }
    }

    /// Places `spec` at `path` as [`Layer::insert_prim`] does, recording no
    /// change.
    fn place_prim(&mut self, path: PathId, spec: Box<PrimSpec>) {
        // One lookup: a read places every spec of a layer this way.
        let mut slot = match self.prims.entry(path) {
            hashbrown::hash_map::Entry::Vacant(slot) => {
                slot.insert(spec);
                return;
            }
            hashbrown::hash_map::Entry::Occupied(slot) => slot,
        };
        if slot.get().outer_variant_sites() == spec.outer_variant_sites() {
            slot.insert(spec);
        } else if !spec.outer_variant_sites().is_empty() {
            insert_branch_spec(self.variant_prims.entry(path).or_default(), *spec);
        } else {
            let displaced = slot.insert(spec);
            insert_branch_spec(self.variant_prims.entry(path).or_default(), *displaced);
        }
    }

    /// Inserts `spec` at the prim path `path` as [`Layer::insert_prim`] does,
    /// and lists its name after its parent's children when they do not list
    /// it yet. A missing parent spec is created as an `over` (the
    /// pseudo-root's holding children alone) and listed in turn.
    ///
    /// A layer built outside a store, as a file format builds one, keeps the
    /// namespace its specs describe this way, so it can be written: a spec
    /// its parent does not list has no place in a USDA or USDC layer.
    /// Specs inside variant branches go through [`Layer::insert_prim`].
    ///
    /// OpenUSD: `SdfCreatePrimInLayer` and `SdfPrimSpec::New`. AOUSD Core
    /// §7 (prim specs and their children).
    pub fn define_prim(&mut self, path: PathId, spec: PrimSpec, paths: &PathInterner) {
        self.insert_prim(path, spec);
        let mut child = path;
        while let Some(parent) = paths.parent(child) {
            let Some(name) = paths.resolve(child).leaf() else {
                return;
            };
            let created = !self.prims.contains_key(&parent);
            if created {
                let specifier = paths.parent(parent).map(|_| Specifier::Over);
                self.insert_prim(
                    parent,
                    PrimSpec {
                        specifier,
                        ..PrimSpec::default()
                    },
                );
            }
            let parent_spec = self.prims.get_mut(&parent).expect("inserted above");
            if parent_spec.authored_children.contains(&name) {
                return;
            }
            parent_spec.authored_children.push(name);
            self.record_change(true, Some(alloc::vec![parent]));
            if !created {
                return;
            }
            child = parent;
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
    /// layer.set_default_prim(Some(tokens.intern("Model/Geo")));
    /// let path = layer.default_prim_path(&mut tokens).unwrap();
    /// assert_eq!(path.display(&tokens), "/Model/Geo");
    ///
    /// layer.set_default_prim(Some(tokens.intern("Model.attr")));
    /// assert!(layer.default_prim_path(&mut tokens).is_none());
    /// ```
    pub fn default_prim_path(&self, tokens: &TokenInterner) -> Option<Path> {
        let value = String::from(tokens.resolve(self.default_prim?));
        let segments: Vec<TokenId> = default_prim_names(&value)?
            .into_iter()
            .map(|name| tokens.intern(name))
            .collect();
        Some(Path::root().join(&segments))
    }

    /// Returns the paths of the prim specs whose deferred references or
    /// payloads ([`Reference::deferred`]) name one of `assets`, in their own
    /// lists or in their variant branches, sorted; `None` when some asset
    /// is named by no deferred arc, whose binding no prim path bounds, or
    /// may be one of the layer's sublayers by asset path
    /// ([`SublayerEntry::unresolved`]), whose binding changes the layer
    /// stacks holding the layer.
    fn deferred_arc_paths(&self, assets: &[&str]) -> Option<Vec<PathId>> {
        if self.known_without_reference_arcs()
            || self.sublayers.iter().any(|sublayer| {
                sublayer.is_unresolved()
                    && (sublayer.is_expression()
                        || sublayer
                            .asset
                            .as_deref()
                            .is_some_and(|asset| assets.contains(&asset)))
            })
        {
            return None;
        }
        // A dynamic payload asks for its asset with the arguments
        // composition gave it (`crate::dynamic_format`): an arc names the
        // assets of its path whatever their arguments.
        let assets: Vec<&str> = assets
            .iter()
            .map(|asset| crate::asset::FormatArgs::strip(asset))
            .collect();
        let mut named = alloc::vec![false; assets.len()];
        let mut names = |op: &ListOp<Reference>| {
            let mut any = false;
            for arc in op.items().filter(|arc| arc.deferred) {
                let Some(asset) = arc.asset.as_deref() else {
                    continue;
                };
                let asset = crate::asset::FormatArgs::strip(asset);
                for (at, wanted) in assets.iter().enumerate() {
                    if *wanted == asset {
                        named[at] = true;
                        any = true;
                    }
                }
            }
            any
        };
        let mut paths: Vec<PathId> = Vec::new();
        let specs = self.prims.iter().chain(
            self.variant_prims
                .iter()
                .flat_map(|(path, specs)| specs.iter().map(move |spec| (path, spec))),
        );
        for (path, spec) in specs {
            // Every list is visited, so each asset any of them names is
            // counted.
            let mut found = names(&spec.references);
            found |= names(&spec.payloads);
            for branch in spec.variant_branches() {
                found |= names(&branch.spec.references);
                found |= names(&branch.spec.payloads);
            }
            if found {
                paths.push(*path);
            }
        }
        if !named.iter().all(|named| *named) {
            return None;
        }
        paths.sort_unstable();
        paths.dedup();
        Some(paths)
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
            .find(|spec| spec.outer_variant_sites() == sites)
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
            .filter(move |spec| spec.outer_variant_sites().last() == Some(&branch))
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
        selections: &'a VariantSelections,
    ) -> impl Iterator<Item = &'a PrimSpec> {
        self.prim_specs(path).filter(move |spec| {
            spec.outer_variant_sites()
                .last()
                .is_some_and(|site| site.host_path == host)
                && spec
                    .outer_variant_sites()
                    .iter()
                    .filter(|site| site.host_path == host)
                    .all(|site| selections.get(site.set) == Some(site.variant))
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
        if !spec_path.has_variant_selections() {
            return self.prim_spec_in(lookup_path, &[]);
        }
        let depth = paths.resolve(lookup_path).depth();
        let sites: Vec<_> = spec_path
            .variant_selection_sites()
            .take_while(|site| paths.resolve(site.host_path).depth() < depth)
            .collect();
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
    /// variant-qualified `spec_path`, with its `variantSetNames`: those
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
    ) -> Option<(&VariantSets, &ListOp<TokenId>)> {
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
        self.record_change(
            !self.prims.contains_key(&property_path.prim_path()),
            Some(alloc::vec![property_path.prim_path()]),
        );
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
        self.record_change(false, Some(alloc::vec![property_path.prim_path()]));
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
        .find(|existing| existing.outer_variant_sites() == spec.outer_variant_sites())
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
    /// The host has not answered yet: the asset is unavailable, and may
    /// still arrive. Composition asks for it
    /// ([`crate::Stage::asset_requests`]) rather than treating it as
    /// missing.
    Unavailable,
}

/// [`Sync`] with the `std` feature, so that a stage can compose from
/// several threads reading one store ([`crate::StageOptions::threads`]);
/// every type without it. A [`LayerStore`] must be one.
#[cfg(feature = "std")]
pub trait MaybeSync: Sync {}
#[cfg(feature = "std")]
impl<T: Sync + ?Sized> MaybeSync for T {}

/// [`Sync`] with the `std` feature, so that a stage can compose from
/// several threads reading one store ([`crate::StageOptions::threads`]);
/// every type without it. A [`LayerStore`] must be one.
#[cfg(not(feature = "std"))]
pub trait MaybeSync {}
#[cfg(not(feature = "std"))]
impl<T: ?Sized> MaybeSync for T {}

/// A store for accessing layers and shared interners.
///
/// With the `std` feature a store is [`Sync`] ([`MaybeSync`]): composing
/// with [`crate::StageOptions::threads`] above one reads it from several
/// threads at once, and interns into its [`TokenInterner`] and
/// [`PathInterner`] through shared references.
pub trait LayerStore: MaybeSync {
    /// Retains affinity of the shared token/path domains. Moving this store or
    /// adding names preserves it; replacing either interner invalidates it.
    /// Adapters exposing these same interners share affinity. No layer content
    /// or generation is represented by this identity.
    fn identity(&self) -> crate::StoreIdentity {
        crate::StoreIdentity {
            tokens: self.tokens().affinity.clone(),
            paths: self.paths().affinity.clone(),
        }
    }

    /// Returns a layer, if present.
    fn layer(&self, id: LayerId) -> Option<&Layer>;

    /// Whether the layer `id` is still loading: the store will hold it,
    /// but [`Self::layer`] does not return it yet.
    ///
    /// Composition asks only about a layer [`Self::layer`] did not return.
    /// A layer that is neither held nor pending is missing: it composes as a
    /// layer with no opinions, and composition reports it
    /// ([`crate::CompositionError::MissingLayer`]) unless the stage mutes
    /// it. A prim whose index reads a pending layer,
    /// as a layer of the stage's layer stack, a sublayer or the target of
    /// a reference or payload, is not composed: the stage lists it, with
    /// the layers it waits for, in [`crate::Stage::pending_prims`], and
    /// composes none of its descendants. Compose again once they load.
    ///
    /// A store that loads layers while a composition reads it makes a
    /// layer readable through [`Self::layer`] before it stops reporting it
    /// pending.
    ///
    /// Pending is for a store that knows a layer's id before its content,
    /// such as one that streams layers it has already named. An asset
    /// whose id the host has not answered for yet is unavailable instead
    /// ([`AssetAvailability::Unavailable`]): composition composes the prims
    /// that need it without it and asks for it
    /// ([`crate::Stage::asset_requests`], naming each prim), as
    /// `layerstack_io`'s document does for a source its storage cannot
    /// supply yet.
    ///
    /// The default reports no layer pending.
    fn is_layer_pending(&self, id: LayerId) -> bool {
        let _ = id;
        false
    }

    /// Returns a layer mutably, if present, for authoring.
    fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer>;

    /// Returns the shared token interner.
    fn tokens(&self) -> &TokenInterner;

    /// Returns the shared path interner.
    fn paths(&self) -> &PathInterner;

    /// The host's identifier of the layer `id` (`./set.usda`,
    /// `props.usdz[geo.usdc]`), if it has one: what diagnostics print for
    /// the layer ([`crate::CompositionError::display`],
    /// [`crate::EditError::display`]) instead of its id, written
    /// `@identifier@` as OpenUSD writes layers in its errors. An id derived
    /// from an identifier ([`LayerId::FIRST_DERIVED`]) is a hash that says
    /// nothing to a reader.
    ///
    /// Composition does not read it. The default names no layer, and
    /// diagnostics print `layer <id>`.
    fn layer_identifier(&self, id: LayerId) -> Option<&str> {
        let _ = id;
        None
    }

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
            .map_or(AssetAvailability::Unavailable, AssetAvailability::Loaded)
    }
}

/// A simple in-memory [`LayerStore`] implementation.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    /// Shared token interner for all layers in the store ([`Self::tokens`]).
    pub(crate) tokens: TokenInterner,
    /// Shared path interner for all layers in the store ([`Self::paths`]).
    pub(crate) paths: PathInterner,
    /// Layers by id ([`Self::layer`], [`Self::layers`]): looked up on
    /// every composition and query path, so kept hashed, and listed in id
    /// order on demand.
    pub(crate) layers: HashMap<LayerId, Layer>,
    /// The layers asset paths resolve to, by the layer each path is
    /// anchored to, then by path, for [`LayerStore::asset_layer`].
    /// [`LayerId::UNRESOLVED`] records a confirmed missing asset.
    pub(crate) asset_layers: HashMap<LayerId, HashMap<Arc<str>, LayerId>>,
    /// The identifiers hosts named layers by
    /// ([`Self::set_layer_identifier`], [`LayerStore::layer_identifier`]).
    pub(crate) identifiers: HashMap<LayerId, Arc<str>>,
}

impl InMemoryStore {
    /// The token interner every layer of the store shares. Interning
    /// takes `&self`; the interner itself cannot be replaced, which would
    /// invalidate the store's [`LayerStore::identity`] and every id its
    /// layers and stages hold.
    #[inline]
    #[must_use]
    pub fn tokens(&self) -> &TokenInterner {
        &self.tokens
    }

    /// The path interner every layer of the store shares; see
    /// [`Self::tokens`].
    #[inline]
    #[must_use]
    pub fn paths(&self) -> &PathInterner {
        &self.paths
    }

    /// Copies authored layers, bindings and interners into an independent domain.
    ///
    /// Existing token/path IDs retain their meanings in the snapshot. Future
    /// interning is independent: newly created IDs must not be mixed between the
    /// two stores. Their [`LayerStore::identity`] values differ so retained queries
    /// cannot reuse evidence from a divergent domain. Immutable numeric buffers
    /// and strings share ownership; layer maps, bindings and interner indexes copy.
    /// Mutating the snapshot does not publish edits to this store.
    #[must_use]
    pub fn snapshot(&self) -> Self {
        Self {
            tokens: self.tokens.snapshot(),
            paths: self.paths.snapshot(),
            layers: self.layers.clone(),
            asset_layers: self.asset_layers.clone(),
            identifiers: self.identifiers.clone(),
        }
    }

    /// Names the layer `layer` by `identifier`, the host's name for it
    /// (`./set.usda`, `props.usdz[geo.usdc]`), which diagnostics print in
    /// place of its id ([`LayerStore::layer_identifier`]). Composition does
    /// not read it, and the layer need not be held yet; a second call
    /// renames it.
    pub fn set_layer_identifier(&mut self, layer: LayerId, identifier: impl Into<Arc<str>>) {
        self.identifiers.insert(layer, identifier.into());
    }
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
            layer.change_history.clear();
            layer.structure = layer
                .structure
                .max(previous.structure)
                .checked_add(1)
                .expect("layer structural generation exhausted");
        }
        self.layers.insert(layer.id, layer);
    }

    /// The layer `id`, if the store holds it.
    #[must_use]
    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.get(&id)
    }

    /// The layer `id` for editing, if the store holds it. Edits through
    /// it are the host's: tell a live stage about them
    /// ([`crate::LiveStage::notify_layer_edit`] and its siblings).
    #[must_use]
    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        self.layers.get_mut(&id)
    }

    /// Whether the store holds the layer `id`.
    #[must_use]
    pub fn contains_layer(&self, id: LayerId) -> bool {
        self.layers.contains_key(&id)
    }

    /// The number of layers the store holds.
    #[must_use]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// The ids of the layers the store holds, in id order.
    #[must_use]
    pub fn layer_ids(&self) -> Vec<LayerId> {
        let mut ids: Vec<LayerId> = self.layers.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// The layers the store holds, in id order.
    pub fn layers(&self) -> impl Iterator<Item = &Layer> + '_ {
        self.layer_ids()
            .into_iter()
            .filter_map(move |id| self.layers.get(&id))
    }

    /// Removes the layer `id`, returning it. Bindings that name it stay.
    pub fn remove_layer(&mut self, id: LayerId) -> Option<Layer> {
        self.layers.remove(&id)
    }

    /// Every recorded asset binding, `(anchor, asset path, layer)`, sorted
    /// by anchor and path; [`LayerId::UNRESOLVED`] marks a confirmed missing
    /// asset.
    #[must_use]
    pub fn asset_bindings(&self) -> Vec<(LayerId, Arc<str>, LayerId)> {
        let mut bindings: Vec<_> = self
            .asset_layers
            .iter()
            .flat_map(|(anchor, assets)| {
                assets
                    .iter()
                    .map(|(path, layer)| (*anchor, path.clone(), *layer))
            })
            .collect();
        bindings.sort_unstable();
        bindings
    }

    /// Forgets every asset recorded missing, so composition asks for each
    /// again (as `PcpCache::Reload` retries invalid assets). Bindings to
    /// loaded layers stay.
    pub fn forget_missing_assets(&mut self) {
        for assets in self.asset_layers.values_mut() {
            assets.retain(|_, layer| *layer != LayerId::UNRESOLVED);
        }
    }

    /// Records that `asset_path`, anchored to the layer `anchor`, resolves
    /// to the layer `layer` (see [`LayerStore::asset_layer`]).
    ///
    /// A changed binding is a structural change of `anchor`. When deferred
    /// arcs ([`Reference::deferred`]) name the asset, it is recorded at the
    /// prims that author them, so a live stage recomposes only what those
    /// arcs reach; otherwise at no known prim, which composes the stage
    /// anew.
    pub fn insert_asset_layer(&mut self, anchor: LayerId, asset_path: &str, layer: LayerId) {
        self.insert_asset_layers(anchor, [(asset_path, layer)]);
    }

    /// Records the layers several asset paths, anchored to the layer
    /// `anchor`, resolve to, as [`Self::insert_asset_layer`] does for each,
    /// recording one change of `anchor` for all of them.
    /// [`LayerId::UNRESOLVED`] records a confirmed missing asset.
    ///
    /// A path is taken as an [`Arc<str>`] or anything that converts into
    /// one, such as a `&str`, so a host that holds its paths shared stores
    /// them without copying.
    ///
    /// Binding the paths of an anchor the store does not hold yet records
    /// no change: no stage has seen the layer, so it changes nothing a
    /// stage composed. A host loading a layer and the bindings of its arcs
    /// together binds them before [`Self::insert_layer`], as an importer
    /// does.
    pub fn insert_asset_layers<A: Into<Arc<str>>>(
        &mut self,
        anchor: LayerId,
        bindings: impl IntoIterator<Item = (A, LayerId)>,
    ) {
        let assets = self.asset_layers.entry(anchor).or_default();
        let source = self.layers.get_mut(&anchor);
        let mut changed: Vec<Arc<str>> = Vec::new();
        for (asset_path, layer) in bindings {
            let asset_path: Arc<str> = asset_path.into();
            if source.is_none() {
                assets.insert(asset_path, layer);
            } else if assets.insert(Arc::clone(&asset_path), layer) != Some(layer) {
                changed.push(asset_path);
            }
        }
        if !changed.is_empty()
            && let Some(source) = source
        {
            let changed: Vec<&str> = changed.iter().map(|asset| &**asset).collect();
            let paths = source.deferred_arc_paths(&changed);
            source.record_change(true, paths);
        }
    }

    /// Reserves room for at least `additional` more layers, so inserting
    /// the layers of a large load does not grow the store's table again
    /// and again.
    pub fn reserve_layers(&mut self, additional: usize) {
        self.layers.reserve(additional);
    }

    /// Records a host-confirmed missing asset, invalidating its anchor layer.
    ///
    /// This lets value-clip templates distinguish a filename hole from a
    /// candidate that has not yet been resolved. A later [`Self::insert_asset_layer`]
    /// replaces this result and invalidates the same anchor.
    pub fn mark_asset_missing(&mut self, anchor: LayerId, asset_path: &str) {
        self.insert_asset_layer(anchor, asset_path, LayerId::UNRESOLVED);
    }

    /// Parses and interns an absolute prim path such as `/World/Rock`,
    /// returning its [`PathId`]. Interns nothing when `s` is rejected.
    ///
    /// Interning takes `&self`, so this works while the store is borrowed
    /// elsewhere, for example by a [`crate::StageView`].
    ///
    /// ```
    /// use layerstack::{InMemoryStore, PathError};
    ///
    /// let store = InMemoryStore::default();
    /// let rock = store.try_path("/World/Rock").unwrap();
    /// assert_eq!(store.paths().display(rock, store.tokens()), "/World/Rock");
    /// assert_eq!(store.try_path("World/Rock"), Err(PathError::NotAbsolute));
    /// assert_eq!(store.try_path("/.radius"), Err(PathError::InvalidPrimName));
    /// ```
    ///
    /// # Errors
    ///
    /// [`PathError`] when `s` is not absolute, has an empty segment, or has
    /// a segment that is not a prim name (an identifier).
    ///
    /// Spec: AOUSD Core §7.3.3 (`PrimName`), §8.5 (path grammar).
    pub fn try_path(&self, s: &str) -> Result<PathId, PathError> {
        crate::path::check_prim_names(s)?;
        Ok(self.paths.intern(Path::parse_absolute(s, &self.tokens)?))
    }

    /// Parses and interns an absolute prim path, as [`Self::try_path`]
    /// does: a shorthand for tests and examples.
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a prim path such as `/World/Rock`.
    #[track_caller]
    pub fn path(&self, s: &str) -> PathId {
        self.try_path(s)
            .unwrap_or_else(|error| panic!("`{s}` is not a prim path: {error}"))
    }

    /// Parses a concrete property path such as `/World/Rock.radius`,
    /// interning its prim path and property name. Interns nothing when `s`
    /// is rejected.
    ///
    /// # Errors
    ///
    /// [`PropertyPathError`] when `s` is not a prim path followed by `.`
    /// and a property name: identifiers joined by `:`.
    ///
    /// Spec: AOUSD Core §7.3.3 (`PropertyName`), §8.5 (path grammar).
    pub fn try_property_path(&self, s: &str) -> Result<PropertyPath, PropertyPathError> {
        let (prim, property) = PropertyPath::split(s)?;
        crate::path::check_prim_names(prim)?;
        if !crate::path::is_property_name(property) {
            return Err(PropertyPathError::InvalidPropertyName);
        }
        PropertyPath::parse(s, &self.tokens, &self.paths)
    }

    /// Parses a concrete property path, as [`Self::try_property_path`]
    /// does: a shorthand for tests and examples.
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a property path such as `/World/Rock.radius`.
    #[track_caller]
    pub fn property_path(&self, s: &str) -> PropertyPath {
        self.try_property_path(s)
            .unwrap_or_else(|error| panic!("`{s}` is not a property path: {error}"))
    }

    /// Parses a concrete relationship or connection target path: a prim
    /// path ([`Self::try_path`]) or, with a `.`, a property path
    /// ([`Self::try_property_path`]). Interns nothing when `s` is rejected.
    ///
    /// # Errors
    ///
    /// [`TargetPathError`] with the prim or property path's error.
    pub fn try_target_path(&self, s: &str) -> Result<TargetPath, TargetPathError> {
        if s.contains('.') {
            self.try_property_path(s)
                .map(TargetPath::Property)
                .map_err(TargetPathError::Property)
        } else {
            self.try_path(s)
                .map(TargetPath::Prim)
                .map_err(TargetPathError::Prim)
        }
    }

    /// Parses a concrete target path, as [`Self::try_target_path`] does: a
    /// shorthand for tests and examples.
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a target path such as `/Prim` or
    /// `/Prim.attrName`.
    #[track_caller]
    pub fn target_path(&self, s: &str) -> TargetPath {
        self.try_target_path(s)
            .unwrap_or_else(|error| panic!("`{s}` is not a target path: {error}"))
    }

    /// Parses a spec path, which may select variants, such as
    /// `/World/Fern{season=autumn}.color`: the store's shorthand for
    /// [`SpecPath::parse`]. Interns nothing when `s` is rejected.
    ///
    /// # Errors
    ///
    /// [`SpecPathError`] when `s` is malformed, a prim or variant set name
    /// is not an identifier, the property is on the pseudo-root, or the
    /// property name is not identifiers joined by `:`.
    ///
    /// Spec: AOUSD Core §7.3.3 (names), §8.5 (path grammar).
    pub fn try_spec_path(&self, s: &str) -> Result<SpecPath, SpecPathError> {
        crate::spec_path::check_names(s)?;
        SpecPath::parse(s, &self.tokens, &self.paths)
    }

    /// Parses a spec path, as [`Self::try_spec_path`] does: a shorthand for
    /// tests and examples.
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a spec path such as `/World/Fern{season=autumn}`.
    #[track_caller]
    pub fn spec_path(&self, s: &str) -> SpecPath {
        self.try_spec_path(s)
            .unwrap_or_else(|error| panic!("`{s}` is not a spec path: {error}"))
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

    fn paths(&self) -> &PathInterner {
        &self.paths
    }

    fn layer_identifier(&self, id: LayerId) -> Option<&str> {
        self.identifiers.get(&id).map(|identifier| &**identifier)
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
            None => AssetAvailability::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;

    /// A set has one selection; a new one replaces it, and iteration
    /// visits each set once.
    #[test]
    fn variant_selections_hold_one_variant_per_set() {
        let store = InMemoryStore::default();
        let [shape, size, round, square, big] =
            ["shape", "size", "round", "square", "big"].map(|name| store.tokens.intern(name));
        let mut selections: VariantSelections = [(shape, round)].into_iter().collect();
        assert_eq!(selections.insert(shape, square), Some(round));
        selections.extend([(size, big)]);
        assert_eq!((selections.get(shape), selections.len()), (Some(square), 2));
        assert!(selections.contains(size) && !selections.contains(big));
        let mut all: Vec<_> = selections.iter().collect();
        all.sort();
        let mut expected = vec![(shape, square), (size, big)];
        expected.sort();
        assert_eq!(all, expected);
        assert_eq!(selections.remove(size), Some(big));
        assert_eq!(selections.remove(size), None);
        selections.clear();
        assert!(selections.is_empty());
        assert_eq!(selections, VariantSelections::new());
    }

    /// Variants keep the order they were added, a replaced one in place,
    /// and compare equal by name whatever their order.
    #[test]
    fn variants_keep_their_order_and_compare_by_name() {
        let store = InMemoryStore::default();
        let [a, b, c] = ["a", "b", "c"].map(|name| store.tokens.intern(name));
        let marked = |name: TokenId| VariantSpec {
            authored_children: vec![name],
            ..VariantSpec::default()
        };
        let mut variants = Variants::from([(c, VariantSpec::default()), (a, marked(a))]);
        variants.insert(b, VariantSpec::default());
        assert_eq!(variants.insert(c, marked(c)), Some(VariantSpec::default()));
        assert_eq!(variants.keys().copied().collect::<Vec<_>>(), [c, a, b]);
        assert_eq!(variants[&c], marked(c));
        variants.insert_at(0, store.tokens.intern("d"), VariantSpec::default());
        assert_eq!(
            variants.remove(&store.tokens.intern("d")),
            Some(VariantSpec::default())
        );
        let reordered: Variants = [(b, VariantSpec::default()), (a, marked(a)), (c, marked(c))]
            .into_iter()
            .collect();
        assert_eq!(variants, reordered);
        assert_ne!(variants, Variants::from([(a, marked(a)), (c, marked(c))]));
        assert_ne!(
            variants,
            Variants::from([(a, marked(a)), (b, marked(b)), (c, marked(c))])
        );
    }

    /// A prim spec authoring no rare field allocates none, reads each as
    /// unauthored, and compares equal to one whose rare fields were
    /// allocated and emptied again.
    #[test]
    fn rare_prim_fields_read_as_unauthored_until_authored() {
        let store = InMemoryStore::default();
        let base = store.path("/Base");
        let (set, variant) = (store.tokens.intern("set"), store.tokens.intern("variant"));
        let plain = PrimSpec::def().with_outer_variant_sites(Vec::new());
        assert!(
            plain.rare().is_none(),
            "nothing authored, nothing allocated"
        );
        assert!(plain.inherits().is_empty() && plain.specializes().is_empty());
        assert!(plain.variant_sets().is_empty() && plain.variant_set_names().is_empty());
        assert_eq!(plain.variant_selections(), None);
        assert_eq!(plain.variant_selection(set), None);
        assert_eq!((plain.property_order(), plain.prim_order()), (None, None));
        assert!(plain.outer_variant_sites().is_empty());

        let mut authored = PrimSpec::def().with_inherit(base);
        authored.rare_mut().variant_selections.insert(set, variant);
        assert_eq!(authored.inherits().prepend, []);
        assert_eq!(authored.inherits().append, [base]);
        assert_eq!(authored.variant_selection(set), Some(variant));
        assert_ne!(authored, plain);

        let rare = authored.authored_rare_mut().expect("allocated");
        rare.inherits = SparseListOp::new();
        rare.variant_selections.clear();
        assert!(authored.rare().is_some_and(RarePrimFields::is_empty));
        assert_eq!(
            authored.variant_selections(),
            None,
            "an emptied map reads as none"
        );
        assert_eq!(authored, plain);
        assert_eq!(plain, authored);
    }

    /// Sets keep their order: a new one last, a replaced one in place, a
    /// removed one leaving the others in order; `insert_at` places one.
    #[test]
    fn variant_sets_keep_their_order() {
        let tokens = TokenInterner::default();
        let [a, b, c, d, unknown] = ["a", "b", "c", "d", "e"].map(|name| tokens.intern(name));
        let names = |sets: &VariantSets| sets.keys().copied().collect::<Vec<_>>();
        let mut sets: VariantSets = [c, a, b]
            .into_iter()
            .map(|name| (name, VariantSetSpec::default()))
            .collect();
        assert_eq!(names(&sets), [c, a, b]);
        let mut one = VariantSetSpec::default();
        one.variants.insert(d, VariantSpec::default());
        assert!(sets.insert(a, one.clone()).is_some());
        assert_eq!(names(&sets), [c, a, b], "replaced in place");
        assert_eq!(sets[&a], one);
        sets.get_or_insert_default(d);
        assert_eq!(names(&sets), [c, a, b, d]);
        assert!(sets.remove(&a).is_some());
        assert_eq!(names(&sets), [c, b, d]);
        sets.insert_at(1, a, VariantSetSpec::default());
        assert_eq!(names(&sets), [c, a, b, d]);
        assert_eq!(sets.index_of(&b), Some(2));
        assert_eq!(sets.index_of(&unknown), None);
        let reversed: VariantSets = sets.clone().into_iter().rev().collect();
        assert_ne!(reversed, sets, "the order is part of the value");
    }

    /// Each constructor makes the source it names, and a source makes the
    /// arc it was read from.
    #[test]
    fn reference_sources_name_one_layer_each() {
        let (scene, rock) = (LayerId(1), LayerId(2));
        let at = |offset| LayerOffset { offset, scale: 1.0 };
        let cases = [
            (
                Reference::to_default_prim(scene),
                ReferenceSource::Layer {
                    layer: scene,
                    asset: None,
                },
            ),
            (
                Reference::with_asset_default_prim(rock, "./rock.usda"),
                ReferenceSource::Layer {
                    layer: rock,
                    asset: Some("./rock.usda"),
                },
            ),
            (
                Reference::deferred(scene, "./plant.usda", ReferenceTarget::DefaultPrim, at(2.0)),
                ReferenceSource::Asset {
                    anchor: scene,
                    path: "./plant.usda",
                },
            ),
            (
                Reference::expression(
                    scene,
                    "`\"./${NAME}.usda\"`",
                    ReferenceTarget::DefaultPrim,
                    at(3.0),
                ),
                ReferenceSource::Expression {
                    anchor: scene,
                    text: "`\"./${NAME}.usda\"`",
                },
            ),
            (
                Reference::unresolved("./moss.usda", ReferenceTarget::DefaultPrim, at(4.0)),
                ReferenceSource::Unresolved {
                    path: "./moss.usda",
                },
            ),
        ];
        for (reference, source) in cases {
            assert_eq!(reference.source(), source);
            let rebuilt = Reference::from_source(source, reference.target.clone())
                .with_layer_offset(reference.layer_offset);
            assert_eq!(rebuilt, reference, "{source:?}");
            assert_eq!(
                reference.asset_path(),
                match source {
                    ReferenceSource::Layer { asset, .. } => asset,
                    ReferenceSource::Asset { path, .. } | ReferenceSource::Unresolved { path } => {
                        Some(path)
                    }
                    ReferenceSource::Expression { text, .. } => Some(text),
                }
            );
        }
        // Rewriting the source keeps the target, offset and custom data.
        let localized =
            Reference::deferred(scene, "./plant.usda", ReferenceTarget::DefaultPrim, at(2.0))
                .with_custom_data(vec![("note".into(), Value::Bool(true))])
                .with_source(ReferenceSource::Layer {
                    layer: rock,
                    asset: Some("plant.usda"),
                });
        assert_eq!(localized.layer_offset, at(2.0));
        assert_eq!(localized.custom_data.len(), 1);
        assert!(!localized.is_unresolved());
    }
    use alloc::format;
    use alloc::sync::Arc;
    use alloc::vec;

    fn entry(key: &str, val: Value) -> (Arc<str>, Value) {
        (Arc::from(key), val)
    }

    /// `Layer::insert_prims` places specs as `Layer::insert_prim` does each,
    /// a branch spec beside the namespace spec at its path, as one change
    /// naming every path.
    #[test]
    fn binding_assets_together_bounds_the_change_only_by_deferred_arcs() {
        let mut store = InMemoryStore::default();
        let a = store.path("/A");
        let mut layer = Layer::new(LayerId(1));
        let mut spec = PrimSpec::def();
        spec.references.make_mut().prepend.push(Reference::deferred(
            LayerId(1),
            "a.usda",
            ReferenceTarget::DefaultPrim,
            LayerOffset::IDENTITY,
        ));
        layer.insert_prim(a, spec);
        store.insert_layer(layer);
        let since = store.layers[&LayerId(1)].generation();
        // The deferred arc's asset alone: the change is at its prim.
        store.insert_asset_layers(LayerId(1), [("a.usda", LayerId(2))]);
        assert_eq!(
            store.layers[&LayerId(1)].changed_paths_since(since),
            Some(alloc::vec![a])
        );
        // With an asset no deferred arc names, nothing bounds the change,
        // as binding that asset alone would not.
        let since = store.layers[&LayerId(1)].generation();
        store.insert_asset_layers(LayerId(1), [("a.usda", LayerId(3)), ("b.usda", LayerId(4))]);
        assert_eq!(store.layers[&LayerId(1)].changed_paths_since(since), None);
    }

    /// Binding an asset the layer also sublayers by asset path changes its
    /// layer stack, so no prim path bounds the change, though a deferred
    /// arc names the same asset.
    #[test]
    fn binding_a_sublayer_asset_bounds_the_change_by_no_prim() {
        let mut store = InMemoryStore::default();
        let a = store.path("/A");
        let mut layer = Layer::new(LayerId(1));
        layer
            .sublayers
            .push(SublayerEntry::unresolved("a.usda", LayerOffset::IDENTITY));
        let mut spec = PrimSpec::def();
        spec.references.make_mut().prepend.push(Reference::deferred(
            LayerId(1),
            "a.usda",
            ReferenceTarget::DefaultPrim,
            LayerOffset::IDENTITY,
        ));
        layer.insert_prim(a, spec);
        store.insert_layer(layer);
        let since = store.layers[&LayerId(1)].generation();
        store.insert_asset_layers(LayerId(1), [("a.usda", LayerId(2))]);
        assert_eq!(store.layers[&LayerId(1)].changed_paths_since(since), None);
    }

    /// A layer's bindings recorded before the store holds it change
    /// nothing: the layer arrives with them at its own generation, its
    /// note that it authors no reference arc intact. Once the store holds
    /// it, binding another asset is a change of it again.
    #[test]
    fn binding_assets_before_their_anchor_arrives_records_no_change() {
        let mut store = InMemoryStore::default();
        let mut layer = Layer::new(LayerId(1));
        layer.sublayers.push(SublayerEntry::new(LayerId(2)));
        layer.note_no_reference_arcs();
        let generation = layer.generation();
        let shared: Arc<str> = Arc::from("sub.usda");
        store.insert_asset_layers(LayerId(1), [(Arc::clone(&shared), LayerId(2))]);
        store.insert_layer(layer);
        let arrived = &store.layers[&LayerId(1)];
        assert_eq!(arrived.generation(), generation);
        assert!(arrived.known_without_reference_arcs());
        assert_eq!(store.asset_layer(LayerId(1), "sub.usda"), Some(LayerId(2)));
        store.insert_asset_layers(LayerId(1), [("other.usda", LayerId(3))]);
        assert!(store.layers[&LayerId(1)].generation() > generation);
    }

    #[test]
    fn reference_arc_note_lasts_until_the_layer_changes() {
        let store = InMemoryStore::default();
        let a = store.path("/A");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(a, PrimSpec::def());
        assert!(!layer.known_without_reference_arcs(), "unknown until noted");
        layer.note_no_reference_arcs();
        assert!(layer.known_without_reference_arcs());
        // Renumbering keeps it; any edit ends it.
        layer.map_layer_ids(|id| LayerId(id.0 + 1));
        assert!(layer.known_without_reference_arcs());
        let mut renumbered = layer.clone();
        renumbered
            .prim_mut(a)
            .unwrap()
            .references
            .make_mut()
            .prepend
            .push(Reference::new(LayerId(9), a));
        assert!(!renumbered.known_without_reference_arcs());
        renumbered.map_layer_ids(|id| if id == LayerId(9) { LayerId(10) } else { id });
        assert_eq!(
            renumbered.prim(a).unwrap().references.prepend[0].layer,
            LayerId(10),
            "a layer not known to author none has its arcs renumbered"
        );
        layer.insert_prim(a, PrimSpec::def());
        assert!(
            !layer.known_without_reference_arcs(),
            "a replaced spec ends it"
        );
    }

    #[test]
    fn inserting_prims_together_places_them_as_one_change() {
        let store = InMemoryStore::default();
        let (a, b) = (store.path("/A"), store.path("/A/B"));
        let (set, variant) = (store.tokens.intern("look"), store.tokens.intern("red"));
        let mut branch = PrimSpec::over();
        branch.rare_mut().outer_variant_sites = vec![VariantSelectionSite {
            host_path: a,
            set,
            variant,
        }];
        let specs = || {
            [
                (a, PrimSpec::def()),
                (b, branch.clone()),
                (b, PrimSpec::def()),
            ]
        };
        let mut one_by_one = Layer::new(LayerId(1));
        for (path, spec) in specs() {
            one_by_one.insert_prim(path, spec);
        }
        let mut together = Layer::new(LayerId(1));
        together.insert_prims(specs());
        assert_eq!(together.prims, one_by_one.prims);
        assert_eq!(together.variant_prims, one_by_one.variant_prims);
        assert_eq!(together.generation(), 1);
        assert_eq!(together.changed_paths_since(0), Some(vec![a, b]));
        together.insert_prims::<PrimSpec>([]);
        assert_eq!(together.generation(), 1, "no specs, no change");
    }

    #[test]
    fn checked_path_helpers_reject_malformed_text() {
        let store = InMemoryStore::default();
        let paths_before = store.paths.len();
        for bad in ["World/Rock", "/.radius", ""] {
            assert!(store.try_path(bad).is_err(), "try_path({bad:?})");
            assert!(
                store.try_property_path(bad).is_err(),
                "try_property_path({bad:?})"
            );
            assert!(
                store.try_target_path(bad).is_err(),
                "try_target_path({bad:?})"
            );
            assert!(store.try_spec_path(bad).is_err(), "try_spec_path({bad:?})");
        }
        assert_eq!(store.try_path("World/Rock"), Err(PathError::NotAbsolute));
        assert_eq!(store.try_path("/.radius"), Err(PathError::InvalidPrimName));
        assert_eq!(store.try_path(""), Err(PathError::NotAbsolute));
        assert_eq!(
            store.try_property_path("/.radius"),
            Err(PropertyPathError::RootPrimNotAllowed)
        );
        assert_eq!(
            store.try_property_path("World/Rock"),
            Err(PropertyPathError::MissingProperty)
        );
        assert_eq!(
            store.try_property_path("/World/Ro ck.radius"),
            Err(PropertyPathError::InvalidPrimPath)
        );
        assert_eq!(
            store.try_property_path("/World/Rock.ra dius"),
            Err(PropertyPathError::InvalidPropertyName)
        );
        assert_eq!(
            store.try_target_path("/.radius"),
            Err(TargetPathError::Property(
                PropertyPathError::RootPrimNotAllowed
            ))
        );
        assert_eq!(
            store.try_target_path(""),
            Err(TargetPathError::Prim(PathError::NotAbsolute))
        );
        assert_eq!(
            store.try_spec_path("/.radius"),
            Err(SpecPathError::RootPrimNotAllowed)
        );
        assert_eq!(store.try_spec_path(""), Err(SpecPathError::NotAbsolute));
        assert_eq!(
            store.try_spec_path("/World/Fern{1season=autumn}"),
            Err(SpecPathError::MalformedVariantSelection)
        );
        assert_eq!(
            store.try_spec_path("/World//Fern"),
            Err(SpecPathError::EmptySegment)
        );
        assert_eq!(
            store.try_path("/World/Rock{shape=jagged}"),
            Err(PathError::InvalidPrimName)
        );
        assert!(
            store.tokens.lookup("World/Rock").is_none() && store.tokens.lookup(".radius").is_none(),
            "a rejected path interns nothing"
        );
        assert_eq!(store.paths.len(), paths_before);
        assert_eq!(
            format!("{}", PathError::InvalidPrimName),
            "a segment of the path is not a prim name (an identifier)"
        );
    }

    #[test]
    fn checked_path_helpers_intern_through_a_shared_reference() {
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(LayerId(1)));
        let stage = crate::Stage::compose(&mut store, LayerId(1), crate::StageOptions::default());
        let shared: &InMemoryStore = &store;
        // A view borrows the store while paths are still interned.
        let view = stage.view(shared);
        let rock = shared.try_path("/World/Rock").unwrap();
        assert!(view.prim(rock).is_none());
        assert_eq!(shared.path("/World/Rock"), rock);
        let root = shared.paths.lookup(&Path::root()).unwrap();
        assert_eq!(shared.try_path("/"), Ok(root));
        let radius = shared.try_property_path("/World/Rock.geom:radius").unwrap();
        assert_eq!(
            (radius.prim_path(), shared.tokens.resolve(radius.property())),
            (rock, "geom:radius")
        );
        assert_eq!(shared.property_path("/World/Rock.geom:radius"), radius);
        assert_eq!(
            shared.try_target_path("/World/Rock"),
            Ok(TargetPath::Prim(rock))
        );
        assert_eq!(
            shared.try_target_path("/World/Rock.geom:radius"),
            Ok(TargetPath::Property(radius))
        );
        assert_eq!(shared.target_path("/World/Rock"), TargetPath::Prim(rock));
        let branch = shared
            .try_spec_path("/World/Fern{season=autumn}Leaf.color")
            .unwrap();
        assert_eq!(
            branch.display(&shared.paths, &shared.tokens),
            "/World/Fern{season=autumn}Leaf.color"
        );
        assert_eq!(
            shared.spec_path("/World/Fern{season=}"),
            shared.try_spec_path("/World/Fern{season=}").unwrap()
        );
        assert_eq!(
            shared.try_spec_path("/"),
            Ok(SpecPath::from_prim_path(root))
        );
    }

    #[test]
    fn defined_prims_are_listed_among_their_parents_children() {
        let store = InMemoryStore::default();
        let (abc, ad) = (store.path("/A/B/C"), store.path("/A/D"));
        let (root, a, b) = (store.path("/"), store.path("/A"), store.path("/A/B"));
        let name = |s: &str| store.tokens.intern(s);
        let mut layer = Layer::new(LayerId(1));
        layer.define_prim(abc, PrimSpec::def(), &store.paths);
        layer.define_prim(ad, PrimSpec::def(), &store.paths);
        layer.define_prim(ad, PrimSpec::def().with_active(false), &store.paths);
        assert_eq!(layer.prim(root).unwrap().authored_children, [name("A")]);
        assert_eq!(layer.prim(root).unwrap().specifier, None);
        let parent = layer.prim(a).unwrap();
        assert_eq!(parent.specifier, Some(Specifier::Over));
        assert_eq!(parent.authored_children, [name("B"), name("D")]);
        assert_eq!(layer.prim(b).unwrap().authored_children, [name("C")]);
        assert_eq!(layer.prim(abc).unwrap().specifier, Some(Specifier::Def));
        assert_eq!(layer.prim(ad).unwrap().active, Some(false));
    }

    #[test]
    fn layer_methods_move_the_generation_forward() {
        let store = InMemoryStore::default();
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
        let _ = layer.prim_mut(rock);
        step(&layer, "prim_mut", true);
        let _ = layer.prims_mut();
        step(&layer, "prims_mut", true);
        let _ = layer.sublayers_mut();
        step(&layer, "sublayers_mut", true);
        let _ = layer.relocates_mut();
        step(&layer, "relocates_mut", true);
        assert!(layer.has_relocates());
        layer.clear_relocates();
        step(&layer, "clear_relocates", true);
        assert!(!layer.has_relocates());
        let _ = layer.metadata_fields_mut();
        step(&layer, "metadata_fields_mut", true);
        layer.set_default_prim(Some(size));
        step(&layer, "set_default_prim", true);
        layer.map_layer_ids(|id| id);
        step(&layer, "map_layer_ids", true);
        assert!(layer.remove_prim(pebble).is_some());
        step(&layer, "remove_prim", true);
        let unchanged = layer.generation();
        assert!(layer.prim_mut(pebble).is_none());
        assert!(layer.remove_prim(pebble).is_none());
        assert_eq!(
            layer.generation(),
            unchanged,
            "a missing prim records nothing"
        );
        layer.touch();
        step(&layer, "touch", true);

        let mut same = pristine.clone();
        same.touch();
        assert_eq!(same, pristine, "equality compares content, not generations");
        assert_ne!(layer, pristine);
    }

    #[test]
    fn prim_edits_record_their_path() {
        let store = InMemoryStore::default();
        let rock = store.path("/Rock");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(rock, PrimSpec::def());
        let seen = layer.generation();
        let _ = layer.prim_mut(rock);
        assert_eq!(layer.changed_paths_since(seen), Some(vec![rock]));
        assert!(layer.remove_prim(rock).is_some());
        assert_eq!(layer.changed_paths_since(seen), Some(vec![rock]));
        let _ = layer.prims_mut();
        assert_eq!(
            layer.changed_paths_since(seen),
            None,
            "bulk edits are unscoped"
        );
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
        let tokens = TokenInterner::default();
        let mut layer = Layer::new(LayerId(1));
        assert_eq!(layer.default_prim_path(&tokens), None, "not authored");
        for (token, expected) in cases {
            layer.default_prim = Some(tokens.intern(token));
            let actual = layer
                .default_prim_path(&tokens)
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
        let p = paths.intern(Path::parse_absolute("/P", &tokens).unwrap());
        let variant = |value: i64, sets: Vec<(TokenId, TokenId, VariantSpec)>| {
            let mut spec = VariantSpec::default();
            spec.fields.push(FieldEntry {
                name: count,
                value: FieldValue::Value(Value::Int64(value)),
            });
            for (set, name, nested) in sets {
                if !spec.variant_set_names.prepend.contains(&set) {
                    spec.variant_set_names.make_mut().prepend.push(set);
                }
                spec.variant_sets
                    .get_or_insert_default(set)
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
        prim.rare_mut().variant_set_names = ListOp::prepended(vec![a]).into();
        let set = prim.rare_mut().variant_sets.get_or_insert_default(a);
        set.variants.insert(x, ax);
        set.variants.insert(y, ay);
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(p, prim);

        let count_at = |path: &str, tokens: &TokenInterner, paths: &PathInterner| {
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

        let sets_at = |path: &str, tokens: &TokenInterner, paths: &PathInterner| {
            let path = SpecPath::parse(path, tokens, paths).unwrap();
            layer
                .variant_sets_at(&path, paths)
                .map(|(_, names)| names.prepend.clone())
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
        let selections: VariantSelections = [(a, y), (b, y)].into_iter().collect();
        let mut selected: Vec<Vec<(TokenId, TokenId)>> = prim
            .selected_variant_branches(&selections)
            .map(|branch| branch.chain().collect())
            .collect();
        selected.sort();
        assert_eq!(selected, [vec![(a, y)], vec![(a, y), (b, y)]]);
    }
}
