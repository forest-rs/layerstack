// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dense/sparse point and normal offsets, with weighted inbetween interpolation.
use super::{SkelError, SkeletonQuery, SkinningQuery, invalid, length, read, tokens};
use crate::{PrimView, Scene, Time, usd_skel::BlendShape};
use alloc::{string::String, vec, vec::Vec};
use layerstack::{HashSet, PathId, TargetPath};

fn validate_offsets(offsets: &[[f32; 3]], indices: &[i32], count: usize) -> Result<(), SkelError> {
    if (indices.is_empty() && offsets.len() != count)
        || (!indices.is_empty() && offsets.len() != indices.len())
    {
        return Err(SkelError::InvalidDeformation {
            element: None,
            reason: "blend-shape offset count",
        });
    }
    let mut seen = HashSet::new();
    for (element, &index) in indices.iter().enumerate() {
        if usize::try_from(index).ok().is_none_or(|i| i >= count) || !seen.insert(index) {
            return Err(SkelError::InvalidDeformation {
                element: Some(element),
                reason: "blend-shape point index out of range or duplicated",
            });
        }
    }
    Ok(())
}
fn apply_offsets(weight: f32, offsets: &[[f32; 3]], indices: &[i32], points: &mut [[f32; 3]]) {
    if weight.abs() <= 1e-6 {
        return;
    }
    for (i, offset) in offsets.iter().enumerate() {
        let point = if indices.is_empty() {
            i
        } else {
            usize::try_from(indices[i]).expect("validated index")
        };
        for (value, &offset) in points[point].iter_mut().zip(offset) {
            *value += offset * weight;
        }
    }
}
/// Applies one dense or sparse blend shape, as `UsdSkelApplyBlendShape`.
/// Empty indices mean dense offsets. Weights can
/// extrapolate beyond zero and one. Inputs are validated before output is made.
/// OpenUSD `UsdSkelApplyBlendShape`; AOUSD Core §6.2 (float vector values).
pub fn apply_blend_shape(
    weight: f32,
    offsets: &[[f32; 3]],
    point_indices: &[i32],
    points: &[[f32; 3]],
) -> Result<Vec<[f32; 3]>, SkelError> {
    let mut result = points.to_vec();
    apply_blend_shape_in_place(weight, offsets, point_indices, &mut result)?;
    Ok(result)
}
/// Applies offsets to a reusable point/normal buffer without allocating.
/// Validation errors leave the buffer unchanged. As the C++ utility, weights
/// within `1e-6` of zero do no work and do not inspect offset/index arrays.
pub fn apply_blend_shape_in_place(
    weight: f32,
    offsets: &[[f32; 3]],
    point_indices: &[i32],
    points: &mut [[f32; 3]],
) -> Result<(), SkelError> {
    if !weight.is_finite() {
        return Err(SkelError::InvalidDeformation {
            element: None,
            reason: "nonfinite blend-shape weight",
        });
    }
    if weight.abs() > 1e-6 {
        validate_offsets(offsets, point_indices, points.len())?;
        apply_offsets(weight, offsets, point_indices, points);
    }
    Ok(())
}
#[derive(Clone, Debug)]
struct Sample {
    weight: f32,
    offsets: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
}
#[derive(Clone, Debug)]
struct Shape {
    indices: Vec<i32>,
    samples: Vec<Sample>,
}
/// One non-null shape sample's contribution after inbetween interpolation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlendShapeContribution {
    /// Index in the binding's blend-shape order.
    pub shape: usize,
    /// Sample index in that shape's ascending knot order (including null knot).
    pub sample: usize,
    /// Multiplicative weight of this sample's offsets.
    pub weight: f32,
}
/// Local geometry blend-shape definition snapshot. Names and targets are never
/// inherited. Rebuild after shape definitions, inbetween metadata or bindings edit.
/// Dense/sparse offsets apply in geometry space, before joint skinning.
#[derive(Clone, Debug)]
pub struct BlendShapeQuery {
    geometry: PathId,
    names: Vec<String>,
    targets: Vec<PathId>,
    shapes: Vec<Shape>,
}
impl BlendShapeQuery {
    /// Captures local names, target offsets and authored inbetween weights.
    /// Absent local blend-shape bindings return `None`; malformed definitions
    /// error. Reserved/duplicate/nonfinite inbetween knots are rejected.
    pub fn new(scene: &Scene<'_>, geometry: PathId) -> Result<Option<Self>, SkelError> {
        if !scene.has_api(geometry, "SkelBindingAPI", None) {
            return Ok(None);
        }
        let prim = PrimView::new(*scene, geometry);
        let names = tokens(&prim, "skel:blendShapes");
        let targets = prim.read_targets("skel:blendShapeTargets");
        if names.is_none() && targets.is_empty() {
            return Ok(None);
        }
        let names = names.ok_or_else(|| invalid(geometry, "skel:blendShapes"))?;
        length(
            geometry,
            "skel:blendShapeTargets",
            names.len(),
            targets.len(),
        )?;
        let mut unique = HashSet::new();
        if names.iter().any(|name| !unique.insert(name)) {
            return Err(invalid(geometry, "skel:blendShapes"));
        }
        drop(unique);
        let mut paths = Vec::with_capacity(targets.len());
        let mut shapes = Vec::with_capacity(targets.len());
        for target in targets {
            let TargetPath::Prim(path) = target else {
                return Err(SkelError::InvalidTarget {
                    prim: geometry,
                    relationship: "skel:blendShapeTargets",
                    target,
                });
            };
            let shape = BlendShape::new(scene, path).ok_or(SkelError::InvalidTarget {
                prim: geometry,
                relationship: "skel:blendShapeTargets",
                target,
            })?;
            let indices = shape.point_indices().unwrap_or_default();
            let offsets = shape.offsets().ok_or_else(|| invalid(path, "offsets"))?;
            let mut samples = vec![
                Sample {
                    weight: 0.,
                    offsets: Vec::new(),
                    normals: Vec::new(),
                },
                Sample {
                    weight: 1.,
                    offsets,
                    normals: shape.normal_offsets().unwrap_or_default(),
                },
            ];
            for token in scene.stage().authored_property_names(path, scene.store()) {
                let name = scene.store().tokens().resolve(token);
                if !name.starts_with("inbetweens:") || name.ends_with(":normalOffsets") {
                    continue;
                }
                let Some(weight) = shape.property_metadata(name).and_then(|m| m.weight()) else {
                    continue;
                };
                if !weight.is_finite() || weight.abs() <= 1e-6 || (weight - 1.).abs() <= 1e-6 {
                    return Err(invalid(path, "inbetweens"));
                }
                let offsets = shape
                    .read_value(name, crate::value::read_float3_array)
                    .ok_or_else(|| invalid(path, "inbetweens"))?;
                let normals = shape
                    .read_value(
                        &alloc::format!("{name}:normalOffsets"),
                        crate::value::read_float3_array,
                    )
                    .unwrap_or_default();
                samples.push(Sample {
                    weight,
                    offsets,
                    normals,
                });
            }
            samples.sort_by(|a, b| a.weight.total_cmp(&b.weight));
            if samples
                .windows(2)
                .any(|pair| pair[0].weight == pair[1].weight)
            {
                return Err(invalid(path, "inbetweens"));
            }
            paths.push(path);
            shapes.push(Shape { indices, samples });
        }
        Ok(Some(Self {
            geometry,
            names,
            targets: paths,
            shapes,
        }))
    }
    /// Geometry owning this local binding.
    #[must_use]
    pub fn geometry_path(&self) -> PathId {
        self.geometry
    }
    /// Blend-shape names in binding order, for animation weight remapping.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }
    /// Shape definition roots consulted by this snapshot, in binding order.
    #[must_use]
    pub fn targets(&self) -> &[PathId] {
        &self.targets
    }
    /// Number of retained sample knots, including each shape's null/primary knots.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.shapes.iter().map(|s| s.samples.len()).sum()
    }
    /// Computes non-null sample contributions, with piecewise interpolation and
    /// endpoint extrapolation. Matches `UsdSkelBlendShapeQuery`'s `1e-6` epsilon.
    pub fn compute_weights(
        &self,
        weights: &[f32],
    ) -> Result<Vec<BlendShapeContribution>, SkelError> {
        length(
            self.geometry,
            "skel:blendShapes",
            self.shapes.len(),
            weights.len(),
        )?;
        let mut result = Vec::with_capacity(weights.len() * 2);
        for (shape, (&weight, data)) in weights.iter().zip(&self.shapes).enumerate() {
            if !weight.is_finite() {
                return Err(SkelError::InvalidDeformation {
                    element: Some(shape),
                    reason: "nonfinite blend-shape weight",
                });
            }
            if data.samples.len() == 2 {
                result.push(BlendShapeContribution {
                    shape,
                    sample: 1,
                    weight,
                });
                continue;
            }
            let upper = data
                .samples
                .partition_point(|s| s.weight <= weight)
                .clamp(1, data.samples.len() - 1);
            let lower = upper - 1;
            let delta = data.samples[upper].weight - data.samples[lower].weight;
            if delta <= 1e-6 {
                continue;
            }
            let alpha = (weight - data.samples[lower].weight) / delta;
            for (sample, weight) in [(lower, 1. - alpha), (upper, alpha)] {
                if data.samples[sample].weight != 0. && weight.abs() > 1e-6 {
                    result.push(BlendShapeContribution {
                        shape,
                        sample,
                        weight,
                    });
                }
            }
        }
        Ok(result)
    }
    fn deform(
        &self,
        weights: &[f32],
        values: &[[f32; 3]],
        normals: bool,
    ) -> Result<Vec<[f32; 3]>, SkelError> {
        let mut result = values.to_vec();
        self.deform_in_place(weights, &mut result, normals)?;
        Ok(result)
    }
    fn deform_in_place(
        &self,
        weights: &[f32],
        values: &mut [[f32; 3]],
        normals: bool,
    ) -> Result<(), SkelError> {
        let contributions = self.compute_weights(weights)?;
        // Validate every active offset set before accumulating any output.
        let offsets = |c: &BlendShapeContribution| {
            let sample = &self.shapes[c.shape].samples[c.sample];
            if normals {
                &sample.normals
            } else {
                &sample.offsets
            }
        };
        for c in &contributions {
            if c.weight.abs() > 1e-6 && !offsets(c).is_empty() {
                validate_offsets(offsets(c), &self.shapes[c.shape].indices, values.len())?;
            }
        }
        for c in contributions {
            apply_offsets(c.weight, offsets(&c), &self.shapes[c.shape].indices, values);
        }
        Ok(())
    }
    /// Applies weighted point offsets in geometry space, including inbetweens.
    pub fn deform_points(
        &self,
        weights: &[f32],
        points: &[[f32; 3]],
    ) -> Result<Vec<[f32; 3]>, SkelError> {
        self.deform(weights, points, false)
    }
    /// Applies point offsets to a reusable buffer. All active offset sets
    /// validate before mutation; errors leave the buffer unchanged.
    pub fn deform_points_in_place(
        &self,
        weights: &[f32],
        points: &mut [[f32; 3]],
    ) -> Result<(), SkelError> {
        self.deform_in_place(weights, points, false)
    }
    /// Applies normal offsets to a reusable buffer, without normalizing or
    /// joint skinning. All active sets validate before mutation.
    pub fn deform_normals_in_place(
        &self,
        weights: &[f32],
        normals: &mut [[f32; 3]],
    ) -> Result<(), SkelError> {
        self.deform_in_place(weights, normals, true)
    }
    /// Applies weighted normal offsets without normalization or joint skinning.
    pub fn deform_normals(
        &self,
        weights: &[f32],
        normals: &[[f32; 3]],
    ) -> Result<Vec<[f32; 3]>, SkelError> {
        self.deform(weights, normals, true)
    }
}
impl SkeletonQuery<'_> {
    /// Maps sampled animation weights into a geometry's blend-shape name order.
    /// Unmapped names and unreadable weight arrays use zero; inconsistent readable
    /// arrays error. Missing joint animation does not suppress blend animation.
    pub fn blend_shape_weights(&self, time: Time, names: &[String]) -> Result<Vec<f32>, SkelError> {
        let Some(animation) = self.animation else {
            return Ok(vec![0.; names.len()]);
        };
        let order = &self.definition.blend_shapes;
        let Some(weights) = read(
            &animation,
            "blendShapeWeights",
            time,
            crate::value::read_float_array,
        ) else {
            return Ok(vec![0.; names.len()]);
        };
        length(
            animation.path(),
            "blendShapeWeights",
            order.len(),
            weights.len(),
        )?;
        let mut seen = HashSet::new();
        if order.iter().any(|name| !seen.insert(name)) {
            return Err(invalid(animation.path(), "blendShapes"));
        }
        Ok(names
            .iter()
            .map(|name| {
                order
                    .iter()
                    .position(|j| j == name)
                    .map_or(0., |i| weights[i])
            })
            .collect())
    }
}
impl SkinningQuery<'_> {
    /// Applies local animated blend shapes before joint skinning, returning
    /// skeleton-space points. Missing shape bindings leave the points unchanged
    /// before skinning. Shape definitions are captured by the binding snapshot.
    pub fn compute_deformed_points(&self, time: Time) -> Result<Vec<[f32; 3]>, SkelError> {
        let points = read(
            &PrimView::new(self.scene, self.definition.geometry),
            "points",
            time,
            crate::value::read_float3_array,
        )
        .ok_or_else(|| invalid(self.definition.geometry, "points"))?;
        let points = if let Some(shapes) = &self.definition.blend_shapes {
            shapes.deform_points(
                &self.skeleton.blend_shape_weights(time, shapes.names())?,
                &points,
            )?
        } else {
            points
        };
        self.skin_points(&points, time)
    }
    /// Local shape definition snapshot retained by this geometry binding.
    #[must_use]
    pub fn blend_shape_query(&self) -> Option<&BlendShapeQuery> {
        self.definition.blend_shapes.as_ref()
    }
}
