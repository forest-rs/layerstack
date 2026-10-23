// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Intrinsic extents of `Skeleton` and `SkelRoot`, as OpenUSD's `UsdSkel`
//! extent plugins compute them for `UsdGeomBoundable::ComputeExtent`.
//!
//! A `Skeleton` is bounded by its posed joint pivots. A `SkelRoot` is bounded by
//! the pivots of each skeleton its bindings use, placed in the root's space,
//! padded by how far the skeleton's skinned geometry reaches past its rest
//! pivots. Neither reads deformed points.

use alloc::vec::Vec;

use layerstack::PathId;

use super::{SkelError, SkeletonQuery, skinning::inherited_property};
use crate::{
    PrimView, Scene, Time, XformCache,
    bounds::{BoundingBox, Range3d},
    extent::{ExtentContext, ExtentError},
    gf,
    usd_skel::{SkelRoot, Skeleton},
};

/// The extent of the `Skeleton` at `context.prim()`: its joint pivots in
/// skeleton space, posed at the requested time. A skeleton that cannot be
/// posed (no joints, mismatched rest transforms) has an empty extent, as
/// the C++ plugin leaves it, so its ancestors still bound.
///
/// OpenUSD: `usdSkel/skeleton.cpp` `_ComputeExtent`,
/// `UsdSkelComputeJointsExtent`.
pub(crate) fn skeleton(context: &mut ExtentContext<'_>) -> Result<Range3d, ExtentError> {
    let scene = *context.scene();
    let Some(skeleton) = Skeleton::new(&scene, context.prim()) else {
        return Ok(Range3d::default());
    };
    let Some(query) = query(context, skeleton)? else {
        return Ok(Range3d::default());
    };
    let Some(pose) = pose(context, &query)? else {
        return Ok(Range3d::default());
    };
    Ok(joints_extent(&pose, 0., None))
}

/// The extent of the `SkelRoot` at `context.prim()`, in its own space. Each
/// skeleton its bindings use contributes its posed pivots, placed by the
/// skeleton's transform relative to the root at default time (C++ builds a
/// default-time `UsdGeomXformCache`, whatever the requested time), padded
/// by the largest [`extents_padding`] of the geometry it skins. A root with
/// no bindings contributes its descendant skeletons, unpadded.
///
/// OpenUSD: `usdSkel/root.cpp` `_ComputeExtent`.
pub(crate) fn skel_root(context: &mut ExtentContext<'_>) -> Result<Range3d, ExtentError> {
    let scene = *context.scene();
    let root = context.prim();
    let Some(skel_root) = SkelRoot::new(&scene, root) else {
        return Ok(Range3d::default());
    };
    // Discovery reads bindings anywhere under the root; the root's own
    // dependency covers its namespace. Every bound prim pads its skeleton,
    // with or without joint influences, as C++'s skinning targets do.
    let bindings = skel_root.bindings_with_instance_proxies();
    // Skeletons in order of first use, each with the geometry it binds.
    let mut skeletons: Vec<(SkeletonQuery<'_>, Vec<PathId>)> = Vec::new();
    let mut seen: Vec<PathId> = Vec::new();
    for binding in &bindings.bindings {
        if let Some((_, targets)) = skeletons
            .iter_mut()
            .find(|(query, _)| query.skeleton_path() == binding.skeleton)
        {
            targets.push(binding.geometry);
        } else if !seen.contains(&binding.skeleton) {
            seen.push(binding.skeleton);
            let skeleton = Skeleton::new(&scene, binding.skeleton).expect("a bound Skeleton");
            if let Some(query) = query(context, skeleton)? {
                skeletons.push((query, alloc::vec![binding.geometry]));
            }
        }
    }
    if bindings.bindings.is_empty() {
        for path in descendant_skeletons(&scene, root) {
            let skeleton = Skeleton::new(&scene, path).expect("a Skeleton prim");
            if let Some(query) = query(context, skeleton)? {
                skeletons.push((query, Vec::new()));
            }
        }
    }
    let mut xforms = XformCache::new(Time::Default);
    let mut bound = Range3d::default();
    for (query, targets) in &skeletons {
        context.track_prim(query.skeleton_path());
        context.track_animation(query);
        let Some(pose) = pose(context, query)? else {
            continue;
        };
        let mut padding = 0_f32;
        if let Some(rest) = query
            .rest_transforms()
            .ok()
            .and_then(|rest| query.topology().concatenate(&rest))
        {
            for &target in targets {
                context.track_prim(target);
                padding = padding.max(extents_padding(&scene, root, target, &rest));
            }
        }
        let placement = xforms
            .relative_transform(&scene, query.skeleton_path(), root)
            .map_or(gf::IDENTITY, |relative| relative.matrix);
        bound.union_with(joints_extent(&pose, padding, Some(&placement)));
    }
    Ok(bound)
}

/// How far a skinned prim's bind-pose bound reaches past its skeleton's
/// rest pivots, on its worst axis, or `0` when it does not: its authored
/// `extent` at the earliest time, placed by its `geomBindTransform` at the
/// earliest time, against the pivots of `rest` (skeleton-space rest
/// transforms). A prim without a two-element `extent` adds no padding.
///
/// OpenUSD: `UsdSkelSkinningQuery::ComputeExtentsPadding`.
fn extents_padding(scene: &Scene<'_>, root: PathId, target: PathId, rest: &[gf::Matrix4]) -> f32 {
    let earliest = Time::held(f64::MIN);
    let prim = PrimView::new(*scene, target);
    let Some(extent) = prim
        .read_value_at("extent", earliest, crate::value::read_float3_array)
        .filter(|extent| extent.len() == 2)
    else {
        return 0.;
    };
    let geom_bind =
        inherited_property(scene, target, "primvars:skel:geomBindTransform", Some(root))
            .and_then(|source| {
                PrimView::new(*scene, source).read_value_at(
                    "primvars:skel:geomBindTransform",
                    earliest,
                    crate::value::read_matrix4d,
                )
            })
            .unwrap_or(gf::IDENTITY);
    let joints = joints_extent(rest, 0., None);
    let geometry = BoundingBox {
        range: Range3d {
            min: extent[0].map(f64::from),
            max: extent[1].map(f64::from),
        },
        matrix: geom_bind,
    }
    .aligned_range();
    let mut padding = 0_f32;
    for axis in 0..3 {
        padding = padding.max(f32_of(joints.min[axis]) - f32_of(geometry.min[axis]));
        padding = padding.max(f32_of(geometry.max[axis]) - f32_of(joints.max[axis]));
    }
    padding
}

/// The union of each transform's pivot (its translation, as `GfVec3f`),
/// placed by `placement` when given, grown by `padding` on every side.
///
/// OpenUSD: `UsdSkelComputeJointsExtent`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "UsdSkelComputeJointsExtent accumulates a GfRange3f"
)]
fn joints_extent(
    transforms: &[gf::Matrix4],
    padding: f32,
    placement: Option<&gf::Matrix4>,
) -> Range3d {
    let mut min = [f32::MAX; 3];
    let mut max = [-f32::MAX; 3];
    for transform in transforms {
        let pivot = [0, 1, 2].map(|axis| transform[3][axis] as f32);
        let pivot = match placement {
            Some(m) => [0, 1, 2].map(|axis| {
                (f64::from(pivot[0]) * m[0][axis]
                    + f64::from(pivot[1]) * m[1][axis]
                    + f64::from(pivot[2]) * m[2][axis]
                    + m[3][axis]) as f32
            }),
            None => pivot,
        };
        for axis in 0..3 {
            min[axis] = min[axis].min(pivot[axis]);
            max[axis] = max[axis].max(pivot[axis]);
        }
    }
    if transforms.is_empty() {
        return Range3d::default();
    }
    Range3d {
        min: min.map(|v| f64::from(v - padding)),
        max: max.map(|v| f64::from(v + padding)),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "C++ compares padding as GfVec3f"
)]
fn f32_of(value: f64) -> f32 {
    value as f32
}

