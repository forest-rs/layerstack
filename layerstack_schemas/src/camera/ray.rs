// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Rays and their intersection tests, as `GfRay` computes them.
//!
//! OpenUSD `pxr/base/gf/ray.cpp`, `plane.cpp` and `bbox3d.cpp`; the
//! tolerances are `GF_MIN_VECTOR_LENGTH` (`1e-10`) and `GfRay`'s quadratic
//! tolerance (`1e-6`).
use crate::{
    bounds::{BoundingBox, Range3d},
    gf,
};

type Matrix = [[f64; 4]; 4];

/// `GF_MIN_VECTOR_LENGTH`: below it a vector or dot product counts as zero.
const MIN_VECTOR_LENGTH: f64 = 1e-10;
/// `GfRay`'s tolerance for a vanishing quadratic coefficient or discriminant.
const QUADRATIC_TOLERANCE: f64 = 1e-6;
/// `GfBBox3d`'s determinant limit below which its matrix counts as singular.
const BBOX_PRECISION_LIMIT: f64 = 1e-13;

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// `GfVec3d::GetNormalized`: divides by the length, or by
/// `GF_MIN_VECTOR_LENGTH` when the length is not above it.
pub(super) fn normalized(v: [f64; 3]) -> [f64; 3] {
    let length = libm::sqrt(dot(v, v));
    let divisor = if length > MIN_VECTOR_LENGTH {
        length
    } else {
        MIN_VECTOR_LENGTH
    };
    v.map(|c| c / divisor)
}

/// A half-line from `origin` along `direction`, as `GfRay` stores it.
///
/// The direction need not be unit length: every distance a test reports is
/// the parameter `t` of the hit point `origin + t * direction`, as `GfRay`
/// reports it, so it is a scene-unit distance only for a unit direction.
/// Pick rays from [`super::PreparedFrustum`] have unit directions.
#[derive(Clone, Copy, Debug, PartialEq)]
#[doc(alias = "GfRay")]
pub struct Ray {
    /// The start point.
    pub origin: [f64; 3],
    /// The direction, not necessarily normalized.
    pub direction: [f64; 3],
}

/// Where a ray crosses a plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaneHit {
    /// Ray parameter of the hit point.
    pub distance: f64,
    /// Whether the ray meets the plane's front (normal) side: the direction
    /// points against the normal.
    pub front_facing: bool,
}

/// Where a ray crosses a triangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriangleHit {
    /// Ray parameter of the hit point.
    pub distance: f64,
    /// Weights of `p0`, `p1` and `p2` at the hit point; they sum to one.
    /// The first is rounded to single precision, as `GfRay` computes it.
    pub barycentric: [f64; 3],
    /// Whether the ray meets the side the counterclockwise winding
    /// `p0, p1, p2` faces, the side `(p1 - p0) × (p2 - p0)` points to.
    pub front_facing: bool,
}

/// The ray parameters where a ray enters and leaves a solid. `enter` is
/// negative when the ray starts inside; `enter == exit` for a tangent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayInterval {
    /// Ray parameter where the ray enters the solid.
    pub enter: f64,
    /// Ray parameter where the ray leaves the solid; never negative.
    pub exit: f64,
}

impl Ray {
    /// The point at ray parameter `distance`, as `GfRay::GetPoint`.
    #[must_use]
    #[doc(alias = "GetPoint")]
    pub fn point(&self, distance: f64) -> [f64; 3] {
        core::array::from_fn(|i| self.origin[i] + distance * self.direction[i])
    }

    /// The ray carried by a row-vector matrix, as `GfRay::Transform`: the
    /// origin as a point, the direction as a direction (no translation, not
    /// renormalized), so ray parameters are preserved.
    #[must_use]
    #[doc(alias = "Transform")]
    pub fn transformed(&self, matrix: &Matrix) -> Self {
        let origin = self.origin;
        let direction = self.direction;
        Self {
            origin: core::array::from_fn(|j| {
                origin[0] * matrix[0][j]
                    + origin[1] * matrix[1][j]
                    + origin[2] * matrix[2][j]
                    + matrix[3][j]
            }),
            direction: core::array::from_fn(|j| {
                direction[0] * matrix[0][j]
                    + direction[1] * matrix[1][j]
                    + direction[2] * matrix[2][j]
            }),
        }
    }

