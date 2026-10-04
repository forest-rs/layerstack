// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Packing specs into the crate layout: value representations and value
//! data, the token/string/path/field/field-set tables, the structural
//! sections, the table of contents and the bootstrap header.
//!
//! Each step mirrors the OpenUSD routine named in its comment
//! (`pxr/usd/sdf/crateFile.cpp`, `crateValueInliners.h`,
//! `crateData.cpp`).
//!
//! Spec: AOUSD Core §16.3.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::hash::BuildHasher;

use layerstack::HashMap;
use smallvec::SmallVec;

use super::array_dedup::{Array, ArrayDedup};
use super::compress::{IntWidth, LZ4_MAX_TOTAL_INPUT, compressed_ints, lz4_compress};
use super::error::UsdcWriteError;
use super::path::{CratePath, Element, path_tree};
use super::{ListOp, Permission, Reference, Spec, Specifier, Value, Variability};
use crate::toc;
use crate::value_type::{SpecForm, ValueType};
use crate::version::CrateVersion;

/// Size of `CrateFile::_BootStrap`: identifier, version, TOC offset and
/// eight reserved `int64`s.
const BOOTSTRAP_SIZE: usize = 88;

/// Arrays shorter than this are never compressed (`MinCompressedArraySize`).
const MIN_COMPRESSED_ARRAY_SIZE: usize = 16;

/// Largest value-representation payload (48 bits).
const MAX_PAYLOAD: u64 = (1 << 48) - 1;

/// Cheap, bitwise equality for common metadata leaves. Other kinds fall
/// through to canonical encoded-byte deduplication, not `Value::PartialEq`,
/// whose numerical float equality would conflate signed zeros.
fn same_metadata_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::Double(a), Value::Double(b)) | (Value::TimeCode(a), Value::TimeCode(b)) => {
            a.to_bits() == b.to_bits()
        }
        (a, b)
            if matches!(
                a,
                Value::Block
                    | Value::AnimationBlock
                    | Value::Bool(_)
                    | Value::UChar(_)
                    | Value::Int(_)
                    | Value::UInt(_)
                    | Value::Int64(_)
                    | Value::UInt64(_)
                    | Value::Half(_)
                    | Value::String(_)
                    | Value::Token(_)
                    | Value::Asset(_)
                    | Value::Specifier(_)
                    | Value::Variability(_)
                    | Value::Permission(_)
            ) =>
        {
            a == b
        }
        (a, b) => match (Array::from_value(a), Array::from_value(b)) {
            (Some(a), Some(b)) => a.same(b),
            _ => false,
        },
    }
}

// ── Value representations ────────────────────────────────────────────────

const ARRAY_BIT: u64 = 1 << 63;
const INLINED_BIT: u64 = 1 << 62;
const COMPRESSED_BIT: u64 = 1 << 61;

/// `ValueRep`: payload in the low 48 bits, type in bits 48–55, flags above.
fn rep(ty: ValueType, flags: u64, payload: u64) -> u64 {
    debug_assert!(payload <= MAX_PAYLOAD, "payload fits 48 bits");
    flags | (u64::from(ty as u8) << 48) | payload
}

fn inlined(ty: ValueType, payload: u32) -> u64 {
    rep(ty, INLINED_BIT, u64::from(payload))
}

/// Where a spec's field is being packed, for error reports.
#[derive(Clone, Copy)]
struct Site<'a> {
    path: &'a str,
    field: &'a str,
}

impl Site<'_> {
    fn nul(self) -> UsdcWriteError {
        UsdcWriteError::NulInText {
            path: self.path.into(),
            field: self.field.into(),
        }
    }

    fn check_text(self, text: &str) -> Result<(), UsdcWriteError> {
        if text.contains('\0') {
            Err(self.nul())
        } else {
            Ok(())
        }
    }

    fn list_op(self, reason: &'static str) -> UsdcWriteError {
        UsdcWriteError::InvalidListOp {
            path: self.path.into(),
            field: self.field.into(),
            reason,
        }
    }
}

/// Converts a table length to a `u32` index, leaving `u32::MAX` free (it
/// is the field-set terminator).
fn index(len: usize) -> Result<u32, UsdcWriteError> {
    u32::try_from(len)
        .ok()
        .filter(|&i| i != u32::MAX)
        .ok_or(UsdcWriteError::TooLarge)
}

type SortedDictionary<'a> = SmallVec<[&'a (String, Value); 8]>;

/// The tables and value data of a file being written.
struct Packer<'a> {
    /// Bootstrap placeholder, then value data, then sections.
    out: Vec<u8>,
    arrays: ArrayDedup<'a>,
    /// Last sorted map of each size: a borrowed fast path before child packing.
    dictionaries: HashMap<usize, (SortedDictionary<'a>, u64)>,
    tokens: Vec<String>,
    token_index: BTreeMap<String, u32>,
    /// Token index of each string (`_strings`).
    strings: Vec<u32>,
    string_index: BTreeMap<String, u32>,
    /// Each path, `None` for the empty path (`SdfPath()`), which has an
    /// index but no place in the path tree.
    paths: Vec<Option<CratePath>>,
    path_index: BTreeMap<CratePath, u32>,
    empty_path_index: Option<u32>,
    /// `(token index, value representation)` of each field.
    fields: Vec<(u32, u64)>,
    field_index: BTreeMap<(u32, u64), u32>,
    /// Field indexes, each set terminated by `u32::MAX` (`FieldIndex()`).
    fieldsets: Vec<u32>,
    fieldset_index: BTreeMap<Vec<u32>, u32>,
    /// `(path index, field set index, spec form)`.
    specs: Vec<(u32, u32, u32)>,
    /// Non-array candidates by `(value type, representation flags, length, hash)`.
    /// Retain offsets into `out`, not a second copy of every encoded payload.
    blobs: HashMap<(u8, u64, usize, u64), SmallVec<[usize; 1]>>,
}

impl<'a> Packer<'a> {
    fn new() -> Self {
        let mut packer = Self {
            out: alloc::vec![0; BOOTSTRAP_SIZE],
            tokens: Vec::new(),
            token_index: BTreeMap::new(),
            strings: Vec::new(),
            string_index: BTreeMap::new(),
            paths: Vec::new(),
            path_index: BTreeMap::new(),
            empty_path_index: None,
            fields: Vec::new(),
            field_index: BTreeMap::new(),
            fieldsets: Vec::new(),
            fieldset_index: BTreeMap::new(),
            specs: Vec::new(),
            blobs: HashMap::default(),
            arrays: ArrayDedup::default(),
            dictionaries: HashMap::default(),
        };
        // `CrateFile::StartPacking`: token 0 is one that can never be a
        // property name, because the path tree marks property elements by
        // negating their token index, which cannot mark index 0.
        packer.token(";-)").expect("the first token always fits");
        packer
    }

    /// `_AddToken`.
    fn token(&mut self, text: &str) -> Result<u32, UsdcWriteError> {
        if let Some(&i) = self.token_index.get(text) {
            return Ok(i);
        }
        let i = index(self.tokens.len())?;
        self.tokens.push(text.into());
        self.token_index.insert(text.into(), i);
        Ok(i)
    }

    /// `_AddString`: a string is stored as a token and indexed separately.
    fn string(&mut self, text: &str) -> Result<u32, UsdcWriteError> {
        if let Some(&i) = self.string_index.get(text) {
            return Ok(i);
        }
        let token = self.token(text)?;
        let i = index(self.strings.len())?;
        self.strings.push(token);
        self.string_index.insert(text.into(), i);
        Ok(i)
    }

    /// `_AddPath`: adds the parent first, then this path's element token.
    fn path(&mut self, path: &CratePath) -> Result<u32, UsdcWriteError> {
        if let Some(&i) = self.path_index.get(path) {
            return Ok(i);
        }
        if let Some(parent) = path.parent() {
            self.path(&parent)?;
        }
        self.token(&path.element_token())?;
        let i = index(self.paths.len())?;
        self.paths.push(Some(path.clone()));
        self.path_index.insert(path.clone(), i);
        Ok(i)
    }

    /// `_AddPath(SdfPath())`: the empty path, a reference or payload's
    /// prim path when it targets the `defaultPrim`. It is counted among the
    /// paths but left out of the path tree (`_WritePaths`), so a reader
    /// finds it as the default, empty path.
    fn empty_path(&mut self) -> Result<u32, UsdcWriteError> {
        if let Some(i) = self.empty_path_index {
            return Ok(i);
        }
        let i = index(self.paths.len())?;
        self.paths.push(None);
        self.empty_path_index = Some(i);
        Ok(i)
    }

    /// `_AddField`.
    fn field(&mut self, token: u32, value_rep: u64) -> Result<u32, UsdcWriteError> {
        if let Some(&i) = self.field_index.get(&(token, value_rep)) {
            return Ok(i);
        }
        let i = index(self.fields.len())?;
        self.fields.push((token, value_rep));
        self.field_index.insert((token, value_rep), i);
        Ok(i)
    }

    /// `_AddFieldSet`.
    fn fieldset(&mut self, fields: Vec<u32>) -> Result<u32, UsdcWriteError> {
        if let Some(&i) = self.fieldset_index.get(&fields) {
            return Ok(i);
        }
        let i = index(self.fieldsets.len())?;
        self.fieldsets.extend_from_slice(&fields);
        self.fieldsets.push(u32::MAX);
        self.fieldset_index.insert(fields, i);
        Ok(i)
    }

