// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sparse array edit operations.
//!
//! The edit language follows the sparse-array-edit proposal shape:
//! instructions mutate the destination array in sequence, either using literal
//! values or by copying elements from the destination array as edited so far.

use alloc::vec::Vec;

use crate::{doc::Value, property::PropertyType};

/// An index in the destination array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayIndex {
    /// A concrete numeric index. Negative values index from the end.
    Position(i64),
    /// The index past the final element.
    End,
}

/// An edit source operand.
#[derive(Clone, Debug, PartialEq)]
pub enum ArrayEditOperand {
    /// Use an immediate literal value.
    Literal(Value),
    /// Copy the value currently stored at the given array index.
    CopyFrom(ArrayIndex),
}

/// A single sparse array edit instruction.
#[derive(Clone, Debug, PartialEq)]
pub enum ArrayEditOp {
    /// Overwrite an existing element without changing array size.
    Write {
        /// Source value for the write.
        src: ArrayEditOperand,
        /// Destination index to overwrite.
        index: ArrayIndex,
    },
    /// Insert a new element, shifting later elements to the right.
    Insert {
        /// Source value for the insert.
        src: ArrayEditOperand,
        /// Destination insertion point.
        index: ArrayIndex,
    },
    /// Erase an existing element.
    Erase {
        /// Index to remove.
        index: ArrayIndex,
    },
    /// Grow the array to at least `len`, filling new elements with the
    /// property type's default element.
    MinSize {
        /// Minimum number of elements after editing.
        len: usize,
    },
    /// Grow the array to at least `len`, filling new elements with `fill`.
    ///
    /// This is OpenUSD's `minsize N fill <literal>` (`OpMinSizeFill` in
    /// `pxr/base/vt/arrayEditOps.h`).
    MinSizeFill {
        /// Minimum number of elements after editing.
        len: usize,
        /// Value given to each element added by the edit.
        fill: Value,
    },
    /// Shrink the array to at most `len`.
    MaxSize {
        /// Maximum number of elements after editing.
        len: usize,
    },
    /// Resize the array to exactly `len`, filling new elements with the
    /// property type's default element.
    Resize {
        /// Final number of elements after editing.
        len: usize,
    },
    /// Resize the array to exactly `len`, filling new elements with `fill`.
    ///
    /// This is OpenUSD's `resize N fill <literal>` (`OpSetSizeFill` in
    /// `pxr/base/vt/arrayEditOps.h`).
    ResizeFill {
        /// Final number of elements after editing.
        len: usize,
        /// Value given to each element added by the edit.
        fill: Value,
    },
}

/// A sparse array edit expressed as an instruction sequence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArrayEdit {
    /// Instructions applied in order.
    pub ops: Vec<ArrayEditOp>,
}

impl ArrayEdit {
    /// Returns `true` when the edit performs no work.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.ops.is_empty()
    }

    /// Composes this stronger edit over a weaker edit.
    ///
    /// Applying the returned edit to a dense array is equivalent to applying
    /// `weaker` first and then `self`.
    #[must_use]
    pub fn compose_over(&self, weaker: &Self) -> Self {
        let mut ops = weaker.ops.clone();
        ops.extend(self.ops.iter().cloned());
        Self { ops }
    }

    /// Applies the edit over a dense array value.
    #[must_use]
    pub fn compose_over_array(
        &self,
        weaker: &[Value],
        property_type: Option<&PropertyType>,
    ) -> Vec<Value> {
        let mut result = weaker.to_vec();
        self.apply_in_place(&mut result, property_type);
        result
    }

    /// Applies the edit over a dense [`Value::Array`].
    #[must_use]
    pub fn compose_over_value(
        &self,
        weaker: &Value,
        property_type: Option<&PropertyType>,
    ) -> Option<Value> {
        let Value::Array(items) = weaker else {
            return None;
        };
        Some(Value::Array(self.compose_over_array(items, property_type)))
    }

    /// Applies the edit in place to the provided array.
    pub fn apply_in_place(&self, array: &mut Vec<Value>, property_type: Option<&PropertyType>) {
        for op in &self.ops {
            match op {
                ArrayEditOp::Write { src, index } => {
                    let Some(dst_idx) = resolve_write_index(array.len(), *index) else {
                        continue;
                    };
                    let Some(value) = resolve_operand(array, src) else {
                        continue;
                    };
                    array[dst_idx] = value;
                }
                ArrayEditOp::Insert { src, index } => {
                    let Some(dst_idx) = resolve_insert_index(array.len(), *index) else {
                        continue;
                    };
                    let Some(value) = resolve_operand(array, src) else {
                        continue;
                    };
                    array.insert(dst_idx, value);
                }
                ArrayEditOp::Erase { index } => {
                    let Some(dst_idx) = resolve_write_index(array.len(), *index) else {
                        continue;
                    };
                    array.remove(dst_idx);
                }
                ArrayEditOp::MinSize { len } => {
                    grow_to(array, *len, property_type);
                }
                ArrayEditOp::MinSizeFill { len, fill } => {
                    fill_to(array, *len, fill);
                }
                ArrayEditOp::MaxSize { len } => {
                    array.truncate(*len);
                }
                ArrayEditOp::Resize { len } => {
                    if array.len() > *len {
                        array.truncate(*len);
                    } else {
                        grow_to(array, *len, property_type);
                    }
                }
                ArrayEditOp::ResizeFill { len, fill } => {
                    array.truncate(*len);
                    fill_to(array, *len, fill);
                }
            }
        }
    }
}

