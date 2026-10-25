// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Viewing frusta prepared for repeated culling and picking, as `GfFrustum`.
//!
//! OpenUSD `pxr/base/gf/frustum.cpp`. A frustum is a conformed (rigid,
//! right-handed) camera placement, a reference window, a near/far interval and
//! a projection. Perspective windows lie on the eye-space plane at depth 1
//! (`GfFrustum::GetReferencePlaneDepth`); orthographic windows are in scene
//! units. Normalized window points run from `(-1, -1)` at the lower left to
//! `(1, 1)` at the upper right.
use super::{CameraError, ray::Ray};
use crate::{bounds::BoundingBox, gf};

type Matrix = [[f64; 4]; 4];

/// How a frustum projects eye space onto its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(alias = "GfFrustum::ProjectionType")]
pub enum FrustumProjection {
    /// Rays diverge from the eye through the window at depth 1.
    Perspective,
    /// Rays run parallel to `-Z` through the window.
    Orthographic,
}

/// Which window extent a field of view measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOfViewAxis {
    /// The field of view spans the window's height (`isFovVertical`).
    Vertical,
    /// The field of view spans the window's width.
    Horizontal,
}

/// Symmetric perspective parameters, as `GfFrustum::SetPerspective` takes
/// them and `GfFrustum::GetPerspective` returns them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Perspective {
    /// Full field of view in degrees along the chosen [`FieldOfViewAxis`].
    pub field_of_view: f64,
    /// Window width over height. Zero is taken as 1 when building a frustum.
    pub aspect_ratio: f64,
    /// Near clipping distance in scene units.
    pub near: f64,
    /// Far clipping distance in scene units.
    pub far: f64,
}

/// A world-to-eye transform and its inverse, from a conformed placement.
#[derive(Clone, Copy, Debug)]
pub(super) struct ConformedView {
    pub(super) view: Matrix,
    pub(super) view_inverse: Matrix,
}

/// Conform a camera-to-world transform as
/// `GfFrustum::SetPositionAndRotationFromMatrix`: flip a left-handed basis's
/// first row, orthonormalize, divide out a homogeneous `w`, and keep only the
/// rotation and translation. `None` for a nonfinite or singular transform.
pub(super) fn conform(camera_to_world: &Matrix) -> Option<ConformedView> {
    if camera_to_world.iter().flatten().any(|v| !v.is_finite()) {
        return None;
    }
    let mut linear = gf::IDENTITY;
    for i in 0..3 {
        linear[i][..3].copy_from_slice(&camera_to_world[i][..3]);
    }
    let (_, det) = gf::inverse(&linear);
    if !det.is_finite() || det == 0. {
        return None;
    }
    if det < 0. {
        linear[0] = linear[0].map(|v| -v);
    }
    let rows = core::array::from_fn(|i| core::array::from_fn(|j| linear[i][j]));
    let q = gf::decomposition::quaternion(&gf::decomposition::orthonormalize(rows));
    let rotation = gf::Rotation::from_quat([q[1], q[2], q[3], q[0]]);
    let w = camera_to_world[3][3];
    let position: [f64; 3] = core::array::from_fn(|i| {
        if w != 1. && w.abs() > 1e-10 {
            camera_to_world[3][i] / w
        } else {
            camera_to_world[3][i]
        }
    });
    let view = gf::mul(
        &gf::translate(position.map(|v| -v)),
        &rotation.inverse().matrix(),
    );
    let (view_inverse, det) = gf::inverse(&view);
    if det == 0. || view.iter().flatten().any(|v| !v.is_finite()) {
        return None;
    }
    Some(ConformedView { view, view_inverse })
}

pub(super) fn transform_point(point: [f64; 3], matrix: &Matrix) -> [f64; 3] {
    core::array::from_fn(|j| {
        point[0] * matrix[0][j] + point[1] * matrix[1][j] + point[2] * matrix[2][j] + matrix[3][j]
    })
}

fn transform_dir(dir: [f64; 3], matrix: &Matrix) -> [f64; 3] {
    core::array::from_fn(|j| dir[0] * matrix[0][j] + dir[1] * matrix[1][j] + dir[2] * matrix[2][j])
}

