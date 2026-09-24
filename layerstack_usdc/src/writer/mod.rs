// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDC (crate) writer.
//!
//! [`write_crate`] serializes a list of [`Spec`]s — each a path, a
//! [`SpecForm`] and named [`Field`]s holding crate-level [`Value`]s — into
//! the binary crate format that [`crate::read_usdc`] and OpenUSD read.
//! This layer knows nothing about any authoring model: callers state every
//! field (`specifier`, `typeName`, `primChildren`, `properties`, `default`,
//! metadata, ...) explicitly, as OpenUSD's `Sdf_CrateData` stores them.
//! [`write_document`] is such a caller: it lowers an authored
//! [`layerstack_usda::writer::Document`] to the specs OpenUSD's text parser
//! stores for the same document's USDA.
//!
//! # Format
//!
//! The file is laid out as OpenUSD writes it (`pxr/usd/sdf/crateFile.cpp`,
//! `CrateFile::_Write`): an 88-byte bootstrap header, the value data, the
//! six structural sections (TOKENS, STRINGS, FIELDS, FIELDSETS, PATHS,
//! SPECS) and the table of contents. Tokens are LZ4-compressed; field token
//! indexes, field sets, the path tree and the spec table are integer-coded
//! and compressed; field value representations are LZ4-compressed.
//!
//! Values follow OpenUSD's encoding choices: scalars that fit in
//! four bytes are inlined in the value representation (including doubles
//! exactly representable as `float`, and vectors or diagonal matrices whose
//! components fit `int8`); integer arrays of 16 or more elements are
//! integer-coded, and floating-point arrays of 16 or more are stored as
//! coded integers or as a lookup table when that applies. Two deliberate
//! differences keep values exact where OpenUSD's choice would not: a
//! `-0.0` component never selects an integer encoding (which would store
//! `+0`), and lookup tables compare elements by bit pattern.
//!
//! Output is deterministic: the same specs always produce the same bytes.
//! Specs are laid out in `Sdf_CrateData::Save` order (prim paths in
//! `SdfPath` order, then properties grouped by name), whatever order they
//! are given in; fields keep the given order; dictionaries are written in
//! key order, as `VtDictionary` holds them. Identical values are stored
//! once, except dictionaries, which OpenUSD also shares; that only affects
//! file size.
//!
//! # Version
//!
//! Files are written as crate version 0.8.0
//! ([`CrateVersion::NEW_FILE_DEFAULT`]), OpenUSD's default for new files
//! (`DEFAULT_NEW_VERSION`, `USD_WRITE_NEW_USDC_FILES_AS_VERSION`), and
//! upgraded only when a value needs a newer one, as OpenUSD's
//! `RequestWriteVersionUpgrade` does: `timecode` values require 0.9.0
//! ([`CrateVersion::TIMECODES`]). No value this writer supports needs
//! anything newer, so files are readable by every OpenUSD release that
//! reads crate 0.8.0 (0.9.0 with timecodes), including the ones in Apple's
//! platforms, and by this crate's reader.
//!
//! Spec: AOUSD Core §16.3 (crate file format).

mod compress;
pub mod document;
mod encode;
mod error;
mod path;
#[cfg(test)]
mod tests;

use alloc::string::String;
use alloc::vec::Vec;

pub use document::write_document;
pub use error::UsdcWriteError;
pub use layerstack::doc::Specifier;

pub use crate::value_type::SpecForm;
use crate::version::CrateVersion;

/// A spec: path, form and fields.
///
/// `path` is `/` for the pseudo-root ([`SpecForm::PseudoRoot`]), a prim path
/// such as `/Root/Mesh` for [`SpecForm::Prim`], or a prim property path such
/// as `/Root/Mesh.points` for [`SpecForm::Attribute`] and
/// [`SpecForm::Relationship`].
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    /// Absolute path.
    pub path: String,
    /// What kind of spec this is.
    pub form: SpecForm,
    /// Fields, in the order they are stored.
    pub fields: Vec<Field>,
}

impl Spec {
    /// Creates a spec without fields.
    pub fn new(path: impl Into<String>, form: SpecForm) -> Self {
        Self {
            path: path.into(),
            form,
            fields: Vec::new(),
        }
    }

    /// Appends a field.
    #[must_use]
    pub fn with_field(mut self, name: impl Into<String>, value: Value) -> Self {
        self.fields.push(Field::new(name, value));
        self
    }
}

/// A named field of a spec (e.g. `typeName`, `default`, `kind`).
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// Field name, as OpenUSD stores it (`SdfFieldKeys`, e.g.
    /// `documentation` for USDA's `doc`).
    pub name: String,
    /// Field value.
    pub value: Value,
}

impl Field {
    /// Creates a field.
    pub fn new(name: impl Into<String>, value: Value) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

/// Attribute variability (`SdfVariability`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variability {
    /// May vary over time.
    #[default]
    Varying,
    /// Does not vary over time (`uniform`).
    Uniform,
}

/// Spec permission (`SdfPermission`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Permission {
    /// Public.
    #[default]
    Public,
    /// Private.
    Private,
}

/// An `SdfListOp`: either an explicit list, or prepended, appended and
/// deleted items. The deprecated "added" and "ordered" lists are not
/// written.
///
/// Spec: AOUSD Core §12.4 (list ops), §16.3.10 (list op encoding).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListOp<T> {
    /// `Some` makes the list op explicit (possibly with no items).
    pub explicit: Option<Vec<T>>,
    /// Prepended items.
    pub prepended: Vec<T>,
    /// Appended items.
    pub appended: Vec<T>,
    /// Deleted items.
    pub deleted: Vec<T>,
}

