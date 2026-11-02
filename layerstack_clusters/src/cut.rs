// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Choosing the clusters to draw for a camera and a pixel error.

use crate::{ClusterSet, NO_GROUP};

/// The view a cut is chosen for, in the mesh's own space (transform the
/// camera position by the inverse of the mesh's world transform; under a
/// scale, divide the pixel error's reach accordingly).
///
/// The projection ignores perspective distortion and view direction, as
/// meshoptimizer's `clusterlod` does: a sphere's error projects by its
/// distance alone, so a cut does not change as the camera turns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CutCamera {
    /// The camera's position.
    pub position: [f32; 3],
    /// The projection's vertical scale: element `[1][1]` of a perspective
    /// projection matrix, `1 / tan(fov_y / 2)`.
    pub projection_y: f32,
    /// The viewport's height in pixels.
    pub viewport_height: f32,
    /// The near plane's distance, positive; closer spheres project as if
    /// at this distance.
    pub z_near: f32,
}

impl CutCamera {
    /// The on-screen size, in pixels, of `error` (mesh units) at a sphere
    /// (center and radius): `error / max(distance - radius, z_near) *
    /// projection_y / 2 * viewport_height`. Infinite for an infinite
    /// error.
    #[must_use]
    pub fn projected_error(&self, sphere: [f32; 4], error: f32) -> f32 {
        let d = [
            sphere[0] - self.position[0],
            sphere[1] - self.position[1],
            sphere[2] - self.position[2],
        ];
        let distance = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() - sphere[3];
        error / distance.max(self.z_near) * (self.projection_y * 0.5 * self.viewport_height)
    }
}

impl ClusterSet {
    /// Fills `out` with the clusters to draw for `camera` at `pixel_error`,
    /// cleared first: each cluster whose group's simplified error projects
    /// above `pixel_error` and whose source group's (the group it was
    /// simplified from, [`crate::ClusterTable::refined`]) projects at or
    /// below it; a cluster of the original triangles has no source and
    /// passes the second test. Walks the [`crate::NodeTable`], descending
    /// only into nodes whose largest error projects above the threshold.
    ///
    /// The clusters come in hierarchy order. Together they cover the
    /// mesh's surface once.
    pub fn select_cut(&self, camera: &CutCamera, pixel_error: f32, out: &mut Vec<u32>) {
        out.clear();
        let nodes = &self.nodes;
        let mut stack: Vec<u32> = (0..self.levels).rev().collect();
        while let Some(node) = stack.pop() {
            let n = node as usize;
            if camera.projected_error(nodes.bounds[n], nodes.error[n]) <= pixel_error {
                continue;
            }
            let group = nodes.group[n];
            if group == NO_GROUP {
                let start = nodes.child_start[n];
                stack.extend((start..start + nodes.child_count[n]).rev());
            } else {
                self.select_group(camera, pixel_error, group, out);
            }
        }
    }

    /// [`Self::select_cut`] by testing every cluster in turn, without the
    /// hierarchy; the clusters come in table order. For checking the
    /// hierarchy, and for sets small enough that walking it costs more.
    pub fn select_cut_linear(&self, camera: &CutCamera, pixel_error: f32, out: &mut Vec<u32>) {
        out.clear();
        for group in 0..self.groups.len() {
            if camera.projected_error(self.groups.bounds[group], self.groups.error[group])
                > pixel_error
            {
                self.select_group(camera, pixel_error, crate::build::to_u32(group), out);
            }
        }
    }

    /// The clusters of `group` (whose own error projects above the
    /// threshold) that are fine enough to draw.
    fn select_group(&self, camera: &CutCamera, pixel_error: f32, group: u32, out: &mut Vec<u32>) {
        let g = group as usize;
        let start = self.groups.cluster_start[g];
        for cluster in start..start + self.groups.cluster_count[g] {
            let refined = self.clusters.refined[cluster as usize];
            if refined == NO_GROUP {
                out.push(cluster);
                continue;
            }
            let r = refined as usize;
            if camera.projected_error(self.groups.bounds[r], self.groups.error[r]) <= pixel_error {
                out.push(cluster);
            }
        }
    }
}
