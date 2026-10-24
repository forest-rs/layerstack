// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Baking skinning into layers: skinned and blend-shaped `points` and
//! `normals`, and rigid prims' transforms, written as plain time samples so
//! tools and engines without skinning read the posed geometry.
//!
//! OpenUSD: `UsdSkelBakeSkinning` (`pxr/usd/usdSkel/bakeSkinning.cpp`).
//! The bake samples the stage at the times OpenUSD does, deforms each
//! bound prim as `UsdSkelBakeSkinning`'s adapters do (blend shapes, then
//! skinning in skeleton space, then into the prim's space), writes the
//! results straight into each binding's layer, and retypes each `SkelRoot`
//! to an `Xform` there. It does not remove `skel:skeleton`,
//! `skel:animationSource` or `SkelBindingAPI`: OpenUSD keeps them, and
//! without a `SkelRoot` above them they no longer deform anything.
use super::{
    BlendShapeQuery, SkelError, SkelProblem, SkeletonQuery, SkinningBindingInputs, SkinningMethod,
    SkinningQuery, normals,
};
use crate::{ExtentsHintError, edit::CannotEdit};
use crate::{
    PrimView, Scene, SchemaEdit, Time, XformCache,
    bounds::{BoundsCache, BoundsError, BoundsOptions, Extent},
    extent::ExtentError,
    gf,
    primvar::Primvar,
    usd_geom::{GeomModelApiEdit, ImageablePurpose, Xformable},
    usd_skel::{SkelAnimation, SkelRoot, Skeleton},
};
use alloc::{string::String, sync::Arc, vec, vec::Vec};
use core::ops::{Bound, RangeBounds};
use hashbrown::HashMap;
use layerstack::edit::{Address, EditTarget};
use layerstack::{
    Applied, EditError, LayerId, LayerStore, LiveStage, PathId, PrimPredicate, PropertyPath,
    PropertySpec, PropertyType, TokenId, Transaction, TypedArray, Value, Variability,
};

/// Which deformations a skinning bake writes.
///
/// OpenUSD: `UsdSkelBakeSkinningParms::DeformationFlags`; the default is
/// [`Self::ALL`], OpenUSD's `DeformAll`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[doc(alias = "UsdSkelBakeSkinningParms::DeformationFlags")]
pub struct BakeDeformations {
    /// Skin `points` with joint influences (`DeformPointsWithSkinning`).
    pub points_with_skinning: bool,
    /// Skin `normals` with joint influences (`DeformNormalsWithSkinning`).
    pub normals_with_skinning: bool,
    /// Bake a rigidly bound prim's skinning into its transform, as
    /// `xformOp:transform` (`DeformXformWithSkinning`).
    pub xforms_with_skinning: bool,
    /// Apply blend shapes to `points` (`DeformPointsWithBlendShapes`).
    pub points_with_blend_shapes: bool,
    /// Apply blend shapes to vertex or varying `normals`
    /// (`DeformNormalsWithBlendShapes`).
    pub normals_with_blend_shapes: bool,
}

impl BakeDeformations {
    /// Every deformation (`DeformAll`).
    pub const ALL: Self = Self {
        points_with_skinning: true,
        normals_with_skinning: true,
        xforms_with_skinning: true,
        points_with_blend_shapes: true,
        normals_with_blend_shapes: true,
    };
    /// No deformation.
    pub const NONE: Self = Self {
        points_with_skinning: false,
        normals_with_skinning: false,
        xforms_with_skinning: false,
        points_with_blend_shapes: false,
        normals_with_blend_shapes: false,
    };
    /// Joint skinning of points, normals and transforms (`DeformWithSkinning`).
    pub const SKINNING: Self = Self {
        points_with_blend_shapes: false,
        normals_with_blend_shapes: false,
        ..Self::ALL
    };
    /// Blend shapes on points and normals (`DeformWithBlendShapes`).
    pub const BLEND_SHAPES: Self = Self {
        points_with_skinning: false,
        normals_with_skinning: false,
        xforms_with_skinning: false,
        ..Self::ALL
    };

    /// Whether any deformation is set.
    #[must_use]
    pub fn any(self) -> bool {
        self != Self::NONE
    }

    fn skinning(self) -> bool {
        self.points_with_skinning || self.normals_with_skinning || self.xforms_with_skinning
    }

    fn blend_shapes(self) -> bool {
        self.points_with_blend_shapes || self.normals_with_blend_shapes
    }

    fn points(self) -> bool {
        self.points_with_skinning || self.points_with_blend_shapes
    }

    fn normals(self) -> bool {
        self.normals_with_skinning || self.normals_with_blend_shapes
    }
}

impl Default for BakeDeformations {
    fn default() -> Self {
        Self::ALL
    }
}

/// One skeleton's bindings under a `SkelRoot`, and the layer the bake
/// writes their deformed values to.
///
/// OpenUSD: one `UsdSkelBinding` of `UsdSkelBakeSkinningParms::bindings`,
/// with its entry of `layers` and `layerIndices`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BakeBinding {
    /// The `SkelRoot` whose bindings are baked.
    pub root: PathId,
    /// Only the prims bound to this skeleton, or every skeleton's (`None`).
    pub skeleton: Option<PathId>,
    /// The layer the baked values, and the root's new `Xform` type, are
    /// written to, at the prims' own paths and with stage times as layer
    /// times. It should be in the stage's layer stack, as OpenUSD requires,
    /// or the bake does not change what the stage composes.
    pub layer: LayerId,
}

/// A skinning bake: which bindings, which deformations, over which times,
/// and where extents go. Run it with [`Self::bake`].
///
/// OpenUSD: `UsdSkelBakeSkinningParms` and `UsdSkelBakeSkinning`. OpenUSD's
/// `saveLayers` and `memoryLimit` have no counterpart: the bake changes
/// layers in memory and reports them ([`SkinningBakeReport::layers`]);
/// saving them is the caller's step, through its host's save path (for
/// example `layerstack_io`).
#[derive(Clone, Debug, PartialEq)]
#[doc(alias = "UsdSkelBakeSkinningParms")]
pub struct SkinningBake {
    /// The bindings to bake, each with its layer.
    pub bindings: Vec<BakeBinding>,
    /// The deformations to write.
    pub deformations: BakeDeformations,
    /// The stage times to bake: the time samples within it, and the stage's
    /// playback frames within it (OpenUSD's `GfInterval`; every time by
    /// default).
    pub interval: (Bound<f64>, Bound<f64>),
    /// Recompute `extent` from the deformed points (`updateExtents`): a
    /// `Mesh`'s is written with its points, another point-based prim's is
    /// computed after the bake, as `UsdGeomBoundable::ComputeExtentFromPlugins`
    /// does, and authored through [`Self::extents_target`].
    pub update_extents: bool,
    /// Recompute the `extentsHint` of every model at or above a baked prim
    /// that has one (`updateExtentHints`), through [`Self::extents_target`].
    pub update_extents_hints: bool,
    /// Where extents recomputed after the bake, and extents hints, are
    /// authored: OpenUSD authors them through the stage's edit target.
    pub extents_target: EditTarget,
    /// The most threads the deformation runs on, the calling thread
    /// included. `0` and `1`, the default, deform on the calling thread;
    /// above that, samples (one prim at one time each) deform on scoped
    /// worker threads that last as long as the bake, with this crate's
    /// `std` feature (without it, on the calling thread). Reading the stage
    /// and writing the results stay on the calling thread, so the bake
    /// writes the same values in the same order for every count. OpenUSD
    /// deforms on its work dispatcher's threads.
    pub threads: usize,
}

impl SkinningBake {
    /// A bake of no bindings yet, writing every deformation over every time,
    /// with extents and extents hints authored through `target`.
    #[must_use]
    pub fn new(target: EditTarget) -> Self {
        Self {
            bindings: Vec::new(),
            deformations: BakeDeformations::ALL,
            interval: (Bound::Unbounded, Bound::Unbounded),
            update_extents: true,
            update_extents_hints: true,
            extents_target: target,
            threads: 1,
        }
    }

    /// Every binding under `root`, written to `target`'s layer.
    ///
    /// OpenUSD: `UsdSkelBakeSkinning(const UsdSkelRoot&, interval)`, which
    /// bakes into the stage's edit target.
    #[must_use]
    pub fn for_root(root: PathId, target: EditTarget) -> Self {
        let layer = target.layer();
        let mut bake = Self::new(target);
        bake.bindings.push(BakeBinding {
            root,
            skeleton: None,
            layer,
        });
        bake
    }

