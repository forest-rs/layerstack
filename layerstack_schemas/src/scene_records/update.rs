// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{capture, types::*};
use crate::{
    BindingCache, MeshSites, Scene, XformCache,
    shading::{MaterialNetworkCache, MaterialNetworkSample},
};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};
use layerstack::{
    ChangeCursor, ChangeHistoryError, Changes, LayerStore, LiveStage, PathId, StoreIdentity, Time,
    TokenId,
};

/// Retained supported scene records in one store domain. Polling is explicit.
///
/// Handles identify record incarnations rather than namespace paths. A foreign
/// store is rejected before interpreting IDs. Each successful update atomically
/// publishes components and advances its independent composed change cursor.
/// Temporary capture/cache work may survive failure; public records do not.
#[derive(Debug)]
pub struct SceneObserver {
    options: SceneOptions,
    observer: Arc<()>,
    next_handle: u64,
    identity: Option<StoreIdentity>,
    cursor: Option<ChangeCursor>,
    time: Option<Time>,
    records: BTreeMap<PathId, SceneRecord>,
    handles: BTreeMap<u64, PathId>,
    transforms: XformCache,
    networks: MaterialNetworkCache,
    materials: BTreeMap<PathId, MaterialNetworkSample>,
}
impl Default for SceneObserver {
    fn default() -> Self {
        Self::new(SceneOptions::default())
    }
}
impl SceneObserver {
    /// Empty observer with immutable capture policies and no store affinity yet.
    pub fn new(options: SceneOptions) -> Self {
        let contexts: Vec<_> = options.render_contexts.iter().map(|s| s.as_str()).collect();
        Self {
            networks: MaterialNetworkCache::new(Time::Default, &contexts),
            options,
            observer: Arc::default(),
            next_handle: 1,
            identity: None,
            cursor: None,
            time: None,
            records: BTreeMap::new(),
            handles: BTreeMap::new(),
            transforms: XformCache::new(Time::Default),
            materials: BTreeMap::new(),
        }
    }
    /// Domain affinity established by the first successful extraction.
    pub fn store_identity(&self) -> Option<&StoreIdentity> {
        self.identity.as_ref()
    }
    /// Release records, material graphs, change history position and store affinity.
    /// The caller discards its inventory; the next update reports initial additions.
    /// Incarnation numbers remain unreused, so old handles never alias new records.
    /// Explicitly clear before rebinding to a replacement document's store domain;
    /// foreign IDs are never automatically remapped.
    pub fn clear(&mut self) {
        self.records.clear();
        self.handles.clear();
        self.materials.clear();
        self.transforms.clear();
        self.networks.clear();
        self.cursor = None;
        self.identity = None;
        self.time = None;
    }
    /// Most recently published explicit extraction time.
    pub fn time(&self) -> Option<Time> {
        self.time
    }
    /// Most recently consumed composed revision, or absence before first update.
    pub fn revision(&self) -> Option<u64> {
        self.cursor.as_ref().map(ChangeCursor::revision)
    }
    /// Current records ordered by store-local path IDs, not lexical path spelling.
    pub fn records(&self) -> impl Iterator<Item = &SceneRecord> {
        self.records.values()
    }
    /// Resolve a live observer-local handle; foreign and retired handles fail.
    pub fn record(&self, handle: &SceneHandle) -> Option<&SceneRecord> {
        if !Arc::ptr_eq(&self.observer, &handle.observer) {
            return None;
        }
        self.handles
            .get(&handle.incarnation)
            .and_then(|path| self.records.get(path))
    }
    /// Find the current incarnation at a path in this observer's store domain.
    pub fn record_at(&self, path: PathId) -> Option<&SceneRecord> {
        self.records.get(&path)
    }
    /// Captured upstream graph for a currently referenced material.
    pub fn material(&self, path: PathId) -> Option<&MaterialNetworkSample> {
        self.materials.get(&path)
    }
    /// Referenced immutable upstream material graph handoffs.
    pub fn materials(&self) -> impl Iterator<Item = (PathId, &MaterialNetworkSample)> {
        self.materials.iter().map(|(&path, sample)| (path, sample))
    }
    /// Synchronize source edits and publish checked records at `time`.
    ///
    /// Composed change history is independent from other observers. Missing
    /// history reconstructs all records and reports a reset. Invalid input or
    /// decode failure commits neither records nor cursor, allowing explicit retry.
    /// Invisible geometry stays inspectable. Inactive, undefined and abstract
    /// prims are omitted. Unsupported Gprim schemas fail explicitly.
    /// AOUSD Core §10–12 (composition/population and value resolution);
    /// OpenUSD UsdGeom/UsdShade computed transforms, primvars and bindings.
    pub fn update(
        &mut self,
        live: &mut LiveStage,
        store: &mut dyn LayerStore,
        time: Time,
    ) -> Result<SceneUpdate, SceneError> {
        let identity = store.identity();
        if live.stage().store_identity() != Some(&identity)
            || self.identity.as_ref().is_some_and(|old| old != &identity)
        {
            return Err(SceneError::ForeignStore);
        }
        if matches!(time, Time::At { code, .. } if !code.is_finite()) {
            return Err(SceneError::InvalidTime);
        }
        live.synchronize(store);
        let initial = self.cursor.is_none();
        let mut cursor = self.cursor.clone().unwrap_or_else(|| live.change_cursor());
        let reports = live
            .changes_since(&mut cursor)
            .map(|reports| reports.cloned().collect::<Vec<_>>());
        let (reports, reset) = match reports {
            Ok(reports) => (reports, None),
            Err(ChangeHistoryError::Expired) => (Vec::new(), Some(SceneReset::HistoryLost)),
            Err(ChangeHistoryError::DifferentStage) => {
                cursor = live.change_cursor();
                (Vec::new(), Some(SceneReset::DifferentStage))
            }
        };
        let mut update = SceneUpdate {
            added: Vec::new(),
            changed: Vec::new(),
            removed: Vec::new(),
            materials: Vec::new(),
            removed_materials: Vec::new(),
            reset,
            revision: cursor.revision(),
            time,
            work: SceneWork::default(),
        };
        let time_changed = self.time != Some(time);
        if !initial && reset.is_none() && reports.is_empty() && !time_changed {
            return Ok(update);
        }
        let scene = Scene::new(live.stage(), store);
        if scene.stage().schemas().is_none_or(|registry| {
            ["Mesh", "Material", "PointInstancer"].iter().any(|name| {
                scene
                    .store()
                    .tokens()
                    .lookup(name)
                    .is_none_or(|token| registry.schema(token).is_none())
            })
        }) {
            return Err(SceneError::MissingSchemas);
        }
        let full = initial || reset.is_some();
        let structural = full
            || reports
                .iter()
                .any(|r| !r.created.is_empty() || !r.removed.is_empty() || !r.resynced.is_empty());
        let inventory = if structural {
            capture::paths(&scene, &mut update.work)?
        } else {
            self.records.values().map(|r| (r.path, r.kind)).collect()
        };
        let mut retired: BTreeSet<_> = reports
            .iter()
            .flat_map(|r| r.removed.iter().copied())
            .collect();
        if reset.is_some() {
            retired.extend(self.records.keys().copied());
        }
        let mut records = self.records.clone();
        update.work.staged_records = records.len();
        let current: BTreeSet<_> = inventory.iter().map(|(p, _)| *p).collect();
        records.retain(|path, record| {
            let keep = current.contains(path) && !retired.contains(path);
            if !keep {
                update.removed.push(record.handle.clone());
            }
            keep
        });
        let mut next_handle = self.next_handle;
        let mut dirty = BTreeMap::new();
        for &(path, kind) in &inventory {
            let old = records.get(&path);
            let added = old.is_none() || old.is_some_and(|r| r.kind != kind);
            let routed = route(
                &scene,
                path,
                kind,
                old,
                &reports,
                full || added,
                time_changed,
            );
            if old.is_some_and(|r| r.kind != kind) {
                update
                    .removed
                    .push(old.expect("checked old record").handle.clone());
                records.remove(&path);
            }
            update.work.routed_records += usize::from(!full);
            dirty.insert(path, routed);
            if added {
                let handle = SceneHandle {
                    observer: self.observer.clone(),
                    incarnation: next_handle,
                };
                next_handle = next_handle
                    .checked_add(1)
                    .expect("scene incarnation exhausted");
                update.added.push(handle.clone());
                records.insert(path, empty(path, kind, handle));
            }
        }
        let (sources, native) = native_sources(&scene, &current);
        update.work.native_members = native.values().map(Vec::len).sum();
        let dirty_sources: BTreeSet<_> = inventory
            .iter()
            .filter(|(path, kind)| *kind == SceneKind::Mesh && dirty[path].geometry)
            .map(|(path, _)| sources.get(path).copied().unwrap_or(*path))
            .collect();
        let mut geometry = BTreeMap::new();
        for &(path, kind) in &inventory {
            if kind != SceneKind::Mesh {
                continue;
            }
            let source = sources.get(&path).copied().unwrap_or(path);
            if geometry.contains_key(&source) {
                continue;
            }
            let recapture = full || dirty_sources.contains(&source);
            let previous = records.get(&source).and_then(|r| r.mesh.clone());
            let mesh = match previous {
                Some(previous) if !recapture => previous,
                previous => capture::mesh(&scene, source, time, previous, &mut update.work)?,
            };
            geometry.insert(source, mesh);
        }
        let mut transforms = self.transforms.clone();
        if full {
            transforms.clear();
        } else {
            for report in &reports {
                transforms.apply_changes(&scene, report);
            }
        }
        transforms.set_time(time);
        let before = transforms.stats();
        let mut bindings = BindingCache::new(
            self.options.material_purpose.clone(),
            self.options.binding_options,
        );
        let added: BTreeSet<_> = update.added.iter().map(|h| h.incarnation).collect();
        for &(path, kind) in &inventory {
            let changes = dirty[&path];
            let record = records.get_mut(&path).expect("staged record");
            let mut changed = SceneComponents::default();
            if changes.transform {
                replace_equal(
                    &mut record.transform,
                    capture::transform(&scene, path, &mut transforms)?,
                    &mut changed.transform,
                );
            }
            if changes.appearance {
                replace_equal(
                    &mut record.appearance,
                    capture::appearance(&scene, path, time),
                    &mut changed.appearance,
                );
            }
            if kind == SceneKind::Mesh {
                let source = sources.get(&path).copied().unwrap_or(path);
                let mesh = geometry[&source].clone();
                changed.geometry = record
                    .mesh
                    .as_ref()
                    .is_none_or(|old| !Arc::ptr_eq(old, &mesh));
                record.mesh = Some(mesh);
            }
            if kind == SceneKind::PointInstancer && changes.point_instances {
                let points = capture::point_instances(&scene, path, time, &mut update.work)?;
                if record.point_instances.as_deref() != Some(&points) {
                    record.point_instances = Some(Arc::new(points));
                    changed.point_instances = true;
                }
            }
            if (changes.primvars || changes.point_instances) && kind != SceneKind::NativeInstance {
                let sites = record.mesh.as_ref().map_or_else(
                    || {
                        let count = record
                            .point_instances
                            .as_ref()
                            .map_or(0, |p| p.transforms.source_len());
                        MeshSites {
                            points: count,
                            faces: count,
                            corners: count,
                        }
                    },
                    |mesh| capture::sites(mesh.polygons.mesh()),
                );
                let (primvars, id_targets) =
                    capture::primvars(&scene, path, time, sites, &mut update.work)?;
                replace_equal(&mut record.primvars, primvars, &mut changed.primvars);
                changed.primvars |= record.has_id_target_primvars != id_targets;
                record.has_id_target_primvars = id_targets;
            }
            if changes.materials {
                replace_equal(
                    &mut record.materials,
                    capture::materials(
                        &scene,
                        path,
                        time,
                        kind == SceneKind::Mesh,
                        &mut bindings,
                        &mut update.work,
                    )?,
                    &mut changed.materials,
                );
            }
            if changed.any() && !added.contains(&record.handle.incarnation) {
                update.changed.push(SceneChange {
                    handle: record.handle.clone(),
                    components: changed,
                });
            }
        }
        let mut change_indices: BTreeMap<_, _> = update
            .changed
            .iter()
            .enumerate()
            .map(|(i, change)| (change.handle.incarnation, i))
            .collect();
        for &(path, _) in &inventory {
            let source = sources
                .get(&path)
                .copied()
                .filter(|source| *source != path)
                .map(|source| self_handle(&records, source));
            let members = native.get(&path).map(|members| SceneNativeInstance {
                members: members
                    .iter()
                    .map(|(relative_path, occurrence, source)| SceneNativeMember {
                        relative_path: relative_path.clone(),
                        occurrence: self_handle(&records, *occurrence),
                        source: self_handle(&records, *source),
                    })
                    .collect(),
            });
            let record = records.get_mut(&path).expect("staged record");
            let mut components = SceneComponents::default();
            if source != record.geometry_source {
                record.geometry_source = source;
                components.geometry = true;
            }
            if members.as_ref() != record.native_instance.as_deref() {
                record.native_instance = members.map(Arc::new);
                components.native_instance = true;
            }
            if components.any() && !added.contains(&record.handle.incarnation) {
                if let Some(&index) = change_indices.get(&record.handle.incarnation) {
                    let change = &mut update.changed[index];
                    change.components.geometry |= components.geometry;
                    change.components.native_instance |= components.native_instance;
                } else {
                    change_indices.insert(record.handle.incarnation, update.changed.len());
                    update.changed.push(SceneChange {
                        handle: record.handle.clone(),
                        components,
                    });
                }
            }
        }
        update.work.local_transforms = transforms.stats().local_computed - before.local_computed;
        update.work.world_transforms = transforms.stats().world_computed - before.world_computed;
        let mut materials = BTreeMap::new();
        self.networks.set_time(time);
        let captures = self.networks.stats().captures;
        let mut referenced = BTreeSet::new();
        for record in records.values() {
            referenced.extend(record.materials.binding.material);
            if let Some(subsets) = &record.materials.subsets {
                referenced.extend(subsets.subsets.iter().filter_map(|s| s.material.material));
            }
        }
        for material in referenced {
            let sample = self
                .networks
                .get(&scene, material, self.options.material_terminal)
                .map_err(|source| SceneError::MaterialNetwork {
                    prim: material,
                    source,
                })?;
            if reset.is_some()
                || self
                    .materials
                    .get(&material)
                    .is_none_or(|old| old.revisions != sample.revisions)
            {
                update.materials.push(material);
            }
            materials.insert(material, sample);
        }
        update.work.material_networks = self.networks.stats().captures - captures;
        update.removed_materials = self
            .materials
            .keys()
            .filter(|path| !materials.contains_key(path))
            .copied()
            .collect();
        for path in &update.removed_materials {
            self.networks.remove(*path);
        }
        self.handles = records
            .values()
            .map(|r| (r.handle.incarnation, r.path))
            .collect();
        self.records = records;
        self.materials = materials;
        self.transforms = transforms;
        self.next_handle = next_handle;
        self.identity = Some(identity);
        self.cursor = Some(cursor);
        self.time = Some(time);
        Ok(update)
    }
}