/// `GfFrustum::ComputeProjectionMatrix` with OpenGL `[-1,1]` depth.
pub(super) fn projection_matrix(
    window: [f64; 4],
    clipping_range: [f64; 2],
    projection: FrustumProjection,
) -> Matrix {
    let [l, b, r, t] = window;
    let [n, f] = clipping_range;
    let rl = r - l;
    let tb = t - b;
    let fn_ = f - n;
    let mut matrix = gf::IDENTITY;
    matrix[0][0] = 2. / rl;
    matrix[1][1] = 2. / tb;
    if projection == FrustumProjection::Orthographic {
        matrix[2][2] = -2. / fn_;
        matrix[3][0] = -(r + l) / rl;
        matrix[3][1] = -(t + b) / tb;
        matrix[3][2] = -(f + n) / fn_;
    } else {
        matrix[2][2] = -(f + n) / fn_;
        matrix[2][0] = (r + l) / rl;
        matrix[2][1] = (t + b) / tb;
        matrix[3][2] = -2. * n * f / fn_;
        matrix[2][3] = -1.;
        matrix[3][3] = 0.;
    }
    matrix
}

/// `GfFrustum::ComputeCorners`: near then far, each lower-left, lower-right,
/// upper-left, upper-right.
pub(super) fn corners(
    view_inverse: &Matrix,
    window: [f64; 4],
    clipping_range: [f64; 2],
    projection: FrustumProjection,
) -> [[f64; 3]; 8] {
    core::array::from_fn(|i| {
        let distance = clipping_range[i / 4];
        let scale = if projection == FrustumProjection::Perspective {
            distance
        } else {
            1.
        };
        transform_point(
            [
                window[if i % 2 == 0 { 0 } else { 2 }] * scale,
                window[if i % 4 < 2 { 1 } else { 3 }] * scale,
                -distance,
            ],
            view_inverse,
        )
    })
}

/// Six normalized inward world-space planes: left, right, bottom, top, near,
/// far, in `GfFrustum`'s order.
pub(super) fn planes(
    view: &Matrix,
    window: [f64; 4],
    clipping_range: [f64; 2],
    projection: FrustumProjection,
) -> [[f64; 4]; 6] {
    let [l, b, r, t] = window;
    let [n, f] = clipping_range;
    // Derive from the window rather than subtracting projection columns:
    // large far/near ratios can round the far plane's normal to zero.
    let eye = if projection == FrustumProjection::Perspective {
        [
            [1., 0., l, 0.],
            [-1., 0., -r, 0.],
            [0., 1., b, 0.],
            [0., -1., -t, 0.],
            [0., 0., -1., -n],
            [0., 0., 1., f],
        ]
    } else {
        [
            [1., 0., 0., -l],
            [-1., 0., 0., r],
            [0., 1., 0., -b],
            [0., -1., 0., t],
            [0., 0., -1., -n],
            [0., 0., 1., f],
        ]
    };
    eye.map(|p| {
        let length = libm::sqrt(p[0] * p[0] + p[1] * p[1] + p[2] * p[2]);
        let p = p.map(|v| v / length);
        // Row-vector world-to-eye transform acts on plane coefficients
        // as a matrix times a column vector.
        let world: [f64; 4] = core::array::from_fn(|i| (0..4).map(|j| view[i][j] * p[j]).sum());
        let length = libm::sqrt(world[0] * world[0] + world[1] * world[1] + world[2] * world[2]);
        world.map(|v| v / length)
    })
}

#[inline]
fn signed_distance(plane: &[f64; 4], point: [f64; 3]) -> f64 {
    plane[0] * point[0] + plane[1] * point[1] + plane[2] * point[2] + plane[3]
}

#[inline]
pub(super) fn contains_planes(planes: &[[f64; 4]; 6], point: [f64; 3]) -> bool {
    planes.iter().all(|p| signed_distance(p, point) >= 0.)
}