    /// Stores `bytes` (deduplicated) and returns the representation
    /// pointing at them. `align` pads the data to 8 bytes first, as
    /// `_WriteUncompressedArray` does.
    fn blob(
        &mut self,
        ty: ValueType,
        flags: u64,
        bytes: impl AsRef<[u8]>,
        align: bool,
    ) -> Result<u64, UsdcWriteError> {
        let bytes = bytes.as_ref();
        let index = if align {
            self.out.len().next_multiple_of(8)
        } else {
            self.out.len()
        };
        let key = (
            ty as u8,
            flags,
            bytes.len(),
            self.blobs.hasher().hash_one(bytes),
        );
        let candidates = self.blobs.entry(key).or_default();
        for &offset in candidates.iter() {
            if &self.out[offset..offset + bytes.len()] == bytes {
                return Ok(rep(ty, flags, offset as u64));
            }
        }
        candidates.push(index);
        let offset = index as u64;
        if offset > MAX_PAYLOAD {
            return Err(UsdcWriteError::TooLarge);
        }
        self.out.resize(index, 0);
        self.out.extend_from_slice(bytes);
        Ok(rep(ty, flags, offset))
    }

    // ── Values (`_PackValue`, `_ValueHandler::Pack`) ────────────────────

    fn pack(&mut self, value: &'a Value, site: Site<'_>) -> Result<u64, UsdcWriteError> {
        if let Some(array) = Array::from_value(value) {
            let (found, hash) = self.arrays.lookup(array);
            if let Some(rep) = found {
                return Ok(rep);
            }
            let rep = self.pack_value(value, site)?;
            self.arrays.insert(array, hash, rep);
            return Ok(rep);
        }
        self.pack_value(value, site)
    }

