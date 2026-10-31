// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Curves (`UsdGeomBasisCurves`, `UsdGeomNurbsCurves`,
//! `UsdGeomHermiteCurves`) as line segments or swept triangles.
//!
//! This module holds OpenUSD's semantics: which controls form each
//! segment (`vstep`, wrap, pinned ends), the data sizes of each primvar
//! interpolation, and how per-curve, per-vertex and per-joint data follows
//! the curve. The geometry (basis evaluation, de Boor, frames, tubes and
//! ribbons) is `exedra_curves`'.

#![expect(
    clippy::cast_possible_truncation,
    reason = "buffer indices and counts are u32, as a renderer takes them, and samples f32"
)]

use alloc::string::String;
use alloc::vec::Vec;

use exedra_curves::{
    CubicBasis, Frame, Nurbs, NurbsError, NurbsSamples, Span, Station, SweptMesh, Workspace,
};

use crate::input::{Interpolation, Primvar};
use crate::plan::{DrawRange, MeshBuffers};
use crate::report::{ChannelState, DropReason};

/// `UsdGeomBasisCurves`' `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CurveType {
    /// Straight segments between consecutive vertices.
    Linear,
    /// Cubic segments of four vertices, weighted by the basis.
    Cubic,
}

/// `UsdGeomBasisCurves`' `basis` (unused for linear curves).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CurveBasis {
    /// Bezier, `vstep` 3.
    Bezier,
    /// Uniform cubic B-spline, `vstep` 1.
    BSpline,
    /// Catmull-Rom, `vstep` 1.
    CatmullRom,
}

/// `UsdGeomBasisCurves`' `wrap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CurveWrap {
    /// Open curves.
    Nonperiodic,
    /// Closed curves: the first vertices repeat to close each one.
    Periodic,
    /// Open B-spline and Catmull-Rom curves that begin and end at their
    /// first and last vertices; others are drawn as nonperiodic.
    Pinned,
}

impl CurveType {
    /// Its token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Cubic => "cubic",
        }
    }

    /// The type a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "linear" => Self::Linear,
            "cubic" => Self::Cubic,
            _ => return None,
        })
    }
}

impl CurveBasis {
    /// Its token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bezier => "bezier",
            Self::BSpline => "bspline",
            Self::CatmullRom => "catmullRom",
        }
    }

    /// The basis a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "bezier" => Self::Bezier,
            "bspline" => Self::BSpline,
            "catmullRom" => Self::CatmullRom,
            _ => return None,
        })
    }

    /// OpenUSD's `vstep`: 3 for Bezier, 1 otherwise.
    #[must_use]
    pub fn vstep(self) -> i32 {
        match self {
            Self::Bezier => 3,
            Self::BSpline | Self::CatmullRom => 1,
        }
    }

    fn cubic(self) -> CubicBasis {
        match self {
            Self::Bezier => CubicBasis::Bezier,
            Self::BSpline => CubicBasis::BSpline,
            Self::CatmullRom => CubicBasis::CatmullRom,
        }
    }
}

impl CurveWrap {
    /// Its token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nonperiodic => "nonperiodic",
            Self::Periodic => "periodic",
            Self::Pinned => "pinned",
        }
    }

    /// The wrap a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "nonperiodic" => Self::Nonperiodic,
            "periodic" => Self::Periodic,
            "pinned" => Self::Pinned,
            _ => return None,
        })
    }
}

/// Which curve schema the input is, with its own attributes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveForm<'a> {
    /// `BasisCurves`: its `type`, `basis` and `wrap` tokens as authored
    /// (or their fallbacks `cubic`, `bezier`, `nonperiodic`). A token the
    /// schema does not allow draws the curves as linear, nonperiodic for an
    /// unknown `wrap`, and the report names it ([`CurveFallback`]).
    Basis {
        /// `type`.
        curve_type: &'a str,
        /// `basis`.
        basis: &'a str,
        /// `wrap`.
        wrap: &'a str,
    },
    /// `NurbsCurves`: each curve's `order`, the curves' `knots` one after
    /// another (`count + order` each), each curve's parameter `ranges`
    /// (empty: each curve's whole domain) and `pointWeights` (empty: all 1).
    Nurbs {
        /// `order`, one per curve.
        order: &'a [i32],
        /// `knots`.
        knots: &'a [f64],
        /// `ranges`, one per curve, or empty.
        ranges: &'a [[f64; 2]],
        /// `pointWeights`, one per point, or empty.
        weights: &'a [f64],
    },
    /// `HermiteCurves`: one tangent per point.
    Hermite {
        /// `tangents`.
        tangents: &'a [[f32; 3]],
    },
}

impl Default for CurveForm<'_> {
    fn default() -> Self {
        Self::Basis {
            curve_type: "cubic",
            basis: "bezier",
            wrap: "nonperiodic",
        }
    }
}

/// Everything a curves plan reads.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CurvesInput<'a> {
    /// The schema and its own attributes.
    pub form: CurveForm<'a>,
    /// `curveVertexCounts`.
    pub vertex_counts: &'a [i32],
    /// `points`.
    pub points: &'a [[f32; 3]],
    /// `widths`, with its interpolation (`UsdGeomCurves::GetWidthsInterpolation`,
    /// `vertex` when unauthored); without, every width is 1.
    pub widths: Option<Primvar<'a, f32>>,
    /// `normals` (`primvars:normals`, else the attribute): with them the
    /// curves are swept as ribbons facing the normals, without as tubes
    /// (`UsdGeomBasisCurves`, "Tubes and Ribbons").
    pub normals: Option<Primvar<'a, [f32; 3]>>,
    /// `primvars:displayColor`.
    pub colors: Option<Primvar<'a, [f32; 3]>>,
    /// `primvars:displayOpacity`.
    pub opacities: Option<Primvar<'a, f32>>,
}

/// How a curves plan draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CurveStyle {
    /// Line segments through the samples, with a width per sample for a
    /// renderer that draws wide lines.
    Lines,
    /// Triangles: ribbons where normals are authored, tubes of
    /// [`CurveOptions::sides`] sides otherwise, of radius `width / 2`.
    /// The line segments are kept too.
    Swept,
}

/// The explicit sampling of a curves plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CurveOptions {
    /// Lines or swept triangles. Default [`CurveStyle::Lines`].
    pub style: CurveStyle,
    /// Samples per cubic segment and per NURBS knot span: a segment is
    /// sampled at `u = k / samples` for `k` in `0..samples`, and each curve
    /// at its end. Linear curves are drawn through their vertices. Default
    /// 8; 0 is taken as 1.
    pub samples: u32,
    /// Sides of a tube (at least 3). Default 8.
    pub sides: u32,
}

impl Default for CurveOptions {
    fn default() -> Self {
        Self {
            style: CurveStyle::Lines,
            samples: 8,
            sides: 8,
        }
    }
}

/// How the curves were evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveEvaluation {
    /// `BasisCurves`, with the type, basis and wrap used (after any
    /// [`CurveFallback`]).
    Basis {
        /// The type.
        curve_type: CurveType,
        /// The basis; `None` for linear curves.
        basis: Option<CurveBasis>,
        /// The wrap.
        wrap: CurveWrap,
    },
    /// `NurbsCurves`, by de Boor's algorithm.
    Nurbs,
    /// `HermiteCurves`, by the cubic Hermite basis.
    Hermite,
}

