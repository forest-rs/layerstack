// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Common translation, pivot, Euler rotation and scale authoring.

use crate::xform::{INVERT_PREFIX, RESET_XFORM_STACK};
use crate::{SchemaEdit, XformOpPrecision, XformOpType, XformOpValue, usd_geom::XformableEdit};
use alloc::{string::String, vec::Vec};

/// The order in which a common transform applies its Euler rotations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RotationOrder {
    /// X, then Y, then Z.
    #[default]
    Xyz,
    /// X, then Z, then Y.
    Xzy,
    /// Y, then X, then Z.
    Yxz,
    /// Y, then Z, then X.
    Yzx,
    /// Z, then X, then Y.
    Zxy,
    /// Z, then Y, then X.
    Zyx,
}
impl RotationOrder {
    fn op_type(self) -> XformOpType {
        match self {
            Self::Xyz => XformOpType::RotateXyz,
            Self::Xzy => XformOpType::RotateXzy,
            Self::Yxz => XformOpType::RotateYxz,
            Self::Yzx => XformOpType::RotateYzx,
            Self::Zxy => XformOpType::RotateZxy,
            Self::Zyx => XformOpType::RotateZyx,
        }
    }
    fn from_type(value: XformOpType) -> Option<Self> {
        Some(match value {
            XformOpType::RotateXyz => Self::Xyz,
            XformOpType::RotateXzy => Self::Xzy,
            XformOpType::RotateYxz => Self::Yxz,
            XformOpType::RotateYzx => Self::Yzx,
            XformOpType::RotateZxy => Self::Zxy,
            XformOpType::RotateZyx => Self::Zyx,
            _ => return None,
        })
    }
}

/// Values authored by `UsdGeomXformCommonAPI::SetXformVectors`.
/// The stack order is translation, pivot, rotation, scale, inverse pivot;
/// row vectors therefore apply scale and rotation about the pivot first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommonTransform {
    /// Translation, in double precision.
    pub translation: [f64; 3],
    /// Euler angles in degrees, about X/Y/Z respectively.
    pub rotation: [f32; 3],
    /// Scale along X/Y/Z.
    pub scale: [f32; 3],
    /// Pivot for rotation and scale.
    pub pivot: [f32; 3],
    /// Euler rotation order; an existing rotation op must agree.
    pub rotation_order: RotationOrder,
}
impl Default for CommonTransform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: [1.0; 3],
            pivot: [0.0; 3],
            rotation_order: RotationOrder::default(),
        }
    }
}

/// A common-transform edit rejected before authoring any operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommonTransformError {
    /// The ordered operations are not a subsequence of a common stack, or
    /// a pivot is missing its inverse. Missing ordered attributes are rejected.
    IncompatibleOps,
    /// An existing rotation op cannot change its order through this helper.
    RotationOrderMismatch {
        /// The existing order.
        existing: RotationOrder,
        /// The requested order.
        requested: RotationOrder,
    },
    /// A participating attribute has a non-vector numeric type.
    IncompatibleAttribute {
        /// The attribute that must be repaired or removed explicitly.
        name: String,
    },
}
impl core::fmt::Display for CommonTransformError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::IncompatibleOps => f.write_str("transform ops are not a compatible common stack"),
            Self::RotationOrderMismatch {
                existing,
                requested,
            } => write!(
                f,
                "rotation order {existing:?} differs from requested {requested:?}"
            ),
            Self::IncompatibleAttribute { name } => {
                write!(f, "{name} must be a float3, double3 or half3 attribute")
            }
        }
    }
}
impl core::error::Error for CommonTransformError {}

impl XformableEdit {
    /// Author a common TRS/pivot stack at default time, preserving its reset flag.
    ///
    /// Compatible partial stacks gain missing ops in canonical order. New
    /// translation uses double precision; rotation, scale and pivot use float.
    /// Existing vector precisions are retained. This is the authoring portion
    /// of OpenUSD's `UsdGeomXformCommonAPI::SetXformVectors`, not matrix
    /// decomposition or automatic conversion of arbitrary transform stacks.
    ///
    /// # Errors
    /// No operations are authored when the stack, rotation order or attribute
    /// types are incompatible. Repair malformed ordered attributes explicitly.
    pub fn set_common_transform(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &CommonTransform,
    ) -> Result<&Self, CommonTransformError> {
        self.write_common_transform(edit, value, None)
    }