    /// Every `SkelRoot` at or beneath `prim` in default traversal (active,
    /// loaded, defined, not abstract, not beneath an instance), each
    /// written to `target`'s layer.
    ///
    /// OpenUSD: `UsdSkelBakeSkinning(const UsdPrimRange&, interval)` over
    /// `UsdPrimRange(prim)`.
    #[must_use]
    pub fn for_subtree(scene: &Scene<'_>, prim: PathId, target: EditTarget) -> Self {
        let layer = target.layer();
        let mut bake = Self::new(target);
        bake.bindings = scene
            .stage()
            .prim_range(prim, scene.store(), PrimPredicate::DEFAULT)
            .filter(|path| scene.is_a(*path, "SkelRoot"))
            .map(|root| BakeBinding {
                root,
                skeleton: None,
                layer,
            })
            .collect();
        bake
    }

    /// This bake over the stage times in `interval` only.
    #[must_use]
    pub fn with_interval(mut self, interval: impl RangeBounds<f64>) -> Self {
        self.interval = (
            interval.start_bound().cloned(),
            interval.end_bound().cloned(),
        );
        self
    }

    /// This bake deforming on up to `threads` threads ([`Self::threads`]).
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads;
        self
    }

    /// This bake writing only `deformations`.
    #[must_use]
    pub fn with_deformations(mut self, deformations: BakeDeformations) -> Self {
        self.deformations = deformations;
        self
    }
}

/// Why a bake changed nothing.
#[derive(Clone, Debug, PartialEq)]
pub enum SkinningBakeError {
    /// A binding's root is not a `SkelRoot` on the stage.
    NotSkelRoot(PathId),
    /// A binding's root, or a prim it binds, is a native instance or an
    /// instance proxy, which OpenUSD does not bake ("not supported for
    /// instanced").
    Instanced(PathId),
    /// A binding's layer is not in the store.
    MissingLayer(LayerId),
    /// Applying the baked values failed; nothing was applied.
    Edit(EditError),
}

impl core::fmt::Display for SkinningBakeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotSkelRoot(_) => f.write_str("a bake binding's root is not a SkelRoot"),
            Self::Instanced(_) => f.write_str("instanced skinning is not baked"),
            Self::MissingLayer(layer) => write!(f, "layer {layer:?} is not in the store"),
            Self::Edit(error) => write!(f, "baked values did not apply: {error}"),
        }
    }
}

impl core::error::Error for SkinningBakeError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Edit(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EditError> for SkinningBakeError {
    fn from(error: EditError) -> Self {
        Self::Edit(error)
    }
}

/// What a bake wrote for one bound prim.
#[derive(Clone, Debug, PartialEq)]
pub struct BakedPrim {
    /// The prim.
    pub prim: PathId,
    /// The skeleton it is bound to.
    pub skeleton: PathId,
    /// The layer its values were written to.
    pub layer: LayerId,
    /// The deformations baked for it: points or normals for a skinned or
    /// blend-shaped prim, a transform for a rigid one.
    pub deformations: BakeDeformations,
    /// Whether its `extent` was written with its points (a `Mesh`).
    pub extent: bool,
    /// The times it was sampled at, in order: [`Time::Default`] alone for a
    /// skeleton with no animation, else time codes. A sample that failed is
    /// left out of its attribute and reported in
    /// [`SkinningBakeReport::problems`].
    pub times: Vec<Time>,
}

/// Something a bake skipped, where OpenUSD warns and goes on.
#[derive(Clone, Debug, PartialEq)]
pub struct BakeProblem {
    /// The prim it concerns.
    pub prim: PathId,
    /// The time it happened at, for a failed sample.
    pub time: Option<Time>,
    /// What went wrong, and what was left out.
    pub reason: BakeProblemReason,
}

/// What a [`BakeProblem`] left out.
#[derive(Clone, Debug, PartialEq)]
pub enum BakeProblemReason {
    /// Skel discovery skipped or left out an input ([`SkelProblem`]).
    Discovery(SkelProblem),
    /// The prim's skeleton cannot be queried; none of its prims is baked.
    Skeleton(SkelError),
    /// Evaluating a deformation failed: that sample is not written.
    Evaluation(SkelError),
    /// The layer already has the attribute `name` with another type, so
    /// the bake does not write it (OpenUSD: "Spec type mismatch").
    AttributeType {
        /// The attribute.
        name: &'static str,
        /// Its existing type.
        existing: String,
    },
    /// A recomputed extent failed: that sample is not written.
    Extent(ExtentError),
    /// A model's extents hint failed to compute: that sample is not written.
    Bounds(BoundsError),
    /// A model's extents hint could not be authored.
    ExtentsHint(ExtentsHintError),
    /// The extents target may not author the prim.
    CannotEdit(CannotEdit),
}

/// A `SkelRoot` a bake retyped to `Xform`.
///
/// OpenUSD: `_ConvertSkelRootsToXforms`, which gives the root's spec in the
/// binding's layer the `Xform` type and the `def` specifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetypedRoot {
    /// The root.
    pub root: PathId,
    /// The layer it was retyped in.
    pub layer: LayerId,
}

/// What a bake did: what it wrote where, at which times, what it skipped,
/// and the applied transactions.
#[derive(Debug)]
pub struct SkinningBakeReport {
    /// Every time the bake considered, in order: [`Time::Default`], then
    /// each time code. Each prim was sampled at some of them
    /// ([`BakedPrim::times`]).
    pub times: Vec<Time>,
    /// Every bound prim something was baked for, in binding order.
    pub prims: Vec<BakedPrim>,
    /// Bound prims nothing was baked for: no deformation applies to them
    /// under the bake's [`BakeDeformations`], such as a rigid prim when
    /// transforms are not baked, a prim with no authored `points` or
    /// `normals`, or one whose skeleton has no animation or rest pose.
    pub unbaked: Vec<PathId>,
    /// Each `SkelRoot` retyped to `Xform`, with the layer it was retyped in.
    pub retyped_roots: Vec<RetypedRoot>,
    /// Prims whose `extent` was recomputed after the bake.
    pub extents: Vec<PathId>,
    /// Models whose `extentsHint` was recomputed.
    pub extents_hints: Vec<PathId>,
    /// What was skipped, in the order it happened.
    pub problems: Vec<BakeProblem>,
    /// Every layer the bake changed, in first-written order: save these to
    /// keep the bake.
    pub layers: Vec<LayerId>,
    /// The transactions applied, in order: the baked values, then
    /// recomputed extents, then extents hints, each present only when it
    /// changed something. Pass each one's `changes` to retained caches.
    pub applied: Vec<Applied>,
}

impl SkinningBakeReport {
    /// A transaction that undoes the whole bake: the applied transactions'
    /// inverses, last first.
    #[must_use]
    pub fn undo(&self) -> Transaction {
        let mut undo = Transaction::new();
        for applied in self.applied.iter().rev() {
            undo.append(applied.inverse.clone());
        }
        undo
    }
}

// ------------------------------------------------------------------------
// The skeleton side: OpenUSD's `_SkelAdapter`.

struct Rig<'a> {
    path: PathId,
    query: SkeletonQuery<'a>,
    animation: Option<SkelAnimation<'a>>,
    /// Anim blend shape names, in its weight order.
    blend_names: Vec<String>,
    /// `_skinningXformsTask.IsActive()`: a mapped animation or authored rest
    /// transforms.
    can_skin: bool,
    /// `_blendShapeWeightsTask.IsActive()`: authored weights.
    can_blend: bool,
    skinning_required: bool,
    blend_required: bool,
    /// Indices into the bake's times this skeleton is sampled at.
    mask: Vec<bool>,
    transforms: Option<Arc<Vec<gf::Matrix4>>>,
    weights: Option<Vec<f32>>,
    world: Option<gf::Matrix4>,
}

impl Rig<'_> {
    fn has_tasks(&self) -> bool {
        (self.can_skin && self.skinning_required) || (self.can_blend && self.blend_required)
    }
}

// ------------------------------------------------------------------------
// The prim side: OpenUSD's `_SkinningAdapter`.

#[derive(Clone, Copy, PartialEq, Eq)]
enum NormalsSource {
    None,
    Vertex,
    FaceVarying,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParentSpace {
    /// The transform is not baked.
    Unused,
    /// The prim resets the transform stack: its parent space is the
    /// identity.
    Identity,
    /// Read from the transform cache.
    Computed,
}

/// One attribute the bake writes, with the samples its layer already has.
struct Writer {
    property: PropertyPath,
    /// Layer sample times on the existing spec, which OpenUSD clears.
    stale: Vec<f64>,
    written: Vec<f64>,
}

struct Target<'a> {
    path: PathId,
    rig: usize,
    layer: LayerId,
    flags: BakeDeformations,
    skinning: Option<SkinningQuery<'a>>,
    shapes: Option<Arc<BlendShapeQuery>>,
    /// Each local blend shape's index in the animation's weights.
    shape_map: Vec<Option<usize>>,
    normals: NormalsSource,
    local_to_world: bool,
    parent: ParentSpace,
    /// Cached binding inputs, when they cannot vary.
    inputs: Option<Arc<SkinningBindingInputs>>,
    inputs_vary: bool,
    points: Option<Writer>,
    normal_writer: Option<Writer>,
    extent: Option<Writer>,
    xform: Option<Writer>,
    times: Vec<Time>,
}