/// A `BasisCurves` token the schema does not allow, and what was drawn
/// instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurveFallback {
    /// `type`, `basis` or `wrap`.
    pub attribute: &'static str,
    /// The token authored.
    pub token: String,
    /// The token drawn instead: `linear` for a type or basis,
    /// `nonperiodic` for a wrap.
    pub used: &'static str,
}

/// What the curves were swept as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveGeometry {
    /// Line segments only ([`CurveStyle::Lines`]).
    Lines,
    /// Tubes of this many sides.
    Tubes {
        /// Sides per ring.
        sides: u32,
    },
    /// Ribbons facing the authored normals.
    Ribbons,
}

/// A curve the plan drew differently from its authored vertices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveNote {
    /// The curve, by its position in `curveVertexCounts`.
    pub curve: u32,
    /// What happened.
    pub kind: CurveNoteKind,
}

/// What happened to one curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveNoteKind {
    /// Too few vertices to draw anything: fewer than 2 (1 for a linear
    /// periodic curve); not drawn.
    Skipped {
        /// Its vertex count.
        count: i32,
    },
    /// Fewer vertices than one full segment needs (4 for an open cubic,
    /// 3 for a periodic one): drawn as one segment with the last vertex
    /// repeated, as Hydra does (`HdSt_BasisCurvesIndexBuilderComputation`,
    /// `pxr/imaging/hdSt/basisCurvesComputations.cpp`).
    Repeated {
        /// Its vertex count.
        count: i32,
    },
    /// Vertices past the last whole Bezier segment: `(count - 4) % 3` (or
    /// `count % 3` when periodic) vertices are not drawn.
    Unused {
        /// The vertices left over.
        unused: i32,
    },
    /// A NURBS curve that cannot be evaluated; not drawn.
    Nurbs(NurbsError),
}

/// What makes a curves input undrawable as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveProblem {
    /// A curve has a negative vertex count.
    NegativeCount {
        /// The curve.
        curve: u32,
        /// Its count.
        count: i32,
    },
    /// The vertex counts do not sum to the point count.
    PointCount {
        /// The sum of the vertex counts.
        expected: u64,
        /// The points.
        found: u64,
    },
    /// The NURBS `order` or `knots` are not one per curve, or the
    /// Hermite `tangents` not one per point.
    Arrays {
        /// The attribute: `order`, `knots` or `tangents`.
        attribute: &'static str,
    },
}

/// What a curves plan made of its input, so a caller can check the buffers
/// without reading them.
#[derive(Clone, Debug, PartialEq)]
pub struct CurvesReport {
    /// How the curves were evaluated.
    pub evaluation: CurveEvaluation,
    /// Tokens drawn as something else.
    pub fallbacks: Vec<CurveFallback>,
    /// Curves authored.
    pub curves: u32,
    /// Curves drawn.
    pub drawn_curves: u32,
    /// Segments drawn per curve (knot spans for NURBS), 0 for one not drawn.
    pub segments: Vec<u32>,
    /// Samples per segment ([`CurveOptions::samples`]); 1 for linear curves.
    pub samples: u32,
    /// Vertices in the buffers: the samples of every drawn curve, a closed
    /// curve's first repeated at its end.
    pub vertices: u32,
    /// Line segments.
    pub lines: u32,
    /// What the curves were swept as.
    pub geometry: CurveGeometry,
    /// Triangles swept.
    pub triangles: u32,
    /// `widths`.
    pub widths: ChannelState,
    /// `normals`.
    pub normals: ChannelState,
    /// `displayColor`.
    pub colors: ChannelState,
    /// `displayOpacity`.
    pub opacities: ChannelState,
    /// Curves drawn otherwise than authored.
    pub notes: Vec<CurveNote>,
    /// Swept stations where the sweep met a repeated point, a cusp, a zero
    /// width, or a ribbon normal along the curve
    /// (`exedra_curves::FrameReport`, `SweepReport`).
    pub degenerate_stations: u32,
    /// Samples whose width was negative or not finite, drawn as 0: widths
    /// interpolated along a Catmull-Rom chain can undershoot, and a pinned
    /// end's reflected width can fall below zero.
    pub clamped_widths: u32,
    /// Why nothing is drawn, when nothing is.
    pub problem: Option<CurveProblem>,
    /// `UsdGeomCurves::ComputeExtent` of the points and widths
    /// ([`curves_extent`]); `None` for no points.
    pub extent: Option<[[f32; 3]; 2]>,
}

/// Curves' buffers: samples drawn as line segments, and swept triangles.
#[derive(Clone, Debug, PartialEq)]
pub struct CurveBuffers {
    /// Each sample's position.
    pub positions: Vec<[f32; 3]>,
    /// Each sample's width (1 where `widths` is not drawn).
    pub widths: Vec<f32>,
    /// Each sample's normal, when normals are drawn.
    pub normals: Option<Vec<[f32; 3]>>,
    /// Each sample's display color, when drawn.
    pub colors: Option<Vec<[f32; 3]>>,
    /// Each sample's display opacity, when drawn.
    pub opacities: Option<Vec<f32>>,
    /// Two sample indices per line segment.
    pub lines: Vec<u32>,
    /// Each drawn curve's samples: `[first, count]`.
    pub curves: Vec<[u32; 2]>,
    /// The swept triangles, with [`CurveStyle::Swept`]: positions,
    /// normals, colors (one per ring vertex, from its sample), indices,
    /// each triangle's curve in `triangle_faces`, one range.
    pub swept: Option<MeshBuffers>,
    /// What was made.
    pub report: CurvesReport,
}

/// The data sizes of each interpolation of a `BasisCurves` primvar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveDataSizes {
    /// `uniform`: one per curve.
    pub uniform: u64,
    /// `varying`: one per segment joint.
    pub varying: u64,
    /// `vertex`: one per vertex.
    pub vertex: u64,
}

/// `UsdGeomBasisCurves::ComputeSegmentCounts`: each curve's segment count
/// by the schema's table ("Segment Indexing", `basisCurves.h`), with C++'s
/// truncating integer division, so counts below the basis minimum give
/// what C++ gives (bspline `[2]` is `-1`); `None` when there is no count
/// (an unknown token). Pinned is nonperiodic for Bezier and linear curves,
/// `count - 1` for B-spline and Catmull-Rom.
/// OpenUSD: `pxr/usd/usdGeom/basisCurves.cpp`.
#[must_use]
pub fn basis_segment_counts(
    curve_type: CurveType,
    basis: CurveBasis,
    wrap: CurveWrap,
    counts: &[i32],
) -> Vec<i32> {
    counts
        .iter()
        .map(|&n| match (curve_type, wrap) {
            (CurveType::Linear, CurveWrap::Periodic) => n,
            (CurveType::Linear, _) => n - 1,
            (CurveType::Cubic, CurveWrap::Periodic) => n / basis.vstep(),
            (CurveType::Cubic, CurveWrap::Pinned) if basis != CurveBasis::Bezier => n - 1,
            (CurveType::Cubic, _) => (n - 4) / basis.vstep() + 1,
        })
        .collect()
}

