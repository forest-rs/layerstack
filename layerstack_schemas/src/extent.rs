// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Intrinsic bounds for the built-in geometry supported by `BoundsCache`.

mod providers;
pub use providers::{
    ExtentContext, ExtentDependencies, ExtentError, ExtentProvider, ExtentProviders,
    ExtentRegistrationError,
};

use crate::{PrimView, Scene, Time, bounds::Range3d};
use layerstack::{PathId, TokenInterner, Value};

pub(crate) fn has_builtin(schema: &str) -> bool {
    matches!(
        schema,
        "PointBased"
            | "Points"
            | "Curves"
            | "Cube"
            | "Sphere"
            | "Cylinder"
            | "Cylinder_1"
            | "Cone"
            | "Capsule"
            | "Capsule_1"
            | "Plane"
            | "PointInstancer"
    ) || cfg!(feature = "usd-lux")
        && matches!(
            schema,
            "SphereLight" | "CylinderLight" | "DiskLight" | "RectLight" | "PortalLight"
        )
        || cfg!(feature = "usd-skel") && matches!(schema, "Skeleton" | "SkelRoot")
}

fn decode_error(prim: &PrimView<'_>, name: &str, error: layerstack::ArrayReadError) -> ExtentError {
    ExtentError::Decode {
        property: layerstack::PropertyPath::new(
            prim.path(),
            prim.scene()
                .store()
                .tokens()
                .lookup(name)
                .expect("decoded property token"),
        ),
        error,
    }
}

/// Read authored extent before dispatching any provider. A compatible malformed
/// extent can fall through to computation; retained decode failures cannot.
/// OpenUSD `UsdGeomBoundable::ComputeExtent`; AOUSD Core §12.3.
pub(crate) fn authored(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
) -> Result<Option<(Range3d, bool)>, ExtentError> {
    let prim = PrimView::new(*scene, path);
    if !prim.has_authored_value("extent") {
        return Ok(None);
    }
    let extent = prim
        .try_read_value("extent", time, |value, tokens| {
            let values = value.array_ref()?;
            if let Some(typed) = values.typed() {
                typed.as_vec3f()?;
            } else if !values.iter().all(|v| matches!(&*v, Value::Vec3f(_))) {
                return None;
            }
            if values.len() != 2 {
                return Some(None);
            }
            Some(Some(Range3d {
                min: crate::value::read_float3(values.get(0)?.as_ref(), tokens)?.map(f64::from),
                max: crate::value::read_float3(values.get(1)?.as_ref(), tokens)?.map(f64::from),
            }))
        })
        .map_err(|error| decode_error(&prim, "extent", error))?;
    Ok(extent
        .flatten()
        .map(|range| (range, prim.property_might_vary("extent"))))
}

/// Preserve retained numeric failures before the intrinsic provider consumes
/// points or widths. Typed selection is identical to the intrinsic reader.
pub(crate) fn checked_compute(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
) -> Result<Option<(Range3d, bool)>, ExtentError> {
    if let Some(extent) = authored(scene, path, time)? {
        return Ok(Some(extent));
    }
    checked_intrinsic(scene, path, time)
}

/// [`checked_compute`] without the authored extent: what the intrinsic
/// provider computes, as `UsdGeomBoundable::ComputeExtentFromPlugins` does.
pub(crate) fn checked_intrinsic(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
) -> Result<Option<(Range3d, bool)>, ExtentError> {
    let prim = PrimView::new(*scene, path);
    if scene.is_a(path, "PointBased") {
        prim.try_read_value("points", time, crate::value::read_float3_array_shared)
            .map_err(|error| decode_error(&prim, "points", error))?;
        if scene.is_a(path, "Points") || scene.is_a(path, "Curves") {
            prim.try_read_value("widths", time, crate::value::read_float_array_shared)
                .map_err(|error| decode_error(&prim, "widths", error))?;
        }
    }
    Ok(compute_intrinsic(scene, path, time))
}

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
    compute_intrinsic(scene, path, time)
}

