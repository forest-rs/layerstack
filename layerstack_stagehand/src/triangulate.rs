// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Faces to triangles: Hydra's fan, and ear clipping for concave faces.
//!
//! Triangles are kept as face-vertex (corner) indices, positions into
//! `faceVertexIndices`, so that every per-corner value (the point, a
//! face-varying primvar) is found through the same triangle.

use alloc::vec::Vec;

use exedra_triangulate::{PolygonInput, TriError, TriParams, triangulate_preserving_boundary};

/// How faces become triangles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Triangulation {
    /// Every face fanned from its first vertex, exactly as Hydra
    /// triangulates (`pxr/imaging/hd/meshUtil.cpp`,
    /// `HdMeshUtil::ComputeTriangleIndices`): the same triangles in the same
    /// order. A concave face fanned this way covers area outside the face;
    /// the [`crate::MeshReport`] still counts such faces.
    HydraFan,
    /// Convex faces fanned exactly as [`Self::HydraFan`]; concave faces,
    /// projected to their best-fit plane, ear-clipped by
    /// `exedra_triangulate` so the triangles cover the face and nothing
    /// else. A face the ear clipper refuses (self-intersecting, degenerate)
    /// is fanned and reported as a fallback. The default.
    #[default]
    Concave,
}

/// What became of one face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FaceOutcome {
    /// Fanned: a convex face, or any face under [`Triangulation::HydraFan`].
    Fanned {
        /// Whether the face is concave (fanned under `HydraFan`).
        concave: bool,
    },
    /// A concave face ear-clipped.
    Clipped,
    /// A concave face the ear clipper refused, fanned instead.
    FellBack(TriError),
}

/// Appends the triangles of the face whose corners are
/// `first..first + count` to `out`, as corner indices, wound as Hydra
/// winds them: counter-clockwise for a right-handed face, flipped for a
/// left-handed one (`flip`).
///
/// `corner_point` gives each corner's position.
pub(crate) fn triangulate_face(
    first: u32,
    count: u32,
    flip: bool,
    mode: Triangulation,
    corner_point: impl Fn(u32) -> [f32; 3],
    out: &mut Vec<[u32; 3]>,
    scratch: &mut Scratch,
) -> FaceOutcome {
    if count == 3 {
        fan(first, count, flip, out);
        return FaceOutcome::Fanned { concave: false };
    }
    scratch.polygon.clear();
    scratch
        .polygon
        .extend((first..first + count).map(|corner| corner_point(corner).map(f64::from)));
    let normal = newell_normal(&scratch.polygon);
    let concave = is_concave(&scratch.polygon, normal);
    if !concave || mode == Triangulation::HydraFan {
        fan(first, count, flip, out);
        return FaceOutcome::Fanned { concave };
    }
    match ear_clip(&scratch.polygon, normal, &mut scratch.projected) {
        Ok(triangles) => {
            out.extend(triangles.into_iter().map(|[a, b, c]| {
                // The ear clipper winds every triangle as the face winds
                // (counter-clockwise about its Newell normal); a
                // left-handed face is flipped, as the fan flips it.
                if flip {
                    [first + a, first + c, first + b]
                } else {
                    [first + a, first + b, first + c]
                }
            }));
            FaceOutcome::Clipped
        }
        Err(error) => {
            fan(first, count, flip, out);
            FaceOutcome::FellBack(error)
        }
    }
}

/// Reused buffers of [`triangulate_face`].
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    polygon: Vec<[f64; 3]>,
    projected: Vec<[f64; 2]>,
}

