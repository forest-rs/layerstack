// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#![allow(
    clippy::cast_possible_truncation,
    reason = "USD physics evaluates float32 properties from double stage units and transforms"
)]
use super::*;
use crate::{
    BindingCache, BindingOptions, MaterialPurpose, PrimView, Scene, Time, XformCache,
    usd_physics::{PhysicsMassApi, PhysicsMaterialApi, PhysicsRigidBodyApi},
};
use alloc::vec::Vec;
use layerstack::{PathId, Value};

struct Authored {
    mass: Option<f32>,
    density: f32,
    inertia: Option<[f32; 3]>,
    axes: Option<[f32; 4]>,
}
fn authored(prim: PrimView<'_>) -> Result<Authored, MassError> {
    let mut result = Authored {
        mass: None,
        density: 0.,
        inertia: None,
        axes: None,
    };
    if let Some(api) = PhysicsMassApi::get(&prim.scene(), prim.path()) {
        let mass = api.mass().unwrap_or(0.);
        result.density = api.density().unwrap_or(0.);
        if !mass.is_finite() || !result.density.is_finite() {
            return Err(MassError::InvalidMass);
        }
        result.mass = (mass > 0.).then_some(mass);
        let inertia = api.diagonal_inertia().unwrap_or([0.; 3]);
        if !inertia.iter().all(|v| v.is_finite() && *v >= 0.) {
            return Err(MassError::InvalidInertia);
        }
        if inertia.iter().map(|v| v * v).sum::<f32>() > 1e-10 {
            result.inertia = Some(inertia);
        }
        let axes = api.principal_axes().unwrap_or([0.; 4]);
        if axes[..3].iter().map(|v| v * v).sum::<f32>() > 1e-10 || axes[3].abs() > 1e-5 {
            check_quaternion(axes)?;
            result.axes = Some(axes);
        } else if !axes.iter().all(|v| v.is_finite()) {
            return Err(MassError::InvalidTransform);
        }
    }
    Ok(result)
}
fn center(
    scene: &Scene<'_>,
    path: PathId,
    cache: &mut XformCache,
) -> Result<Option<[f32; 3]>, MassError> {
    let Some(com) = PhysicsMassApi::get(scene, path)
        .and_then(|api| api.center_of_mass())
        .filter(|v| v.iter().all(|v| v.is_finite()))
    else {
        return Ok(None);
    };
    let matrix = cache
        .local_to_world(scene, path)
        .ok_or(MassError::UnreadableTransform(path))?;
    let linear = core::array::from_fn(|i| core::array::from_fn(|j| matrix[i][j]));
    let scale = crate::gf::decomposition::factored_scale(&linear)
        .ok_or(MassError::UnreadableTransform(path))?;
    Ok(Some(core::array::from_fn(|i| com[i] * scale[i] as f32)))
}
fn diagonal(v: [f32; 3]) -> Matrix3 {
    [[v[0], 0., 0.], [0., v[1], 0.], [0., 0., v[2]]]
}
fn colliders(scene: Scene<'_>, root: PathId) -> Vec<PathId> {
    let mut result = Vec::new();
    let mut ancestor = scene.parent(root);
    while let Some(path) = ancestor {
        if scene.parent(path).is_some() && !scene.stage().is_defined(path, scene.store()) {
            return result;
        }
        ancestor = scene.parent(path);
    }
    let mut pending = alloc::vec![root];
    while let Some(path) = pending.pop() {
        if !scene.stage().is_defined(path, scene.store())
            || scene.stage().is_abstract(path, scene.store())
            || PrimView::new(scene, path).metadata_value("active") == Some(Value::Bool(false))
            || (path != root && scene.has_api(path, "PhysicsRigidBodyAPI", None))
        {
            continue;
        }
        if scene.has_api(path, "PhysicsCollisionAPI", None) {
            result.push(path);
        }
        pending.extend(
            scene
                .stage()
                .children_of(path)
                .unwrap_or_default()
                .iter()
                .rev()
                .copied(),
        );
    }
    result
}
impl<'a> PhysicsRigidBodyApi<'a> {
    /// Computes this body's mass, inertia and center at default time.
    ///
    /// `collider` supplies scaled volume, unit-density inertia and placement for
    /// every `PhysicsCollisionAPI` prim in the body subtree, including instance
    /// proxies and collision-disabled shapes. Nested rigid bodies are excluded.
    /// Density precedence is collider, body, bound physics material, then the
    /// unit-adjusted default 1000 kg/m³. Authored body mass overrides the total;
    /// collider/body inertia, centers and axes follow OpenUSD's override rules.
    ///
    /// Requires `usd-shade` for complete material binding resolution. Geometry
    /// integration and simulation belong to the callback's backend. If body
    /// mass, inertia and center are all authored, no collider calls are needed.
    /// Without colliders or inertia, a positive authored mass uses the reference
    /// sphere approximation and reports it in the result.
    ///
    /// Returns an error for invalid callback data, units, transforms or mass.
    /// Unlike C++'s diagnostic/default path, invalid or zero aggregate mass does
    /// not produce an apparently usable result containing nonfinite values.
    pub fn compute_mass_properties(
        &self,
        mut collider: impl FnMut(PrimView<'a>) -> Result<CollisionMassInformation, MassError>,
    ) -> Result<RigidBodyMass, MassError> {
        let scene = self.scene();
        let body = authored(PrimView::new(scene, self.path()))?;
        let mut cache = XformCache::new(Time::Default);
        let com = center(&scene, self.path(), &mut cache)?;
        let mut result = RigidBodyMass {
            mass: body.mass.unwrap_or(0.),
            center_of_mass: com.unwrap_or([0.; 3]),
            diagonal_inertia: body.inertia.unwrap_or([1.; 3]),
            principal_axes: body.axes.unwrap_or([0., 0., 0., 1.]),
            collider_count: 0,
            used_sphere_approximation: false,
        };
        if body.mass.is_some() && body.inertia.is_some() && com.is_some() {
            return Ok(result);
        }
        let meters = scene.metadata().meters_per_unit().unwrap_or(0.01) as f32;
        let kilograms = scene.metadata().kilograms_per_unit().unwrap_or(1.) as f32;
        if !meters.is_finite() || meters <= 0. || !kilograms.is_finite() || kilograms <= 0. {
            return Err(MassError::InvalidUnits);
        }
        let default_density = 1000. * meters * meters * meters / kilograms;
        let mut bindings = BindingCache::new(
            MaterialPurpose::from_token("physics"),
            BindingOptions::default(),
        );
        let mut contributions = Vec::new();
        for path in colliders(scene, self.path()) {
            let prim = PrimView::new(scene, path);
            let shape = authored(prim)?;
            let info = collider(prim)?;
            result.collider_count += 1;
            if !info.volume.is_finite()
                || info.volume < 0.
                || (info.volume == 0. && shape.mass.is_some())
            {
                return Err(MassError::InvalidCollider(path));
            }
            let mut density = if shape.density > 0. {
                shape.density
            } else {
                body.density
            };
            if density <= 0. {
                density = bindings
                    .compute_bound_material(&scene, path)
                    .material
                    .and_then(|p| PhysicsMaterialApi::get(&scene, p))
                    .and_then(|api| api.density())
                    .unwrap_or(0.);
            }
            if !density.is_finite() {
                return Err(MassError::InvalidMass);
            }
            if density <= 0. {
                density = default_density;
            }
            let mass = shape.mass.unwrap_or(info.volume * density);
            let mut properties = MassProperties {
                mass,
                inertia: info
                    .inertia
                    .map(|r| r.map(|v| v * shape.mass.map_or(density, |m| m / info.volume))),
                center_of_mass: info.center_of_mass,
            };
            if let Some(inertia) = shape.inertia {
                properties.inertia = diagonal(inertia);
            }
            if let Some(axes) = shape.axes {
                properties.inertia = rotate_inertia(properties.inertia, axes);
            }
            if let Some(center) = center(&scene, path, &mut cache)? {
                if shape.inertia.is_none() {
                    properties = properties.translated(core::array::from_fn(|i| {
                        center[i] - properties.center_of_mass[i]
                    }));
                }
                properties.center_of_mass = center;
            }
            check_quaternion(info.local_orientation)?;
            contributions.push(MassContribution {
                properties,
                position: info.local_position,
                // C++ selects SetRotateOnly(GfRotation) for the GfQuatd
                // argument, introducing a normalized double round trip.
                orientation: crate::gf::Rotation::from_quat(info.local_orientation.map(f64::from))
                    .quat()
                    .map(|v| v as f32),
            });
        }
        if contributions.is_empty() {
            if result.mass <= 0. {
                return Err(MassError::InvalidMass);
            }
            if body.inertia.is_none() {
                let radius = 0.1 / meters;
                result.diagonal_inertia = [0.4 * result.mass * radius * radius; 3];
                result.used_sphere_approximation = true;
            }
        } else {
            let mut properties = MassProperties::sum(&contributions)?;
            if properties.mass <= 0. {
                return Err(MassError::InvalidMass);
            }
            if let Some(mass) = body.mass {
                properties = properties.scaled(mass / properties.mass);
            }
            if let Some(center) = com {
                properties = properties.translated(core::array::from_fn(|i| {
                    center[i] - properties.center_of_mass[i]
                }));
            }
            let principal = properties.principal_inertia()?;
            result.mass = properties.mass;
            result.center_of_mass = properties.center_of_mass;
            result.diagonal_inertia = body.inertia.unwrap_or(principal.diagonal);
            result.principal_axes = body.axes.unwrap_or(principal.axes);
        }
        if !result.mass.is_finite()
            || !result.diagonal_inertia.iter().all(|v| v.is_finite())
            || !result.center_of_mass.iter().all(|v| v.is_finite())
        {
            return Err(MassError::InvalidMass);
        }
        Ok(result)
    }
}
