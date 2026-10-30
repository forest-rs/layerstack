// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The geometry a renderer reads from TypeScript: transforms, visibility
//! and purpose, primvars and bounds.
//!
//! A stage keeps one [`GeomState`] with its own change cursor: before each
//! transform or bounds read it hears the stage's changes since the last
//! one and passes them to its retained [`XformCache`] and
//! [`BoundsCache`]s (`apply_changes`), which invalidate only what the
//! changes touch, or clears them when the change history expired. A
//! frame's reads then reuse every transform and bound an edit did not
//! touch, and recompute time-varying ones at the new time.
//!
//! Batched results leave as JS-owned typed arrays (`Float64Array`),
//! copied once out of the module; matrices
//! are 16 numbers each, row by row, in OpenUSD's row-vector convention
//! (`GfMatrix4d`: a point `p` transforms as `p * M`, the translation in
//! the last row).
//!
//! Spec: AOUSD Core §6.3 (row-vector matrices), §12.3 (value resolution
//! at a time). OpenUSD: `UsdGeomXformCache`, `UsdGeomXformable`,
//! `UsdGeomImageable`, `UsdGeomPrimvarsAPI`, `UsdGeomPrimvar`,
//! `UsdGeomBBoxCache`, `UsdGeomModelAPI::ComputeExtentsHint`.

use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use layerstack::{ChangeCursor, InMemoryStore, LiveStage, PathId, StageView, Time, TypedArray};
use layerstack_schemas::{
    PrimView, Scene, Visibility, XformCache, XformCacheStats, XformProblemKind,
    bounds::{BoundingBox, BoundsCache, BoundsError, BoundsOptions, BoundsStats, Range3d},
    primvar::{Primvar, PrimvarError},
    usd_geom::{Imageable, ImageablePurpose, Xformable},
};

use crate::{
    array::ArrayHandle,
    error::Error,
    schemas::path_text,
    stage::{Stage, time_of},
    value,
    wire::Wire,
};

/// At most this many bounds caches, one per set of options, are kept: a
/// renderer uses one or two. Asking for another drops the least recently
/// used.
const BOUNDS_CACHES: usize = 4;

/// The options a bounds cache is made with: the included purposes (as
/// given, sorted and without repeats), whether model `extentsHint`s are
/// used, and whether visibility is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BoundsKey {
    purposes: Vec<String>,
    hints: bool,
    ignore_visibility: bool,
}

impl BoundsKey {
    fn options(&self) -> BoundsOptions {
        BoundsOptions {
            included_purposes: self
                .purposes
                .iter()
                .map(|purpose| ImageablePurpose::from_token(purpose))
                .collect(),
            use_extents_hint: self.hints,
            ignore_visibility: self.ignore_visibility,
        }
    }
}

/// The geometry state a stage keeps: the retained transform and bounds
/// caches, and the cursor of the changes they have heard.
#[derive(Debug)]
pub(crate) struct GeomState {
    cursor: ChangeCursor,
    transforms: XformCache,
    /// Bounds caches, least recently used first.
    bounds: Vec<(BoundsKey, BoundsCache)>,
    /// The transform counters of caches cleared since the stage opened.
    transform_carry: XformCacheStats,
    /// The bounds counters of caches cleared or dropped since.
    bounds_carry: BoundsStats,
    /// Change batches heard.
    batches: u64,
    /// Times the change history expired and the caches were cleared.
    resets: u64,
}

impl GeomState {
    /// Empty caches hearing `live`'s changes from now on.
    pub(crate) fn new(live: &mut LiveStage) -> Self {
        Self {
            cursor: live.change_cursor(),
            transforms: XformCache::new(Time::Default),
            bounds: Vec::new(),
            transform_carry: XformCacheStats::default(),
            bounds_carry: BoundsStats::default(),
            batches: 0,
            resets: 0,
        }
    }

    /// Hears every change the stage made since the last call: each cache
    /// invalidates what the changes touch, or, when the history expired,
    /// starts over.
    fn hear(&mut self, live: &LiveStage, store: &InMemoryStore) {
        let scene = Scene::new(live.stage(), store);
        match live.changes_since(&mut self.cursor) {
            Ok(batches) => {
                for changes in batches {
                    self.batches += 1;
                    self.transforms.apply_changes(&scene, changes);
                    for (_, cache) in &mut self.bounds {
                        cache.apply_changes(&scene, changes);
                    }
                }
            }
            // Evidence expired: start over.
            Err(_) => {
                add_transform_stats(&mut self.transform_carry, self.transforms.stats());
                self.transforms.clear();
                for (_, cache) in &mut self.bounds {
                    add_bounds_stats(&mut self.bounds_carry, cache.stats());
                    cache.clear();
                }
                self.resets += 1;
            }
        }
    }

