// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Points (`UsdGeomPoints`) as point sprites, or as instanced spheres and
//! discs.

#![expect(
    clippy::cast_possible_truncation,
    reason = "counts are u32 and positions f32, as a renderer takes them"
)]

use alloc::vec::Vec;

use crate::input::{Interpolation, Primvar};
use crate::plan::{DrawRange, MeshBuffers};
use crate::report::{ChannelState, DropReason};
use crate::shapes::{Shape, Tessellation, tessellate};

/// Everything a point set's buffers are filled from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointsInput<'a> {
    /// `points`.
    pub points: &'a [[f32; 3]],
    /// `widths` (diameters), `vertex` or `varying` (one per point) or
    /// `constant`; without, every width is 1.
    pub widths: Option<Primvar<'a, f32>>,
    /// `normals`: with them each point is a disc facing its normal,
    /// without a sphere (`UsdGeomPoints`).
    pub normals: Option<Primvar<'a, [f32; 3]>>,
    /// `primvars:displayColor`.
    pub colors: Option<Primvar<'a, [f32; 3]>>,
    /// `primvars:displayOpacity`.
    pub opacities: Option<Primvar<'a, f32>>,
    /// `ids`: each point's stable identity over time, one per point, or
    /// empty.
    pub ids: &'a [i64],
}

/// What each point is drawn as, when drawn as a shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointShape {
    /// A sphere of diameter `width`: no normals authored.
    Spheres,
    /// A disc of diameter `width` facing the point's normal.
    Discs,
}

/// What a point set's buffers were filled from.
#[derive(Clone, Debug, PartialEq)]
pub struct PointsReport {
    /// Points.
    pub points: u32,
    /// What each point is ([`PointShape`]).
    pub shape: PointShape,
    /// `widths`.
    pub widths: ChannelState,
    /// `normals`.
    pub normals: ChannelState,
    /// `displayColor`.
    pub colors: ChannelState,
    /// `displayOpacity`.
    pub opacities: ChannelState,
    /// `ids`: used (`vertex`) when one per point, else dropped.
    pub ids: ChannelState,
    /// `UsdGeomPoints::ComputeExtent` of the points and widths: `None`
    /// where it fails (widths given, but not one per point).
    pub extent: Option<[[f32; 3]; 2]>,
}

/// A point set's buffers: one entry per point.
#[derive(Clone, Debug, PartialEq)]
pub struct PointBuffers {
    /// Each point's position.
    pub positions: Vec<[f32; 3]>,
    /// Each point's width, a diameter (1 where `widths` is not drawn).
    pub widths: Vec<f32>,
    /// Each point's unit normal, for [`PointShape::Discs`].
    pub normals: Option<Vec<[f32; 3]>>,
    /// Each point's display color, when drawn.
    pub colors: Option<Vec<[f32; 3]>>,
    /// Each point's display opacity, when drawn.
    pub opacities: Option<Vec<f32>>,
    /// Each point's id, when `ids` is drawn; empty otherwise.
    pub ids: Vec<i64>,
    /// What was made.
    pub report: PointsReport,
}

/// Fills a point set's buffers. Every primvar is per point: `constant`
/// (and `uniform`, one for the one prim) values repeat, `vertex` and
/// `varying` values are taken one per point; a primvar of another size or
/// interpolation is dropped, and its state says why.
#[must_use]
pub fn fill_points(input: &PointsInput<'_>) -> PointBuffers {
    let n = input.points.len();
    let mut report = PointsReport {
        points: n as u32,
        shape: PointShape::Spheres,
        widths: ChannelState::Absent,
        normals: ChannelState::Absent,
        colors: ChannelState::Absent,
        opacities: ChannelState::Absent,
        ids: ChannelState::Absent,
        extent: None,
    };
    let widths = per_point(input.widths.as_ref(), n, &mut report.widths);
    let normals = per_point(input.normals.as_ref(), n, &mut report.normals).map(|normals| {
        normals
            .into_iter()
            .map(|v| {
                let length = libm::sqrtf(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]);
                if length > 0.0 {
                    v.map(|c| c / length)
                } else {
                    [0.0, 0.0, 1.0]
                }
            })
            .collect::<Vec<_>>()
    });
    if normals.is_some() {
        report.shape = PointShape::Discs;
    }
    let colors = per_point(input.colors.as_ref(), n, &mut report.colors);
    let opacities = per_point(input.opacities.as_ref(), n, &mut report.opacities);
    let ids = if input.ids.is_empty() {
        Vec::new()
    } else if input.ids.len() == n {
        report.ids = ChannelState::Used {
            interpolation: Interpolation::Vertex,
            indexed: false,
        };
        input.ids.to_vec()
    } else {
        report.ids = ChannelState::Dropped(DropReason::TooFew {
            expected: n as u64,
            found: input.ids.len() as u64,
        });
        Vec::new()
    };
    report.extent = points_extent(input.points, input.widths.map(|widths| widths.values));
    PointBuffers {
        positions: input.points.to_vec(),
        widths: widths.unwrap_or_else(|| alloc::vec![1.0; n]),
        normals,
        colors,
        opacities,
        ids,
        report,
    }
}

