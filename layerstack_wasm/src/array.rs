// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composed arrays handed to JavaScript without a copy.
//!
//! A numeric array value crosses the boundary as an [`ArrayHandle`]: the
//! native buffer (`Arc<Vec<T>>`) it shares with the stage, pinned by the
//! handle, with the address and length of its bytes in the module's
//! memory. The wrapper builds a typed array over those bytes (a view) or
//! copies them out with one `slice`; Rust copies no element.
//!
//! Spec: AOUSD Core §6.2–§6.3 (array values).

use alloc::{string::String, vec::Vec};
use core::mem::size_of_val;

use layerstack::TypedArray;
use wasm_bindgen::prelude::*;

use crate::{value::InputData, wire::Wire};

/// The layout of a numeric array's buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// The value type name of the storage type, without a role
    /// (`float3[]`), for an array whose attribute declares no type.
    pub(crate) type_name: &'static str,
    /// The scalar storage type, as the wrapper spells it (`f32`, `f16` for
    /// half floats as raw bits, `bool` for bytes of 0 or 1).
    pub(crate) scalar: &'static str,
    /// Scalars per element.
    pub(crate) components: u32,
}

/// Where a numeric array's bytes are: their address in the module's
/// memory and their length in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bytes {
    pub(crate) address: usize,
    pub(crate) length: usize,
}

/// The layout and bytes of a materialized numeric array; `None` for text
/// (`token[]`, `string[]`) and a deferred array, which have no flat buffer
/// of scalars.
pub(crate) fn numeric(array: &TypedArray) -> Option<(Layout, Bytes)> {
    fn of<T>(
        type_name: &'static str,
        scalar: &'static str,
        components: u32,
        items: &[T],
    ) -> (Layout, Bytes) {
        (
            Layout {
                type_name,
                scalar,
                components,
            },
            Bytes {
                address: items.as_ptr().addr(),
                length: size_of_val(items),
            },
        )
    }
    Some(match array {
        TypedArray::Bool(items) => of("bool[]", "bool", 1, items),
        TypedArray::UChar(items) => of("uchar[]", "u8", 1, items),
        TypedArray::Int(items) => of("int[]", "i32", 1, items),
        TypedArray::UInt(items) => of("uint[]", "u32", 1, items),
        TypedArray::Int64(items) => of("int64[]", "i64", 1, items),
        TypedArray::UInt64(items) => of("uint64[]", "u64", 1, items),
        TypedArray::Half(items) => of("half[]", "f16", 1, items),
        TypedArray::Float(items) => of("float[]", "f32", 1, items),
        TypedArray::Double(items) => of("double[]", "f64", 1, items),
        TypedArray::TimeCode(items) => of("timecode[]", "f64", 1, items),
        TypedArray::Duration(items) => of("duration[]", "f64", 1, items),
        TypedArray::Vec2f(items) => of("float2[]", "f32", 2, items),
        TypedArray::Vec3f(items) => of("float3[]", "f32", 3, items),
        TypedArray::Vec4f(items) => of("float4[]", "f32", 4, items),
        TypedArray::Quatf(items) => of("quatf[]", "f32", 4, items),
        TypedArray::Vec2d(items) => of("double2[]", "f64", 2, items),
        TypedArray::Vec3d(items) => of("double3[]", "f64", 3, items),
        TypedArray::Vec4d(items) => of("double4[]", "f64", 4, items),
        TypedArray::Quatd(items) => of("quatd[]", "f64", 4, items),
        TypedArray::Matrix2d(items) => of("matrix2d[]", "f64", 4, items),
        TypedArray::Matrix3d(items) => of("matrix3d[]", "f64", 9, items),
        TypedArray::Matrix4d(items) => of("matrix4d[]", "f64", 16, items),
        TypedArray::Vec2h(items) => of("half2[]", "f16", 2, items),
        TypedArray::Vec3h(items) => of("half3[]", "f16", 3, items),
        TypedArray::Vec4h(items) => of("half4[]", "f16", 4, items),
        TypedArray::Quath(items) => of("quath[]", "f16", 4, items),
        TypedArray::Vec2i(items) => of("int2[]", "i32", 2, items),
        TypedArray::Vec3i(items) => of("int3[]", "i32", 3, items),
        TypedArray::Vec4i(items) => of("int4[]", "i32", 4, items),
        TypedArray::Token(_) | TypedArray::String(_) | TypedArray::Deferred(_) => return None,
    })
}