fn resolve_operand(array: &[Value], operand: &ArrayEditOperand) -> Option<Value> {
    match operand {
        ArrayEditOperand::Literal(value) => Some(value.clone()),
        ArrayEditOperand::CopyFrom(index) => {
            let idx = resolve_write_index(array.len(), *index)?;
            Some(array[idx].clone())
        }
    }
}

fn resolve_write_index(len: usize, index: ArrayIndex) -> Option<usize> {
    match index {
        ArrayIndex::End => None,
        ArrayIndex::Position(value) if value >= 0 => {
            let idx = usize::try_from(value).ok()?;
            (idx < len).then_some(idx)
        }
        ArrayIndex::Position(value) => {
            let offset = usize::try_from(value.unsigned_abs()).ok()?;
            len.checked_sub(offset).filter(|idx| *idx < len)
        }
    }
}

fn resolve_insert_index(len: usize, index: ArrayIndex) -> Option<usize> {
    match index {
        ArrayIndex::End => Some(len),
        ArrayIndex::Position(value) if value >= 0 => {
            let idx = usize::try_from(value).ok()?;
            (idx <= len).then_some(idx)
        }
        ArrayIndex::Position(value) => {
            let offset = usize::try_from(value.unsigned_abs()).ok()?;
            len.checked_sub(offset)
        }
    }
}

fn grow_to(array: &mut Vec<Value>, len: usize, property_type: Option<&PropertyType>) {
    let Some(fill) = property_type.and_then(PropertyType::default_array_element) else {
        return;
    };

    fill_to(array, len, &fill);
}