/// `UsdGeomBasisCurves::Compute{Uniform,Varying,Vertex}DataSize`: a linear
/// curve has `count` varying values, one more when periodic; a periodic
/// cubic curve `count / vstep`; any other cubic curve, pinned included,
/// `max(count - 4, 0) / vstep + 2`. C++ computes the last in unsigned
/// arithmetic, so below 4 vertices its `max` does not clamp and the size
/// wraps around (B035); this is the size its comment intends, 2.
/// `None` for a negative count.
/// OpenUSD: `_ComputeVaryingDataSize`, `pxr/usd/usdGeom/basisCurves.cpp`.
#[must_use]
pub fn basis_data_sizes(
    curve_type: CurveType,
    basis: CurveBasis,
    wrap: CurveWrap,
    counts: &[i32],
) -> Option<CurveDataSizes> {
    let mut sizes = CurveDataSizes {
        uniform: counts.len() as u64,
        varying: 0,
        vertex: 0,
    };
    for &n in counts {
        let count = u64::try_from(n).ok()?;
        sizes.vertex += count;
        sizes.varying += match (curve_type, wrap) {
            (CurveType::Linear, CurveWrap::Periodic) => count + 1,
            (CurveType::Linear, _) => count,
            (CurveType::Cubic, CurveWrap::Periodic) => count / basis.vstep() as u64,
            (CurveType::Cubic, _) => count.saturating_sub(4) / basis.vstep() as u64 + 2,
        };
    }
    Some(sizes)
}

/// `UsdGeomBasisCurves::ComputeInterpolationForSize`: the first of
/// constant (1), uniform, varying and vertex whose size is `size`.
#[must_use]
pub fn interpolation_for_size(sizes: CurveDataSizes, size: u64) -> Option<Interpolation> {
    [
        (Interpolation::Constant, 1),
        (Interpolation::Uniform, sizes.uniform),
        (Interpolation::Varying, sizes.varying),
        (Interpolation::Vertex, sizes.vertex),
    ]
    .into_iter()
    .find_map(|(interpolation, n)| (n == size).then_some(interpolation))
}

/// `UsdGeomCurves::ComputeExtent`: the points' bounds grown by half the
/// largest width, whatever its interpolation (C++ bounds every basis by
/// its control hull, Catmull-Rom and Hermite included, and NURBS curves
/// ignoring their weights), in `float` as C++ computes it. `None` for no
/// points. OpenUSD: `pxr/usd/usdGeom/curves.cpp`.
#[must_use]
pub fn curves_extent(points: &[[f32; 3]], widths: &[f32]) -> Option<[[f32; 3]; 2]> {
    let first = *points.first()?;
    let (mut min, mut max) = (first, first);
    for point in points {
        for k in 0..3 {
            min[k] = min[k].min(point[k]);
            max[k] = max[k].max(point[k]);
        }
    }
    let half = widths
        .iter()
        .copied()
        .fold(None, |m: Option<f32>, w| Some(m.map_or(w, |m| m.max(w))));
    let half = half.unwrap_or(0.0) * 0.5;
    Some([min.map(|v| v - half), max.map(|v| v + half)])
}

/// One drawn curve's layout.
#[derive(Clone, Debug, PartialEq)]
struct Layout {
    /// The curve.
    curve: u32,
    /// Its first vertex.
    first: u32,
    /// Its vertex count.
    count: u32,
    /// The cubic segments over [`Self::controls`]; empty for linear.
    spans: Vec<Span>,
    /// The vertex each control is (pinned phantoms reflect `[0]` over
    /// `[1]`, or `[count-1]` over `[count-2]`: `Phantom`).
    controls: Vec<Control>,
    /// The joints each segment's varying values run between, as offsets
    /// into the curve's varying values.
    joints: Vec<[u32; 2]>,
    /// The sampled stations; for linear curves, one per vertex.
    stations: Vec<Station>,
    /// Whether the curve closes (its first sample repeated at its end).
    closed: bool,
    /// NURBS: the samples that start a new piece after a jump
    /// (`exedra_curves::NurbsSamples::breaks`). No line segment or sweep
    /// crosses one.
    breaks: Vec<u32>,
    /// Its first varying value.
    varying_first: u32,
}

/// A cubic control: a vertex of the curve, or a pinned end's phantom
/// `2 * a - b` (`UsdGeomBasisCurves`, pinned curves).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Vertex(u32),
    Phantom(u32, u32),
}

/// What a plan keeps of its input's topology.
#[derive(Clone, Debug, PartialEq)]
struct Topology {
    form: FormKey,
    vertex_counts: Vec<i32>,
    points: usize,
}

#[derive(Clone, Debug, PartialEq)]
enum FormKey {
    Basis(String, String, String),
    Nurbs(Vec<i32>, Vec<u64>, Vec<[u64; 2]>, Vec<u64>),
    Hermite,
}

impl FormKey {
    fn of(form: &CurveForm<'_>) -> Self {
        match *form {
            CurveForm::Basis {
                curve_type,
                basis,
                wrap,
            } => Self::Basis(curve_type.into(), basis.into(), wrap.into()),
            CurveForm::Nurbs {
                order,
                knots,
                ranges,
                weights,
            } => Self::Nurbs(
                order.to_vec(),
                knots.iter().map(|k| k.to_bits()).collect(),
                ranges.iter().map(|r| r.map(f64::to_bits)).collect(),
                weights.iter().map(|w| w.to_bits()).collect(),
            ),
            CurveForm::Hermite { .. } => Self::Hermite,
        }
    }
}

/// A curves plan: each curve's segments and stations, worked out once
/// per topology (`curveVertexCounts`, `type`, `basis`, `wrap`, or a NURBS
/// curve's `order`, `knots`, `ranges` and `pointWeights`) and kept, so new points, widths,
/// normals or colors fill the buffers again ([`Self::buffers`]) without
/// laying the curves out again.
///
/// # Segments
///
/// A `BasisCurves` curve of `n` vertices is laid out as Hydra lays it out
/// (`_BuildCubicIndexArray`, `pxr/imaging/hdSt/basisCurvesComputations.cpp`),
/// except at pinned ends:
///
/// - **linear**: through its vertices, closing back to the first when
///   periodic;
/// - **cubic nonperiodic** (and pinned Bezier): `max(n - 4, 0) / vstep + 1`
///   segments, segment `i` on vertices `i * vstep + 0..4`, a vertex past the
///   last repeating the last;
/// - **cubic periodic**: `max(n / vstep, 1)` segments on vertices
///   `(i * vstep + 0..4) % n`;
/// - **pinned B-spline and Catmull-Rom**: the schema's phantom points
///   `P[-1] = 2 P[0] - P[1]` and `P[n] = 2 P[n-1] - P[n-2]` are added and
///   the `n + 2` controls laid out as nonperiodic, `n - 1` segments, which
///   is `ComputeSegmentCounts`' count; the curve begins at `P[0]` and ends
///   at `P[n-1]`. (Storm repeats the end vertices instead, which also
///   reaches them but with other end tangents.)
///
/// `NurbsCurves` are sampled per nonempty knot span of their range;
/// `HermiteCurves` have `n - 1` segments, from each point to the next.
///
/// # Primvars
///
/// Widths, normals, colors and opacities are read at their interpolation,
/// sized as [`basis_data_sizes`] (for NURBS and Hermite curves `varying` is
/// one per vertex, as `vertex` is):
///
/// - `constant`: one value; `uniform`: one per curve;
/// - `vertex`: one per vertex, weighted by the curve's basis at each sample
///   as the points are (a pinned phantom reflects the end values); linear
///   and Hermite curves interpolate them linearly;
/// - `varying`: one per segment joint, interpolated linearly along each
///   segment (`UsdGeomBasisCurves`, "Primvar Interpolation"); a pinned
///   curve's `n - 2` values run along its interior segments and hold at its
///   two end segments. NURBS curves draw only `constant` and `uniform`
///   values.
///
/// A primvar of another size or interpolation is dropped, and its
/// [`ChannelState`] says why.
#[derive(Clone, Debug)]
pub struct CurvesPlan {
    options: CurveOptions,
    topology: Topology,
    layouts: Vec<Layout>,
    base: CurvesReport,
    cubic: Option<CubicBasis>,
    pinned: bool,
}

