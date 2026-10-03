// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Owned default-time physics descriptors, without a simulation backend.
//!
//! Discovery, ownership and scene defaults follow OpenUSD 26.08
//! `usdPhysics/parseUtils.cpp` and `parsingUtils.dox`. Includes traverse active,
//! defined, concrete instance proxies. Encountering an excluded path or a point
//! instancer prunes that traversal; a separate include below an excluded ancestor
//! remains permitted, as in OpenUSD. Overlapping includes are deduplicated. Owner filters retain bodies
//! by their own owners, attached shapes by retained bodies, static shapes by
//! their own owners, joints by both bodies, and articulations by all bodies.
//! Invalid body, shape and joint records are omitted by a nonempty owner filter,
//! matching OpenUSD; invalid articulation records remain visible. Materials and
//! collision groups are independent of owner filtering.
//! AOUSD Core §11, §12.3–12.5, §13.3.2.
//!
//! Geometry uses stage units; angular values are degrees. Capture after edits.
//! Shear/singular transforms are rejected explicitly rather than silently
//! dropping stretch axes. With `usd-shade` disabled shape material capture is
//! unavailable and represented by `None`. No mesh cooking or solver resources
//! are produced.

mod articulation;
mod geometry;
mod joints;
use crate::{
    PrimView, Scene, XformCache,
    affine::AffineFactors,
    usd_physics::{
        PhysicsCollisionApi, PhysicsCollisionGroup, PhysicsMaterialApi, PhysicsRigidBodyApi,
        PhysicsScene,
    },
};
use alloc::{sync::Arc, vec::Vec};
pub use articulation::ArticulationDescriptor;
pub use geometry::{PhysicsAxis, ShapeGeometry, SpherePoint};
pub use joints::{JointDescriptor, JointDof, JointDrive, JointKind, JointLimit, JointPose};
use layerstack::{HashSet, PathId, TargetPath, Time, TokenInterner, Value};