    /// The transform cache, current with the stage, at `time`.
    fn transforms_at(
        &mut self,
        live: &LiveStage,
        store: &InMemoryStore,
        time: Time,
    ) -> &mut XformCache {
        self.hear(live, store);
        self.transforms.set_time(time);
        &mut self.transforms
    }

    /// The bounds cache of `key`, current with the stage, at `time`.
    fn bounds_at(
        &mut self,
        live: &LiveStage,
        store: &InMemoryStore,
        key: BoundsKey,
        time: Time,
    ) -> &mut BoundsCache {
        self.hear(live, store);
        let at = match self.bounds.iter().position(|(held, _)| *held == key) {
            Some(at) => at,
            None => {
                if self.bounds.len() == BOUNDS_CACHES {
                    let (_, dropped) = self.bounds.remove(0);
                    add_bounds_stats(&mut self.bounds_carry, dropped.stats());
                }
                let options = key.options();
                self.bounds.push((key, BoundsCache::new(time, options)));
                self.bounds.len() - 1
            }
        };
        // Most recently used last.
        let entry = self.bounds.remove(at);
        self.bounds.push(entry);
        let cache = &mut self.bounds.last_mut().expect("just pushed").1;
        cache.set_time(time);
        cache
    }
}

fn add_transform_stats(total: &mut XformCacheStats, stats: XformCacheStats) {
    total.local_computed += stats.local_computed;
    total.world_computed += stats.world_computed;
    total.hits += stats.hits;
}

fn add_bounds_stats(total: &mut BoundsStats, stats: BoundsStats) {
    total.computed += stats.computed;
    total.hits += stats.hits;
    total.invalidated += stats.invalidated;
}

/// A matrix's 16 numbers, row by row.
fn push_matrix(out: &mut Vec<f64>, matrix: &[[f64; 4]; 4]) {
    out.extend_from_slice(matrix.as_flattened());
}

/// A range's `[minX, minY, minZ, maxX, maxY, maxZ]`.
fn push_range(out: &mut Vec<f64>, range: &Range3d) {
    out.extend_from_slice(&range.min);
    out.extend_from_slice(&range.max);
}

fn problem_name(kind: XformProblemKind) -> &'static str {
    match kind {
        XformProblemKind::NoAttribute => "noAttribute",
        XformProblemKind::UnknownOpType => "unknownOpType",
        XformProblemKind::NoValue => "noValue",
        XformProblemKind::ValueType => "valueType",
        XformProblemKind::Singular => "singular",
        // A problem this build does not name.
        _ => "other",
    }
}

fn visibility_name(visibility: Visibility) -> &'static str {
    visibility.as_str()
}

/// The code of a bounds failure.
fn bounds_code(error: &BoundsError) -> &'static str {
    match error {
        BoundsError::MissingPrim(_) => "prim.not_found",
        BoundsError::ExtentUnavailable(_) => "bounds.extent",
        BoundsError::ExtentProvider { .. } => "bounds.extent_provider",
        BoundsError::InvalidPointInstancer { .. } => "bounds.instancer",
        BoundsError::PointInstancerCycle(_) => "bounds.instancer_cycle",
        BoundsError::SingularTransform(_) => "bounds.singular",
        // A failure this build does not name.
        _ => "bounds.invalid",
    }
}

/// The code of a primvar failure.
fn primvar_code(error: &PrimvarError) -> &'static str {
    match error {
        PrimvarError::MissingIndices => "primvar.indices",
        PrimvarError::InvalidIndex { .. } => "primvar.index",
        PrimvarError::Decode { .. } => "value.decode",
        PrimvarError::MissingPrim(_) => "prim.not_found",
        // A failure this build does not name.
        _ => "primvar.invalid",
    }
}

/// The purposes of a bounds query: `default` alone when none are given.
fn bounds_key(purposes: &[String], hints: bool, ignore_visibility: bool) -> BoundsKey {
    let mut purposes: Vec<String> = if purposes.is_empty() {
        vec![String::from("default")]
    } else {
        purposes.to_vec()
    };
    purposes.sort_unstable();
    purposes.dedup();
    BoundsKey {
        purposes,
        hints,
        ignore_visibility,
    }
}

