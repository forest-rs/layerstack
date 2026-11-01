// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Skinning from TypeScript: a mesh's skinning binding, its joint
//! influences and joint palette as typed arrays, its blend-shaped points
//! and normals (what the palette skins), its deformed points and normals,
//! and the skinning functions over the caller's own arrays.
//!
//! A stage keeps one [`SkelCache`] and one [`BlendShapeCache`] with one
//! change cursor: before each skinning read they hear the stage's changes
//! since the last one and invalidate what they touch
//! ([`SkelCache::apply_changes`], [`BlendShapeCache::apply_changes`]), or
//! are cleared when the change history expired. A frame's reads then reuse
//! the rigs' definitions, inverse binds, influences and blend shapes, and
//! recompute only what time-varying inputs need at the new time.
//!
//! Arrays leave as [`ArrayHandle`]s, which the wrapper views as typed
//! arrays (`Float32Array`, `Float64Array`, `Int32Array`) without a copy;
//! the free functions take and return typed arrays, copied once each way.
//!
//! Spec: AOUSD Core §6.3 (row-vector matrices), §12.3 (value resolution
//! at a time). OpenUSD: `UsdSkelSkinningQuery`, `UsdSkelCache`,
//! `UsdSkelBlendShapeQuery`, `UsdSkelSkinPoints`, `UsdSkelSkinNormals`,
//! `UsdSkelSkinTransform`.

use alloc::{format, string::ToString, sync::Arc, vec, vec::Vec};

use layerstack::{ChangeCursor, InMemoryStore, LiveStage, PathId, Time, TypedArray};
use layerstack_schemas::{
    PrimView, Scene,
    skel::{
        BlendShapeCache, InfluenceInterpolation, InterleavedInfluences, JointInfluences, SkelCache,
        SkelError, SkinningMethod, SkinningQuery,
    },
    usd_skel::SkelAnimation,
    value::read_float3_array_shared,
};
use wasm_bindgen::prelude::*;

use crate::{array::ArrayHandle, error::Error, schemas::path_text, stage::Stage, wire::Wire};

/// A row-vector 4x4 matrix, as USD stores them.
type Matrix = [[f64; 4]; 4];

/// The skinning state a stage keeps: the retained evaluators, and the
/// cursor of the changes they have heard.
#[derive(Debug)]
pub(crate) struct SkelState {
    cache: SkelCache,
    /// Blend shapes alone, before skinning: what a GPU palette skins.
    shapes: BlendShapeCache,
    cursor: ChangeCursor,
    /// Times the change history expired and the caches were cleared.
    resets: u64,
}

impl SkelState {
    /// Empty evaluators hearing `live`'s changes from now on.
    pub(crate) fn new(live: &mut LiveStage) -> Self {
        Self {
            cache: SkelCache::new(Time::Default),
            shapes: BlendShapeCache::new(Time::Default),
            cursor: live.change_cursor(),
            resets: 0,
        }
    }

    /// Invalidates what every change the stage made since the last call
    /// touches, in both caches.
    fn hear(&mut self, live: &LiveStage, store: &InMemoryStore) {
        let scene = Scene::new(live.stage(), store);
        match live.changes_since(&mut self.cursor) {
            Ok(batches) => {
                for changes in batches {
                    self.cache.apply_changes(&scene, changes);
                    self.shapes.apply_changes(&scene, changes);
                }
            }
            // Evidence expired: start over.
            Err(_) => {
                self.cache.clear();
                self.shapes.clear();
                self.resets += 1;
            }
        }
    }

    /// The skinning cache, up to date with the stage, at `time`.
    fn at(&mut self, live: &LiveStage, store: &InMemoryStore, time: Time) -> &mut SkelCache {
        self.hear(live, store);
        self.cache.set_time(time);
        &mut self.cache
    }

    /// The blend-shape cache, up to date with the stage, at `time`.
    fn shapes_at(
        &mut self,
        live: &LiveStage,
        store: &InMemoryStore,
        time: Time,
    ) -> &mut BlendShapeCache {
        self.hear(live, store);
        self.shapes.set_time(time);
        &mut self.shapes
    }
}

/// The code of a skeletal failure.
fn skel_code(error: &SkelError) -> &'static str {
    match error {
        SkelError::Decode { .. } => "value.decode",
        SkelError::InvalidTopology { .. } => "skel.topology",
        SkelError::InvalidInput { .. } => "skel.input",
        SkelError::ArrayLength { .. } => "skel.array_length",
        SkelError::InvalidTarget { .. } => "skel.target",
        SkelError::Primvar { .. } => "skel.primvar",
        SkelError::InvalidDeformation { .. } => "skel.deformation",
        SkelError::UnsupportedSkinningMethod { .. } => "skel.method",
        SkelError::SingularNormalTransform { .. } | SkelError::SingularBind { .. } => {
            "skel.singular"
        }
        SkelError::Influences { .. } => "skel.influences",
        // A failure this build does not name.
        _ => "skel.invalid",
    }
}

/// A skeletal failure as an error, its paths named with `store`.
fn skel_error(error: &SkelError, store: &InMemoryStore) -> Error {
    Error::new(skel_code(error), error.display(store).to_string())
}

/// A skeletal failure of the free functions, which name no prim.
fn free_error(error: &SkelError) -> Error {
    Error::new(skel_code(error), error.to_string())
}

