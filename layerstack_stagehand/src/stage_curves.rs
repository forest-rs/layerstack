// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reading curves and points off a composed stage.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::{PathId, PrimPathArg, Time};
use layerstack_schemas::usd_geom::{BasisCurves, Curves, HermiteCurves, NurbsCurves, Points};
use layerstack_schemas::value::{read_float_array_shared, read_float3_array_shared, read_token};
use layerstack_schemas::{PrimView, Scene};

use crate::curves::{CurveForm, CurvesInput};
use crate::input::Interpolation;
use crate::points::PointsInput;
use crate::stage::{Owned, Read, primvar, same_primvar};

/// A curve schema's own attributes, as read.
#[derive(Clone, Debug, PartialEq)]
enum Form {
    Basis {
        curve_type: String,
        basis: String,
        wrap: String,
    },
    Nurbs {
        order: Arc<Vec<i32>>,
        knots: Arc<Vec<f64>>,
        ranges: Arc<Vec<[f64; 2]>>,
        weights: Arc<Vec<f64>>,
    },
    Hermite {
        tangents: Arc<Vec<[f32; 3]>>,
    },
}

/// A `BasisCurves`, `NurbsCurves` or `HermiteCurves` read off a composed
/// stage at one time, holding the stage's shared arrays;
/// [`Self::input`] lends them to a [`crate::CurvesPlan`].
///
/// What it reads (`UsdGeomCurves`, `UsdGeomBasisCurves`,
/// `UsdGeomNurbsCurves`, `UsdGeomHermiteCurves`):
///
/// - `curveVertexCounts`, `points`; `type`, `basis` and `wrap` with their
///   schema fallbacks (`cubic`, `bezier`, `nonperiodic`); `order`,
///   `knots`, `ranges` and `pointWeights`; `tangents`;
/// - widths: the `primvars:widths` primvar when it has a value, else the
///   `widths` attribute with its `interpolation` (`vertex` when
///   unauthored, `UsdGeomCurves::GetWidthsInterpolation`);
/// - normals: `primvars:normals`, else `normals` (`vertex` when
///   unauthored);
/// - `primvars:displayColor` and `primvars:displayOpacity`, their own or a
///   constant one inherited from an ancestor.
///
/// A primvar of another type is left out and named in [`Self::unread`].
#[derive(Clone, Debug)]
pub struct StageCurves {
    form: Form,
    vertex_counts: Arc<Vec<i32>>,
    points: Arc<Vec<[f32; 3]>>,
    widths: Option<Owned<f32>>,
    normals: Option<Owned<[f32; 3]>>,
    colors: Option<Owned<[f32; 3]>>,
    opacities: Option<Owned<f32>>,
    unread: Vec<&'static str>,
}

fn token(prim: &PrimView<'_>, name: &str, time: Time, fallback: &str) -> String {
    prim.read_value_at(name, time, read_token)
        .unwrap_or(fallback)
        .into()
}

/// A point-based attribute with interpolation metadata: the primvar of
/// that name when it has a value, else the attribute.
fn interpolated<T>(
    scene: &Scene<'_>,
    view: &PrimView<'_>,
    name: &'static str,
    unread_name: &'static str,
    time: Time,
    read: fn(&layerstack::Value, &layerstack::TokenInterner) -> Option<Arc<Vec<T>>>,
    unread: &mut Vec<&'static str>,
) -> Option<Owned<T>> {
    let prim = view.path();
    match primvar(scene, prim, name, false, time, read) {
        Read::Value(value) => Some(value),
        Read::Unread => {
            unread.push(unread_name);
            None
        }
        Read::Absent => view.read_value_at(name, time, read).map(|values| Owned {
            values,
            indices: None,
            interpolation: view
                .property_metadata(name)
                .and_then(|metadata| metadata.interpolation())
                .and_then(Interpolation::from_token)
                .unwrap_or(Interpolation::Vertex),
            element_size: 1,
        }),
    }
}

fn display(
    scene: &Scene<'_>,
    prim: PathId,
    time: Time,
    unread: &mut Vec<&'static str>,
) -> (Option<Owned<[f32; 3]>>, Option<Owned<f32>>) {
    let colors = primvar(
        scene,
        prim,
        "displayColor",
        true,
        time,
        read_float3_array_shared,
    )
    .keep("primvars:displayColor", unread);
    let opacities = primvar(
        scene,
        prim,
        "displayOpacity",
        true,
        time,
        read_float_array_shared,
    )
    .keep("primvars:displayOpacity", unread);
    (colors, opacities)
}

impl StageCurves {
    /// The curves at `path` at `time`; `None` when there is no
    /// `BasisCurves`, `NurbsCurves` or `HermiteCurves` there.
    #[must_use]
    pub fn read(scene: &Scene<'_>, path: impl PrimPathArg, time: Time) -> Option<Self> {
        let prim = scene.prim_path(path)?;
        let form = if let Some(curves) = BasisCurves::get(scene, prim) {
            Form::Basis {
                curve_type: token(&curves, "type", time, "cubic"),
                basis: token(&curves, "basis", time, "bezier"),
                wrap: token(&curves, "wrap", time, "nonperiodic"),
            }
        } else if let Some(curves) = NurbsCurves::get(scene, prim) {
            Form::Nurbs {
                order: curves.order_at(time).unwrap_or_default(),
                knots: curves.knots_at(time).unwrap_or_default(),
                ranges: curves.ranges_at(time).unwrap_or_default(),
                weights: curves.point_weights_at(time).unwrap_or_default(),
            }
        } else {
            let curves = HermiteCurves::get(scene, prim)?;
            Form::Hermite {
                tangents: curves.tangents_at(time).unwrap_or_default(),
            }
        };
        let curves = Curves::get(scene, prim)?;
        let mut unread = Vec::new();
        let widths = interpolated(
            scene,
            &curves,
            "widths",
            "primvars:widths",
            time,
            read_float_array_shared,
            &mut unread,
        );
        let normals = interpolated(
            scene,
            &curves,
            "normals",
            "primvars:normals",
            time,
            read_float3_array_shared,
            &mut unread,
        );
        let (colors, opacities) = display(scene, prim, time, &mut unread);
        Some(Self {
            form,
            vertex_counts: curves.curve_vertex_counts_at(time).unwrap_or_default(),
            points: curves.points_at(time).unwrap_or_default(),
            widths,
            normals,
            colors,
            opacities,
            unread,
        })
    }