/// A primvar's values, one per point.
fn per_point<T: Copy>(
    primvar: Option<&Primvar<'_, T>>,
    n: usize,
    state: &mut ChannelState,
) -> Option<Vec<T>> {
    let primvar = primvar?;
    if primvar.element_size != 1 {
        *state = ChannelState::Dropped(DropReason::ElementSize(primvar.element_size));
        return None;
    }
    let mut values = Vec::new();
    match primvar.indices {
        Some(indices) => {
            for &index in indices {
                let Some(value) = usize::try_from(index)
                    .ok()
                    .and_then(|i| primvar.values.get(i))
                else {
                    *state = ChannelState::Dropped(DropReason::Index(index));
                    return None;
                };
                values.push(*value);
            }
        }
        None => values.extend_from_slice(primvar.values),
    }
    let per_point = match primvar.interpolation {
        Interpolation::Constant | Interpolation::Uniform => false,
        Interpolation::Vertex | Interpolation::Varying => true,
        Interpolation::FaceVarying => {
            *state = ChannelState::Dropped(DropReason::Interpolation(primvar.interpolation));
            return None;
        }
    };
    let expected = if per_point { n } else { 1 };
    if values.len() < expected {
        *state = ChannelState::Dropped(DropReason::TooFew {
            expected: expected as u64,
            found: values.len() as u64,
        });
        return None;
    }
    *state = ChannelState::Used {
        interpolation: primvar.interpolation,
        indexed: primvar.indices.is_some(),
    };
    if per_point {
        values.truncate(n);
        Some(values)
    } else {
        Some(alloc::vec![values[0]; n])
    }
}

/// `UsdGeomPoints::ComputeExtent`: the union of a sphere of diameter
/// `widths[i]` at each point. `None` when widths are given but not one per
/// point, as C++ fails then; without widths, the points' bounds (the
/// `UsdGeomPointBased` extent the extent plugin falls back to). `None` for
/// no points. OpenUSD: `pxr/usd/usdGeom/points.cpp`.
#[must_use]
pub fn points_extent(points: &[[f32; 3]], widths: Option<&[f32]>) -> Option<[[f32; 3]; 2]> {
    if points.is_empty() {
        return None;
    }
    if widths.is_some_and(|widths| widths.len() != points.len()) {
        return None;
    }
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for (i, point) in points.iter().enumerate() {
        let half = widths.map_or(0.0, |widths| f64::from(widths[i] * 0.5));
        for k in 0..3 {
            min[k] = min[k].min(f64::from(point[k]) - half);
            max[k] = max[k].max(f64::from(point[k]) + half);
        }
    }
    Some([min.map(|v| v as f32), max.map(|v| v as f32)])
}

/// The unit shape a point is instanced as, of diameter 1 at the origin,
/// for a renderer to place at each point scaled by its width: a sphere
/// tessellated as an implicit `Sphere` of radius 0.5 is, or a disc of
/// `tessellation.segments` segments in the XY plane facing +Z (turned to
/// each point's normal).
#[must_use]
pub fn point_shape(shape: PointShape, tessellation: &Tessellation) -> MeshBuffers {
    match shape {
        PointShape::Spheres => tessellate(&Shape::Sphere { radius: 0.5 }, tessellation),
        PointShape::Discs => {
            let segments = tessellation.segments.max(3);
            let mut positions = alloc::vec![[0.0_f32; 3]];
            for k in 0..segments {
                let angle = core::f64::consts::TAU * f64::from(k) / f64::from(segments);
                let (sin, cos) = (libm::sin(angle), libm::cos(angle));
                positions.push([(0.5 * cos) as f32, (0.5 * sin) as f32, 0.0]);
            }
            let mut indices = Vec::with_capacity(segments as usize * 3);
            for k in 0..segments {
                indices.extend_from_slice(&[0, 1 + k, 1 + (k + 1) % segments]);
            }
            let count = indices.len() as u32;
            MeshBuffers {
                normals: alloc::vec![[0.0, 0.0, 1.0]; positions.len()],
                positions,
                triangle_faces: alloc::vec![0; segments as usize],
                indices,
                ranges: alloc::vec![DrawRange {
                    subset: None,
                    first: 0,
                    count,
                }],
                ..MeshBuffers::default()
            }
        }
    }
}
