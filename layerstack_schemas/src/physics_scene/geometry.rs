// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
/// Axis of rotationally symmetric physics geometry or a joint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicsAxis {
    /// X axis.
    X,
    /// Y axis.
    Y,
    /// Z axis.
    Z,
}
impl PhysicsAxis {
    pub(super) fn index(self) -> usize {
        match self {
            Self::X => 0,
            Self::Y => 1,
            Self::Z => 2,
        }
    }
}
pub(super) fn axis(
    prim: &PrimView<'_>,
    name: &'static str,
    default: PhysicsAxis,
) -> Result<PhysicsAxis, PhysicsSceneError> {
    Ok(match prim.read_value(name, crate::value::read_token) {
        Some("X") => PhysicsAxis::X,
        Some("Y") => PhysicsAxis::Y,
        Some("Z") => PhysicsAxis::Z,
        None => default,
        _ => return Err(PhysicsSceneError::InvalidAttribute(name)),
    })
}
/// Sphere from `Points`, center in shape coordinates and world-scaled radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpherePoint {
    /// Authored local position.
    pub center: [f32; 3],
    /// Width times half the maximum absolute world scale.
    pub radius: f32,
}
/// Bounded geometry capture, matching OpenUSD's physics shape dimensions.
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeGeometry {
    /// Sphere radius includes maximum absolute world scale.
    Sphere {
        /// World-scaled nonnegative radius.
        radius: f32,
    },
    /// Cube with signed world scales, as OpenUSD's half-extents descriptor.
    Box {
        /// Half edge lengths multiplied by signed world scales.
        half_extents: [f32; 3],
    },
    /// Capsule, cylinder, cone or asymmetric radius variant.
    Axial {
        /// Original USD type name.
        kind: Arc<str>,
        /// Longitudinal axis.
        axis: PhysicsAxis,
        /// Top radius, scaled by the larger absolute transverse scale.
        radius_top: f32,
        /// Bottom radius (equal to top except asymmetric USD geometry).
        radius_bottom: f32,
        /// Half axial height multiplied by signed world scale.
        half_height: f32,
    },
    /// Infinite plane, as the physics parser represents `UsdGeomPlane`.
    Plane {
        /// Plane normal axis.
        axis: PhysicsAxis,
    },
    /// Polygonal mesh source, available without a cooking dependency.
    Mesh {
        /// Authored mesh points, without world-scale baking.
        points: Vec<[f32; 3]>,
        /// Polygon sizes.
        face_vertex_counts: Vec<i32>,
        /// Flattened polygon vertex indices.
        face_vertex_indices: Vec<i32>,
        /// Signed world scales applied by the consuming mesh cooker.
        mesh_scale: [f32; 3],
        /// Collision approximation token, `none` absent `MeshCollisionAPI`.
        approximation: Arc<str>,
        /// Authored double-sidedness.
        double_sided: bool,
    },
    /// Point cloud interpreted as one collision sphere per point.
    SpherePoints {
        /// Local centers and world-scaled radii.
        spheres: Vec<SpherePoint>,
    },
}
fn double(prim: &PrimView<'_>, name: &'static str) -> Result<f32, PhysicsSceneError> {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "USD physics narrows double shape parameters to float"
    )]
    let value = read(prim, name, crate::value::read_double)? as f32;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(PhysicsSceneError::InvalidAttribute(name))
    }
}
fn finite(value: f32, name: &'static str) -> Result<f32, PhysicsSceneError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(PhysicsSceneError::InvalidAttribute(name))
    }
}
pub(super) fn capture(
    prim: &PrimView<'_>,
    scale: [f32; 3],
) -> Result<ShapeGeometry, PhysicsSceneError> {
    let scene = prim.scene();
    let path = prim.path();
    if scene.is_a(path, "Sphere") {
        return Ok(ShapeGeometry::Sphere {
            radius: finite(
                (double(prim, "radius")? * scale.iter().map(|s| s.abs()).fold(0., f32::max)).abs(),
                "radius",
            )?,
        });
    }
    if scene.is_a(path, "Cube") {
        let half = double(prim, "size")?.abs() * 0.5;
        return Ok(ShapeGeometry::Box {
            half_extents: [
                finite(scale[0] * half, "size")?,
                finite(scale[1] * half, "size")?,
                finite(scale[2] * half, "size")?,
            ],
        });
    }
    for kind in ["Capsule", "Capsule_1", "Cylinder", "Cylinder_1", "Cone"] {
        if scene.is_a(path, kind) {
            let axis = axis(prim, "axis", PhysicsAxis::X)?;
            let i = axis.index();
            let radial = (0..3)
                .filter(|&j| j != i)
                .map(|j| scale[j].abs())
                .fold(0., f32::max);
            let (top, bottom) = if kind.ends_with("_1") {
                (double(prim, "radiusTop")?, double(prim, "radiusBottom")?)
            } else {
                let r = double(prim, "radius")?;
                (r, r)
            };
            return Ok(ShapeGeometry::Axial {
                kind: Arc::from(kind),
                axis,
                radius_top: finite(
                    top * radial,
                    if kind.ends_with("_1") {
                        "radiusTop"
                    } else {
                        "radius"
                    },
                )?,
                radius_bottom: finite(
                    bottom * radial,
                    if kind.ends_with("_1") {
                        "radiusBottom"
                    } else {
                        "radius"
                    },
                )?,
                half_height: finite(double(prim, "height")? * 0.5 * scale[i], "height")?,
            });
        }
    }
    if scene.is_a(path, "Plane") {
        return Ok(ShapeGeometry::Plane {
            axis: axis(prim, "axis", PhysicsAxis::X)?,
        });
    }
    if scene.is_a(path, "Mesh") {
        let points: Vec<[f32; 3]> = read(prim, "points", crate::value::read_float3_array)?;
        let counts: Vec<i32> = read(prim, "faceVertexCounts", crate::value::read_int_array)?;
        let indices: Vec<i32> = read(prim, "faceVertexIndices", crate::value::read_int_array)?;
        if points.iter().flatten().any(|v| !v.is_finite())
            || counts.iter().any(|&v| v < 0)
            || counts
                .iter()
                .try_fold(0_usize, |n, &v| n.checked_add(usize::try_from(v).ok()?))
                != Some(indices.len())
            || indices
                .iter()
                .any(|&i| usize::try_from(i).map_or(true, |i| i >= points.len()))
        {
            return Err(PhysicsSceneError::InvalidAttribute("meshTopology"));
        }
        let approximation = if scene.has_api(path, "PhysicsMeshCollisionAPI", None) {
            Arc::from(read(
                prim,
                "physics:approximation",
                crate::value::read_token,
            )?)
        } else {
            Arc::from("none")
        };
        return Ok(ShapeGeometry::Mesh {
            points,
            face_vertex_counts: counts,
            face_vertex_indices: indices,
            mesh_scale: scale,
            approximation,
            double_sided: read(prim, "doubleSided", crate::value::read_bool)?,
        });
    }
    if scene.is_a(path, "Points") {
        let positions: Vec<[f32; 3]> = read(prim, "points", crate::value::read_float3_array)?;
        let widths = if prim.has_authored_value("primvars:widths") {
            let variable = crate::primvar::Primvar::new(&scene, path, "widths")
                .ok_or(PhysicsSceneError::InvalidAttribute("primvars:widths"))?;
            let value = variable
                .compute_flattened(Time::Default)
                .map_err(|_| PhysicsSceneError::InvalidAttribute("primvars:widths"))?
                .ok_or(PhysicsSceneError::InvalidAttribute("primvars:widths"))?;
            crate::value::read_float_array(&value, scene.store().tokens())
                .ok_or(PhysicsSceneError::InvalidAttribute("primvars:widths"))?
        } else {
            read(prim, "widths", crate::value::read_float_array)?
        };
        if positions.is_empty()
            || positions.len() != widths.len()
            || positions
                .iter()
                .flatten()
                .chain(&widths)
                .any(|v| !v.is_finite())
        {
            return Err(PhysicsSceneError::InvalidAttribute("widths"));
        }
        let max = scale.iter().map(|s| s.abs()).fold(0., f32::max);
        return Ok(ShapeGeometry::SpherePoints {
            spheres: positions
                .into_iter()
                .zip(widths)
                .map(|(center, width)| {
                    Ok(SpherePoint {
                        center,
                        radius: finite(max * width * 0.5, "widths")?,
                    })
                })
                .collect::<Result<Vec<_>, PhysicsSceneError>>()?,
        });
    }
    Err(PhysicsSceneError::UnsupportedShape)
}
#[cfg(feature = "usd-shade")]
pub(super) fn materials(prim: &PrimView<'_>) -> Vec<PathId> {
    let scene = prim.scene();
    let mut material_paths = Vec::new();
    let mut candidates = Vec::new();
    if scene.is_a(prim.path(), "Mesh") {
        for &child in scene.stage().children_of(prim.path()).unwrap_or_default() {
            let child = PrimView::new(scene, child);
            if scene.is_a(child.path(), "GeomSubset")
                && child.read_value("elementType", crate::value::read_token) == Some("face")
            {
                candidates.push(child);
            }
        }
    }
    candidates.push(*prim);
    for candidate in candidates {
        if let Some(material) = candidate
            .compute_bound_material(
                &crate::MaterialPurpose::from_token("physics"),
                crate::BindingOptions::default(),
            )
            .material
            && scene.has_api(material, "PhysicsMaterialAPI", None)
        {
            material_paths.push(material);
        }
    }
    material_paths
}
