// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Viewing frusta prepared for repeated culling, as `GfFrustum`.
//!
//! OpenUSD `pxr/base/gf/frustum.cpp`. A frustum is a conformed (rigid,
//! right-handed) camera placement, a reference window, a near/far interval and
//! a projection. Perspective windows lie on the eye-space plane at depth 1
//! (`GfFrustum::GetReferencePlaneDepth`); orthographic windows are in scene
//! units.
use crate::{bounds::BoundingBox, gf};

type Matrix = [[f64; 4]; 4];

/// How a frustum projects eye space onto its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(alias = "GfFrustum::ProjectionType")]
pub(super) enum FrustumProjection {
    /// Rays diverge from the eye through the window at depth 1.
    Perspective,
    /// Rays run parallel to `-Z` through the window.
    Orthographic,
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

/// Immutable primary-frustum planes prepared for repeated world-space queries.
///
/// Owns its six normalized planes without scene borrows or allocations. Prepare
/// from a [`super::ComputedCamera`] with
/// [`super::ComputedCamera::prepare_frustum`], then reuse across points and
/// bounds. It does not observe camera, time or scene changes; prepare again
/// from the recomputed camera after such changes. Additional authored clipping
/// planes are excluded, as in the one-shot camera helpers.
#[derive(Clone, Debug)]
#[doc(alias = "GfFrustum")]
pub struct PreparedFrustum {
    pub(super) planes: [[f64; 4]; 6],
}

impl PreparedFrustum {
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
}