/// The built-in providers' extent, ignoring any authored one.
#[allow(
    clippy::cast_possible_truncation,
    reason = "USD extents are float3 even for double geometry parameters"
)]
pub(crate) fn compute_intrinsic(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
) -> Option<(Range3d, bool)> {
    let prim = PrimView::new(*scene, path);
    let extent_varying = prim.property_might_vary("extent");
    let varying =
        |names: &[&str]| extent_varying || names.iter().any(|name| prim.property_might_vary(name));
    let points_schema = scene.is_a(path, "Points");
    let curves = scene.is_a(path, "Curves");
    // PointBased owns the inherited provider, including Mesh, TetMesh and host
    // concrete descendants; Points and Curves supply their width-aware variants.
    // OpenUSD pointBased.cpp::_ComputeExtentForPointBased; AOUSD Core §13.3.1.
    if scene.is_a(path, "PointBased") {
        let points = read(
            &prim,
            "points",
            time,
            crate::value::read_float3_array_shared,
        )?;
        let widths = if points_schema || curves {
            read(&prim, "widths", time, crate::value::read_float_array_shared)
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
    // UsdLux shape providers use float parameters and float3 extents, including
    // signed dimensions. AOUSD Core §12.3 (attribute value resolution).
    #[cfg(feature = "usd-lux")]
    {
        let number = |name| {
            if time == Time::Default && lux_dimension_blocked(&prim, name) {
                None
            } else {
                read(&prim, name, time, crate::value::read_float)
            }
        };
        let shape: Option<([f32; 3], &[&str])> = if scene.is_a(path, "SphereLight") {
            Some(([number("inputs:radius")?; 3], &["inputs:radius"]))
        } else if scene.is_a(path, "CylinderLight") {
            let radius = number("inputs:radius")?;
            Some((
                [number("inputs:length")? * 0.5, radius, radius],
                &["inputs:length", "inputs:radius"],
            ))
        } else if scene.is_a(path, "DiskLight") {
            let radius = number("inputs:radius")?;
            Some(([radius, radius, 0.], &["inputs:radius"]))
        } else if scene.is_a(path, "RectLight") || scene.is_a(path, "PortalLight") {
            Some((
                [
                    number("inputs:width")? * 0.5,
                    number("inputs:height")? * 0.5,
                    0.,
                ],
                &["inputs:width", "inputs:height"],
            ))
        } else {
            None
        };
        if let Some((max, inputs)) = shape {
            return Some((
                Range3d {
                    min: max.map(|v| f64::from(-v)),
                    max: max.map(f64::from),
                },
                varying(inputs) || inputs.iter().any(|name| lux_dimension_blocked(&prim, name)),
            ));
        }
    }
    let number = |name| read(&prim, name, time, crate::value::read_double);
    let (max, inputs): ([f32; 3], &[&str]) = if scene.is_a(path, "Cube") {
        ([(number("size")? * 0.5) as f32; 3], &["size"])
    } else if scene.is_a(path, "Sphere") {
        ([number("radius")? as f32; 3], &["radius"])
    } else if scene.is_a(path, "Plane") {
        // Resolve varying dimensions at the requested time (AOUSD Core
        // §12.3–12.5). OpenUSD 26.8's Plane plugin omits time on these reads;
        // its static formula still supplies the numerical extent contract.
        let half_width = (number("width")? * 0.5) as f32;
        let half_length = (number("length")? * 0.5) as f32;
        let max = match read(&prim, "axis", time, crate::value::read_token)? {
            "X" => [0., half_length, half_width],
            "Y" => [half_width, 0., half_length],
            "Z" => [half_width, half_length, 0.],
            _ => return None,
        };
        (max, &["width", "length", "axis"])
    } else if scene.is_a(path, "Capsule_1") {
        let height = number("height")?;
        let top = number("radiusTop")?;
        let bottom = number("radiusBottom")?;
        let axis = read(&prim, "axis", time, crate::value::read_token)?;
        let (min, max) = capsule_1_extent(height, top, bottom, axis)?;
        return Some((
            Range3d {
                min: min.map(f64::from),
                max: max.map(f64::from),
            },
            varying(&["height", "radiusTop", "radiusBottom", "axis"]),
        ));
    } else {
        let capsule = scene.is_a(path, "Capsule");
        let separate_radii = scene.is_a(path, "Cylinder_1");
        if !capsule && !separate_radii && !scene.is_a(path, "Cylinder") && !scene.is_a(path, "Cone")
        {
            return None;
        }
        // UsdGeom{Cylinder,Cone,Capsule} (and Cylinder_1)::_ComputeExtentMax. Match the float
        // extent rounding and signed dimensions; do not silently take abs().
        let radius = if separate_radii {
            let top = number("radiusTop")?;
            let bottom = number("radiusBottom")?;
            // Preserve std::max's first-argument choice for ties and NaNs.
            if top < bottom { bottom } else { top }
        } else {
            number("radius")?
        };
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
        let inputs: &[&str] = if separate_radii {
            &["radiusTop", "radiusBottom", "height", "axis"]
        } else {
            &["radius", "height", "axis"]
        };
        (max, inputs)
    };
    Some((
        Range3d {
            min: max.map(|v| f64::from(-v)),
            max: max.map(f64::from),
        },
        varying(inputs),
    ))
}

/// The extent of a `Capsule_1`, as `(min, max)`: the ends are spheres whose
/// centers sit inward of the height's ends so the slanted side meets both
/// tangentially, which makes the box asymmetric along the axis when the
/// radii differ. `None` for a negative dimension or an unknown axis.
///
/// OpenUSD 26.11: `_ComputeExtentMax` (`usdGeom/capsule_1.cpp`), computed
/// in double and rounded to float once per component.
#[expect(
    clippy::cast_possible_truncation,
    reason = "OpenUSD rounds the double extent to float"
)]
fn capsule_1_extent(
    height: f64,
    top: f64,
    bottom: f64,
    axis: &str,
) -> Option<([f32; 3], [f32; 3])> {
    if height < 0.0 || top < 0.0 || bottom < 0.0 {
        return None;
    }
    let half = 0.5 * height;
    let slope = if height > 0.0 {
        (bottom - top) / height
    } else {
        0.0
    };
    let secant = libm::sqrt(slope * slope + 1.0);
    let top_radius = top * secant;
    let bottom_radius = bottom * secant;
    let max_along = (half - slope * top + top_radius) as f32;
    let min_along = (-half - slope * bottom - bottom_radius) as f32;
    // `std::max`: the first argument for ties and NaNs.
    let radius = if top_radius < bottom_radius {
        bottom_radius
    } else {
        top_radius
    } as f32;
    let index = match axis {
        "X" => 0,
        "Y" => 1,
        "Z" => 2,
        _ => return None,
    };
    let mut min = [-radius; 3];
    let mut max = [radius; 3];
    min[index] = min_along;
    max[index] = max_along;
    Some((min, max))
}

