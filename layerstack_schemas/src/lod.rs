// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Owned OpenUSD LOD queries, with explicit capture and pure runtime evaluation.
//!
//! Matrices transform row vectors. Extents are local to the LOD root, even when
//! obtained through `boundingVolume`; the target's transform is intentionally
//! not applied, matching `UsdLod*Heuristic::Create*HeuristicQuery`. Capture again
//! after scene edits. These queries select a numeric LOD, not visibility or
//! renderer resources. AOUSD Core §6.3, §12.3–12.5; OpenUSD `usdLod` queries.

mod projection;
pub use projection::{LodFrustum, LodProjection, ProjectionMethod};

use crate::{
    PrimView, Scene, gf,
    usd_lod::{LODDistanceHeuristic, LODScreenSizeHeuristic},
};
use alloc::{sync::Arc, vec::Vec};
use layerstack::{PathId, PropertyPath, TargetPath, Time, TokenInterner, Value};

/// USD row-vector matrix, translation in the last row.
pub type LodMatrix = [[f64; 4]; 4];
/// A local-space extent, minimum and maximum corners. Reversed axes are empty.
pub type LodExtent = [[f64; 3]; 2];

/// A recoverable input error. No stage or renderer state changes on error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LodError {
    /// Missing, incompatible, malformed or nonfinite schema attribute.
    InvalidAttribute(&'static str),
    /// A threshold list is not ordered in the metric's required direction.
    UnsortedThresholds(&'static str),
    /// A metric, viewpoint, or previous metric is negative or nonfinite.
    InvalidMetric,
    /// Hysteresis must be finite and nonnegative.
    InvalidHysteresis,
    /// A runtime transform is nonfinite or is not affine.
    InvalidTransform,
    /// Frustum parameters do not define a finite, nondegenerate view.
    InvalidFrustum,
    /// An unknown projection method cannot be evaluated.
    UnsupportedProjection,
    /// Resolving a nonempty bounding-volume relationship needs `usd-geom`.
    GeometryFeatureRequired,
    /// Projection reached zero homogeneous W or produced a nonfinite result.
    InvalidProjection,
}
impl core::fmt::Display for LodError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid LOD input: {self:?}")
    }
}
impl core::error::Error for LodError {}

/// Runtime hysteresis state supplied by the caller. It retains no scene handles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodHysteresis {
    /// Previously returned metric, in distance units or viewport area fraction.
    pub previous: f64,
    /// Nonnegative width in the same units as the metric.
    pub width: f64,
}
impl LodHysteresis {
    /// OpenUSD's lagging deadband: retain the previous value within the window,
    /// otherwise follow the current value minus/plus the window width.
    pub fn apply(self, current: f64) -> Result<f64, LodError> {
        metric(current)?;
        metric(self.previous)?;
        if !self.width.is_finite() || self.width < 0. {
            return Err(LodError::InvalidHysteresis);
        }
        Ok(if self.previous < current - self.width {
            current - self.width
        } else if self.previous > current + self.width {
            current + self.width
        } else {
            self.previous
        })
    }
}
/// A numeric LOD decision and the metric to retain for the next evaluation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodDecision {
    /// Distance or projected viewport-area fraction, after optional hysteresis.
    pub metric: f64,
    /// Fractional index, as `UsdLod*HeuristicQuery::ComputeLOD` returns it.
    pub index: f32,
}