/// The method `name` names.
fn method(name: &str) -> Result<SkinningMethod, Error> {
    match name {
        "classicLinear" => Ok(SkinningMethod::ClassicLinear),
        "dualQuaternion" => Ok(SkinningMethod::DualQuaternion),
        other => Err(Error::new(
            "argument.invalid",
            format!("`{other}` is not a skinning method: classicLinear or dualQuaternion"),
        )),
    }
}

fn method_name(method: SkinningMethod) -> &'static str {
    match method {
        SkinningMethod::ClassicLinear => "classicLinear",
        SkinningMethod::DualQuaternion => "dualQuaternion",
    }
}

/// An array handle on `array`, typed `type_name`.
fn handle(type_name: &str, array: TypedArray) -> Wire {
    ArrayHandle::new(Some(type_name), &array).map_or(Wire::Null, Wire::Array)
}

/// A matrix as its 16 numbers, row by row.
fn matrix_record(matrix: &Matrix) -> Wire {
    Wire::List(
        matrix
            .as_flattened()
            .iter()
            .map(|&value| Wire::Number(value))
            .collect(),
    )
}

impl Stage {
    /// What the stage's skinning cache has done and holds:
    /// `[hits, misses, skeletonBuilds, bindingBuilds, poseEvaluations,
    /// influenceResolutions, pointVertices, normalVectors, invalidations,
    /// resets, skeletons, bindings, usedBytes, capacityBytes,
    /// blendShapeHits, blendShapeMisses, blendShapedVectors,
    /// blendShapeBindings]`: deformed arrays reused and recomputed, rig and
    /// binding snapshots built, poses evaluated, influence arrays
    /// flattened, points and normals skinned, entries edits invalidated
    /// (in either cache), times the change history expired and the caches
    /// started over (cumulative since the stage opened), then the rigs and
    /// bindings it retains and the bytes of both caches' arrays; then the
    /// blend-shaped arrays reused and recomputed, the points and normals
    /// blend-shaped (cumulative), and the blend-shape bindings retained.
    pub(crate) fn skinning_cache(&self) -> Wire {
        let state = self.skel_state();
        let stats = state.cache.stats();
        let memory = state.cache.memory();
        let shape_stats = state.shapes.stats();
        let shape_memory = state.shapes.memory();
        #[expect(clippy::cast_precision_loss, reason = "counters stay far below 2^53")]
        let count = |value: u64| Wire::Number(value as f64);
        let size = |value: usize| Wire::Number(value as f64);
        Wire::List(vec![
            count(stats.hits),
            count(stats.misses),
            count(stats.skeleton_builds),
            count(stats.binding_builds),
            count(stats.pose_evaluations),
            count(stats.influence_resolutions),
            count(stats.point_vertices),
            count(stats.normal_vectors),
            count(stats.invalidations + shape_stats.invalidations),
            count(state.resets),
            size(memory.skeletons),
            size(memory.bindings),
            size(memory.used_bytes + shape_memory.used_bytes),
            size(memory.capacity_bytes + shape_memory.capacity_bytes),
            count(shape_stats.hits),
            count(shape_stats.misses),
            count(shape_stats.point_vertices + shape_stats.normal_vectors),
            size(shape_memory.bindings),
        ])
    }

