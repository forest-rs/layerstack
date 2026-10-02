// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Intrinsic bounds for the built-in geometry supported by `BoundsCache`.

use crate::{PrimView, Scene, Time, bounds::Range3d};
use layerstack::{PathId, TokenInterner, Value};

/// OpenUSD `UsdGeomBoundable::ComputeExtent`: a valid authored extent wins;
/// schema fallback extents do not replace evaluation of intrinsic geometry.
/// Value resolution (including blocks) follows AOUSD Core §12.3.
#[allow(
    clippy::cast_possible_truncation,
    reason = "USD extents are float3 even for double geometry parameters"
)]
pub(crate) fn compute(scene: &Scene<'_>, path: PathId, time: Time) -> Option<(Range3d, bool)> {
    let prim = PrimView::new(*scene, path);
    let extent_varying = prim.property_might_vary("extent");
    if prim.has_authored_value("extent")
        && let Some(range) = read(&prim, "extent", time, |value, tokens| {
            let values = value.array_ref()?;
            if let Some(typed) = values.typed() {
                typed.as_vec3f()?;
            } else if !values
                .iter()
                .all(|value| matches!(&*value, Value::Vec3f(_)))
            {
                return None;
            }
            // A float3[] of the wrong length is compatible but malformed.
            // Accept its type before checking shape, so a default-time read
            // cannot replace malformed authored data with a schema fallback.
            if values.len() != 2 {
                return Some(None);
            }
            Some(Some(Range3d {
                min: crate::value::read_float3(values.get(0)?.as_ref(), tokens)?.map(f64::from),
                max: crate::value::read_float3(values.get(1)?.as_ref(), tokens)?.map(f64::from),
            }))
        })
        .flatten()
    {
        return Some((range, extent_varying));
    }
    let varying =
        |names: &[&str]| extent_varying || names.iter().any(|name| prim.property_might_vary(name));
    let points_schema = scene.is_a(path, "Points");
    let curves = scene.is_a(path, "Curves");
    if scene.is_a(path, "Mesh") || points_schema || curves {
        let points = read(&prim, "points", time, crate::value::read_float3_array)?;
        let widths = if points_schema || curves {
            read(&prim, "widths", time, crate::value::read_float_array)
        } else {
            None
        };
        // UsdGeomPoints::_ComputeExtent requires one readable width per point.
        // Unreadable widths fall back to point-based bounds (AOUSD Core §12.3).
        if points_schema && widths.as_ref().is_some_and(|w| w.len() != points.len()) {
            return None;
        }
        let mut range = Range3d::default();
        for (index, point) in points.iter().enumerate() {
            let half_width = if points_schema {
                widths.as_ref().map_or(0., |w| w[index] * 0.5)
            } else {
                0.
            };
            // GfVec3f rounds before the double-precision union. Union both
            // corners, including signed widths, as the C++ point provider does.
            for sign in [-1., 1.] {
                let corner = point.map(|v| f64::from(v + sign * half_width));
                range.union_with(Range3d {
                    min: corner,
                    max: corner,
                });
            }
        }
        if curves {
            // UsdGeomCurves::ComputeExtent uses the control hull plus maximum
            // width. Catmull-Rom and Hermite keep the same approximation as C++,
            // which can miss overshoot; this is not an analytic spline bound.
            let half_width = widths
                .as_ref()
                .and_then(|w| w.iter().copied().reduce(f32::max))
                .unwrap_or(0.)
                * 0.5;
            range.min = range.min.map(|v| f64::from(v as f32 - half_width));
            range.max = range.max.map(|v| f64::from(v as f32 + half_width));
        }
        return Some((range, varying(&["points", "widths"])));
    }
    let number = |name| read(&prim, name, time, crate::value::read_double);
    let (max, inputs): ([f32; 3], &[&str]) = if scene.is_a(path, "Cube") {
        ([(number("size")? * 0.5) as f32; 3], &["size"])
    } else if scene.is_a(path, "Sphere") {
        ([number("radius")? as f32; 3], &["radius"])
    } else {
        let capsule = scene.is_a(path, "Capsule");
        if !capsule && !scene.is_a(path, "Cylinder") && !scene.is_a(path, "Cone") {
            return None;
        }
        // UsdGeom{Cylinder,Cone,Capsule}::_ComputeExtentMax. Match the float
        // extent rounding and signed dimensions; do not silently take abs().
        let radius = number("radius")?;
        let height = number("height")?;
        let axis = read(&prim, "axis", time, crate::value::read_token)?;
        let index = match axis {
            "X" => 0,
            "Y" => 1,
            "Z" => 2,
            _ => return None,
        };
        let mut max = [radius as f32; 3];
        max[index] = (height * 0.5 + if capsule { radius } else { 0.0 }) as f32;
        (max, &["radius", "height", "axis"])
    };
    Some((
        Range3d {
            min: max.map(|v| f64::from(-v)),
            max: max.map(f64::from),
        },
        varying(inputs),
    ))
}

fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &str,
    time: Time,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Option<T> {
    match time {
        Time::Default => prim.read_value(name, decode),
        Time::At {
            code,
            interpolation,
        } => prim.read_value_at(name, code, interpolation, decode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use layerstack::{
        InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, Stage, StageOptions, TypedArray,
    };

    #[test]
    fn mesh_points_reject_wrong_native_empty_kind() {
        let mut store = InMemoryStore::default();
        let path = store.path("/Mesh");
        let mesh = store.tokens.intern("Mesh");
        let points = store.tokens.intern("points");
        for (value, accepted) in [
            (
                Value::TypedArray(TypedArray::Vec3f(Arc::new(Vec::new()))),
                true,
            ),
            (
                Value::TypedArray(TypedArray::Vec3d(Arc::new(Vec::new()))),
                false,
            ),
            (
                Value::TypedArray(TypedArray::Int(Arc::new(Vec::new()))),
                false,
            ),
        ] {
            let mut layer = Layer::new(LayerId(1));
            layer.insert_prim(
                path,
                PrimSpec::def()
                    .with_type_name(mesh)
                    .with_property(points, PropertySpec::attribute().with_default(value)),
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
            assert_eq!(compute(&scene, path, Time::Default).is_some(), accepted);
        }
    }
}