    /// Intersection with the plane `a*x + b*y + c*z + d = 0`, the
    /// convention of [`super::PreparedFrustum::planes`]. The equation is
    /// normalized as `GfPlane(GfVec4d)` does. A ray within
    /// `GF_MIN_VECTOR_LENGTH` of parallel (including a zero normal) misses,
    /// as does a plane behind the origin. `GfRay::Intersect(GfPlane)`.
    #[must_use]
    #[doc(alias = "Intersect")]
    pub fn intersect_plane(&self, plane: [f64; 4]) -> Option<PlaneHit> {
        let mut normal = [plane[0], plane[1], plane[2]];
        let length = libm::sqrt(dot(normal, normal));
        // GfPlane::Set(GfVec4d): the normal by GfVec3d::Normalize, the
        // distance by the length only when it is nonzero.
        normal = normalized(normal);
        let mut distance = -plane[3];
        if length != 0. {
            distance /= length;
        }
        self.intersect_normalized_plane(normal, distance)
    }

    /// `GfRay::Intersect(GfPlane)` against `normal · x = distance`.
    fn intersect_normalized_plane(&self, normal: [f64; 3], distance: f64) -> Option<PlaneHit> {
        let d = dot(self.direction, normal);
        if d < MIN_VECTOR_LENGTH && d > -MIN_VECTOR_LENGTH {
            return None;
        }
        let plane_point = normal.map(|n| distance * n);
        let t = dot(sub(plane_point, self.origin), normal) / d;
        if t < 0. {
            return None;
        }
        Some(PlaneHit {
            distance: t,
            front_facing: d < 0.,
        })
    }