#[inline]
pub(super) fn intersects_planes(planes: &[[f64; 4]; 6], bound: &BoundingBox) -> bool {
    planes.iter().all(|plane| {
        (0..8).any(|corner| {
            let point = core::array::from_fn(|i| {
                if corner & (1 << i) == 0 {
                    bound.range.min[i]
                } else {
                    bound.range.max[i]
                }
            });
            signed_distance(plane, transform_point(point, &bound.matrix)) >= 0.
        })
    })
}

/// An immutable viewing frustum with its planes prepared for repeated
/// world-space culling and picking queries, as `GfFrustum` with its planes
/// cached.
///
/// Prepare from a [`super::ComputedCamera`] with
/// [`super::ComputedCamera::prepare_frustum`], or build one directly with
/// [`Self::new`] or [`Self::from_perspective`]. It owns its planes and
/// matrices without scene borrows or allocations, and does not observe camera,
/// time or scene changes: prepare again after such changes. Additional
/// authored clipping planes are excluded, as in the one-shot camera helpers.
#[derive(Clone, Debug)]
#[doc(alias = "GfFrustum")]
pub struct PreparedFrustum {
    planes: [[f64; 4]; 6],
    view: Matrix,
    view_inverse: Matrix,
    window: [f64; 4],
    clipping_range: [f64; 2],
    projection: FrustumProjection,
}

impl PreparedFrustum {
    /// Prepare from an already conformed view, as a camera holds it.
    pub(super) fn from_view(
        view: ConformedView,
        window: [f64; 4],
        clipping_range: [f64; 2],
        projection: FrustumProjection,
    ) -> Self {
        Self {
            planes: planes(&view.view, window, clipping_range, projection),
            view: view.view,
            view_inverse: view.view_inverse,
            window,
            clipping_range,
            projection,
        }
    }