impl Target<'_> {
    fn requires_post_extent(&self) -> bool {
        self.flags.points() && self.extent.is_none()
    }
}

struct Names {
    points: TokenId,
    normals: TokenId,
    extent: TokenId,
    transform: TokenId,
    xform_op_order: TokenId,
    xform: TokenId,
}

impl BlendShapeQuery {
    fn has_offsets(&self, normals: bool) -> bool {
        (0..self.sample_count()).any(|shape| {
            self.samples(shape).is_some_and(|mut samples| {
                samples.any(|s| !(if normals { s.normal_offsets } else { s.offsets }).is_empty())
            })
        })
    }
}

impl SkinningBake {
    /// Bakes the bindings into their layers, applies the result to `live`,
    /// then recomputes extents and extents hints and applies those.
    ///
    /// The bake samples, for each skeleton, the time samples within
    /// [`Self::interval`] of its animation, its own transform and its
    /// bound prims' inputs (`points`, `normals`, influences, geometry
    /// bind transform and transforms), plus the stage's playback frames
    /// (`startTimeCode` to one frame past `endTimeCode`, a frame being
    /// `timeCodesPerSecond / framesPerSecond`) between its first and last
    /// sample. A skeleton with no such samples is baked at the default
    /// time, overwriting the defaults. Each sampled attribute's existing
    /// time samples in the layer are replaced. As in OpenUSD, only a
    /// prim's own transform samples are gathered, not its ancestors', and a
    /// rigid prim's `xformOpOrder` becomes `["xformOp:transform"]`.
    ///
    /// OpenUSD: `UsdSkelBakeSkinning(const UsdSkelCache&, const
    /// UsdSkelBakeSkinningParms&, const GfInterval&)`.
    ///
    /// # Errors
    /// [`SkinningBakeError`] before anything is written for a binding whose
    /// root is not a `SkelRoot`, is instanced or binds an instance, or
    /// whose layer is missing; and when a transaction fails to apply, after
    /// which the transactions applied before it stay applied (undo them with
    /// their inverses).
    #[doc(alias = "UsdSkelBakeSkinning")]
    pub fn bake(
        &self,
        live: &mut LiveStage,
        store: &mut dyn LayerStore,
    ) -> Result<SkinningBakeReport, SkinningBakeError> {
        let names = {
            let tokens = store.tokens_mut();
            Names {
                points: tokens.intern("points"),
                normals: tokens.intern("normals"),
                extent: tokens.intern("extent"),
                transform: tokens.intern("xformOp:transform"),
                xform_op_order: tokens.intern("xformOpOrder"),
                xform: tokens.intern("Xform"),
            }
        };
        let mut report = SkinningBakeReport {
            times: Vec::new(),
            prims: Vec::new(),
            unbaked: Vec::new(),
            retyped_roots: Vec::new(),
            extents: Vec::new(),
            extents_hints: Vec::new(),
            problems: Vec::new(),
            layers: Vec::new(),
            applied: Vec::new(),
        };
        let (transaction, post) = {
            let scene = Scene::new(live.stage(), &*store);
            self.deform(&scene, &names, &mut report)?
        };
        if !transaction.is_empty() {
            report.applied.push(live.apply(store, &transaction)?);
        }
        if self.update_extents && !post.extents.is_empty() {
            let transaction = self.post_extents(live, store, &post, &mut report);
            if !transaction.is_empty() {
                note_layer(&mut report.layers, self.extents_target.layer());
                report.applied.push(live.apply(store, &transaction)?);
            }
        }
        if self.update_extents_hints && !post.hints.is_empty() {
            let transaction = self.extents_hints(live, store, &post, &mut report);
            if !transaction.is_empty() {
                note_layer(&mut report.layers, self.extents_target.layer());
                report.applied.push(live.apply(store, &transaction)?);
            }
        }
        Ok(report)
    }
}

/// What the bake recomputes after its values are applied.
struct PostBake {
    times: Vec<Time>,
    /// Point-based prims whose extent is recomputed.
    extents: Vec<SampledPrim>,
    /// Models with an `extentsHint`, at the times any of their baked prims
    /// was sampled at.
    hints: Vec<SampledPrim>,
}

/// A prim, and the indices of the bake's times it was sampled at.
struct SampledPrim {
    prim: PathId,
    indices: Vec<usize>,
}

/// A value recomputed after the bake, at one time.
struct Recomputed<T> {
    time: Time,
    value: T,
}

fn note_layer(layers: &mut Vec<LayerId>, layer: LayerId) {
    if !layers.contains(&layer) {
        layers.push(layer);
    }
}

fn contains(interval: &(Bound<f64>, Bound<f64>), time: f64) -> bool {
    interval.contains(&time)
}

fn sorted_unique(mut times: Vec<f64>) -> Vec<f64> {
    times.sort_by(f64::total_cmp);
    times.dedup();
    times
}

/// The stage's playback time codes within `interval`.
///
/// OpenUSD: `_GetStagePlaybackTimeCodesInRange`, which samples one frame
/// past `endTimeCode` (its loop runs to `numTimeSamples` inclusive).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "OpenUSD counts frames with an int64 truncation"
)]
fn playback_times(scene: &Scene<'_>, interval: &(Bound<f64>, Bound<f64>)) -> Vec<f64> {
    let (stage, store) = (scene.stage(), scene.store());
    if !stage.has_authored_time_code_range(store) {
        return Vec::new();
    }
    let codes = stage.time_codes_per_second(store);
    let frames = stage.frames_per_second(store);
    if codes.abs() <= 1e-6 || frames.abs() <= 1e-6 {
        return Vec::new();
    }
    let step = (codes / frames).abs();
    let (start, end) = (stage.start_time_code(store), stage.end_time_code(store));
    if end < start {
        return Vec::new();
    }
    let count = ((end - start) / step + 1.0) as i64;
    (0..=count)
        .map(|i| start + step * i as f64)
        .filter(|t| contains(interval, *t))
        .collect()
}

/// The time samples of the prim's own transform within `interval`.
///
/// OpenUSD: `_ExtendWorldTransformTimeSamples`, whose walk up the
/// ancestors queries the starting prim at each step, so only that prim's
/// samples count.
fn own_transform_times(
    scene: &Scene<'_>,
    path: PathId,
    interval: &(Bound<f64>, Bound<f64>),
) -> Vec<f64> {
    Xformable::new(scene, path)
        .map(|xformable| xformable.transform_time_samples_in_range(*interval))
        .unwrap_or_default()
}

fn attribute_times(
    scene: &Scene<'_>,
    path: PathId,
    name: &str,
    interval: &(Bound<f64>, Bound<f64>),
) -> Vec<f64> {
    PrimView::new(*scene, path)
        .attribute(name)
        .map(|attribute| attribute.sample_times_in_range(*interval))
        .unwrap_or_default()
}

/// `GfVec3f::Normalize`: a float length, floored at `1e-10`.
fn normalize(vectors: &mut [[f32; 3]]) {
    for v in vectors {
        let length = libm::sqrtf(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]);
        let divisor = if length > 1e-10 { length } else { 1e-10 };
        *v = v.map(|x| x / divisor);
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenUSD rounds transformed normals to GfVec3f"
)]
fn transform_normal(n: [f32; 3], m: &[[f64; 3]; 3]) -> [f32; 3] {
    core::array::from_fn(|j| {
        (f64::from(n[0]) * m[0][j] + f64::from(n[1]) * m[1][j] + f64::from(n[2]) * m[2][j]) as f32
    })
}

/// `UsdGeomPointBased::ComputeExtent`.
fn points_extent(points: &[[f32; 3]]) -> [[f32; 3]; 2] {
    let mut extent = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
    for point in points {
        for axis in 0..3 {
            extent[0][axis] = extent[0][axis].min(point[axis]);
            extent[1][axis] = extent[1][axis].max(point[axis]);
        }
    }
    extent
}

fn vec3f_array(values: Vec<[f32; 3]>) -> Value {
    Value::TypedArray(TypedArray::Vec3f(Arc::new(values)))
}