/// Why a plan cannot fill buffers from an input: its topology is not the
/// plan's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurvesMismatch;

impl CurvesPlan {
    /// Lays the input's curves out.
    #[must_use]
    pub fn new(input: &CurvesInput<'_>, options: &CurveOptions) -> Self {
        let options = CurveOptions {
            samples: options.samples.max(1),
            ..*options
        };
        let curves = u32::try_from(input.vertex_counts.len()).unwrap_or(u32::MAX);
        let mut base = CurvesReport {
            evaluation: CurveEvaluation::Hermite,
            fallbacks: Vec::new(),
            curves,
            drawn_curves: 0,
            segments: alloc::vec![0; curves as usize],
            samples: options.samples,
            vertices: 0,
            lines: 0,
            geometry: CurveGeometry::Lines,
            triangles: 0,
            widths: ChannelState::Absent,
            normals: ChannelState::Absent,
            colors: ChannelState::Absent,
            opacities: ChannelState::Absent,
            notes: Vec::new(),
            degenerate_stations: 0,
            clamped_widths: 0,
            problem: None,
            extent: None,
        };
        let topology = Topology {
            form: FormKey::of(&input.form),
            vertex_counts: input.vertex_counts.to_vec(),
            points: input.points.len(),
        };
        let mut plan = Self {
            options,
            topology,
            layouts: Vec::new(),
            base,
            cubic: None,
            pinned: false,
        };
        plan.base.evaluation = plan.resolve(&input.form);
        base = core::mem::replace(&mut plan.base, base_placeholder());
        if let Some(problem) = problem(input) {
            base.problem = Some(problem);
            plan.base = base;
            return plan;
        }
        let mut first = 0_u32;
        let mut varying_first = 0_u32;
        let mut knot_first = 0_usize;
        for (curve, &count) in input.vertex_counts.iter().enumerate() {
            let curve = curve as u32;
            let n = count as u32;
            let layout = match (base.evaluation, input.form) {
                (
                    CurveEvaluation::Basis {
                        curve_type,
                        basis,
                        wrap,
                    },
                    _,
                ) => basis_layout(
                    curve_type,
                    basis,
                    wrap,
                    count,
                    &options,
                    &mut base.notes,
                    curve,
                ),
                (
                    CurveEvaluation::Nurbs,
                    CurveForm::Nurbs {
                        order,
                        knots,
                        ranges,
                        weights,
                    },
                ) => {
                    let order_n = order[curve as usize].max(0) as u32;
                    let knot_count = (n + order_n) as usize;
                    let these = knots
                        .get(knot_first..knot_first + knot_count)
                        .unwrap_or(&[]);
                    knot_first += knot_count;
                    let points: Vec<[f64; 3]> = input.points[first as usize..(first + n) as usize]
                        .iter()
                        .map(|p| p.map(f64::from))
                        .collect();
                    let weights = (!weights.is_empty())
                        .then(|| weights.get(first as usize..(first + n) as usize))
                        .flatten();
                    let curve_def = Nurbs {
                        order: order_n,
                        knots: these,
                        points: &points,
                        weights,
                    };
                    let range = ranges.get(curve as usize).copied();
                    match curve_def
                        .validate()
                        .and_then(|valid| valid.sample(range, options.samples))
                    {
                        Ok(samples) => Some(Partial {
                            spans: Vec::new(),
                            controls: Vec::new(),
                            joints: Vec::new(),
                            stations: Vec::new(),
                            closed: false,
                            breaks: samples.breaks.iter().map(|&b| b as u32).collect(),
                            segments: samples.spans,
                        }),
                        Err(error) => {
                            base.notes.push(CurveNote {
                                curve,
                                kind: CurveNoteKind::Nurbs(error),
                            });
                            None
                        }
                    }
                }
                _ => hermite_layout(count, &options, &mut base.notes, curve),
            };
            if let Some(partial) = layout {
                base.segments[curve as usize] = partial.segments;
                base.drawn_curves += 1;
                plan.layouts.push(Layout {
                    curve,
                    first,
                    count: n,
                    spans: partial.spans,
                    controls: partial.controls,
                    joints: partial.joints,
                    stations: partial.stations,
                    closed: partial.closed,
                    breaks: partial.breaks,
                    varying_first,
                });
            }
            first += n;
            varying_first += varying_count(base.evaluation, count);
        }
        plan.base = base;
        plan
    }

    /// Resolves the form's tokens, noting fallbacks.
    fn resolve(&mut self, form: &CurveForm<'_>) -> CurveEvaluation {
        match *form {
            CurveForm::Basis {
                curve_type,
                basis,
                wrap,
            } => {
                let mut fallback = |attribute, token: &str, used| {
                    self.base.fallbacks.push(CurveFallback {
                        attribute,
                        token: token.into(),
                        used,
                    });
                };
                let mut resolved_type = CurveType::from_token(curve_type).unwrap_or_else(|| {
                    fallback("type", curve_type, "linear");
                    CurveType::Linear
                });
                let resolved_basis = match resolved_type {
                    CurveType::Linear => None,
                    CurveType::Cubic => CurveBasis::from_token(basis).or_else(|| {
                        fallback("basis", basis, "linear");
                        resolved_type = CurveType::Linear;
                        None
                    }),
                };
                let resolved_wrap = CurveWrap::from_token(wrap).unwrap_or_else(|| {
                    fallback("wrap", wrap, "nonperiodic");
                    CurveWrap::Nonperiodic
                });
                self.cubic = resolved_basis.map(CurveBasis::cubic);
                self.pinned = resolved_wrap == CurveWrap::Pinned;
                CurveEvaluation::Basis {
                    curve_type: resolved_type,
                    basis: resolved_basis,
                    wrap: resolved_wrap,
                }
            }
            CurveForm::Nurbs { .. } => CurveEvaluation::Nurbs,
            CurveForm::Hermite { .. } => {
                self.cubic = Some(CubicBasis::Hermite);
                CurveEvaluation::Hermite
            }
        }
    }

    /// What the plan made of its input's topology; [`CurveBuffers::report`]
    /// adds the primvars and counts of one fill.
    #[must_use]
    pub fn report(&self) -> &CurvesReport {
        &self.base
    }

