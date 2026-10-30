// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared homogeneous numeric and text buffers, independent of USD type
//! aliases.
//!
//! Each buffer keeps native elements together: numbers and token ids in one
//! vector, strings in one text buffer ([`StringArray`]). Cloning shares the
//! buffer;
//! editing calls `Arc::make_mut`, retaining its allocation when uniquely
//! owned and copying once when another value still owns the old snapshot.
//! `Arc<Vec<T>>` deliberately retains spare capacity for sparse insertions
//! and removals. Typed slices borrow the native elements without expanding
//! them into `Value` records. Type aliases remain on `PropertyType`.

use alloc::{borrow::Cow, boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{fmt, mem::size_of};

use crate::{ArrayEdit, ArrayEditOp, ArrayEditOperand, TokenId, Value};

/// A retained numeric source could not materialize its immutable buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArrayReadError {
    /// Malformed or unsupported encoded data; the adapter supplies context.
    InvalidData(Arc<str>),
    /// The retained source exhausted its shared decode budget.
    BudgetExceeded {
        /// Original budget limit, in adapter-defined units.
        limit: u64,
    },
    /// A source returned another deferred source instead of a native buffer.
    RecursiveSource,
}
impl fmt::Display for ArrayReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(message) => f.write_str(message),
            Self::BudgetExceeded { limit } => {
                write!(f, "array decode budget exceeded ({limit} units)")
            }
            Self::RecursiveSource => f.write_str("array source returned a deferred buffer"),
        }
    }
}
impl core::error::Error for ArrayReadError {}

/// Adapter-owned immutable numeric storage, decoded from retained memory.
///
/// Implementations cache success and failure, return a native buffer, and do
/// no file I/O. The core owns neither scheduling nor an adapter's cache policy.
/// Sources remain usable across threads and after their loader is dropped.
pub trait DeferredArraySource: fmt::Debug + Send + Sync {
    /// Materializes once and borrows the cached success or error.
    fn materialize(&self) -> Result<&TypedArray, &ArrayReadError>;
    /// Scalar storage kind without decoding any elements.
    fn element_kind(&self) -> Value;
    /// Optionally retains an affine transform of `TimeCode` elements without
    /// decoding them: `time * offset.scale + offset.offset` (AOUSD Core
    /// §12.3.2.1). Other kinds must return `None`. Returning `None` uses the
    /// core's ordinary eager transform; success and failure still cache.
    fn retimed(&self, _offset: crate::LayerOffset) -> Option<TypedArray> {
        None
    }
}