fn matrix_value(m: &gf::Matrix4) -> Value {
    let mut flat = [0.0; 16];
    for (i, row) in m.iter().enumerate() {
        flat[i * 4..i * 4 + 4].copy_from_slice(row);
    }
    Value::Matrix4d(alloc::boxed::Box::new(flat))
}

impl SkinningBake {
    /// Discovers, samples and deforms every binding, returning the
    /// transaction of baked values and what to recompute after it applies.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over OpenUSD's adapter setup, sampling and writing"
    )]
    fn deform<'a>(
        &self,
        scene: &Scene<'a>,
        names: &Names,
        report: &mut SkinningBakeReport,
    ) -> Result<(Transaction, PostBake), SkinningBakeError> {
        let mut rigs: Vec<Rig<'a>> = Vec::new();
        let mut rig_index: HashMap<PathId, Option<usize>> = HashMap::new();
        let mut targets: Vec<Target<'a>> = Vec::new();
        let mut transaction = Transaction::new();
        let mut roots: Vec<RetypedRoot> = Vec::new();
        let deformations = self.deformations;
        // Validate every binding before anything is written.
        let mut discovered = Vec::with_capacity(self.bindings.len());
        for binding in &self.bindings {
            let prim = scene
                .prim(binding.root)
                .ok_or(SkinningBakeError::NotSkelRoot(binding.root))?;
            let root = SkelRoot::new(scene, binding.root)
                .ok_or(SkinningBakeError::NotSkelRoot(binding.root))?;
            if prim.is_instance() || prim.is_instance_proxy() {
                return Err(SkinningBakeError::Instanced(binding.root));
            }
            if scene.store().layer(binding.layer).is_none() {
                return Err(SkinningBakeError::MissingLayer(binding.layer));
            }
            let found = root.bindings();
            for bound in &found.bindings {
                if scene.prim(bound.geometry).is_some_and(|p| p.is_instance()) {
                    return Err(SkinningBakeError::Instanced(bound.geometry));
                }
            }
            discovered.push(found);
        }
        for (binding, found) in self.bindings.iter().zip(discovered) {
            report
                .problems
                .extend(found.problems().map(|problem| BakeProblem {
                    prim: problem.prim,
                    time: None,
                    reason: BakeProblemReason::Discovery(problem.clone()),
                }));
            let mut converted: Vec<PathId> = Vec::new();
            for bound in found.bindings {
                if binding.skeleton.is_some_and(|s| s != bound.skeleton) {
                    continue;
                }
                // OpenUSD: `_ConvertSkelRootsToXforms` retypes the root
                // above each binding's first target, whatever was baked.
                if !converted.contains(&bound.skeleton) {
                    converted.push(bound.skeleton);
                    if let Some(root) = nearest_skel_root(scene, bound.geometry) {
                        let retyped = RetypedRoot {
                            root,
                            layer: binding.layer,
                        };
                        if !roots.contains(&retyped) {
                            roots.push(retyped);
                        }
                    }
                }
                let rig =
                    *rig_index.entry(bound.skeleton).or_insert_with(|| {
                        match Skeleton::new(scene, bound.skeleton)
                            .expect("a bound skeleton")
                            .query()
                        {
                            Ok(query) => {
                                rigs.push(Rig::new(scene, bound.skeleton, query, deformations));
                                Some(rigs.len() - 1)
                            }
                            Err(error) => {
                                report.problems.push(BakeProblem {
                                    prim: bound.skeleton,
                                    time: None,
                                    reason: BakeProblemReason::Skeleton(error),
                                });
                                None
                            }
                        }
                    });
                let Some(rig) = rig else {
                    report.unbaked.push(bound.geometry);
                    continue;
                };
                match Target::new(
                    scene,
                    bound.geometry,
                    rig,
                    &mut rigs[rig],
                    binding.layer,
                    deformations,
                    bound.skinning,
                    bound.blend_shapes,
                    self.update_extents,
                    names,
                    &mut transaction,
                    report,
                ) {
                    Some(target) => targets.push(target),
                    None => report.unbaked.push(bound.geometry),
                }
            }
        }
        // Retype the roots, as `_ConvertSkelRootsToXforms` does.
        for retyped in &roots {
            let at = EditTarget::for_layer(retyped.layer).prim(retyped.root);
            transaction.set_type_name(at.clone(), names.xform);
            transaction.set_specifier(at, layerstack::Specifier::Def);
            note_layer(&mut report.layers, retyped.layer);
        }
        report.retyped_roots = roots;
        rigs.iter_mut().for_each(|rig| {
            if !rig.has_tasks() {
                rig.mask.clear();
            }
        });
        let times = self.times(scene, &mut rigs, &targets);
        report.times.clone_from(&times);
        // Read every sample's inputs in order, deform them on up to
        // `threads` threads, then write them in order: the result does not
        // depend on the thread count.
        let mut xforms = XformCache::new(Time::Default);
        let mut jobs = Vec::new();
        for (index, &time) in times.iter().enumerate() {
            xforms.set_time(time);
            for rig in &mut rigs {
                rig.sample(scene, index, time, &mut xforms, report);
            }
            for (number, target) in targets.iter_mut().enumerate() {
                let rig = &rigs[target.rig];
                if rig.mask.get(index).copied().unwrap_or(false) {
                    jobs.push(target.gather(number, scene, rig, time, &mut xforms, report));
                }
            }
        }
        for (number, output) in run_all(jobs, self.threads) {
            targets[number].write(output, &mut transaction, report);
        }
        let mut post = PostBake {
            times,
            extents: Vec::new(),
            hints: Vec::new(),
        };
        let mut models: Vec<SampledPrim> = Vec::new();
        let pseudo_root = scene.root();
        for target in &mut targets {
            let rig = &rigs[target.rig];
            let processed: Vec<usize> = (0..post.times.len())
                .filter(|&i| rig.mask.get(i).copied().unwrap_or(false))
                .collect();
            if target.requires_post_extent() {
                post.extents.push(SampledPrim {
                    prim: target.path,
                    indices: processed.clone(),
                });
            }
            // OpenUSD `_UpdateExtentHints`: every model at or above the
            // prim with an `extentsHint` attribute.
            let mut at = Some(target.path).filter(|path| Some(*path) != pseudo_root);
            while let Some(path) = at {
                if scene.is_model(path)
                    && PrimView::new(*scene, path)
                        .attribute("extentsHint")
                        .is_some()
                {
                    match models.iter_mut().find(|model| model.prim == path) {
                        Some(model) => model.indices.extend(&processed),
                        None => models.push(SampledPrim {
                            prim: path,
                            indices: processed.clone(),
                        }),
                    }
                }
                at = scene
                    .parent(path)
                    .filter(|parent| Some(*parent) != pseudo_root);
            }
            for writer in [
                &mut target.points,
                &mut target.normal_writer,
                &mut target.extent,
                &mut target.xform,
            ]
            .into_iter()
            .flatten()
            {
                let address = EditTarget::for_layer(target.layer).property(writer.property);
                for &time in &writer.stale {
                    if !writer.written.contains(&time) {
                        transaction.remove_time_sample(address.clone(), time);
                    }
                }
            }
            note_layer(&mut report.layers, target.layer);
            report.prims.push(BakedPrim {
                prim: target.path,
                skeleton: rigs[target.rig].path,
                layer: target.layer,
                deformations: target.flags,
                extent: target.extent.is_some(),
                times: core::mem::take(&mut target.times),
            });
        }
        for model in &mut models {
            model.indices.sort_unstable();
            model.indices.dedup();
        }
        post.hints = models;
        Ok((transaction, post))
    }

    /// The bake's times, and each skeleton's mask of them.
    ///
    /// OpenUSD: `_ComputeTimeSamples`.
    fn times(&self, scene: &Scene<'_>, rigs: &mut [Rig<'_>], targets: &[Target<'_>]) -> Vec<Time> {
        let interval = &self.interval;
        let mut rig_times: Vec<Vec<f64>> = rigs
            .iter()
            .map(|rig| {
                let mut times = Vec::new();
                if !rig.has_tasks() {
                    return times;
                }
                if rig.can_skin && rig.skinning_required {
                    if let Some(animation) = &rig.animation {
                        times.extend(animation.joint_transform_time_samples_in_range(*interval));
                    }
                    times.extend(own_transform_times(scene, rig.path, interval));
                }
                if rig.can_blend
                    && rig.blend_required
                    && let Some(animation) = &rig.animation
                {
                    times.extend(animation.blend_shape_weight_time_samples_in_range(*interval));
                }
                times
            })
            .collect();
        for target in targets {
            target.extend_times(scene, interval, &mut rig_times[target.rig]);
        }
        let rig_times: Vec<Vec<f64>> = rig_times.into_iter().map(sorted_unique).collect();
        let stage_times = playback_times(scene, interval);
        let mut all: Vec<f64> = stage_times.clone();
        for times in &rig_times {
            all.extend(times);
        }
        let all = sorted_unique(all);
        let index = |t: f64| 1 + all.partition_point(|x| *x < t);
        for (rig, times) in rigs.iter_mut().zip(&rig_times) {
            if !rig.has_tasks() {
                continue;
            }
            let mut mask = vec![false; all.len() + 1];
            if times.is_empty() {
                mask[0] = true;
            } else {
                for &t in times {
                    mask[index(t)] = true;
                }
                if times.len() > 1 {
                    let (first, last) = (times[0], times[times.len() - 1]);
                    for &t in stage_times.iter().filter(|t| **t >= first && **t <= last) {
                        mask[index(t)] = true;
                    }
                }
            }
            rig.mask = mask;
        }
        core::iter::once(Time::Default)
            .chain(all.into_iter().map(Time::at))
            .collect()
    }
}