    fn pack_value(&mut self, value: &'a Value, site: Site<'_>) -> Result<u64, UsdcWriteError> {
        use ValueType as T;
        Ok(match value {
            // `_IsAlwaysInlined`: a bitwise type of at most four bytes is
            // always inlined as its own bytes, low byte first (`bool`,
            // `uchar`, `int`, `uint`, `half`, `float`, `half2`, the
            // specifier/variability/permission enums, `SdfValueBlock`), as
            // are the string, token and asset path indexes.
            Value::Block => inlined(T::ValueBlock, 0),
            Value::AnimationBlock => inlined(T::AnimationBlock, 0),
            Value::Bool(v) => inlined(T::Bool, u32::from(*v)),
            Value::UChar(v) => inlined(T::UChar, u32::from(*v)),
            #[allow(clippy::cast_sign_loss, reason = "bit pattern")]
            Value::Int(v) => inlined(T::Int, *v as u32),
            Value::UInt(v) => inlined(T::UInt, *v),
            // `_EncodeInline` for integers: inline when the value fits the
            // 32-bit type of the same signedness.
            #[allow(clippy::cast_sign_loss, reason = "bit pattern")]
            Value::Int64(v) => match i32::try_from(*v) {
                Ok(small) => inlined(T::Int64, small as u32),
                Err(_) => self.blob(T::Int64, 0, v.to_le_bytes(), false)?,
            },
            Value::UInt64(v) => match u32::try_from(*v) {
                Ok(small) => inlined(T::UInt64, small),
                Err(_) => self.blob(T::UInt64, 0, v.to_le_bytes(), false)?,
            },
            Value::Half(v) => inlined(T::Half, u32::from(*v)),
            Value::Float(v) => inlined(T::Float, v.to_bits()),
            // `_EncodeInline` for floating point: inline a double that is
            // exactly a float.
            Value::Double(v) => match exact_f32(*v) {
                Some(f) => inlined(T::Double, f.to_bits()),
                None => self.blob(T::Double, 0, v.to_le_bytes(), false)?,
            },
            Value::TimeCode(v) => self.blob(T::TimeCode, 0, v.to_le_bytes(), false)?,
            Value::String(v) => {
                site.check_text(v)?;
                inlined(T::String, self.string(v)?)
            }
            Value::Token(v) => {
                site.check_text(v)?;
                inlined(T::Token, self.token(v)?)
            }
            // `GetInlinedValue(SdfAssetPath)`: a scalar asset path is
            // inlined as a *token* index (arrays use string indexes).
            Value::Asset(v) => {
                site.check_text(v)?;
                inlined(T::AssetPath, self.token(v)?)
            }
            Value::PathExpression(v) => {
                site.check_text(v)?;
                // OpenUSD writes expression text through Write(string), unlike
                // the specialized token-index inliner for SdfAssetPath.
                let index = self.string(v)?;
                self.blob(T::PathExpression, 0, index.to_le_bytes(), false)?
            }
            Value::PathExpressionArray(v) => {
                let indexes = self.text_indexes(v, site, Self::string)?;
                self.plain_array(T::PathExpression, v.len(), |out| {
                    out.extend_from_slice(&indexes);
                })?
            }
            Value::Relocates(entries) => {
                let mut bytes = (entries.len() as u64).to_le_bytes().to_vec();
                for (source, target) in entries {
                    for path in [source, target] {
                        let index = if path.is_empty() {
                            self.empty_path()?
                        } else {
                            self.path_text(path)?
                        };
                        bytes.extend_from_slice(&index.to_le_bytes());
                    }
                }
                self.blob(T::Relocates, 0, bytes, false)?
            }
            Value::Specifier(v) => inlined(
                T::Specifier,
                match v {
                    Specifier::Def => 0,
                    Specifier::Over => 1,
                    Specifier::Class => 2,
                },
            ),
            Value::Variability(v) => inlined(
                T::Variability,
                match v {
                    Variability::Varying => 0,
                    Variability::Uniform => 1,
                },
            ),
            Value::Permission(v) => inlined(
                T::Permission,
                match v {
                    Permission::Public => 0,
                    Permission::Private => 1,
                },
            ),
            // `GfVec2h` is four bytes, so it is always inlined bitwise
            // rather than through `_EncodeInline`'s `int8` components.
            Value::Vec2h([x, y]) => inlined(T::Vec2h, u32::from(*x) | (u32::from(*y) << 16)),
            Value::Vec3h(v) => self.vector(T::Vec3h, &halves(v))?,
            Value::Vec4h(v) => self.vector(T::Vec4h, &halves(v))?,
            Value::Vec2f(v) => self.vector(T::Vec2f, &floats(v))?,
            Value::Vec3f(v) => self.vector(T::Vec3f, &floats(v))?,
            Value::Vec4f(v) => self.vector(T::Vec4f, &floats(v))?,
            Value::Vec2d(v) => self.vector(T::Vec2d, &doubles(v))?,
            Value::Vec3d(v) => self.vector(T::Vec3d, &doubles(v))?,
            Value::Vec4d(v) => self.vector(T::Vec4d, &doubles(v))?,
            Value::Vec2i(v) => self.vector(T::Vec2i, &ints(v))?,
            Value::Vec3i(v) => self.vector(T::Vec3i, &ints(v))?,
            Value::Vec4i(v) => self.vector(T::Vec4i, &ints(v))?,
            // Quaternions are not `GfVec`s and are never inlined.
            Value::Quath(v) => self.blob(T::Quath, 0, halves(v).bytes, false)?,
            Value::Quatf(v) => self.blob(T::Quatf, 0, floats(v).bytes, false)?,
            Value::Quatd(v) => self.blob(T::Quatd, 0, doubles(v).bytes, false)?,
            Value::Matrix2d(m) => self.matrix(T::Matrix2d, m.as_flattened(), 2)?,
            Value::Matrix3d(m) => self.matrix(T::Matrix3d, m.as_flattened(), 3)?,
            Value::Matrix4d(m) => self.matrix(T::Matrix4d, m.as_flattened(), 4)?,
            Value::BoolArray(v) => self.plain_array(T::Bool, v.len(), |out| {
                out.extend(v.iter().map(|&b| u8::from(b)));
            })?,
            Value::UCharArray(v) => {
                self.plain_array(T::UChar, v.len(), |out| out.extend_from_slice(v))?
            }
            Value::IntArray(v) => self.int_array(
                T::Int,
                &v.iter().map(|&x| i64::from(x)).collect::<Vec<_>>(),
                IntWidth::W32,
            )?,
            #[allow(clippy::cast_possible_wrap, reason = "bit pattern")]
            Value::UIntArray(v) => self.int_array(
                T::UInt,
                &v.iter().map(|&x| i64::from(x as i32)).collect::<Vec<_>>(),
                IntWidth::W32,
            )?,
            Value::Int64Array(v) => self.int_array(T::Int64, v, IntWidth::W64)?,
            #[allow(clippy::cast_possible_wrap, reason = "bit pattern")]
            Value::UInt64Array(v) => self.int_array(
                T::UInt64,
                &v.iter().map(|&x| x as i64).collect::<Vec<_>>(),
                IntWidth::W64,
            )?,
            Value::HalfArray(v) => self.float_array(T::Half, &halves(v))?,
            Value::FloatArray(v) => self.float_array(T::Float, &floats(v))?,
            Value::DoubleArray(v) => self.float_array(T::Double, &doubles(v))?,
            // `GfTimeCode` arrays have no compressed form.
            Value::TimeCodeArray(v) => {
                self.math_array(T::TimeCode, v.len(), v, f64::to_le_bytes)?
            }
            Value::StringArray(v) => {
                let indexes = self.text_indexes(v, site, Self::string)?;
                self.plain_array(T::String, v.len(), |out| out.extend_from_slice(&indexes))?
            }
            Value::TokenArray(v) => {
                let indexes = self.text_indexes(v, site, Self::token)?;
                self.plain_array(T::Token, v.len(), |out| out.extend_from_slice(&indexes))?
            }
            Value::AssetArray(v) => {
                let indexes = self.text_indexes(v, site, Self::string)?;
                self.plain_array(T::AssetPath, v.len(), |out| out.extend_from_slice(&indexes))?
            }
            Value::Vec2hArray(v) => {
                self.math_array(T::Vec2h, v.len(), v.as_flattened(), u16::to_le_bytes)?
            }
            Value::Vec3hArray(v) => {
                self.math_array(T::Vec3h, v.len(), v.as_flattened(), u16::to_le_bytes)?
            }
            Value::Vec4hArray(v) => {
                self.math_array(T::Vec4h, v.len(), v.as_flattened(), u16::to_le_bytes)?
            }
            Value::Vec2fArray(v) => {
                self.math_array(T::Vec2f, v.len(), v.as_flattened(), f32::to_le_bytes)?
            }
            Value::Vec3fArray(v) => {
                self.math_array(T::Vec3f, v.len(), v.as_flattened(), f32::to_le_bytes)?
            }
            Value::Vec4fArray(v) => {
                self.math_array(T::Vec4f, v.len(), v.as_flattened(), f32::to_le_bytes)?
            }
            Value::Vec2dArray(v) => {
                self.math_array(T::Vec2d, v.len(), v.as_flattened(), f64::to_le_bytes)?
            }
            Value::Vec3dArray(v) => {
                self.math_array(T::Vec3d, v.len(), v.as_flattened(), f64::to_le_bytes)?
            }
            Value::Vec4dArray(v) => {
                self.math_array(T::Vec4d, v.len(), v.as_flattened(), f64::to_le_bytes)?
            }
            Value::Vec2iArray(v) => {
                self.math_array(T::Vec2i, v.len(), v.as_flattened(), i32::to_le_bytes)?
            }
            Value::Vec3iArray(v) => {
                self.math_array(T::Vec3i, v.len(), v.as_flattened(), i32::to_le_bytes)?
            }
            Value::Vec4iArray(v) => {
                self.math_array(T::Vec4i, v.len(), v.as_flattened(), i32::to_le_bytes)?
            }
            Value::QuathArray(v) => {
                self.math_array(T::Quath, v.len(), v.as_flattened(), u16::to_le_bytes)?
            }
            Value::QuatfArray(v) => {
                self.math_array(T::Quatf, v.len(), v.as_flattened(), f32::to_le_bytes)?
            }
            Value::QuatdArray(v) => {
                self.math_array(T::Quatd, v.len(), v.as_flattened(), f64::to_le_bytes)?
            }
            Value::Matrix2dArray(v) => self.math_array(
                T::Matrix2d,
                v.len(),
                v.as_flattened().as_flattened(),
                f64::to_le_bytes,
            )?,
            Value::Matrix3dArray(v) => self.math_array(
                T::Matrix3d,
                v.len(),
                v.as_flattened().as_flattened(),
                f64::to_le_bytes,
            )?,
            Value::Matrix4dArray(v) => self.math_array(
                T::Matrix4d,
                v.len(),
                v.as_flattened().as_flattened(),
                f64::to_le_bytes,
            )?,
            Value::Dictionary(entries) => self.dictionary(entries, site)?,
            // `Write(std::vector<TfToken>)`: count, then token indexes.
            Value::TokenVector(v) => {
                let indexes = self.text_indexes(v, site, Self::token)?;
                let mut bytes = (v.len() as u64).to_le_bytes().to_vec();
                bytes.extend_from_slice(&indexes);
                self.blob(T::TokenVector, 0, bytes, false)?
            }
            Value::TokenListOp(op) => {
                let bytes = self.list_op(op, site, text_item(site, Self::token))?;
                self.blob(T::TokenListOp, 0, bytes, false)?
            }
            Value::StringListOp(op) => {
                let bytes = self.list_op(op, site, text_item(site, Self::string))?;
                self.blob(T::StringListOp, 0, bytes, false)?
            }
            Value::PathListOp(op) => {
                let bytes = self.list_op(op, site, text_item(site, Self::path_text))?;
                self.blob(T::PathListOp, 0, bytes, false)?
            }
            // Integer list op items are stored as themselves.
            Value::IntListOp(op) => {
                let bytes = self.list_op(op, site, le_item(i32::to_le_bytes))?;
                self.blob(T::IntListOp, 0, bytes, false)?
            }
            Value::UIntListOp(op) => {
                let bytes = self.list_op(op, site, le_item(u32::to_le_bytes))?;
                self.blob(T::UIntListOp, 0, bytes, false)?
            }
            Value::Int64ListOp(op) => {
                let bytes = self.list_op(op, site, le_item(i64::to_le_bytes))?;
                self.blob(T::Int64ListOp, 0, bytes, false)?
            }
            Value::UInt64ListOp(op) => {
                let bytes = self.list_op(op, site, le_item(u64::to_le_bytes))?;
                self.blob(T::UInt64ListOp, 0, bytes, false)?
            }
            Value::ReferenceListOp(op) => {
                let bytes = self.list_op(op, site, |packer, item, out| {
                    packer.reference(item, &item.custom_data, false, site, out)
                })?;
                self.blob(T::ReferenceListOp, 0, bytes, false)?
            }
            // `Sdf_CrateData` stores an explicit payload list op of no
            // payload, or of one payload with an asset path, as a single
            // `SdfPayload` (an internal payload needs the list op form).
            Value::PayloadListOp(ListOp {
                explicit: Some(items),
                prepended,
                appended,
                deleted,
                added,
                reordered,
            }) if prepended.is_empty()
                && appended.is_empty()
                && deleted.is_empty()
                && added.is_empty()
                && reordered.is_empty()
                && match items.as_slice() {
                    [] => true,
                    [one] => !one.asset.is_empty(),
                    _ => false,
                } =>
            {
                let none = Reference {
                    asset: String::new(),
                    prim_path: String::new(),
                    offset: 0.0,
                    scale: 1.0,
                    custom_data: Vec::new(),
                };
                let mut bytes = Vec::new();
                self.reference(items.first().unwrap_or(&none), &[], true, site, &mut bytes)?;
                self.blob(T::Payload, 0, bytes, false)?
            }
            Value::PayloadListOp(op) => {
                let bytes = self.list_op(op, site, |packer, item, out| {
                    packer.reference(item, &item.custom_data, true, site, out)
                })?;
                self.blob(T::PayloadListOp, 0, bytes, false)?
            }
            Value::Payload(payload) => {
                let mut bytes = Vec::new();
                self.reference(payload, &payload.custom_data, true, site, &mut bytes)?;
                self.blob(T::Payload, 0, bytes, false)?
            }
            // `Write(std::vector<std::string>)`: count, then string indexes.
            Value::StringVector(v) => {
                let indexes = self.text_indexes(v, site, Self::string)?;
                let mut bytes = (v.len() as u64).to_le_bytes().to_vec();
                bytes.extend_from_slice(&indexes);
                self.blob(T::StringVector, 0, bytes, false)?
            }
            // `Write(std::vector<SdfLayerOffset>)`: count, then each offset
            // and scale.
            Value::LayerOffsetVector(v) => {
                let mut bytes = (v.len() as u64).to_le_bytes().to_vec();
                for (offset, scale) in v {
                    bytes.extend_from_slice(&offset.to_le_bytes());
                    bytes.extend_from_slice(&scale.to_le_bytes());
                }
                self.blob(T::LayerOffsetVector, 0, bytes, false)?
            }
            // `Write(SdfVariantSelectionMap)` (`WriteMap`): count, then each
            // set name's and variant name's string index, in set name order
            // as the `std::map` holds them.
            Value::VariantSelectionMap(entries) => {
                let mut sorted: Vec<&(String, String)> = entries.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                if let Some(pair) = sorted.windows(2).find(|w| w[0].0 == w[1].0) {
                    return Err(UsdcWriteError::DuplicateDictionaryKey {
                        path: site.path.into(),
                        field: site.field.into(),
                        key: pair[0].0.clone(),
                    });
                }
                let mut bytes = (sorted.len() as u64).to_le_bytes().to_vec();
                for (set, variant) in sorted {
                    for text in [set, variant] {
                        site.check_text(text)?;
                        bytes.extend_from_slice(&self.string(text)?.to_le_bytes());
                    }
                }
                self.blob(T::VariantSelectionMap, 0, bytes, false)?
            }
            Value::UnregisteredValue(inner) => self.unregistered(inner, site)?,
            Value::TimeSamples(samples) => self.time_samples(samples, site)?,
            Value::Spline(spline, custom_data) => self.spline(spline, custom_data, site)?,
            Value::ArrayEdit {
                literals,
                instructions,
            } => self.array_edit(literals, instructions, site)?,
        })
    }