    /// A frustum placed by a camera-to-world transform, as `GfFrustum`'s
    /// `(camToWorldXf, window, nearFar, projectionType)` constructor. The
    /// transform is conformed by `GfFrustum::SetPositionAndRotationFromMatrix`:
    /// a left-handed basis has its first row flipped, scale and shear are
    /// removed, and only rotation and translation remain.
    /// `window` is `[left, bottom, right, top]`.
    ///
    /// # Errors
    ///
    /// [`CameraError::InvalidTransform`] for a nonfinite or singular
    /// transform; [`CameraError::InvalidAttribute`] for a nonfinite or empty
    /// window, a nonfinite or empty near/far interval, or a perspective near
    /// distance that is not positive. OpenUSD accepts these and computes
    /// unusable planes.
    #[doc(alias = "SetPositionAndRotationFromMatrix")]
    pub fn new(
        camera_to_world: &Matrix,
        window: [f64; 4],
        clipping_range: [f64; 2],
        projection: FrustumProjection,
    ) -> Result<Self, CameraError> {
        let [l, b, r, t] = window;
        if window.iter().any(|v| !v.is_finite()) || l >= r || b >= t {
            return Err(CameraError::InvalidAttribute("window"));
        }
        let [n, f] = clipping_range;
        if !n.is_finite()
            || !f.is_finite()
            || n >= f
            || (projection == FrustumProjection::Perspective && n <= 0.)
        {
            return Err(CameraError::InvalidAttribute("clippingRange"));
        }
        let view = conform(camera_to_world).ok_or(CameraError::InvalidTransform)?;
        let frustum = Self::from_view(view, window, clipping_range, projection);
        if frustum.planes.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CameraError::InvalidTransform);
        }
        Ok(frustum)
    }

    /// A symmetric perspective frustum, as `GfFrustum::SetPerspective` on a
    /// frustum placed by `camera_to_world`: the window's half extent along
    /// `axis` is `tan(field_of_view / 2)` at the reference depth 1, and the
    /// other half extent follows from the aspect ratio (zero taken as 1).
    ///
    /// # Errors
    ///
    /// As [`Self::new`], including a field of view that yields no window.
    #[doc(alias = "SetPerspective")]
    pub fn from_perspective(
        camera_to_world: &Matrix,
        axis: FieldOfViewAxis,
        perspective: Perspective,
    ) -> Result<Self, CameraError> {
        let aspect = if perspective.aspect_ratio == 0. {
            1.
        } else {
            perspective.aspect_ratio
        };
        let half = libm::tan((perspective.field_of_view / 2.).to_radians());
        let (x, y) = match axis {
            FieldOfViewAxis::Vertical => (half * aspect, half),
            FieldOfViewAxis::Horizontal => (half, half / aspect),
        };
        Self::new(
            camera_to_world,
            [-x, -y, x, y],
            [perspective.near, perspective.far],
            FrustumProjection::Perspective,
        )
    }

    /// The perspective parameters along `axis`, as
    /// `GfFrustum::GetPerspective`: the field of view spans the window's
    /// extent at depth 1, so an off-center window reports its full extent.
    /// `None` for an orthographic frustum.
    #[must_use]
    #[doc(alias = "GetPerspective")]
    pub fn perspective(&self, axis: FieldOfViewAxis) -> Option<Perspective> {
        if self.projection != FrustumProjection::Perspective {
            return None;
        }
        let [l, b, r, t] = self.window;
        let extent = match axis {
            FieldOfViewAxis::Vertical => t - b,
            FieldOfViewAxis::Horizontal => r - l,
        };
        Some(Perspective {
            field_of_view: 2. * libm::atan(extent / 2.).to_degrees(),
            aspect_ratio: (r - l) / (t - b),
            near: self.clipping_range[0],
            far: self.clipping_range[1],
        })
    }

    /// The projection model.
    #[must_use]
    pub fn projection(&self) -> FrustumProjection {
        self.projection
    }

    /// Conformed world-to-eye transform, as `GfFrustum::ComputeViewMatrix`.
    #[must_use]
    #[doc(alias = "ComputeViewMatrix")]
    pub fn view_matrix(&self) -> &Matrix {
        &self.view
    }

    /// Conformed eye-to-world transform, as `GfFrustum::ComputeViewInverse`.
    #[must_use]
    #[doc(alias = "ComputeViewInverse")]
    pub fn view_inverse(&self) -> &Matrix {
        &self.view_inverse
    }

    /// Eye-to-clip projection with OpenGL `[-1,1]` depth, as
    /// `GfFrustum::ComputeProjectionMatrix`. A narrowed frustum's matrix
    /// renders just its picking region, for GPU picking.
    #[must_use]
    #[doc(alias = "ComputeProjectionMatrix")]
    pub fn projection_matrix(&self) -> Matrix {
        projection_matrix(self.window, self.clipping_range, self.projection)
    }

    /// Reference window `[left, bottom, right, top]`.
    #[must_use]
    pub fn window(&self) -> [f64; 4] {
        self.window
    }

    /// Near/far eye-space distances in scene units.
    #[must_use]
    pub fn clipping_range(&self) -> [f64; 2] {
        self.clipping_range
    }

    /// Eight world-space corners, as `GfFrustum::ComputeCorners`: near then
    /// far, each lower-left, lower-right, upper-left, upper-right.
    #[must_use]
    #[doc(alias = "ComputeCorners")]
    pub fn corners(&self) -> [[f64; 3]; 8] {
        corners(
            &self.view_inverse,
            self.window,
            self.clipping_range,
            self.projection,
        )
    }

    /// Normalized inward world-space planes `[a,b,c,d]`: left, right,
    /// bottom, top, near, far. Nonnegative signed distance is inside.
    #[must_use]
    pub fn planes(&self) -> &[[f64; 4]; 6] {
        &self.planes
    }

    /// Whether a finite world point lies inside all six primary planes,
    /// boundary included, as `GfFrustum::Intersects(GfVec3d)`.
    #[must_use]
    #[doc(alias = "Intersects")]
    pub fn contains_world_point(&self, point: [f64; 3]) -> bool {
        point.iter().all(|v| v.is_finite()) && contains_planes(&self.planes, point)
    }

    /// Conservative intersection with an oriented world bound, as
    /// `GfFrustum::Intersects(GfBBox3d)`. Rejects a box wholly outside any
    /// primary plane; crossing boxes can be false positives. Empty bounds
    /// do not intersect. Uses the same transformed-corner test as
    /// [`super::ComputedCamera::intersects_world_bound`], without rebuilding
    /// the planes.
    #[must_use]
    #[doc(alias = "Intersects")]
    pub fn intersects_world_bound(&self, bound: &BoundingBox) -> bool {
        !bound.range.is_empty() && intersects_planes(&self.planes, bound)
    }

    /// `GfFrustum::_WindowNormalizedToPoint`.
    fn window_point(&self, window_point: [f64; 2]) -> [f64; 2] {
        let [l, b, r, t] = self.window;
        [
            l + 0.5 * (1. + window_point[0]) * (r - l),
            b + 0.5 * (1. + window_point[1]) * (t - b),
        ]
    }

    /// `GfFrustum::_ComputePickRayOffsetToNearPlane`.
    fn offset_pick_ray(&self, from: [f64; 3], direction: [f64; 3]) -> Ray {
        let near = self.clipping_range[0];
        let from = core::array::from_fn(|i| from[i] + near * direction[i]);
        Ray {
            origin: transform_point(from, &self.view_inverse),
            direction: transform_dir(direction, &self.view_inverse),
        }
    }

    /// The world-space pick ray through a normalized window point, as
    /// `GfFrustum::ComputePickRay(GfVec2d)`. Its direction is unit length.
    /// A perspective ray leaves the eye through the window point and starts
    /// `near` along it (on the sphere of radius `near`, not the near plane).
    /// An orthographic ray runs along the view direction from the window
    /// point and, as OpenUSD offsets it twice, starts at eye depth `2 * near`.
    #[must_use]
    #[doc(alias = "ComputePickRay")]
    pub fn pick_ray(&self, window_point: [f64; 2]) -> Ray {
        let [x, y] = self.window_point(window_point);
        match self.projection {
            FrustumProjection::Perspective => {
                self.offset_pick_ray([0.; 3], super::ray::normalized([x, y, -1.]))
            }
            FrustumProjection::Orthographic => {
                self.offset_pick_ray([x, y, -self.clipping_range[0]], [0., 0., -1.])
            }
        }
    }

    /// The world-space pick ray toward a world point, as
    /// `GfFrustum::ComputePickRay(GfVec3d)`, starting `near` along it from
    /// the eye (perspective) or at eye depth `near` (orthographic). Its
    /// direction is unit length, or zero for the eye position itself.
    #[must_use]
    #[doc(alias = "ComputePickRay")]
    pub fn pick_ray_through(&self, world_point: [f64; 3]) -> Ray {
        let eye = transform_point(world_point, &self.view);
        match self.projection {
            FrustumProjection::Perspective => {
                self.offset_pick_ray([0.; 3], super::ray::normalized(eye))
            }
            FrustumProjection::Orthographic => {
                self.offset_pick_ray([eye[0], eye[1], 0.], [0., 0., -1.])
            }
        }
    }

    /// A frustum narrowed to a picking region around a normalized window
    /// point, as `GfFrustum::ComputeNarrowedFrustum(GfVec2d, GfVec2d)`. The
    /// narrowed window spans `size` times this window's width and height,
    /// centered on the point and clipped to this window; placement, near/far
    /// and projection are unchanged. A zero `size` yields a zero-area window,
    /// whose projection matrix is not finite.
    #[must_use]
    #[doc(alias = "ComputeNarrowedFrustum")]
    pub fn narrowed(&self, window_point: [f64; 2], size: [f64; 2]) -> Self {
        self.narrowed_at(self.window_point(window_point), size)
    }

    /// A frustum narrowed to a picking region around the projection of a
    /// world point, as `GfFrustum::ComputeNarrowedFrustum(GfVec3d, GfVec2d)`,
    /// with `size` as in [`Self::narrowed`]. `None` when the point is at or
    /// behind the eye plane, where OpenUSD warns and returns the frustum
    /// unchanged.
    #[must_use]
    #[doc(alias = "ComputeNarrowedFrustum")]
    pub fn narrowed_around(&self, world_point: [f64; 3], size: [f64; 2]) -> Option<Self> {
        let eye = transform_point(world_point, &self.view);
        if eye[2].is_nan() || eye[2] >= 0. {
            return None;
        }
        let mut point = [eye[0], eye[1]];
        if self.projection == FrustumProjection::Perspective {
            point = point.map(|v| v / -eye[2]);
        }
        Some(self.narrowed_at(point, size))
    }

    /// `GfFrustum::_ComputeNarrowedFrustumSub`.
    fn narrowed_at(&self, point: [f64; 2], size: [f64; 2]) -> Self {
        let [l, b, r, t] = self.window;
        let half = [0.5 * size[0] * (r - l), 0.5 * size[1] * (t - b)];
        let mut window = [
            point[0] - half[0],
            point[1] - half[1],
            point[0] + half[0],
            point[1] + half[1],
        ];
        // OpenUSD clamps each bound separately, so a region wholly outside
        // the window can still invert.
        if window[0] < l {
            window[0] = l;
        }
        if window[1] < b {
            window[1] = b;
        }
        if window[2] > r {
            window[2] = r;
        }
        if window[3] > t {
            window[3] = t;
        }
        Self {
            planes: planes(&self.view, window, self.clipping_range, self.projection),
            window,
            ..*self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FieldOfViewAxis, FrustumProjection, Perspective, PreparedFrustum};
    use crate::gf;

    fn frustum(projection: FrustumProjection) -> PreparedFrustum {
        PreparedFrustum::new(&gf::IDENTITY, [-1., -1., 1., 1.], [1., 10.], projection).unwrap()
    }

    #[test]
    fn pick_rays_start_where_openusd_starts_them() {
        let perspective = frustum(FrustumProjection::Perspective);
        let ray = perspective.pick_ray([0., 0.]);
        assert_eq!((ray.origin, ray.direction), ([0., 0., -1.], [0., 0., -1.]));
        let through = perspective.pick_ray_through([0., 0., -4.]);
        assert_eq!(through.origin, [0., 0., -1.]);
        // OpenUSD offsets an orthographic window ray by `near` twice.
        let orthographic = frustum(FrustumProjection::Orthographic);
        let ray = orthographic.pick_ray([1., -1.]);
        assert_eq!((ray.origin, ray.direction), ([1., -1., -2.], [0., 0., -1.]));
        let through = orthographic.pick_ray_through([0.5, 0.5, -4.]);
        assert_eq!(through.origin, [0.5, 0.5, -1.]);
    }

    #[test]
    fn narrowed_windows_clamp_to_the_window_and_reject_points_behind_the_eye() {
        let frustum = frustum(FrustumProjection::Perspective);
        assert_eq!(
            frustum.narrowed([1., 0.], [0.5, 0.5]).window(),
            [0.5, -0.5, 1., 0.5]
        );
        let around = frustum.narrowed_around([1., 0., -2.], [0.1, 0.1]).unwrap();
        assert_eq!(around.window(), [0.4, -0.1, 0.6, 0.1]);
        assert!(frustum.narrowed_around([0., 0., 1.], [0.1, 0.1]).is_none());
        assert!(frustum.narrowed_around([0.; 3], [0.1, 0.1]).is_none());
    }

    #[test]
    fn perspective_parameters_round_trip() {
        let perspective = Perspective {
            field_of_view: 60.,
            aspect_ratio: 1.5,
            near: 0.5,
            far: 200.,
        };
        let frustum = PreparedFrustum::from_perspective(
            &gf::IDENTITY,
            FieldOfViewAxis::Vertical,
            perspective,
        )
        .unwrap();
        let back = frustum.perspective(FieldOfViewAxis::Vertical).unwrap();
        assert!((back.field_of_view - 60.).abs() < 1e-12);
        assert_eq!(back.aspect_ratio, 1.5);
        assert_eq!(frustum.clipping_range(), [0.5, 200.]);
        let orthographic = FrustumProjection::Orthographic;
        assert_eq!(
            PreparedFrustum::new(&gf::IDENTITY, [-1., -1., 1., 1.], [1., 2.], orthographic)
                .unwrap()
                .perspective(FieldOfViewAxis::Horizontal),
            None
        );
    }
}