fn fill_to(array: &mut Vec<Value>, len: usize, fill: &Value) {
    if array.len() < len {
        array.resize(len, fill.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec;

    fn int_array(values: &[i32]) -> Vec<Value> {
        values.iter().copied().map(Value::Int).collect()
    }

    fn int_array_type() -> PropertyType {
        PropertyType::new(Arc::<str>::from("int"), true, Value::Int(0))
    }

    #[test]
    fn compose_over_concatenates_instruction_sequences() {
        let weak = ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(1)),
                index: ArrayIndex::End,
            }],
        };
        let strong = ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Int(9)),
                index: ArrayIndex::Position(0),
            }],
        };

        let composed = strong.compose_over(&weak);
        let result = composed.compose_over_array(&[], Some(&int_array_type()));
        assert_eq!(result, int_array(&[9]));
    }

    #[test]
    fn resize_uses_property_default_elements() {
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::Resize { len: 3 }],
        };

        let result = edit.compose_over_array(&[], Some(&int_array_type()));
        assert_eq!(result, int_array(&[0, 0, 0]));
    }

    // Vectors ported from OpenUSD v26.08 `pxr/base/vt/testenv/testVtArrayEdit.cpp`
    // (`testBuilderAndComposition`) and `testVtArrayEdit.py`.

    fn lit(value: i32) -> ArrayEditOperand {
        ArrayEditOperand::Literal(Value::Int(value))
    }

    fn edit(ops: Vec<ArrayEditOp>) -> ArrayEdit {
        ArrayEdit { ops }
    }

    fn apply(edit: &ArrayEdit, weaker: &[i32]) -> Vec<Value> {
        edit.compose_over_array(&int_array(weaker), Some(&int_array_type()))
    }

    fn zero_nine() -> ArrayEdit {
        edit(vec![
            ArrayEditOp::Insert {
                src: lit(0),
                index: ArrayIndex::Position(0),
            },
            ArrayEditOp::Insert {
                src: lit(9),
                index: ArrayIndex::End,
            },
        ])
    }

    fn mix_and_trim() -> ArrayEdit {
        edit(vec![
            ArrayEditOp::Write {
                src: ArrayEditOperand::CopyFrom(ArrayIndex::Position(-1)),
                index: ArrayIndex::Position(2),
            },
            ArrayEditOp::Write {
                src: ArrayEditOperand::CopyFrom(ArrayIndex::Position(0)),
                index: ArrayIndex::Position(4),
            },
            ArrayEditOp::Erase {
                index: ArrayIndex::Position(-1),
            },
            ArrayEditOp::Erase {
                index: ArrayIndex::Position(0),
            },
        ])
    }

    #[test]
    fn vt_prepend_append_and_self_composition() {
        let zero_nine = zero_nine();
        assert_eq!(apply(&zero_nine, &[]), int_array(&[0, 9]));
        assert_eq!(apply(&zero_nine, &[5]), int_array(&[0, 5, 9]));

        let zero09_nine = zero_nine.compose_over(&zero_nine);
        assert_eq!(apply(&zero09_nine, &[]), int_array(&[0, 0, 9, 9]));
        assert_eq!(
            apply(&zero09_nine, &[3, 4, 5]),
            int_array(&[0, 0, 3, 4, 5, 9, 9])
        );
    }

    #[test]
    fn vt_references_and_out_of_bounds_ops() {
        let mix_and_trim = mix_and_trim();
        assert_eq!(
            apply(&mix_and_trim, &[0, 0, 3, 4, 5, 9, 9]),
            int_array(&[0, 9, 4, 0, 9])
        );
        // Out-of-bounds operations are ignored.
        assert_eq!(apply(&mix_and_trim, &[4, 5, 6, 7]), int_array(&[5, 7]));

        let composed = mix_and_trim.compose_over(&zero_nine());
        assert_eq!(
            apply(&composed, &[1, 2, 3, 4, 5, 6, 7]),
            int_array(&[1, 9, 3, 0, 5, 6, 7])
        );
        assert_eq!(apply(&composed, &[4, 5]), int_array(&[4, 9]));
    }

    #[test]
    fn vt_size_ops() {
        let min_size10 = edit(vec![ArrayEditOp::MinSize { len: 10 }]);
        assert_eq!(apply(&min_size10, &[]), int_array(&[0; 10]));
        assert_eq!(apply(&min_size10, &[7; 15]), int_array(&[7; 15]));

        let min_size10_fill9 = edit(vec![ArrayEditOp::MinSizeFill {
            len: 10,
            fill: Value::Int(9),
        }]);
        assert_eq!(apply(&min_size10_fill9, &[]), int_array(&[9; 10]));
        assert_eq!(apply(&min_size10_fill9, &[7; 15]), int_array(&[7; 15]));

        let max_size15 = edit(vec![ArrayEditOp::MaxSize { len: 15 }]);
        assert_eq!(apply(&max_size15, &[]), int_array(&[]));
        assert_eq!(apply(&max_size15, &[2; 20]), int_array(&[2; 15]));

        let size10to15 = max_size15.compose_over(&min_size10);
        assert_eq!(
            apply(&size10to15, &[1; 7]),
            int_array(&[1, 1, 1, 1, 1, 1, 1, 0, 0, 0])
        );
        assert_eq!(apply(&size10to15, &[2; 20]), int_array(&[2; 15]));
        assert_eq!(apply(&size10to15, &[3; 13]), int_array(&[3; 13]));

        let size7 = edit(vec![ArrayEditOp::Resize { len: 7 }]);
        assert_eq!(apply(&size7, &[1; 7]), int_array(&[1; 7]));
        assert_eq!(apply(&size7, &[]), int_array(&[0; 7]));
        assert_eq!(apply(&size7, &[9; 27]), int_array(&[9; 7]));

        let size7_fill3 = edit(vec![ArrayEditOp::ResizeFill {
            len: 7,
            fill: Value::Int(3),
        }]);
        assert_eq!(apply(&size7_fill3, &[1; 7]), int_array(&[1; 7]));
        assert_eq!(apply(&size7_fill3, &[]), int_array(&[3; 7]));
        assert_eq!(apply(&size7_fill3, &[9; 27]), int_array(&[9; 7]));
    }

    #[test]
    fn vt_nested_prepend_append_composition() {
        let prepend_append = |front: i32, back: i32| {
            edit(vec![
                ArrayEditOp::Insert {
                    src: lit(front),
                    index: ArrayIndex::Position(0),
                },
                ArrayEditOp::Insert {
                    src: lit(back),
                    index: ArrayIndex::End,
                },
            ])
        };
        let composed = prepend_append(0, 9)
            .compose_over(&prepend_append(1, 8).compose_over(&prepend_append(2, 7)));
        assert_eq!(apply(&composed, &[]), int_array(&[0, 1, 2, 7, 8, 9]));
    }
}