// UsdLux extent providers call UsdAttribute::Get<float>. At default time an
// authored block suppresses schema fallback (AOUSD Core §12.3.6); numeric Get
// may still use it. That difference must remain a retained-cache dependency.
#[cfg(feature = "usd-lux")]
fn lux_dimension_blocked(prim: &PrimView<'_>, name: &str) -> bool {
    let Some(property) = prim.prim().and_then(|handle| handle.property(name)) else {
        return false;
    };
    let opinions = property.opinions();
    for value in opinions.iter().filter_map(|o| o.value().default_value()) {
        if matches!(value, Value::Blocked) {
            return true;
        }
        if crate::value::read_float(value, prim.scene().store().tokens()).is_some() {
            return false;
        }
    }
    false
}

fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &str,
    time: Time,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Option<T> {
    prim.read_value_at(name, time, decode)
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
            let schemas = crate::openusd(&store.tokens);
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

    #[test]
    fn custom_concrete_point_based_schema_inherits_intrinsic_extent() {
        let mut store = InMemoryStore::default();
        let path = store.path("/Shape");
        let mut builder = layerstack::SchemaRegistry::builder();
        crate::register(&mut builder, &[crate::Domain::UsdGeom], &store.tokens).unwrap();
        let schema = store.tokens.intern("HostPointGeometry");
        builder.register(
            layerstack::SchemaDefinition::typed(schema)
                .with_parent(store.tokens.lookup("PointBased").unwrap()),
        );
        let schemas = builder.build(&store.tokens);
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            path,
            PrimSpec::def().with_type_name(schema).with_property(
                store.tokens.intern("points"),
                PropertySpec::attribute()
                    .with_default(Value::from(alloc::vec![[-1_f32, 2., 3.], [4., -5., 6.]])),
            ),
        );
        store.insert_layer(layer);
        let stage = Stage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(Arc::new(schemas)),
                ..StageOptions::default()
            },
        );
        assert_eq!(
            checked_compute(&Scene::new(&stage, &store), path, Time::Default),
            Ok(Some((
                Range3d {
                    min: [-1., -5., 3.],
                    max: [4., 2., 6.]
                },
                false
            ))),
        );
    }

    #[test]
    fn shape_provider_preserves_signed_cpp_endpoints_before_aggregation() {
        // OpenUSD 26.11 ComputeExtentFromPlugins, Z axis. BBoxCache discards
        // these empty inverted ranges; the intrinsic formula must not abs().
        // `Capsule_1` computes no extent from a negative dimension.
        for (schema, max) in [
            ("Plane", Some([-3., 5., 0.])),
            ("Cylinder_1", Some([-1., -1., -2.])),
            ("Capsule_1", None),
        ] {
            let mut store = InMemoryStore::default();
            let path = store.path("/Shape");
            let mut prim = PrimSpec::def().with_type_name(store.tokens.intern(schema));
            for (name, value) in [
                ("width", -6.),
                ("length", 10.),
                ("height", -4.),
                ("radiusTop", -1.),
                ("radiusBottom", -3.),
            ] {
                prim = prim.with_property(
                    store.tokens.intern(name),
                    PropertySpec::attribute().with_default(Value::Double(value)),
                );
            }
            let mut layer = Layer::new(LayerId(1));
            layer.insert_prim(path, prim);
            store.insert_layer(layer);
            let schemas = crate::openusd(&store.tokens);
            let stage = Stage::compose(
                &mut store,
                LayerId(1),
                StageOptions {
                    schemas: Some(Arc::new(schemas)),
                    ..StageOptions::default()
                },
            );
            assert_eq!(
                checked_compute(&Scene::new(&stage, &store), path, Time::Default),
                Ok(max.map(|max: [f64; 3]| (
                    Range3d {
                        min: max.map(|v| -v),
                        max
                    },
                    false
                ))),
                "{schema}"
            );
        }
    }
}