/// The nearest `SkelRoot` at or above `path`. OpenUSD: `UsdSkelRoot::Find`.
fn nearest_skel_root(scene: &Scene<'_>, path: PathId) -> Option<PathId> {
    let mut at = Some(path);
    while let Some(path) = at {
        if scene.is_a(path, "SkelRoot") {
            return Some(path);
        }
        at = scene.parent(path);
    }
    None
}

impl<'a> Rig<'a> {
    fn new(
        scene: &Scene<'a>,
        path: PathId,
        query: SkeletonQuery<'a>,
        deformations: BakeDeformations,
    ) -> Self {
        let animation = query.animation;
        let mapped = query.definition.mapping.iter().any(Option::is_some);
        let rest = PrimView::new(*scene, path).has_authored_value("restTransforms");
        let can_skin = deformations.skinning() && ((animation.is_some() && mapped) || rest);
        let can_blend = deformations.blend_shapes()
            && animation.is_some_and(|a| a.has_authored_value("blendShapeWeights"));
        Self {
            path,
            blend_names: query.definition.blend_shapes.clone(),
            query,
            animation,
            can_skin,
            can_blend,
            skinning_required: false,
            blend_required: false,
            mask: Vec::new(),
            transforms: None,
            weights: None,
            world: None,
        }
    }

    /// OpenUSD `_SkelAdapter::UpdateTransform` and `UpdateAnimation`.
    fn sample(
        &mut self,
        scene: &Scene<'_>,
        index: usize,
        time: Time,
        xforms: &mut XformCache,
        report: &mut SkinningBakeReport,
    ) {
        self.transforms = None;
        self.weights = None;
        self.world = None;
        if !self.mask.get(index).copied().unwrap_or(false) {
            return;
        }
        if self.can_skin && self.skinning_required {
            self.world = Some(
                xforms
                    .local_to_world(scene, self.path)
                    .unwrap_or(gf::IDENTITY),
            );
            match self.query.skinning_transforms(time) {
                Ok(transforms) => self.transforms = Some(Arc::new(transforms)),
                Err(error) => report.problems.push(BakeProblem {
                    prim: self.path,
                    time: Some(time),
                    reason: BakeProblemReason::Evaluation(error),
                }),
            }
        }
        if self.can_blend
            && self.blend_required
            && let Some(animation) = &self.animation
        {
            match animation.compute_blend_shape_weights(time) {
                Ok(weights) => self.weights = weights,
                Err(error) => report.problems.push(BakeProblem {
                    prim: animation.path(),
                    time: Some(time),
                    reason: BakeProblemReason::Evaluation(error),
                }),
            }
        }
    }
}

/// The type of an attribute spec the layer already has at `property`.
fn existing_spec(
    store: &dyn LayerStore,
    layer: LayerId,
    property: PropertyPath,
) -> Option<&PropertySpec> {
    let prim = store.layer(layer)?.prim(property.prim_path())?;
    prim.properties
        .iter()
        .find(|entry| entry.name == property.property())
        .map(|entry| &*entry.spec)
}