    /// The input of a [`crate::CurvesPlan`], borrowing these arrays.
    #[must_use]
    pub fn input(&self) -> CurvesInput<'_> {
        CurvesInput {
            form: match &self.form {
                Form::Basis {
                    curve_type,
                    basis,
                    wrap,
                } => CurveForm::Basis {
                    curve_type,
                    basis,
                    wrap,
                },
                Form::Nurbs {
                    order,
                    knots,
                    ranges,
                    weights,
                } => CurveForm::Nurbs {
                    order,
                    knots,
                    ranges,
                    weights,
                },
                Form::Hermite { tangents } => CurveForm::Hermite { tangents },
            },
            vertex_counts: &self.vertex_counts,
            points: &self.points,
            widths: self.widths.as_ref().map(Owned::primvar),
            normals: self.normals.as_ref().map(Owned::primvar),
            colors: self.colors.as_ref().map(Owned::primvar),
            opacities: self.opacities.as_ref().map(Owned::primvar),
        }
    }

    /// Primvars of another type than drawn, by name.
    #[must_use]
    pub fn unread(&self) -> &[&'static str] {
        &self.unread
    }

    /// Whether `other` holds the same arrays, value for value: then the
    /// buffers are unchanged.
    pub(crate) fn same_values(&self, other: &Self) -> bool {
        let (a, b) = (self.input(), other.input());
        a.form == b.form
            && a.vertex_counts == b.vertex_counts
            && a.points == b.points
            && same_primvar(a.widths.as_ref(), b.widths.as_ref())
            && same_primvar(a.normals.as_ref(), b.normals.as_ref())
            && same_primvar(a.colors.as_ref(), b.colors.as_ref())
            && same_primvar(a.opacities.as_ref(), b.opacities.as_ref())
    }
}

/// A `Points` read off a composed stage at one time: `points`, widths and
/// normals (each the primvar when it has a value, else the attribute, at
/// its interpolation, `vertex` when unauthored), `ids`, and
/// `primvars:displayColor` and `primvars:displayOpacity` (inherited when
/// constant). [`Self::input`] lends them to [`crate::fill_points`].
#[derive(Clone, Debug)]
pub struct StagePoints {
    points: Arc<Vec<[f32; 3]>>,
    widths: Option<Owned<f32>>,
    normals: Option<Owned<[f32; 3]>>,
    colors: Option<Owned<[f32; 3]>>,
    opacities: Option<Owned<f32>>,
    ids: Arc<Vec<i64>>,
    unread: Vec<&'static str>,
}

impl StagePoints {
    /// The points at `path` at `time`; `None` when there is no `Points`
    /// there.
    #[must_use]
    pub fn read(scene: &Scene<'_>, path: impl PrimPathArg, time: Time) -> Option<Self> {
        let points = Points::get(scene, path)?;
        let prim = points.path();
        let mut unread = Vec::new();
        let widths = interpolated(
            scene,
            &points,
            "widths",
            "primvars:widths",
            time,
            read_float_array_shared,
            &mut unread,
        );
        let normals = interpolated(
            scene,
            &points,
            "normals",
            "primvars:normals",
            time,
            read_float3_array_shared,
            &mut unread,
        );
        let (colors, opacities) = display(scene, prim, time, &mut unread);
        Some(Self {
            points: points.points_at(time).unwrap_or_default(),
            widths,
            normals,
            colors,
            opacities,
            ids: points.ids_at(time).unwrap_or_default(),
            unread,
        })
    }

    /// The input of [`crate::fill_points`], borrowing these arrays.
    #[must_use]
    pub fn input(&self) -> PointsInput<'_> {
        PointsInput {
            points: &self.points,
            widths: self.widths.as_ref().map(Owned::primvar),
            normals: self.normals.as_ref().map(Owned::primvar),
            colors: self.colors.as_ref().map(Owned::primvar),
            opacities: self.opacities.as_ref().map(Owned::primvar),
            ids: &self.ids,
        }
    }

    /// Primvars of another type than drawn, by name.
    #[must_use]
    pub fn unread(&self) -> &[&'static str] {
        &self.unread
    }

    /// Whether `other` holds the same arrays, value for value.
    pub(crate) fn same_values(&self, other: &Self) -> bool {
        let (a, b) = (self.input(), other.input());
        a.points == b.points
            && a.ids == b.ids
            && same_primvar(a.widths.as_ref(), b.widths.as_ref())
            && same_primvar(a.normals.as_ref(), b.normals.as_ref())
            && same_primvar(a.colors.as_ref(), b.colors.as_ref())
            && same_primvar(a.opacities.as_ref(), b.opacities.as_ref())
    }
}