    /// Intersection with the triangle `p0, p1, p2`, as
    /// `GfRay::Intersect(p0, p1, p2, ...)` with no maximum distance: the
    /// ray meets the triangle's plane (so a degenerate triangle, whose
    /// normal vanishes, and a ray parallel to the plane miss), then the hit
    /// is tested inside the triangle projected onto the plane's dominant
    /// axes, clamping barycentrics within `GF_MIN_VECTOR_LENGTH` below zero.
    /// Both faces are hit; [`TriangleHit::front_facing`] tells them apart.
    /// Like `GfRay`, it can miss some hits on very small triangles.
    #[must_use]
    #[doc(alias = "Intersect")]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "GfRay computes the first barycentric weight in single precision"
    )]
    pub fn intersect_triangle(&self, [p0, p1, p2]: [[f64; 3]; 3]) -> Option<TriangleHit> {
        // GfPlane(p0, p1, p2).
        let normal = normalized(cross(sub(p1, p0), sub(p2, p0)));
        let hit = self.intersect_normalized_plane(normal, dot(normal, p0))?;
        let abs = normal.map(f64::abs);
        let (axis0, axis1) = if abs[0] > abs[1] && abs[0] > abs[2] {
            (1, 2)
        } else if abs[1] > abs[2] {
            (2, 0)
        } else {
            (0, 1)
        };
        let inter0 = self.origin[axis0] + hit.distance * self.direction[axis0];
        let inter1 = self.origin[axis1] + hit.distance * self.direction[axis1];
        let d0 = [inter0 - p0[axis0], inter1 - p0[axis1]];
        let d1 = [p1[axis0] - p0[axis0], p1[axis1] - p0[axis1]];
        let d2 = [p2[axis0] - p0[axis0], p2[axis1] - p0[axis1]];
        let mut beta = (d0[1] * d1[0] - d0[0] * d1[1]) / (d2[1] * d1[0] - d2[0] * d1[1]);
        if beta < 0. && beta > -MIN_VECTOR_LENGTH {
            beta = 0.;
        }
        // Written to also reject a NaN `beta`, as OpenUSD's comparisons do.
        if !(0. ..=1.).contains(&beta) {
            return None;
        }
        let mut alpha = if d1[1] < -MIN_VECTOR_LENGTH || d1[1] > MIN_VECTOR_LENGTH {
            (d0[1] - beta * d2[1]) / d1[1]
        } else {
            (d0[0] - beta * d2[0]) / d1[0]
        };
        if alpha < 0. && alpha > -MIN_VECTOR_LENGTH {
            alpha = 0.;
        }
        // OpenUSD holds the remaining weight in a `float`.
        let mut gamma = (1. - (alpha + beta)) as f32;
        if gamma < 0. && f64::from(gamma) > -MIN_VECTOR_LENGTH {
            gamma = 0.;
        }
        if alpha < 0. || gamma < 0. {
            return None;
        }
        Some(TriangleHit {
            distance: hit.distance,
            barycentric: [f64::from(gamma), alpha, beta],
            front_facing: hit.front_facing,
        })
    }

    /// Intersection with an axis-aligned range, as
    /// `GfRay::Intersect(GfRange3d)`: an empty range misses, an axis within
    /// `GF_MIN_VECTOR_LENGTH` of parallel needs the origin within its slab,
    /// and a range wholly behind the origin misses.
    #[must_use]
    #[doc(alias = "Intersect")]
    pub fn intersect_range(&self, range: &Range3d) -> Option<RayInterval> {
        if range.is_empty() {
            return None;
        }
        let mut enter = -f64::MAX;
        let mut exit = f64::MAX;
        for i in 0..3 {
            let d = self.direction[i];
            if d.abs() < MIN_VECTOR_LENGTH {
                if self.origin[i] < range.min[i] || self.origin[i] > range.max[i] {
                    return None;
                }
                continue;
            }
            let d = 1. / d;
            let mut t1 = d * (range.min[i] - self.origin[i]);
            let mut t2 = d * (range.max[i] - self.origin[i]);
            if t1 > t2 {
                core::mem::swap(&mut t1, &mut t2);
            }
            if t1 > enter {
                enter = t1;
            }
            if t2 < exit {
                exit = t2;
            }
        }
        if enter > exit || exit < 0. {
            return None;
        }
        Some(RayInterval { enter, exit })
    }

    /// Intersection with an oriented bound, as `GfRay::Intersect(GfBBox3d)`:
    /// the ray is carried into the bound's local space, preserving ray
    /// parameters. Like `GfBBox3d`, a matrix whose determinant is within
    /// `1e-13` of zero is degenerate and its inverse is taken as identity,
    /// so the untransformed range is tested.
    #[must_use]
    #[doc(alias = "Intersect")]
    pub fn intersect_bound(&self, bound: &BoundingBox) -> Option<RayInterval> {
        let (inverse, det) = gf::inverse(&bound.matrix);
        let local = if det.abs() <= BBOX_PRECISION_LIMIT {
            *self
        } else {
            self.transformed(&inverse)
        };
        local.intersect_range(&bound.range)
    }

    /// Intersection with a sphere, as `GfRay::Intersect(center, radius)`.
    /// A sphere wholly behind the origin misses.
    #[must_use]
    #[doc(alias = "Intersect")]
    pub fn intersect_sphere(&self, center: [f64; 3], radius: f64) -> Option<RayInterval> {
        let [x1, y1, z1] = self.origin;
        let [x2, y2, z2] = self.point(1.);
        let [x3, y3, z3] = center;
        let a = (x2 - x1) * (x2 - x1) + (y2 - y1) * (y2 - y1) + (z2 - z1) * (z2 - z1);
        let b = 2. * ((x2 - x1) * (x1 - x3) + (y2 - y1) * (y1 - y3) + (z2 - z1) * (z1 - z3));
        let c = x3 * x3 + y3 * y3 + z3 * z3 + x1 * x1 + y1 * y1 + z1 * z1
            - 2. * (x3 * x1 + y3 * y1 + z3 * z1)
            - radius * radius;
        solve_quadratic(a, b, c)
    }

    /// Intersection with an infinite cylinder around the line through
    /// `origin` along `axis`, as `GfRay::Intersect(origin, axis, radius)`.
    #[must_use]
    #[doc(alias = "Intersect")]
    pub fn intersect_cylinder(
        &self,
        origin: [f64; 3],
        axis: [f64; 3],
        radius: f64,
    ) -> Option<RayInterval> {
        let unit = normalized(axis);
        let delta = sub(self.origin, origin);
        let u = reject(self.direction, unit);
        let v = reject(delta, unit);
        solve_quadratic(dot(u, u), 2. * dot(u, v), dot(v, v) - radius * radius)
    }

    /// Intersection with the cone whose base circle of `radius` is centered
    /// at `origin` and whose apex lies `height` along `axis`, as
    /// `GfRay::Intersect(origin, axis, radius, height)`. The cone is
    /// unbounded past the base; hits on the mirrored cone beyond the apex
    /// are dropped, a lone remaining hit filling both ends.
    #[must_use]
    #[doc(alias = "Intersect")]
    pub fn intersect_cone(
        &self,
        origin: [f64; 3],
        axis: [f64; 3],
        radius: f64,
        height: f64,
    ) -> Option<RayInterval> {
        let unit = normalized(axis);
        let apex = core::array::from_fn(|i| origin[i] + height * unit[i]);
        let delta = sub(self.origin, apex);
        let u = reject(self.direction, unit);
        let v = reject(delta, unit);
        let p = dot(self.direction, unit);
        let q = dot(delta, unit);
        let cos2 = height * height / (height * height + radius * radius);
        let sin2 = 1. - cos2;
        let a = cos2 * dot(u, u) - sin2 * p * p;
        let b = 2. * (cos2 * dot(u, v) - sin2 * p * q);
        let c = cos2 * dot(v, v) - sin2 * q * q;
        let mut interval = solve_quadratic(a, b, c)?;
        let enter_valid = dot(unit, sub(self.point(interval.enter), apex)) <= 0.;
        let exit_valid = dot(unit, sub(self.point(interval.exit), apex)) <= 0.;
        match (enter_valid, exit_valid) {
            (false, false) => return None,
            (false, true) => interval.enter = interval.exit,
            (true, false) => interval.exit = interval.enter,
            (true, true) => {}
        }
        Some(interval)
    }
}