impl<'a> Target<'a> {
    /// OpenUSD `_SkinningAdapter`'s constructor: which deformations apply,
    /// and the attribute specs they write.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "OpenUSD's adapter setup, condition by condition"
    )]
    fn new(
        scene: &Scene<'a>,
        path: PathId,
        rig_index: usize,
        rig: &mut Rig<'a>,
        layer: LayerId,
        deformations: BakeDeformations,
        skinning: Option<SkinningQuery<'a>>,
        shapes: Option<BlendShapeQuery>,
        update_extents: bool,
        names: &Names,
        transaction: &mut Transaction,
        report: &mut SkinningBakeReport,
    ) -> Option<Self> {
        let prim = PrimView::new(*scene, path);
        let point_based = scene.is_a(path, "PointBased");
        let xformable = point_based || scene.is_a(path, "Xformable");
        let mesh = scene.is_a(path, "Mesh");
        let mut rest_points = false;
        let mut normals = NormalsSource::None;
        if point_based {
            if deformations.points() {
                rest_points = prim.has_authored_value("points");
            }
            if deformations.normals() {
                let interpolation = prim
                    .property_metadata("normals")
                    .and_then(|m| m.interpolation().map(String::from))
                    .unwrap_or_else(|| String::from("vertex"));
                let authored = prim.has_authored_value("normals");
                if interpolation == "faceVarying" && mesh {
                    if authored && prim.has_authored_value("faceVertexIndices") {
                        normals = NormalsSource::FaceVarying;
                    }
                } else if authored && matches!(interpolation.as_str(), "vertex" | "varying") {
                    normals = NormalsSource::Vertex;
                }
            }
        }
        let mut flags = BakeDeformations::NONE;
        if deformations.skinning()
            && let Some(query) = &skinning
        {
            if query.interpolation() == super::InfluenceInterpolation::Constant && xformable {
                flags.xforms_with_skinning = deformations.xforms_with_skinning && rig.can_skin;
            } else if point_based {
                flags.points_with_skinning =
                    deformations.points_with_skinning && rest_points && rig.can_skin;
                flags.normals_with_skinning = deformations.normals_with_skinning
                    && normals != NormalsSource::None
                    && rig.can_skin;
            }
        }
        let mut shape_map = Vec::new();
        let shapes = shapes.filter(|shapes| {
            if !(deformations.blend_shapes()
                && rig.can_blend
                && point_based
                && (rest_points || normals != NormalsSource::None))
            {
                return false;
            }
            flags.points_with_blend_shapes =
                deformations.points_with_blend_shapes && rest_points && shapes.has_offsets(false);
            // Face-varying normals take no blend shapes (OpenUSD: "Only
            // vertex/varying normals are supported for blendshapes").
            flags.normals_with_blend_shapes = deformations.normals_with_blend_shapes
                && normals == NormalsSource::Vertex
                && shapes.has_offsets(true);
            flags.blend_shapes()
        });
        let shapes = shapes.map(Arc::new);
        if let Some(shapes) = &shapes {
            shape_map = shapes
                .names()
                .iter()
                .map(|name| rig.blend_names.iter().position(|n| n == name))
                .collect();
        }
        if !flags.any() {
            return None;
        }
        // OpenUSD `_AttrWriter::Define`: reuse a spec of the right type,
        // refuse one of another type, and clear its samples.
        let store = scene.store();
        let mut define = |name: &'static str,
                          token: TokenId,
                          ty: &str,
                          variability: Variability,
                          report: &mut SkinningBakeReport| {
            let property = PropertyPath::new(path, token);
            let ty = PropertyType::parse(ty).expect("a registered value type");
            let stale = match existing_spec(store, layer, property) {
                Some(spec) => {
                    if spec.type_name.as_ref() != Some(&ty) {
                        report.problems.push(BakeProblem {
                            prim: path,
                            time: None,
                            reason: BakeProblemReason::AttributeType {
                                name,
                                existing: spec
                                    .type_name
                                    .as_ref()
                                    .map(|t| String::from(t.type_name()))
                                    .unwrap_or_default(),
                            },
                        });
                        return None;
                    }
                    spec.time_samples
                        .iter()
                        .flat_map(|samples| samples.iter().map(|(t, _)| *t))
                        .collect()
                }
                None => {
                    let mut spec = PropertySpec::typed_attribute(ty);
                    spec.variability = variability;
                    transaction
                        .create_property(EditTarget::for_layer(layer).property(property), spec);
                    Vec::new()
                }
            };
            Some(Writer {
                property,
                stale,
                written: Vec::new(),
            })
        };
        let mut points = None;
        let mut extent = None;
        if flags.points() {
            points = define(
                "points",
                names.points,
                "point3f[]",
                Variability::Varying,
                report,
            );
            if points.is_some() {
                if update_extents && mesh {
                    extent = define(
                        "extent",
                        names.extent,
                        "float3[]",
                        Variability::Varying,
                        report,
                    );
                }
            } else {
                flags.points_with_skinning = false;
                flags.points_with_blend_shapes = false;
            }
        }
        let mut normal_writer = None;
        if flags.normals() {
            normal_writer = define(
                "normals",
                names.normals,
                "normal3f[]",
                Variability::Varying,
                report,
            );
            if normal_writer.is_none() {
                flags.normals_with_skinning = false;
                flags.normals_with_blend_shapes = false;
            }
        }
        let mut xform = None;
        if flags.xforms_with_skinning {
            xform = define(
                "xformOp:transform",
                names.transform,
                "matrix4d",
                Variability::Varying,
                report,
            );
            let order = define(
                "xformOpOrder",
                names.xform_op_order,
                "token[]",
                Variability::Uniform,
                report,
            );
            match (&xform, order) {
                (Some(_), Some(order)) => {
                    let address = EditTarget::for_layer(layer).property(order.property);
                    for &time in &order.stale {
                        transaction.remove_time_sample(address.clone(), time);
                    }
                    let value = Value::TypedArray(TypedArray::Token(Arc::from([names.transform])));
                    transaction.set_default(address, value);
                }
                _ => {
                    xform = None;
                    flags.xforms_with_skinning = false;
                }
            }
        }
        if !flags.any() {
            return None;
        }
        // OpenUSD marks the skeleton's computations these deformations need.
        if flags.skinning() {
            rig.skinning_required = true;
        }
        if flags.blend_shapes() {
            rig.blend_required = true;
        }
        let parent = if !flags.xforms_with_skinning {
            ParentSpace::Unused
        } else if Xformable::new(scene, path)
            .is_some_and(|x| x.ordered_xform_ops().resets_xform_stack)
        {
            ParentSpace::Identity
        } else {
            ParentSpace::Computed
        };
        let inputs_vary = skinning.as_ref().is_some_and(|query| {
            let definition = &query.definition;
            let varies = |path, name| {
                Primvar::new(scene, path, name).is_some_and(|pv| pv.might_be_time_varying())
            };
            varies(definition.indices, "skel:jointIndices")
                || varies(definition.weights, "skel:jointWeights")
                || definition.geom_bind.is_some_and(|p| {
                    PrimView::new(*scene, p)
                        .attribute("primvars:skel:geomBindTransform")
                        .is_some_and(|a| a.might_be_time_varying())
                })
        });
        Some(Self {
            path,
            rig: rig_index,
            layer,
            flags,
            skinning,
            shapes,
            shape_map,
            normals: if flags.normals() {
                normals
            } else {
                NormalsSource::None
            },
            local_to_world: flags.points_with_skinning || flags.normals_with_skinning,
            parent,
            inputs: None,
            inputs_vary,
            points,
            normal_writer,
            extent,
            xform,
            times: Vec::new(),
        })
    }

    /// OpenUSD `_SkinningAdapter::ExtendTimeSamples`.
    fn extend_times(
        &self,
        scene: &Scene<'_>,
        interval: &(Bound<f64>, Bound<f64>),
        times: &mut Vec<f64>,
    ) {
        if self.flags.points() {
            times.extend(attribute_times(scene, self.path, "points", interval));
        }
        if self.normals != NormalsSource::None {
            times.extend(attribute_times(scene, self.path, "normals", interval));
            if self.normals == NormalsSource::FaceVarying {
                times.extend(attribute_times(
                    scene,
                    self.path,
                    "faceVertexIndices",
                    interval,
                ));
            }
        }
        if self.flags.skinning()
            && let Some(query) = &self.skinning
        {
            let definition = &query.definition;
            if let Some(source) = definition.geom_bind {
                times.extend(attribute_times(
                    scene,
                    source,
                    "primvars:skel:geomBindTransform",
                    interval,
                ));
            }
            for (path, name) in [
                (definition.indices, "skel:jointIndices"),
                (definition.weights, "skel:jointWeights"),
            ] {
                if let Some(primvar) = Primvar::new(scene, path, name) {
                    times.extend(primvar.sample_times_in_range(*interval));
                }
            }
        }
        if self.local_to_world {
            times.extend(own_transform_times(scene, self.path, interval));
        }
        if self.parent == ParentSpace::Computed
            && let Some(parent) = scene.parent(self.path)
        {
            times.extend(own_transform_times(scene, parent, interval));
        }
    }

    fn problem(&self, time: Time, error: SkelError, report: &mut SkinningBakeReport) {
        report.problems.push(BakeProblem {
            prim: self.path,
            time: Some(time),
            reason: BakeProblemReason::Evaluation(error),
        });
    }

    /// The binding inputs at `time`, cached when they cannot vary.
    fn inputs(&mut self, time: Time) -> Result<Arc<SkinningBindingInputs>, SkelError> {
        if !self.inputs_vary
            && let Some(inputs) = &self.inputs
        {
            return Ok(Arc::clone(inputs));
        }
        let query = self.skinning.as_ref().expect("a skinned prim");
        let inputs = Arc::new(query.binding_inputs(time)?);
        if !self.inputs_vary && matches!(time, Time::At { .. }) {
            self.inputs = Some(Arc::clone(&inputs));
        }
        Ok(inputs)
    }

    /// Reads what one sample deforms: OpenUSD `_SkinningAdapter`'s
    /// `UpdateTransform` and the input reads of `Update`. The deformation
    /// itself runs in [`run`], on any thread.
    fn gather(
        &mut self,
        index: usize,
        scene: &Scene<'_>,
        rig: &Rig<'_>,
        time: Time,
        xforms: &mut XformCache,
        report: &mut SkinningBakeReport,
    ) -> Job {
        self.times.push(time);
        let prim = PrimView::new(*scene, self.path);
        let read = |name: &str, wanted: bool, report: &mut SkinningBakeReport| {
            if !wanted {
                return None;
            }
            super::read(&prim, name, time, crate::value::read_float3_array_shared).unwrap_or_else(
                |error| {
                    self.problem(time, error, report);
                    None
                },
            )
        };
        let rest_points = read("points", self.flags.points(), report);
        let rest_normals = read("normals", self.normals != NormalsSource::None, report);
        let mut job = Job {
            target: index,
            time,
            flags: self.flags,
            normals: self.normals,
            extent: self.extent.is_some(),
            rest_points,
            rest_normals,
            shapes: None,
            skinning: None,
        };
        // Blend shapes precede skinning (OpenUSD `_DeformWithBlendShapes`).
        if self.flags.blend_shapes()
            && let (Some(shapes), Some(weights)) = (&self.shapes, &rig.weights)
            // OpenUSD's blend shape mapper fails a weight array that does
            // not match the animation's shapes; unmapped shapes weigh zero.
            && weights.len() == rig.blend_names.len()
        {
            let weights: Vec<f32> = self
                .shape_map
                .iter()
                .map(|i| i.map_or(0.0, |i| weights[i]))
                .collect();
            match shapes.compute_weights(&weights) {
                Ok(contributions) => job.shapes = Some((Arc::clone(shapes), contributions)),
                Err(error) => self.problem(time, error, report),
            }
        }
        if !self.flags.skinning() {
            return job;
        }
        let (Some(world), Some(transforms)) = (rig.world, &rig.transforms) else {
            return job;
        };
        let space = if self.local_to_world {
            // Skinning deforms in skeleton space; bake into the prim's:
            // skelToGprim = skelLocalToWorld * inverse(gprimLocalToWorld).
            let local_to_world = xforms
                .local_to_world(scene, self.path)
                .unwrap_or(gf::IDENTITY);
            Space::Prim(gf::mul(&world, &gf::inverse(&local_to_world).0))
        } else {
            let parent = match self.parent {
                ParentSpace::Computed => xforms
                    .parent_to_world(scene, self.path)
                    .unwrap_or(gf::IDENTITY),
                ParentSpace::Identity | ParentSpace::Unused => gf::IDENTITY,
            };
            // newLocal = skelSkinned * skelLocalToWorld * inverse(parentToWorld).
            Space::Parent(gf::mul(&world, &gf::inverse(&parent).0))
        };
        let inputs = match self.inputs(time) {
            Ok(inputs) => inputs,
            Err(error) => {
                self.problem(time, error, report);
                return job;
            }
        };
        let corners =
            if self.normals == NormalsSource::FaceVarying && self.flags.normals_with_skinning {
                match super::read(
                    &prim,
                    "faceVertexIndices",
                    time,
                    crate::value::read_int_array_shared,
                ) {
                    Ok(Some(corners)) => Some(corners),
                    Ok(None) => {
                        self.problem(time, super::invalid(self.path, "faceVertexIndices"), report);
                        None
                    }
                    Err(error) => {
                        self.problem(time, error, report);
                        None
                    }
                }
            } else {
                None
            };
        // Handle the prim's own `skel:joints` order.
        let query = self.skinning.as_ref().expect("a skinned prim");
        let transforms = match query.joint_mapping() {
            Some(mapping) => Arc::new(
                mapping
                    .iter()
                    .map(|i| i.map_or(gf::IDENTITY, |i| transforms[i]))
                    .collect(),
            ),
            None => Arc::clone(transforms),
        };
        job.skinning = Some(SkinJob {
            inputs,
            transforms,
            space,
            corners,
        });
        job
    }

    /// Writes one sample's outputs: OpenUSD `_SkinningAdapter::Write`.
    fn write(
        &mut self,
        output: Output,
        transaction: &mut Transaction,
        report: &mut SkinningBakeReport,
    ) {
        for error in output.problems {
            self.problem(output.time, error, report);
        }
        let code = time_code(output.time);
        let layer = self.layer;
        let mut write = |writer: &mut Option<Writer>, value: Value| {
            let Some(writer) = writer else {
                return;
            };
            let address = EditTarget::for_layer(layer).property(writer.property);
            match code {
                Some(code) => {
                    transaction.set_time_sample(address, code, value);
                    writer.written.push(code);
                }
                None => {
                    transaction.set_default(address, value);
                }
            }
        };
        if let Some(extent) = output.extent {
            write(&mut self.extent, vec3f_array(extent.to_vec()));
        }
        if let Some(points) = output.points {
            write(&mut self.points, vec3f_array(points));
        }
        if let Some(normals) = output.normals {
            write(&mut self.normal_writer, vec3f_array(normals));
        }
        if let Some(xform) = output.xform {
            write(&mut self.xform, matrix_value(&xform));
        }
    }
}

