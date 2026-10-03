// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::usd_physics::{PhysicsDriveApi, PhysicsLimitApi};
/// A joint degree of freedom, in OpenUSD's canonical D6 order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JointDof {
    /// Radial distance.
    Distance,
    /// Translation along X.
    TranslationX,
    /// Translation along Y.
    TranslationY,
    /// Translation along Z.
    TranslationZ,
    /// Rotation about X, in degrees.
    RotationX,
    /// Rotation about Y, in degrees.
    RotationY,
    /// Rotation about Z, in degrees.
    RotationZ,
}
/// Limit configuration. A lower bound greater than upper means locked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointLimit {
    /// Whether either/both bounds activate this type's limit.
    pub enabled: bool,
    /// Lower bound or distance minimum / spherical cone-angle zero.
    pub lower: f32,
    /// Upper bound or distance maximum / spherical cone-angle one.
    pub upper: f32,
}
/// Applied joint drive parameters; force = stiffness*(target-position)+
/// damping*(target_velocity-velocity), before force/acceleration conventions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointDrive {
    /// Target position, degrees for angular drives.
    pub target_position: f32,
    /// Target velocity, degrees/second for angular drives.
    pub target_velocity: f32,
    /// Maximum force/torque; infinity denotes unlimited.
    pub maximum_force: f32,
    /// Spring stiffness.
    pub stiffness: f32,
    /// Damping coefficient.
    pub damping: f32,
    /// Acceleration rather than force drive.
    pub acceleration: bool,
}
/// Specialized joint configuration, with absent drives represented explicitly.
#[derive(Clone, Debug, PartialEq)]
pub enum JointKind {
    /// Zero degrees of freedom.
    Fixed,
    /// Rotation about one axis.
    Revolute {
        /// Rotation axis.
        axis: PhysicsAxis,
        /// Angular limit in degrees, enabled only when both bounds are finite.
        limit: JointLimit,
        /// Applied `angular` drive.
        drive: Option<JointDrive>,
    },
    /// Translation along one axis.
    Prismatic {
        /// Translation axis.
        axis: PhysicsAxis,
        /// Linear limit, enabled when either bound is finite.
        limit: JointLimit,
        /// Applied `linear` drive.
        drive: Option<JointDrive>,
    },
    /// Three rotational degrees of freedom with optional cone constraints.
    Spherical {
        /// Cone axis.
        axis: PhysicsAxis,
        /// Cone angles in degrees, enabled only when both are nonnegative finite.
        limit: JointLimit,
    },
    /// Distance constraint with independently enabled minimum/maximum.
    Distance {
        /// Minimum distance; negative disables it.
        minimum: f32,
        /// Maximum distance; negative disables it.
        maximum: f32,
        /// Whether the minimum is nonnegative.
        minimum_enabled: bool,
        /// Whether the maximum is nonnegative.
        maximum_enabled: bool,
    },
    /// Generic USD joint with applied limits/drives for seven degrees of freedom.
    D6 {
        /// Applied per-axis limits, in canonical DOF order.
        limits: Vec<(JointDof, JointLimit)>,
        /// Applied per-axis drives, in canonical DOF order.
        drives: Vec<(JointDof, JointDrive)>,
    },
}
/// Local joint anchor with scale baked into position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointPose {
    /// Position in body coordinates (world coordinates for an absent body).
    pub position: [f32; 3],
    /// Normalized quaternion `[i,j,k,r]`.
    pub orientation: [f32; 4],
}
/// Common joint values and its resolved physics endpoints.
#[derive(Clone, Debug, PartialEq)]
pub struct JointDescriptor {
    /// Original relationship targets; empty relationships use world space.
    pub relationship_targets: [Option<PathId>; 2],
    /// Resolved nearest body (or static collision ancestor); `None` means world.
    pub bodies: [Option<PathId>; 2],
    /// Body-local or world anchor poses.
    pub local_poses: [JointPose; 2],
    /// Whether the constraint is enabled.
    pub enabled: bool,
    /// Whether collision between connected objects is enabled.
    pub collision_enabled: bool,
    /// Force needed to break the constraint; infinity is unlimited.
    pub break_force: f32,
    /// Torque needed to break the constraint; infinity is unlimited.
    pub break_torque: f32,
    /// Whether this joint is excluded from articulation traversal.
    pub exclude_from_articulation: bool,
    /// Type-specific configuration.
    pub kind: JointKind,
}
fn drive(api: PhysicsDriveApi<'_>) -> Result<JointDrive, PhysicsSceneError> {
    let required = |value: Option<f32>, name| {
        value
            .filter(|v| !v.is_nan())
            .ok_or(PhysicsSceneError::InvalidAttribute(name))
    };
    Ok(JointDrive {
        target_position: required(api.target_position(), "drive:targetPosition")?,
        target_velocity: required(api.target_velocity(), "drive:targetVelocity")?,
        maximum_force: required(api.max_force(), "drive:maxForce")?,
        stiffness: required(api.stiffness(), "drive:stiffness")?,
        damping: required(api.damping(), "drive:damping")?,
        acceleration: matches!(
            api.drive_type(),
            Some(crate::usd_physics::PhysicsDriveApiDriveType::Acceleration)
        ),
    })
}
fn get_drive(prim: &PrimView<'_>, instance: &str) -> Result<Option<JointDrive>, PhysicsSceneError> {
    PhysicsDriveApi::get(&prim.scene(), prim.path(), instance)
        .map(drive)
        .transpose()
}
fn limit(lower: f32, upper: f32, both: bool) -> JointLimit {
    let low = lower.is_finite() && lower > -0.5e38;
    let high = upper.is_finite() && upper < 0.5e38;
    JointLimit {
        lower,
        upper,
        enabled: if both { low && high } else { low || high },
    }
}
fn target(prim: &PrimView<'_>, name: &str) -> Result<Option<PathId>, PhysicsSceneError> {
    match prim.read_targets(name).first() {
        None => Ok(None),
        Some(TargetPath::Prim(path)) if prim.scene().stage().has_prim(*path) => Ok(Some(*path)),
        _ => Err(PhysicsSceneError::InvalidJointTarget),
    }
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "normalization bounds each component to [-1,1]"
)]
fn normalized(mut q: [f32; 4]) -> Result<[f32; 4], PhysicsSceneError> {
    if q.iter().any(|v| !v.is_finite()) {
        return Err(PhysicsSceneError::InvalidAttribute("localRot"));
    }
    let length = libm::sqrt(q.iter().map(|&v| f64::from(v) * f64::from(v)).sum());
    if length < 1e-10 {
        q = [0., 0., 0., 1.];
    } else {
        q = q.map(|v| (f64::from(v) / length) as f32);
    }
    Ok(q)
}
fn pose(
    scene: &Scene<'_>,
    target: Option<PathId>,
    mut position: [f32; 3],
    mut orientation: [f32; 4],
    xforms: &mut XformCache,
) -> Result<(Option<PathId>, JointPose), PhysicsSceneError> {
    let Some(target) = target else {
        return Ok((
            None,
            JointPose {
                position,
                orientation,
            },
        ));
    };
    let mut current = Some(target);
    let mut body = None;
    let mut collision = None;
    while let Some(path) = current {
        if scene.has_api(path, "PhysicsRigidBodyAPI", None) {
            body = Some(path);
            break;
        }
        if scene.has_api(path, "PhysicsCollisionAPI", None) {
            collision = Some(path);
        }
        current = scene.parent(path);
    }
    let body = body.or(collision);
    let world_target = checked_world(scene, target, xforms)?;
    let body_world = if let Some(body) = body {
        checked_world(scene, body, xforms)?
    } else {
        crate::gf::IDENTITY
    };
    let body_transform = transform(&body_world)?;
    if body != Some(target) {
        let mut local = crate::gf::Rotation::from_quat(orientation.map(f64::from)).matrix();
        local[3][..3].copy_from_slice(&position.map(f64::from));
        let (inverse, det) = crate::gf::inverse(&body_world);
        if det == 0. {
            return Err(PhysicsSceneError::InvalidTransform);
        }
        let relative = crate::gf::mul(&crate::gf::mul(&local, &world_target), &inverse);
        let relative = transform(&relative)?;
        position = relative.position;
        orientation = relative.orientation;
    }
    for (p, scale) in position.iter_mut().zip(body_transform.scale) {
        *p *= scale;
        if !p.is_finite() {
            return Err(PhysicsSceneError::InvalidAttribute("localPos"));
        }
    }
    Ok((
        body,
        JointPose {
            position,
            orientation,
        },
    ))
}
pub(super) fn capture(
    scene: &Scene<'_>,
    path: PathId,
    xforms: &mut XformCache,
) -> Result<JointDescriptor, PhysicsSceneError> {
    let prim = PrimView::new(*scene, path);
    let targets = [
        target(&prim, "physics:body0")?,
        target(&prim, "physics:body1")?,
    ];
    let (body0, pose0) = pose(
        scene,
        targets[0],
        vector(&prim, "physics:localPos0")?,
        normalized(read(&prim, "physics:localRot0", crate::value::read_quatf)?)?,
        xforms,
    )?;
    let (body1, pose1) = pose(
        scene,
        targets[1],
        vector(&prim, "physics:localPos1")?,
        normalized(read(&prim, "physics:localRot1", crate::value::read_quatf)?)?,
        xforms,
    )?;
    let axis = || geometry::axis(&prim, "physics:axis", PhysicsAxis::X);
    let kind = if scene.is_a(path, "PhysicsFixedJoint") {
        JointKind::Fixed
    } else if scene.is_a(path, "PhysicsRevoluteJoint") {
        JointKind::Revolute {
            axis: axis()?,
            limit: limit(
                float(&prim, "physics:lowerLimit")?,
                float(&prim, "physics:upperLimit")?,
                true,
            ),
            drive: get_drive(&prim, "angular")?,
        }
    } else if scene.is_a(path, "PhysicsPrismaticJoint") {
        JointKind::Prismatic {
            axis: axis()?,
            limit: limit(
                float(&prim, "physics:lowerLimit")?,
                float(&prim, "physics:upperLimit")?,
                false,
            ),
            drive: get_drive(&prim, "linear")?,
        }
    } else if scene.is_a(path, "PhysicsSphericalJoint") {
        let lower = float(&prim, "physics:coneAngle0Limit")?;
        let upper = float(&prim, "physics:coneAngle1Limit")?;
        JointKind::Spherical {
            axis: axis()?,
            limit: JointLimit {
                lower,
                upper,
                enabled: lower.is_finite() && upper.is_finite() && lower >= 0. && upper >= 0.,
            },
        }
    } else if scene.is_a(path, "PhysicsDistanceJoint") {
        let minimum = float(&prim, "physics:minDistance")?;
        let maximum = float(&prim, "physics:maxDistance")?;
        JointKind::Distance {
            minimum,
            maximum,
            minimum_enabled: minimum >= 0.,
            maximum_enabled: maximum >= 0.,
        }
    } else {
        let dofs = [
            (JointDof::Distance, "distance"),
            (JointDof::TranslationX, "transX"),
            (JointDof::TranslationY, "transY"),
            (JointDof::TranslationZ, "transZ"),
            (JointDof::RotationX, "rotX"),
            (JointDof::RotationY, "rotY"),
            (JointDof::RotationZ, "rotZ"),
        ];
        let mut limits = Vec::new();
        let mut drives = Vec::new();
        for (dof, instance) in dofs {
            if let Some(api) = PhysicsLimitApi::get(scene, path, instance) {
                limits.push((
                    dof,
                    limit(
                        api.low()
                            .ok_or(PhysicsSceneError::InvalidAttribute("limit:low"))?,
                        api.high()
                            .ok_or(PhysicsSceneError::InvalidAttribute("limit:high"))?,
                        false,
                    ),
                ));
            }
            if let Some(drive) = get_drive(&prim, instance)? {
                drives.push((dof, drive));
            }
        }
        JointKind::D6 { limits, drives }
    };
    Ok(JointDescriptor {
        relationship_targets: targets,
        bodies: [body0, body1],
        local_poses: [pose0, pose1],
        enabled: read(&prim, "physics:jointEnabled", crate::value::read_bool)?,
        collision_enabled: read(&prim, "physics:collisionEnabled", crate::value::read_bool)?,
        break_force: float(&prim, "physics:breakForce")?,
        break_torque: float(&prim, "physics:breakTorque")?,
        exclude_from_articulation: read(
            &prim,
            "physics:excludeFromArticulation",
            crate::value::read_bool,
        )?,
        kind,
    })
}