fn replace_equal<T: PartialEq>(target: &mut Arc<T>, value: T, changed: &mut bool) {
    if **target != value {
        *target = Arc::new(value);
        *changed = true;
    }
}
fn self_handle(records: &BTreeMap<PathId, SceneRecord>, path: PathId) -> SceneHandle {
    records[&path].handle.clone()
}
fn empty(path: PathId, kind: SceneKind, handle: SceneHandle) -> SceneRecord {
    SceneRecord {
        handle,
        path,
        kind,
        transform: Arc::new(SceneTransform {
            local: crate::LocalTransform::identity(),
            local_to_world: crate::gf::IDENTITY,
        }),
        appearance: Arc::new(SceneAppearance {
            visibility: crate::Visibility::Inherited,
            purpose: crate::usd_geom::ImageablePurpose::Default,
            effective_visibility: crate::Visibility::Visible,
        }),
        mesh: None,
        primvars: Arc::new(Vec::new()),
        has_id_target_primvars: false,
        materials: Arc::new(SceneMaterials {
            binding: crate::BoundMaterial::default(),
            subsets: None,
        }),
        point_instances: None,
        native_instance: None,
        geometry_source: None,
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct Dirty {
    transform: bool,
    appearance: bool,
    geometry: bool,
    primvars: bool,
    materials: bool,
    point_instances: bool,
}
fn route(
    scene: &Scene<'_>,
    path: PathId,
    kind: SceneKind,
    old: Option<&SceneRecord>,
    reports: &[Changes],
    full: bool,
    time: bool,
) -> Dirty {
    let mut dirty = Dirty {
        transform: full || time,
        appearance: full || time,
        geometry: full || time,
        primvars: full
            || time
            || (old.is_some_and(|r| r.has_id_target_primvars) && !reports.is_empty()),
        materials: full || time || !reports.is_empty(),
        point_instances: full || time,
    };
    let paths = scene.store().paths();
    for report in reports {
        for &root in report
            .created
            .iter()
            .chain(&report.removed)
            .chain(&report.resynced)
        {
            if paths.resolve(root).is_prefix_of(paths.resolve(path)) {
                dirty.transform = true;
                dirty.appearance = true;
                dirty.geometry = true;
                dirty.primvars = true;
                dirty.point_instances = true;
            }
            if old
                .and_then(|r| r.point_instances.as_ref())
                .is_some_and(|p| {
                    p.prototypes.iter().any(|prototype| {
                        paths.resolve(root).is_prefix_of(paths.resolve(*prototype))
                    })
                })
            {
                dirty.point_instances = true;
            }
        }
        for &prim in &report.changed_info_only {
            let ancestor = paths.resolve(prim).is_prefix_of(paths.resolve(path));
            let prototype = old
                .and_then(|r| r.point_instances.as_ref())
                .is_some_and(|p| p.prototypes.contains(&prim));
            let Some(properties) = report.properties_for(prim) else {
                if ancestor {
                    dirty.transform = true;
                    dirty.appearance = true;
                    dirty.primvars = true;
                    dirty.point_instances = true;
                }
                if prim == path {
                    dirty.geometry = true;
                    dirty.point_instances = true;
                }
                if prototype {
                    dirty.point_instances = true;
                }
                continue;
            };
            for property in properties {
                let name = scene.store().tokens().resolve(property.name);
                if ancestor && (name == "xformOpOrder" || name.starts_with("xformOp:")) {
                    dirty.transform = true;
                }
                if ancestor
                    && (name == "purpose" || name.ends_with("Visibility") || name == "visibility")
                {
                    dirty.appearance = true;
                }
                if ancestor && name.starts_with("primvars:") {
                    dirty.primvars = true;
                }
                if prim == path && geometry_property(name) {
                    dirty.geometry = true;
                    dirty.primvars = true;
                }
                if kind == SceneKind::PointInstancer
                    && ((prim == path
                        && !name.starts_with("material:")
                        && !name.starts_with("xformOp"))
                        || (prototype && (name == "xformOpOrder" || name.starts_with("xformOp:"))))
                {
                    dirty.point_instances = true;
                }
            }
        }
    }
    dirty
}
fn geometry_property(name: &str) -> bool {
    matches!(
        name,
        "points"
            | "faceVertexCounts"
            | "faceVertexIndices"
            | "holeIndices"
            | "normals"
            | "orientation"
            | "doubleSided"
            | "subdivisionScheme"
            | "interpolateBoundary"
            | "faceVaryingLinearInterpolation"
            | "triangleSubdivisionRule"
            | "cornerIndices"
            | "cornerSharpnesses"
            | "creaseIndices"
            | "creaseLengths"
            | "creaseSharpnesses"
    )
}
type NativeMembers = BTreeMap<PathId, Vec<(Vec<TokenId>, PathId, PathId)>>;
fn native_sources(
    scene: &Scene<'_>,
    current: &BTreeSet<PathId>,
) -> (BTreeMap<PathId, PathId>, NativeMembers) {
    let mut sources = BTreeMap::new();
    let mut native = BTreeMap::new();
    for prototype in scene.stage().prototypes() {
        for &instance in prototype.instances() {
            if !current.contains(&instance) {
                continue;
            }
            let mut members = Vec::new();
            for member in prototype.prims() {
                let path = scene
                    .store()
                    .paths()
                    .resolve(instance)
                    .join(member.relative_path());
                if let Some(occurrence) = scene.store().paths().lookup(&path)
                    && current.contains(&occurrence)
                    && current.contains(&member.representative())
                {
                    if scene.is_a(occurrence, "Mesh") {
                        sources.insert(occurrence, member.representative());
                    }
                    members.push((
                        member.relative_path().to_vec(),
                        occurrence,
                        member.representative(),
                    ));
                }
            }
            native.insert(instance, members);
        }
    }
    (sources, native)
}