    /// OpenUSD `_ValueHandler::PackArrayEdit`: packed literals, packed int64
    /// instructions and the discarded legacy `isDense` byte. Each instruction
    /// is its own group; group coalescing changes size, never semantics.
    fn array_edit(
        &mut self,
        literals: &'a Value,
        instructions: &[crate::value_rep::CrateArrayEditOp],
        site: Site<'_>,
    ) -> Result<u64, UsdcWriteError> {
        use crate::value_rep::CrateArrayEditOp as I;
        let invalid = || UsdcWriteError::InvalidArrayEdit {
            path: site.path.into(),
            field: site.field.into(),
        };
        let num_literals = array_len(literals).ok_or_else(invalid)?;
        let literal = |i: usize| {
            if i < num_literals {
                i64::try_from(i).map_err(|_| UsdcWriteError::TooLarge)
            } else {
                Err(invalid())
            }
        };
        let size = |n: u64| i64::try_from(n).map_err(|_| UsdcWriteError::TooLarge);
        let mut words = Vec::new();
        for op in instructions {
            let (opcode, args) = match *op {
                I::WriteLiteral { literal: i, index } => (0_u8, [literal(i)?, index]),
                I::WriteRef { src, index } => (1, [src, index]),
                I::InsertLiteral { literal: i, index } => (2, [literal(i)?, index]),
                I::InsertRef { src, index } => (3, [src, index]),
                I::Erase { index } => (4, [index, 0]),
                I::MinSize { len } => (5, [size(len)?, 0]),
                I::MinSizeFill { len, literal: i } => (6, [size(len)?, literal(i)?]),
                I::SetSize { len } => (7, [size(len)?, 0]),
                I::SetSizeFill { len, literal: i } => (8, [size(len)?, literal(i)?]),
                I::MaxSize { len } => (9, [size(len)?, 0]),
            };
            words.push((i64::from(opcode) << 56) | 1);
            words.push(args[0]);
            if matches!(opcode, 0..=3 | 6 | 8) {
                words.push(args[1]);
            }
        }
        let rep_literals = self.pack(literals, site)?;
        let element_type = u8::try_from((rep_literals >> 48) & 0xff).map_err(|_| invalid())?;
        let element_type = ValueType::try_from(element_type).map_err(|_| invalid())?;
        let rep_words = self.int_array(ValueType::Int64, &words, IntWidth::W64)?;
        let mut bytes = rep_literals.to_le_bytes().to_vec();
        bytes.extend_from_slice(&rep_words.to_le_bytes());
        bytes.push(0);
        self.blob(element_type, 1 << 60, bytes, false)
    }

    /// `Write(TsSpline)`: the spline's Ts binary data as a byte vector (a
    /// `u64` length, then the bytes), then its knot custom data as a map
    /// (a `u64` count followed by each time and typed dictionary). An empty spline is inlined as
    /// nothing.
    ///
    /// Spec: AOUSD Core §16.3.10.33 (spline encoding).
    fn spline(
        &mut self,
        spline: &layerstack::spline::SplineData,
        custom_data: &'a [(f64, Vec<(String, Value)>)],
        site: Site<'_>,
    ) -> Result<u64, UsdcWriteError> {
        if custom_data.iter().enumerate().any(|(i, (time, _))| {
            !spline.knots.iter().any(|k| k.time == *time)
                || custom_data[..i].iter().any(|(t, _)| t == time)
        }) || spline
            .knots
            .iter()
            .any(|k| !k.custom_data.is_empty() && !custom_data.iter().any(|(t, _)| *t == k.time))
        {
            return Err(UsdcWriteError::Document(
                layerstack_usda::writer::WriteError::InvalidSplineCustomData {
                    path: site.path.into(),
                },
            ));
        }
        if is_empty_spline(spline) {
            return Ok(inlined(ValueType::Spline, 0));
        }
        let data = ts_spline_data(spline);
        let mut bytes = (data.len() as u64).to_le_bytes().to_vec();
        bytes.extend_from_slice(&data);
        bytes.extend_from_slice(&(custom_data.len() as u64).to_le_bytes());
        for (time, entries) in custom_data {
            bytes.extend_from_slice(&time.to_le_bytes());
            self.dictionary_inline(entries, site, &mut bytes)?;
        }
        self.blob(ValueType::Spline, 0, bytes, false)
    }

    /// `Write(TimeSamples)`: the times, packed as a `std::vector<double>`,
    /// and then the count and representations of the values, each part
    /// reached through `_RecursiveWrite` — a relative offset, preceded by
    /// any data the part itself needs.
    fn time_samples(
        &mut self,
        samples: &'a [(f64, Value)],
        site: Site<'_>,
    ) -> Result<u64, UsdcWriteError> {
        if samples
            .windows(2)
            .any(|w| w[0].0.partial_cmp(&w[1].0) != Some(core::cmp::Ordering::Less))
            || samples.iter().any(|(time, _)| !time.is_finite())
        {
            return Err(UsdcWriteError::InvalidTimeSamples {
                path: site.path.into(),
                field: site.field.into(),
            });
        }
        let mut times = (samples.len() as u64).to_le_bytes().to_vec();
        for (time, _) in samples {
            times.extend_from_slice(&time.to_le_bytes());
        }
        let times_rep = self.blob(ValueType::DoubleVector, 0, times, false)?;
        // Pack dependencies first. Every recursive jump is then eight bytes,
        // so equal sample maps have equal bytes regardless of where written.
        let mut bytes = Vec::with_capacity(32 + samples.len() * 8);
        bytes.extend_from_slice(&8_u64.to_le_bytes());
        bytes.extend_from_slice(&times_rep.to_le_bytes());
        bytes.extend_from_slice(&8_u64.to_le_bytes());
        bytes.extend_from_slice(&(samples.len() as u64).to_le_bytes());
        for (_, value) in samples {
            bytes.extend_from_slice(&self.pack(value, site)?.to_le_bytes());
        }
        self.blob(ValueType::TimeSamples, 0, bytes, false)
    }

    /// Adds each text to a table, returning the `u32` indexes as bytes.
    fn text_indexes(
        &mut self,
        items: &[String],
        site: Site<'_>,
        add: fn(&mut Self, &str) -> Result<u32, UsdcWriteError>,
    ) -> Result<Vec<u8>, UsdcWriteError> {
        let mut bytes = Vec::with_capacity(items.len() * 4);
        for item in items {
            site.check_text(item)?;
            bytes.extend_from_slice(&add(self, item)?.to_le_bytes());
        }
        Ok(bytes)
    }

    /// `GfVec` values wider than four bytes: inlined as one `int8` per
    /// component when every component is exactly an `int8` (`_EncodeInline`
    /// for `GfVec`). Not for `half2`, which is always inlined bitwise.
    fn vector(&mut self, ty: ValueType, elems: &Elems) -> Result<u64, UsdcWriteError> {
        match elems
            .values
            .iter()
            .map(|&v| exact_i8(v))
            .collect::<Option<Vec<_>>>()
        {
            Some(small) => Ok(inlined(ty, pack_i8s(&small))),
            None => self.blob(ty, 0, elems.bytes.clone(), false),
        }
    }

    /// `GfMatrix` values: inlined as the `int8` diagonal when every other
    /// entry is zero (`_EncodeInline` for `GfMatrix`).
    fn matrix(&mut self, ty: ValueType, m: &[f64], dim: usize) -> Result<u64, UsdcWriteError> {
        let diagonal: Option<Vec<i8>> = (0..dim * dim)
            .map(|i| {
                if i % (dim + 1) == 0 {
                    exact_i8(m[i]).map(Some)
                } else {
                    // Off-diagonal entries must be `+0` (a `-0` would be lost).
                    (m[i].to_bits() == 0).then_some(None)
                }
            })
            .collect::<Option<Vec<_>>>()
            .map(|entries| entries.into_iter().flatten().collect());
        match diagonal {
            Some(small) => Ok(inlined(ty, pack_i8s(&small))),
            None => self.blob(ty, 0, doubles(m).bytes, false),
        }
    }

    /// `_WriteUncompressedArray`: 8-byte aligned `u64` count and elements.
    /// An empty array is a representation with payload 0 (`PackArray`).
    fn plain_array(
        &mut self,
        ty: ValueType,
        len: usize,
        elements: impl FnOnce(&mut Vec<u8>),
    ) -> Result<u64, UsdcWriteError> {
        if len == 0 {
            return Ok(rep(ty, ARRAY_BIT, 0));
        }
        let mut bytes = (len as u64).to_le_bytes().to_vec();
        elements(&mut bytes);
        self.array_blob(ty, ARRAY_BIT, &bytes, true)
    }

    /// Uncompressed math/timecode arrays need no numeric staging for
    /// compression decisions. After input deduplication, encode directly into
    /// the output, preserving bit patterns (including half NaNs and signed zero).
    /// Keep the encoder generic so component conversion can inline/vectorize;
    /// an indirect function call per component defeats that optimization.
    /// OpenUSD: `_WriteUncompressedArray` writes the count and contiguous data.
    fn math_array<T: Copy, const N: usize>(
        &mut self,
        ty: ValueType,
        len: usize,
        components: &[T],
        encode: impl Fn(T) -> [u8; N],
    ) -> Result<u64, UsdcWriteError> {
        if len == 0 {
            return Ok(rep(ty, ARRAY_BIT, 0));
        }
        let size = components
            .len()
            .checked_mul(N)
            .and_then(|bytes| bytes.checked_add(8))
            .ok_or(UsdcWriteError::TooLarge)?;
        let index = self.out.len().next_multiple_of(8);
        if index as u64 > MAX_PAYLOAD {
            return Err(UsdcWriteError::TooLarge);
        }
        self.out
            .try_reserve(
                size.checked_add(index - self.out.len())
                    .ok_or(UsdcWriteError::TooLarge)?,
            )
            .map_err(|_| UsdcWriteError::TooLarge)?;
        self.out.resize(index, 0);
        self.out.extend_from_slice(&(len as u64).to_le_bytes());
        self.out
            .extend(components.iter().flat_map(|&component| encode(component)));
        Ok(rep(ty, ARRAY_BIT, index as u64))
    }

    /// Arrays have already been deduplicated by borrowed input value.
    fn array_blob(
        &mut self,
        ty: ValueType,
        flags: u64,
        bytes: &[u8],
        align: bool,
    ) -> Result<u64, UsdcWriteError> {
        let index = if align {
            self.out.len().next_multiple_of(8)
        } else {
            self.out.len()
        };
        if index as u64 > MAX_PAYLOAD {
            return Err(UsdcWriteError::TooLarge);
        }
        self.out.resize(index, 0);
        self.out.extend_from_slice(bytes);
        Ok(rep(ty, flags, index as u64))
    }

