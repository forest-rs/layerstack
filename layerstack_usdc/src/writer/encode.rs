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

use super::compress::{IntWidth, LZ4_MAX_TOTAL_INPUT, compressed_ints, lz4_compress};
use super::error::UsdcWriteError;
use super::path::{CratePath, path_tree};
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

/// The tables and value data of a file being written.
struct Packer {
    /// Bootstrap placeholder, then value data, then sections.
    out: Vec<u8>,
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
    /// Stored value data by `(value type, representation flags, bytes)`,
    /// so identical values are written once (the `_valueDedup` and
    /// `_arrayDedup` tables).
    blobs: BTreeMap<(u8, u64, Vec<u8>), u64>,
}

impl Packer {
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
            blobs: BTreeMap::new(),
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
        self.token(path.element_token())?;
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
        bytes: Vec<u8>,
        align: bool,
    ) -> Result<u64, UsdcWriteError> {
        let key = (ty as u8, flags, bytes);
        if let Some(&offset) = self.blobs.get(&key) {
            return Ok(rep(ty, flags, offset));
        }
        if align {
            let padded = self.out.len().next_multiple_of(8);
            self.out.resize(padded, 0);
        }
        let offset = self.out.len() as u64;
        if offset > MAX_PAYLOAD {
            return Err(UsdcWriteError::TooLarge);
        }
        self.out.extend_from_slice(&key.2);
        self.blobs.insert(key, offset);
        Ok(rep(ty, flags, offset))
    }

    // ── Values (`_PackValue`, `_ValueHandler::Pack`) ────────────────────

    fn pack(&mut self, value: &Value, site: Site<'_>) -> Result<u64, UsdcWriteError> {
        use ValueType as T;
        Ok(match value {
            // `_IsAlwaysInlined`: a bitwise type of at most four bytes is
            // always inlined as its own bytes, low byte first (`bool`,
            // `uchar`, `int`, `uint`, `half`, `float`, `half2`, the
            // specifier/variability/permission enums, `SdfValueBlock`), as
            // are the string, token and asset path indexes.
            Value::Block => inlined(T::ValueBlock, 0),
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
                Err(_) => self.blob(T::Int64, 0, v.to_le_bytes().to_vec(), false)?,
            },
            Value::UInt64(v) => match u32::try_from(*v) {
                Ok(small) => inlined(T::UInt64, small),
                Err(_) => self.blob(T::UInt64, 0, v.to_le_bytes().to_vec(), false)?,
            },
            Value::Half(v) => inlined(T::Half, u32::from(*v)),
            Value::Float(v) => inlined(T::Float, v.to_bits()),
            // `_EncodeInline` for floating point: inline a double that is
            // exactly a float.
            Value::Double(v) => match exact_f32(*v) {
                Some(f) => inlined(T::Double, f.to_bits()),
                None => self.blob(T::Double, 0, v.to_le_bytes().to_vec(), false)?,
            },
            Value::TimeCode(v) => self.blob(T::TimeCode, 0, v.to_le_bytes().to_vec(), false)?,
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
            Value::TimeCodeArray(v) => self.plain_array(T::TimeCode, v.len(), |out| {
                out.extend_from_slice(&doubles(v).bytes);
            })?,
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
            Value::Vec2hArray(v) => self.math_array(T::Vec2h, v.len(), halves(v.as_flattened()))?,
            Value::Vec3hArray(v) => self.math_array(T::Vec3h, v.len(), halves(v.as_flattened()))?,
            Value::Vec4hArray(v) => self.math_array(T::Vec4h, v.len(), halves(v.as_flattened()))?,
            Value::Vec2fArray(v) => self.math_array(T::Vec2f, v.len(), floats(v.as_flattened()))?,
            Value::Vec3fArray(v) => self.math_array(T::Vec3f, v.len(), floats(v.as_flattened()))?,
            Value::Vec4fArray(v) => self.math_array(T::Vec4f, v.len(), floats(v.as_flattened()))?,
            Value::Vec2dArray(v) => {
                self.math_array(T::Vec2d, v.len(), doubles(v.as_flattened()))?
            }
            Value::Vec3dArray(v) => {
                self.math_array(T::Vec3d, v.len(), doubles(v.as_flattened()))?
            }
            Value::Vec4dArray(v) => {
                self.math_array(T::Vec4d, v.len(), doubles(v.as_flattened()))?
            }
            Value::Vec2iArray(v) => self.math_array(T::Vec2i, v.len(), ints(v.as_flattened()))?,
            Value::Vec3iArray(v) => self.math_array(T::Vec3i, v.len(), ints(v.as_flattened()))?,
            Value::Vec4iArray(v) => self.math_array(T::Vec4i, v.len(), ints(v.as_flattened()))?,
            Value::QuathArray(v) => self.math_array(T::Quath, v.len(), halves(v.as_flattened()))?,
            Value::QuatfArray(v) => self.math_array(T::Quatf, v.len(), floats(v.as_flattened()))?,
            Value::QuatdArray(v) => {
                self.math_array(T::Quatd, v.len(), doubles(v.as_flattened()))?
            }
            Value::Matrix2dArray(v) => self.math_array(
                T::Matrix2d,
                v.len(),
                doubles(v.as_flattened().as_flattened()),
            )?,
            Value::Matrix3dArray(v) => self.math_array(
                T::Matrix3d,
                v.len(),
                doubles(v.as_flattened().as_flattened()),
            )?,
            Value::Matrix4dArray(v) => self.math_array(
                T::Matrix4d,
                v.len(),
                doubles(v.as_flattened().as_flattened()),
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
                    packer.reference(item, false, site, out)
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
            }) if prepended.is_empty()
                && appended.is_empty()
                && deleted.is_empty()
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
                };
                self.pack(
                    &Value::Payload(items.first().unwrap_or(&none).clone()),
                    site,
                )?
            }
            Value::PayloadListOp(op) => {
                let bytes = self.list_op(op, site, |packer, item, out| {
                    packer.reference(item, true, site, out)
                })?;
                self.blob(T::PayloadListOp, 0, bytes, false)?
            }
            Value::Payload(payload) => {
                let mut bytes = Vec::new();
                self.reference(payload, true, site, &mut bytes)?;
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
            Value::UnregisteredValue(inner) => self.unregistered(inner, site)?,
            Value::TimeSamples(samples) => self.time_samples(samples, site)?,
        })
    }

    /// `Write(TimeSamples)`: the times, packed as a `std::vector<double>`,
    /// and then the count and representations of the values, each part
    /// reached through `_RecursiveWrite` — a relative offset, preceded by
    /// any data the part itself needs.
    fn time_samples(
        &mut self,
        samples: &[(f64, Value)],
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
        let offset = self.out.len() as u64;
        if offset > MAX_PAYLOAD {
            return Err(UsdcWriteError::TooLarge);
        }
        let at = self.out.len();
        self.out.extend_from_slice(&[0; 8]);
        let mut times = (samples.len() as u64).to_le_bytes().to_vec();
        for (time, _) in samples {
            times.extend_from_slice(&time.to_le_bytes());
        }
        let times_rep = self.blob(ValueType::DoubleVector, 0, times, false)?;
        let jump = (self.out.len() - at) as u64;
        self.out[at..at + 8].copy_from_slice(&jump.to_le_bytes());
        self.out.extend_from_slice(&times_rep.to_le_bytes());

        let at = self.out.len();
        self.out.extend_from_slice(&[0; 8]);
        let mut reps = Vec::with_capacity(samples.len());
        for (_, value) in samples {
            reps.push(self.pack(value, site)?);
        }
        let jump = (self.out.len() - at) as u64;
        self.out[at..at + 8].copy_from_slice(&jump.to_le_bytes());
        self.out
            .extend_from_slice(&(samples.len() as u64).to_le_bytes());
        for value_rep in reps {
            self.out.extend_from_slice(&value_rep.to_le_bytes());
        }
        Ok(rep(ValueType::TimeSamples, 0, offset))
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
        self.blob(ty, ARRAY_BIT, bytes, true)
    }

    fn math_array(
        &mut self,
        ty: ValueType,
        len: usize,
        elems: Elems,
    ) -> Result<u64, UsdcWriteError> {
        self.plain_array(ty, len, |out| out.extend_from_slice(&elems.bytes))
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
            return self.blob(ty, ARRAY_BIT, bytes, false);
        }
        bytes.extend_from_slice(&checked_ints(values, width)?);
        self.blob(ty, ARRAY_BIT | COMPRESSED_BIT, bytes, false)
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
            return self.blob(ty, ARRAY_BIT | COMPRESSED_BIT, bytes, false);
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
        self.blob(ty, ARRAY_BIT | COMPRESSED_BIT, bytes, false)
    }

    /// `WriteMap(VtDictionary)`: count, then per entry (in key order) the
    /// key's string index and the value written through
    /// `_RecursiveWrite` — a relative offset to the value representation,
    /// preceded by any data the value itself needs. An empty dictionary is
    /// inlined.
    fn dictionary(
        &mut self,
        entries: &[(String, Value)],
        site: Site<'_>,
    ) -> Result<u64, UsdcWriteError> {
        if entries.is_empty() {
            return Ok(inlined(ValueType::Dictionary, 0));
        }
        let mut sorted: Vec<&(String, Value)> = entries.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some(pair) = sorted.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(UsdcWriteError::DuplicateDictionaryKey {
                path: site.path.into(),
                field: site.field.into(),
                key: pair[0].0.clone(),
            });
        }
        let offset = self.out.len() as u64;
        if offset > MAX_PAYLOAD {
            return Err(UsdcWriteError::TooLarge);
        }
        self.out
            .extend_from_slice(&(sorted.len() as u64).to_le_bytes());
        for (key, value) in sorted {
            site.check_text(key)?;
            let key = self.string(key)?;
            self.out.extend_from_slice(&key.to_le_bytes());
            let at = self.out.len();
            self.out.extend_from_slice(&[0; 8]);
            let value_rep = self.pack(value, site)?;
            let jump = (self.out.len() - at) as u64;
            self.out[at..at + 8].copy_from_slice(&jump.to_le_bytes());
            self.out.extend_from_slice(&value_rep.to_le_bytes());
        }
        Ok(rep(ValueType::Dictionary, 0, offset))
    }

    /// `Write(SdfUnregisteredValue)`: the held value written through
    /// `_RecursiveWrite`, as a dictionary entry's value is — a relative
    /// offset to the value representation, preceded by any data the value
    /// itself needs.
    fn unregistered(&mut self, inner: &Value, site: Site<'_>) -> Result<u64, UsdcWriteError> {
        let offset = self.out.len() as u64;
        if offset > MAX_PAYLOAD {
            return Err(UsdcWriteError::TooLarge);
        }
        let at = self.out.len();
        self.out.extend_from_slice(&[0; 8]);
        let value_rep = self.pack(inner, site)?;
        let jump = (self.out.len() - at) as u64;
        self.out[at..at + 8].copy_from_slice(&jump.to_le_bytes());
        self.out.extend_from_slice(&value_rep.to_le_bytes());
        Ok(rep(ValueType::UnregisteredValue, 0, offset))
    }

    /// A path list op item: the index of an absolute path.
    fn path_text(&mut self, text: &str) -> Result<u32, UsdcWriteError> {
        let path = CratePath::parse(text)?;
        self.path(&path)
    }

    /// `Write(SdfReference)` / `Write(SdfPayload)` into `out`: the asset
    /// path's string index, the prim path's index (the empty path for the
    /// `defaultPrim`), the layer offset and scale, and for a reference its
    /// `customData`, written empty (a zero entry count).
    fn reference(
        &mut self,
        arc: &Reference,
        payload: bool,
        site: Site<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(), UsdcWriteError> {
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
            out.extend_from_slice(&0_u64.to_le_bytes());
        }
        Ok(())
    }

    /// `Write(SdfListOp)`: a header byte of `_ListOpHeader` bits, then each
    /// present list as a `u64` count and its items, each written by `item`.
    fn list_op<T>(
        &mut self,
        op: &ListOp<T>,
        site: Site<'_>,
        mut item: impl FnMut(&mut Self, &T, &mut Vec<u8>) -> Result<(), UsdcWriteError>,
    ) -> Result<Vec<u8>, UsdcWriteError> {
        const IS_EXPLICIT: u8 = 1 << 0;
        const HAS_EXPLICIT: u8 = 1 << 1;
        const HAS_DELETED: u8 = 1 << 3;
        const HAS_PREPENDED: u8 = 1 << 5;
        const HAS_APPENDED: u8 = 1 << 6;

        if op.explicit.is_some()
            && !(op.prepended.is_empty() && op.appended.is_empty() && op.deleted.is_empty())
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
            (op.prepended.as_slice(), HAS_PREPENDED),
            (op.appended.as_slice(), HAS_APPENDED),
            (op.deleted.as_slice(), HAS_DELETED),
        ] {
            if !list.is_empty() {
                header |= bit;
            }
        }
        let mut bytes = alloc::vec![header];
        // Written in `_ListOpHeader` order: explicit, prepended, appended,
        // deleted. Equal items encode to equal bytes, so a repeat within a
        // list is found by its encoding.
        for list in [explicit, &op.prepended, &op.appended, &op.deleted] {
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
            let token = self.token_index[path.element_token()];
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
fn text_item<'a>(
    site: Site<'a>,
    add: fn(&mut Packer, &str) -> Result<u32, UsdcWriteError>,
) -> impl Fn(&mut Packer, &String, &mut Vec<u8>) -> Result<(), UsdcWriteError> + 'a {
    move |packer, text, out| {
        site.check_text(text)?;
        out.extend_from_slice(&add(packer, text)?.to_le_bytes());
        Ok(())
    }
}