    /// The geometry prim at `path`, which must exist, and the scene.
    fn skinning_query(&self, path: &str) -> Result<Option<SkinningQuery<'_>>, Error> {
        let prim = self.existing_prim(path)?.path();
        let document = self.document();
        let scene = Scene::new(document.stage().stage(), document.store());
        SkinningQuery::new(&scene, prim)
            .map_err(|error| skel_error(&error, document.store()).at_path(path))
    }

    /// The skinning binding of the geometry at `path`, or `null` when no
    /// skeleton with joint influences binds it:
    /// `[skeleton, joints, jointMapping, elementSize, interpolation,
    /// method, animation, blendShapes, blendShapeWeightsVary]`, the bound
    /// skeleton, its joints in skeleton order, for each influence index
    /// the skeleton joint it names (`-1` for none; `null` when the
    /// binding's order is the skeleton's), the influences per point (or
    /// for the whole prim), `constant` (rigid) or `vertex`, the skinning
    /// method, the animation source (`null` without one), whether it binds
    /// blend shapes, and whether the animation's blend-shape weights might
    /// vary with time (two or more samples, a spline or value clips).
    ///
    /// OpenUSD: `UsdSkelSkinningQuery`,
    /// `UsdSkelAnimQuery::BlendShapeWeightsMightBeTimeVarying`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`; a `skel.*` code for a binding
    /// that is partial or malformed (`skel.input` for joint indices
    /// without weights), `skel.method` for an unknown skinning method.
    pub(crate) fn skinning(&self, path: &str) -> Result<Wire, Error> {
        let Some(query) = self.skinning_query(path)? else {
            return Ok(Wire::Null);
        };
        let document = self.document();
        let store = document.store();
        let scene = Scene::new(document.stage().stage(), store);
        let skeleton = query.skeleton_query();
        let method = query
            .skinning_method()
            .map_err(|error| skel_error(&error, store).at_path(path))?;
        let mapping = query.joint_mapping().map_or(Wire::Null, |mapping| {
            Wire::List(
                mapping
                    .iter()
                    .map(|joint| {
                        Wire::Number(joint.map_or(-1.0, |joint| {
                            f64::from(u32::try_from(joint).unwrap_or(u32::MAX))
                        }))
                    })
                    .collect(),
            )
        });
        Ok(Wire::List(vec![
            path_text(store, skeleton.skeleton_path()),
            Wire::List(skeleton.joints().iter().map(Wire::text).collect()),
            mapping,
            Wire::Number(query.element_size() as f64),
            Wire::text(match query.interpolation() {
                InfluenceInterpolation::Constant => "constant",
                InfluenceInterpolation::Vertex => "vertex",
            }),
            Wire::text(method_name(method)),
            skeleton
                .animation_path()
                .map_or(Wire::Null, |animation| path_text(store, animation)),
            Wire::Bool(query.blend_shapes_property().is_some()),
            Wire::Bool(
                skeleton
                    .animation_path()
                    .and_then(|animation| SkelAnimation::get(&scene, animation))
                    .is_some_and(|animation| animation.blend_shape_weights_might_be_time_varying()),
            ),
        ]))
    }

    /// The geometry's joint influences at `time`, flattened and checked
    /// for their shape: `[indices, weights]`, an `int[]` and a `float[]`
    /// array handle, or with `interleaved` `[pairs]`, one `float2[]` of
    /// `[index, weight]` pairs (`UsdSkelInterleaveInfluences`), the form
    /// GPU skinning takes.
    ///
    /// OpenUSD: `UsdSkelSkinningQuery::ComputeJointInfluences`.
    ///
    /// # Errors
    ///
    /// `prim.not_found`; `skel.unbound` when no skeleton with influences
    /// binds the prim; `skel.influences` for as many weights as indices
    /// missing, or a count that is not a whole number of blocks;
    /// `skel.primvar` for influences that do not flatten.
    pub(crate) fn skinning_influences(
        &self,
        path: &str,
        time: Option<f64>,
        interleaved: bool,
    ) -> Result<Wire, Error> {
        let store = self.document().store();
        let query = self.skinning_query(path)?.ok_or_else(|| unbound(path))?;
        let influences = query
            .compute_joint_influences(crate::stage::time_of(time))
            .map_err(|error| skel_error(&error, store).at_path(path))?;
        Ok(if interleaved {
            Wire::List(vec![handle(
                "float2[]",
                TypedArray::Vec2f(Arc::new(influences.interleaved())),
            )])
        } else {
            Wire::List(vec![
                handle("int[]", TypedArray::Int(Arc::new(influences.indices))),
                handle("float[]", TypedArray::Float(Arc::new(influences.weights))),
            ])
        })
    }

    /// The joint palette the geometry skins with at `time`:
    /// `[transforms, geomBindTransform, method]`, a `matrix4d[]` array
    /// handle of inverse-bind-times-animated joint transforms in the
    /// binding's joint order (what influence indices address), the
    /// geometry's bind transform as 16 numbers, row by row, and the
    /// skinning method. With [`Self::skinning_influences`], what a GPU
    /// skins the mesh with each frame; points come out in skeleton space.
    ///
    /// OpenUSD: `UsdSkelSkeletonQuery::ComputeSkinningTransforms`,
    /// `UsdSkelSkinningQuery::GetGeomBindTransform`.
    ///
    /// # Errors
    ///
    /// `prim.not_found`, `skel.unbound`, and the `skel.*` codes of a
    /// skeleton that cannot be posed (`skel.singular` for a bind transform
    /// without an inverse).
    pub(crate) fn skinning_transforms(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?.path();
        let (document, skel) = self.document_and_skel();
        let store = document.store();
        let live = document.stage();
        let cache = skel.at(live, store, crate::stage::time_of(time));
        let scene = Scene::new(live.stage(), store);
        let inputs = cache
            .deformation_inputs(&scene, prim)
            .map_err(|error| skel_error(&error, store).at_path(path))?
            .ok_or_else(|| unbound(path))?;
        let binding = inputs.binding();
        Ok(Wire::List(vec![
            handle(
                "matrix4d[]",
                TypedArray::Matrix4d(Arc::new(
                    inputs
                        .skinning_transforms()
                        .iter()
                        .map(|matrix| {
                            let mut flat = [0.0; 16];
                            flat.copy_from_slice(matrix.as_flattened());
                            flat
                        })
                        .collect(),
                )),
            ),
            matrix_record(binding.geom_bind_transform()),
            Wire::text(method_name(binding.skinning_method())),
        ]))
    }

    /// The geometry's points at `time` with its blend shapes applied and
    /// not skinned, in the geometry's space, as a `point3f[]` array handle:
    /// the points [`Self::skinning_transforms`] skin. Geometry binding no
    /// blend shapes gives its points as authored.
    ///
    /// OpenUSD: `UsdSkelBlendShapeQuery::ComputeDeformedPoints`, with the
    /// skeleton animation's weights mapped to the geometry's shapes
    /// (`UsdSkelSkinningQuery::GetBlendShapeMapper`), as
    /// `UsdSkelBakeSkinning` applies them before skinning.
    ///
    /// # Errors
    ///
    /// `prim.not_found`; `skel.input` for a prim without points;
    /// `skel.deformation` for offsets that do not fit the points;
    /// `skel.array_length` for weights that do not match the animation's
    /// blend shapes; `value.decode` for points that do not decode.
    pub(crate) fn blend_shaped_points(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<Wire, Error> {
        self.blend_shaped(path, time, "points")
    }

    /// The geometry's `normals` at `time` with its blend shapes' normal
    /// offsets applied, not normalized and not skinned, as a `normal3f[]`
    /// array handle: vertex, varying and (on a mesh) face-varying normals,
    /// each offset spread to the corners of its point. Geometry binding no
    /// blend shapes gives its normals as authored.
    ///
    /// OpenUSD: `UsdSkelBlendShapeQuery::ComputeDeformedNormals`.
    ///
    /// # Errors
    ///
    /// As [`Self::blend_shaped_points`]; `skel.input` for a prim without
    /// normals, or with uniform ones, or constant ones that shapes offset.
    pub(crate) fn blend_shaped_normals(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<Wire, Error> {
        self.blend_shaped(path, time, "normals")
    }

    /// The `points` or `normals` of the geometry at `path` and `time`,
    /// blend-shaped, or as authored without blend shapes.
    fn blend_shaped(&mut self, path: &str, time: Option<f64>, name: &str) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?.path();
        let (document, skel) = self.document_and_skel();
        let store = document.store();
        let live = document.stage();
        let time = crate::stage::time_of(time);
        let shapes = skel.shapes_at(live, store, time);
        let scene = Scene::new(live.stage(), store);
        let shaped = if name == "normals" {
            shapes.deformed_normals(&scene, prim)
        } else {
            shapes.deformed_points(&scene, prim)
        }
        .map_err(|error| skel_error(&error, store).at_path(path))?;
        let values = match shaped {
            Some(values) => Arc::new(values.to_vec()),
            None => authored(&scene, prim, name, time).map_err(|error| error.at_path(path))?,
        };
        Ok(handle(
            if name == "normals" {
                "normal3f[]"
            } else {
                "point3f[]"
            },
            TypedArray::Vec3f(values),
        ))
    }

    /// The geometry's points at `time` deformed by its blend shapes and
    /// then its skeleton, in skeleton space, as a `point3f[]` array handle.
    ///
    /// OpenUSD: `UsdSkelBakeSkinning`'s point deformation.
    ///
    /// # Errors
    ///
    /// `prim.not_found`, `skel.unbound`; `skel.input` for a prim without
    /// points; `skel.deformation` for influences that do not fit the
    /// points (a count that is not one block per point) or index past the
    /// joints.
    pub(crate) fn deformed_points(&mut self, path: &str, time: Option<f64>) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?.path();
        let (document, skel) = self.document_and_skel();
        let store = document.store();
        let live = document.stage();
        let cache = skel.at(live, store, crate::stage::time_of(time));
        let scene = Scene::new(live.stage(), store);
        let points = cache
            .deformed_points(&scene, prim)
            .map_err(|error| skel_error(&error, store).at_path(path))?
            .ok_or_else(|| unbound(path))?;
        Ok(handle(
            "point3f[]",
            TypedArray::Vec3f(Arc::new(points.to_vec())),
        ))
    }

    /// The geometry's normals at `time` deformed by its blend shapes and
    /// then skinned, normalized, as a `normal3f[]` array handle:
    /// vertex, varying and (on a mesh) face-varying normals.
    ///
    /// OpenUSD: `UsdSkelSkinNormals`, `UsdSkelSkinFaceVaryingNormals`.
    ///
    /// # Errors
    ///
    /// As [`Self::deformed_points`]; `skel.input` for a prim without
    /// normals, or with uniform ones.
    pub(crate) fn deformed_normals(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?.path();
        let (document, skel) = self.document_and_skel();
        let store = document.store();
        let live = document.stage();
        let cache = skel.at(live, store, crate::stage::time_of(time));
        let scene = Scene::new(live.stage(), store);
        let normals = cache
            .deformed_normals(&scene, prim)
            .map_err(|error| skel_error(&error, store).at_path(path))?
            .ok_or_else(|| unbound(path))?;
        Ok(handle(
            "normal3f[]",
            TypedArray::Vec3f(Arc::new(normals.to_vec())),
        ))
    }
}