    /// Whether `input` has this plan's topology, so [`Self::buffers`]
    /// fills from it.
    #[must_use]
    pub fn same_topology(&self, input: &CurvesInput<'_>) -> bool {
        self.topology.form == FormKey::of(&input.form)
            && self.topology.vertex_counts == input.vertex_counts
            && self.topology.points == input.points.len()
    }

    /// The buffers for `input`: its points, tangents, weights and primvars
    /// sampled at the plan's stations, and swept with
    /// [`CurveStyle::Swept`].
    ///
    /// # Errors
    ///
    /// [`CurvesMismatch`] when the input's topology is not the plan's.
    pub fn buffers(&self, input: &CurvesInput<'_>) -> Result<CurveBuffers, CurvesMismatch> {
        if !self.same_topology(input) {
            return Err(CurvesMismatch);
        }
        let mut report = self.base.clone();
        report.extent = curves_extent(input.points, input.widths.map_or(&[], |w| w.values));
        let mut out = CurveBuffers {
            positions: Vec::new(),
            widths: Vec::new(),
            normals: None,
            colors: None,
            opacities: None,
            lines: Vec::new(),
            curves: Vec::new(),
            swept: None,
            report: self.base.clone(),
        };
        if report.problem.is_some() {
            out.report = report;
            return Ok(out);
        }
        let sizes = self.sizes(input.vertex_counts);
        let widths = channel(
            input.widths.as_ref(),
            sizes,
            &mut report.widths,
            self.base.evaluation,
        );
        let normals = channel(
            input.normals.as_ref(),
            sizes,
            &mut report.normals,
            self.base.evaluation,
        );
        let colors = channel(
            input.colors.as_ref(),
            sizes,
            &mut report.colors,
            self.base.evaluation,
        );
        let opacities = channel(
            input.opacities.as_ref(),
            sizes,
            &mut report.opacities,
            self.base.evaluation,
        );
        let mut normal_out = normals.as_ref().map(|_| Vec::new());
        let mut color_out = colors.as_ref().map(|_| Vec::new());
        let mut opacity_out = opacities.as_ref().map(|_| Vec::new());
        let tangents = match input.form {
            CurveForm::Hermite { tangents } => tangents,
            _ => &[],
        };
        let nurbs = match input.form {
            CurveForm::Nurbs {
                order,
                knots,
                ranges,
                weights,
            } => Some((order, knots, ranges, weights)),
            _ => None,
        };
        let mut swept = (self.options.style == CurveStyle::Swept).then(MeshBuffers::default);
        let mut knot_first = 0_usize;
        let mut knot_cursor = 0_usize;
        // Buffers reused from curve to curve, so a curve allocates only
        // what it adds to the output.
        let mut scratch = Scratch::default();
        for layout in &self.layouts {
            let start = out.positions.len() as u32;
            let points: &[[f64; 3]] = match nurbs {
                Some((order, knots, ranges, weights)) => {
                    // Skip the knots of curves before this one.
                    while knot_cursor < layout.curve as usize {
                        knot_first +=
                            (input.vertex_counts[knot_cursor] + order[knot_cursor]) as usize;
                        knot_cursor += 1;
                    }
                    let n = layout.count as usize;
                    let o = order[layout.curve as usize] as usize;
                    let first = layout.first as usize;
                    scratch.controls.clear();
                    scratch.controls.extend(
                        input.points[first..first + n]
                            .iter()
                            .map(|p| p.map(f64::from)),
                    );
                    let curve = Nurbs {
                        order: o as u32,
                        knots: &knots[knot_first..knot_first + n + o],
                        points: &scratch.controls,
                        weights: (!weights.is_empty()).then(|| &weights[first..first + n]),
                    };
                    // The samples' own points: at a break the parameter
                    // repeats, and evaluating it again would give only the
                    // right side of the jump. The plan validated these knots
                    // and weights (both are its topology); the points are
                    // this refill's, so a control that is not finite fails
                    // here, and the curve is noted and left undrawn.
                    let range = ranges.get(layout.curve as usize).copied();
                    let sampled = curve.validate().and_then(|valid| {
                        valid.sample_into(
                            range,
                            self.options.samples,
                            &mut scratch.sweep.workspace,
                            &mut scratch.samples,
                        )
                    });
                    if let Err(error) = sampled {
                        report.drawn_curves -= 1;
                        report.notes.push(CurveNote {
                            curve: layout.curve,
                            kind: CurveNoteKind::Nurbs(error),
                        });
                        continue;
                    }
                    &scratch.samples.points
                }
                _ => {
                    self.sample_vertex(
                        layout,
                        input.points,
                        tangents,
                        &mut scratch.controls,
                        &mut scratch.points,
                    );
                    &scratch.points
                }
            };
            let station_count = points.len();
            let value_at = |channel: &Channel<'_>, k: usize, value: &dyn Fn(usize) -> [f64; 4]| {
                self.sample_channel(layout, channel, k, station_count, value)
            };
            let mut width_values: Vec<f64> = match &widths {
                Some(channel) => (0..station_count)
                    .map(|k| {
                        value_at(channel, k, &|i| {
                            [f64::from(float_value(channel, i)), 0.0, 0.0, 0.0]
                        })[0]
                    })
                    .collect(),
                None => alloc::vec![1.0; station_count],
            };
            for width in &mut width_values {
                // A width is a diameter, so one below zero (a Catmull-Rom
                // chain undershoots, a pinned end reflects) or not finite is
                // drawn as 0, which a sweep accepts (`SweepError::Size`).
                let clamped = width.max(0.0);
                let clamped = if clamped.is_finite() { clamped } else { 0.0 };
                if clamped != *width {
                    report.clamped_widths += 1;
                    *width = clamped;
                }
            }
            let normal_values: Option<Vec<[f64; 3]>> = normals.as_ref().map(|channel| {
                (0..station_count)
                    .map(|k| {
                        let v = value_at(channel, k, &|i| vec3(channel, i));
                        [v[0], v[1], v[2]]
                    })
                    .collect()
            });
            let color_values: Option<Vec<[f64; 3]>> = colors.as_ref().map(|channel| {
                (0..station_count)
                    .map(|k| {
                        let v = value_at(channel, k, &|i| vec3(channel, i));
                        [v[0], v[1], v[2]]
                    })
                    .collect()
            });
            if let (Some(out), Some(opacity)) = (opacity_out.as_mut(), opacities.as_ref()) {
                for k in 0..station_count {
                    out.push(
                        value_at(opacity, k, &|i| {
                            [f64::from(float_value(opacity, i)), 0.0, 0.0, 0.0]
                        })[0] as f32,
                    );
                }
            }
            out.positions.extend(points.iter().map(|p| p.map(narrow)));
            out.widths.extend(width_values.iter().map(|&w| narrow(w)));
            if let (Some(out), Some(values)) = (normal_out.as_mut(), normal_values.as_ref()) {
                out.extend(values.iter().map(|v| v.map(narrow)));
            }
            if let (Some(out), Some(values)) = (color_out.as_mut(), color_values.as_ref()) {
                out.extend(values.iter().map(|v| v.map(narrow)));
            }
            let count = station_count as u32;
            for k in 1..count {
                if !layout.breaks.contains(&k) {
                    out.lines.extend_from_slice(&[start + k - 1, start + k]);
                }
            }
            out.curves.push([start, count]);
            if let Some(mesh) = swept.as_mut() {
                let geometry = sweep_curve(
                    mesh,
                    points,
                    layout.closed,
                    &layout.breaks,
                    &width_values,
                    normal_values.as_deref(),
                    color_values.as_deref(),
                    self.options.sides,
                    layout.curve,
                    &mut scratch.sweep,
                );
                report.geometry = geometry.0;
                report.degenerate_stations += geometry.1;
            }
        }
        if let Some(mesh) = swept.as_mut() {
            let count = mesh.indices.len() as u32;
            mesh.ranges.push(DrawRange {
                subset: None,
                first: 0,
                count,
            });
            report.triangles = count / 3;
            if self.layouts.is_empty() {
                report.geometry = if normals.is_some() {
                    CurveGeometry::Ribbons
                } else {
                    CurveGeometry::Tubes {
                        sides: self.options.sides.max(3),
                    }
                };
            }
        }
        report.vertices = out.positions.len() as u32;
        report.lines = (out.lines.len() / 2) as u32;
        out.normals = normal_out;
        out.colors = color_out;
        out.opacities = opacity_out;
        out.swept = swept;
        out.report = report;
        Ok(out)
    }

    fn sizes(&self, counts: &[i32]) -> CurveDataSizes {
        match self.base.evaluation {
            CurveEvaluation::Basis {
                curve_type,
                basis,
                wrap,
            } => basis_data_sizes(
                curve_type,
                basis.unwrap_or(CurveBasis::Bezier),
                wrap,
                counts,
            )
            .unwrap_or(CurveDataSizes {
                uniform: 0,
                varying: 0,
                vertex: 0,
            }),
            _ => {
                let vertex = counts.iter().map(|&n| n.max(0) as u64).sum();
                CurveDataSizes {
                    uniform: counts.len() as u64,
                    varying: vertex,
                    vertex,
                }
            }
        }
    }

    /// A curve's points at its stations.
    fn sample_vertex(
        &self,
        layout: &Layout,
        points: &[[f32; 3]],
        tangents: &[[f32; 3]],
        controls: &mut Vec<[f64; 3]>,
        out: &mut Vec<[f64; 3]>,
    ) {
        let vertex = |i: u32| points[(layout.first + i) as usize].map(f64::from);
        out.clear();
        controls.clear();
        let basis = match self.cubic {
            None => {
                out.extend((0..layout.count).map(vertex));
                if layout.closed {
                    out.push(vertex(0));
                }
                return;
            }
            Some(CubicBasis::Hermite) => {
                let tangent = |i: u32| tangents[(layout.first + i) as usize].map(f64::from);
                controls.extend((0..layout.count).flat_map(|i| [vertex(i), tangent(i)]));
                CubicBasis::Hermite
            }
            Some(basis) => {
                controls.extend(layout.controls.iter().map(|&c| resolve(c, &vertex)));
                basis
            }
        };
        if exedra_curves::sample_into(basis, controls, &layout.spans, &layout.stations, out)
            .is_err()
        {
            out.clear();
        }
    }

    /// A primvar's value at station `k` of a curve.
    fn sample_channel(
        &self,
        layout: &Layout,
        channel: &Channel<'_>,
        k: usize,
        stations: usize,
        value: &dyn Fn(usize) -> [f64; 4],
    ) -> [f64; 4] {
        match channel.interpolation {
            Interpolation::Constant => value(0),
            Interpolation::Uniform => value(layout.curve as usize),
            Interpolation::Vertex | Interpolation::Varying if self.cubic.is_none() => {
                // Linear curves: one value per vertex; a closed curve's last
                // station repeats its first, or (varying, which C++ sizes one
                // more per periodic curve) takes the closing value.
                let i = if layout.closed && k + 1 == stations {
                    if channel.interpolation == Interpolation::Varying {
                        return value((layout.varying_first + layout.count) as usize);
                    }
                    0
                } else {
                    k as u32
                };
                let base = if channel.interpolation == Interpolation::Varying {
                    layout.varying_first
                } else {
                    layout.first
                };
                value((base + i) as usize)
            }
            Interpolation::Vertex if self.cubic != Some(CubicBasis::Hermite) => {
                let station = layout.stations[k];
                let span = layout.spans[station.span as usize];
                let controls = span.map(|c| {
                    resolve4(layout.controls[c as usize], &|i| {
                        value((layout.first + i) as usize)
                    })
                });
                self.cubic
                    .unwrap_or(CubicBasis::Bezier)
                    .evaluate(controls, station.u)
            }
            Interpolation::Vertex => {
                // Hermite: linear between a segment's two points.
                let station = layout.stations[k];
                let a = value((layout.first + station.span) as usize);
                let b = value((layout.first + station.span + 1) as usize);
                lerp4(a, b, station.u)
            }
            Interpolation::Varying => {
                let station = layout.stations[k];
                if self.cubic == Some(CubicBasis::Hermite) {
                    let a = value((layout.first + station.span) as usize);
                    let b = value((layout.first + station.span + 1) as usize);
                    return lerp4(a, b, station.u);
                }
                let [a, b] = layout.joints[station.span as usize];
                let a = value((layout.varying_first + a) as usize);
                let b = value((layout.varying_first + b) as usize);
                lerp4(a, b, station.u)
            }
            Interpolation::FaceVarying => value(0),
        }
    }
}