/// A primvar as a record: `[prim, name, typeName, interpolation,
/// elementSize, indexed, idTarget, mightBeTimeVarying]`, the prim whose
/// attribute it is (an ancestor's for an inherited primvar), its name
/// without `primvars:`, its attribute's value type, its interpolation
/// (`constant` when unauthored), its element size (1 when unauthored),
/// whether it has `:indices`, whether it is an id-target string, and
/// whether its value (or indices) might vary over time.
fn primvar_record(view: StageView<'_>, store: &InMemoryStore, primvar: &Primvar<'_>) -> Wire {
    let property = primvar.property();
    let type_name = view
        .attribute(property)
        .and_then(layerstack::Attribute::type_name)
        .unwrap_or("");
    Wire::List(vec![
        path_text(store, property.prim_path()),
        Wire::text(primvar.name()),
        Wire::text(type_name),
        Wire::text(primvar.interpolation()),
        Wire::Number(f64::from(primvar.element_size())),
        Wire::Bool(primvar.is_indexed()),
        Wire::Bool(primvar.is_id_target()),
        Wire::Bool(primvar.might_be_time_varying()),
    ])
}

impl Stage {
    /// The paths of `paths`, each a prim that must exist.
    fn prim_ids(&self, paths: &[String]) -> Result<Vec<PathId>, Error> {
        paths
            .iter()
            .map(|path| Ok(self.existing_prim(path)?.path()))
            .collect()
    }

    /// What the stage's transform cache has done and holds:
    /// `[localComputed, worldComputed, hits, prims, changeBatches,
    /// resets]`: local and local-to-world transforms computed, queries
    /// answered from the cache (cumulative since the stage opened), the
    /// prims it holds transforms for, the change batches it heard, and the
    /// times the change history expired and it started over.
    pub(crate) fn transform_cache(&self) -> Wire {
        let state = self.geom_state();
        let mut stats = state.transform_carry;
        add_transform_stats(&mut stats, state.transforms.stats());
        let size = |value: usize| Wire::Number(value as f64);
        #[expect(clippy::cast_precision_loss, reason = "counters stay far below 2^53")]
        let count = |value: u64| Wire::Number(value as f64);
        Wire::List(vec![
            size(stats.local_computed),
            size(stats.world_computed),
            size(stats.hits),
            size(state.transforms.len()),
            count(state.batches),
            count(state.resets),
        ])
    }

    /// What the stage's bounds caches have done and hold: `[computed,
    /// hits, invalidated, prims, caches, resets]`: prim bounds computed,
    /// reused, and evicted by edits (cumulative since the stage opened),
    /// the prim bounds held, the caches held (one per set of options, at
    /// most four), and the times the change history expired.
    pub(crate) fn bounds_cache(&self) -> Wire {
        let state = self.geom_state();
        let mut stats = state.bounds_carry;
        let mut prims = 0;
        for (_, cache) in &state.bounds {
            add_bounds_stats(&mut stats, cache.stats());
            prims += cache.len();
        }
        let size = |value: usize| Wire::Number(value as f64);
        #[expect(clippy::cast_precision_loss, reason = "counters stay far below 2^53")]
        let count = |value: u64| Wire::Number(value as f64);
        Wire::List(vec![
            size(stats.computed),
            size(stats.hits),
            size(stats.invalidated),
            size(prims),
            size(state.bounds.len()),
            count(state.resets),
        ])
    }

