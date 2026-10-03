// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Backend-neutral mass aggregation and USD rigid-body interpretation.
//!
//! Geometry volume and unit-density inertia belong to the caller's collider
//! backend. USD resolves the composed authored overrides, density and hierarchy.
//! Pure helpers require `usd-physics`; rigid-body evaluation additionally requires
//! `usd-shade` to resolve physics-purpose material bindings.
//! OpenUSD: `UsdPhysicsMassProperties`, `UsdPhysicsRigidBodyAPI::ComputeMassProperties`.
//! Composition follows AOUSD Core §12 and physics schema mass precedence rules.
mod diagonalize;
#[cfg(feature = "usd-shade")]
mod scene;

/// A 3×3 inertia tensor stored as rows.
pub type Matrix3 = [[f32; 3]; 3];
/// Invalid inputs to mass evaluation; no partial aggregate is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MassError {
    /// Mass is negative or nonfinite, or an aggregate has no positive mass.
    InvalidMass,
    /// Inertia is nonfinite, nonsymmetric or has a negative principal moment.
    InvalidInertia,
    /// Position or orientation is nonfinite, or a quaternion is not unit length.
    InvalidTransform,
    /// Stage length or mass units are nonpositive or nonfinite.
    InvalidUnits,
    /// Collider volume is invalid, or zero with a positive mass override.
    InvalidCollider(layerstack::PathId),
    /// A required composed transform cannot be evaluated or factored.
    UnreadableTransform(layerstack::PathId),
}
impl core::fmt::Display for MassError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid mass properties: {self:?}")
    }
}
impl core::error::Error for MassError {}