/// The `points` or `normals` (`name`) of the prim at `path` and `time`,
/// as authored, shared without a copy.
fn authored(
    scene: &Scene<'_>,
    path: PathId,
    name: &str,
    time: Time,
) -> Result<Arc<Vec<[f32; 3]>>, Error> {
    PrimView::new(*scene, path)
        .try_read_value(name, time, read_float3_array_shared)
        .map_err(|error| Error::new("value.decode", format!("`{name}`: {error}")))?
        .ok_or_else(|| Error::new("skel.input", format!("the prim has no `{name}`")))
}

/// The error of a skinning read of geometry nothing skins.
fn unbound(path: &str) -> Error {
    Error::new(
        "skel.unbound",
        "no skeleton with joint influences binds this prim: `stage.skinning(path)` is null",
    )
    .at_path(path)
}

/// Joint influences from the wrapper: separate indices and weights, or
/// interleaved `[index, weight]` pairs.
struct Influences {
    indices: Option<Vec<i32>>,
    weights: Option<Vec<f32>>,
    pairs: Option<Vec<f32>>,
    element_size: usize,
    interpolation: InfluenceInterpolation,
}

impl Influences {
    /// Separate index and weight arrays, if given so.
    fn separate(&self) -> Option<JointInfluences<'_>> {
        Some(JointInfluences {
            indices: self.indices.as_deref()?,
            weights: self.weights.as_deref()?,
            element_size: self.element_size,
            interpolation: self.interpolation,
        })
    }

    /// Interleaved `[index, weight]` pairs, if given so.
    fn interleaved(&self) -> Option<InterleavedInfluences<'_>> {
        self.pairs.as_deref().map(|pairs| InterleavedInfluences {
            influences: pairs.as_chunks::<2>().0,
            element_size: self.element_size,
            interpolation: self.interpolation,
        })
    }

    fn new(
        indices: Option<Vec<i32>>,
        weights: Option<Vec<f32>>,
        pairs: Option<Vec<f32>>,
        element_size: u32,
        constant: bool,
    ) -> Result<Self, Error> {
        match (&indices, &weights, &pairs) {
            (Some(_), Some(_), None) => {}
            (None, None, Some(pairs)) if pairs.len().is_multiple_of(2) => {}
            (None, None, Some(_)) => {
                return Err(Error::new(
                    "skel.deformation",
                    "interleaved influences hold an [index, weight] pair per influence: an even \
                     number of floats",
                ));
            }
            _ => {
                return Err(Error::new(
                    "argument.invalid",
                    "influences are `indices` and `weights`, or `interleaved` pairs",
                ));
            }
        }
        Ok(Self {
            indices,
            weights,
            pairs,
            element_size: element_size as usize,
            interpolation: if constant {
                InfluenceInterpolation::Constant
            } else {
                InfluenceInterpolation::Vertex
            },
        })
    }
}