    /// `_WritePossiblyCompressedArray` for (u)int and (u)int64 arrays:
    /// unaligned, integer-coded from 16 elements up.
    fn int_array(
        &mut self,
        ty: ValueType,
        values: &[i64],
        width: IntWidth,
    ) -> Result<u64, UsdcWriteError> {
        if values.is_empty() {
            return Ok(rep(ty, ARRAY_BIT, 0));
        }
        let mut bytes = (values.len() as u64).to_le_bytes().to_vec();
        if values.len() < MIN_COMPRESSED_ARRAY_SIZE {
            let size = match width {
                IntWidth::W32 => 4,
                IntWidth::W64 => 8,
            };
            for v in values {
                bytes.extend_from_slice(&v.to_le_bytes()[..size]);
            }
            return self.array_blob(ty, ARRAY_BIT, &bytes, false);
        }
        bytes.extend_from_slice(&checked_ints(values, width)?);
        self.array_blob(ty, ARRAY_BIT | COMPRESSED_BIT, &bytes, false)
    }

    /// `_WritePossiblyCompressedArray` for half, float and double arrays:
    /// from 16 elements up, all-integral arrays are stored as coded `int32`s
    /// (`'i'`), arrays with few distinct values as a lookup table and coded
    /// indexes (`'t'`); anything else is written uncompressed.
    fn float_array(&mut self, ty: ValueType, elems: &Elems) -> Result<u64, UsdcWriteError> {
        let len = elems.values.len();
        if len < MIN_COMPRESSED_ARRAY_SIZE {
            return self.plain_array(ty, len, |out| out.extend_from_slice(&elems.bytes));
        }
        let mut bytes = (len as u64).to_le_bytes().to_vec();
        if let Some(ints) = elems
            .values
            .iter()
            .map(|&v| exact_i32(v).map(i64::from))
            .collect::<Option<Vec<_>>>()
        {
            bytes.push(b'i');
            bytes.extend_from_slice(&checked_ints(&ints, IntWidth::W32)?);
            return self.array_blob(ty, ARRAY_BIT | COMPRESSED_BIT, &bytes, false);
        }
        // Give up on a lookup table once it would hold more than a quarter
        // of the elements (at most 1024), as OpenUSD does.
        let max_lut = (len / 4).min(1024);
        let size = elems.bytes.len() / len;
        let element = |i: usize| &elems.bytes[i * size..(i + 1) * size];
        let mut lut: Vec<usize> = Vec::new();
        let mut indexes: Vec<i64> = Vec::with_capacity(len);
        for i in 0..len {
            let found = lut.iter().position(|&j| element(j) == element(i));
            let at = match found {
                Some(at) => at,
                None if lut.len() < max_lut => {
                    lut.push(i);
                    lut.len() - 1
                }
                None => {
                    lut.clear();
                    break;
                }
            };
            indexes.push(at as i64);
        }
        if lut.is_empty() {
            return self.plain_array(ty, len, |out| out.extend_from_slice(&elems.bytes));
        }
        bytes.push(b't');
        #[allow(clippy::cast_possible_truncation, reason = "at most 1024 entries")]
        bytes.extend_from_slice(&(lut.len() as u32).to_le_bytes());
        for &j in &lut {
            bytes.extend_from_slice(element(j));
        }
        bytes.extend_from_slice(&checked_ints(&indexes, IntWidth::W32)?);
        self.array_blob(ty, ARRAY_BIT | COMPRESSED_BIT, &bytes, false)
    }