macro_rules! array_types {
    ($($variant:ident, $ty:ty, $accessor:ident, $wrap:expr, $read:expr, $same:expr;)*) => {
        /// A homogeneous native buffer with shared, copy-on-write ownership.
        ///
        /// Numeric kinds remain distinct, including time codes and quaternions.
        /// Empty arrays retain their kind. Half values use their exact raw bits;
        /// matrix elements are flat row-major arrays. `token[]` elements are the
        /// store's [`TokenId`]s and `string[]` elements share one text buffer
        /// ([`StringArray`]); neither holds a value or an allocation per element.
        /// A mutable buffer is obtained with `Arc::make_mut` on a variant's payload.
        ///
        /// Spec: AOUSD Core §6.2–§6.3 (scalar and dimensioned array values).
        #[derive(Clone, Debug)]
        #[non_exhaustive]
        pub enum TypedArray {
            $(#[doc = concat!("Native `", stringify!($variant), "` elements.")]
            $variant(Arc<Vec<$ty>>),)*
            /// `token[]` elements, the store's token ids, in one allocation
            /// with the shared header: a read reaches them through one pointer.
            Token(Arc<[TokenId]>),
            /// `string[]` elements in one text buffer.
            String(Arc<StringArray>),
            /// An immutable adapter source; use `try_materialize` for errors.
            Deferred(Arc<dyn DeferredArraySource>),
        }
        impl PartialEq for TypedArray {
            fn eq(&self, other: &Self) -> bool {
                let (Ok(a), Ok(b)) = (self.try_materialize(), other.try_materialize()) else { return false; };
                match (a,b) {
                    $((Self::$variant(a), Self::$variant(b)) => a == b,)*
                    (Self::Token(a), Self::Token(b)) => a == b,
                    (Self::String(a), Self::String(b)) => a == b,
                    _ => false,
                }
            }
        }

        impl TypedArray {
            /// Whether these arrays retain the same native owner or deferred
            /// source. This never decodes storage or compares elements. Equal
            /// contents in independent allocations are different storage.
            pub fn shares_storage(&self, other: &Self) -> bool {
                match (self, other) {
                    $((Self::$variant(a), Self::$variant(b)) => Arc::ptr_eq(a, b),)*
                    (Self::Token(a), Self::Token(b)) => Arc::ptr_eq(a, b),
                    (Self::String(a), Self::String(b)) => Arc::ptr_eq(a, b),
                    (Self::Deferred(a), Self::Deferred(b)) => Arc::ptr_eq(a, b),
                    _ => false,
                }
            }

            /// Borrows a native buffer, or the source's cached decode error.
            /// No file I/O occurs. Prefer this before infallible inspection:
            /// `len`, `capacity` and `element_bytes` report zero on failure;
            /// element and slice accessors report `None`.
            pub fn try_materialize(&self) -> Result<&Self, &ArrayReadError> {
                static RECURSIVE: ArrayReadError = ArrayReadError::RecursiveSource;
                match self {
                    Self::Deferred(source) => match source.materialize()? {
                        Self::Deferred(_) => Err(&RECURSIVE),
                        native => Ok(native),
                    },
                    native => Ok(native),
                }
            }
            /// Number of native elements.
            #[must_use]
            pub fn len(&self) -> usize {
                match self { $(Self::$variant(items) => items.len(),)*Self::Token(items) => items.len(), Self::String(items) => items.len(), Self::Deferred(_) => self.try_materialize().map_or(0, Self::len) }
            }

            /// Whether the buffer contains no elements.
            #[must_use]
            pub fn is_empty(&self) -> bool { self.len() == 0 }

            /// Bytes occupied by initialized native elements, excluding capacity
            /// and the small shared allocation header. A string array counts its
            /// text and one end offset per element.
            #[must_use]
            pub fn element_bytes(&self) -> usize {
                match self { $(Self::$variant(items) => items.len() * size_of::<$ty>(),)*Self::Token(items) => items.len() * size_of::<TokenId>(), Self::String(items) => items.text_bytes() + items.len() * size_of::<usize>(), Self::Deferred(_) => self.try_materialize().map_or(0, Self::element_bytes) }
            }

            /// Number of elements that fit in the retained allocation.
            #[must_use]
            pub fn capacity(&self) -> usize {
                match self { $(Self::$variant(items) => items.capacity(),)*Self::Token(items) => items.len(), Self::String(items) => items.ends.capacity(), Self::Deferred(_) => self.try_materialize().map_or(0, Self::capacity) }
            }

            /// One element expressed as a scalar value, without expanding the buffer.
            /// A string element is copied into its own `Value::String`; borrow
            /// it with [`Self::as_string`] instead.
            #[must_use]
            pub fn get(&self, index: usize) -> Option<Value> {
                match self { $(Self::$variant(items) => items.get(index).copied().map($wrap),)*Self::Token(items) => items.get(index).copied().map(Value::Token), Self::String(items) => items.get(index).map(Value::string), Self::Deferred(_) => self.try_materialize().ok().and_then(|native| native.get(index)) }
            }

            /// A scalar value of this buffer's element kind. Values are zero
            /// (the empty token or string); this describes the kind, not a
            /// property's schema default.
            #[must_use]
            pub fn element_kind(&self) -> Value {
                match self { $(Self::$variant(_) => ($wrap)(<$ty>::default()),)*Self::Token(_) => Value::Token(TokenId::EMPTY), Self::String(_) => Value::string(""), Self::Deferred(source) => source.element_kind() }
            }

            /// Iterates scalar values on demand. Prefer native slices in hot loops.
            pub fn values(&self) -> impl ExactSizeIterator<Item = Value> + '_ {
                (0..self.len()).map(|i| self.get(i).expect("index is within buffer"))
            }

            $(#[doc = concat!("Borrows native `", stringify!($variant), "` elements, or returns `None` for another kind.")]
            #[must_use]
            pub fn $accessor(&self) -> Option<&[$ty]> {
                match self.try_materialize().ok()? { Self::$variant(items) => Some(items), _ => None }
            })*

            /// Borrows native `token[]` elements, or returns `None` for another kind.
            #[must_use]
            pub fn as_token(&self) -> Option<&[TokenId]> {
                match self.try_materialize().ok()? { Self::Token(items) => Some(items), _ => None }
            }

            /// Borrows native `string[]` elements, or returns `None` for another kind.
            #[must_use]
            pub fn as_string(&self) -> Option<&StringArray> {
                match self.try_materialize().ok()? { Self::String(items) => Some(items), _ => None }
            }

            /// Packs `values` into the buffer of `kind`'s scalar type, or
            /// into a heterogeneous [`Value::Array`] from the first element
            /// that does not fit. The iterator is type-erased so that one
            /// copy serves every caller: a generic copy is a match over
            /// every kind, about 13 KB of Wasm per caller. A virtual `next`
            /// per element is cheaper than the generic copy's code was
            /// (1M floats pack in 2.1 ms rather than 2.5 ms).
            pub(crate) fn pack_iter(kind: &Value, values: &mut dyn Iterator<Item = Value>) -> Value {
                match kind {
                    $(Value::$variant(_) => {
                        let mut items = Vec::<$ty>::with_capacity(values.size_hint().0);
                        for value in &mut *values {
                            if let Some(item) = ($read)(&value) { items.push(item); }
                            else { return mixed(Self::$variant(Arc::new(items)), value, values); }
                        }
                        Value::TypedArray(Self::$variant(Arc::new(items)))
                    },)*
                    Value::Token(_) => {
                        let mut items = Vec::<TokenId>::with_capacity(values.size_hint().0);
                        for value in &mut *values {
                            if let Value::Token(token) = value { items.push(token); }
                            else { return mixed(Self::Token(items.into()), value, values); }
                        }
                        Value::TypedArray(Self::Token(items.into()))
                    },
                    Value::String(_) => {
                        let mut items = StringArray::with_capacity(values.size_hint().0, 0);
                        for value in &mut *values {
                            if let Value::String(text) = &value { items.push(text); }
                            else { return mixed(Self::String(Arc::new(items)), value, values); }
                        }
                        Value::TypedArray(Self::String(Arc::new(items)))
                    },
                    _ => Value::Array(values.collect()),
                }
            }

            pub(crate) fn same(&self, other: &Self) -> bool {
                if let (Self::Deferred(a), Self::Deferred(b)) = (self, other)
                    && Arc::ptr_eq(a,b) { return true; }
                let (Ok(a), Ok(b)) = (self.try_materialize(), other.try_materialize()) else { return false; };
                match (a, b) {
                    $((Self::$variant(a), Self::$variant(b)) => {
                        Arc::ptr_eq(a,b) || (a.len() == b.len() && a.iter().zip(b.iter()).all(|(a,b)| ($same)(a,b)))
                    },)*
                    (Self::Token(a), Self::Token(b)) => Arc::ptr_eq(a, b) || a == b,
                    (Self::String(a), Self::String(b)) => Arc::ptr_eq(a, b) || a == b,
                    _ => false,
                }
            }

            pub(crate) fn apply_edit(&mut self, edit: &ArrayEdit, fill: Option<&Value>) -> bool {
                if matches!(self, Self::Deferred(_)) {
                    // Keep a failed selected source intact: a weaker value must
                    // never turn a decoding failure into apparently valid data.
                    let Ok(native) = self.try_materialize() else { return true; };
                    *self = native.clone();
                }
                #[cfg(target_family = "wasm")]
                return self.apply_edit_by_plan(edit, fill);
                #[cfg(not(target_family = "wasm"))]
                match self {
                    $(Self::$variant(items) => {
                        let Some(edit) = convert_edit(edit, $read) else { return false; };
                        let fill = match fill {
                            Some(value) => {
                                let Some(value) = ($read)(value) else { return false; };
                                Some(value)
                            }
                            None => None,
                        };
                        if !edit.is_identity() { edit.apply_in_place(Arc::make_mut(items), fill); }
                        true
                    },)*
                    Self::Token(items) => {
                        let token = |value: &Value| if let Value::Token(token) = value { Some(*token) } else { None };
                        let Some(edit) = convert_edit(edit, token) else { return false; };
                        let fill = match fill {
                            Some(value) => {
                                let Some(value) = token(value) else { return false; };
                                Some(value)
                            }
                            None => None,
                        };
                        if !edit.is_identity() {
                            // The buffer has no spare room: edit a copy, then share it.
                            let mut edited = items.to_vec();
                            edit.apply_in_place(&mut edited, fill);
                            *items = edited.into();
                        }
                        true
                    },
                    Self::String(items) => {
                        let Some(edit) = convert_edit(edit, string_element) else { return false; };
                        let fill = match fill {
                            Some(value) => {
                                let Some(value) = string_element(value) else { return false; };
                                Some(value)
                            }
                            None => None,
                        };
                        if !edit.is_identity() {
                            // Edit borrowed elements, then gather them into one new buffer.
                            let mut elements: Vec<&str> = items.iter().collect();
                            edit.apply_in_place(&mut elements, fill);
                            let edited: StringArray = elements.into_iter().collect();
                            *items = Arc::new(edited);
                        }
                        true
                    },
                    Self::Deferred(_) => unreachable!("materialized above"),
                }
            }

            /// [`Self::apply_edit`] on WebAssembly: the edit runs once over
            /// an [`EditPlan`] of element sources, and each element kind
            /// only gathers its elements from the plan, so the instruction
            /// interpreter is compiled once rather than for every kind.
            /// The result is the per-kind interpreter's, and a uniquely
            /// owned buffer keeps its allocation, as there.
            #[cfg(any(target_family = "wasm", test))]
            fn apply_edit_by_plan(&mut self, edit: &ArrayEdit, fill: Option<&Value>) -> bool {
                match self {
                    $(Self::$variant(items) => {
                        if !edit_reads(edit, fill, &|value| ($read)(value).is_some()) { return false; }
                        let Some(plan) = EditPlan::new(edit, items.len(), fill) else { return true; };
                        let edited: Vec<$ty> = plan.gather(|at| items[at], |value| ($read)(value));
                        match Arc::get_mut(items) {
                            Some(unique) => {
                                unique.clear();
                                unique.extend_from_slice(&edited);
                            }
                            None => *items = Arc::new(edited),
                        }
                        true
                    },)*
                    Self::Token(items) => {
                        let token = |value: &Value| if let Value::Token(token) = value { Some(*token) } else { None };
                        if !edit_reads(edit, fill, &|value| token(value).is_some()) { return false; }
                        let Some(plan) = EditPlan::new(edit, items.len(), fill) else { return true; };
                        let edited: Vec<TokenId> = plan.gather(|at| items[at], token);
                        *items = edited.into();
                        true
                    },
                    Self::String(items) => {
                        if !edit_reads(edit, fill, &|value| string_element(value).is_some()) { return false; }
                        let Some(plan) = EditPlan::new(edit, items.len(), fill) else { return true; };
                        let edited: Vec<&str> = plan.gather(|at| items.get(at).expect("a planned element is in the array"), string_element);
                        let edited: StringArray = edited.into_iter().collect();
                        *items = Arc::new(edited);
                        true
                    },
                    Self::Deferred(_) => unreachable!("materialized above"),
                }
            }
        }
    };
}