/// A matrix from 16 numbers, row by row.
fn matrix(values: &[f64]) -> Result<Matrix, Error> {
    let rows: &[[f64; 4]] = values.as_chunks::<4>().0;
    match rows {
        [a, b, c, d] if values.len() == 16 => Ok([*a, *b, *c, *d]),
        _ => Err(Error::new(
            "argument.invalid",
            "a matrix is 16 numbers, row by row",
        )),
    }
}

/// Matrices from 16 numbers each.
fn matrices(values: &[f64]) -> Result<Vec<Matrix>, Error> {
    if !values.len().is_multiple_of(16) {
        return Err(Error::new(
            "argument.invalid",
            "joint transforms are 16 numbers each, row by row",
        ));
    }
    values
        .as_chunks::<16>()
        .0
        .iter()
        .map(|values| matrix(values))
        .collect()
}

/// Points or normals from 3 floats each.
fn vectors(values: &[f32], what: &str) -> Result<Vec<[f32; 3]>, Error> {
    let (vectors, rest) = values.as_chunks::<3>();
    if !rest.is_empty() {
        return Err(Error::new(
            "argument.invalid",
            format!("{what} are 3 floats each"),
        ));
    }
    Ok(vectors.to_vec())
}

/// Skins `points` (3 floats each, geometry space) with `method`
/// (`classicLinear` or `dualQuaternion`), the geometry bind transform
/// `geom_bind` (16 numbers, row by row), the joint palette `joints` (16
/// numbers per joint, in the influences' joint order) and the joint
/// influences: `indices` and `weights`, or `interleaved` `[index, weight]`
/// pairs, `element_size` per point (or for the whole prim, `constant`).
/// Returns the points in skeleton space. Weights are used as given, not
/// normalized.
///
/// OpenUSD: `UsdSkelSkinPoints`, both its separate and its interleaved
/// influence overloads.
///
/// # Errors
///
/// `argument.invalid` for arrays of the wrong shape (a matrix that is not
/// 16 numbers, influences given both ways or neither);
/// `skel.deformation` for influences that do not fit the points (not one
/// block per point, as many weights as indices), a joint index past the
/// palette or a weight that is not finite; `argument.invalid` for an
/// unknown method.
#[wasm_bindgen(js_name = skinPoints)]
#[expect(
    clippy::too_many_arguments,
    reason = "the arrays cross as typed arrays, one argument each"
)]
pub fn skin_points(
    method_name: &str,
    geom_bind: Vec<f64>,
    joints: Vec<f64>,
    indices: Option<Vec<i32>>,
    weights: Option<Vec<f32>>,
    interleaved: Option<Vec<f32>>,
    element_size: u32,
    constant: bool,
    points: Vec<f32>,
) -> Result<Vec<f32>, JsValue> {
    Ok(skin_points_native(
        method_name,
        geom_bind,
        joints,
        indices,
        weights,
        interleaved,
        element_size,
        constant,
        points,
    )?)
}

/// [`skin_points`], natively.
#[allow(clippy::too_many_arguments, reason = "one argument per array")]
fn skin_points_native(
    method_name: &str,
    geom_bind: Vec<f64>,
    joints: Vec<f64>,
    indices: Option<Vec<i32>>,
    weights: Option<Vec<f32>>,
    interleaved: Option<Vec<f32>>,
    element_size: u32,
    constant: bool,
    points: Vec<f32>,
) -> Result<Vec<f32>, Error> {
    let method = method(method_name)?;
    let bind = matrix(&geom_bind)?;
    let joints = matrices(&joints)?;
    let points = vectors(&points, "points")?;
    let influences = Influences::new(indices, weights, interleaved, element_size, constant)?;
    let skinned = match (influences.interleaved(), influences.separate()) {
        (Some(pairs), _) => layerstack_schemas::skel::skin_points_with_method(
            method, &bind, &joints, pairs, &points,
        ),
        (None, Some(separate)) => layerstack_schemas::skel::skin_points_with_method(
            method, &bind, &joints, separate, &points,
        ),
        (None, None) => unreachable!("checked: indices and weights, or pairs"),
    };
    Ok(skinned
        .map_err(|error| free_error(&error))?
        .as_flattened()
        .to_vec())
}

