// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{format, string::String, vec::Vec};
use core::fmt::{self, Debug, Write};

use crate::usda::writer::{LayerOffset, ListOp, Reference, Value};
use layerstack::spline::{Extrapolation, SplineData};
use opinionated::{ArrayEditOp as Instruction, ArrayEditOperand as Operand};

use super::{ArrayChange, KnotData};

// AOUSD Core §6.2–6.5. Compare typed storage. An authored NaN compares
// equal to itself and the sign of zero is preserved.
trait Same {
    fn same(&self, other: &Self) -> bool;
}

macro_rules! exact {
    ($($ty:ty),*) => {$(
        impl Same for $ty {
            fn same(&self, other: &Self) -> bool {
                self == other
            }
        }
    )*};
}
exact!(bool, u8, u16, i32, u32, i64, u64, String);

impl Same for f32 {
    fn same(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl Same for f64 {
    fn same(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl<T: Same, const N: usize> Same for [T; N] {
    fn same(&self, other: &Self) -> bool {
        same_items(self, other, Same::same)
    }
}

impl<T: Same> Same for Option<T> {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Some(a), Some(b)) => a.same(b),
            (None, None) => true,
            _ => false,
        }
    }
}

fn same_items<T>(a: &[T], b: &[T], equal: impl Fn(&T, &T) -> bool) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
}

// Lists are positional, including array edits. Dictionary keys are already
// sorted by the shared layer lowering.
pub(super) fn same_value(a: &Value, b: &Value) -> bool {
    macro_rules! scalars {
        ($($variant:ident),*) => {$(
            if let (Value::$variant(a), Value::$variant(b)) = (a, b) {
                return a.same(b);
            }
        )*};
    }
    scalars!(
        Float, Double, TimeCode, Duration, Float2, Float3, Float4, Double2, Double3, Double4,
        Quatf, Quatd, Matrix2d, Matrix3d, Matrix4d
    );
    match (a, b) {
        (Value::Dictionary(a), Value::Dictionary(b)) => same_dictionary(a, b),
        (
            Value::ArrayEdit {
                element: a,
                instructions: a_ops,
            },
            Value::ArrayEdit {
                element: b,
                instructions: b_ops,
            },
        ) => same_value(a, b) && same_items(a_ops, b_ops, same_instruction),
        _ => arrays!(a, b, array_equal).unwrap_or_else(|| a == b),
    }
}

fn same_dictionary(a: &[(String, Value)], b: &[(String, Value)]) -> bool {
    // Most callers supply sorted entries, including dictionary values. The
    // order reference and spline custom data arrived in is immaterial.
    if a.len() != b.len() {
        return false;
    }
    if a.iter().zip(b).all(|(a, b)| a.0 == b.0) {
        return a.iter().zip(b).all(|(a, b)| same_value(&a.1, &b.1));
    }
    let mut a: Vec<_> = a.iter().collect();
    let mut b: Vec<_> = b.iter().collect();
    a.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    b.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    a.into_iter()
        .zip(b)
        .all(|(a, b)| a.0 == b.0 && same_value(&a.1, &b.1))
}

fn same_instruction(a: &Instruction<Value>, b: &Instruction<Value>) -> bool {
    let same_source = |a: &Operand<Value>, b: &Operand<Value>| match (a, b) {
        (Operand::Literal(a), Operand::Literal(b)) => same_value(a, b),
        _ => a == b,
    };
    match (a, b) {
        (
            Instruction::Write {
                src: a,
                index: a_index,
            },
            Instruction::Write {
                src: b,
                index: b_index,
            },
        )
        | (
            Instruction::Insert {
                src: a,
                index: a_index,
            },
            Instruction::Insert {
                src: b,
                index: b_index,
            },
        ) => a_index == b_index && same_source(a, b),
        (
            Instruction::MinSizeFill {
                len: a_len,
                fill: a,
            },
            Instruction::MinSizeFill {
                len: b_len,
                fill: b,
            },
        )
        | (
            Instruction::ResizeFill {
                len: a_len,
                fill: a,
            },
            Instruction::ResizeFill {
                len: b_len,
                fill: b,
            },
        ) => a_len == b_len && same_value(a, b),
        _ => a == b,
    }
}

