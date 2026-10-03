// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{LodError, LodExtent, LodMatrix, empty, point, validate_transform};
use crate::gf;
use alloc::vec::Vec;

/// Metric approximation selected by a screen-size heuristic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionMethod {
    /// Area of the projected diagonal-derived bounding sphere, as OpenUSD.
    Sphere,
    /// Area of the projected box's near-plane-clipped convex hull.
    Extent,
}
/// Primary frustum projection model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LodProjection {
    /// Window measured on the reference plane at eye depth one.
    Perspective,
    /// Window measured in scene units, independent of depth.
    Orthographic,
}
/// Owned primary viewing frustum for pure LOD evaluation.
///
/// Eye coordinates look down negative Z. Camera scale/shear must be conformed by
/// the caller before construction. Additional clipping planes and render aspect
/// conforming are caller responsibilities. AOUSD Core §6.3; `GfFrustum`.
#[derive(Clone, Debug, PartialEq)]
pub struct LodFrustum {
    /// Perspective or orthographic projection.
    pub projection: LodProjection,
    /// Rigid row-vector world-to-eye transform.
    pub view_matrix: LodMatrix,
    /// Reference window `[left,bottom,right,top]`.
    pub window: [f64; 4],
    /// Positive near/far distances along negative eye Z.
    pub clipping_range: [f64; 2],
}
impl LodFrustum {
    /// Checks a rigid finite view, positive window area and valid clipping range.
    pub fn validate(&self) -> Result<(), LodError> {
        validate_transform(&self.view_matrix)?;
        if self.window.iter().any(|v| !v.is_finite())
            || self.window[0] >= self.window[2]
            || self.window[1] >= self.window[3]
            || self.clipping_range.iter().any(|v| !v.is_finite())
            || self.clipping_range[0] >= self.clipping_range[1]
            || (self.projection == LodProjection::Perspective && self.clipping_range[0] <= 0.)
        {
            return Err(LodError::InvalidFrustum);
        }
        for i in 0..3 {
            for j in 0..3 {
                let dot: f64 = (0..3)
                    .map(|k| self.view_matrix[i][k] * self.view_matrix[j][k])
                    .sum();
                if (dot - if i == j { 1. } else { 0. }).abs() > 1e-8 {
                    return Err(LodError::InvalidFrustum);
                }
            }
        }
        Ok(())
    }
    fn projection_matrix(&self) -> LodMatrix {
        let [l, b, r, t] = self.window;
        let [n, f] = self.clipping_range;
        let mut m = gf::IDENTITY;
        m[0][0] = 2. / (r - l);
        m[1][1] = 2. / (t - b);
        if self.projection == LodProjection::Perspective {
            m[2][0] = (r + l) / (r - l);
            m[2][1] = (t + b) / (t - b);
            m[2][2] = -(f + n) / (f - n);
            m[2][3] = -1.;
            m[3][2] = -2. * n * f / (f - n);
            m[3][3] = 0.;
        } else {
            m[2][2] = -2. / (f - n);
            m[3][0] = -(r + l) / (r - l);
            m[3][1] = -(t + b) / (t - b);
            m[3][2] = -(f + n) / (f - n);
        }
        m
    }
    fn eye_planes(&self) -> [[f64; 4]; 6] {
        let [l, b, r, t] = self.window;
        let [n, f] = self.clipping_range;
        let planes = if self.projection == LodProjection::Perspective {
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
        planes.map(|p| {
            let norm = libm::sqrt(p[0] * p[0] + p[1] * p[1] + p[2] * p[2]);
            p.map(|v| v / norm)
        })
    }
}
#[cfg(feature = "usd-geom")]
impl From<&crate::camera::ComputedCamera> for LodFrustum {
    fn from(camera: &crate::camera::ComputedCamera) -> Self {
        Self {
            projection: if *camera.projection() == crate::usd_geom::CameraProjection::Perspective {
                LodProjection::Perspective
            } else {
                LodProjection::Orthographic
            },
            view_matrix: *camera.view_matrix(),
            window: camera.window(),
            clipping_range: camera.clipping_range(),
        }
    }
}
fn corner(extent: &LodExtent, i: usize) -> [f64; 3] {
    core::array::from_fn(|j| extent[usize::from(i & (1 << j) != 0)][j])
}
fn signed(p: [f64; 3], plane: &[f64; 4]) -> f64 {
    (0..3).map(|i| p[i] * plane[i]).sum::<f64>() + plane[3]
}

pub(super) fn size(
    extent: LodExtent,
    method: ProjectionMethod,
    frustum: &LodFrustum,
    transform: &LodMatrix,
) -> Result<f64, LodError> {
    if empty(&extent) {
        return Ok(0.);
    }
    let view_transform = gf::mul(transform, &frustum.view_matrix);
    let planes = frustum.eye_planes();
    if method == ProjectionMethod::Sphere {
        // OpenUSD usdLod/screenSizeHeuristicQuery.cpp: diagonal-derived radius,
        // deliberately approximate under shear; Euclidean distance, not eye Z.
        let min = point(extent[0], transform);
        let max = point(extent[1], transform);
        let radius = libm::sqrt(
            (0..3)
                .map(|i| {
                    let delta = max[i] - min[i];
                    delta * delta
                })
                .sum(),
        ) / 2.;
        let center = point(
            core::array::from_fn(|i| (extent[0][i] + extent[1][i]) / 2.),
            &view_transform,
        );
        if planes.iter().any(|p| signed(center, p) < -radius) {
            return Ok(0.);
        }
        let radius = if frustum.projection == LodProjection::Perspective {
            let distance = libm::sqrt(center.iter().map(|v| v * v).sum());
            if distance == 0. {
                return Ok(f64::MAX);
            }
            radius / distance
        } else {
            radius
        };
        let area = core::f64::consts::PI * radius * radius;
        return Ok(area
            / ((frustum.window[2] - frustum.window[0]) * (frustum.window[3] - frustum.window[1])));
    }
    // GfFrustum::Intersects(GfBBox3d) rejects boxes wholly outside any plane.
    let corners: [[f64; 3]; 8] =
        core::array::from_fn(|i| point(corner(&extent, i), &view_transform));
    if planes
        .iter()
        .any(|p| corners.iter().all(|&v| signed(v, p) < 0.))
    {
        return Ok(0.);
    }
    let clip_transform = gf::mul(&view_transform, &frustum.projection_matrix());
    let clips: [[f64; 4]; 8] = core::array::from_fn(|i| {
        let p = corner(&extent, i);
        core::array::from_fn(|j| {
            p[0] * clip_transform[0][j]
                + p[1] * clip_transform[1][j]
                + p[2] * clip_transform[2][j]
                + clip_transform[3][j]
        })
    });
    let edges = [
        (0, 1),
        (2, 3),
        (4, 5),
        (6, 7),
        (0, 2),
        (1, 3),
        (4, 6),
        (5, 7),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    let mut points = Vec::with_capacity(24);
    for (a, b) in edges {
        let (p0, p1) = (clips[a], clips[b]);
        let (d0, d1) = (p0[2] + p0[3], p1[2] + p1[3]);
        if d0 < 0. && d1 < 0. {
            continue;
        }
        let (mut lo, mut hi) = (0., 1.);
        if d0 < 0. || d1 < 0. {
            let t = d0 / (d0 - d1);
            if d0 < 0. {
                lo = t;
            } else {
                hi = t;
            }
        }
        for t in [lo, hi] {
            // Match GfLerp's (1-t)*a + t*b evaluation order.
            let p: [f64; 4] = core::array::from_fn(|j| (1. - t) * p0[j] + t * p1[j]);
            if p[3] == 0. {
                return Err(LodError::InvalidProjection);
            }
            let xy = [p[0] / p[3], p[1] / p[3]];
            if xy.iter().any(|v| !v.is_finite()) {
                return Err(LodError::InvalidProjection);
            }
            points.push(xy);
        }
    }
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    points.dedup_by(|a, b| (a[0] - b[0]).abs() <= 1e-6 && (a[1] - b[1]).abs() <= 1e-6);
    if points.len() < 3 {
        return Ok(0.);
    }
    let mut hull: Vec<[f64; 2]> = Vec::with_capacity(points.len() + 1);
    for &p in &points {
        push_hull(&mut hull, p, 2);
    }
    let upper_min = hull.len() + 1;
    for &p in points[..points.len() - 1].iter().rev() {
        push_hull(&mut hull, p, upper_min);
    }
    Ok(hull
        .windows(2)
        .map(|p| p[0][0] * p[1][1] - p[0][1] * p[1][0])
        .sum::<f64>()
        / 8.)
}
fn push_hull(hull: &mut Vec<[f64; 2]>, p: [f64; 2], min: usize) {
    while hull.len() >= min {
        let a = hull[hull.len() - 2];
        let b = hull[hull.len() - 1];
        if (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]) > 0. {
            break;
        }
        hull.pop();
    }
    hull.push(p);
}
