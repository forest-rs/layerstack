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
    /// Grow the array to at least `len`.
    MinSize {
        /// Minimum number of elements after editing.
        len: usize,
    },
    /// Shrink the array to at most `len`.
    MaxSize {
        /// Maximum number of elements after editing.
        len: usize,
    },
    /// Resize the array to exactly `len`.
    Resize {
        /// Final number of elements after editing.
        len: usize,
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

    while array.len() < len {
        array.push(fill.clone());
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
}