/// A list op item stored as its own little-endian bytes.
fn le_item<T: Copy, const N: usize>(
    bytes: fn(T) -> [u8; N],
) -> impl Fn(&mut Packer, &T, &mut Vec<u8>) -> Result<(), UsdcWriteError> {
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
/// the version a value needs (`Write(GfTimeCode)`).
pub(super) fn required_version(specs: &[Spec]) -> CrateVersion {
    let timecode = specs
        .iter()
        .flat_map(|s| &s.fields)
        .any(|f| has_timecode(&f.value));
    if timecode {
        CrateVersion::TIMECODES
    } else {
        CrateVersion::NEW_FILE_DEFAULT
    }
}

/// Checks the specs' paths, forms, parents and field names, and returns
/// them in `Sdf_CrateData::Save` order: prim paths (the pseudo-root first)
/// in `SdfPath` order, then property paths grouped by name.
fn prepare(specs: &[Spec]) -> Result<Vec<(CratePath, &Spec)>, UsdcWriteError> {
    let mut forms: BTreeMap<CratePath, SpecForm> = BTreeMap::new();
    let mut prepared = Vec::with_capacity(specs.len());
    for spec in specs {
        let path = CratePath::parse(&spec.path)?;
        let fits = match spec.form {
            SpecForm::PseudoRoot => path.is_root(),
            SpecForm::Prim => !path.is_root() && !path.is_property(),
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
    for (path, spec) in &prepared {
        if let Some(parent) = path.parent() {
            let ok = matches!(
                (forms.get(&parent), path.is_property()),
                (Some(SpecForm::Prim), _) | (Some(SpecForm::PseudoRoot), false)
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
