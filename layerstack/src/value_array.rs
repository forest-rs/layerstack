// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared homogeneous numeric buffers, independent of USD type aliases.
//!
//! Each buffer keeps native elements together. Cloning shares the buffer;
//! editing calls `Arc::make_mut`, retaining its allocation when uniquely
//! owned and copying once when another value still owns the old snapshot.
//! `Arc<Vec<T>>` deliberately retains spare capacity for sparse insertions
//! and removals. Typed slices borrow the native elements without expanding
//! them into `Value` records. Type aliases remain on `PropertyType`.

use alloc::{borrow::Cow, boxed::Box, sync::Arc, vec::Vec};
use core::mem::size_of;

use crate::{ArrayEdit, ArrayEditOp, ArrayEditOperand, Value};

/// A retained numeric source could not materialize its immutable buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
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
impl core::fmt::Display for ArrayReadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
pub trait DeferredArraySource: core::fmt::Debug + Send + Sync {
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
        /// matrix elements are flat row-major arrays. A mutable buffer is obtained
        /// with `Arc::make_mut` on a variant's payload.
        ///
        /// Spec: AOUSD Core §6.2–§6.3 (scalar and dimensioned array values).
        #[derive(Clone, Debug)]
        pub enum TypedArray {
            $(#[doc = concat!("Native `", stringify!($variant), "` elements.")]
            $variant(Arc<Vec<$ty>>),)*
            /// An immutable adapter source; use `try_materialize` for errors.
            Deferred(Arc<dyn DeferredArraySource>),
        }
        impl PartialEq for TypedArray {
            fn eq(&self, other: &Self) -> bool {
                let (Ok(a), Ok(b)) = (self.try_materialize(), other.try_materialize()) else { return false; };
                match (a,b) {
                    $((Self::$variant(a), Self::$variant(b)) => a == b,)*
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
                match self { $(Self::$variant(items) => items.len(),)*Self::Deferred(_) => self.try_materialize().map_or(0, Self::len) }
            }

            /// Whether the buffer contains no elements.
            #[must_use]
            pub fn is_empty(&self) -> bool { self.len() == 0 }

            /// Bytes occupied by initialized native elements, excluding capacity
            /// and the small shared allocation header.
            #[must_use]
            pub fn element_bytes(&self) -> usize {
                match self { $(Self::$variant(items) => items.len() * size_of::<$ty>(),)*Self::Deferred(_) => self.try_materialize().map_or(0, Self::element_bytes) }
            }

            /// Number of elements that fit in the retained allocation.
            #[must_use]
            pub fn capacity(&self) -> usize {
                match self { $(Self::$variant(items) => items.capacity(),)*Self::Deferred(_) => self.try_materialize().map_or(0, Self::capacity) }
            }

            /// One element expressed as a scalar value, without expanding the buffer.
            #[must_use]
            pub fn get(&self, index: usize) -> Option<Value> {
                match self { $(Self::$variant(items) => items.get(index).copied().map($wrap),)*Self::Deferred(_) => self.try_materialize().ok().and_then(|native| native.get(index)) }
            }

            /// A scalar value of this buffer's element kind. Values are zero;
            /// this describes the kind, not a property's schema default.
            #[must_use]
            pub fn element_kind(&self) -> Value {
                match self { $(Self::$variant(_) => ($wrap)(<$ty>::default()),)*Self::Deferred(source) => source.element_kind() }
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

            pub(crate) fn pack_iter(kind: &Value, values: impl Iterator<Item = Value>) -> Value {
                match kind {
                    $(Value::$variant(_) => {
                        let mut values = values;
                        let mut items = Vec::<$ty>::with_capacity(values.size_hint().0);
                        for value in values.by_ref() {
                            if let Some(item) = ($read)(&value) { items.push(item); }
                            else {
                                let mut mixed: Vec<Value> = items.into_iter().map($wrap).collect();
                                mixed.push(value);
                                mixed.extend(values);
                                return Value::Array(mixed);
                            }
                        }
                        Value::TypedArray(Self::$variant(Arc::new(items)))
                    },)*
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

fn convert_edit<T: Copy>(
    edit: &ArrayEdit,
    read: impl Fn(&Value) -> Option<T>,
) -> Option<opinionated::ArrayEdit<T>> {
    use ArrayEditOp as Op;
    let operand = |src: &ArrayEditOperand| match src {
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

/// A borrowed array view, backed by either heterogeneous values or a native buffer.
///
/// The view never copies the whole array. `get` and `iter` borrow heterogeneous
/// elements and construct individual native scalar values on demand. Use
/// `typed` and the buffer's native slice accessors for allocation-free numeric loops.
#[derive(Clone, Copy, Debug)]
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
        let ty = PropertyType::new("int", true, Value::Int(4));
        let expected = crate::array_edit::apply_to_value(&edit, &generic, Some(&ty));
        let actual = crate::array_edit::apply_to_value(&edit, &native, Some(&ty));
        assert_eq!(actual, expected);
        assert!(matches!(
            actual,
            Some(Value::TypedArray(TypedArray::Int(_)))
        ));
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

    #[test]
    fn incompatible_property_fill_keeps_heterogeneous_growth() {
        let legacy = Value::Array(vec![Value::Int(1)]);
        let native = Value::from(vec![1_i32]);
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::Resize { len: 2 }],
        };
        let wrong_type = PropertyType::new("float", true, Value::Float(0.0));
        let expected = crate::array_edit::apply_to_value(&edit, &legacy, Some(&wrong_type));
        let actual = crate::array_edit::apply_to_value(&edit, &native, Some(&wrong_type));
        assert_eq!(actual, expected);
        assert_eq!(
            actual,
            Some(Value::Array(vec![Value::Int(1), Value::Float(0.0)]))
        );
    }
}