impl<T> ListOp<T> {
    /// An explicit list op (`rel r = [...]`, `x.connect = [...]`).
    pub fn explicit(items: Vec<T>) -> Self {
        Self {
            explicit: Some(items),
            prepended: Vec::new(),
            appended: Vec::new(),
            deleted: Vec::new(),
        }
    }

    /// A list op that prepends `items` (`prepend apiSchemas = [...]`).
    pub fn prepend(items: Vec<T>) -> Self {
        Self {
            explicit: None,
            prepended: items,
            appended: Vec::new(),
            deleted: Vec::new(),
        }
    }
}

/// A crate value.
///
/// Scalar and array variants are named after OpenUSD's value types; math
/// types store components in file order (row-major matrices, quaternions as
/// imaginary `i, j, k` then real). Half-precision values are IEEE 754
/// binary16 bit patterns. Paths in list ops are absolute path strings.
///
/// Spec: AOUSD Core §16.3.10 (value types).
#[derive(Clone, Debug, PartialEq)]
#[allow(missing_docs, reason = "variants are named after the value types")]
pub enum Value {
    /// `SdfValueBlock` (`None` in USDA).
    Block,
    Bool(bool),
    UChar(u8),
    Int(i32),
    UInt(u32),
    Int64(i64),
    UInt64(u64),
    Half(u16),
    Float(f32),
    Double(f64),
    /// `SdfTimeCode`; requires crate version 0.9.0.
    TimeCode(f64),
    String(String),
    Token(String),
    /// `SdfAssetPath`.
    Asset(String),
    Specifier(Specifier),
    Variability(Variability),
    Permission(Permission),
    Vec2h([u16; 2]),
    Vec3h([u16; 3]),
    Vec4h([u16; 4]),
    Vec2f([f32; 2]),
    Vec3f([f32; 3]),
    Vec4f([f32; 4]),
    Vec2d([f64; 2]),
    Vec3d([f64; 3]),
    Vec4d([f64; 4]),
    Vec2i([i32; 2]),
    Vec3i([i32; 3]),
    Vec4i([i32; 4]),
    Quath([u16; 4]),
    Quatf([f32; 4]),
    Quatd([f64; 4]),
    Matrix2d([[f64; 2]; 2]),
    Matrix3d([[f64; 3]; 3]),
    Matrix4d([[f64; 4]; 4]),
    BoolArray(Vec<bool>),
    UCharArray(Vec<u8>),
    IntArray(Vec<i32>),
    UIntArray(Vec<u32>),
    Int64Array(Vec<i64>),
    UInt64Array(Vec<u64>),
    HalfArray(Vec<u16>),
    FloatArray(Vec<f32>),
    DoubleArray(Vec<f64>),
    /// `SdfTimeCode[]`; requires crate version 0.9.0.
    TimeCodeArray(Vec<f64>),
    StringArray(Vec<String>),
    TokenArray(Vec<String>),
    AssetArray(Vec<String>),
    Vec2hArray(Vec<[u16; 2]>),
    Vec3hArray(Vec<[u16; 3]>),
    Vec4hArray(Vec<[u16; 4]>),
    Vec2fArray(Vec<[f32; 2]>),
    Vec3fArray(Vec<[f32; 3]>),
    Vec4fArray(Vec<[f32; 4]>),
    Vec2dArray(Vec<[f64; 2]>),
    Vec3dArray(Vec<[f64; 3]>),
    Vec4dArray(Vec<[f64; 4]>),
    Vec2iArray(Vec<[i32; 2]>),
    Vec3iArray(Vec<[i32; 3]>),
    Vec4iArray(Vec<[i32; 4]>),
    QuathArray(Vec<[u16; 4]>),
    QuatfArray(Vec<[f32; 4]>),
    QuatdArray(Vec<[f64; 4]>),
    Matrix2dArray(Vec<[[f64; 2]; 2]>),
    Matrix3dArray(Vec<[[f64; 3]; 3]>),
    Matrix4dArray(Vec<[[f64; 4]; 4]>),
    /// `VtDictionary`: string keys (unique) to values.
    Dictionary(Vec<(String, Self)>),
    /// `std::vector<TfToken>` (e.g. `primChildren`, `properties`).
    TokenVector(Vec<String>),
    /// `SdfTokenListOp` (e.g. `apiSchemas`).
    TokenListOp(ListOp<String>),
    /// `SdfStringListOp` (e.g. `variantSetNames`).
    StringListOp(ListOp<String>),
    /// `SdfPathListOp` (e.g. `targetPaths`, `connectionPaths`).
    PathListOp(ListOp<String>),
}

/// Serializes `specs` as a USDC file.
///
/// Exactly one spec must be the pseudo-root at `/`, and every other spec's
/// parent prim (or the pseudo-root) must have a spec. Children lists
/// (`primChildren`, `properties`) are fields like any other: the writer
/// stores what it is given.
///
/// # Errors
///
/// Returns a [`UsdcWriteError`] for an invalid or unsupported path or spec
/// form, a duplicate spec or field, a missing pseudo-root or parent, text
/// containing NUL, a duplicate dictionary key, an invalid list op, or a
/// layer beyond the format's limits. Nothing is produced in that case.
pub fn write_crate(specs: &[Spec]) -> Result<Vec<u8>, UsdcWriteError> {
    encode::write(specs)
}

/// The crate version [`write_crate`] writes for `specs`:
/// [`CrateVersion::NEW_FILE_DEFAULT`], or [`CrateVersion::TIMECODES`] when a
/// field holds a `timecode` value.
pub fn required_version(specs: &[Spec]) -> CrateVersion {
    encode::required_version(specs)
}