array_types! {
    Bool, bool, as_bool, Value::Bool, |v: &Value| if let Value::Bool(v) = v { Some(*v) } else { None }, |a, b| a == b;
    UChar, u8, as_uchar, Value::UChar, |v: &Value| if let Value::UChar(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Int, i32, as_int, Value::Int, |v: &Value| if let Value::Int(v) = v { Some(*v) } else { None }, |a, b| a == b;
    UInt, u32, as_uint, Value::UInt, |v: &Value| if let Value::UInt(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Int64, i64, as_int64, Value::Int64, |v: &Value| if let Value::Int64(v) = v { Some(*v) } else { None }, |a, b| a == b;
    UInt64, u64, as_uint64, Value::UInt64, |v: &Value| if let Value::UInt64(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Half, u16, as_half, Value::Half, |v: &Value| if let Value::Half(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Float, f32, as_float, Value::Float, |v: &Value| if let Value::Float(v) = v { Some(*v) } else { None }, |a: &f32, b: &f32| a.to_bits() == b.to_bits();
    Double, f64, as_double, Value::Double, |v: &Value| if let Value::Double(v) = v { Some(*v) } else { None }, |a: &f64, b: &f64| a.to_bits() == b.to_bits();
    TimeCode, f64, as_timecode, Value::TimeCode, |v: &Value| if let Value::TimeCode(v) = v { Some(*v) } else { None }, |a: &f64, b: &f64| a.to_bits() == b.to_bits();
    Duration, f64, as_duration, Value::Duration, |v: &Value| if let Value::Duration(v) = v { Some(*v) } else { None }, |a: &f64, b: &f64| a.to_bits() == b.to_bits();
    Vec2f, [f32; 2], as_vec2f, Value::Vec2f, |v: &Value| if let Value::Vec2f(v) = v { Some(*v) } else { None }, |a: &[f32; 2], b: &[f32; 2]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Vec3f, [f32; 3], as_vec3f, Value::Vec3f, |v: &Value| if let Value::Vec3f(v) = v { Some(*v) } else { None }, |a: &[f32; 3], b: &[f32; 3]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Vec4f, [f32; 4], as_vec4f, Value::Vec4f, |v: &Value| if let Value::Vec4f(v) = v { Some(*v) } else { None }, |a: &[f32; 4], b: &[f32; 4]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Vec2d, [f64; 2], as_vec2d, Value::Vec2d, |v: &Value| if let Value::Vec2d(v) = v { Some(*v) } else { None }, |a: &[f64; 2], b: &[f64; 2]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Vec3d, [f64; 3], as_vec3d, Value::Vec3d, |v: &Value| if let Value::Vec3d(v) = v { Some(*v) } else { None }, |a: &[f64; 3], b: &[f64; 3]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Vec4d, [f64; 4], as_vec4d, Value::Vec4d, |v: &Value| if let Value::Vec4d(v) = v { Some(*v) } else { None }, |a: &[f64; 4], b: &[f64; 4]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Vec2h, [u16; 2], as_vec2h, Value::Vec2h, |v: &Value| if let Value::Vec2h(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Vec3h, [u16; 3], as_vec3h, Value::Vec3h, |v: &Value| if let Value::Vec3h(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Vec4h, [u16; 4], as_vec4h, Value::Vec4h, |v: &Value| if let Value::Vec4h(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Vec2i, [i32; 2], as_vec2i, Value::Vec2i, |v: &Value| if let Value::Vec2i(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Vec3i, [i32; 3], as_vec3i, Value::Vec3i, |v: &Value| if let Value::Vec3i(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Vec4i, [i32; 4], as_vec4i, Value::Vec4i, |v: &Value| if let Value::Vec4i(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Quatf, [f32; 4], as_quatf, Value::Quatf, |v: &Value| if let Value::Quatf(v) = v { Some(*v) } else { None }, |a: &[f32; 4], b: &[f32; 4]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Quatd, [f64; 4], as_quatd, Value::Quatd, |v: &Value| if let Value::Quatd(v) = v { Some(*v) } else { None }, |a: &[f64; 4], b: &[f64; 4]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Quath, [u16; 4], as_quath, Value::Quath, |v: &Value| if let Value::Quath(v) = v { Some(*v) } else { None }, |a, b| a == b;
    Matrix2d, [f64; 4], as_matrix2d, |v| Value::Matrix2d(Box::new(v)), |v: &Value| if let Value::Matrix2d(v) = v { Some(**v) } else { None }, |a: &[f64; 4], b: &[f64; 4]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Matrix3d, [f64; 9], as_matrix3d, |v| Value::Matrix3d(Box::new(v)), |v: &Value| if let Value::Matrix3d(v) = v { Some(**v) } else { None }, |a: &[f64; 9], b: &[f64; 9]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
    Matrix4d, [f64; 16], as_matrix4d, |v| Value::Matrix4d(Box::new(v)), |v: &Value| if let Value::Matrix4d(v) = v { Some(**v) } else { None }, |a: &[f64; 16], b: &[f64; 16]| a.iter().zip(b).all(|(a,b)| a.to_bits() == b.to_bits());
}

macro_rules! native_arrays {
    ($($ty:ty => $kind:ident),* $(,)?) => {
        $(impl From<Vec<$ty>> for TypedArray {
            fn from(items: Vec<$ty>) -> Self { Self::$kind(Arc::new(items)) }
        }
        impl From<Vec<$ty>> for Value {
            fn from(items: Vec<$ty>) -> Self { Self::TypedArray(items.into()) }
        })*
    };
}

// Rust element types select canonical USD scalar/vector kinds. Quaternions,
// time codes and matrix2d share a Rust element type with another kind and use
// explicit variants instead, so conversion never guesses those meanings.
native_arrays! {
    bool => Bool, u8 => UChar, i32 => Int, u32 => UInt,
    i64 => Int64, u64 => UInt64, u16 => Half, f32 => Float, f64 => Double,
    [f32; 2] => Vec2f, [f32; 3] => Vec3f, [f32; 4] => Vec4f,
    [f64; 2] => Vec2d, [f64; 3] => Vec3d, [f64; 4] => Vec4d,
    [u16; 2] => Vec2h, [u16; 3] => Vec3h, [u16; 4] => Vec4h,
    [i32; 2] => Vec2i, [i32; 3] => Vec3i, [i32; 4] => Vec4i,
    [f64; 9] => Matrix3d, [f64; 16] => Matrix4d,
}

impl From<Vec<TokenId>> for TypedArray {
    fn from(items: Vec<TokenId>) -> Self {
        Self::Token(items.into())
    }
}

impl From<Vec<TokenId>> for Value {
    fn from(items: Vec<TokenId>) -> Self {
        Self::TypedArray(items.into())
    }
}

impl From<StringArray> for TypedArray {
    fn from(items: StringArray) -> Self {
        Self::String(Arc::new(items))
    }
}

impl From<StringArray> for Value {
    fn from(items: StringArray) -> Self {
        Self::TypedArray(items.into())
    }
}

/// `string[]` elements held in one text buffer, without an allocation or a
/// [`Value`] per element.
///
/// Element `i` is the text between the end of element `i - 1` and its own
/// end. Building one from `&str` elements copies their text once; reading
/// one borrows it. Two arrays are equal when their elements are.
///
/// Spec: AOUSD Core §6.2 (the `string` value type and its arrays).
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct StringArray {
    text: String,
    /// The end of each element in `text`.
    ends: Vec<usize>,
}

impl StringArray {
    /// An empty array.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text: String::new(),
            ends: Vec::new(),
        }
    }

    /// An empty array with room for `elements` elements of `text_bytes`
    /// bytes of text in all.
    #[must_use]
    pub fn with_capacity(elements: usize, text_bytes: usize) -> Self {
        Self {
            text: String::with_capacity(text_bytes),
            ends: Vec::with_capacity(elements),
        }
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether the array has no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Bytes of text in all the elements.
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        self.text.len()
    }

    /// Borrows element `index`, or `None` past the end.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&str> {
        let end = *self.ends.get(index)?;
        let start = index.checked_sub(1).map_or(0, |before| self.ends[before]);
        Some(&self.text[start..end])
    }

    /// Borrows the elements in order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> + DoubleEndedIterator + '_ {
        (0..self.len()).map(|index| self.get(index).expect("index is within the array"))
    }

    /// Appends one element, copying its text.
    pub fn push(&mut self, element: &str) {
        self.text.push_str(element);
        self.ends.push(self.text.len());
    }
}

impl fmt::Debug for StringArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<S: AsRef<str>> FromIterator<S> for StringArray {
    fn from_iter<I: IntoIterator<Item = S>>(elements: I) -> Self {
        let mut items = Self::new();
        items.extend(elements);
        items
    }
}

impl<S: AsRef<str>> Extend<S> for StringArray {
    fn extend<I: IntoIterator<Item = S>>(&mut self, elements: I) {
        let elements = elements.into_iter();
        self.ends.reserve(elements.size_hint().0);
        for element in elements {
            self.push(element.as_ref());
        }
    }
}

/// The heterogeneous array of `packed`'s elements, `value` and the rest of
/// `values`: what [`TypedArray::pack_iter`] returns from the first element
/// that does not fit. One function for every kind, as the elements already
/// packed read back through [`TypedArray::values`].
fn mixed(packed: TypedArray, value: Value, values: &mut dyn Iterator<Item = Value>) -> Value {
    let mut mixed: Vec<Value> = Vec::with_capacity(packed.len() + 1 + values.size_hint().0);
    mixed.extend(packed.values());
    mixed.push(value);
    mixed.extend(values);
    Value::Array(mixed)
}

/// Whether every value `edit` holds (its literals and sized fills) and
/// `fill` read as the element kind `reads` accepts: the per-kind
/// interpreter's conversion succeeds exactly then.
#[cfg(any(target_family = "wasm", test))]
fn edit_reads(edit: &ArrayEdit, fill: Option<&Value>, reads: &dyn Fn(&Value) -> bool) -> bool {
    fill.is_none_or(reads)
        && edit.ops.iter().all(|op| match op {
            ArrayEditOp::Write { src, .. } | ArrayEditOp::Insert { src, .. } => match src {
                ArrayEditOperand::Literal(value) => reads(value),
                ArrayEditOperand::CopyFrom(_) => true,
            },
            ArrayEditOp::MinSizeFill { fill, .. } | ArrayEditOp::ResizeFill { fill, .. } => {
                reads(fill)
            }
            ArrayEditOp::Erase { .. }
            | ArrayEditOp::MinSize { .. }
            | ArrayEditOp::MaxSize { .. }
            | ArrayEditOp::Resize { .. } => true,
        })
}

/// Where one element of an edited array comes from.
#[cfg(any(target_family = "wasm", test))]
#[derive(Clone, Copy)]
enum Source<'e> {
    /// The element at this position of the array before the edit.
    Kept(usize),
    /// A value the edit holds: a literal, a sized fill or the fill.
    Held(&'e Value),
}

/// An array edit run over the positions of an array rather than its
/// elements: the edited array's elements, each as its [`Source`].
#[cfg(any(target_family = "wasm", test))]
struct EditPlan<'e>(Vec<Source<'e>>);

#[cfg(any(target_family = "wasm", test))]
impl<'e> EditPlan<'e> {
    /// Runs `edit` over an array of `len` elements, growing with `fill`;
    /// `None` for an edit with no instructions, which changes nothing.
    fn new(edit: &'e ArrayEdit, len: usize, fill: Option<&'e Value>) -> Option<Self> {
        use opinionated::{ArrayEditOp as Op, ArrayEditOperand as Operand};
        if edit.is_identity() {
            return None;
        }
        let operand = |src: &'e ArrayEditOperand| match src {
            ArrayEditOperand::Literal(value) => Operand::Literal(Source::Held(value)),
            ArrayEditOperand::CopyFrom(index) => Operand::CopyFrom(*index),
        };
        let ops = edit
            .ops
            .iter()
            .map(|op| match op {
                ArrayEditOp::Write { src, index } => Op::Write {
                    src: operand(src),
                    index: *index,
                },
                ArrayEditOp::Insert { src, index } => Op::Insert {
                    src: operand(src),
                    index: *index,
                },
                ArrayEditOp::Erase { index } => Op::Erase { index: *index },
                ArrayEditOp::MinSize { len } => Op::MinSize { len: *len },
                ArrayEditOp::MaxSize { len } => Op::MaxSize { len: *len },
                ArrayEditOp::Resize { len } => Op::Resize { len: *len },
                ArrayEditOp::MinSizeFill { len, fill } => Op::MinSizeFill {
                    len: *len,
                    fill: Source::Held(fill),
                },
                ArrayEditOp::ResizeFill { len, fill } => Op::ResizeFill {
                    len: *len,
                    fill: Source::Held(fill),
                },
            })
            .collect();
        let mut sources: Vec<Source<'e>> = (0..len).map(Source::Kept).collect();
        opinionated::ArrayEdit { ops }.apply_in_place(&mut sources, fill.map(Source::Held));
        Some(Self(sources))
    }

    /// The edited elements: `kept` reads an element of the array before the
    /// edit, `read` one the edit holds, which [`edit_reads`] checked.
    fn gather<T>(
        &self,
        kept: impl Fn(usize) -> T,
        read: impl Fn(&'e Value) -> Option<T>,
    ) -> Vec<T> {
        self.0
            .iter()
            .map(|source| match *source {
                Source::Kept(at) => kept(at),
                Source::Held(value) => read(value).expect("checked by `edit_reads`"),
            })
            .collect()
    }
}

#[cfg(not(target_family = "wasm"))]
fn convert_edit<'e, T: Copy>(
    edit: &'e ArrayEdit,
    read: impl Fn(&'e Value) -> Option<T>,
) -> Option<opinionated::ArrayEdit<T>> {
    use ArrayEditOp as Op;
    let operand = |src: &'e ArrayEditOperand| match src {
        ArrayEditOperand::Literal(v) => Some(opinionated::ArrayEditOperand::Literal(read(v)?)),
        ArrayEditOperand::CopyFrom(index) => Some(opinionated::ArrayEditOperand::CopyFrom(*index)),
    };
    let ops = edit
        .ops
        .iter()
        .map(|op| {
            Some(match op {
                Op::Write { src, index } => opinionated::ArrayEditOp::Write {
                    src: operand(src)?,
                    index: *index,
                },
                Op::Insert { src, index } => opinionated::ArrayEditOp::Insert {
                    src: operand(src)?,
                    index: *index,
                },
                Op::Erase { index } => opinionated::ArrayEditOp::Erase { index: *index },
                Op::MinSize { len } => opinionated::ArrayEditOp::MinSize { len: *len },
                Op::MaxSize { len } => opinionated::ArrayEditOp::MaxSize { len: *len },
                Op::Resize { len } => opinionated::ArrayEditOp::Resize { len: *len },
                Op::MinSizeFill { len, fill } => opinionated::ArrayEditOp::MinSizeFill {
                    len: *len,
                    fill: read(fill)?,
                },
                Op::ResizeFill { len, fill } => opinionated::ArrayEditOp::ResizeFill {
                    len: *len,
                    fill: read(fill)?,
                },
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(opinionated::ArrayEdit { ops })
}

/// The text of a `string` value.
fn string_element(value: &Value) -> Option<&str> {
    match value {
        Value::String(text) => Some(text),
        _ => None,
    }
}

/// A borrowed array view, backed by either heterogeneous values or a native buffer.
///
/// The view never copies the whole array. `get` and `iter` borrow heterogeneous
/// elements and construct individual native scalar values on demand. Use
/// `typed` and the buffer's native slice accessors for allocation-free numeric loops.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum ArrayRef<'a> {
    /// Heterogeneous or legacy scalar values.
    Values(&'a [Value]),
    /// Native homogeneous values.
    Typed(&'a TypedArray),
}

impl<'a> ArrayRef<'a> {
    /// Number of elements.
    #[must_use]
    pub fn len(self) -> usize {
        match self {
            Self::Values(values) => values.len(),
            Self::Typed(values) => values.len(),
        }
    }

    /// Whether this array is empty.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Returns the homogeneous native buffer, when present.
    #[must_use]
    pub fn typed(self) -> Option<&'a TypedArray> {
        match self {
            Self::Typed(values) => Some(values),
            Self::Values(_) => None,
        }
    }

    /// Borrows a legacy element or constructs one scalar from a native buffer.
    #[must_use]
    pub fn get(self, index: usize) -> Option<Cow<'a, Value>> {
        match self {
            Self::Values(values) => values.get(index).map(Cow::Borrowed),
            Self::Typed(values) => values.get(index).map(Cow::Owned),
        }
    }

    /// Iterates values without expanding the array into a temporary vector.
    pub fn iter(self) -> impl ExactSizeIterator<Item = Cow<'a, Value>> {
        (0..self.len()).map(move |i| self.get(i).expect("index is within array"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArrayIndex, PropertyType};
    use alloc::vec;

    #[test]
    fn shared_storage_comparison_distinguishes_owners_from_equal_contents() {
        let array = TypedArray::Int(Arc::new(vec![1, 2, 3]));
        assert!(array.shares_storage(&array.clone()));
        assert!(!array.shares_storage(&TypedArray::Int(Arc::new(vec![1, 2, 3]))));
        assert!(!array.shares_storage(&TypedArray::Float(Arc::new(vec![1., 2., 3.]))));
    }

    /// Every short program over a pool of instructions edits each kind of
    /// buffer the same through the plan (WebAssembly's path) as through
    /// the per-kind interpreter, and both refuse the same mismatched
    /// literals and fills.
    #[test]
    fn the_edit_plan_matches_the_per_kind_interpreter() {
        use ArrayEditOp as Op;
        use ArrayEditOperand as Operand;
        let kinds: [(TypedArray, Value, Value); 4] = [
            (
                TypedArray::Int(Arc::new(vec![1, 2, 3])),
                Value::Int(7),
                Value::Float(0.5),
            ),
            (
                TypedArray::Vec3f(Arc::new(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]])),
                Value::Vec3f([7.0, 8.0, 9.0]),
                Value::Int(1),
            ),
            (
                TypedArray::Token(vec![TokenId::EMPTY; 2].into()),
                Value::Token(TokenId::EMPTY),
                Value::string("text"),
            ),
            (
                TypedArray::String(Arc::new(["a", "b", "c", "d"].into_iter().collect())),
                Value::string("x"),
                Value::Token(TokenId::EMPTY),
            ),
        ];
        for (array, literal, other) in kinds {
            let indices = [
                ArrayIndex::Position(0),
                ArrayIndex::Position(-1),
                ArrayIndex::Position(2),
                ArrayIndex::Position(9),
                ArrayIndex::End,
            ];
            let mut pool = vec![
                Op::Erase {
                    index: ArrayIndex::Position(1),
                },
                Op::MinSize { len: 6 },
                Op::MinSizeFill {
                    len: 5,
                    fill: literal.clone(),
                },
                Op::MaxSize { len: 2 },
                Op::Resize { len: 4 },
                Op::ResizeFill {
                    len: 1,
                    fill: literal.clone(),
                },
                Op::Write {
                    src: Operand::Literal(other.clone()),
                    index: ArrayIndex::Position(0),
                },
            ];
            for index in indices {
                pool.push(Op::Write {
                    src: Operand::Literal(literal.clone()),
                    index,
                });
                pool.push(Op::Insert {
                    src: Operand::CopyFrom(index),
                    index: ArrayIndex::Position(1),
                });
                pool.push(Op::Insert {
                    src: Operand::Literal(literal.clone()),
                    index,
                });
                pool.push(Op::Write {
                    src: Operand::CopyFrom(index),
                    index: ArrayIndex::Position(0),
                });
            }
            let mut programs: Vec<Vec<ArrayEditOp>> = vec![Vec::new()];
            for a in &pool {
                programs.push(vec![a.clone()]);
                for b in &pool {
                    programs.push(vec![a.clone(), b.clone()]);
                    programs.push(vec![b.clone(), a.clone(), b.clone()]);
                }
            }
            for ops in programs {
                let edit = ArrayEdit { ops };
                for fill in [None, Some(&literal), Some(&other)] {
                    let mut by_kind = array.clone();
                    let mut by_plan = array.clone();
                    let kind_applied = by_kind.apply_edit(&edit, fill);
                    let plan_applied = by_plan.apply_edit_by_plan(&edit, fill);
                    assert_eq!(kind_applied, plan_applied, "{edit:?} with {fill:?}");
                    assert_eq!(by_kind, by_plan, "{edit:?} with {fill:?}");
                }
            }
        }
    }

    /// The plan keeps a uniquely owned buffer's allocation, as the
    /// per-kind interpreter does.
    #[test]
    fn the_edit_plan_keeps_a_unique_allocation() {
        let mut array = TypedArray::Vec3f(Arc::new(vec![[1.0, 2.0, 3.0]; 8]));
        let TypedArray::Vec3f(items) = &array else {
            unreachable!()
        };
        let allocation = items.as_ptr();
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Vec3f([4.0, 5.0, 6.0])),
                index: ArrayIndex::Position(0),
            }],
        };
        assert!(array.apply_edit_by_plan(&edit, None));
        let TypedArray::Vec3f(items) = &array else {
            unreachable!()
        };
        assert_eq!(items.as_ptr(), allocation);
        assert_eq!(items[0], [4.0, 5.0, 6.0]);
    }

    #[test]
    fn native_points_share_until_an_edit_needs_unique_ownership() {
        let points = Arc::new(vec![[1.0_f32, 2.0, 3.0]; 1_000]);
        let mut value = Value::TypedArray(TypedArray::Vec3f(points.clone()));
        let snapshot = value.clone();
        let Value::TypedArray(TypedArray::Vec3f(shared)) = &snapshot else {
            panic!("native points");
        };
        assert!(Arc::ptr_eq(&points, shared));
        assert_eq!(
            value.array_ref().unwrap().typed().unwrap().element_bytes(),
            12_000
        );
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Vec3f([4.0, 5.0, 6.0])),
                index: ArrayIndex::Position(0),
            }],
        };
        assert!(crate::array_edit::apply_in_place(&edit, &mut value, None));
        let Value::TypedArray(TypedArray::Vec3f(edited)) = &value else {
            panic!("native result");
        };
        assert!(!Arc::ptr_eq(&points, edited));
        assert_eq!(points[0], [1.0, 2.0, 3.0]);
        assert_eq!(edited[0], [4.0, 5.0, 6.0]);
        let allocation = edited.as_ptr();
        assert!(crate::array_edit::apply_in_place(&edit, &mut value, None));
        let Value::TypedArray(TypedArray::Vec3f(edited)) = &value else {
            panic!("native result");
        };
        assert_eq!(
            allocation,
            edited.as_ptr(),
            "a unique buffer retains its allocation"
        );
    }

    #[test]
    fn empty_native_kinds_and_float_bits_survive() {
        let empty_float = Value::array_with_element(vec![], Some(&Value::Float(0.0)));
        let empty_int = Value::array_with_element(vec![], Some(&Value::Int(0)));
        let unknown = Value::Array(vec![]);
        assert_eq!(empty_float, unknown);
        assert_eq!(unknown, empty_int);
        assert_eq!(
            empty_float, empty_int,
            "empty content equality is transitive"
        );
        assert!(!empty_float.same_representation(&unknown));
        assert!(!unknown.same_representation(&empty_int));
        assert!(!empty_float.same_representation(&empty_int));
        let nan = f32::from_bits(0x7fc0_0123);
        let value = Value::array(vec![Value::Float(nan), Value::Float(-0.0)]);
        assert_ne!(
            value,
            value.clone(),
            "semantic equality preserves NaN behavior"
        );
        assert!(value.same_representation(&value.clone()));
        let legacy = Value::Array(vec![Value::Float(nan), Value::Float(-0.0)]);
        assert!(value.same_representation(&legacy));
        let positive_zero = Value::array(vec![Value::Float(nan), Value::Float(0.0)]);
        assert!(!value.same_representation(&positive_zero));
    }

    #[test]
    fn typed_edits_match_the_generic_interpreter_and_preserve_fill() {
        let generic = Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let native = Value::array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let edit = ArrayEdit {
            ops: vec![
                ArrayEditOp::Insert {
                    src: ArrayEditOperand::CopyFrom(ArrayIndex::Position(-1)),
                    index: ArrayIndex::Position(0),
                },
                ArrayEditOp::Write {
                    src: ArrayEditOperand::Literal(Value::Int(7)),
                    index: ArrayIndex::Position(-1),
                },
                ArrayEditOp::Erase {
                    index: ArrayIndex::Position(1),
                },
                ArrayEditOp::MinSize { len: 5 },
                ArrayEditOp::ResizeFill {
                    len: 6,
                    fill: Value::Int(9),
                },
                ArrayEditOp::MaxSize { len: 5 },
            ],
        };
        let ty = PropertyType::parse("int[]").unwrap();
        let expected = crate::array_edit::apply_to_value(&edit, &generic, Some(&ty));
        let actual = crate::array_edit::apply_to_value(&edit, &native, Some(&ty));
        assert_eq!(actual, expected);
        assert!(matches!(
            actual,
            Some(Value::TypedArray(TypedArray::Int(_)))
        ));
    }

    /// Packing falls back to a heterogeneous array at the first element of
    /// another kind, keeping every element before it, it and the rest, in
    /// order, for numeric, token and string buffers alike.
    #[test]
    fn packing_falls_back_to_every_element_in_order() {
        let cases = [
            vec![
                Value::Vec2f([1.0, 2.0]),
                Value::Vec2f([3.0, 4.0]),
                Value::Int(5),
                Value::Vec2f([6.0, 7.0]),
            ],
            vec![
                Value::Token(TokenId::EMPTY),
                Value::string("a"),
                Value::Token(TokenId::EMPTY),
            ],
            vec![Value::string("a"), Value::string("b"), Value::Bool(true)],
        ];
        for elements in cases {
            let Value::Array(packed) = Value::array_from_iter(elements.clone(), None) else {
                panic!("a heterogeneous array");
            };
            assert_eq!(packed, elements);
        }
    }

    #[test]
    fn mixed_literal_edits_keep_values_instead_of_coercing() {
        let value = Value::array(vec![Value::Int(1), Value::Int(2)]);
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Float(1.5)),
                index: ArrayIndex::End,
            }],
        };
        assert_eq!(
            crate::array_edit::apply_to_value(&edit, &value, None),
            Some(Value::Array(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Float(1.5)
            ]))
        );
    }

    fn tokens(store: &crate::TokenInterner, names: &[&str]) -> Vec<TokenId> {
        names.iter().map(|name| store.intern(name)).collect()
    }

    #[test]
    fn token_arrays_hold_ids_and_share_until_edited() {
        let interner = crate::TokenInterner::default();
        let ids = tokens(&interner, &["a", "b", "c"]);
        let value = Value::from(ids.clone());
        let legacy = Value::Array(ids.iter().copied().map(Value::Token).collect());
        assert_eq!(value, legacy, "content equality spans representations");
        assert!(value.same_representation(&legacy));
        let array = value.array_ref().unwrap().typed().unwrap();
        assert_eq!(array.as_token(), Some(&ids[..]));
        assert_eq!(array.element_bytes(), 12, "four bytes per element");
        assert_eq!(array.element_kind(), Value::Token(TokenId::EMPTY));
        assert_eq!(array.get(1), Some(Value::Token(ids[1])));
        let mut edited = value.clone();
        let edit = ArrayEdit {
            ops: vec![
                ArrayEditOp::Erase {
                    index: ArrayIndex::Position(0),
                },
                ArrayEditOp::Insert {
                    src: ArrayEditOperand::Literal(Value::Token(ids[0])),
                    index: ArrayIndex::End,
                },
                ArrayEditOp::Resize { len: 4 },
            ],
        };
        let ty = PropertyType::parse("token[]").unwrap();
        assert!(crate::array_edit::apply_in_place(
            &edit,
            &mut edited,
            Some(&ty)
        ));
        let expected = crate::array_edit::apply_to_value(&edit, &legacy, Some(&ty)).unwrap();
        assert_eq!(
            edited, expected,
            "native edits match the generic interpreter"
        );
        let Value::TypedArray(TypedArray::Token(edited)) = &edited else {
            panic!("native token result");
        };
        assert_eq!(edited[..], [ids[1], ids[2], ids[0], TokenId::EMPTY]);
        assert_eq!(edited.len(), 4, "one exact allocation");
        assert_eq!(
            value.array_ref().unwrap().len(),
            3,
            "the snapshot is unchanged"
        );
    }

    #[test]
    fn string_arrays_keep_one_text_buffer() {
        let items: StringArray = ["", "two", "drei", "\u{1f600}"].into_iter().collect();
        assert_eq!(items.len(), 4);
        assert_eq!(items.text_bytes(), 11);
        assert_eq!(items.get(0), Some(""));
        assert_eq!(items.get(3), Some("\u{1f600}"));
        assert_eq!(items.get(4), None);
        assert_eq!(
            items.iter().rev().collect::<Vec<_>>(),
            ["\u{1f600}", "drei", "two", ""]
        );
        assert_eq!(
            alloc::format!("{items:?}"),
            alloc::format!("{:?}", ["", "two", "drei", "\u{1f600}"])
        );
        let value = Value::from(items.clone());
        let legacy = Value::Array(items.iter().map(Value::string).collect());
        assert_eq!(value, legacy);
        assert!(value.same_representation(&legacy));
        assert!(value.same_representation(&value.clone()));
        let array = value.array_ref().unwrap().typed().unwrap();
        assert_eq!(array.as_string(), Some(&items));
        assert_eq!(array.get(2), Some(Value::string("drei")));
        assert_eq!(array.element_kind(), Value::string(""));
        assert_eq!(alloc::format!("{value}"), "[, two, drei, \u{1f600}]");
        assert_ne!(
            value,
            Value::from(
                ["", "two", "dre", "i\u{1f600}"]
                    .into_iter()
                    .collect::<StringArray>()
            ),
            "equal text split differently is a different array"
        );
    }

    #[test]
    fn string_edits_match_the_generic_interpreter() {
        let items: StringArray = ["a", "b", "c"].into_iter().collect();
        let native = Value::from(items.clone());
        let legacy = Value::Array(items.iter().map(Value::string).collect());
        let edit = ArrayEdit {
            ops: vec![
                ArrayEditOp::Write {
                    src: ArrayEditOperand::Literal(Value::string("z")),
                    index: ArrayIndex::Position(1),
                },
                ArrayEditOp::Insert {
                    src: ArrayEditOperand::CopyFrom(ArrayIndex::Position(-1)),
                    index: ArrayIndex::Position(0),
                },
                ArrayEditOp::Erase {
                    index: ArrayIndex::Position(-1),
                },
                ArrayEditOp::ResizeFill {
                    len: 5,
                    fill: Value::string("fill"),
                },
                ArrayEditOp::Resize { len: 6 },
            ],
        };
        let ty = PropertyType::parse("string[]").unwrap();
        let expected = crate::array_edit::apply_to_value(&edit, &legacy, Some(&ty)).unwrap();
        let actual = crate::array_edit::apply_to_value(&edit, &native, Some(&ty)).unwrap();
        assert_eq!(actual, expected);
        let Value::TypedArray(TypedArray::String(edited)) = &actual else {
            panic!("native string result");
        };
        assert_eq!(
            edited.iter().collect::<Vec<_>>(),
            ["c", "a", "z", "fill", "fill", ""]
        );
        assert_eq!(native, legacy, "the edited source is unchanged");
        // A token literal does not coerce into a string array.
        let token = ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Token(TokenId::EMPTY)),
                index: ArrayIndex::End,
            }],
        };
        assert_eq!(
            crate::array_edit::apply_to_value(&token, &native, None),
            crate::array_edit::apply_to_value(&token, &legacy, None)
        );
    }

    #[test]
    fn incompatible_property_fill_keeps_heterogeneous_growth() {
        let legacy = Value::Array(vec![Value::Int(1)]);
        let native = Value::from(vec![1_i32]);
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::Resize { len: 2 }],
        };
        let wrong_type = PropertyType::parse("float[]").unwrap();
        let expected = crate::array_edit::apply_to_value(&edit, &legacy, Some(&wrong_type));
        let actual = crate::array_edit::apply_to_value(&edit, &native, Some(&wrong_type));
        assert_eq!(actual, expected);
        assert_eq!(
            actual,
            Some(Value::Array(vec![Value::Int(1), Value::Float(0.0)]))
        );
    }
}