/// Captured distance query, independent of the source stage.
#[derive(Clone, Debug, PartialEq)]
pub struct DistanceHeuristicQuery {
    /// Domain identifying which renderer should consider this query.
    pub domain: Arc<str>,
    /// Fallback center in local coordinates, ignored for a usable extent.
    pub center: [f64; 3],
    /// Optional local bounding extent. Empty ranges act as no extent.
    pub extent: Option<LodExtent>,
    /// Path that supplied the extent, useful when invalidating capture.
    pub bounding_volume: Option<PathId>,
    /// Distance transitions, in ascending order.
    pub thresholds: Vec<f32>,
    /// Ascending transition end points; missing entries disable blending there.
    pub blend_thresholds: Vec<f32>,
}
impl DistanceHeuristicQuery {
    /// Checks finite geometry and the ordering required by binary search.
    pub fn validate(&self) -> Result<(), LodError> {
        if self.center.iter().any(|v| !v.is_finite()) {
            return Err(LodError::InvalidAttribute("center"));
        }
        validate_extent(self.extent)?;
        thresholds(&self.thresholds, false, "thresholds")?;
        thresholds(&self.blend_thresholds, false, "blendThresholds")
    }
    /// Computes distance to the clamped local extent point transformed back to
    /// viewpoint space. This follows OpenUSD's approximation for sheared boxes.
    /// A determinant of magnitude <=1e-10 uses the transformed center instead.
    pub fn compute_distance(
        &self,
        viewpoint: [f64; 3],
        transform: &LodMatrix,
    ) -> Result<f64, LodError> {
        self.validate()?;
        validate_transform(transform)?;
        if viewpoint.iter().any(|v| !v.is_finite()) {
            return Err(LodError::InvalidMetric);
        }
        let mut target = self.center;
        if let Some(extent) = self.extent.filter(|e| !empty(e)) {
            let (inverse, determinant) = gf::inverse(transform);
            if determinant.abs() > 1e-10 {
                let local = point(viewpoint, &inverse);
                target = core::array::from_fn(|i| local[i].clamp(extent[0][i], extent[1][i]));
            }
        }
        let target = point(target, transform);
        let distance = libm::sqrt(
            (0..3)
                .map(|i| {
                    let delta = target[i] - viewpoint[i];
                    delta * delta
                })
                .sum(),
        );
        metric(distance)?;
        Ok(distance)
    }
    /// Evaluates ascending thresholds and clamped blend windows.
    pub fn compute_lod(&self, distance: f64) -> Result<f32, LodError> {
        self.validate()?;
        blend_lod(distance, &self.thresholds, &self.blend_thresholds, false)
    }
    /// Captures the next runtime metric and LOD decision without mutating state.
    pub fn evaluate(
        &self,
        viewpoint: [f64; 3],
        transform: &LodMatrix,
        hysteresis: Option<LodHysteresis>,
    ) -> Result<LodDecision, LodError> {
        let mut distance = self.compute_distance(viewpoint, transform)?;
        if let Some(h) = hysteresis {
            distance = h.apply(distance)?;
        }
        Ok(LodDecision {
            metric: distance,
            index: self.compute_lod(distance)?,
        })
    }
}

/// Captured screen-area query, independent of the source stage.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenSizeHeuristicQuery {
    /// Domain identifying which renderer should consider this query.
    pub domain: Arc<str>,
    /// Extent in the LOD root's coordinates. Empty ranges have size zero.
    pub extent: Option<LodExtent>,
    /// Path that supplied an overriding extent.
    pub bounding_volume: Option<PathId>,
    /// Sphere approximation or near-plane-clipped projected convex hull.
    pub projection_method: ProjectionMethod,
    /// Viewport-area fractions, in descending order.
    pub thresholds: Vec<f32>,
    /// Descending transition end points; missing entries disable blending there.
    pub blend_thresholds: Vec<f32>,
}
impl ScreenSizeHeuristicQuery {
    /// Checks finite geometry and descending transition arrays.
    pub fn validate(&self) -> Result<(), LodError> {
        validate_extent(self.extent)?;
        thresholds(&self.thresholds, true, "thresholds")?;
        thresholds(&self.blend_thresholds, true, "blendThresholds")
    }
    /// Computes unclamped fraction of viewport area. Completely clipped objects
    /// yield zero. Extent projection clips only the near plane after rejection,
    /// preserving size as an object slides through the sides of the frustum.
    pub fn compute_screen_size(
        &self,
        frustum: &LodFrustum,
        transform: &LodMatrix,
    ) -> Result<f64, LodError> {
        self.validate()?;
        validate_transform(transform)?;
        frustum.validate()?;
        let Some(extent) = self.extent.filter(|e| !empty(e)) else {
            return Ok(0.);
        };
        let result = projection::size(extent, self.projection_method, frustum, transform)?;
        metric(result)?;
        Ok(result)
    }
    /// Evaluates descending thresholds and clamped blend windows.
    pub fn compute_lod(&self, size: f64) -> Result<f32, LodError> {
        self.validate()?;
        blend_lod(size, &self.thresholds, &self.blend_thresholds, true)
    }
    /// Returns the next metric and LOD decision, with optional caller-owned state.
    pub fn evaluate(
        &self,
        frustum: &LodFrustum,
        transform: &LodMatrix,
        hysteresis: Option<LodHysteresis>,
    ) -> Result<LodDecision, LodError> {
        let mut size = self.compute_screen_size(frustum, transform)?;
        if let Some(h) = hysteresis {
            size = h.apply(size)?;
        }
        Ok(LodDecision {
            metric: size,
            index: self.compute_lod(size)?,
        })
    }
}

fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &str,
    time: Time,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Option<T> {
    match time {
        Time::Default => prim.read_value(name, decode),
        Time::At {
            code,
            interpolation,
        } => prim.read_value_at(name, code, interpolation, decode),
    }
}
fn volume(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
) -> Result<Option<(LodExtent, PathId)>, LodError> {
    let Some(token) = scene.store().tokens().lookup("boundingVolume") else {
        return Ok(None);
    };
    let targets = crate::view::forwarded_targets(scene, PropertyPath::new(path, token));
    #[cfg(not(feature = "usd-geom"))]
    {
        let _ = time;
        if !targets.is_empty() {
            return Err(LodError::GeometryFeatureRequired);
        }
    }
    #[cfg(feature = "usd-geom")]
    for target in targets {
        if let TargetPath::Prim(path) = target
            && scene.is_a(path, "Boundable")
            && let Some((range, _)) = crate::extent::compute(scene, path, time)
        {
            let extent = [range.min, range.max];
            validate_extent(Some(extent))?;
            return Ok(Some((extent, path)));
        }
    }
    Ok(None)
}
impl LODDistanceHeuristic<'_> {
    /// Reads an owned query at one explicit time. The first forwarded boundable
    /// target with a computed extent wins, including an empty extent.
    pub fn capture_query(&self, time: Time) -> Result<DistanceHeuristicQuery, LodError> {
        let bound = volume(&self.scene(), self.path(), time)?;
        let query = DistanceHeuristicQuery {
            domain: Arc::from(
                read(self, "lod:domain", time, crate::value::read_token)
                    .ok_or(LodError::InvalidAttribute("lod:domain"))?,
            ),
            center: read(self, "center", time, crate::value::read_float3)
                .ok_or(LodError::InvalidAttribute("center"))?
                .map(f64::from),
            extent: bound.map(|v| v.0),
            bounding_volume: bound.map(|v| v.1),
            thresholds: read(self, "thresholds", time, crate::value::read_float_array)
                .ok_or(LodError::InvalidAttribute("thresholds"))?,
            blend_thresholds: read(
                self,
                "blendThresholds",
                time,
                crate::value::read_float_array,
            )
            .ok_or(LodError::InvalidAttribute("blendThresholds"))?,
        };
        query.validate()?;
        Ok(query)
    }
}
impl LODScreenSizeHeuristic<'_> {
    /// Reads an owned query at one explicit time. Bounding-volume geometry
    /// overrides the authored extent without using the target's transform.
    pub fn capture_query(&self, time: Time) -> Result<ScreenSizeHeuristicQuery, LodError> {
        let bound = volume(&self.scene(), self.path(), time)?;
        let extent = if let Some((extent, _)) = bound {
            Some(extent)
        } else {
            let values = read(self, "extent", time, crate::value::read_float3_array)
                .ok_or(LodError::InvalidAttribute("extent"))?;
            match values.as_slice() {
                [] => None,
                [a, b] => Some([a.map(f64::from), b.map(f64::from)]),
                _ => return Err(LodError::InvalidAttribute("extent")),
            }
        };
        let projection_method = match read(self, "projectionMethod", time, crate::value::read_token)
        {
            Some("projectedSphere") => ProjectionMethod::Sphere,
            Some("projectedExtent") => ProjectionMethod::Extent,
            _ => return Err(LodError::UnsupportedProjection),
        };
        let query = ScreenSizeHeuristicQuery {
            domain: Arc::from(
                read(self, "lod:domain", time, crate::value::read_token)
                    .ok_or(LodError::InvalidAttribute("lod:domain"))?,
            ),
            extent,
            bounding_volume: bound.map(|v| v.1),
            projection_method,
            thresholds: read(self, "thresholds", time, crate::value::read_float_array)
                .ok_or(LodError::InvalidAttribute("thresholds"))?,
            blend_thresholds: read(
                self,
                "blendThresholds",
                time,
                crate::value::read_float_array,
            )
            .ok_or(LodError::InvalidAttribute("blendThresholds"))?,
        };
        query.validate()?;
        Ok(query)
    }
}
fn metric(value: f64) -> Result<(), LodError> {
    if value.is_finite() && value >= 0. {
        Ok(())
    } else {
        Err(LodError::InvalidMetric)
    }
}
fn thresholds(values: &[f32], descending: bool, name: &'static str) -> Result<(), LodError> {
    if values.iter().any(|v| !v.is_finite()) {
        return Err(LodError::InvalidAttribute(name));
    }
    if values
        .windows(2)
        .any(|w| if descending { w[0] < w[1] } else { w[0] > w[1] })
    {
        return Err(LodError::UnsortedThresholds(name));
    }
    Ok(())
}
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "OpenUSD returns a float LOD index and blend fraction"
)]
fn blend_lod(
    value: f64,
    thresholds: &[f32],
    blends: &[f32],
    descending: bool,
) -> Result<f32, LodError> {
    metric(value)?;
    let passed = |v: &f32| {
        if descending {
            f64::from(*v) >= value
        } else {
            f64::from(*v) <= value
        }
    };
    let index = thresholds.partition_point(passed);
    if blends.is_empty() {
        return Ok(index as f32);
    }
    let blend = blends
        .partition_point(passed)
        .clamp(index.saturating_sub(1), index);
    if blend == index || blend >= blends.len() {
        return Ok(index as f32);
    }
    let a = thresholds[blend];
    let mut b = blends[blend];
    if let Some(&next) = thresholds.get(index) {
        b = if descending { b.max(next) } else { b.min(next) };
    }
    let (a, b) = (f64::from(a), f64::from(b));
    if if descending {
        a >= value && value > b
    } else {
        a <= value && value < b
    } {
        // C++ rounds the fraction to float before adding the integral index.
        Ok(blend as f32 + ((value - a) / (b - a)) as f32)
    } else {
        Ok(index as f32)
    }
}
fn empty(extent: &LodExtent) -> bool {
    (0..3).any(|i| extent[0][i] > extent[1][i])
}
fn validate_extent(extent: Option<LodExtent>) -> Result<(), LodError> {
    if extent.is_some_and(|e| e.iter().flatten().any(|v| !v.is_finite())) {
        Err(LodError::InvalidAttribute("extent"))
    } else {
        Ok(())
    }
}
fn validate_transform(matrix: &LodMatrix) -> Result<(), LodError> {
    if matrix.iter().flatten().any(|v| !v.is_finite())
        || matrix[0][3] != 0.
        || matrix[1][3] != 0.
        || matrix[2][3] != 0.
        || matrix[3][3] != 1.
    {
        Err(LodError::InvalidTransform)
    } else {
        Ok(())
    }
}
fn point(point: [f64; 3], matrix: &LodMatrix) -> [f64; 3] {
    core::array::from_fn(|j| {
        point[0] * matrix[0][j] + point[1] * matrix[1][j] + point[2] * matrix[2][j] + matrix[3][j]
    })
}
#[cfg(test)]
mod tests;