/// Mass, center of mass and inertia about that center, all in one frame.
/// Values use the stage's length and mass units; no implicit unit conversion occurs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MassProperties {
    /// Total mass.
    pub mass: f32,
    /// Symmetric inertia tensor about `center_of_mass`.
    pub inertia: Matrix3,
    /// Center of mass in the same local frame as the inertia tensor.
    pub center_of_mass: [f32; 3],
}
impl Default for MassProperties {
    /// OpenUSD's unit-mass, identity-inertia default.
    fn default() -> Self {
        Self {
            mass: 1.,
            inertia: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            center_of_mass: [0.; 3],
        }
    }
}
/// One body's properties and a rigid transform into the aggregate frame.
/// Geometry scaling must already be reflected in the supplied properties.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MassContribution {
    /// Properties in the contribution's local frame.
    pub properties: MassProperties,
    /// Local-frame origin in the aggregate frame.
    pub position: [f32; 3],
    /// Unit orientation quaternion in `[x, y, z, real]` order.
    pub orientation: [f32; 4],
}
/// Principal inertia moments and the orientation of their frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrincipalInertia {
    /// Principal moments in OpenUSD's unsorted Jacobi axis order.
    pub diagonal: [f32; 3],
    /// Unit quaternion `[x, y, z, real]` rotating that frame into the body frame.
    pub axes: [f32; 4],
}
impl MassProperties {
    /// Scales mass and inertia together; the center of mass stays fixed.
    /// `factor` should be finite and nonnegative.
    #[must_use]
    pub fn scaled(self, factor: f32) -> Self {
        Self {
            mass: self.mass * factor,
            inertia: self.inertia.map(|row| row.map(|v| v * factor)),
            ..self
        }
    }
    /// Moves the center of mass and adds the parallel-axis inertia adjustment.
    /// This matches OpenUSD's mass-override operation, rather than a rigid move
    /// of the entire object; use [`MassContribution`] for rigid placement.
    #[must_use]
    pub fn translated(self, offset: [f32; 3]) -> Self {
        Self {
            inertia: translate_inertia(self.inertia, self.mass, offset),
            center_of_mass: core::array::from_fn(|i| self.center_of_mass[i] + offset[i]),
            ..self
        }
    }
    /// Diagonalizes this tensor, rejecting invalid inertia. Eigenvector signs
    /// are equivalent; degenerate moments do not define a unique orientation.
    pub fn principal_inertia(&self) -> Result<PrincipalInertia, MassError> {
        diagonalize_inertia(self.inertia)
    }
    /// Combines rigidly placed contributions using a weighted center and the
    /// parallel-axis theorem. Returns a zero aggregate for an empty input or
    /// all-zero masses, matching OpenUSD's pure aggregation helper.
    /// Rejects invalid masses, tensors, positions and orientation quaternions.
    pub fn sum(contributions: &[MassContribution]) -> Result<Self, MassError> {
        let mut result = Self {
            mass: 0.,
            inertia: [[0.; 3]; 3],
            center_of_mass: [0.; 3],
        };
        for c in contributions {
            if !c.properties.mass.is_finite() || c.properties.mass < 0. {
                return Err(MassError::InvalidMass);
            }
            diagonalize_inertia(c.properties.inertia)?;
            check_quaternion(c.orientation)?;
            if !c
                .position
                .iter()
                .chain(&c.properties.center_of_mass)
                .all(|v| v.is_finite())
            {
                return Err(MassError::InvalidTransform);
            }
            let center = transformed_center(c);
            result.mass += c.properties.mass;
            for (i, v) in result.center_of_mass.iter_mut().enumerate() {
                *v += center[i] * c.properties.mass;
            }
        }
        if result.mass > 0. {
            result.center_of_mass = result.center_of_mass.map(|v| v / result.mass);
        }
        for c in contributions {
            let center = transformed_center(c);
            let offset = core::array::from_fn(|i| result.center_of_mass[i] - center[i]);
            let inertia = translate_inertia(
                rotate_inertia(
                    c.properties.inertia,
                    diagonalize::extracted_orientation(c.orientation),
                ),
                c.properties.mass,
                offset,
            );
            for (row, addition) in result.inertia.iter_mut().zip(inertia) {
                for (v, a) in row.iter_mut().zip(addition) {
                    *v += a;
                }
            }
        }
        if !result.mass.is_finite() || !result.center_of_mass.iter().all(|v| v.is_finite()) {
            return Err(MassError::InvalidMass);
        }
        diagonalize_inertia(result.inertia)?;
        Ok(result)
    }
}
fn transformed_center(c: &MassContribution) -> [f32; 3] {
    let r = rotation(c.orientation);
    core::array::from_fn(|j| {
        c.properties.center_of_mass[0] * r[0][j]
            + c.properties.center_of_mass[1] * r[1][j]
            + c.properties.center_of_mass[2] * r[2][j]
            + c.position[j]
    })
}
/// Adds the parallel-axis adjustment for an offset from the original center.
/// Inputs should be finite; mass should be nonnegative.
#[must_use]
pub fn translate_inertia(inertia: Matrix3, mass: f32, offset: [f32; 3]) -> Matrix3 {
    let [x, y, z] = offset;
    let skew = [[0., -z, y], [z, 0., -x], [-y, x, 0.]];
    let adjustment = mul(skew, transpose(skew));
    core::array::from_fn(|i| core::array::from_fn(|j| inertia[i][j] + adjustment[i][j] * mass))
}
/// Rotates inertia about its center with a unit quaternion `[x, y, z, real]`.
#[must_use]
pub fn rotate_inertia(inertia: Matrix3, orientation: [f32; 4]) -> Matrix3 {
    let rotation = rotation(orientation);
    mul(mul(transpose(rotation), inertia), rotation)
}
/// Computes principal moments and axes, preserving OpenUSD's axis order.
/// Rejects nonfinite, nonsymmetric or negative-moment tensors.
pub fn diagonalize_inertia(inertia: Matrix3) -> Result<PrincipalInertia, MassError> {
    check_inertia(inertia)?;
    let (diagonal, axes) = diagonalize::diagonalize(inertia);
    if !diagonal.iter().all(|v| v.is_finite() && *v >= 0.) {
        return Err(MassError::InvalidInertia);
    }
    Ok(PrincipalInertia { diagonal, axes })
}
fn check_inertia(m: Matrix3) -> Result<(), MassError> {
    if !m.iter().flatten().all(|v| v.is_finite())
        || (0..3).any(|i| {
            (0..3).any(|j| {
                (m[i][j] - m[j][i]).abs() > 1e-5 * m[i][j].abs().max(m[j][i].abs()).max(1.)
            })
        })
    {
        return Err(MassError::InvalidInertia);
    }
    Ok(())
}
fn check_quaternion(q: [f32; 4]) -> Result<(), MassError> {
    if !q.iter().all(|v| v.is_finite()) || (q.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 1e-4
    {
        return Err(MassError::InvalidTransform);
    }
    Ok(())
}
fn mul(a: Matrix3, b: Matrix3) -> Matrix3 {
    core::array::from_fn(|i| {
        core::array::from_fn(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j])
    })
}
fn transpose(m: Matrix3) -> Matrix3 {
    core::array::from_fn(|i| core::array::from_fn(|j| m[j][i]))
}
fn rotation([x, y, z, w]: [f32; 4]) -> Matrix3 {
    [
        [
            1. - 2. * (y * y + z * z),
            2. * (x * y + z * w),
            2. * (z * x - y * w),
        ],
        [
            2. * (x * y - z * w),
            1. - 2. * (z * z + x * x),
            2. * (y * z + x * w),
        ],
        [
            2. * (z * x + y * w),
            2. * (y * z - x * w),
            1. - 2. * (y * y + x * x),
        ],
    ]
}

/// Collider information supplied by the geometry or physics backend.
/// Volume, center and unit-density inertia must already include geometry scaling
/// in stage units. The rigid placement is relative to the evaluated rigid body.
/// The callback owns shape integration; USD does not infer a mesh's solid volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollisionMassInformation {
    /// Nonnegative scaled collider volume.
    pub volume: f32,
    /// Inertia about `center_of_mass` at unit density, rather than unit total mass.
    pub inertia: Matrix3,
    /// Scaled center of mass in collider-local coordinates.
    pub center_of_mass: [f32; 3],
    /// Collider-local origin in the rigid body's scaled frame.
    pub local_position: [f32; 3],
    /// Unit collider orientation `[x, y, z, real]` relative to the body.
    pub local_orientation: [f32; 4],
}
/// Resolved rigid-body mass properties at default time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RigidBodyMass {
    /// Positive total mass in stage mass units.
    pub mass: f32,
    /// Scaled body-local center of mass.
    pub center_of_mass: [f32; 3],
    /// Principal moments in stage mass × length² units.
    pub diagonal_inertia: [f32; 3],
    /// Unit principal-frame orientation `[x, y, z, real]`. Without colliders
    /// or authored axes, identity is returned deterministically; OpenUSD 26.8
    /// leaves its output uninitialized in that case.
    pub principal_axes: [f32; 4],
    /// Colliders whose geometry was requested, excluding nested rigid bodies.
    pub collider_count: usize,
    /// With no colliders or authored inertia, used OpenUSD's 0.1-meter sphere.
    pub used_sphere_approximation: bool,
}