fn base_placeholder() -> CurvesReport {
    CurvesReport {
        evaluation: CurveEvaluation::Hermite,
        fallbacks: Vec::new(),
        curves: 0,
        drawn_curves: 0,
        segments: Vec::new(),
        samples: 0,
        vertices: 0,
        lines: 0,
        geometry: CurveGeometry::Lines,
        triangles: 0,
        widths: ChannelState::Absent,
        normals: ChannelState::Absent,
        colors: ChannelState::Absent,
        opacities: ChannelState::Absent,
        notes: Vec::new(),
        degenerate_stations: 0,
        clamped_widths: 0,
        problem: None,
        extent: None,
    }
}

/// A layout before it is placed.
struct Partial {
    spans: Vec<Span>,
    controls: Vec<Control>,
    joints: Vec<[u32; 2]>,
    stations: Vec<Station>,
    closed: bool,
    breaks: Vec<u32>,
    segments: u32,
}

/// Lays out one `BasisCurves` curve of `count` vertices.
fn basis_layout(
    curve_type: CurveType,
    basis: Option<CurveBasis>,
    wrap: CurveWrap,
    count: i32,
    options: &CurveOptions,
    notes: &mut Vec<CurveNote>,
    curve: u32,
) -> Option<Partial> {
    let periodic = wrap == CurveWrap::Periodic;
    let Some(basis) = basis.filter(|_| curve_type == CurveType::Cubic) else {
        // Linear: through the vertices.
        if count < 2 && !(periodic && count >= 1) || count < 1 {
            notes.push(CurveNote {
                curve,
                kind: CurveNoteKind::Skipped { count },
            });
            return None;
        }
        let segments = if periodic { count } else { count - 1 } as u32;
        return Some(Partial {
            spans: Vec::new(),
            controls: Vec::new(),
            joints: Vec::new(),
            stations: Vec::new(),
            closed: periodic,
            breaks: Vec::new(),
            segments,
        });
    };
    if count < 2 {
        notes.push(CurveNote {
            curve,
            kind: CurveNoteKind::Skipped { count },
        });
        return None;
    }
    let n = count as u32;
    let step = basis.vstep() as u32;
    let pinned = wrap == CurveWrap::Pinned && basis != CurveBasis::Bezier;
    let mut spans = Vec::new();
    let mut joints = Vec::new();
    let mut controls: Vec<Control> = (0..n).map(Control::Vertex).collect();
    if pinned {
        // P[-1] and P[n]: the controls are P[-1], P[0..n], P[n].
        controls.insert(0, Control::Phantom(0, 1));
        controls.push(Control::Phantom(n - 1, n - 2));
        let segments = n - 1;
        // C++ sizes a pinned curve's varying data as a nonperiodic one's:
        // `max(n - 4, 0) + 2` joints along the interior segments.
        let varying = n.saturating_sub(4) + 2;
        for k in 0..segments {
            spans.push([k, k + 1, k + 2, k + 3]);
            let a = k.saturating_sub(1).min(varying - 1);
            let b = k.min(varying - 1);
            joints.push([a, b]);
        }
    } else if periodic {
        let segments = (n / step).max(1);
        if n < 3 {
            notes.push(CurveNote {
                curve,
                kind: CurveNoteKind::Repeated { count },
            });
        }
        if !n.is_multiple_of(step) {
            notes.push(CurveNote {
                curve,
                kind: CurveNoteKind::Unused {
                    unused: (n % step) as i32,
                },
            });
        }
        for i in 0..segments {
            spans.push(core::array::from_fn(|v| (i * step + v as u32) % n));
            joints.push([i, (i + 1) % segments]);
        }
    } else {
        let segments = n.saturating_sub(4) / step + 1;
        if n < 4 {
            notes.push(CurveNote {
                curve,
                kind: CurveNoteKind::Repeated { count },
            });
        } else if !(n - 4).is_multiple_of(step) {
            notes.push(CurveNote {
                curve,
                kind: CurveNoteKind::Unused {
                    unused: ((n - 4) % step) as i32,
                },
            });
        }
        for i in 0..segments {
            spans.push(core::array::from_fn(|v| (i * step + v as u32).min(n - 1)));
            joints.push([i, i + 1]);
        }
    }
    let segments = spans.len() as u32;
    Some(Partial {
        stations: exedra_curves::stations(segments, options.samples, false),
        spans,
        controls,
        joints,
        closed: periodic,
        breaks: Vec::new(),
        segments,
    })
}