/// Hydra's fan of one face of `count >= 3` corners starting at `first`.
///
/// A port of `pxr/imaging/hd/meshUtil.cpp`, `_FanTriangulate` and the loop
/// of `HdMeshUtil::ComputeTriangleIndices`: triangle `j` is corners
/// `(0, j + 1, j + 2)`, or `(0, j + 2, j + 1)` when flipped; on a flipped
/// face of more than three corners the first triangle is rotated to
/// `(2, 1, 0)` and the last to `(nv - 2, 0, nv - 1)`, which keeps the edge
/// Hydra hides for wireframes in the same slot. The rotation changes no
/// triangle's winding.
pub(crate) fn fan(first: u32, count: u32, flip: bool, out: &mut Vec<[u32; 3]>) {
    for j in 0..count - 2 {
        let mut triangle = if flip {
            [first, first + j + 2, first + j + 1]
        } else {
            [first, first + j + 1, first + j + 2]
        };
        if count > 3 && flip {
            if j == 0 {
                triangle = [triangle[1], triangle[2], triangle[0]];
            } else if j == count - 3 {
                triangle = [triangle[2], triangle[0], triangle[1]];
            }
        }
        out.push(triangle);
    }
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

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The polygon's Newell normal: twice its vector area, pointing so that
/// the polygon winds counter-clockwise about it; the normal of its
/// best-fit plane for a non-planar polygon.
pub(crate) fn newell_normal(polygon: &[[f64; 3]]) -> [f64; 3] {
    let mut normal = [0.0; 3];
    for (i, &p) in polygon.iter().enumerate() {
        let q = polygon[(i + 1) % polygon.len()];
        normal[0] += (p[1] - q[1]) * (p[2] + q[2]);
        normal[1] += (p[2] - q[2]) * (p[0] + q[0]);
        normal[2] += (p[0] - q[0]) * (p[1] + q[1]);
    }
    normal
}

/// Whether the polygon turns clockwise about `normal` at any corner: a
/// reflex corner, which a fan from corner 0 may cover wrongly. Straight
/// and repeated corners are not reflex; a polygon with no area is not
/// concave (there is nothing to cover).
fn is_concave(polygon: &[[f64; 3]], normal: [f64; 3]) -> bool {
    let scale = dot(normal, normal);
    if scale == 0.0 || !scale.is_finite() {
        return false;
    }
    let n = polygon.len();
    (0..n).any(|i| {
        let previous = polygon[(i + n - 1) % n];
        let here = polygon[i];
        let next = polygon[(i + 1) % n];
        let turn = dot(cross(sub(here, previous), sub(next, here)), normal);
        // Relative to the corner's edges and the face's size, so that
        // rounding on a straight corner does not read as a turn.
        let edges = libm::sqrt(
            dot(sub(here, previous), sub(here, previous)) * dot(sub(next, here), sub(next, here)),
        );
        turn < -1e-9 * edges * libm::sqrt(scale)
    })
}

/// Ear-clips the polygon in the plane through it perpendicular to
/// `normal`, in a basis where it winds counter-clockwise, keeping every
/// corner (`triangulate_preserving_boundary`), so a straight corner still
/// joins the triangles beside it and leaves no crack against a neighbor.
/// Triangles are corner offsets within the face.
fn ear_clip(
    polygon: &[[f64; 3]],
    normal: [f64; 3],
    projected: &mut Vec<[f64; 2]>,
) -> Result<Vec<[u32; 3]>, TriError> {
    let length = libm::sqrt(dot(normal, normal));
    let n = normal.map(|c| c / length);
    // Any unit vector perpendicular to `n`, then `v = n × u`, so `u × v = n`
    // and the polygon, counter-clockwise about `n`, is counter-clockwise in
    // `(u, v)`.
    let helper = if n[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = cross(helper, n);
    let u_length = libm::sqrt(dot(u, u));
    let u = u.map(|c| c / u_length);
    let v = cross(n, u);
    // Centered on the first corner, to keep the coordinates small.
    let origin = polygon[0];
    projected.clear();
    projected.extend(polygon.iter().map(|&p| {
        let d = sub(p, origin);
        [dot(d, u), dot(d, v)]
    }));
    let input = PolygonInput {
        outer: projected,
        holes: &[],
    };
    triangulate_preserving_boundary(&input, &TriParams::ear_clip()).map(|result| result.triangles)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    fn fanned(count: u32, flip: bool) -> Vec<[u32; 3]> {
        let mut out = Vec::new();
        fan(10, count, flip, &mut out);
        out
    }

    #[test]
    fn a_right_handed_fan_is_hydras() {
        assert_eq!(fanned(3, false), vec![[10, 11, 12]]);
        assert_eq!(
            fanned(5, false),
            vec![[10, 11, 12], [10, 12, 13], [10, 13, 14]]
        );
    }

    #[test]
    fn a_left_handed_fan_is_hydras_with_its_rotations() {
        // A triangle: flipped, not rotated.
        assert_eq!(fanned(3, true), vec![[10, 12, 11]]);
        // A quad: (0, 2, 1) rotated to (2, 1, 0); (0, 3, 2) to (2, 0, 3).
        assert_eq!(fanned(4, true), vec![[12, 11, 10], [12, 10, 13]]);
        // A pentagon: the middle triangle is only flipped.
        assert_eq!(
            fanned(5, true),
            vec![[12, 11, 10], [10, 13, 12], [13, 10, 14]]
        );
    }

    #[test]
    fn concavity_is_found_in_any_plane() {
        // An L in the plane x = 1, wound about +x.
        let l: Vec<[f64; 3]> = [
            [0.0, 0.0],
            [2.0, 0.0],
            [2.0, 1.0],
            [1.0, 1.0],
            [1.0, 2.0],
            [0.0, 2.0],
        ]
        .iter()
        .map(|&[a, b]| [1.0, a, b])
        .collect();
        assert!(is_concave(&l, newell_normal(&l)));
        let square = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        assert!(!is_concave(&square, newell_normal(&square)));
        // A straight corner is not a reflex one.
        let straight = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        assert!(!is_concave(&straight, newell_normal(&straight)));
    }
}