/// Where skinned values go after skinning in skeleton space.
#[derive(Clone, Copy)]
enum Space {
    /// Points and normals, into the prim's space by this transform.
    Prim(gf::Matrix4),
    /// A rigid prim's transform, into its parent's space by this transform.
    Parent(gf::Matrix4),
}

/// The skinning inputs of one sample.
struct SkinJob {
    inputs: Arc<SkinningBindingInputs>,
    /// Skinning transforms in the prim's joint order.
    transforms: Arc<Vec<gf::Matrix4>>,
    space: Space,
    /// A mesh's `faceVertexIndices`, for face-varying normals.
    corners: Option<Arc<Vec<i32>>>,
}

/// Everything one sample of one prim deforms, read from the stage; [`run`]
/// needs nothing else, so samples deform on any thread.
struct Job {
    target: usize,
    time: Time,
    flags: BakeDeformations,
    normals: NormalsSource,
    extent: bool,
    rest_points: Option<Arc<Vec<[f32; 3]>>>,
    rest_normals: Option<Arc<Vec<[f32; 3]>>>,
    shapes: Option<(Arc<BlendShapeQuery>, Vec<super::BlendShapeContribution>)>,
    skinning: Option<SkinJob>,
}

/// One sample's deformed values, and what failed.
struct Output {
    time: Time,
    points: Option<Vec<[f32; 3]>>,
    normals: Option<Vec<[f32; 3]>>,
    extent: Option<[[f32; 3]; 2]>,
    xform: Option<gf::Matrix4>,
    problems: Vec<SkelError>,
}

/// Deforms every job on up to `threads` threads, returning each output
/// with its target, in job order.
fn run_all(jobs: Vec<Job>, threads: usize) -> Vec<(usize, Output)> {
    let work = |(order, job): (usize, Job), _: &mut Vec<(usize, Job)>| {
        let target = job.target;
        (order, target, run(job))
    };
    let tasks: Vec<(usize, Job)> = jobs.into_iter().enumerate().collect();
    let mut outputs = layerstack::parallel::with_pool(threads, &work, |pool| pool.run(tasks));
    outputs.sort_unstable_by_key(|(order, _, _)| *order);
    outputs
        .into_iter()
        .map(|(_, target, output)| (target, output))
        .collect()
}

/// Deforms one sample: OpenUSD `_SkinningAdapter::Update` after its reads.
fn run(job: Job) -> Output {
    let mut output = Output {
        time: job.time,
        points: None,
        normals: None,
        extent: None,
        xform: None,
        problems: Vec::new(),
    };
    let flags = job.flags;
    if let Some((shapes, contributions)) = &job.shapes {
        if flags.points_with_blend_shapes
            && let Some(rest) = &job.rest_points
        {
            let mut deformed = rest.as_ref().clone();
            match shapes.deform_contributions(contributions, &mut deformed, false) {
                Ok(()) => output.points = Some(deformed),
                Err(error) => output.problems.push(error),
            }
        }
        if flags.normals_with_blend_shapes
            && let Some(rest) = &job.rest_normals
        {
            let mut deformed = rest.as_ref().clone();
            match shapes.deform_contributions(contributions, &mut deformed, true) {
                Ok(()) => {
                    // `ComputeDeformedNormals` normalizes.
                    normalize(&mut deformed);
                    output.normals = Some(deformed);
                }
                Err(error) => output.problems.push(error),
            }
        }
    }
    if let Some(skin) = &job.skinning {
        let inputs = &*skin.inputs;
        let method = inputs.skinning_method();
        let bind = inputs.geom_bind_transform();
        let influences = inputs.influences();
        let transforms = &skin.transforms[..];
        match skin.space {
            Space::Prim(to_prim) => {
                if flags.points_with_skinning
                    && let Some(rest) = &job.rest_points
                {
                    let mut skinned = output
                        .points
                        .take()
                        .unwrap_or_else(|| rest.as_ref().clone());
                    match super::skin_points_in_place_with_method(
                        method,
                        bind,
                        transforms,
                        influences,
                        &mut skinned,
                    ) {
                        Ok(()) => {
                            for point in &mut skinned {
                                *point = super::skinning::transform_point(*point, &to_prim, true);
                            }
                            output.points = Some(skinned);
                        }
                        Err(error) => output.problems.push(error),
                    }
                }
                if flags.normals_with_skinning
                    && let Some(rest) = &job.rest_normals
                {
                    let start = output
                        .normals
                        .take()
                        .unwrap_or_else(|| rest.as_ref().clone());
                    let skinned =
                        skin_normals(job.normals, method, bind, transforms, inputs, skin, start)
                            .and_then(|mut skinned| {
                                let m = normals::inverse_transpose(&to_prim, None)?;
                                for n in &mut skinned {
                                    *n = transform_normal(*n, &m);
                                }
                                Ok(skinned)
                            });
                    match skinned {
                        Ok(skinned) => output.normals = Some(skinned),
                        Err(error) => output.problems.push(error),
                    }
                }
            }
            Space::Parent(to_parent) => {
                if flags.xforms_with_skinning {
                    match super::rigid_skinning_transform_with_method(
                        method, bind, transforms, influences,
                    ) {
                        Ok(skinned) => output.xform = Some(gf::mul(&skinned, &to_parent)),
                        Err(error) => output.problems.push(error),
                    }
                }
            }
        }
    }
    if job.extent
        && let Some(points) = &output.points
    {
        output.extent = Some(points_extent(points));
    }
    output
}

/// Skins `normals` in skeleton space: vertex or varying normals, or a
/// mesh's face-varying normals through its `faceVertexIndices`.
fn skin_normals(
    source: NormalsSource,
    method: SkinningMethod,
    bind: &gf::Matrix4,
    transforms: &[gf::Matrix4],
    inputs: &SkinningBindingInputs,
    skin: &SkinJob,
    mut normals: Vec<[f32; 3]>,
) -> Result<Vec<[f32; 3]>, SkelError> {
    let influences = inputs.influences();
    if source == NormalsSource::FaceVarying {
        let corners = skin
            .corners
            .as_deref()
            .ok_or(SkelError::InvalidDeformation {
                element: None,
                reason: "face-varying normals without faceVertexIndices",
            })?;
        let point_count = influences.indices.len() / influences.element_size.max(1);
        super::skin_face_varying_normals_with_method(
            method,
            bind,
            transforms,
            influences,
            point_count,
            corners,
            &normals,
        )
    } else {
        super::skin_normals_in_place_with_method(
            method,
            bind,
            transforms,
            influences,
            &mut normals,
        )?;
        Ok(normals)
    }
}