/// Lays out one `HermiteCurves` curve: a segment from each point to the
/// next, over interleaved point and tangent controls.
fn hermite_layout(
    count: i32,
    options: &CurveOptions,
    notes: &mut Vec<CurveNote>,
    curve: u32,
) -> Option<Partial> {
    if count < 2 {
        notes.push(CurveNote {
            curve,
            kind: CurveNoteKind::Skipped { count },
        });
        return None;
    }
    let segments = count as u32 - 1;
    Some(Partial {
        spans: (0..segments)
            .map(|j| [2 * j, 2 * j + 1, 2 * j + 2, 2 * j + 3])
            .collect(),
        controls: Vec::new(),
        joints: Vec::new(),
        stations: exedra_curves::stations(segments, options.samples, false),
        closed: false,
        breaks: Vec::new(),
        segments,
    })
}

/// The varying values one curve takes.
fn varying_count(evaluation: CurveEvaluation, count: i32) -> u32 {
    let count = count.max(0) as u32;
    match evaluation {
        CurveEvaluation::Basis {
            curve_type,
            basis,
            wrap,
        } => {
            let step = basis.map_or(1, |b| b.vstep() as u32);
            match (curve_type, wrap) {
                (CurveType::Linear, CurveWrap::Periodic) => count + 1,
                (CurveType::Linear, _) => count,
                (CurveType::Cubic, CurveWrap::Periodic) => count / step,
                (CurveType::Cubic, _) => count.saturating_sub(4) / step + 2,
            }
        }
        _ => count,
    }
}

fn problem(input: &CurvesInput<'_>) -> Option<CurveProblem> {
    let mut sum = 0_u64;
    for (curve, &count) in input.vertex_counts.iter().enumerate() {
        let Ok(count) = u64::try_from(count) else {
            return Some(CurveProblem::NegativeCount {
                curve: curve as u32,
                count,
            });
        };
        sum += count;
    }
    if sum != input.points.len() as u64 {
        return Some(CurveProblem::PointCount {
            expected: sum,
            found: input.points.len() as u64,
        });
    }
    match input.form {
        CurveForm::Nurbs { order, knots, .. } => {
            if order.len() != input.vertex_counts.len() || order.iter().any(|&o| o < 0) {
                return Some(CurveProblem::Arrays { attribute: "order" });
            }
            let expected: i64 = input
                .vertex_counts
                .iter()
                .zip(order)
                .map(|(&n, &o)| i64::from(n) + i64::from(o))
                .sum();
            if knots.len() as i64 != expected {
                return Some(CurveProblem::Arrays { attribute: "knots" });
            }
        }
        CurveForm::Hermite { tangents } => {
            if tangents.len() != input.points.len() {
                return Some(CurveProblem::Arrays {
                    attribute: "tangents",
                });
            }
        }
        CurveForm::Basis { .. } => {}
    }
    None
}

/// A primvar flattened to one value per element of its interpolation.
struct Channel<'a> {
    interpolation: Interpolation,
    floats: Option<Vec<f32>>,
    vectors: Option<Vec<[f32; 3]>>,
    _marker: core::marker::PhantomData<&'a ()>,
}

trait ChannelValues: Copy {
    fn into_channel<'a>(values: Vec<Self>, interpolation: Interpolation) -> Channel<'a>;
}

impl ChannelValues for f32 {
    fn into_channel<'a>(values: Vec<Self>, interpolation: Interpolation) -> Channel<'a> {
        Channel {
            interpolation,
            floats: Some(values),
            vectors: None,
            _marker: core::marker::PhantomData,
        }
    }
}

impl ChannelValues for [f32; 3] {
    fn into_channel<'a>(values: Vec<Self>, interpolation: Interpolation) -> Channel<'a> {
        Channel {
            interpolation,
            floats: None,
            vectors: Some(values),
            _marker: core::marker::PhantomData,
        }
    }
}

fn float_value(channel: &Channel<'_>, i: usize) -> f32 {
    channel
        .floats
        .as_ref()
        .and_then(|values| values.get(i))
        .copied()
        .unwrap_or(1.0)
}

fn vec3(channel: &Channel<'_>, i: usize) -> [f64; 4] {
    let v = channel
        .vectors
        .as_ref()
        .and_then(|values| values.get(i))
        .copied()
        .unwrap_or([0.0; 3]);
    [f64::from(v[0]), f64::from(v[1]), f64::from(v[2]), 0.0]
}

