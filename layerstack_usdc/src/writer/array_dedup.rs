// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Borrowed array deduplication before conversion or compression.
//! OpenUSD's `PackArray` likewise looks up typed inputs before encoding.
//! Floating-point equality here is deliberately bitwise, matching our writer.

use super::Value;
use crate::value_type::ValueType;
use alloc::string::String;
use core::hash::{BuildHasher, Hash, Hasher};
use layerstack::HashMap;
use smallvec::SmallVec;

#[derive(Clone, Copy)]
enum Components<'a> {
    Bool(&'a [bool]),
    U8(&'a [u8]),
    I32(&'a [i32]),
    U32(&'a [u32]),
    I64(&'a [i64]),
    U64(&'a [u64]),
    U16(&'a [u16]),
    F32(&'a [f32]),
    F64(&'a [f64]),
    Text(&'a [String]),
}

impl Components<'_> {
    fn len(self) -> usize {
        match self {
            Self::Bool(v) => v.len(),
            Self::U8(v) => v.len(),
            Self::I32(v) => v.len(),
            Self::U32(v) => v.len(),
            Self::I64(v) => v.len(),
            Self::U64(v) => v.len(),
            Self::U16(v) => v.len(),
            Self::F32(v) => v.len(),
            Self::F64(v) => v.len(),
            Self::Text(v) => v.len(),
        }
    }

    fn same(self, other: Self) -> bool {
        // Float chunks reduce bit differences without a branch per component,
        // allowing vectorization. Chunk boundaries still permit early exit;
        // the tail check quickly rejects arrays with long equal prefixes.
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => core::ptr::eq(a, b) || (a == b),
            (Self::U8(a), Self::U8(b)) => core::ptr::eq(a, b) || (a == b),
            (Self::I32(a), Self::I32(b)) => core::ptr::eq(a, b) || (a == b),
            (Self::U32(a), Self::U32(b)) => core::ptr::eq(a, b) || (a == b),
            (Self::I64(a), Self::I64(b)) => core::ptr::eq(a, b) || (a == b),
            (Self::U64(a), Self::U64(b)) => core::ptr::eq(a, b) || (a == b),
            (Self::U16(a), Self::U16(b)) => core::ptr::eq(a, b) || (a == b),
            (Self::F32(a), Self::F32(b)) => {
                core::ptr::eq(a, b)
                    || (a.len() == b.len()
                        && a.last().map(|v| v.to_bits()) == b.last().map(|v| v.to_bits())
                        && a.chunks(64).zip(b.chunks(64)).all(|(a, b)| {
                            a.iter()
                                .zip(b)
                                .fold(0, |diff, (a, b)| diff | (a.to_bits() ^ b.to_bits()))
                                == 0
                        }))
            }
            (Self::F64(a), Self::F64(b)) => {
                core::ptr::eq(a, b)
                    || (a.len() == b.len()
                        && a.last().map(|v| v.to_bits()) == b.last().map(|v| v.to_bits())
                        && a.chunks(64).zip(b.chunks(64)).all(|(a, b)| {
                            a.iter()
                                .zip(b)
                                .fold(0, |diff, (a, b)| diff | (a.to_bits() ^ b.to_bits()))
                                == 0
                        }))
            }
            (Self::Text(a), Self::Text(b)) => core::ptr::eq(a, b) || (a == b),
            _ => false,
        }
    }

    fn hash(self, state: &mut impl Hasher) {
        match self {
            Self::Bool(v) => v.hash(state),
            Self::U8(v) => v.hash(state),
            Self::I32(v) => v.hash(state),
            Self::U32(v) => v.hash(state),
            Self::I64(v) => v.hash(state),
            Self::U64(v) => v.hash(state),
            Self::U16(v) => v.hash(state),
            Self::F32(values) => {
                // Stack chunks permit bulk hashing without unsafe casts or
                // a payload-sized allocation for floating-point bit patterns.
                let mut bits = [0_u32; 512];
                for chunk in values.chunks(512) {
                    for (out, value) in bits.iter_mut().zip(chunk) {
                        *out = value.to_bits();
                    }
                    bits[..chunk.len()].hash(state);
                }
            }
            Self::F64(values) => {
                // Stack chunks permit bulk hashing without unsafe casts or
                // a payload-sized allocation for floating-point bit patterns.
                let mut bits = [0_u64; 512];
                for chunk in values.chunks(512) {
                    for (out, value) in bits.iter_mut().zip(chunk) {
                        *out = value.to_bits();
                    }
                    bits[..chunk.len()].hash(state);
                }
            }
            Self::Text(v) => v.hash(state),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Array<'a> {
    ty: u8,
    components: Components<'a>,
}

impl<'a> Array<'a> {
    pub(super) fn from_value(value: &'a Value) -> Option<Self> {
        let (ty, components) = match value {
            Value::BoolArray(v) => (ValueType::Bool, Components::Bool(v)),
            Value::UCharArray(v) => (ValueType::UChar, Components::U8(v)),
            Value::IntArray(v) => (ValueType::Int, Components::I32(v)),
            Value::UIntArray(v) => (ValueType::UInt, Components::U32(v)),
            Value::Int64Array(v) => (ValueType::Int64, Components::I64(v)),
            Value::UInt64Array(v) => (ValueType::UInt64, Components::U64(v)),
            Value::HalfArray(v) => (ValueType::Half, Components::U16(v)),
            Value::FloatArray(v) => (ValueType::Float, Components::F32(v)),
            Value::DoubleArray(v) => (ValueType::Double, Components::F64(v)),
            Value::TimeCodeArray(v) => (ValueType::TimeCode, Components::F64(v)),
            Value::StringArray(v) => (ValueType::String, Components::Text(v)),
            Value::TokenArray(v) => (ValueType::Token, Components::Text(v)),
            Value::AssetArray(v) => (ValueType::AssetPath, Components::Text(v)),
            Value::Vec2hArray(v) => (ValueType::Vec2h, Components::U16(v.as_flattened())),
            Value::Vec3hArray(v) => (ValueType::Vec3h, Components::U16(v.as_flattened())),
            Value::Vec4hArray(v) => (ValueType::Vec4h, Components::U16(v.as_flattened())),
            Value::Vec2fArray(v) => (ValueType::Vec2f, Components::F32(v.as_flattened())),
            Value::Vec3fArray(v) => (ValueType::Vec3f, Components::F32(v.as_flattened())),
            Value::Vec4fArray(v) => (ValueType::Vec4f, Components::F32(v.as_flattened())),
            Value::Vec2dArray(v) => (ValueType::Vec2d, Components::F64(v.as_flattened())),
            Value::Vec3dArray(v) => (ValueType::Vec3d, Components::F64(v.as_flattened())),
            Value::Vec4dArray(v) => (ValueType::Vec4d, Components::F64(v.as_flattened())),
            Value::Vec2iArray(v) => (ValueType::Vec2i, Components::I32(v.as_flattened())),
            Value::Vec3iArray(v) => (ValueType::Vec3i, Components::I32(v.as_flattened())),
            Value::Vec4iArray(v) => (ValueType::Vec4i, Components::I32(v.as_flattened())),
            Value::QuathArray(v) => (ValueType::Quath, Components::U16(v.as_flattened())),
            Value::QuatfArray(v) => (ValueType::Quatf, Components::F32(v.as_flattened())),
            Value::QuatdArray(v) => (ValueType::Quatd, Components::F64(v.as_flattened())),
            Value::Matrix2dArray(v) => (
                ValueType::Matrix2d,
                Components::F64(v.as_flattened().as_flattened()),
            ),
            Value::Matrix3dArray(v) => (
                ValueType::Matrix3d,
                Components::F64(v.as_flattened().as_flattened()),
            ),
            Value::Matrix4dArray(v) => (
                ValueType::Matrix4d,
                Components::F64(v.as_flattened().as_flattened()),
            ),
            _ => return None,
        };
        (components.len() != 0).then_some(Self {
            ty: ty as u8,
            components,
        })
    }

    fn same(self, other: Self) -> bool {
        self.ty == other.ty && self.components.same(other.components)
    }
}

type Entry<'a> = (Array<'a>, u64);

enum Candidates<'a> {
    Single(Entry<'a>),
    Indexed {
        last: Entry<'a>,
        hashes: HashMap<u64, SmallVec<[Entry<'a>; 1]>>,
    },
}

/// Owns only indexes and borrows input arrays for the duration of one write.
#[derive(Default)]
pub(super) struct ArrayDedup<'a> {
    values: HashMap<(u8, usize), Candidates<'a>>,
}

impl<'a> ArrayDedup<'a> {
    pub(super) fn lookup(&mut self, array: Array<'a>) -> (Option<u64>, u64) {
        let Some(candidates) = self.values.get_mut(&(array.ty, array.components.len())) else {
            return (None, 0);
        };
        let &(last, rep) = match candidates {
            Candidates::Single(entry) | Candidates::Indexed { last: entry, .. } => &*entry,
        };
        if array.same(last) {
            return (Some(rep), 0);
        }
        if let Candidates::Single(first) = candidates {
            let mut hashes = HashMap::<u64, SmallVec<[Entry<'a>; 1]>>::default();
            let mut state = hashes.hasher().build_hasher();
            first.0.components.hash(&mut state);
            hashes.entry(state.finish()).or_default().push(*first);
            *candidates = Candidates::Indexed {
                last: *first,
                hashes,
            };
        }
        let Candidates::Indexed { last, hashes } = candidates else {
            unreachable!()
        };
        let mut state = hashes.hasher().build_hasher();
        array.components.hash(&mut state);
        let hash = state.finish();
        if let Some(bucket) = hashes.get(&hash) {
            for &(candidate, rep) in bucket {
                if array.same(candidate) {
                    *last = (candidate, rep);
                    return (Some(rep), hash);
                }
            }
        }
        (None, hash)
    }

    pub(super) fn insert(&mut self, array: Array<'a>, hash: u64, rep: u64) {
        let key = (array.ty, array.components.len());
        if let Some(Candidates::Indexed { last, hashes }) = self.values.get_mut(&key) {
            hashes.entry(hash).or_default().push((array, rep));
            *last = (array, rep);
        } else {
            self.values.insert(key, Candidates::Single((array, rep)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn remember<'a>(cache: &mut ArrayDedup<'a>, value: &'a Value, rep: u64) -> u64 {
        let array = Array::from_value(value).unwrap();
        let (found, hash) = cache.lookup(array);
        if let Some(rep) = found {
            return rep;
        }
        cache.insert(array, hash, rep);
        rep
    }

    #[test]
    fn float_equality_is_bitwise_across_chunks_and_tails() {
        let bits = [0, 0x8000_0000, 0x7fc0_0001, 0x7fc0_0002, 0x7f80_0000];
        let values: vec::Vec<_> = (0..515)
            .map(|i| f32::from_bits(bits[i % bits.len()]))
            .collect();
        let original = Value::FloatArray(values.clone());
        let duplicate = Value::FloatArray(values.clone());
        let mut changed = values;
        changed[257] = f32::from_bits(0x7fc0_0002);
        let changed = Value::FloatArray(changed);
        let mut cache = ArrayDedup::default();
        assert_eq!(remember(&mut cache, &original, 10), 10);
        assert_eq!(remember(&mut cache, &duplicate, 20), 10);
        assert_eq!(remember(&mut cache, &changed, 30), 30);
        assert_eq!(remember(&mut cache, &duplicate, 40), 10);

        let doubles = [
            Value::DoubleArray(vec![0.0; 515]),
            Value::DoubleArray(vec![-0.0; 515]),
            Value::DoubleArray(vec![f64::from_bits(0x7ff8_0000_0000_0001); 515]),
        ];
        let copies = doubles.clone();
        let mut cache = ArrayDedup::default();
        for (i, value) in doubles.iter().enumerate() {
            assert_eq!(remember(&mut cache, value, i as u64), i as u64);
        }
        for (i, value) in copies.iter().enumerate() {
            assert_eq!(remember(&mut cache, value, 99), i as u64);
        }
    }

    #[test]
    fn collisions_types_and_lengths_do_not_rebind_arrays() {
        let first = Value::IntArray(vec![1, 2, 3]);
        let second = Value::IntArray(vec![1, 4, 3]);
        let duplicate = second.clone();
        let shorter = Value::IntArray(vec![1, 2]);
        let unsigned = Value::UIntArray(vec![1, 2, 3]);
        let mut cache = ArrayDedup::default();
        assert_eq!(remember(&mut cache, &first, 10), 10);
        let array = Array::from_value(&second).unwrap();
        let (found, hash) = cache.lookup(array);
        assert_eq!(found, None);
        let key = (array.ty, array.components.len());
        let Candidates::Indexed { last, hashes } = cache.values.get_mut(&key).unwrap() else {
            panic!("indexed")
        };
        // Simulate an unequal candidate colliding with the second input.
        hashes.entry(hash).or_default().push(*last);
        assert_eq!(remember(&mut cache, &second, 20), 20);
        let Candidates::Indexed { last, .. } = cache.values.get_mut(&key).unwrap() else {
            panic!("indexed")
        };
        *last = (Array::from_value(&first).unwrap(), 10);
        assert_eq!(remember(&mut cache, &duplicate, 30), 20);
        assert_eq!(remember(&mut cache, &shorter, 40), 40);
        assert_eq!(remember(&mut cache, &unsigned, 50), 50);
    }
}