/// Skins vertex or varying `normals` (3 floats each) as [`skin_points`]
/// skins points: with the inverse transposes of the bind transform and
/// the joint palette, normalized.
///
/// OpenUSD: `UsdSkelSkinNormals`, both influence overloads.
///
/// # Errors
///
/// As [`skin_points`]; `skel.singular` for a transform whose linear part
/// has no inverse.
#[wasm_bindgen(js_name = skinNormals)]
#[expect(
    clippy::too_many_arguments,
    reason = "the arrays cross as typed arrays, one argument each"
)]
pub fn skin_normals(
    method_name: &str,
    geom_bind: Vec<f64>,
    joints: Vec<f64>,
    indices: Option<Vec<i32>>,
    weights: Option<Vec<f32>>,
    interleaved: Option<Vec<f32>>,
    element_size: u32,
    constant: bool,
    normals: Vec<f32>,
) -> Result<Vec<f32>, JsValue> {
    Ok(skin_normals_native(
        method_name,
        geom_bind,
        joints,
        indices,
        weights,
        interleaved,
        element_size,
        constant,
        normals,
    )?)
}

/// [`skin_normals`], natively.
#[allow(clippy::too_many_arguments, reason = "one argument per array")]
fn skin_normals_native(
    method_name: &str,
    geom_bind: Vec<f64>,
    joints: Vec<f64>,
    indices: Option<Vec<i32>>,
    weights: Option<Vec<f32>>,
    interleaved: Option<Vec<f32>>,
    element_size: u32,
    constant: bool,
    normals: Vec<f32>,
) -> Result<Vec<f32>, Error> {
    let method = method(method_name)?;
    let bind = matrix(&geom_bind)?;
    let joints = matrices(&joints)?;
    let normals = vectors(&normals, "normals")?;
    let influences = Influences::new(indices, weights, interleaved, element_size, constant)?;
    let skinned = match (influences.interleaved(), influences.separate()) {
        (Some(pairs), _) => layerstack_schemas::skel::skin_normals_with_method(
            method, &bind, &joints, pairs, &normals,
        ),
        (None, Some(separate)) => layerstack_schemas::skel::skin_normals_with_method(
            method, &bind, &joints, separate, &normals,
        ),
        (None, None) => unreachable!("checked: indices and weights, or pairs"),
    };
    Ok(skinned
        .map_err(|error| free_error(&error))?
        .as_flattened()
        .to_vec())
}

/// The one transform constant influences move a rigid prim by, as 16
/// numbers row by row: the bind transform, then the influences' weighted
/// joint palette, in skeleton space.
///
/// OpenUSD: `UsdSkelSkinTransform`, both influence overloads.
///
/// # Errors
///
/// As [`skin_points`]; `skel.deformation` for influences that are not
/// constant.
#[wasm_bindgen(js_name = skinTransform)]
pub fn skin_transform(
    method_name: &str,
    geom_bind: Vec<f64>,
    joints: Vec<f64>,
    indices: Option<Vec<i32>>,
    weights: Option<Vec<f32>>,
    interleaved: Option<Vec<f32>>,
    element_size: u32,
) -> Result<Vec<f64>, JsValue> {
    Ok(skin_transform_native(
        method_name,
        geom_bind,
        joints,
        indices,
        weights,
        interleaved,
        element_size,
    )?)
}

/// [`skin_transform`], natively.
#[allow(clippy::too_many_arguments, reason = "one argument per array")]
fn skin_transform_native(
    method_name: &str,
    geom_bind: Vec<f64>,
    joints: Vec<f64>,
    indices: Option<Vec<i32>>,
    weights: Option<Vec<f32>>,
    interleaved: Option<Vec<f32>>,
    element_size: u32,
) -> Result<Vec<f64>, Error> {
    let method = method(method_name)?;
    let bind = matrix(&geom_bind)?;
    let joints = matrices(&joints)?;
    let influences = Influences::new(indices, weights, interleaved, element_size, true)?;
    let transform = match (influences.interleaved(), influences.separate()) {
        (Some(pairs), _) => layerstack_schemas::skel::rigid_skinning_transform_with_method(
            method, &bind, &joints, pairs,
        ),
        (None, Some(separate)) => layerstack_schemas::skel::rigid_skinning_transform_with_method(
            method, &bind, &joints, separate,
        ),
        (None, None) => unreachable!("checked: indices and weights, or pairs"),
    };
    Ok(transform
        .map_err(|error| free_error(&error))?
        .as_flattened()
        .to_vec())
}

#[cfg(test)]
pub(crate) mod tests {
    use alloc::vec;

    use super::*;
    use crate::{
        edit::EditBatch,
        schemas::tests::{open, show},
    };