/// Flattens and checks one primvar, setting its state.
fn channel<'a, T: ChannelValues>(
    primvar: Option<&Primvar<'_, T>>,
    sizes: CurveDataSizes,
    state: &mut ChannelState,
    evaluation: CurveEvaluation,
) -> Option<Channel<'a>> {
    let primvar = primvar?;
    let indexed = primvar.indices.is_some();
    let drop = |state: &mut ChannelState, reason| {
        *state = ChannelState::Dropped(reason);
        None
    };
    if primvar.element_size != 1 {
        return drop(state, DropReason::ElementSize(primvar.element_size));
    }
    let interpolation = primvar.interpolation;
    let expected = match interpolation {
        Interpolation::Constant => 1,
        Interpolation::Uniform => sizes.uniform,
        Interpolation::Varying | Interpolation::Vertex if evaluation == CurveEvaluation::Nurbs => {
            return drop(state, DropReason::Interpolation(interpolation));
        }
        Interpolation::Varying => sizes.varying,
        Interpolation::Vertex => sizes.vertex,
        Interpolation::FaceVarying => {
            return drop(state, DropReason::Interpolation(interpolation));
        }
    };
    let values: Vec<T> = match primvar.indices {
        Some(indices) => {
            let mut out = Vec::with_capacity(indices.len());
            for &index in indices {
                let Some(value) = usize::try_from(index)
                    .ok()
                    .and_then(|i| primvar.values.get(i))
                else {
                    return drop(state, DropReason::Index(index));
                };
                out.push(*value);
            }
            out
        }
        None => primvar.values.to_vec(),
    };
    if (values.len() as u64) < expected {
        return drop(
            state,
            DropReason::TooFew {
                expected,
                found: values.len() as u64,
            },
        );
    }
    *state = ChannelState::Used {
        interpolation,
        indexed,
    };
    Some(T::into_channel(values, interpolation))
}

fn resolve(control: Control, vertex: &dyn Fn(u32) -> [f64; 3]) -> [f64; 3] {
    match control {
        Control::Vertex(i) => vertex(i),
        Control::Phantom(a, b) => {
            let (a, b) = (vertex(a), vertex(b));
            core::array::from_fn(|k| 2.0 * a[k] - b[k])
        }
    }
}

fn resolve4(control: Control, value: &dyn Fn(u32) -> [f64; 4]) -> [f64; 4] {
    match control {
        Control::Vertex(i) => value(i),
        Control::Phantom(a, b) => {
            let (a, b) = (value(a), value(b));
            core::array::from_fn(|k| 2.0 * a[k] - b[k])
        }
    }
}

fn lerp4(a: [f64; 4], b: [f64; 4], u: f64) -> [f64; 4] {
    core::array::from_fn(|k| (1.0 - u) * a[k] + u * b[k])
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "buffers are f32, as a renderer takes them"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

/// Sweeps one curve's samples into `mesh`: a ribbon when normals are
/// drawn, a tube otherwise, one sweep per piece between `breaks`. Each
/// piece after the first is seeded with the frame its predecessor ended
/// on, so pieces whose ends meet continue without a twist. Returns what it
/// swept and its degenerate stations.
#[expect(clippy::too_many_arguments, reason = "one curve's channels")]
fn sweep_curve(
    mesh: &mut MeshBuffers,
    points: &[[f64; 3]],
    closed: bool,
    breaks: &[u32],
    widths: &[f64],
    normals: Option<&[[f64; 3]]>,
    colors: Option<&[[f64; 3]]>,
    sides: u32,
    curve: u32,
    scratch: &mut SweepScratch,
) -> (CurveGeometry, u32) {
    let geometry = if normals.is_some() {
        CurveGeometry::Ribbons
    } else {
        CurveGeometry::Tubes {
            sides: sides.max(3),
        }
    };
    let mut degenerate = 0;
    let mut seed = None;
    let mut first = 0;
    for end in breaks.iter().map(|&b| b as usize).chain([points.len()]) {
        let piece = first..end;
        first = end;
        degenerate += sweep_piece(
            mesh,
            &points[piece.clone()],
            closed,
            &widths[piece.clone()],
            normals.map(|normals| &normals[piece.clone()]),
            colors.map(|colors| &colors[piece]),
            sides,
            curve,
            &mut seed,
            scratch,
        );
    }
    (geometry, degenerate)
}

/// Sweeps one piece of a curve into `mesh`, framed from `seed` and leaving
/// its last normal there. Returns its degenerate stations.
#[expect(clippy::too_many_arguments, reason = "one piece's channels")]
fn sweep_piece(
    mesh: &mut MeshBuffers,
    points: &[[f64; 3]],
    closed: bool,
    widths: &[f64],
    normals: Option<&[[f64; 3]]>,
    colors: Option<&[[f64; 3]]>,
    sides: u32,
    curve: u32,
    seed: &mut Option<[f64; 3]>,
    scratch: &mut SweepScratch,
) -> u32 {
    // A closed curve's last sample repeats its first: frame the distinct
    // samples around the loop, then repeat the first frame.
    let unique = if closed && points.len() > 1 {
        &points[..points.len() - 1]
    } else {
        points
    };
    let SweepScratch {
        workspace,
        frames,
        radii,
        mesh: swept,
    } = scratch;
    let Ok(frame_report) = exedra_curves::rotation_minimizing_frames_into(
        unique, None, closed, *seed, workspace, frames,
    ) else {
        return 0;
    };
    if unique.len() < points.len() {
        frames.push(frames[0]);
    }
    *seed = frames.last().map(|frame| frame.normal);
    let result = match normals {
        Some(normals) => exedra_curves::ribbon_into(frames, Some(normals), widths, false, swept),
        None => {
            radii.clear();
            radii.extend(widths.iter().map(|w| w * 0.5));
            exedra_curves::tube_into(frames, radii, sides.max(3), false, workspace, swept)
        }
    };
    if result.is_err() {
        return 0;
    }
    let base = mesh.positions.len() as u32;
    let ring = swept.ring as usize;
    mesh.positions
        .extend(swept.positions.iter().map(|p| p.map(narrow)));
    mesh.normals
        .extend(swept.normals.iter().map(|n| n.map(narrow)));
    if let Some(colors) = colors {
        let out = mesh.colors.get_or_insert_with(Vec::new);
        for color in colors {
            for _ in 0..ring {
                out.push(color.map(narrow));
            }
        }
    }
    mesh.indices
        .extend(swept.indices.iter().map(|index| base + index));
    mesh.triangle_faces
        .extend(core::iter::repeat_n(curve, swept.indices.len() / 3));
    frame_report.repeated + frame_report.cusps + swept.report.collapsed + swept.report.unoriented
}

/// Buffers a fill reuses from curve to curve through `exedra_curves`'
/// `_into` forms, so a curve allocates only what it adds to the output.
/// They last one fill: keeping them from fill to fill measured no faster.
#[derive(Default)]
struct Scratch {
    controls: Vec<[f64; 3]>,
    points: Vec<[f64; 3]>,
    samples: NurbsSamples,
    sweep: SweepScratch,
}

/// Buffers a sweep reuses from piece to piece.
#[derive(Default)]
struct SweepScratch {
    workspace: Workspace,
    frames: Vec<Frame>,
    radii: Vec<f64>,
    mesh: SweptMesh,
}