    /// [`Self::set_common_transform`] authoring values at a numeric time.
    /// Op declarations and `xformOpOrder` remain uniform.
    ///
    /// # Errors
    /// As for [`Self::set_common_transform`], before authoring anything.
    pub fn set_common_transform_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &CommonTransform,
    ) -> Result<&Self, CommonTransformError> {
        self.write_common_transform(edit, value, Some(time))
    }

    fn write_common_transform(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &CommonTransform,
        time: Option<f64>,
    ) -> Result<&Self, CommonTransformError> {
        // UsdGeomXformCommonAPI::_GetCommonXformOps: optional ordered TRS
        // entries, with both pivot entries present or both absent.
        let order = self.order(edit);
        let reset = order.iter().rposition(|op| op == RESET_XFORM_STACK);
        let effective = &order[reset.map_or(0, |i| i + 1)..];
        let mut names = [
            String::from("xformOp:translate"),
            String::from("xformOp:translate:pivot"),
            value.rotation_order.op_type().attribute_name(None),
            String::from("xformOp:scale"),
            String::from("!invert!xformOp:translate:pivot"),
        ];
        let mut present = [false; 5];
        let mut previous = None;
        for name in effective {
            let index = match name.as_str() {
                "xformOp:translate" => 0,
                "xformOp:translate:pivot" => 1,
                "xformOp:scale" => 3,
                "!invert!xformOp:translate:pivot" => 4,
                _ => {
                    if !name.starts_with("xformOp:") {
                        return Err(CommonTransformError::IncompatibleOps);
                    }
                    let existing = XformOpType::of_attribute(name)
                        .and_then(RotationOrder::from_type)
                        .ok_or(CommonTransformError::IncompatibleOps)?;
                    if existing != value.rotation_order {
                        return Err(CommonTransformError::RotationOrderMismatch {
                            existing,
                            requested: value.rotation_order,
                        });
                    }
                    names[2] = name.clone();
                    2
                }
            };
            if previous.is_some_and(|previous| index <= previous) {
                return Err(CommonTransformError::IncompatibleOps);
            }
            previous = Some(index);
            present[index] = true;
        }
        if present[1] != present[4] {
            return Err(CommonTransformError::IncompatibleOps);
        }
        for (i, name) in names.iter().enumerate() {
            let attribute = name.strip_prefix(INVERT_PREFIX).unwrap_or(name);
            if let Some(ty) = edit.attribute_type(self.path(), attribute) {
                if ty.is_array || !matches!(ty.type_name.as_ref(), "float3" | "double3" | "half3") {
                    return Err(CommonTransformError::IncompatibleAttribute {
                        name: attribute.into(),
                    });
                }
            } else if present[i] {
                return Err(CommonTransformError::IncompatibleOps);
            }
            if !present[i] && order.contains(name) {
                return Err(CommonTransformError::IncompatibleOps);
            }
        }
        let kinds = [
            XformOpType::Translate,
            XformOpType::Translate,
            value.rotation_order.op_type(),
            XformOpType::Scale,
            XformOpType::Translate,
        ];
        let values = [
            value.translation,
            value.pivot.map(f64::from),
            value.rotation.map(f64::from),
            value.scale.map(f64::from),
        ];
        for i in 0..5 {
            let suffix = if i == 1 || i == 4 {
                Some("pivot")
            } else if i == 2 {
                names[i].splitn(3, ':').nth(2)
            } else {
                None
            };
            let handle = if present[i] {
                self.get_op(edit, kinds[i], suffix, i == 4)
                    .expect("validated common op")
            } else {
                self.add_op(
                    edit,
                    kinds[i],
                    if i == 0 {
                        XformOpPrecision::Double
                    } else {
                        XformOpPrecision::Float
                    },
                    suffix,
                    i == 4,
                )
                .expect("preflight excludes duplicate ops")
            };
            if i < 4 {
                let value = XformOpValue::Vector(values[i]);
                if let Some(time) = time {
                    handle.set_at(edit, time, value)
                } else {
                    handle.set(edit, value)
                }
                .expect("common ops accept vector values");
            }
        }
        if present.contains(&false) {
            let mut canonical: Vec<String> = Vec::with_capacity(6);
            if reset.is_some() {
                canonical.push(RESET_XFORM_STACK.into());
            }
            canonical.extend(names);
            self.set_order(edit, &canonical);
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usd_geom::Xform;
    use alloc::sync::Arc;
    use layerstack::{
        InMemoryStore, Layer, LayerId, LiveStage, PropertyType, StageOptions, Value,
        edit::EditTarget,
    };

    #[test]
    fn malformed_common_stacks_are_rejected_before_authoring() {
        for case in 0..4 {
            let mut store = InMemoryStore::default();
            store.insert_layer(Layer::new(LayerId(1)));
            let path = store.path("/Rig");
            let options = StageOptions {
                schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
                ..StageOptions::default()
            };
            let live = LiveStage::compose(&mut store, LayerId(1), options);
            let mut edit =
                SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
            Xform::define(&mut edit, path);
            let xform = XformableEdit::new(&edit, path).unwrap();
            match case {
                0 => {
                    xform
                        .add_op(
                            &mut edit,
                            XformOpType::RotateXyz,
                            XformOpPrecision::Float,
                            None,
                            true,
                        )
                        .unwrap();
                }
                1 => {
                    xform.add_transform_op(&mut edit).unwrap();
                }
                2 => {
                    edit.create_attribute(
                        path,
                        "xformOp:scale",
                        PropertyType::new("float3", true, Value::Vec3f([0.0; 3])),
                    );
                    xform.set_xform_op_order(&mut edit, &["xformOp:scale"]);
                }
                _ => {
                    xform.set_xform_op_order(&mut edit, &["xformOp:translate"]);
                }
            }
            let before = edit.transaction().clone();
            assert!(
                xform
                    .set_common_transform(&mut edit, &CommonTransform::default())
                    .is_err()
            );
            assert_eq!(edit.transaction(), &before);
        }
    }

    #[test]
    fn existing_vector_precisions_are_preserved() {
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(LayerId(1)));
        let path = store.path("/Rig");
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
            ..StageOptions::default()
        };
        let live = LiveStage::compose(&mut store, LayerId(1), options);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        Xform::define(&mut edit, path);
        let xform = XformableEdit::new(&edit, path).unwrap();
        xform
            .add_translate_op(&mut edit, XformOpPrecision::Half)
            .unwrap();
        xform
            .add_scale_op(&mut edit, XformOpPrecision::Double)
            .unwrap();
        xform
            .set_common_transform(
                &mut edit,
                &CommonTransform {
                    translation: [1.0, 2.0, 3.0],
                    ..CommonTransform::default()
                },
            )
            .unwrap();
        assert_eq!(
            edit.default_value(path, "xformOp:translate"),
            Some(Value::Vec3h(
                [1.0, 2.0, 3.0].map(layerstack::half::from_f32)
            ))
        );
        assert_eq!(
            edit.default_value(path, "xformOp:scale"),
            Some(Value::Vec3d([1.0; 3]))
        );
    }
}