/// `v` less its component along the unit vector `unit`.
fn reject(v: [f64; 3], unit: [f64; 3]) -> [f64; 3] {
    let along = dot(v, unit);
    core::array::from_fn(|i| v[i] - along * unit[i])
}

/// `GfIsClose(value, 0, tolerance)`.
fn near_zero(value: f64) -> bool {
    value.abs() <= QUADRATIC_TOLERANCE
}

/// `GfRay::_SolveQuadratic`: the real roots of `a t² + b t + c`, rejecting
/// both roots behind the origin.
fn solve_quadratic(a: f64, b: f64, c: f64) -> Option<RayInterval> {
    if near_zero(a) {
        if near_zero(b) {
            return None;
        }
        let t = -c / b;
        return (t >= 0.).then_some(RayInterval { enter: t, exit: t });
    }
    let disc = b * b - 4. * a * c;
    if near_zero(disc) {
        let t = -b / (2. * a);
        return (t >= 0.).then_some(RayInterval { enter: t, exit: t });
    }
    if disc < 0. {
        return None;
    }
    let q = -0.5 * (b + libm::copysign(1., b) * libm::sqrt(disc));
    let mut t0 = q / a;
    let mut t1 = c / q;
    if t0 > t1 {
        core::mem::swap(&mut t0, &mut t1);
    }
    (t1 >= 0.).then_some(RayInterval {
        enter: t0,
        exit: t1,
    })
}

#[cfg(test)]
mod tests {
    use super::{Ray, RayInterval};
    use crate::bounds::{BoundingBox, Range3d};

    const DOWN: Ray = Ray {
        origin: [0.25, 0.25, 5.],
        direction: [0., 0., -1.],
    };

    #[test]
    fn triangle_hits_report_distance_weights_and_facing() {
        let triangle = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]];
        let hit = DOWN.intersect_triangle(triangle).unwrap();
        assert_eq!(hit.distance, 5.);
        assert_eq!(hit.barycentric, [0.5, 0.25, 0.25]);
        assert!(hit.front_facing);
        let back = DOWN.intersect_triangle([triangle[0], triangle[2], triangle[1]]);
        assert!(!back.unwrap().front_facing);
        let degenerate = [[0., 0., 0.], [1., 1., 0.], [2., 2., 0.]];
        assert_eq!(DOWN.intersect_triangle(degenerate), None);
        let sideways = Ray {
            direction: [1., 0., 0.],
            ..DOWN
        };
        assert_eq!(sideways.intersect_triangle(triangle), None);
    }

    #[test]
    fn plane_hits_reject_parallel_and_behind() {
        let hit = DOWN.intersect_plane([0., 0., 2., -2.]).unwrap();
        assert_eq!(hit.distance, 4.);
        assert!(hit.front_facing);
        assert_eq!(DOWN.intersect_plane([1., 0., 0., 0.]), None);
        assert_eq!(DOWN.intersect_plane([0., 0., 1., -6.]), None);
        assert_eq!(DOWN.intersect_plane([0.; 4]), None);
    }

    #[test]
    fn solids_report_enter_and_exit() {
        let unit = Range3d {
            min: [0.; 3],
            max: [1.; 3],
        };
        assert_eq!(
            DOWN.intersect_range(&unit),
            Some(RayInterval {
                enter: 4.,
                exit: 5.
            })
        );
        assert_eq!(DOWN.intersect_range(&Range3d::default()), None);
        let mut bound = BoundingBox {
            range: unit,
            ..BoundingBox::default()
        };
        bound.matrix[3][0] = 10.;
        assert_eq!(DOWN.intersect_bound(&bound), None);
        bound.matrix[0][0] = 0.;
        // A singular matrix tests the untransformed range, as `GfBBox3d` does.
        assert!(DOWN.intersect_bound(&bound).is_some());
        let sphere = DOWN.intersect_sphere([0.25, 0.25, 0.], 1.).unwrap();
        assert_eq!((sphere.enter, sphere.exit), (4., 6.));
        assert_eq!(DOWN.intersect_sphere([0.25, 0.25, 10.], 1.), None);
        let cylinder = DOWN.intersect_cylinder([0.; 3], [0., 1., 0.], 1.).unwrap();
        assert!(cylinder.enter < cylinder.exit);
        // The mirrored cone above the apex is dropped: one hit fills both ends.
        let offset = Ray {
            origin: [0.75, 0.25, 5.],
            ..DOWN
        };
        let cone = offset
            .intersect_cone([0.25, 0.25, 0.], [0., 0., 1.], 1., 1.)
            .unwrap();
        assert!((cone.enter - 4.5).abs() < 1e-12 && cone.enter == cone.exit);
    }
}
