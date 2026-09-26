// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored-state identity for edit guards.
//!
//! A guard asks whether a slot still holds exactly what was authored
//! there, not whether two values are numerically equal. The derived
//! `PartialEq` of [`Value`] uses IEEE float equality, under which an
//! unchanged NaN differs from itself and `+0.0` equals `-0.0`; either
//! would make a guard wrong. [`Same`] compares every float, at any depth
//! (scalars, vectors, matrices, quaternions, arrays, dictionaries, sparse
//! array edits, time samples, splines), by its bit pattern, and everything
//! else by ordinary equality.
//!
//! OpenUSD keeps authored values as written, and its file formats
//! preserve their bits, so a signed zero or a NaN payload is authored
//! state an edit can change.

use alloc::vec::Vec;

use hashbrown::HashMap;

use crate::{
    array_edit::{ArrayEditOp, ArrayEditOperand},
    doc::{FieldEntry, FieldValue, PrimSpec, Value, VariantSetSpec, VariantSpec},
    property::{PropertyEntry, PropertySpec, PropertyType},
    spline::{Extrapolation, Knot, LoopParams, SplineData},
};

/// Authored-state identity: equal structure, and every float with the
/// same bit pattern. See the [module docs](self).
pub(crate) trait Same {
    /// Returns `true` when `self` and `other` are the same authored state.
    fn same(&self, other: &Self) -> bool;
}

impl Same for f64 {
    fn same(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl Same for f32 {
    fn same(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl<T: Same, const N: usize> Same for [T; N] {
    fn same(&self, other: &Self) -> bool {
        self.iter().zip(other).all(|(a, b)| a.same(b))
    }
}

impl<T: Same> Same for [T] {
    fn same(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a.same(b))
    }
}

impl<T: Same> Same for Vec<T> {
    fn same(&self, other: &Self) -> bool {
        self.as_slice().same(other.as_slice())
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

impl<A: Same, B: Same> Same for (A, B) {
    fn same(&self, other: &Self) -> bool {
        self.0.same(&other.0) && self.1.same(&other.1)
    }
}

impl<K: Eq + core::hash::Hash, V: Same> Same for HashMap<K, V> {
    fn same(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(key, value)| other.get(key).is_some_and(|o| value.same(o)))
    }
}

impl Same for Value {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Float(a), Self::Float(b)) => a.same(b),
            (Self::Double(a), Self::Double(b)) | (Self::TimeCode(a), Self::TimeCode(b)) => {
                a.same(b)
            }
            (Self::Vec2d(a), Self::Vec2d(b)) => a.same(b),
            (Self::Vec3d(a), Self::Vec3d(b)) => a.same(b),
            (Self::Vec4d(a), Self::Vec4d(b)) | (Self::Quatd(a), Self::Quatd(b)) => a.same(b),
            (Self::Vec2f(a), Self::Vec2f(b)) => a.same(b),
            (Self::Vec3f(a), Self::Vec3f(b)) => a.same(b),
            (Self::Vec4f(a), Self::Vec4f(b)) | (Self::Quatf(a), Self::Quatf(b)) => a.same(b),
            (Self::Matrix2d(a), Self::Matrix2d(b)) => a.same(b),
            (Self::Matrix3d(a), Self::Matrix3d(b)) => a.same(b),
            (Self::Matrix4d(a), Self::Matrix4d(b)) => a.same(b),
            (Self::Array(a), Self::Array(b)) => a.same(b),
            (Self::Dictionary(a), Self::Dictionary(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ka, va), (kb, vb))| ka == kb && va.same(vb))
            }
            (Self::ArrayEdit(a), Self::ArrayEdit(b)) => a.ops.same(&b.ops),
            // Halves are stored as raw bits already; the rest hold no float.
            _ => self == other,
        }
    }
}

impl Same for ArrayEditOperand {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Literal(a), Self::Literal(b)) => a.same(b),
            _ => self == other,
        }
    }
}

impl Same for ArrayEditOp {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Write { src: a, index: i }, Self::Write { src: b, index: j })
            | (Self::Insert { src: a, index: i }, Self::Insert { src: b, index: j }) => {
                i == j && a.same(b)
            }
            (Self::MinSizeFill { len: m, fill: a }, Self::MinSizeFill { len: n, fill: b })
            | (Self::ResizeFill { len: m, fill: a }, Self::ResizeFill { len: n, fill: b }) => {
                m == n && a.same(b)
            }
            _ => self == other,
        }
    }
}

impl Same for FieldValue {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Value(a), Self::Value(b)) => a.same(b),
            _ => self == other,
        }
    }
}

impl Same for FieldEntry {
    fn same(&self, other: &Self) -> bool {
        self.name == other.name && self.value.same(&other.value)
    }
}

impl Same for PropertyType {
    fn same(&self, other: &Self) -> bool {
        self.type_name == other.type_name
            && self.is_array == other.is_array
            && self.default_scalar.same(&other.default_scalar)
    }
}

impl Same for Extrapolation {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Sloped(a), Self::Sloped(b)) => a.same(b),
            _ => self == other,
        }
    }
}

