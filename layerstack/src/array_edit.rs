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
// Portable instruction types for format adapters with their own literal model.
pub use opinionated::{ArrayEditOp as Instruction, ArrayEditOperand as Operand};

/// A sparse array edit over USD values.
///
/// See [`opinionated::ArrayEdit`] for the instruction semantics.
pub type ArrayEdit = opinionated::ArrayEdit<Value>;

/// A sparse edit with the actual array type carried by its authored value.
///
/// The descriptor is independent of the attribute declaration: USDC records
/// an edit's element type even when the declaration disagrees or the program
/// contains no literals. The generic program remains in [`opinionated`].
///
/// OpenUSD: `VtArrayEdit<T>`. Spec: AOUSD Core §6.2 (value types);
/// sparse-array-edits proposal, "Array Edit Value Types".
#[derive(Clone, Debug, PartialEq)]
pub struct TypedArrayEdit {
    edit: ArrayEdit,
    value_type: PropertyType,
}

impl TypedArrayEdit {
    /// Retains `edit` and its actual value type, normalizing the descriptor
    /// to an array with the canonical storage name. Semantic roles remain on
    /// the attribute declaration, as with native `VtArrayEdit<T>`. The
    /// descriptor's scalar default supplies growth fills.
    #[must_use]
    pub fn new(edit: ArrayEdit, mut value_type: PropertyType) -> Self {
        value_type.is_array = true;
        // A Vt edit's native element tag has no role aliases. USDA and USDC
        // must agree on this descriptor while preserving the property role.
        // OpenUSD: SDF_VALUE_TYPES, `_ValueHandler::PackArrayEdit`.
        let authored = value_type
            .type_name
            .strip_suffix("[]")
            .unwrap_or(&value_type.type_name);
        let canonical = match authored {
            "texCoord2f" => "float2",
            "texCoord2d" => "double2",
            "texCoord2h" => "half2",
            "point3f" | "normal3f" | "vector3f" | "color3f" | "texCoord3f" => "float3",
            "point3d" | "normal3d" | "vector3d" | "color3d" | "texCoord3d" => "double3",
            "point3h" | "normal3h" | "vector3h" | "color3h" | "texCoord3h" => "half3",
            "color4f" => "float4",
            "color4d" => "double4",
            "color4h" => "half4",
            "frame4d" => "matrix4d",
            name => name,
        };
        if canonical != &*value_type.type_name {
            value_type.type_name = alloc::sync::Arc::from(canonical);
        }
        Self { edit, value_type }
    }

    /// Borrows the generic edit program.
    #[must_use]
    pub fn edit(&self) -> &ArrayEdit {
        &self.edit
    }

    /// The actual array type, independent of an attribute's declaration.
    #[must_use]
    pub fn value_type(&self) -> &PropertyType {
        &self.value_type
    }
}

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

/// Applies `edit` over a dense array, filling growth from `property_type`.
/// Native buffers retain their element kind and use copy-on-write ownership.
///
/// Returns `None` when `weaker` is not an array.
#[must_use]
pub fn apply_to_value(
    edit: &ArrayEdit,
    weaker: &Value,
    property_type: Option<&PropertyType>,
) -> Option<Value> {
    let mut value = weaker.clone();
    apply_in_place(edit, &mut value, property_type).then_some(value)
}

/// Applies a sparse edit to either array representation, retaining unique
/// native allocations. Shared buffers copy once before mutation. Incompatible
/// literal kinds preserve the legacy heterogeneous-array behavior.
pub fn apply_in_place(
    edit: &ArrayEdit,
    value: &mut Value,
    property_type: Option<&PropertyType>,
) -> bool {
    let fill = property_type.and_then(PropertyType::default_array_element);
    match value {
        Value::TypedArray(items) => {
            if items.apply_edit(edit, fill.as_ref()) {
                return true;
            }
            let mut values = items.values().collect();
            edit.apply_in_place(&mut values, PropertyTypeFill(property_type));
            *value = Value::array(values);
            true
        }
        Value::Array(items) => {
            edit.apply_in_place(items, PropertyTypeFill(property_type));
            true
        }
        _ => false,
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