    /// A robot whose arm joint moves +2 in x from time 0 to 10, a mesh
    /// whose last two points follow the arm, and a mesh with one weight
    /// for two joint indices.
    pub(crate) const ROBOT: &str = r#"#usda 1.0
def SkelRoot "Robot" (
    prepend apiSchemas = ["SkelBindingAPI"]
) {
    rel skel:skeleton = </Robot/Skel>
    def Skeleton "Skel" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        uniform token[] joints = ["Root", "Root/Arm"]
        uniform matrix4d[] bindTransforms = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1)), ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 1, 0, 1))]
        uniform matrix4d[] restTransforms = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1)), ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 1, 0, 1))]
        rel skel:animationSource = </Robot/Skel/Wave>
        def SkelAnimation "Wave" {
            uniform token[] joints = ["Root/Arm"]
            float3[] translations.timeSamples = {
                0: [(0, 1, 0)],
                10: [(2, 1, 0)],
            }
            quatf[] rotations = [(1, 0, 0, 0)]
            half3[] scales = [(1, 1, 1)]
        }
    }
    def Mesh "Arm" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (0, 1, 0), (1, 1, 0)]
        normal3f[] normals = [(0, 0, 1), (0, 0, 1), (0, 0, 1)] (
            interpolation = "vertex"
        )
        int[] primvars:skel:jointIndices = [0, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
    }
    def Mesh "Bad" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (0, 1, 0), (1, 1, 0)]
        int[] primvars:skel:jointIndices = [0, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1] (
            elementSize = 1
            interpolation = "vertex"
        )
    }
}
def Mesh "Loose" {
}
"#;

    #[test]
    fn an_engine_reads_a_binding_and_skins_it_each_frame() {
        let mut stage = open(ROBOT);
        assert_eq!(
            show(&stage.skinning("/Robot/Arm").unwrap()),
            "[/Robot/Skel, [Root, Root/Arm], null, 1, vertex, classicLinear, \
             /Robot/Skel/Wave, false, false]"
        );
        assert_eq!(stage.skinning("/Loose").unwrap(), Wire::Null);
        assert_eq!(
            show(
                &stage
                    .skinning_influences("/Robot/Arm", None, false)
                    .unwrap()
            ),
            "[Int([0, 1, 1]), Float([1.0, 1.0, 1.0])]"
        );
        assert_eq!(
            show(&stage.skinning_influences("/Robot/Arm", None, true).unwrap()),
            "[Vec2f([[0.0, 1.0], [1.0, 1.0], [1.0, 1.0]])]"
        );
        assert_eq!(
            show(&stage.deformed_points("/Robot/Arm", Some(0.0)).unwrap()),
            "Vec3f([[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]])"
        );
        assert_eq!(
            show(&stage.deformed_points("/Robot/Arm", Some(5.0)).unwrap()),
            "Vec3f([[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 1.0, 0.0]])"
        );
        assert_eq!(
            show(&stage.deformed_normals("/Robot/Arm", Some(5.0)).unwrap()),
            "Vec3f([[0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]])"
        );
        let palette = show(&stage.skinning_transforms("/Robot/Arm", Some(10.0)).unwrap());
        assert!(
            palette.ends_with(
                "1.0]]), [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1], classicLinear]"
            ),
            "{palette}"
        );
        assert!(
            palette.contains("2.0, 0.0, 0.0, 1.0]"),
            "the arm moved 2 in x: {palette}"
        );

        // An edit of the animation is heard before the next frame.
        let mut batch = EditBatch::new();
        let moved = batch.value_numbers("float3[]".into(), vec![4.0, 1.0, 0.0]);
        batch.set_value("/Robot/Skel/Wave.translations".into(), Some(10.0), moved);
        stage
            .apply(stage.document().root_layer().0, &mut batch)
            .unwrap();
        assert_eq!(
            show(&stage.deformed_points("/Robot/Arm", Some(5.0)).unwrap()),
            "Vec3f([[0.0, 0.0, 0.0], [2.0, 1.0, 0.0], [3.0, 1.0, 0.0]])"
        );
        let stats = show(&stage.skinning_cache());
        assert!(
            stats.starts_with("[0, "),
            "no point array was reused: {stats}"
        );
        assert!(
            stats.contains(", 0, 1, 1, "),
            "no reset; one rig, one binding: {stats}"
        );
    }

    /// A robot whose visor joint moves +2 in x from time 0 to 10, as its
    /// `open` shape's weight goes from 0 to 1, lifting the visor's last
    /// point by 1 in y and tilting its normal.
    const VISOR: &str = r#"#usda 1.0
