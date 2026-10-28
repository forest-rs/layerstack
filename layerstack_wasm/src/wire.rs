// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The values the bindings hand JavaScript, before they become `JsValue`s.
//!
//! Every result is built as a [`Wire`] tree first, so the logic that
//! builds it runs (and is tested) on any target; only [`JsValue::from`]
//! needs a JavaScript host. A record is a positional list whose layout the
//! TypeScript wrapper (`packages/layerstack/src/native.ts`) decodes; the
//! layouts are the module's ABI and change only with [`crate::ABI`].

use alloc::{string::String, vec::Vec};

use wasm_bindgen::JsValue;

/// One value handed to JavaScript.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Wire {
    /// `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number.
    Number(f64),
    /// A `bigint` from a signed 64-bit integer.
    Int64(i64),
    /// A `bigint` from an unsigned 64-bit integer.
    UInt64(u64),
    /// A string.
    Text(String),
    /// An array of values: a record or a list.
    List(Vec<Self>),
    /// A typed array, copied out of Wasm memory.
    Elements(Elements),
}

/// A numeric buffer that becomes one typed array.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Elements {
    /// `Uint8Array`.
    U8(Vec<u8>),
    /// `Uint16Array` (half floats as raw bits).
    U16(Vec<u16>),
    /// `Int32Array`.
    I32(Vec<i32>),
    /// `Uint32Array`.
    U32(Vec<u32>),
    /// `BigInt64Array`.
    I64(Vec<i64>),
    /// `BigUint64Array`.
    U64(Vec<u64>),
    /// `Float32Array`.
    F32(Vec<f32>),
    /// `Float64Array`.
    F64(Vec<f64>),
}

impl Wire {
    /// A string.
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// A string, or `null`.
    pub(crate) fn text_or_null(text: Option<impl Into<String>>) -> Self {
        text.map_or(Self::Null, Self::text)
    }

    /// The string, if this is one.
    #[cfg(test)]
    pub(crate) fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The items, if this is a list.
    #[cfg(test)]
    pub(crate) fn as_list(&self) -> Option<&[Self]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }
}

impl From<Wire> for JsValue {
    fn from(wire: Wire) -> Self {
        match wire {
            Wire::Null => Self::NULL,
            Wire::Bool(value) => Self::from_bool(value),
            Wire::Number(value) => Self::from_f64(value),
            Wire::Int64(value) => Self::from(value),
            Wire::UInt64(value) => Self::from(value),
            Wire::Text(text) => Self::from_str(&text),
            Wire::List(items) => {
                Self::from(items.into_iter().map(Self::from).collect::<Vec<Self>>())
            }
            Wire::Elements(elements) => match elements {
                Elements::U8(items) => Self::from(items),
                Elements::U16(items) => Self::from(items),
                Elements::I32(items) => Self::from(items),
                Elements::U32(items) => Self::from(items),
                Elements::I64(items) => Self::from(items),
                Elements::U64(items) => Self::from(items),
                Elements::F32(items) => Self::from(items),
                Elements::F64(items) => Self::from(items),
            },
        }
    }
}