pub(super) fn same_offset(a: &LayerOffset, b: &LayerOffset) -> bool {
    a.offset.same(&b.offset) && a.scale.same(&b.scale)
}

pub(super) fn same_arcs(a: &ListOp<Reference>, b: &ListOp<Reference>) -> bool {
    let equal = |a: &Reference, b: &Reference| {
        a.asset_path == b.asset_path
            && a.prim_path == b.prim_path
            && same_offset(&a.offset, &b.offset)
            && same_dictionary(&a.custom_data, &b.custom_data)
    };
    let explicit = match (&a.explicit, &b.explicit) {
        (Some(a), Some(b)) => same_items(a, b, equal),
        (None, None) => true,
        _ => false,
    };
    explicit
        && same_items(&a.deleted, &b.deleted, equal)
        && same_items(&a.added, &b.added, equal)
        && same_items(&a.prepended, &b.prepended, equal)
        && same_items(&a.appended, &b.appended, equal)
        && same_items(&a.reordered, &b.reordered, equal)
}

pub(super) fn same_spline(
    a: &SplineData,
    a_data: &[KnotData],
    b: &SplineData,
    b_data: &[KnotData],
) -> bool {
    let same_extrapolation = |a, b| match (a, b) {
        (Extrapolation::Sloped(a), Extrapolation::Sloped(b)) => a.same(&b),
        _ => a == b,
    };
    let loop_params = match (a.loop_params, b.loop_params) {
        (Some(a), Some(b)) => {
            a.proto_start.same(&b.proto_start)
                && a.proto_end.same(&b.proto_end)
                && a.num_pre_loops == b.num_pre_loops
                && a.num_post_loops == b.num_post_loops
                && a.value_offset.same(&b.value_offset)
        }
        (None, None) => true,
        _ => false,
    };
    a.pre_loop_boundary.same(&b.pre_loop_boundary)
        && a.post_loop_boundary.same(&b.post_loop_boundary)
        && a.data_type == b.data_type
        && a.default_curve_type == b.default_curve_type
        && same_extrapolation(a.pre_extrapolation, b.pre_extrapolation)
        && same_extrapolation(a.post_extrapolation, b.post_extrapolation)
        && loop_params
        && same_items(&a.knots, &b.knots, |a, b| {
            a.time.same(&b.time)
                && a.value.same(&b.value)
                && a.pre_value.same(&b.pre_value)
                && a.pre_tan_algorithm == b.pre_tan_algorithm
                && a.post_tan_algorithm == b.post_tan_algorithm
                && a.next_interp == b.next_interp
                && a.curve_type == b.curve_type
                && a.pre_tan_maya_form == b.pre_tan_maya_form
                && a.post_tan_maya_form == b.post_tan_maya_form
                && a.pre_tan_width.same(&b.pre_tan_width)
                && a.post_tan_width.same(&b.post_tan_width)
                && a.pre_tan_slope.same(&b.pre_tan_slope)
                && a.post_tan_slope.same(&b.post_tan_slope)
        })
        && same_items(a_data, b_data, |a, b| {
            a.0.same(&b.0) && same_dictionary(&a.1, &b.1)
        })
}

// Cover the same set of typed arrays for equality, summary, and indices.
macro_rules! match_arrays {
    ($a:expr, $b:expr, $visit:ident; $($variant:ident),*) => {
        match ($a, $b) {
            $((Value::$variant(a), Value::$variant(b)) => Some($visit(stringify!($variant), a, b)),)*
            _ => None,
        }
    };
}

macro_rules! arrays {
    ($a:expr, $b:expr, $visit:ident) => {
        match_arrays!($a, $b, $visit;
            BoolArray, UCharArray, IntArray, UIntArray, Int64Array, UInt64Array,
            HalfArray, FloatArray, DoubleArray, TimeCodeArray, DurationArray,
            StringArray, TokenArray, AssetArray, PathExpressionArray,
            Float2Array, Float3Array, Float4Array, Double2Array, Double3Array,
            Double4Array, Half2Array, Half3Array, Half4Array, Int2Array,
            Int3Array, Int4Array, QuathArray, QuatfArray, QuatdArray,
            Matrix2dArray, Matrix3dArray, Matrix4dArray)
    };
}
use arrays;
use match_arrays;