/// The scalars of a numeric array as the [`InputData`] the wrapper would
/// have sent for them, for authoring it as a type of another storage
/// (a `float3[]` handle sent as a `double3[]`): one copy.
pub(crate) fn input_data(array: &TypedArray) -> Option<InputData> {
    let numbers = |items: &[f64]| InputData::Numbers(items.to_vec());
    let widened = |items: &[i32]| InputData::Numbers(items.iter().map(|&v| f64::from(v)).collect());
    Some(match array {
        TypedArray::Bool(items) => {
            InputData::Numbers(items.iter().map(|&v| f64::from(u8::from(v))).collect())
        }
        TypedArray::UChar(items) => {
            InputData::Numbers(items.iter().map(|&v| f64::from(v)).collect())
        }
        TypedArray::Int(items) => widened(items),
        TypedArray::UInt(items) => {
            InputData::Numbers(items.iter().map(|&v| f64::from(v)).collect())
        }
        TypedArray::Int64(items) => InputData::Int64(items.to_vec()),
        TypedArray::UInt64(items) => InputData::UInt64(items.to_vec()),
        TypedArray::Half(items) => InputData::HalfBits(items.to_vec()),
        TypedArray::Float(items) => InputData::Floats(items.to_vec()),
        TypedArray::Double(items) | TypedArray::TimeCode(items) | TypedArray::Duration(items) => {
            numbers(items)
        }
        TypedArray::Vec2f(items) => InputData::Floats(items.as_flattened().to_vec()),
        TypedArray::Vec3f(items) => InputData::Floats(items.as_flattened().to_vec()),
        TypedArray::Vec4f(items) | TypedArray::Quatf(items) => {
            InputData::Floats(items.as_flattened().to_vec())
        }
        TypedArray::Vec2d(items) => numbers(items.as_flattened()),
        TypedArray::Vec3d(items) => numbers(items.as_flattened()),
        TypedArray::Vec4d(items) | TypedArray::Quatd(items) | TypedArray::Matrix2d(items) => {
            numbers(items.as_flattened())
        }
        TypedArray::Matrix3d(items) => numbers(items.as_flattened()),
        TypedArray::Matrix4d(items) => numbers(items.as_flattened()),
        TypedArray::Vec2h(items) => InputData::HalfBits(items.as_flattened().to_vec()),
        TypedArray::Vec3h(items) => InputData::HalfBits(items.as_flattened().to_vec()),
        TypedArray::Vec4h(items) | TypedArray::Quath(items) => {
            InputData::HalfBits(items.as_flattened().to_vec())
        }
        TypedArray::Vec2i(items) => widened(items.as_flattened()),
        TypedArray::Vec3i(items) => widened(items.as_flattened()),
        TypedArray::Vec4i(items) => widened(items.as_flattened()),
        TypedArray::Token(_) | TypedArray::String(_) | TypedArray::Deferred(_) => return None,
    })
}

/// A composed numeric array, pinned: the native object behind the
/// TypeScript `ArrayHandle`.
///
/// The handle holds the buffer the stage shares (`Arc<Vec<T>>`), so it is
/// a snapshot: edits copy on write and never change it, and it outlives
/// the stage. [`Self::address`] and [`Self::byte_length`] locate its bytes
/// in the module's memory, where the wrapper builds a typed array over
/// them. Such a view is valid until the next call into the module, which
/// may grow the memory (detaching every view: its `byteLength` becomes 0);
/// the wrapper rebuilds it then. A view must never be written: the bytes
/// are the stage's. Freeing the handle releases the pin.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq)]
pub struct ArrayHandle {
    /// The value type name, as the attribute declares it (`point3f[]`).
    type_name: String,
    /// A materialized numeric array.
    array: TypedArray,
    layout: Layout,
}

impl ArrayHandle {
    /// A handle on `array` (materialized), typed `type_name` when given,
    /// else its storage type's name; `None` for a text array.
    pub(crate) fn new(type_name: Option<&str>, array: &TypedArray) -> Option<Self> {
        let (layout, _) = numeric(array)?;
        Some(Self {
            type_name: String::from(type_name.unwrap_or(layout.type_name)),
            array: array.clone(),
            layout,
        })
    }

    /// The pinned array.
    #[must_use]
    pub fn array(&self) -> &TypedArray {
        &self.array
    }

    fn bytes(&self) -> Bytes {
        numeric(&self.array).map_or(
            Bytes {
                address: 0,
                length: 0,
            },
            |(_, bytes)| bytes,
        )
    }

    /// `[type, scalar, components, length]`: the array's layout.
    pub(crate) fn layout(&self) -> Wire {
        Wire::List(Vec::from([
            Wire::text(self.type_name.as_str()),
            Wire::text(self.layout.scalar),
            Wire::Number(f64::from(self.layout.components)),
            Wire::Number(self.array.len() as f64),
        ]))
    }
}

#[wasm_bindgen]
impl ArrayHandle {
    /// The address of the first byte of the scalars in the module's memory.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn address(&self) -> usize {
        self.bytes().address
    }

    /// The bytes of the scalars.
    #[wasm_bindgen(getter, js_name = byteLength)]
    #[must_use]
    pub fn byte_length(&self) -> usize {
        self.bytes().length
    }

    /// The record `layout` builds, as documented in `array.rs`.
    #[wasm_bindgen(js_name = layout)]
    pub fn js_layout(&self) -> JsValue {
        self.layout().into()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec};

    use super::*;

    #[test]
    fn a_handle_shares_the_buffer_and_locates_its_bytes() {
        let points = Arc::new(vec![[1.0_f32, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        let array = TypedArray::Vec3f(Arc::clone(&points));
        let handle = ArrayHandle::new(Some("point3f[]"), &array).unwrap();
        assert!(handle.array().shares_storage(&array), "no copy");
        assert_eq!(handle.address(), points.as_ptr().addr());
        assert_eq!(handle.byte_length(), 24);
        assert_eq!(
            handle.layout(),
            Wire::List(vec![
                Wire::text("point3f[]"),
                Wire::text("f32"),
                Wire::Number(3.0),
                Wire::Number(2.0),
            ])
        );
        let words = TypedArray::String(Arc::new(["a"].into_iter().collect()));
        assert_eq!(
            ArrayHandle::new(None, &words),
            None,
            "text has no buffer of scalars"
        );
        assert_eq!(
            input_data(&array),
            Some(InputData::Floats(vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]))
        );
    }
}