    /// `WriteMap(VtDictionary)`: count, then per entry (in key order) the
    /// key's string index and the value written through
    /// `_RecursiveWrite` — a relative offset to the value representation,
    /// preceded by any data the value itself needs. Dependencies are packed
    /// before the map, leaving fixed eight-byte jumps and canonical bytes for
    /// deduplication. OpenUSD's `_ValueHandler<VtDictionary>` likewise shares
    /// dictionaries. An empty dictionary is inlined.
    fn dictionary(
        &mut self,
        entries: &'a [(String, Value)],
        site: Site<'_>,
    ) -> Result<u64, UsdcWriteError> {
        if entries.is_empty() {
            return Ok(inlined(ValueType::Dictionary, 0));
        }
        let mut sorted: SortedDictionary<'a> = entries.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some(pair) = sorted.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(UsdcWriteError::DuplicateDictionaryKey {
                path: site.path.into(),
                field: site.field.into(),
                key: pair[0].0.clone(),
            });
        }
        if let Some((previous, value_rep)) = self.dictionaries.get(&sorted.len())
            && sorted
                .iter()
                .zip(previous)
                .all(|(a, b)| a.0 == b.0 && same_metadata_value(&a.1, &b.1))
        {
            return Ok(*value_rep);
        }
        // Small metadata maps need no temporary heap allocation. Large maps
        // retain a borrowed sort index and encode into a temporary byte buffer,
        // never cloning the authored values into a deduplication key.
        let mut bytes: SmallVec<[u8; 256]> = SmallVec::with_capacity(8 + sorted.len() * 20);
        bytes.extend_from_slice(&(sorted.len() as u64).to_le_bytes());
        for &(key, value) in &sorted {
            site.check_text(key)?;
            let key = self.string(key)?;
            let value_rep = self.pack(value, site)?;
            bytes.extend_from_slice(&key.to_le_bytes());
            bytes.extend_from_slice(&8_u64.to_le_bytes());
            bytes.extend_from_slice(&value_rep.to_le_bytes());
        }
        let value_rep = self.blob(ValueType::Dictionary, 0, bytes, false)?;
        self.dictionaries.insert(sorted.len(), (sorted, value_rep));
        Ok(value_rep)
    }

    /// `Write(SdfUnregisteredValue)`: the held value written through
    /// `_RecursiveWrite`, as a dictionary entry's value is — a relative
    /// offset to the value representation, preceded by any data the value
    /// itself needs.
    fn unregistered(&mut self, inner: &'a Value, site: Site<'_>) -> Result<u64, UsdcWriteError> {
        let value_rep = self.pack(inner, site)?;
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&8_u64.to_le_bytes());
        bytes[8..].copy_from_slice(&value_rep.to_le_bytes());
        self.blob(ValueType::UnregisteredValue, 0, bytes, false)
    }

    /// A path list op item: the index of an absolute path.
    fn path_text(&mut self, text: &str) -> Result<u32, UsdcWriteError> {
        let path = CratePath::parse(text)?;
        self.path(&path)
    }

    fn dictionary_inline(
        &mut self,
        entries: &'a [(String, Value)],
        site: Site<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(), UsdcWriteError> {
        let rep = self.dictionary(entries, site)?;
        if entries.is_empty() {
            out.extend_from_slice(&0_u64.to_le_bytes());
        } else {
            let at = (rep & ((1_u64 << 48) - 1)) as usize;
            out.extend_from_slice(&self.out[at..at + 8 + entries.len() * 20]);
        }
        Ok(())
    }

    /// `Write(SdfReference)` / `Write(SdfPayload)` into `out`: the asset
    /// path's string index, the prim path's index (the empty path for the
    /// `defaultPrim`), the layer offset and scale, and for a reference its
    /// authored typed `customData`.
    fn reference(
        &mut self,
        arc: &Reference,
        custom_data: &'a [(String, Value)],
        payload: bool,
        site: Site<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(), UsdcWriteError> {
        if payload && !custom_data.is_empty() {
            return Err(UsdcWriteError::Document(
                layerstack_usda::writer::WriteError::PayloadCustomData {
                    path: site.path.into(),
                },
            ));
        }
        site.check_text(&arc.asset)?;
        out.extend_from_slice(&self.string(&arc.asset)?.to_le_bytes());
        let path = if arc.prim_path.is_empty() {
            self.empty_path()?
        } else {
            self.path_text(&arc.prim_path)?
        };
        out.extend_from_slice(&path.to_le_bytes());
        out.extend_from_slice(&arc.offset.to_le_bytes());
        out.extend_from_slice(&arc.scale.to_le_bytes());
        if !payload {
            self.dictionary_inline(custom_data, site, out)?;
        }
        Ok(())
    }

    /// `Write(SdfListOp)`: a header byte of `_ListOpHeader` bits, then each
    /// present list as a `u64` count and its items, each written by `item`.
    fn list_op<T>(
        &mut self,
        op: &'a ListOp<T>,
        site: Site<'_>,
        mut item: impl FnMut(&mut Self, &'a T, &mut Vec<u8>) -> Result<(), UsdcWriteError>,
    ) -> Result<Vec<u8>, UsdcWriteError> {
        const IS_EXPLICIT: u8 = 1 << 0;
        const HAS_EXPLICIT: u8 = 1 << 1;
        const HAS_ADDED: u8 = 1 << 2;
        const HAS_DELETED: u8 = 1 << 3;
        const HAS_ORDERED: u8 = 1 << 4;
        const HAS_PREPENDED: u8 = 1 << 5;
        const HAS_APPENDED: u8 = 1 << 6;

        if op.explicit.is_some()
            && !(op.prepended.is_empty()
                && op.appended.is_empty()
                && op.deleted.is_empty()
                && op.added.is_empty()
                && op.reordered.is_empty())
        {
            return Err(site.list_op("an explicit list op cannot also edit"));
        }
        let explicit = op.explicit.as_deref().unwrap_or(&[]);
        let mut header = 0;
        if op.explicit.is_some() {
            header |= IS_EXPLICIT;
        }
        for (list, bit) in [
            (explicit, HAS_EXPLICIT),
            (op.added.as_slice(), HAS_ADDED),
            (op.prepended.as_slice(), HAS_PREPENDED),
            (op.appended.as_slice(), HAS_APPENDED),
            (op.deleted.as_slice(), HAS_DELETED),
            (op.reordered.as_slice(), HAS_ORDERED),
        ] {
            if !list.is_empty() {
                header |= bit;
            }
        }
        let mut bytes = alloc::vec![header];
        // Written in `_ListOpHeader` order: explicit, added, prepended,
        // appended, deleted, ordered. Equal items encode to equal bytes, so
        // a repeat within a list is found by its encoding.
        for list in [
            explicit,
            &op.added,
            &op.prepended,
            &op.appended,
            &op.deleted,
            &op.reordered,
        ] {
            if list.is_empty() {
                continue;
            }
            bytes.extend_from_slice(&(list.len() as u64).to_le_bytes());
            let mut seen = BTreeSet::new();
            for x in list {
                let mut encoded = Vec::new();
                item(self, x, &mut encoded)?;
                bytes.extend_from_slice(&encoded);
                if !seen.insert(encoded) {
                    return Err(site.list_op("an item repeats within a list"));
                }
            }
        }
        Ok(bytes)
    }

    // ── Structural sections (`_Write`) ──────────────────────────────────

    /// Appends the six sections, the table of contents and fills in the
    /// bootstrap header.
    fn finish(mut self, version: CrateVersion) -> Result<Vec<u8>, UsdcWriteError> {
        let mut sections: Vec<(&'static str, usize, usize)> = Vec::new();

        // TOKENS (`_WriteTokens`): count, uncompressed size, compressed
        // size, LZ4 of the NUL-terminated token strings.
        let start = self.out.len();
        let mut text = Vec::new();
        for token in &self.tokens {
            text.extend_from_slice(token.as_bytes());
            text.push(0);
        }
        let compressed = checked_lz4(&text)?;
        self.put_u64(self.tokens.len());
        self.put_u64(text.len());
        self.put_u64(compressed.len());
        self.out.extend_from_slice(&compressed);
        sections.push((toc::TOKENS, start, self.out.len()));

        // STRINGS: count, then token indexes.
        let start = self.out.len();
        self.put_u64(self.strings.len());
        for i in 0..self.strings.len() {
            let token = self.strings[i];
            self.out.extend_from_slice(&token.to_le_bytes());
        }
        sections.push((toc::STRINGS, start, self.out.len()));

        // FIELDS (`_WriteFields`): count, coded token indexes, then the
        // LZ4-compressed value representations.
        let start = self.out.len();
        self.put_u64(self.fields.len());
        let tokens: Vec<i64> = self.fields.iter().map(|f| i64::from(f.0)).collect();
        let coded = checked_ints(&tokens, IntWidth::W32)?;
        self.out.extend_from_slice(&coded);
        let reps: Vec<u8> = self.fields.iter().flat_map(|f| f.1.to_le_bytes()).collect();
        let compressed = checked_lz4(&reps)?;
        self.put_u64(compressed.len());
        self.out.extend_from_slice(&compressed);
        sections.push((toc::FIELDS, start, self.out.len()));

        // FIELDSETS (`_WriteFieldSets`): count, then coded field indexes
        // with `-1` terminators.
        let start = self.out.len();
        self.put_u64(self.fieldsets.len());
        #[allow(clippy::cast_possible_wrap, reason = "u32::MAX codes as -1")]
        let sets: Vec<i64> = self
            .fieldsets
            .iter()
            .map(|&f| i64::from(f as i32))
            .collect();
        let coded = checked_ints(&sets, IntWidth::W32)?;
        self.out.extend_from_slice(&coded);
        sections.push((toc::FIELDSETS, start, self.out.len()));

        // PATHS (`_WritePaths`, `_WriteCompressedPathData`): path count,
        // encoded count, then the coded path indexes, element tokens and
        // jumps of the tree in `SdfPath` order.
        let start = self.out.len();
        let mut sorted: Vec<(&CratePath, u32, u32)> = Vec::with_capacity(self.paths.len());
        for (i, path) in self.paths.iter().enumerate() {
            let Some(path) = path else {
                continue;
            };
            let token = self.token_index[&*path.element_token()];
            #[allow(clippy::cast_possible_truncation, reason = "path table is u32-indexed")]
            sorted.push((path, i as u32, token));
        }
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        let tree = path_tree(&sorted);
        let mut paths = Vec::new();
        paths.extend_from_slice(&(self.paths.len() as u64).to_le_bytes());
        paths.extend_from_slice(&(sorted.len() as u64).to_le_bytes());
        for column in [&tree.path_indexes, &tree.element_tokens, &tree.jumps] {
            paths.extend_from_slice(&checked_ints(column, IntWidth::W32)?);
        }
        self.out.extend_from_slice(&paths);
        sections.push((toc::PATHS, start, self.out.len()));

        // SPECS (`_WriteSpecs`): count, then coded path indexes, field set
        // indexes and spec forms.
        let start = self.out.len();
        self.put_u64(self.specs.len());
        let columns: [Vec<i64>; 3] = [
            self.specs.iter().map(|s| i64::from(s.0)).collect(),
            self.specs.iter().map(|s| i64::from(s.1)).collect(),
            self.specs.iter().map(|s| i64::from(s.2)).collect(),
        ];
        for column in &columns {
            let coded = checked_ints(column, IntWidth::W32)?;
            self.out.extend_from_slice(&coded);
        }
        sections.push((toc::SPECS, start, self.out.len()));

        // Table of contents: count, then name (16 bytes, NUL-padded),
        // start and size of each section.
        let toc_offset = self.out.len();
        self.put_u64(sections.len());
        for (name, start, end) in sections {
            let mut entry = [0_u8; 16];
            entry[..name.len()].copy_from_slice(name.as_bytes());
            self.out.extend_from_slice(&entry);
            self.put_u64(start);
            self.put_u64(end - start);
        }

        // `_BootStrap`: "PXR-USDC", version (major, minor, patch, then
        // zeros), TOC offset; the reserved words stay zero.
        self.out[..8].copy_from_slice(b"PXR-USDC");
        self.out[8..11].copy_from_slice(&[version.major, version.minor, version.patch]);
        self.out[16..24].copy_from_slice(&(toc_offset as u64).to_le_bytes());
        Ok(self.out)
    }

    fn put_u64(&mut self, v: usize) {
        self.out.extend_from_slice(&(v as u64).to_le_bytes());
    }
}

/// A list op item stored as the `u32` index `add` gives its text.
fn text_item<'a, 'input: 'a>(
    site: Site<'a>,
    add: fn(&mut Packer<'input>, &str) -> Result<u32, UsdcWriteError>,
) -> impl Fn(&mut Packer<'input>, &String, &mut Vec<u8>) -> Result<(), UsdcWriteError> + 'a {
    move |packer, text, out| {
        site.check_text(text)?;
        out.extend_from_slice(&add(packer, text)?.to_le_bytes());
        Ok(())
    }
}

/// A list op item stored as its own little-endian bytes.
fn le_item<T: Copy, const N: usize>(
    bytes: fn(T) -> [u8; N],
) -> impl Fn(&mut Packer<'_>, &T, &mut Vec<u8>) -> Result<(), UsdcWriteError> {
    move |_, item, out| {
        out.extend_from_slice(&bytes(*item));
        Ok(())
    }
}

fn checked_lz4(input: &[u8]) -> Result<Vec<u8>, UsdcWriteError> {
    if input.len() as u64 > LZ4_MAX_TOTAL_INPUT {
        return Err(UsdcWriteError::TooLarge);
    }
    Ok(lz4_compress(input))
}

fn checked_ints(values: &[i64], width: IntWidth) -> Result<Vec<u8>, UsdcWriteError> {
    // The coded form is at most one extra integer plus two bits per value.
    let bytes = match width {
        IntWidth::W32 => 4,
        IntWidth::W64 => 8,
    };
    if (values.len() as u64 + 1) * (bytes + 1) > LZ4_MAX_TOTAL_INPUT {
        return Err(UsdcWriteError::TooLarge);
    }
    Ok(compressed_ints(values, width))
}

// ── Element helpers ─────────────────────────────────────────────────────

/// Elements of a numeric value: numeric values (for encoding choices) and
/// their little-endian bytes (what is stored).
struct Elems {
    values: Vec<f64>,
    bytes: Vec<u8>,
}

fn floats(v: &[f32]) -> Elems {
    Elems {
        values: v.iter().map(|&x| f64::from(x)).collect(),
        bytes: v.iter().flat_map(|x| x.to_le_bytes()).collect(),
    }
}

fn doubles(v: &[f64]) -> Elems {
    Elems {
        values: v.to_vec(),
        bytes: v.iter().flat_map(|x| x.to_le_bytes()).collect(),
    }
}

fn ints(v: &[i32]) -> Elems {
    Elems {
        values: v.iter().map(|&x| f64::from(x)).collect(),
        bytes: v.iter().flat_map(|x| x.to_le_bytes()).collect(),
    }
}

fn halves(v: &[u16]) -> Elems {
    Elems {
        values: v.iter().map(|&h| f64::from(half_to_f32(h))).collect(),
        bytes: v.iter().flat_map(|x| x.to_le_bytes()).collect(),
    }
}

/// `v` as an `int8`, when exactly representable and not `-0`.
fn exact_i8(v: f64) -> Option<i8> {
    #[allow(clippy::cast_possible_truncation, reason = "range checked")]
    let small = v as i8;
    (f64::from(small) == v && !is_negative_zero(v)).then_some(small)
}

/// `v` as an `int32`, when exactly representable and not `-0`.
fn exact_i32(v: f64) -> Option<i32> {
    #[allow(clippy::cast_possible_truncation, reason = "range checked")]
    let small = v as i32;
    (f64::from(small) == v && !is_negative_zero(v)).then_some(small)
}

/// `v` as an `f32`, when exactly representable (`_IsExactlyRepresented`:
/// finite, in range and round-tripping; `-0` keeps its sign).
fn exact_f32(v: f64) -> Option<f32> {
    #[allow(clippy::cast_possible_truncation, reason = "round trip checked")]
    let f = v as f32;
    (v.is_finite() && f.is_finite() && f64::from(f) == v).then_some(f)
}

fn is_negative_zero(v: f64) -> bool {
    v == 0.0 && v.is_sign_negative()
}

/// Packs up to four `int8`s into an inlined payload, first in the low byte.
fn pack_i8s(values: &[i8]) -> u32 {
    let mut bytes = [0_u8; 4];
    for (b, &v) in bytes.iter_mut().zip(values) {
        *b = v.to_le_bytes()[0];
    }
    u32::from_le_bytes(bytes)
}

/// IEEE 754 binary16 to `f32` (exact).
fn half_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exp = u32::from((bits >> 10) & 0x1f);
    let mant = u32::from(bits & 0x3ff);
    let magnitude = match (exp, mant) {
        (0, 0) => 0,
        // Subnormal: mant × 2^-24, exact in f32.
        (0, _) => (mant as f32 * (1.0 / 16_777_216.0)).to_bits(),
        (31, _) => 0x7f80_0000 | (mant << 13),
        _ => ((exp + 112) << 23) | (mant << 13),
    };
    f32::from_bits(sign | magnitude)
}

// ── Entry points ────────────────────────────────────────────────────────

/// Whether `value` (or anything nested in it) is a `timecode`.
fn has_timecode(value: &Value) -> bool {
    match value {
        Value::TimeCode(_) | Value::TimeCodeArray(_) => true,
        Value::Dictionary(entries) => entries.iter().any(|(_, v)| has_timecode(v)),
        Value::UnregisteredValue(inner) => has_timecode(inner),
        Value::TimeSamples(samples) => samples.iter().any(|(_, v)| has_timecode(v)),
        _ => false,
    }
}

/// `RequestWriteVersionUpgrade`: start from OpenUSD's default and move to
/// the version a value needs (`Write(GfTimeCode)`, `Write(TsSpline)`).
pub(super) fn required_version(specs: &[Spec]) -> CrateVersion {
    let mut version = CrateVersion::NEW_FILE_DEFAULT;
    for field in specs.iter().flat_map(|s| &s.fields) {
        if has_array_edit(&field.value) {
            version = version.max(CrateVersion::ARRAY_EDITS);
        }
        if let Value::Spline(s, _) = &field.value {
            let required = if s.pre_loop_boundary.is_some()
                || s.post_loop_boundary.is_some()
                || s.data_type == layerstack::spline::SplineDataType::TimeCode
            {
                CrateVersion::SPLINE_LOOP_BOUNDARY_AND_TIMECODE
            } else if s.knots.iter().any(|k| {
                k.pre_tan_algorithm != layerstack::spline::TangentAlgorithm::None
                    || k.post_tan_algorithm != layerstack::spline::TangentAlgorithm::None
            }) {
                CrateVersion::SPLINE_TANGENT_ALGORITHMS
            } else {
                CrateVersion::SPLINES
            };
            version = version.max(required);
        }
        if matches!(field.value, Value::Relocates(_)) {
            version = version.max(CrateVersion::new(0, 11, 0));
        }
        if has_path_expression(&field.value) {
            version = version.max(CrateVersion::new(0, 10, 0));
        }
        if has_timecode(&field.value) {
            version = version.max(CrateVersion::TIMECODES);
        }
    }
    version
}

fn array_len(value: &Value) -> Option<usize> {
    Some(match value {
        Value::PathExpressionArray(v) => v.len(),
        Value::BoolArray(v) => v.len(),
        Value::UCharArray(v) => v.len(),
        Value::IntArray(v) => v.len(),
        Value::UIntArray(v) => v.len(),
        Value::Int64Array(v) => v.len(),
        Value::UInt64Array(v) => v.len(),
        Value::HalfArray(v) => v.len(),
        Value::FloatArray(v) => v.len(),
        Value::DoubleArray(v) => v.len(),
        Value::TimeCodeArray(v) => v.len(),
        Value::StringArray(v) => v.len(),
        Value::TokenArray(v) => v.len(),
        Value::AssetArray(v) => v.len(),
        Value::Vec2hArray(v) => v.len(),
        Value::Vec3hArray(v) => v.len(),
        Value::Vec4hArray(v) => v.len(),
        Value::Vec2fArray(v) => v.len(),
        Value::Vec3fArray(v) => v.len(),
        Value::Vec4fArray(v) => v.len(),
        Value::Vec2dArray(v) => v.len(),
        Value::Vec3dArray(v) => v.len(),
        Value::Vec4dArray(v) => v.len(),
        Value::Vec2iArray(v) => v.len(),
        Value::Vec3iArray(v) => v.len(),
        Value::Vec4iArray(v) => v.len(),
        Value::QuathArray(v) => v.len(),
        Value::QuatfArray(v) => v.len(),
        Value::QuatdArray(v) => v.len(),
        Value::Matrix2dArray(v) => v.len(),
        Value::Matrix3dArray(v) => v.len(),
        Value::Matrix4dArray(v) => v.len(),
        _ => return None,
    })
}

fn has_array_edit(value: &Value) -> bool {
    match value {
        Value::ArrayEdit { .. } => true,
        Value::Dictionary(entries) => entries.iter().any(|(_, v)| has_array_edit(v)),
        Value::TimeSamples(samples) => samples.iter().any(|(_, v)| has_array_edit(v)),
        Value::UnregisteredValue(v) => has_array_edit(v),
        _ => false,
    }
}

fn has_path_expression(value: &Value) -> bool {
    match value {
        Value::PathExpression(_) | Value::PathExpressionArray(_) => true,
        Value::Dictionary(entries) => entries.iter().any(|(_, value)| has_path_expression(value)),
        Value::TimeSamples(samples) => samples.iter().any(|(_, value)| has_path_expression(value)),
        Value::UnregisteredValue(value) => has_path_expression(value),
        _ => false,
    }
}

/// Whether `spline` is a default `TsSpline`, which OpenUSD inlines.
fn is_empty_spline(spline: &layerstack::spline::SplineData) -> bool {
    use layerstack::spline::{CurveType, Extrapolation, SplineDataType};
    spline.pre_loop_boundary.is_none()
        && spline.post_loop_boundary.is_none()
        && spline.data_type == SplineDataType::Unspecified
        && spline.knots.is_empty()
        && spline.default_curve_type == CurveType::Bezier
        && spline.pre_extrapolation == Extrapolation::Held
        && spline.post_extrapolation == Extrapolation::Held
        && spline.loop_params.is_none_or(|lp| lp == no_loops())
}

fn no_loops() -> layerstack::spline::LoopParams {
    layerstack::spline::LoopParams {
        proto_start: 0.0,
        proto_end: 0.0,
        num_pre_loops: 0,
        num_post_loops: 0,
        value_offset: 0.0,
    }
}

/// Encodes the smallest Ts binary format that retains the authored features:
/// format 1 for basic splines, 2 for tangent algorithms, 3 for loop boundary
/// times and `TimeCode` values. OpenUSD `ts/binary.cpp::GetBinaryData`.
fn ts_spline_data(spline: &layerstack::spline::SplineData) -> Vec<u8> {
    use layerstack::spline::{CurveType, Extrapolation, KnotInterp, SplineDataType};
    let mode = |e: Extrapolation| -> u8 {
        match e {
            Extrapolation::Block => 0,
            Extrapolation::Held => 1,
            Extrapolation::Linear => 2,
            Extrapolation::Sloped(_) => 3,
            Extrapolation::LoopRepeat => 4,
            Extrapolation::LoopReset => 5,
            Extrapolation::LoopOscillate => 6,
        }
    };
    let descriptor: u8 = match spline.data_type {
        SplineDataType::Unspecified => 0,
        SplineDataType::Double => 1,
        SplineDataType::Float => 2,
        SplineDataType::Half => 3,
        SplineDataType::TimeCode => 4,
    };
    let hermite = spline.default_curve_type == CurveType::Hermite;
    let loops = spline.loop_params.filter(|lp| *lp != no_loops());
    let mut out = Vec::new();
    let format = if spline.pre_loop_boundary.is_some()
        || spline.post_loop_boundary.is_some()
        || spline.data_type == SplineDataType::TimeCode
    {
        3
    } else if spline.knots.iter().any(|k| {
        k.pre_tan_algorithm != layerstack::spline::TangentAlgorithm::None
            || k.post_tan_algorithm != layerstack::spline::TangentAlgorithm::None
    }) {
        2
    } else {
        1
    };
    out.push(format | (descriptor << 4) | (u8::from(hermite) << 7));
    out.push(
        mode(spline.pre_extrapolation)
            | (mode(spline.post_extrapolation) << 3)
            | (u8::from(loops.is_some()) << 6),
    );
    if format == 3 {
        out.push(
            u8::from(spline.pre_loop_boundary.is_some())
                | (u8::from(spline.post_loop_boundary.is_some()) << 1),
        );
        for boundary in [spline.pre_loop_boundary, spline.post_loop_boundary]
            .into_iter()
            .flatten()
        {
            out.extend_from_slice(&boundary.to_le_bytes());
        }
    }
    for extrapolation in [spline.pre_extrapolation, spline.post_extrapolation] {
        if let Extrapolation::Sloped(slope) = extrapolation {
            out.extend_from_slice(&slope.to_le_bytes());
        }
    }
    if let Some(lp) = loops {
        out.extend_from_slice(&lp.proto_start.to_le_bytes());
        out.extend_from_slice(&lp.proto_end.to_le_bytes());
        out.extend_from_slice(&lp.num_pre_loops.to_le_bytes());
        out.extend_from_slice(&lp.num_post_loops.to_le_bytes());
        out.extend_from_slice(&lp.value_offset.to_le_bytes());
    }
    if spline.data_type == SplineDataType::Unspecified {
        return out;
    }
    let value = |out: &mut Vec<u8>, v: f64| match spline.data_type {
        SplineDataType::Double | SplineDataType::TimeCode | SplineDataType::Unspecified => {
            out.extend_from_slice(&v.to_le_bytes());
        }
        #[allow(clippy::cast_possible_truncation, reason = "a float spline's values")]
        SplineDataType::Float => out.extend_from_slice(&(v as f32).to_le_bytes()),
        SplineDataType::Half => {
            out.extend_from_slice(&layerstack::half::from_f64(v).to_le_bytes());
        }
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a spline holds fewer knots"
    )]
    out.extend_from_slice(&(spline.knots.len() as u32).to_le_bytes());
    for knot in &spline.knots {
        let interp: u8 = match knot.next_interp {
            KnotInterp::Block => 0,
            KnotInterp::Held => 1,
            KnotInterp::Linear => 2,
            KnotInterp::Curve => 3,
        };
        let flag = u8::from(knot.pre_value.is_some())
            | (interp << 1)
            | (u8::from(knot.curve_type == CurveType::Hermite) << 3)
            | (u8::from(knot.pre_tan_maya_form) << 4)
            | (u8::from(knot.post_tan_maya_form) << 5);
        out.push(flag);
        out.extend_from_slice(&knot.time.to_le_bytes());
        value(&mut out, knot.value);
        if let Some(pre_value) = knot.pre_value {
            value(&mut out, pre_value);
        }
        if !hermite {
            out.extend_from_slice(&knot.pre_tan_width.to_le_bytes());
            out.extend_from_slice(&knot.post_tan_width.to_le_bytes());
        }
        value(&mut out, knot.pre_tan_slope);
        value(&mut out, knot.post_tan_slope);
        if format > 1 {
            out.push(knot.pre_tan_algorithm as u8 | ((knot.post_tan_algorithm as u8) << 4));
        }
    }
    out
}

/// Checks the specs' paths, forms, parents and field names, and returns
/// them in `Sdf_CrateData::Save` order: prim paths (the pseudo-root first)
/// in `SdfPath` order, then property paths grouped by name.
fn prepare(specs: &[Spec]) -> Result<Vec<(CratePath, &Spec)>, UsdcWriteError> {
    let mut forms: BTreeMap<CratePath, SpecForm> = BTreeMap::new();
    let mut prepared = Vec::with_capacity(specs.len());
    for spec in specs {
        let path = CratePath::parse(&spec.path)?;
        let last = path.last_element();
        let fits = match spec.form {
            SpecForm::PseudoRoot => path.is_root(),
            SpecForm::Prim => matches!(last, Some(Element::Prim(_))),
            SpecForm::VariantSet => {
                matches!(last, Some(Element::Variant { variant, .. }) if variant.is_empty())
            }
            SpecForm::Variant => {
                matches!(last, Some(Element::Variant { variant, .. }) if !variant.is_empty())
            }
            SpecForm::Attribute | SpecForm::Relationship => path.is_property(),
            form => {
                return Err(UsdcWriteError::UnsupportedSpecForm {
                    path: spec.path.clone(),
                    form,
                });
            }
        };
        if !fits {
            return Err(UsdcWriteError::SpecPathMismatch {
                path: spec.path.clone(),
                form: spec.form,
            });
        }
        if forms.insert(path.clone(), spec.form).is_some() {
            return Err(UsdcWriteError::DuplicateSpec {
                path: spec.path.clone(),
            });
        }
        let mut names = BTreeSet::new();
        for field in &spec.fields {
            if field.name.is_empty() {
                return Err(UsdcWriteError::InvalidFieldName {
                    path: spec.path.clone(),
                });
            }
            if field.name.contains('\0') {
                return Err(UsdcWriteError::NulInText {
                    path: spec.path.clone(),
                    field: field.name.clone(),
                });
            }
            if !names.insert(field.name.as_str()) {
                return Err(UsdcWriteError::DuplicateField {
                    path: spec.path.clone(),
                    field: field.name.clone(),
                });
            }
        }
        prepared.push((path, spec));
    }
    if !forms.contains_key(&CratePath::root()) {
        return Err(UsdcWriteError::MissingPseudoRoot);
    }
    // A prim, variant set or property belongs to a prim or variant (a root
    // prim to the pseudo-root); a variant to its variant set.
    for (path, spec) in &prepared {
        let owner = match spec.form {
            SpecForm::Variant => path.variant_set(),
            _ => path.parent(),
        };
        if let Some(owner) = owner {
            let ok = matches!(
                (forms.get(&owner), spec.form),
                (Some(SpecForm::Prim | SpecForm::Variant), _)
                    | (Some(SpecForm::PseudoRoot), SpecForm::Prim)
                    | (Some(SpecForm::VariantSet), SpecForm::Variant)
            );
            if !ok {
                return Err(UsdcWriteError::MissingParent {
                    path: spec.path.clone(),
                });
            }
        }
    }
    prepared.sort_by(|(a, _), (b, _)| match (&a.property, &b.property) {
        (None, None) => a.cmp(b),
        (None, Some(_)) => core::cmp::Ordering::Less,
        (Some(_), None) => core::cmp::Ordering::Greater,
        (Some(x), Some(y)) => x.cmp(y).then_with(|| a.cmp(b)),
    });
    Ok(prepared)
}

/// `CrateFile::_Write` over specs packed as `Sdf_CrateData::Save` packs
/// them: per spec, each field's name token and value, then the spec's
/// path, then its field set.
pub(super) fn write(specs: &[Spec]) -> Result<Vec<u8>, UsdcWriteError> {
    let prepared = prepare(specs)?;
    let mut packer = Packer::new();
    for (path, spec) in &prepared {
        let mut fields = Vec::with_capacity(spec.fields.len());
        for field in &spec.fields {
            let token = packer.token(&field.name)?;
            let site = Site {
                path: &spec.path,
                field: &field.name,
            };
            let value_rep = packer.pack(&field.value, site)?;
            fields.push(packer.field(token, value_rep)?);
        }
        let path_index = packer.path(path)?;
        let fieldset = packer.fieldset(fields)?;
        packer
            .specs
            .push((path_index, fieldset, u32::from(spec.form as u8)));
    }
    packer.finish(required_version(specs))
}

#[cfg(test)]
mod blob_tests {
    use super::*;

    #[test]
    fn nested_arrays_share_the_write_session_cache() {
        let array = Value::Vec3fArray(alloc::vec![[1.25, -0.0, f32::from_bits(0x7fc0_0001)]; 129]);
        let nested =
            Value::UnregisteredValue(alloc::boxed::Box::new(Value::Dictionary(alloc::vec![(
                "samples".into(),
                Value::TimeSamples(alloc::vec![(0.0, array.clone()), (1.0, array.clone())])
            ),])));
        let mut packer = Packer::new();
        let site = Site {
            path: "/Root",
            field: "customData",
        };
        packer.pack(&nested, site).unwrap();
        let end = packer.out.len();
        let first = packer.pack(&array, site).unwrap();
        assert_eq!(packer.out.len(), end, "nested array was already encoded");
        assert_eq!(packer.pack(&array, site).unwrap(), first);
        assert_eq!(packer.out.len(), end);
    }

    #[test]
    fn collision_candidates_are_verified_against_output() {
        let mut packer = Packer::new();
        let first_bytes = [1, 2, 3, 4];
        let second_bytes = [1, 2, 3, 5];
        let first = packer.blob(ValueType::Vec2h, 0, first_bytes, true).unwrap();
        let first_offset = usize::try_from(first & MAX_PAYLOAD).unwrap();
        // Force an unequal candidate into the second value's hash bucket.
        let key = (
            ValueType::Vec2h as u8,
            0,
            4,
            packer.blobs.hasher().hash_one(second_bytes.as_slice()),
        );
        packer.blobs.entry(key).or_default().push(first_offset);
        let second = packer
            .blob(ValueType::Vec2h, 0, second_bytes, true)
            .unwrap();
        assert_ne!(first, second, "unequal bytes must not deduplicate");
        let end = packer.out.len();
        assert_eq!(
            packer
                .blob(ValueType::Vec2h, 0, second_bytes, true)
                .unwrap(),
            second
        );
        assert_eq!(packer.out.len(), end);
        packer.out.extend_from_slice(&[0; 4096]);
        assert_eq!(
            packer.blob(ValueType::Vec2h, 0, first_bytes, true).unwrap(),
            first
        );
        let other_type = packer.blob(ValueType::Vec2f, 0, first_bytes, true).unwrap();
        let other_flags = packer
            .blob(ValueType::Vec2h, ARRAY_BIT, first_bytes, true)
            .unwrap();
        assert_ne!(other_type & MAX_PAYLOAD, first & MAX_PAYLOAD);
        assert_ne!(other_flags & MAX_PAYLOAD, first & MAX_PAYLOAD);
    }
}