impl Same for LoopParams {
    fn same(&self, other: &Self) -> bool {
        self.proto_start.same(&other.proto_start)
            && self.proto_end.same(&other.proto_end)
            && self.num_pre_loops == other.num_pre_loops
            && self.num_post_loops == other.num_post_loops
            && self.value_offset.same(&other.value_offset)
    }
}

impl Same for Knot {
    fn same(&self, other: &Self) -> bool {
        self.time.same(&other.time)
            && self.value.same(&other.value)
            && self.pre_value.same(&other.pre_value)
            && self.next_interp == other.next_interp
            && self.curve_type == other.curve_type
            && self.pre_tan_maya_form == other.pre_tan_maya_form
            && self.post_tan_maya_form == other.post_tan_maya_form
            && self.pre_tan_width.same(&other.pre_tan_width)
            && self.post_tan_width.same(&other.post_tan_width)
            && self.pre_tan_slope.same(&other.pre_tan_slope)
            && self.post_tan_slope.same(&other.post_tan_slope)
    }
}

impl Same for SplineData {
    fn same(&self, other: &Self) -> bool {
        self.data_type == other.data_type
            && self.default_curve_type == other.default_curve_type
            && self.pre_extrapolation.same(&other.pre_extrapolation)
            && self.post_extrapolation.same(&other.post_extrapolation)
            && self.loop_params.same(&other.loop_params)
            && self.knots.same(&other.knots)
    }
}

impl Same for PropertySpec {
    fn same(&self, other: &Self) -> bool {
        let Self {
            kind,
            custom,
            variability,
            type_name,
            default,
            time_samples,
            spline,
            targets,
            metadata,
        } = self;
        *kind == other.kind
            && *custom == other.custom
            && *variability == other.variability
            && type_name.same(&other.type_name)
            && default.same(&other.default)
            && time_samples.same(&other.time_samples)
            && spline.same(&other.spline)
            && *targets == other.targets
            && metadata.same(&other.metadata)
    }
}

impl Same for PropertyEntry {
    fn same(&self, other: &Self) -> bool {
        self.name == other.name && self.spec.same(&other.spec)
    }
}

impl Same for VariantSpec {
    fn same(&self, other: &Self) -> bool {
        let Self {
            fields,
            properties,
            authored_children,
            references,
            inherits,
            specializes,
            payloads,
            variant_selections,
            variant_sets,
            variant_set_order,
            property_order,
        } = self;
        fields.same(&other.fields)
            && properties.same(&other.properties)
            && *authored_children == other.authored_children
            && *references == other.references
            && *inherits == other.inherits
            && *specializes == other.specializes
            && *payloads == other.payloads
            && *variant_selections == other.variant_selections
            && variant_sets.same(&other.variant_sets)
            && *variant_set_order == other.variant_set_order
            && *property_order == other.property_order
    }
}

impl Same for VariantSetSpec {
    fn same(&self, other: &Self) -> bool {
        self.variants.same(&other.variants)
    }
}

impl Same for PrimSpec {
    fn same(&self, other: &Self) -> bool {
        let Self {
            specifier,
            type_name,
            fields,
            properties,
            property_order,
            outer_variant_sites,
            authored_children,
            variant_selections,
            variant_sets,
            variant_set_order,
            deleted_variant_sets,
            references,
            inherits,
            specializes,
            payloads,
            prim_order,
            instanceable,
            active,
        } = self;
        *specifier == other.specifier
            && *type_name == other.type_name
            && fields.same(&other.fields)
            && properties.same(&other.properties)
            && *property_order == other.property_order
            && *outer_variant_sites == other.outer_variant_sites
            && *authored_children == other.authored_children
            && *variant_selections == other.variant_selections
            && variant_sets.same(&other.variant_sets)
            && *variant_set_order == other.variant_set_order
            && *deleted_variant_sets == other.deleted_variant_sets
            && *references == other.references
            && *inherits == other.inherits
            && *specializes == other.specializes
            && *payloads == other.payloads
            && *prim_order == other.prim_order
            && *instanceable == other.instanceable
            && *active == other.active
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn floats_compare_by_bits_at_any_depth() {
        let nan = f64::from_bits(0x7ff8_0000_0000_0123);
        let other_nan = f64::from_bits(0x7ff8_0000_0000_0456);
        assert!(Value::Double(nan).same(&Value::Double(nan)));
        assert!(!Value::Double(nan).same(&Value::Double(other_nan)));
        assert!(!Value::Double(0.0).same(&Value::Double(-0.0)));
        assert!(!Value::Float(0.0).same(&Value::Float(-0.0)));
        let array = |x: f32| Value::Array(vec![Value::Vec3f([1.0, x, 2.0])]);
        assert!(array(f32::NAN).same(&array(f32::NAN)));
        assert!(!array(0.0).same(&array(-0.0)));
        let dict = |x| Value::Dictionary(vec![("k".into(), Value::Double(x))]);
        assert!(dict(nan).same(&dict(nan)));
        assert!(!dict(0.0).same(&dict(-0.0)));
        let spec = |x| PropertySpec::attribute().with_time_samples(vec![(x, Value::Double(x))]);
        assert!(spec(nan).same(&spec(nan)));
        assert!(!spec(0.0).same(&spec(-0.0)));
        assert!(Value::string("a").same(&Value::string("a")));
        assert!(!Value::Int(1).same(&Value::Int(2)));
    }
}
