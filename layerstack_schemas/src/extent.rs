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
            if values.len() != 2 {
                return None;
            }
            Some(Range3d {
                min: crate::value::read_float3(values.get(0)?.as_ref(), tokens)?.map(f64::from),
                max: crate::value::read_float3(values.get(1)?.as_ref(), tokens)?.map(f64::from),
            })
        })
    {
        return Some((range, extent_varying));
    }
    let varying =
        |names: &[&str]| extent_varying || names.iter().any(|name| prim.property_might_vary(name));
    // UsdGeomPointBased::ComputeExtent. Restrict this fallback to Mesh: curves
    // and points have width-dependent providers and cannot use the mesh rule.
    if scene.is_a(path, "Mesh") {
        let range = read(&prim, "points", time, |value, tokens| {
            let values = value.array_ref()?;
            let mut range = Range3d::default();
            let mut add = |point: [f32; 3]| {
                let point = point.map(f64::from);
                range.union_with(Range3d {
                    min: point,
                    max: point,
                });
            };
            if let Some(points) = values.typed().and_then(layerstack::TypedArray::as_vec3f) {
                for point in points {
                    add(*point);
                }
            } else {
                for value in values.iter() {
                    add(crate::value::read_float3(&value, tokens)?);
                }
            }
            Some(range)
        })?;
        return Some((range, varying(&["points"])));
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
