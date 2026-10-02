// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit point-hull bounds after deformation, separate from USD extents.
use super::{SkelError, SkinningQuery, invalid};
use crate::{
    Time, XformCache,
    bounds::{BoundingBox, Range3d},
};
pub(super) fn range(points: &[[f32; 3]]) -> Result<Range3d, SkelError> {
    let mut range = Range3d::default();
    for (element, p) in points.iter().enumerate() {
        if p.iter().any(|v| !v.is_finite()) {
            return Err(SkelError::InvalidDeformation {
                element: Some(element),
                reason: "nonfinite deformed mesh point",
            });
        }
        let point = p.map(f64::from);
        range.union_with(Range3d {
            min: point,
            max: point,
        });
    }
    Ok(range)
}
pub(super) fn world(
    range: Range3d,
    scene: &crate::Scene<'_>,
    path: layerstack::PathId,
    time: Time,
    xforms: &mut XformCache,
) -> Result<BoundingBox, SkelError> {
    if xforms.time() != time {
        return Err(SkelError::InvalidDeformation {
            element: None,
            reason: "transform cache time differs from deformation time",
        });
    }
    let matrix = xforms
        .local_to_world(scene, path)
        .ok_or_else(|| invalid(path, "xformOpOrder"))?;
    if matrix.iter().flatten().any(|v| !v.is_finite()) {
        return Err(SkelError::InvalidDeformation {
            element: None,
            reason: "nonfinite world bound transform",
        });
    }
    Ok(BoundingBox { range, matrix })
}
impl SkinningQuery<'_> {
    /// Exact hull of all deformed mesh points, in skeleton space. Ignores
    /// authored `extent` and model hints; empty points yield an empty range.
    /// This is an explicit deformation query, not `UsdGeomMesh::ComputeExtent`.
    /// AOUSD Core §6.3; `UsdSkel` blend-before-skin coordinate convention.
    pub fn compute_deformed_mesh_bounds(&self, time: Time) -> Result<Range3d, SkelError> {
        if !self.scene.is_a(self.geometry_path(), "Mesh") {
            return Err(invalid(self.geometry_path(), "points"));
        }
        range(&self.compute_deformed_points(time)?)
    }
}