def SkelRoot "Robot" (
    prepend apiSchemas = ["SkelBindingAPI"]
) {
    rel skel:skeleton = </Robot/Skel>
    def Skeleton "Skel" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        uniform token[] joints = ["Head"]
        uniform matrix4d[] bindTransforms = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))]
        uniform matrix4d[] restTransforms = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))]
        rel skel:animationSource = </Robot/Skel/Nod>
        def SkelAnimation "Nod" {
            uniform token[] joints = ["Head"]
            float3[] translations.timeSamples = {
                0: [(0, 0, 0)],
                10: [(2, 0, 0)],
            }
            quatf[] rotations = [(1, 0, 0, 0)]
            half3[] scales = [(1, 1, 1)]
            uniform token[] blendShapes = ["open"]
            float[] blendShapeWeights.timeSamples = {
                0: [0],
                10: [1],
            }
        }
    }
    def Mesh "Visor" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0)]
        normal3f[] normals = [(0, 0, 1), (0, 0, 1), (0, 0, 1)] (
            interpolation = "vertex"
        )
        int[] primvars:skel:jointIndices = [0, 0, 0] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        uniform token[] skel:blendShapes = ["open"]
        rel skel:blendShapeTargets = </Robot/Visor/Open>
        def BlendShape "Open" {
            uniform vector3f[] offsets = [(0, 2, 0)]
            uniform vector3f[] normalOffsets = [(0, 2, 0)]
            uniform int[] pointIndices = [2]
        }
    }
    def Mesh "Plate" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        point3f[] points = [(0, 0, 0)]
        int[] primvars:skel:jointIndices = [0] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1] (
            elementSize = 1
            interpolation = "vertex"
        )
    }
}
"#;

    #[test]
    fn blend_shaped_points_are_what_the_palette_skins() {
        let mut stage = open(VISOR);
        assert_eq!(
            show(&stage.skinning("/Robot/Visor").unwrap()),
            "[/Robot/Skel, [Head], null, 1, vertex, classicLinear, /Robot/Skel/Nod, true, true]"
        );
        assert_eq!(
            show(
                &stage
                    .blend_shaped_points("/Robot/Visor", Some(5.0))
                    .unwrap()
            ),
            "Vec3f([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 2.0, 0.0]])"
        );
        // Normal offsets are applied, not normalized.
        assert_eq!(
            show(
                &stage
                    .blend_shaped_normals("/Robot/Visor", Some(5.0))
                    .unwrap()
            ),
            "Vec3f([[0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 1.0, 1.0]])"
        );
        // Skinning them with the palette is what the CPU deforms.
        assert_eq!(
            show(&stage.deformed_points("/Robot/Visor", Some(5.0)).unwrap()),
            "Vec3f([[1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 2.0, 0.0]])"
        );
        assert_eq!(
            show(
                &stage
                    .blend_shaped_points("/Robot/Visor", Some(0.0))
                    .unwrap()
            ),
            "Vec3f([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]])"
        );
        // Without blend shapes: the points as authored.
        assert_eq!(
            show(
                &stage
                    .blend_shaped_points("/Robot/Plate", Some(5.0))
                    .unwrap()
            ),
            "Vec3f([[0.0, 0.0, 0.0]])"
        );
        assert_eq!(
            stage
                .blend_shaped_normals("/Robot/Plate", None)
                .unwrap_err()
                .code,
            "skel.input"
        );

        // Weights authored once do not vary, and an edit is heard.
        let mut batch = EditBatch::new();
        let weights = batch.value_numbers("float[]".into(), vec![0.25]);
        batch.set_value("/Robot/Skel/Nod.blendShapeWeights".into(), None, weights);
        batch.clear_value("/Robot/Skel/Nod.blendShapeWeights".into(), Some(0.0));
        batch.clear_value("/Robot/Skel/Nod.blendShapeWeights".into(), Some(10.0));
        stage
            .apply(stage.document().root_layer().0, &mut batch)
            .unwrap();
        assert!(
            show(&stage.skinning("/Robot/Visor").unwrap()).ends_with("true, false]"),
            "the weights no longer vary"
        );
        assert_eq!(
            show(
                &stage
                    .blend_shaped_points("/Robot/Visor", Some(5.0))
                    .unwrap()
            ),
            "Vec3f([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.5, 0.0]])"
        );
        let stats = show(&stage.skinning_cache());
        assert!(stats.ends_with(", 1]"), "one blend-shape binding: {stats}");
    }

    #[test]
    fn mismatched_influences_and_unbound_prims_fail_with_their_codes() {
        let mut stage = open(ROBOT);
        let error = stage
            .skinning_influences("/Robot/Bad", None, false)
            .unwrap_err();
        assert_eq!(
            (error.code, error.path.as_deref()),
            ("skel.influences", Some("/Robot/Bad"))
        );
        assert!(error.message.contains("/Robot/Bad"), "{}", error.message);
        let error = stage.deformed_points("/Robot/Bad", None).unwrap_err();
        assert_eq!(error.code, "skel.deformation", "{}", error.message);
        let error = stage.deformed_points("/Loose", None).unwrap_err();
        assert_eq!(error.code, "skel.unbound");
        assert_eq!(
            stage.skinning("/Robot/Gone").unwrap_err().code,
            "prim.not_found"
        );
    }

    #[test]
    fn the_free_functions_skin_with_separate_or_interleaved_influences() {
        let identity = vec![
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let mut joints = identity.clone();
        joints.extend([
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 2.0, 0.0, 0.0, 1.0,
        ]);
        let points = vec![0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let separate = skin_points_native(
            "classicLinear",
            identity.clone(),
            joints.clone(),
            Some(vec![0, 1]),
            Some(vec![1.0, 1.0]),
            None,
            1,
            false,
            points.clone(),
        )
        .unwrap();
        assert_eq!(separate, [0.0, 0.0, 0.0, 2.0, 1.0, 0.0]);
        let interleaved = skin_points_native(
            "classicLinear",
            identity.clone(),
            joints.clone(),
            None,
            None,
            Some(vec![0.0, 1.0, 1.0, 1.0]),
            1,
            false,
            points.clone(),
        )
        .unwrap();
        assert_eq!(interleaved, separate);
        let normals = skin_normals_native(
            "dualQuaternion",
            identity.clone(),
            joints.clone(),
            Some(vec![0, 1]),
            Some(vec![1.0, 1.0]),
            None,
            1,
            false,
            vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
        )
        .unwrap();
        assert_eq!(normals, [0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        let rigid = skin_transform_native(
            "classicLinear",
            identity.clone(),
            joints.clone(),
            Some(vec![1]),
            Some(vec![1.0]),
            None,
            1,
        )
        .unwrap();
        assert_eq!(rigid[12], 2.0);
        // Two indices, one weight: refused before any point is skinned.
        let error = skin_points_native(
            "classicLinear",
            identity.clone(),
            joints,
            Some(vec![0, 1]),
            Some(vec![1.0]),
            None,
            1,
            false,
            points,
        );
        assert_eq!(error.unwrap_err().code, "skel.deformation");
        assert_eq!(
            matrix(&identity[..15]).unwrap_err().code,
            "argument.invalid"
        );
        assert_eq!(method("linear").unwrap_err().code, "argument.invalid");
        assert_eq!(
            Influences::new(Some(vec![0]), None, None, 1, false)
                .err()
                .map(|error| error.code),
            Some("argument.invalid")
        );
    }
}