fn array_equal<T: Same>(_: &str, a: &[T], b: &[T]) -> bool {
    same_items(a, b, Same::same)
}

pub(super) fn array_change(a: &Value, b: &Value) -> Option<ArrayChange> {
    fn positions<T: Same>(_: &str, a: &[T], b: &[T]) -> ArrayChange {
        let mut changed_count = 0;
        let mut indices = Vec::new();
        for index in 0..a.len().max(b.len()) {
            let equal = match (a.get(index), b.get(index)) {
                (Some(a), Some(b)) => a.same(b),
                _ => false,
            };
            if !equal {
                changed_count += 1;
                if indices.len() < 8 {
                    indices.push(index);
                }
            }
        }
        ArrayChange {
            before_len: a.len(),
            after_len: b.len(),
            changed_count,
            indices,
        }
    }
    arrays!(a, b, positions)
}

pub(super) fn summary(value: &Value) -> String {
    fn array_summary<T>(name: &str, items: &[T], _: &[T]) -> String {
        format!("{name} ({} elements)", items.len())
    }
    arrays!(value, value, array_summary).unwrap_or_else(|| match value {
        Value::Block => String::from("None (value block)"),
        Value::AnimationBlock => String::from("AnimationBlock"),
        _ => debug_summary(value),
    })
}

// Find the first differing operand, so a wide list can show something
// that actually changed (AOUSD Core §6.6.3).
pub(super) fn path_list_change(
    before: &ListOp<String>,
    after: &ListOp<String>,
) -> Option<(String, String)> {
    fn first_difference(op: &str, before: &[String], after: &[String]) -> Option<(String, String)> {
        let index = before
            .iter()
            .zip(after)
            .position(|(a, b)| a != b)
            .or_else(|| (before.len() != after.len()).then(|| before.len().min(after.len())))?;
        let summary = |list: &[String]| match list.get(index) {
            Some(path) => format!("{op}[{index}] {}", debug_summary(path)),
            None => format!("{op}[{index}] <absent>"),
        };
        Some((summary(before), summary(after)))
    }
    if let (Some(a), Some(b)) = (&before.explicit, &after.explicit) {
        return first_difference("explicit", a, b);
    }
    if before.explicit.is_some() != after.explicit.is_some() {
        return None;
    }
    [
        ("delete", &before.deleted, &after.deleted),
        ("add", &before.added, &after.added),
        ("prepend", &before.prepended, &after.prepended),
        ("append", &before.appended, &after.appended),
        ("reorder", &before.reordered, &after.reordered),
    ]
    .into_iter()
    .find_map(|(op, a, b)| first_difference(op, a, b))
}

pub(super) fn list_summary<T: Debug>(op: &ListOp<T>) -> String {
    let mut text = Preview::default();
    let mut first = true;
    let mut part = |name, items: &[T]| {
        if !first {
            text.write_str(", ")?;
        }
        first = false;
        write!(text, "{name} {items:?}")
    };
    if let Some(items) = &op.explicit {
        let _ = part("explicit", items);
    } else {
        for (name, items) in [
            ("delete", &op.deleted),
            ("add", &op.added),
            ("prepend", &op.prepended),
            ("append", &op.appended),
            ("reorder", &op.reordered),
        ] {
            if !items.is_empty() && part(name, items).is_err() {
                break;
            }
        }
    }
    text.0
}

pub(super) fn debug_summary(value: impl Debug) -> String {
    let mut preview = Preview::default();
    let _ = write!(preview, "{value:?}");
    preview.0
}

#[derive(Default)]
struct Preview(String);

impl Write for Preview {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let available = 160_usize.saturating_sub(self.0.len());
        if text.len() <= available {
            self.0.push_str(text);
            return Ok(());
        }
        let mut end = available;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.0.push_str(&text[..end]);
        self.0.push('…');
        Err(fmt::Error)
    }
}