/// The skeleton's query, or `None` when its definition is invalid, which
/// C++ treats as no extent. A retained array that fails to decode is an
/// error, not an absent input.
fn query<'a>(
    context: &mut ExtentContext<'a>,
    skeleton: Skeleton<'a>,
) -> Result<Option<SkeletonQuery<'a>>, ExtentError> {
    context.track_prim(skeleton.path());
    match skeleton.query() {
        Ok(query) => {
            context.track_animation(&query);
            Ok(Some(query))
        }
        Err(error) => decode_only(error).map(|()| None),
    }
}

/// The skeleton-space joint transforms at the requested time, or `None`
/// when the skeleton cannot be posed.
fn pose(
    context: &ExtentContext<'_>,
    query: &SkeletonQuery<'_>,
) -> Result<Option<Vec<gf::Matrix4>>, ExtentError> {
    match query.skeleton_transforms(context.time()) {
        Ok(pose) => Ok(Some(pose)),
        Err(error) => decode_only(error).map(|()| None),
    }
}

fn decode_only(error: SkelError) -> Result<(), ExtentError> {
    match error {
        SkelError::Decode { property, error } => Err(ExtentError::Decode { property, error }),
        _ => Ok(()),
    }
}

/// Active, defined, non-abstract Skeleton descendants of `root`, through
/// native instances, in namespace order.
fn descendant_skeletons(scene: &Scene<'_>, root: PathId) -> Vec<PathId> {
    let mut found = Vec::new();
    let mut pending = alloc::vec![root];
    while let Some(path) = pending.pop() {
        let Some(prim) = scene.prim(path) else {
            continue;
        };
        if prim.specifier() != layerstack::Specifier::Def || prim.is_abstract() {
            continue;
        }
        if path != root && scene.is_a(path, "Skeleton") {
            found.push(path);
        }
        pending.extend(prim.child_paths().iter().rev().copied());
    }
    found
}

impl ExtentContext<'_> {
    /// Records the skeleton's animation source and whether its pose varies.
    fn track_animation(&mut self, query: &SkeletonQuery<'_>) {
        if let Some(animation) = query.animation_path() {
            self.track_prim(animation);
            let view = PrimView::new(*self.scene(), animation);
            if ["translations", "rotations", "scales"]
                .iter()
                .any(|name| view.property_might_vary(name))
            {
                self.mark_time_varying();
            }
        }
    }
}
