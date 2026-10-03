// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validation for deprecated OpenUSD `RiSplineAPI` dynamic namespaces.
//!
//! This module validates authored spline records; it does not evaluate curves.
//! OpenUSD 26.8 `UsdRiSplineAPI::Validate`; AOUSD Core §12.3 (typed values).
use crate::{PrimView, Scene};
use alloc::{format, string::String, vec::Vec};
use layerstack::{PathId, PropertyKind};

/// Supported array value types for a Ri spline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiSplineValueType {
    /// float[].
    Float,
    /// color3f[].
    Color,
}
impl RiSplineValueType {
    fn type_name(self) -> &'static str {
        match self {
            Self::Float => "float",
            Self::Color => "color3f",
        }
    }
}
/// Owned values for the two supported Ri spline array types.
#[derive(Clone, Debug, PartialEq)]
pub enum RiSplineValues {
    /// Scalar float values.
    Float(Vec<f32>),
    /// RGB float values, with no color-space conversion.
    Color(Vec<[f32; 3]>),
}
/// Why an authored Ri spline cannot be used as a valid record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RiSplineError {
    /// Empty or invalid namespace components.
    InvalidName(String),
    /// The owning prim is absent.
    MissingPrim(PathId),
    /// A required attribute is missing or has no compatible default value.
    MissingAttribute(String),
    /// An attribute has the wrong declared type.
    WrongAttributeType {
        /// Property name.
        attribute: String,
        /// Expected USD type, including array suffix.
        expected: &'static str,
    },
    /// The interpolation token is unsupported.
    InvalidInterpolation(String),
    /// Positions are not nondecreasing; repeated positions are allowed.
    UnsortedPositions,
    /// Values and positions have different lengths.
    SizeMismatch {
        /// Position count.
        positions: usize,
        /// Value count.
        values: usize,
    },
    /// A position or value is NaN or infinite.
    NonFinite,
}
impl core::fmt::Display for RiSplineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Ri spline: {self:?}")
    }
}
impl core::error::Error for RiSplineError {}
/// Owned spline record for validation outside a scene.
#[derive(Clone, Debug, PartialEq)]
pub struct RiSplineData {
    /// constant, linear, catmullRom, or bspline.
    pub interpolation: String,
    /// Nondecreasing knot positions, with duplicates allowed.
    pub positions: Vec<f32>,
    /// Scalar or RGB values, one per position.
    pub values: RiSplineValues,
}
impl RiSplineData {
    /// Checks interpolation, ordering, and equal lengths as C++ does.
    /// Additionally rejects nonfinite data rather than allowing NaN to pass
    /// C++'s `std::is_sorted`. Empty records and repeated positions are valid.
    pub fn validate(&self) -> Result<(), RiSplineError> {
        if !matches!(
            self.interpolation.as_str(),
            "constant" | "linear" | "catmullRom" | "bspline"
        ) {
            return Err(RiSplineError::InvalidInterpolation(
                self.interpolation.clone(),
            ));
        }
        if !self.positions.iter().all(|v| v.is_finite()) {
            return Err(RiSplineError::NonFinite);
        }
        if self.positions.windows(2).any(|p| p[1] < p[0]) {
            return Err(RiSplineError::UnsortedPositions);
        }
        let (count, finite) = match &self.values {
            RiSplineValues::Float(v) => (v.len(), v.iter().all(|v| v.is_finite())),
            RiSplineValues::Color(v) => (v.len(), v.iter().flatten().all(|v| v.is_finite())),
        };
        if !finite {
            return Err(RiSplineError::NonFinite);
        }
        if self.positions.len() != count {
            return Err(RiSplineError::SizeMismatch {
                positions: self.positions.len(),
                values: count,
            });
        }
        Ok(())
    }
}
/// A dynamically named `<name>:spline:*` record on an existing prim.
/// No applied API schema is required, matching C++'s namespace wrapper.
#[derive(Clone, Debug)]
pub struct RiSpline<'a> {
    prim: PrimView<'a>,
    name: String,
    value_type: RiSplineValueType,
}
impl<'a> RiSpline<'a> {
    /// Opens a named record; attribute presence and types are checked by read.
    pub fn new(
        scene: &Scene<'a>,
        path: PathId,
        name: &str,
        value_type: RiSplineValueType,
    ) -> Result<Self, RiSplineError> {
        if name.is_empty() || !name.split(':').all(layerstack::ident::is_identifier) {
            return Err(RiSplineError::InvalidName(name.into()));
        }
        if !scene.stage().has_prim(path) {
            return Err(RiSplineError::MissingPrim(path));
        }
        Ok(Self {
            prim: PrimView::new(*scene, path),
            name: name.into(),
            value_type,
        })
    }
    /// A property name within this record's spline namespace.
    #[must_use]
    pub fn property_name(&self, field: &str) -> String {
        format!("{}:spline:{field}", self.name)
    }
    fn check_type(
        &self,
        name: &str,
        ty: &str,
        array: bool,
        expected: &'static str,
    ) -> Result<(), RiSplineError> {
        let scene = self.prim.scene();
        let token = scene
            .store()
            .tokens()
            .lookup(name)
            .ok_or_else(|| RiSplineError::MissingAttribute(name.into()))?;
        let declaration = scene
            .stage()
            .resolve_property_declaration(self.prim.path(), token);
        let definition = scene
            .stage()
            .property_definition_ref(self.prim.path(), token);
        let kind = declaration
            .as_ref()
            .map(|d| d.kind)
            .or_else(|| definition.map(|d| d.kind));
        if kind != Some(PropertyKind::Attribute) {
            return Err(RiSplineError::MissingAttribute(name.into()));
        }
        let declared = declaration
            .and_then(|d| d.type_name)
            .or_else(|| definition.and_then(|d| d.type_name.clone()));
        if !declared.is_some_and(|d| d.type_name.as_ref() == ty && d.is_array == array) {
            return Err(RiSplineError::WrongAttributeType {
                attribute: name.into(),
                expected,
            });
        }
        Ok(())
    }
    /// Reads a complete default-time record after checking declared types.
    /// Missing values remain errors rather than being silently empty arrays.
    pub fn read(&self) -> Result<RiSplineData, RiSplineError> {
        let interpolation = self.property_name("interpolation");
        let positions = self.property_name("positions");
        let values = self.property_name("values");
        self.check_type(&interpolation, "token", false, "token")?;
        self.check_type(&positions, "float", true, "float[]")?;
        self.check_type(
            &values,
            self.value_type.type_name(),
            true,
            match self.value_type {
                RiSplineValueType::Float => "float[]",
                RiSplineValueType::Color => "color3f[]",
            },
        )?;
        let missing = |name: &str| RiSplineError::MissingAttribute(name.into());
        Ok(RiSplineData {
            interpolation: self
                .prim
                .read_value(&interpolation, crate::value::read_token)
                .ok_or_else(|| missing(&interpolation))?
                .into(),
            positions: self
                .prim
                .read_value(&positions, crate::value::read_float_array)
                .ok_or_else(|| missing(&positions))?,
            values: match self.value_type {
                RiSplineValueType::Float => RiSplineValues::Float(
                    self.prim
                        .read_value(&values, crate::value::read_float_array)
                        .ok_or_else(|| missing(&values))?,
                ),
                RiSplineValueType::Color => RiSplineValues::Color(
                    self.prim
                        .read_value(&values, crate::value::read_float3_array)
                        .ok_or_else(|| missing(&values))?,
                ),
            },
        })
    }
    /// Validates a read record, including interpolation, ordering and sizes.
    pub fn validate(&self) -> Result<(), RiSplineError> {
        self.read()?.validate()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn cpp_validation_contract() {
        let mut data = RiSplineData {
            interpolation: "linear".into(),
            positions: vec![0., 0., 1.],
            values: RiSplineValues::Float(vec![0., 1., 2.]),
        };
        assert!(data.validate().is_ok());
        data.positions.swap(0, 2);
        assert_eq!(data.validate(), Err(RiSplineError::UnsortedPositions));
        data.positions.clear();
        assert_eq!(
            data.validate(),
            Err(RiSplineError::SizeMismatch {
                positions: 0,
                values: 3
            })
        );
        data.values = RiSplineValues::Float(vec![]);
        assert!(data.validate().is_ok());
        data.interpolation = "bezier".into();
        assert_eq!(
            data.validate(),
            Err(RiSplineError::InvalidInterpolation("bezier".into()))
        );
        data.interpolation = "bspline".into();
        data.positions.push(f32::NAN);
        assert_eq!(data.validate(), Err(RiSplineError::NonFinite));
    }
    #[test]
    fn dynamic_namespaces_read_and_check_declared_types() {
        use alloc::sync::Arc;
        use layerstack::{
            InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage,
            StageOptions, Value,
        };
        let mut store = InMemoryStore::default();
        let path = store.path("/P");
        let interpolation = store.tokens.intern("falloff:spline:interpolation");
        let linear = store.tokens.intern("linear");
        let positions = store.tokens.intern("falloff:spline:positions");
        let values = store.tokens.intern("falloff:spline:values");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            path,
            PrimSpec::def()
                .with_property(
                    interpolation,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "token",
                        false,
                        Value::Token(linear),
                    ))
                    .with_default(Value::Token(linear)),
                )
                .with_property(
                    positions,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "float",
                        true,
                        Value::Float(0.),
                    ))
                    .with_default(crate::value::write_float_array(
                        &[0., 0., 1.],
                        &mut store.tokens,
                    )),
                )
                .with_property(
                    values,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "float",
                        true,
                        Value::Float(0.),
                    ))
                    .with_default(crate::value::write_float_array(
                        &[0., 1., 2.],
                        &mut store.tokens,
                    )),
                ),
        );
        store.insert_layer(layer);
        let schemas = crate::openusd(&mut store.tokens);
        let stage = Stage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(Arc::new(schemas)),
                ..StageOptions::default()
            },
        );
        let scene = Scene::new(&stage, &store);
        let spline = RiSpline::new(&scene, path, "falloff", RiSplineValueType::Float).unwrap();
        assert!(spline.validate().is_ok());
        assert_eq!(spline.read().unwrap().positions, vec![0., 0., 1.]);
        assert!(matches!(
            RiSpline::new(&scene, path, "falloff", RiSplineValueType::Color)
                .unwrap()
                .validate(),
            Err(RiSplineError::WrongAttributeType {
                expected: "color3f[]",
                ..
            })
        ));
        assert!(matches!(
            RiSpline::new(&scene, path, "missing", RiSplineValueType::Float)
                .unwrap()
                .validate(),
            Err(RiSplineError::MissingAttribute(_))
        ));
        assert!(matches!(
            RiSpline::new(&scene, path, "", RiSplineValueType::Float),
            Err(RiSplineError::InvalidName(_))
        ));
    }
}
