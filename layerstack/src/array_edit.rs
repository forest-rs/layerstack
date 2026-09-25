// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sparse array edits over USD values.
//!
//! An attribute opinion may author an edit program instead of a dense array
//! (the OpenUSD sparse-array-edits proposal, `VtArrayEdit` in OpenUSD). The
//! program and its interpreter live in [`opinionated`], generic over the
//! element type; this module fixes the element type to [`Value`] and supplies
//! the fill that growth without a literal uses: the property type's default
//! array element ([`PropertyTypeFill`]).
//!
//! Parsing and writing the authored forms belong to `layerstack_usda` and
//! `layerstack_usdc`; recognizing edits within an opinion chain and seeding
//! them with the schema fallback belong to value resolution.

use alloc::vec::Vec;

use opinionated::ArrayFill;

use crate::{doc::Value, property::PropertyType};

pub use opinionated::ArrayIndex;

/// A sparse array edit over USD values.
///
/// See [`opinionated::ArrayEdit`] for the instruction semantics.
pub type ArrayEdit = opinionated::ArrayEdit<Value>;

/// One sparse array edit instruction over USD values.
///
/// See [`opinionated::ArrayEditOp`] for each instruction's semantics.
pub type ArrayEditOp = opinionated::ArrayEditOp<Value>;

/// A sparse array edit source operand over USD values.
pub type ArrayEditOperand = opinionated::ArrayEditOperand<Value>;

/// Fills `minsize`/`resize` growth with a property type's default array
/// element.
///
/// An array-valued [`PropertyType`] supplies its default scalar
/// ([`PropertyType::default_array_element`]). Without a property type, or for
/// a scalar one, there is no fill and the growth is skipped.
#[derive(Clone, Copy, Debug)]
pub struct PropertyTypeFill<'a>(pub Option<&'a PropertyType>);

impl ArrayFill<Value> for PropertyTypeFill<'_> {
    fn fill_element(&mut self) -> Option<Value> {
        self.0.and_then(PropertyType::default_array_element)
    }
}

/// Applies `edit` over the elements of a dense array, filling growth from
/// `property_type`.
#[must_use]
pub fn apply_to_array(
    edit: &ArrayEdit,
    weaker: &[Value],
    property_type: Option<&PropertyType>,
) -> Vec<Value> {
    edit.compose_over_array(weaker, PropertyTypeFill(property_type))
}

/// Applies `edit` over a dense [`Value::Array`], filling growth from
/// `property_type`.
///
/// Returns `None` when `weaker` is not a [`Value::Array`].
#[must_use]
pub fn apply_to_value(
    edit: &ArrayEdit,
    weaker: &Value,
    property_type: Option<&PropertyType>,
) -> Option<Value> {
    let Value::Array(items) = weaker else {
        return None;
    };
    Some(Value::Array(apply_to_array(edit, items, property_type)))
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
    fn resize_uses_property_default_elements() {
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::Resize { len: 3 }],
        };

        let result = apply_to_array(&edit, &[], Some(&int_array_type()));
        assert_eq!(result, int_array(&[0, 0, 0]));
    }

    #[test]
    fn min_size_uses_property_default_elements() {
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::MinSize { len: 3 }],
        };
        let float_array = PropertyType::new(Arc::<str>::from("float"), true, Value::Float(0.5));

        let result = apply_to_array(&edit, &[Value::Float(2.0)], Some(&float_array));
        assert_eq!(
            result,
            [Value::Float(2.0), Value::Float(0.5), Value::Float(0.5)]
        );
    }

    #[test]
    fn growth_without_an_array_type_is_skipped() {
        let edit = ArrayEdit {
            ops: vec![
                ArrayEditOp::MinSize { len: 4 },
                ArrayEditOp::Resize { len: 3 },
            ],
        };
        let scalar_int = PropertyType::new(Arc::<str>::from("int"), false, Value::Int(0));

        assert_eq!(
            apply_to_array(&edit, &int_array(&[1]), None),
            int_array(&[1])
        );
        assert_eq!(
            apply_to_array(&edit, &int_array(&[1]), Some(&scalar_int)),
            int_array(&[1])
        );
        // Shrinking needs no fill.
        assert_eq!(
            apply_to_array(&edit, &int_array(&[1, 2, 3, 4, 5]), None),
            int_array(&[1, 2, 3])
        );
    }

    #[test]
    fn value_adapter_edits_only_dense_arrays() {
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(7)),
                index: ArrayIndex::End,
            }],
        };

        assert_eq!(
            apply_to_value(&edit, &Value::Array(int_array(&[1])), None),
            Some(Value::Array(int_array(&[1, 7])))
        );
        assert_eq!(apply_to_value(&edit, &Value::Int(1), None), None);
        assert_eq!(apply_to_value(&edit, &Value::Blocked, None), None);
    }
}