/// Selection of a simulation owner, including the unowned/default partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimulationOwner {
    /// Objects with no authored simulation-owner targets.
    Default,
    /// Objects explicitly assigned to this scene path.
    Scene(PathId),
}
/// Explicit parse scope; empty includes is an error, empty excludes is valid.
#[derive(Clone, Copy, Debug)]
pub struct PhysicsSceneOptions<'a> {
    /// Subtrees to include. Use the pseudo-root for a complete stage capture.
    pub includes: &'a [PathId],
    /// Paths that prune children when encountered during traversal. A direct
    /// include below an excluded ancestor remains permitted.
    pub excludes: &'a [PathId],
    /// `None` selects all owners. An empty slice follows OpenUSD: scenes are
    /// omitted while bodies/shapes/joints are not owner-filtered.
    pub simulation_owners: Option<&'a [SimulationOwner]>,
}
/// Structured capture failure; an individual bad object does not discard peers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhysicsSceneError {
    /// No include path was supplied.
    EmptyIncludes,
    /// Include path does not identify an existing prim or pseudo-root.
    MissingInclude(PathId),
    /// A required attribute is missing, incompatible or nonfinite.
    InvalidAttribute(&'static str),
    /// Transform is singular, sheared, non-affine or unreadable.
    InvalidTransform,
    /// Collision API is applied to an unsupported geometry type.
    UnsupportedShape,
    /// A joint target is not a prim path or does not identify an existing prim.
    InvalidJointTarget,
    /// Articulation cannot establish any rooted rigid-body topology.
    EmptyArticulation,
    /// An articulation API occurs beneath another articulation API.
    NestedArticulation,
    /// A collision-group collection could not establish valid membership.
    InvalidCollection,
}
impl core::fmt::Display for PhysicsSceneError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "physics capture: {self:?}")
    }
}
impl core::error::Error for PhysicsSceneError {}
/// Source identity and a complete descriptor or an explicit capture failure.
#[derive(Clone, Debug, PartialEq)]
pub struct PhysicsRecord<T> {
    /// Prim that supplied this object.
    pub path: PathId,
    /// Checked descriptor. Failed captures have no misleading partial result.
    pub descriptor: Result<T, PhysicsSceneError>,
}
/// Position, quaternion `[i,j,k,r]` and signed scale derived from a USD transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicsTransform {
    /// Translation in stage length units.
    pub position: [f32; 3],
    /// Normalized rotation quaternion `[i,j,k,r]`.
    pub orientation: [f32; 4],
    /// Signed X/Y/Z scales; geometry descriptors bake world scale into dimensions.
    pub scale: [f32; 3],
}
/// Gravity and source units for a physics scene.
#[derive(Clone, Debug, PartialEq)]
pub struct PhysicsSceneDescriptor {
    /// Unit gravity direction; zero authored direction chooses negative up axis.
    pub gravity_direction: [f32; 3],
    /// Acceleration in stage length units per second squared.
    pub gravity_magnitude: f32,
    /// Scene meters per unit used when default gravity is derived.
    pub meters_per_unit: f64,
}
/// Native rigid-body material values, in the stage's units.
#[derive(Clone, Debug, PartialEq)]
pub struct PhysicsMaterialDescriptor {
    /// Static Coulomb friction coefficient.
    pub static_friction: f32,
    /// Dynamic Coulomb friction coefficient.
    pub dynamic_friction: f32,
    /// Coefficient of restitution.
    pub restitution: f32,
    /// Density in stage mass units / stage length unit cubed.
    pub density: f32,
}
/// Rigid body and its collected collision paths.
#[derive(Clone, Debug, PartialEq)]
pub struct RigidBodyDescriptor {
    /// Pose in world space.
    pub world_transform: PhysicsTransform,
    /// Full authored world transform for consumers needing source evidence.
    pub world_matrix: [[f64; 4]; 4],
    /// Whether the body participates dynamically; false still defines a body.
    pub enabled: bool,
    /// Whether simulation is driven by authored transforms.
    pub kinematic: bool,
    /// Whether the simulation initially puts the body to sleep.
    pub starts_asleep: bool,
    /// Initial local-space linear velocity, stage length units per second.
    pub linear_velocity: [f32; 3],
    /// Initial local-space angular velocity, degrees per second.
    pub angular_velocity: [f32; 3],
    /// Authored simulation-owner relationship targets.
    pub simulation_owners: Vec<TargetPath>,
    /// Authored pair exclusions (arbitrary prim/property targets preserved).
    pub filtered_collisions: Vec<TargetPath>,
    /// Captured shape paths belonging to this body, in traversal order.
    pub collisions: Vec<PathId>,
}
/// One collision shape, including its body and stage-derived runtime parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeDescriptor {
    /// Nearest captured body, including a disabled body. None denotes static.
    pub rigid_body: Option<PathId>,
    /// Pose relative to the body, with body scale baked into the local position;
    /// for a static shape, this is its world pose.
    pub local_transform: PhysicsTransform,
    /// Full shape world matrix, retained for source geometry evaluation.
    pub world_matrix: [[f64; 4]; 4],
    /// Geometry with world-scale dimensions, following OpenUSD parse descriptors.
    pub geometry: ShapeGeometry,
    /// Whether collision response is enabled.
    pub enabled: bool,
    /// Authored simulation owners. Attached shapes use their body's owner filter.
    pub simulation_owners: Vec<TargetPath>,
    /// Authored pair exclusions.
    pub filtered_collisions: Vec<TargetPath>,
    /// Physics-purpose materials, subset materials then mesh material.
    /// `None` means `usd-shade` is disabled; an empty vector means no material.
    pub materials: Option<Vec<PathId>>,
    /// Parsed collision groups whose colliders collection includes this shape.
    pub collision_groups: Vec<PathId>,
}
/// Collision group configuration; merging and pair evaluation are available
/// separately through `physics::compute_collision_group_table`.
#[derive(Clone, Debug, PartialEq)]
pub struct CollisionGroupDescriptor {
    /// Invert authored filters to permit only the named groups.
    pub invert_filtered_groups: bool,
    /// Authored filtered-group targets.
    pub filtered_groups: Vec<TargetPath>,
    /// Optional authored merge name; empty names are retained.
    pub merge_group_name: Option<Arc<str>>,
    /// Captured collider paths in this scoped group.
    pub colliders: Vec<PathId>,
}
/// All captured object families, each retaining its source prim and validity.
#[derive(Clone, Debug, PartialEq)]
pub struct PhysicsSceneSnapshot {
    /// Physics scenes selected by the owner filter.
    pub scenes: Vec<PhysicsRecord<PhysicsSceneDescriptor>>,
    /// Rigid bodies selected by the owner filter.
    pub rigid_bodies: Vec<PhysicsRecord<RigidBodyDescriptor>>,
    /// Supported/unsupported collision shapes selected by the owner filter.
    pub shapes: Vec<PhysicsRecord<ShapeDescriptor>>,
    /// Physics materials, independent of owner filtering.
    pub materials: Vec<PhysicsRecord<PhysicsMaterialDescriptor>>,
    /// Joints whose endpoints survive owner filtering.
    pub joints: Vec<PhysicsRecord<JointDescriptor>>,
    /// Articulations whose body sets survive owner filtering.
    pub articulations: Vec<PhysicsRecord<ArticulationDescriptor>>,
    /// Collision groups, independent of owner filtering.
    pub collision_groups: Vec<PhysicsRecord<CollisionGroupDescriptor>>,
}
impl PhysicsSceneSnapshot {
    /// Captures an owned default-time scene. Deterministic traversal order and
    /// deduplication make overlapping scopes reproducible. Numeric errors live
    /// beside their source path; selection errors reject the entire capture.
    pub fn capture(
        scene: &Scene<'_>,
        options: PhysicsSceneOptions<'_>,
    ) -> Result<Self, PhysicsSceneError> {
        if options.includes.is_empty() {
            return Err(PhysicsSceneError::EmptyIncludes);
        }
        let mut paths = Vec::new();
        let mut seen = HashSet::new();
        for &include in options.includes {
            if !scene.stage().has_prim(include) && Some(include) != scene.root() {
                return Err(PhysicsSceneError::MissingInclude(include));
            }
            let mut pending = alloc::vec![include];
            while let Some(path) = pending.pop() {
                if !seen.insert(path) || options.excludes.contains(&path) {
                    continue;
                }
                let root = Some(path) == scene.root();
                if !root
                    && (!scene.stage().is_defined(path, scene.store())
                        || scene.stage().is_abstract(path, scene.store()))
                {
                    continue;
                }
                if !root {
                    paths.push(path);
                }
                if !scene.is_a(path, "PointInstancer") {
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
            }
        }
        let mut xforms = XformCache::new(Time::Default);
        let mut snapshot = Self {
            scenes: Vec::new(),
            rigid_bodies: Vec::new(),
            shapes: Vec::new(),
            materials: Vec::new(),
            joints: Vec::new(),
            articulations: Vec::new(),
            collision_groups: Vec::new(),
        };
        let mut shape_paths = HashSet::new();
        let mut articulation_paths = HashSet::new();
        for &path in &paths {
            // OpenUSD parseUtils.cpp: MaterialAPI sets its flag only before any
            // recognized physics API. Later APIs do not clear that material flag.
            let mut flags = 0_u8;
            if let Some(definition) = scene.stage().prim_definition_ref(path) {
                for api in definition.applied_schemas() {
                    match scene.store().tokens().resolve(api.schema) {
                        "PhysicsRigidBodyAPI" => flags |= 1,
                        "PhysicsCollisionAPI" => flags |= 2,
                        "PhysicsArticulationRootAPI" => flags |= 4,
                        "PhysicsMaterialAPI" if flags == 0 => flags |= 8,
                        _ => {}
                    }
                }
            }
            if let Some(api) = PhysicsScene::new(scene, path) {
                snapshot.scenes.push(record(path, scene_descriptor(api)));
            } else if let Some(api) = PhysicsCollisionGroup::new(scene, path) {
                let query = api.colliders_collection().membership_query();
                if !query.problems().is_empty() {
                    snapshot
                        .collision_groups
                        .push(record(path, Err(PhysicsSceneError::InvalidCollection)));
                    continue;
                }
                let colliders = paths
                    .iter()
                    .copied()
                    .filter(|&p| query.is_included(scene, TargetPath::Prim(p)).is_included())
                    .collect();
                snapshot.collision_groups.push(record(
                    path,
                    Ok(CollisionGroupDescriptor {
                        invert_filtered_groups: api.invert_filtered_groups().unwrap_or(false),
                        filtered_groups: api.filtered_groups(),
                        merge_group_name: api.merge_group_name(),
                        colliders,
                    }),
                ));
            } else if flags & 8 != 0 {
                let api = PhysicsMaterialApi::get(scene, path)
                    .expect("material flag derives from the applied schema");
                snapshot.materials.push(record(path, material(api)));
            } else if scene.is_a(path, "PhysicsJoint") {
                snapshot
                    .joints
                    .push(record(path, joints::capture(scene, path, &mut xforms)));
                if flags & 4 != 0 {
                    articulation_paths.insert(path);
                }
            } else {
                if flags & 1 != 0 {
                    let api = PhysicsRigidBodyApi::get(scene, path)
                        .expect("body flag derives from the applied schema");
                    snapshot
                        .rigid_bodies
                        .push(record(path, body(api, &mut xforms)));
                }
                if flags & 2 != 0 {
                    shape_paths.insert(path);
                }
                if flags & 4 != 0 {
                    articulation_paths.insert(path);
                }
            }
        }
        for &path in &paths {
            if shape_paths.contains(&path) {
                let api = PhysicsCollisionApi::get(scene, path)
                    .expect("shape flag derives from the applied schema");
                snapshot.shapes.push(record(
                    path,
                    shape(
                        api,
                        &snapshot.rigid_bodies,
                        &snapshot.collision_groups,
                        &mut xforms,
                    ),
                ));
            }
        }
        for body in &mut snapshot.rigid_bodies {
            if let Ok(desc) = &mut body.descriptor {
                desc.collisions = snapshot
                    .shapes
                    .iter()
                    .filter(|s| {
                        s.descriptor
                            .as_ref()
                            .is_ok_and(|s| s.rigid_body == Some(body.path))
                    })
                    .map(|s| s.path)
                    .collect();
            }
        }
        for &path in &paths {
            if articulation_paths.contains(&path) {
                snapshot.articulations.push(record(
                    path,
                    articulation::capture(scene, path, &snapshot.rigid_bodies, &snapshot.joints),
                ));
            }
        }
        if let Some(owners) = options.simulation_owners {
            snapshot
                .scenes
                .retain(|s| owners.contains(&SimulationOwner::Scene(s.path)));
            if !owners.is_empty() {
                snapshot.rigid_bodies.retain(|b| {
                    b.descriptor
                        .as_ref()
                        .is_ok_and(|b| owner_matches(&b.simulation_owners, owners))
                });
                let bodies: HashSet<_> = snapshot.rigid_bodies.iter().map(|b| b.path).collect();
                snapshot.shapes.retain(|s| {
                    s.descriptor.as_ref().is_ok_and(|s| {
                        s.rigid_body.map_or_else(
                            || owner_matches(&s.simulation_owners, owners),
                            |b| bodies.contains(&b),
                        )
                    })
                });
                snapshot.joints.retain(|j| {
                    j.descriptor
                        .as_ref()
                        .is_ok_and(|j| j.bodies.iter().flatten().all(|b| bodies.contains(b)))
                });
                snapshot.articulations.retain(|a| {
                    a.descriptor.as_ref().map_or(true, |a| {
                        a.bodies.iter().flatten().all(|b| bodies.contains(b))
                    })
                });
            }
        }
        Ok(snapshot)
    }
}
fn record<T>(path: PathId, descriptor: Result<T, PhysicsSceneError>) -> PhysicsRecord<T> {
    PhysicsRecord { path, descriptor }
}
fn owner_matches(targets: &[TargetPath], owners: &[SimulationOwner]) -> bool {
    if targets.is_empty() {
        owners.contains(&SimulationOwner::Default)
    } else {
        targets.iter().any(
            |t| matches!(t,TargetPath::Prim(p) if owners.contains(&SimulationOwner::Scene(*p))),
        )
    }
}
fn filtered(prim: &PrimView<'_>) -> Vec<TargetPath> {
    if prim
        .scene()
        .has_api(prim.path(), "PhysicsFilteredPairsAPI", None)
    {
        prim.read_targets("physics:filteredPairs")
    } else {
        Vec::new()
    }
}
fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &'static str,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Result<T, PhysicsSceneError> {
    prim.read_value(name, decode)
        .ok_or(PhysicsSceneError::InvalidAttribute(name))
}
fn float(prim: &PrimView<'_>, name: &'static str) -> Result<f32, PhysicsSceneError> {
    read(prim, name, crate::value::read_float).and_then(|v| {
        if v.is_nan() {
            Err(PhysicsSceneError::InvalidAttribute(name))
        } else {
            Ok(v)
        }
    })
}
fn vector(prim: &PrimView<'_>, name: &'static str) -> Result<[f32; 3], PhysicsSceneError> {
    read(prim, name, crate::value::read_float3).and_then(|v| {
        if v.iter().any(|v| !v.is_finite()) {
            Err(PhysicsSceneError::InvalidAttribute(name))
        } else {
            Ok(v)
        }
    })
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenUSD physics descriptors narrow USD double transforms to float"
)]
fn transform(matrix: &[[f64; 4]; 4]) -> Result<PhysicsTransform, PhysicsSceneError> {
    let factors =
        AffineFactors::compute(matrix).map_err(|_| PhysicsSceneError::InvalidTransform)?;
    let trs = factors
        .to_trs(1e-6)
        .map_err(|_| PhysicsSceneError::InvalidTransform)?;
    let q = crate::gf::decomposition::quaternion(&trs.rotation);
    let result = PhysicsTransform {
        position: trs.translation.map(|v| v as f32),
        scale: trs.scale.map(|v| v as f32),
        orientation: [q[1] as f32, q[2] as f32, q[3] as f32, q[0] as f32],
    };
    if result
        .position
        .iter()
        .chain(&result.scale)
        .chain(&result.orientation)
        .any(|v| !v.is_finite())
    {
        Err(PhysicsSceneError::InvalidTransform)
    } else {
        Ok(result)
    }
}
fn world(prim: &PrimView<'_>, xforms: &mut XformCache) -> Result<[[f64; 4]; 4], PhysicsSceneError> {
    checked_world(&prim.scene(), prim.path(), xforms)
}
fn checked_world(
    scene: &Scene<'_>,
    path: PathId,
    xforms: &mut XformCache,
) -> Result<[[f64; 4]; 4], PhysicsSceneError> {
    let mut current = Some(path);
    while let Some(at) = current {
        let local = xforms
            .local_transform(scene, at)
            .ok_or(PhysicsSceneError::InvalidTransform)?;
        if !local.problems.is_empty() {
            return Err(PhysicsSceneError::InvalidTransform);
        }
        if local.resets_xform_stack {
            break;
        }
        current = scene.parent(at);
    }
    xforms
        .local_to_world(scene, path)
        .ok_or(PhysicsSceneError::InvalidTransform)
}
fn body(
    api: PhysicsRigidBodyApi<'_>,
    xforms: &mut XformCache,
) -> Result<RigidBodyDescriptor, PhysicsSceneError> {
    let world_matrix = world(&api, xforms)?;
    Ok(RigidBodyDescriptor {
        world_transform: transform(&world_matrix)?,
        world_matrix,
        enabled: read(&api, "physics:rigidBodyEnabled", crate::value::read_bool)?,
        kinematic: read(&api, "physics:kinematicEnabled", crate::value::read_bool)?,
        starts_asleep: read(&api, "physics:startsAsleep", crate::value::read_bool)?,
        linear_velocity: vector(&api, "physics:velocity")?,
        angular_velocity: vector(&api, "physics:angularVelocity")?,
        simulation_owners: api.simulation_owner(),
        filtered_collisions: filtered(&api),
        collisions: Vec::new(),
    })
}
fn material(api: PhysicsMaterialApi<'_>) -> Result<PhysicsMaterialDescriptor, PhysicsSceneError> {
    Ok(PhysicsMaterialDescriptor {
        static_friction: float(&api, "physics:staticFriction")?,
        dynamic_friction: float(&api, "physics:dynamicFriction")?,
        restitution: float(&api, "physics:restitution")?,
        density: float(&api, "physics:density")?,
    })
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenUSD's default gravity divides float stage meters per unit"
)]
fn scene_descriptor(api: PhysicsScene<'_>) -> Result<PhysicsSceneDescriptor, PhysicsSceneError> {
    let mut direction = vector(&api, "physics:gravityDirection")?;
    let magnitude = float(&api, "physics:gravityMagnitude")?;
    let metadata = api.scene().metadata();
    let meters_per_unit = metadata.meters_per_unit().unwrap_or(0.01);
    if !meters_per_unit.is_finite() || meters_per_unit <= 0. {
        return Err(PhysicsSceneError::InvalidAttribute("metersPerUnit"));
    }
    if direction == [0.; 3] {
        direction = match metadata.up_axis() {
            Some("X") => [-1., 0., 0.],
            Some("Z") => [0., 0., -1.],
            _ => [0., -1., 0.],
        };
    } else {
        let length = libm::sqrt(direction.iter().map(|&v| f64::from(v) * f64::from(v)).sum());
        direction = direction.map(|v| (f64::from(v) / length.max(1e-10)) as f32);
    }
    let magnitude = if magnitude < -0.5e38 {
        // Preserve native float gravity rounding, but reject narrowing underflow
        // and overflow instead of emitting an infinite/default-zero magnitude.
        let units = meters_per_unit as f32;
        if !units.is_finite() || units <= 0. {
            return Err(PhysicsSceneError::InvalidAttribute("metersPerUnit"));
        }
        9.81 / units
    } else {
        magnitude
    };
    if !magnitude.is_finite() {
        return Err(PhysicsSceneError::InvalidAttribute(
            "physics:gravityMagnitude",
        ));
    }
    Ok(PhysicsSceneDescriptor {
        gravity_direction: direction,
        gravity_magnitude: magnitude,
        meters_per_unit,
    })
}
fn shape(
    api: PhysicsCollisionApi<'_>,
    bodies: &[PhysicsRecord<RigidBodyDescriptor>],
    groups: &[PhysicsRecord<CollisionGroupDescriptor>],
    xforms: &mut XformCache,
) -> Result<ShapeDescriptor, PhysicsSceneError> {
    let scene = api.scene();
    let world_matrix = world(&api, xforms)?;
    let world_transform = transform(&world_matrix)?;
    let mut current = Some(api.path());
    let mut body = None;
    while let Some(path) = current {
        if let Some(found) = bodies.iter().find(|b| b.path == path) {
            body = Some(found);
            break;
        }
        current = scene.parent(path);
    }
    let mut local_transform = if let Some(body) = body {
        let body = body.descriptor.as_ref().map_err(Clone::clone)?;
        let (inverse, det) = crate::gf::inverse(&body.world_matrix);
        if det == 0. {
            return Err(PhysicsSceneError::InvalidTransform);
        }
        let mut local = transform(&crate::gf::mul(&world_matrix, &inverse))?;
        for i in 0..3 {
            local.position[i] *= body.world_transform.scale[i];
            if !local.position[i].is_finite() {
                return Err(PhysicsSceneError::InvalidTransform);
            }
        }
        local
    } else {
        world_transform
    };
    // Exact self-body shape pose is identity, not a rounded inverse product.
    if body.is_some_and(|b| b.path == api.path()) {
        local_transform = PhysicsTransform {
            position: [0.; 3],
            orientation: [0., 0., 0., 1.],
            scale: [1.; 3],
        };
    }
    let geometry = geometry::capture(&api, world_transform.scale)?;
    #[cfg(feature = "usd-shade")]
    let materials = Some(geometry::materials(&api));
    #[cfg(not(feature = "usd-shade"))]
    let materials = None;
    Ok(ShapeDescriptor {
        rigid_body: body.map(|b| b.path),
        local_transform,
        world_matrix,
        geometry,
        enabled: read(&api, "physics:collisionEnabled", crate::value::read_bool)?,
        simulation_owners: api.simulation_owner(),
        filtered_collisions: filtered(&api),
        materials,
        collision_groups: groups
            .iter()
            .filter(|g| {
                g.descriptor
                    .as_ref()
                    .is_ok_and(|g| g.colliders.contains(&api.path()))
            })
            .map(|g| g.path)
            .collect(),
    })
}
#[cfg(test)]
mod tests;