// ------------------------------------------------------------------------
// After the bake: OpenUSD's `_PostUpdateExtents` and `_UpdateExtentHints`.

/// `UsdAttribute::Clear` through `target`: the local default and every
/// local time sample of `property`, keeping its spec.
fn clear_value(
    store: &mut dyn LayerStore,
    target: &EditTarget,
    property: PropertyPath,
    transaction: &mut Transaction,
) {
    let Some(mapped) = target.map_property_to_spec_path(property, store.paths_mut()) else {
        return;
    };
    if !mapped.variant_chain().is_empty() {
        return;
    }
    let local = PropertyPath::new(mapped.prim_path(), property.property());
    let Some(spec) = existing_spec(&*store, target.layer(), local) else {
        return;
    };
    let at = Address::spec(target.layer(), mapped);
    if spec.default.is_some() {
        transaction.clear_default(at.clone());
    }
    for (time, _) in spec.time_samples.iter().flat_map(|samples| samples.iter()) {
        transaction.remove_time_sample(at.clone(), *time);
    }
}

fn time_code(time: Time) -> Option<f64> {
    match time {
        Time::At { code, .. } => Some(code),
        Time::Default => None,
    }
}

impl SkinningBake {
    /// OpenUSD `_PostUpdateExtents`: each point-based prim that is not a
    /// mesh gets its extent from its deformed points, through
    /// `ComputeExtentFromPlugins`, at each time it was sampled; its old
    /// values are cleared first.
    fn post_extents(
        &self,
        live: &LiveStage,
        store: &mut dyn LayerStore,
        post: &PostBake,
        report: &mut SkinningBakeReport,
    ) -> Transaction {
        let token = store.tokens_mut().intern("extent");
        let mut computed: Vec<Vec<Recomputed<Extent>>> = Vec::with_capacity(post.extents.len());
        {
            let scene = Scene::new(live.stage(), &*store);
            for sampled in &post.extents {
                let mut extents = Vec::new();
                for &index in &sampled.indices {
                    let time = post.times[index];
                    match crate::extent::checked_intrinsic(&scene, sampled.prim, time) {
                        Ok(Some((range, _))) => extents.push(Recomputed {
                            time,
                            value: range.to_extent(),
                        }),
                        Ok(None) => {}
                        Err(error) => report.problems.push(BakeProblem {
                            prim: sampled.prim,
                            time: Some(time),
                            reason: BakeProblemReason::Extent(error),
                        }),
                    }
                }
                computed.push(extents);
            }
        }
        let mut transaction = Transaction::new();
        for sampled in &post.extents {
            let property = PropertyPath::new(sampled.prim, token);
            clear_value(store, &self.extents_target, property, &mut transaction);
        }
        let mut edit = SchemaEdit::new(live.stage(), store, self.extents_target.clone());
        for (sampled, extents) in post.extents.iter().zip(computed) {
            if let Err(error) = edit.can_edit(sampled.prim) {
                report.problems.push(BakeProblem {
                    prim: sampled.prim,
                    time: None,
                    reason: BakeProblemReason::CannotEdit(error),
                });
                continue;
            }
            for extent in extents {
                let value = vec3f_array(extent.value.to_vec());
                edit.set_value(sampled.prim, "extent", time_code(extent.time), value);
            }
            report.extents.push(sampled.prim);
        }
        transaction.append(edit.finish());
        transaction
    }

    /// OpenUSD `_UpdateExtentHints`: each model at or above a baked prim
    /// that has an `extentsHint` gets one computed from the baked stage
    /// (every purpose, ignoring hints), at each time one of its prims was
    /// sampled; its old values are cleared first.
    fn extents_hints(
        &self,
        live: &LiveStage,
        store: &mut dyn LayerStore,
        post: &PostBake,
        report: &mut SkinningBakeReport,
    ) -> Transaction {
        let token = store.tokens_mut().intern("extentsHint");
        let mut computed: Vec<Vec<Recomputed<Vec<Extent>>>> = Vec::with_capacity(post.hints.len());
        {
            let scene = Scene::new(live.stage(), &*store);
            let options = BoundsOptions {
                included_purposes: vec![
                    ImageablePurpose::Default,
                    ImageablePurpose::Render,
                    ImageablePurpose::Proxy,
                    ImageablePurpose::Guide,
                ],
                use_extents_hint: false,
                ignore_visibility: false,
            };
            // One cache per time, shared by every model, as OpenUSD shares
            // one `UsdGeomBBoxCache` per time.
            let mut caches: HashMap<usize, BoundsCache> = HashMap::new();
            for sampled in &post.hints {
                let mut hints = Vec::new();
                for &index in &sampled.indices {
                    let time = post.times[index];
                    let cache = caches
                        .entry(index)
                        .or_insert_with(|| BoundsCache::new(time, options.clone()));
                    match cache.compute_extents_hint(&scene, sampled.prim) {
                        Ok(hint) => hints.push(Recomputed { time, value: hint }),
                        Err(error) => report.problems.push(BakeProblem {
                            prim: sampled.prim,
                            time: Some(time),
                            reason: BakeProblemReason::Bounds(error),
                        }),
                    }
                }
                computed.push(hints);
            }
        }
        let mut transaction = Transaction::new();
        for sampled in &post.hints {
            let property = PropertyPath::new(sampled.prim, token);
            clear_value(store, &self.extents_target, property, &mut transaction);
        }
        let mut edit = SchemaEdit::new(live.stage(), store, self.extents_target.clone());
        for (sampled, hints) in post.hints.iter().zip(computed) {
            let model = sampled.prim;
            let handle = match GeomModelApiEdit::new(&edit, model) {
                Ok(Some(handle)) => handle,
                Ok(None) => continue,
                Err(error) => {
                    report.problems.push(BakeProblem {
                        prim: model,
                        time: None,
                        reason: BakeProblemReason::CannotEdit(error),
                    });
                    continue;
                }
            };
            for hint in &hints {
                let result = match time_code(hint.time) {
                    Some(code) => handle.set_extents_hint_at(&mut edit, code, &hint.value),
                    None => handle.set_extents_hint(&mut edit, &hint.value),
                };
                if let Err(error) = result {
                    report.problems.push(BakeProblem {
                        prim: model,
                        time: Some(hint.time),
                        reason: BakeProblemReason::ExtentsHint(error),
                    });
                }
            }
            report.extents_hints.push(model);
        }
        transaction.append(edit.finish());
        transaction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deformation_sets_name_openusd_flags() {
        assert_eq!(BakeDeformations::default(), BakeDeformations::ALL);
        assert!(!BakeDeformations::NONE.any());
        let skinning = BakeDeformations::SKINNING;
        assert!(skinning.skinning() && !skinning.blend_shapes());
        assert!(skinning.points() && skinning.normals());
        let shapes = BakeDeformations::BLEND_SHAPES;
        assert!(shapes.blend_shapes() && !shapes.skinning());
        assert!(
            !BakeDeformations {
                xforms_with_skinning: true,
                ..BakeDeformations::NONE
            }
            .points()
        );
    }

    #[test]
    fn extents_and_normals_round_as_openusd() {
        assert_eq!(
            points_extent(&[[1.0, -2.0, 0.5], [-1.0, 3.0, 0.25]]),
            [[-1.0, -2.0, 0.25], [1.0, 3.0, 0.5]]
        );
        let mut normals = [[3.0, 0.0, 4.0], [0.0, 0.0, 0.0]];
        normalize(&mut normals);
        assert_eq!(normals, [[0.6, 0.0, 0.8], [0.0, 0.0, 0.0]]);
        assert_eq!(sorted_unique(vec![2.0, 1.0, 2.0, 0.5]), [0.5, 1.0, 2.0]);
    }

    #[test]
    fn intervals_bound_bakes() {
        let bake = SkinningBake::new(EditTarget::for_layer(LayerId(1))).with_interval(1.0..3.0);
        assert!(contains(&bake.interval, 1.0) && !contains(&bake.interval, 3.0));
        let paths = layerstack::PathInterner::default();
        let tokens = layerstack::TokenInterner::default();
        let root = paths.intern(layerstack::Path::parse_absolute("/Root", &tokens).unwrap());
        let all = SkinningBake::for_root(root, EditTarget::for_layer(LayerId(2)));
        assert!(contains(&all.interval, -1e300));
        assert_eq!(all.bindings[0].root, root);
        assert_eq!(all.bindings[0].layer, LayerId(2));
        assert_eq!(all.bindings[0].skeleton, None);
    }
}