    /// The local transform of the prim at `path` at `time`, through the
    /// stage's transform cache: `[matrix, resetsXformStack, problems]`,
    /// its 16 numbers row by row (the identity for a prim that is not
    /// `Xformable`), whether it resets the transform stack, and each op
    /// that did not contribute as written as `[op, kind]` (`noAttribute`,
    /// `unknownOpType`, `noValue`, `valueType`, `singular`).
    ///
    /// OpenUSD: `UsdGeomXformCache::GetLocalTransformation`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`.
    pub(crate) fn local_transform(&mut self, path: &str, time: Option<f64>) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?.path();
        let (document, geom) = self.document_and_geom();
        let store = document.store();
        let live = document.stage();
        let cache = geom.transforms_at(live, store, time_of(time));
        let scene = Scene::new(live.stage(), store);
        let local = cache
            .local_transform(&scene, prim)
            .expect("the prim exists");
        let mut matrix = Vec::with_capacity(16);
        push_matrix(&mut matrix, &local.matrix);
        Ok(Wire::List(vec![
            Wire::Floats(matrix),
            Wire::Bool(local.resets_xform_stack),
            Wire::List(
                local
                    .problems
                    .iter()
                    .map(|problem| {
                        Wire::List(vec![
                            Wire::text(problem.op.as_str()),
                            Wire::text(problem_name(problem.kind)),
                        ])
                    })
                    .collect(),
            ),
        ]))
    }

    /// The local-to-world transform of each prim of `paths` at `time`, or
    /// with `parent` its parent's, through the stage's transform cache: 16
    /// numbers per prim, row by row, in order. The pseudo-root's is the
    /// identity.
    ///
    /// OpenUSD: `UsdGeomXformCache::GetLocalToWorldTransform`,
    /// `GetParentToWorldTransform`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` naming the first path no prim
    /// is at.
    pub(crate) fn world_transforms(
        &mut self,
        paths: &[String],
        time: Option<f64>,
        parent: bool,
    ) -> Result<Vec<f64>, Error> {
        let prims = self.prim_ids(paths)?;
        let (document, geom) = self.document_and_geom();
        let store = document.store();
        let live = document.stage();
        let cache = geom.transforms_at(live, store, time_of(time));
        let scene = Scene::new(live.stage(), store);
        let mut out = Vec::with_capacity(16 * prims.len());
        for prim in prims {
            let matrix = if parent {
                cache.parent_to_world(&scene, prim)
            } else {
                cache.local_to_world(&scene, prim)
            };
            push_matrix(&mut out, &matrix.expect("the prim exists"));
        }
        Ok(out)
    }

    /// The prim at `path`'s transform ops, or `null` when it is not
    /// `Xformable`: `[resetsXformStack, ops, mightBeTimeVarying,
    /// timeSamples]`, whether its `xformOpOrder` resets the transform
    /// stack, each op after the reset as `[name, attribute, opType,
    /// inverse]` (`opType` `null` for an attribute naming no op type),
    /// whether the transform might vary over time, and the transform's
    /// time samples, ascending.
    ///
    /// OpenUSD: `UsdGeomXformable::GetResetXformStack`,
    /// `GetOrderedXformOps`, `TransformMightBeTimeVarying`,
    /// `GetTimeSamples`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`.
    pub(crate) fn xformable(&self, path: &str) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?.path();
        let document = self.document();
        let scene = Scene::new(document.stage().stage(), document.store());
        let Some(xformable) = Xformable::get(&scene, prim) else {
            return Ok(Wire::Null);
        };
        let ops = xformable.ordered_xform_ops();
        Ok(Wire::List(vec![
            Wire::Bool(ops.resets_xform_stack),
            Wire::List(
                ops.ops
                    .iter()
                    .map(|op| {
                        Wire::List(vec![
                            Wire::text(op.name),
                            Wire::text(op.attribute),
                            op.op_type
                                .map_or(Wire::Null, |kind| Wire::text(kind.as_str())),
                            Wire::Bool(op.inverse),
                        ])
                    })
                    .collect(),
            ),
            Wire::Bool(xformable.transform_might_be_time_varying()),
            Wire::Floats(xformable.transform_time_samples()),
        ]))
    }

    /// The computed visibility of each prim of `paths` at `time`, in
    /// order: `inherited` or `invisible` (`UsdGeomImageable::ComputeVisibility`),
    /// or with a `purpose` the visibility for that purpose, `visible` among
    /// them (`ComputeEffectiveVisibility`); `null` for a prim that is not
    /// `Imageable`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` naming the first path no prim
    /// is at.
    pub(crate) fn visibilities(
        &self,
        paths: &[String],
        time: Option<f64>,
        purpose: Option<&str>,
    ) -> Result<Wire, Error> {
        let prims = self.prim_ids(paths)?;
        let document = self.document();
        let scene = Scene::new(document.stage().stage(), document.store());
        let time = time_of(time);
        let purpose = purpose.map(ImageablePurpose::from_token);
        Ok(Wire::List(
            prims
                .into_iter()
                .map(|prim| {
                    Imageable::get(&scene, prim).map_or(Wire::Null, |imageable| {
                        Wire::text(visibility_name(match &purpose {
                            Some(purpose) => imageable.compute_effective_visibility(purpose, time),
                            None => imageable.compute_visibility(time),
                        }))
                    })
                })
                .collect(),
        ))
    }

    /// The computed purpose of each prim of `paths`, in order, as
    /// `[purpose, inheritable, authoredOn]` (the prim whose authored
    /// `purpose` it is, `null` for a fallback), or `null` for a prim that
    /// is not `Imageable`.
    ///
    /// OpenUSD: `UsdGeomImageable::ComputePurposeInfo`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` naming the first path no prim
    /// is at.
    pub(crate) fn purposes(&self, paths: &[String]) -> Result<Wire, Error> {
        let prims = self.prim_ids(paths)?;
        let document = self.document();
        let store = document.store();
        let scene = Scene::new(document.stage().stage(), store);
        Ok(Wire::List(
            prims
                .into_iter()
                .map(|prim| {
                    Imageable::get(&scene, prim).map_or(Wire::Null, |imageable| {
                        let info = imageable.compute_purpose_info();
                        Wire::List(vec![
                            Wire::text(info.purpose.as_str()),
                            Wire::Bool(info.inheritable),
                            info.authored_on
                                .map_or(Wire::Null, |at| path_text(store, at)),
                        ])
                    })
                })
                .collect(),
        ))
    }

    /// The primvars of the prim at `path` as primvar records
    /// ([`primvar_record`]), sorted by name: its own, or with `inherited`
    /// those that produce a value, its own and the constant ones its
    /// ancestors pass down.
    ///
    /// OpenUSD: `UsdGeomPrimvarsAPI::GetPrimvars`,
    /// `FindPrimvarsWithInheritance`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`.
    pub(crate) fn primvars(&self, path: &str, inherited: bool) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?.path();
        let document = self.document();
        let store = document.store();
        let scene = Scene::new(document.stage().stage(), store);
        let view = PrimView::new(scene, prim);
        let primvars = if inherited {
            view.primvars_with_inheritance()
        } else {
            view.primvars()
        };
        Ok(Wire::List(
            primvars
                .iter()
                .map(|primvar| primvar_record(document.view(), store, primvar))
                .collect(),
        ))
    }

    /// The primvar `name` (with or without `primvars:`) of the prim at
    /// `path` as a primvar record ([`primvar_record`]), or `null`; with
    /// `inherited`, its own when it authors a value, else the nearest
    /// ancestor's constant one, else its own declaration.
    ///
    /// OpenUSD: `UsdGeomPrimvarsAPI::GetPrimvar`,
    /// `FindPrimvarWithInheritance`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`.
    pub(crate) fn primvar(&self, path: &str, name: &str, inherited: bool) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?.path();
        let document = self.document();
        let store = document.store();
        let scene = Scene::new(document.stage().stage(), store);
        let primvar = if inherited {
            PrimView::new(scene, prim).find_primvar_with_inheritance(name)
        } else {
            Primvar::new(&scene, prim, name)
        };
        Ok(primvar.map_or(Wire::Null, |primvar| {
            primvar_record(document.view(), store, &primvar)
        }))
    }

    /// The primvar `name` of the prim at `path` (which must declare it).
    fn declared_primvar<'a>(
        &self,
        scene: &Scene<'a>,
        path: &str,
        name: &str,
    ) -> Result<Primvar<'a>, Error> {
        let prim = self.existing_prim(path)?.path();
        Primvar::new(scene, prim, name).ok_or_else(|| {
            Error::new(
                "primvar.not_found",
                format!("the prim declares no primvar `{name}`"),
            )
            .at_path(path)
        })
    }

    /// The value of the primvar `name` of the prim at `path` at `time` as
    /// a value record (`null` for none): as authored, or with `flattened`
    /// expanded through its indices, each index giving `elementSize`
    /// values.
    ///
    /// OpenUSD: `UsdGeomPrimvar::Get`, `ComputeFlattened`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`, `primvar.not_found`;
    /// `primvar.indices` for an indexed primvar without readable indices,
    /// `primvar.index` for an index past the values, `value.decode`.
    pub(crate) fn primvar_value(
        &self,
        path: &str,
        name: &str,
        time: Option<f64>,
        flattened: bool,
    ) -> Result<Wire, Error> {
        let document = self.document();
        let store = document.store();
        let scene = Scene::new(document.stage().stage(), store);
        let primvar = self.declared_primvar(&scene, path, name)?;
        let time = time_of(time);
        let value = if flattened {
            primvar.compute_flattened(time)
        } else {
            primvar.value(time)
        }
        .map_err(|error| {
            Error::new(primvar_code(&error), error.display(store).to_string()).at_path(path)
        })?;
        let Some(value) = value else {
            return Ok(Wire::Null);
        };
        let type_name = document
            .view()
            .attribute(primvar.property())
            .and_then(layerstack::Attribute::type_name);
        Ok(value::encode(&value, type_name, store.tokens())
            .map_err(|error| error.at_path(path))?
            .unwrap_or(Wire::Null))
    }

    /// The indices of the primvar `name` of the prim at `path` at `time`,
    /// as an `int[]` array handle, or `null` when it is not indexed.
    ///
    /// OpenUSD: `UsdGeomPrimvar::GetIndices`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`, `primvar.not_found`.
    pub(crate) fn primvar_indices(
        &self,
        path: &str,
        name: &str,
        time: Option<f64>,
    ) -> Result<Wire, Error> {
        let document = self.document();
        let scene = Scene::new(document.stage().stage(), document.store());
        let primvar = self.declared_primvar(&scene, path, name)?;
        Ok(primvar
            .indices(time_of(time))
            .and_then(|indices| ArrayHandle::new(Some("int[]"), &TypedArray::Int(indices)))
            .map_or(Wire::Null, Wire::Array))
    }

    /// The bound of each prim of `paths` at `time` in `space` (`world`,
    /// `local` or `untransformed`), for the included `purposes` (`default`
    /// alone when empty), using models' `extentsHint` with `hints`, and
    /// with `ignore_visibility` counting invisible prims: `[ranges,
    /// matrices, aligned]`, each box's range before its matrix (6 numbers,
    /// minimum then maximum corner), its matrix (16, row by row) and the
    /// axis-aligned range of the transformed box (6). An empty box's range
    /// has its minimum above its maximum (`f64::MAX` and `-f64::MAX`). The
    /// stage keeps a bounds cache for each set of options that hears its
    /// edits.
    ///
    /// OpenUSD: `UsdGeomBBoxCache::ComputeWorldBound`, `ComputeLocalBound`,
    /// `ComputeUntransformedBound`, `GfBBox3d::ComputeAlignedRange`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, `prim.not_found`; `argument.invalid` for another
    /// space; a `bounds.*` code for geometry whose bound cannot be computed
    /// (`bounds.extent` for a boundable without a valid extent nor geometry
    /// to compute one from, `bounds.instancer` for invalid instance data).
    pub(crate) fn bounds(
        &mut self,
        paths: &[String],
        time: Option<f64>,
        purposes: &[String],
        hints: bool,
        ignore_visibility: bool,
        space: &str,
    ) -> Result<Wire, Error> {
        let prims = self.prim_ids(paths)?;
        if !matches!(space, "world" | "local" | "untransformed") {
            return Err(Error::new(
                "argument.invalid",
                format!("`{space}` is not a bounds space: world, local or untransformed"),
            ));
        }
        let (document, geom) = self.document_and_geom();
        let store = document.store();
        let live = document.stage();
        let key = bounds_key(purposes, hints, ignore_visibility);
        let cache = geom.bounds_at(live, store, key, time_of(time));
        let scene = Scene::new(live.stage(), store);
        let mut ranges = Vec::with_capacity(6 * prims.len());
        let mut matrices = Vec::with_capacity(16 * prims.len());
        let mut aligned = Vec::with_capacity(6 * prims.len());
        for (prim, path) in prims.into_iter().zip(paths) {
            let bound: Result<BoundingBox, BoundsError> = match space {
                "world" => cache.world_bound(&scene, prim),
                "local" => cache.local_bound(&scene, prim),
                _ => cache.untransformed_bound(&scene, prim),
            };
            let bound = bound.map_err(|error| {
                Error::new(bounds_code(&error), error.display(store).to_string()).at_path(path)
            })?;
            push_range(&mut ranges, &bound.range);
            push_matrix(&mut matrices, &bound.matrix);
            push_range(&mut aligned, &bound.aligned_range());
        }
        Ok(Wire::List(vec![
            Wire::Floats(ranges),
            Wire::Floats(matrices),
            Wire::Floats(aligned),
        ]))
    }

    /// The `extentsHint` OpenUSD computes for the prim at `path` at
    /// `time` with a bounds cache of these options (as in
    /// [`Self::bounds`]): one extent per purpose, in default, render,
    /// proxy, guide order, 6 numbers each, trailing empty ones dropped but
    /// one kept.
    ///
    /// OpenUSD: `UsdGeomModelAPI::ComputeExtentsHint`.
    ///
    /// # Errors
    ///
    /// As [`Self::bounds`].
    pub(crate) fn extents_hint(
        &mut self,
        path: &str,
        time: Option<f64>,
        purposes: &[String],
        hints: bool,
        ignore_visibility: bool,
    ) -> Result<Vec<f64>, Error> {
        let prim = self.existing_prim(path)?.path();
        let (document, geom) = self.document_and_geom();
        let store = document.store();
        let live = document.stage();
        let key = bounds_key(purposes, hints, ignore_visibility);
        let cache = geom.bounds_at(live, store, key, time_of(time));
        let scene = Scene::new(live.stage(), store);
        let hint = cache.compute_extents_hint(&scene, prim).map_err(|error| {
            Error::new(bounds_code(&error), error.display(store).to_string()).at_path(path)
        })?;
        Ok(hint
            .as_flattened()
            .as_flattened()
            .iter()
            .map(|&value| f64::from(value))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{
        edit::EditBatch,
        schemas::tests::{open, show},
    };

    /// A robot whose arm is translated and moves over time, a hidden lamp,
    /// a guide, and primvars to inherit.
    const SCENE: &str = r#"#usda 1.0
def Xform "Robot" (
    kind = "component"
) {
    double3 xformOp:translate = (1, 2, 3)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    color3f[] primvars:displayColor = [(1, 0, 0)] (
        interpolation = "constant"
    )
    def Xform "Arm" {
        double3 xformOp:translate.timeSamples = {
            0: (0, 0, 0),
            10: (10, 0, 0),
        }
        uniform token[] xformOpOrder = ["xformOp:translate"]
        def Mesh "Hand" {
            float3[] extent = [(-1, -1, -1), (1, 1, 1)]
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(-1, -1, -1), (1, 1, -1), (1, 1, 1)]
            float2[] primvars:st = [(0, 0), (1, 1)] (
                interpolation = "faceVarying"
            )
            int[] primvars:st:indices = [0, 1, 1]
        }
    }
    def Mesh "Guide" {
        uniform token purpose = "guide"
        float3[] extent = [(-5, -5, -5), (5, 5, 5)]
    }
}
def Xform "Lamp" {
    token visibility = "invisible"
    def Sphere "Bulb" {
        double radius = 2
    }
}
def "Loose" {
}
"#;

    #[test]
    fn transforms_are_retained_and_hear_edits() {
        let mut stage = open(SCENE);
        let paths = vec![
            String::from("/Robot"),
            String::from("/Robot/Arm"),
            String::from("/Robot/Arm/Hand"),
        ];
        let world = stage.world_transforms(&paths, Some(5.0), false).unwrap();
        assert_eq!(world.len(), 48);
        // Row-vector matrices: the translation is the last row.
        assert_eq!(&world[12..16], &[1.0, 2.0, 3.0, 1.0]);
        assert_eq!(&world[44..48], &[6.0, 2.0, 3.0, 1.0]);
        let parent = stage
            .world_transforms(&paths[2..], Some(5.0), true)
            .unwrap();
        assert_eq!(&parent[12..16], &[6.0, 2.0, 3.0, 1.0]);
        assert_eq!(
            show(&stage.local_transform("/Robot/Arm", Some(10.0)).unwrap()),
            "[[1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 10, 0, 0, 1], false, []]"
        );
        assert_eq!(
            show(&stage.xformable("/Robot/Arm").unwrap()),
            "[false, [[xformOp:translate, xformOp:translate, translate, false]], true, [0, 10]]"
        );
        assert_eq!(stage.xformable("/Loose").unwrap(), Wire::Null);
        let before = show(&stage.transform_cache());

        // An edit of the robot's translation is heard: its world transform
        // and its descendants' are recomputed, its local transform alone.
        let mut batch = EditBatch::new();
        let moved = batch.value_numbers("double3".into(), vec![0.0, 0.0, 0.0]);
        batch.set_value("/Robot.xformOp:translate".into(), None, moved);
        stage
            .apply(stage.document().root_layer().0, &mut batch)
            .unwrap();
        let world = stage.world_transforms(&paths, Some(5.0), false).unwrap();
        assert_eq!(&world[44..48], &[5.0, 0.0, 0.0, 1.0]);
        let after = show(&stage.transform_cache());
        assert_ne!(before, after);
        assert!(after.ends_with(", 3, 1, 0]"), "{after}");
        assert_eq!(
            stage
                .world_transforms(&[String::from("/Gone")], None, false)
                .unwrap_err()
                .code,
            "prim.not_found"
        );
    }

    #[test]
    fn visibility_and_purpose_are_computed() {
        let stage = open(SCENE);
        let paths = vec![
            String::from("/Lamp/Bulb"),
            String::from("/Robot/Guide"),
            String::from("/Loose"),
        ];
        assert_eq!(
            show(&stage.visibilities(&paths, None, None).unwrap()),
            "[invisible, inherited, null]"
        );
        assert_eq!(
            show(&stage.visibilities(&paths, None, Some("guide")).unwrap()),
            "[invisible, invisible, null]"
        );
        assert_eq!(
            show(&stage.purposes(&paths).unwrap()),
            "[[default, false, null], [guide, true, /Robot/Guide], null]"
        );
    }

    #[test]
    fn primvars_list_inherit_and_flatten() {
        let stage = open(SCENE);
        assert_eq!(
            show(&stage.primvars("/Robot/Arm/Hand", false).unwrap()),
            "[[/Robot/Arm/Hand, displayColor, color3f[], constant, 1, false, false, false], \
             [/Robot/Arm/Hand, displayOpacity, float[], constant, 1, false, false, false], \
             [/Robot/Arm/Hand, st, float2[], faceVarying, 1, true, false, false]]"
        );
        assert_eq!(
            show(&stage.primvars("/Robot/Arm/Hand", true).unwrap()),
            "[[/Robot, displayColor, color3f[], constant, 1, false, false, false], \
             [/Robot/Arm/Hand, st, float2[], faceVarying, 1, true, false, false]]"
        );
        assert_eq!(
            show(
                &stage
                    .primvar("/Robot/Arm/Hand", "displayColor", true)
                    .unwrap()
            ),
            "[/Robot, displayColor, color3f[], constant, 1, false, false, false]"
        );
        // The mesh's own `displayColor` is the schema's, without a value.
        assert_eq!(
            stage
                .primvar_value("/Robot/Arm/Hand", "displayColor", None, true)
                .unwrap(),
            Wire::Null
        );
        let flat = show(
            &stage
                .primvar_value("/Robot/Arm/Hand", "st", None, true)
                .unwrap(),
        );
        assert!(
            flat.ends_with("Vec2f([[0.0, 0.0], [1.0, 1.0], [1.0, 1.0]])]"),
            "{flat}"
        );
        assert_eq!(
            show(
                &stage
                    .primvar_indices("/Robot/Arm/Hand", "st", None)
                    .unwrap()
            ),
            "Int([0, 1, 1])"
        );
        assert_eq!(
            stage
                .primvar_value("/Robot/Arm/Hand", "nothing", None, true)
                .unwrap_err()
                .code,
            "primvar.not_found"
        );
    }

    #[test]
    fn bounds_are_batched_and_hear_edits() {
        let mut stage = open(SCENE);
        // A queried root counts even when invisible, as in OpenUSD; its
        // invisible descendants do not, so the lamp's parent is empty.
        let paths = vec![
            String::from("/Robot"),
            String::from("/Lamp"),
            String::from("/Loose"),
        ];
        let bounds = stage
            .bounds(&paths, Some(0.0), &[], false, false, "world")
            .unwrap();
        let max = f64::MAX;
        assert_eq!(
            show(&bounds),
            format!(
                "[[-1, -1, -1, 1, 1, 1, -2, -2, -2, 2, 2, 2, {max}, {max}, {max}, -{max}, -{max}, \
                 -{max}], [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 1, 2, 3, 1, 1, 0, 0, 0, 0, 1, 0, \
                 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1], [0, \
                 1, 2, 2, 3, 4, -2, -2, -2, 2, 2, 2, {max}, {max}, {max}, -{max}, -{max}, -{max}]]"
            )
        );
        let guides = stage
            .bounds(
                &paths[..1],
                Some(0.0),
                &[String::from("default"), String::from("guide")],
                false,
                false,
                "untransformed",
            )
            .unwrap();
        assert!(
            show(&guides).starts_with("[[-5, -5, -5, 5, 5, 5]"),
            "{}",
            show(&guides)
        );
        let hint = stage
            .extents_hint("/Robot", Some(0.0), &[], false, false)
            .unwrap();
        assert_eq!(&hint[..6], &[-1.0, -1.0, -1.0, 1.0, 1.0, 1.0]);

        let mut batch = EditBatch::new();
        let grown = batch.value_numbers("float3[]".into(), vec![-2.0, -2.0, -2.0, 2.0, 2.0, 2.0]);
        batch.set_value("/Robot/Arm/Hand.extent".into(), None, grown);
        stage
            .apply(stage.document().root_layer().0, &mut batch)
            .unwrap();
        let bounds = stage
            .bounds(&paths[..1], Some(0.0), &[], false, false, "untransformed")
            .unwrap();
        assert!(
            show(&bounds).starts_with("[[-2, -2, -2, 2, 2, 2]"),
            "{}",
            show(&bounds)
        );
        let stats = show(&stage.bounds_cache());
        assert!(stats.ends_with(", 2, 0]"), "two caches, no reset: {stats}");
        assert_eq!(
            stage
                .bounds(&paths, None, &[], false, false, "sideways")
                .unwrap_err()
                .code,
            "argument.invalid"
        );
    }
}
