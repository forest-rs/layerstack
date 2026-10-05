// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{
    BindingOptions, BoundMaterial, LocalTransform, MaterialBindingSubsets, MaterialPurpose,
    MaterialSubsetError, MeshPublicationError, MeshValidationWork, ValidatedMesh, Visibility,
    point_instancer::{InstanceTransform, InstanceTransforms, PointInstancerError},
    primvar::PrimvarError,
    usd_geom::ImageablePurpose,
};
use alloc::{string::String, sync::Arc, vec::Vec};
use layerstack::{ArrayReadError, PathId, PropertyPath, Time, TokenId, Value};

/// Immutable policies for one observer. All-purpose binding is the default.
#[derive(Clone, Debug)]
pub struct SceneOptions {
    /// Binding purpose, with ordinary USD all-purpose fallback.
    pub material_purpose: MaterialPurpose,
    /// Whether legacy bindings without `MaterialBindingAPI` participate.
    pub binding_options: BindingOptions,
    /// Ordered shading render contexts; an empty list selects universal outputs.
    pub render_contexts: Vec<String>,
    /// Material output to retain as an upstream graph.
    pub material_terminal: crate::shading::MaterialTerminal,
}
impl Default for SceneOptions {
    fn default() -> Self {
        Self {
            material_purpose: MaterialPurpose::All,
            binding_options: BindingOptions::default(),
            render_contexts: Vec::new(),
            material_terminal: crate::shading::MaterialTerminal::Surface,
        }
    }
}

/// Observer-local incarnation. Handles from another observer never resolve.
/// Deletion, recreation and history recovery retire old handles permanently.
#[derive(Clone, Debug)]
pub struct SceneHandle {
    pub(super) observer: Arc<()>,
    pub(super) incarnation: u64,
}
impl PartialEq for SceneHandle {
    fn eq(&self, other: &Self) -> bool {
        self.incarnation == other.incarnation && Arc::ptr_eq(&self.observer, &other.observer)
    }
}
impl Eq for SceneHandle {}
impl SceneHandle {
    /// Monotonic observer-local number, useful for diagnostics, not serialization.
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }
}

/// Extracted role. Native instance roots can also have mesh or instancer data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneKind {
    /// Checked polygon control geometry; subdivision is not evaluated.
    Mesh,
    /// Retained point-instance inputs and prototype targets.
    PointInstancer,
    /// A native instance root with occurrence-to-source links.
    NativeInstance,
}

/// A composed prim retained independently of the stage's lifetime.
#[derive(Clone, Debug)]
pub struct SceneRecord {
    /// Observer-local identity.
    pub handle: SceneHandle,
    /// Concrete occurrence path in the observer's store domain.
    pub path: PathId,
    /// Supported role of the prim.
    pub kind: SceneKind,
    /// Local and world transforms; USD warnings remain in the local result.
    pub transform: Arc<SceneTransform>,
    /// Inherited visibility and purpose, evaluated in occurrence namespace.
    pub appearance: Arc<SceneAppearance>,
    /// Shared polygon geometry, when this prim is a mesh.
    pub mesh: Option<Arc<SceneMesh>>,
    /// Indexed local or inherited primvars, never flattened implicitly.
    pub primvars: Arc<Vec<ScenePrimvar>>,
    /// Whether primvar capture inspected string ID-target relationships, including
    /// unresolved ones. Their forwarding can depend outside this namespace.
    pub has_id_target_primvars: bool,
    /// Occurrence-local material binding and validated face subsets.
    pub materials: Arc<SceneMaterials>,
    /// Captured point-instance data, when applicable.
    pub point_instances: Option<Arc<ScenePointInstances>>,
    /// Native instance-root member links, when applicable.
    pub native_instance: Option<Arc<SceneNativeInstance>>,
    /// Representative geometry occurrence for a native instance descendant.
    /// Materials and inherited primvars still belong to `path`.
    pub geometry_source: Option<SceneHandle>,
}

/// Row-vector transforms in USD's convention (translation in the final row).
#[derive(Clone, Debug)]
pub struct SceneTransform {
    /// Local transform and its reset-stack/warning information.
    pub local: LocalTransform,
    /// Local-to-world matrix, honoring transform reset boundaries.
    pub local_to_world: [[f64; 4]; 4],
}
impl PartialEq for SceneTransform {
    fn eq(&self, other: &Self) -> bool {
        fn same(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> bool {
            a.iter()
                .flatten()
                .zip(b.iter().flatten())
                .all(|(a, b)| a.to_bits() == b.to_bits())
        }
        self.local.resets_xform_stack == other.local.resets_xform_stack
            && self.local.problems == other.local.problems
            && same(&self.local.matrix, &other.local.matrix)
            && same(&self.local_to_world, &other.local_to_world)
    }
}
/// Inherited imageable state; records remain present when invisible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneAppearance {
    /// Ordinary inherited visibility at the explicit extraction time.
    pub visibility: Visibility,
    /// Inherited authored purpose or the schema fallback.
    pub purpose: ImageablePurpose,
    /// Purpose-specific visibility, including `VisibilityAPI` inheritance.
    pub effective_visibility: Visibility,
}

/// Checked polygon geometry. No triangles or draw ranges are allocated.
#[derive(Clone, Debug)]
pub struct SceneMesh {
    /// Finite points and checked counts/indices with retained validation/extent.
    pub polygons: ValidatedMesh,
    /// Validated hole face indices, retaining native storage.
    pub holes: Arc<Vec<i32>>,
    /// Optional authored normals, checked against their interpolation domain.
    pub normals: Option<Arc<Vec<[f32; 3]>>>,
    /// Normal interpolation; unauthored metadata follows USD's vertex fallback.
    pub normals_interpolation: String,
    /// Orientation, subdivision settings, double-sidedness and crease/corner
    /// inputs. These are retained control data, not evaluated subdivision.
    pub controls: Vec<(String, Value)>,
}

/// One indexed primvar, including its original value owner and source property.
#[derive(Clone, Debug)]
pub struct ScenePrimvar {
    /// Name without the `primvars:` prefix.
    pub name: String,
    /// Property supplying a local or inherited value.
    pub source: PropertyPath,
    /// Composed interpolation domain.
    pub interpolation: String,
    /// Values per indexed element.
    pub element_size: usize,
    /// Values in source order; strings and other nonnumeric values are retained.
    pub value: Value,
    /// Optional mapping into value elements; never silently expanded.
    pub indices: Option<Arc<Vec<i32>>>,
    /// Placeholder element index, or -1 when unauthored.
    pub unauthored_values_index: i32,
}
impl PartialEq for ScenePrimvar {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.source == other.source
            && self.interpolation == other.interpolation
            && self.element_size == other.element_size
            && self.value.same_representation(&other.value)
            && self.indices == other.indices
            && self.unauthored_values_index == other.unauthored_values_index
    }
}

/// Resolved bindings with winning relationship provenance and missing targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneMaterials {
    /// Whole-prim binding, including its unresolved target when any.
    pub binding: BoundMaterial,
    /// Validated materialBind face family for meshes.
    pub subsets: Option<MaterialBindingSubsets>,
}
impl SceneMaterials {
    /// Distinct resolved materials, including face-subset overrides.
    pub fn material_paths(&self) -> Vec<PathId> {
        let mut paths = Vec::new();
        paths.extend(self.binding.material);
        if let Some(subsets) = &self.subsets {
            paths.extend(subsets.subsets.iter().filter_map(|s| s.material.material));
        }
        paths.sort_unstable();
        paths.dedup();
        paths
    }
}

/// Immutable point-instance inputs. Matrices are computed lazily during iteration.
#[derive(Clone, Debug)]
pub struct ScenePointInstances {
    /// Unmasked input snapshot; iteration preserves every original index and ID.
    pub transforms: InstanceTransforms,
    /// Ordered relationship targets addressed by each `prototype_index`.
    pub prototypes: Vec<PathId>,
    /// Original-array mask; empty means every instance survives.
    pub mask: Arc<Vec<bool>>,
}
impl PartialEq for ScenePointInstances {
    fn eq(&self, other: &Self) -> bool {
        self.prototypes == other.prototypes
            && self.mask == other.mask
            && self.transforms.same_inputs(&other.transforms)
    }
}
impl ScenePointInstances {
    /// Surviving transforms in original order, retaining authored IDs and indices.
    pub fn surviving(&self) -> impl Iterator<Item = InstanceTransform> + '_ {
        self.transforms
            .iter()
            .filter(|item| self.mask.is_empty() || self.mask[item.index])
    }
}
/// One member of a shared native prototype, addressed in occurrence namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneNativeMember {
    /// Relative USD path components below the instance root.
    pub relative_path: Vec<TokenId>,
    /// Extracted member of this occurrence.
    pub occurrence: SceneHandle,
    /// Representative extracted member supplying shared base geometry.
    pub source: SceneHandle,
}
/// Native instancing expressed through stable observer-local record handles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneNativeInstance {
    /// Supported descendants only; instance-root local data is never shared.
    pub members: Vec<SceneNativeMember>,
}

/// Independent component changes on a surviving record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneComponents {
    /// Local/world transform changed.
    pub transform: bool,
    /// Inherited visibility/purpose changed.
    pub appearance: bool,
    /// Geometry recaptured or its source link changed.
    pub geometry: bool,
    /// Primvar values, indexing, metadata or source changed.
    pub primvars: bool,
    /// Binding/subset result changed.
    pub materials: bool,
    /// Captured point inputs, IDs, mask or prototypes changed.
    pub point_instances: bool,
    /// Native occurrence/source member links changed.
    pub native_instance: bool,
}
impl SceneComponents {
    /// Whether any component changed.
    pub fn any(self) -> bool {
        self != Self::default()
    }
}
/// A component update, resolved through [`SceneObserver::record`](super::SceneObserver::record).
#[derive(Clone, Debug)]
pub struct SceneChange {
    /// Surviving observer-local record.
    pub handle: SceneHandle,
    /// Components changed during this extraction.
    pub components: SceneComponents,
}
/// Why all prior handles were retired and the current scene was reconstructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneReset {
    /// Composed change evidence exceeded its retention budget.
    HistoryLost,
    /// The host supplied a different live stage in the same store domain.
    DifferentStage,
}
/// Work performed during one successful update; counts are payload entries/tasks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneWork {
    /// Active namespace prims inspected for supported records.
    pub namespace_prims: usize,
    /// Retained records checked while routing changes.
    pub routed_records: usize,
    /// Polygon snapshots captured and validated (source instances count once).
    pub meshes: usize,
    /// Mesh input snapshots read to test source-owner reuse, including reuse hits.
    pub mesh_inputs: usize,
    /// Point/normal/topology/hole entries scanned when owners differ. Pointer hits
    /// count zero; subdivision control comparison is excluded from this count.
    pub mesh_comparison_entries: usize,
    /// Geometry entries validated during those captures.
    pub mesh_validation: MeshValidationWork,
    /// Primvar records captured, including inherited values.
    pub primvars: usize,
    /// Point-instancer snapshots validated.
    pub point_instancers: usize,
    /// Original point-instance entries checked, before masks.
    pub point_instances: usize,
    /// Record bindings/subset families resolved.
    pub material_assignments: usize,
    /// Local transforms computed by the transform cache.
    pub local_transforms: usize,
    /// World matrices computed by the transform cache.
    pub world_transforms: usize,
    /// Records copied while staging an atomic update (shared owners are cloned).
    pub staged_records: usize,
    /// Supported native prototype member links reconstructed for occurrences.
    pub native_members: usize,
    /// Upstream material graphs captured; cache hits do not count.
    pub material_networks: u64,
}
/// Complete change set for one explicit extraction time and composed revision.
#[derive(Clone, Debug)]
pub struct SceneUpdate {
    /// Newly created handles; inspect their complete records on the observer.
    pub added: Vec<SceneHandle>,
    /// Surviving handles with independent component updates.
    pub changed: Vec<SceneChange>,
    /// Permanently retired handles, including reset/deletion/recreation.
    pub removed: Vec<SceneHandle>,
    /// Referenced material graphs whose cache-local component revisions changed.
    /// Inspect them with [`SceneObserver::material`](super::SceneObserver::material).
    pub materials: Vec<PathId>,
    /// Material graphs no longer referenced by any extracted assignment.
    pub removed_materials: Vec<PathId>,
    /// Explicit full-reconstruction cause, if any.
    pub reset: Option<SceneReset>,
    /// Last composed revision consumed successfully.
    pub revision: u64,
    /// Evaluation time including interpolation mode.
    pub time: Time,
    /// Domain work for this update.
    pub work: SceneWork,
}
impl SceneUpdate {
    /// Whether no consumer-visible records or components changed.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.changed.is_empty()
            && self.removed.is_empty()
            && self.materials.is_empty()
            && self.removed_materials.is_empty()
            && self.reset.is_none()
    }
}

/// A capture failure. The observer keeps its previous successful scene intact.
#[derive(Clone, Debug)]
pub enum SceneError {
    /// Store interners differ from the observer or currently composed stage.
    ForeignStore,
    /// Numeric time is nonfinite.
    InvalidTime,
    /// The stage lacks the required geometry/shading schema registry.
    MissingSchemas,
    /// A renderable geometry schema outside the supported mesh/instancer slice.
    UnsupportedGeometry {
        /// Concrete prim.
        prim: PathId,
        /// Composed type name.
        type_name: String,
    },
    /// Missing or incompatible required attribute.
    MissingAttribute(PropertyPath),
    /// A required standard property name is absent from the supplied registry.
    MissingProperty {
        /// Requested prim.
        prim: PathId,
        /// Missing property spelling.
        name: String,
    },
    /// Retained storage failed to decode at a concrete property.
    Decode {
        /// Concrete property.
        property: PropertyPath,
        /// Original storage failure.
        source: ArrayReadError,
    },
    /// Polygon or attribute cardinality is invalid.
    Mesh {
        /// Concrete mesh.
        prim: PathId,
        /// Validation failure.
        source: MeshPublicationError,
    },
    /// Invalid hole face index.
    Hole {
        /// Concrete mesh.
        prim: PathId,
        /// Invalid authored face index.
        index: i32,
    },
    /// Primvar decoding/indexing failed.
    Primvar {
        /// Geometry occurrence.
        prim: PathId,
        /// Primvar source name.
        name: String,
        /// Validation failure.
        source: PrimvarError,
    },
    /// Material subset family failed validation.
    Materials {
        /// Concrete geometry.
        prim: PathId,
        /// Validation failure.
        source: MaterialSubsetError,
    },
    /// Invalid point instancer; no partial transforms are committed.
    PointInstancer {
        /// Concrete instancer.
        prim: PathId,
        /// Validation failure.
        source: PointInstancerError,
    },
    /// A transform cannot be computed.
    Transform(PathId),
    /// Upstream material request failed; graph issues themselves remain in samples.
    MaterialNetwork {
        /// Requested material.
        prim: PathId,
        /// Request failure.
        source: crate::shading::MaterialCaptureError,
    },
}
impl core::fmt::Display for SceneError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "scene extraction failed: {self:?}")
    }
}
impl core::error::Error for SceneError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Decode { source, .. } => Some(source),
            Self::Mesh { source, .. } => Some(source),
            Self::Primvar { source, .. } => Some(source),
            Self::Materials { source, .. } => Some(source),
            Self::PointInstancer { source, .. } => Some(source),
            Self::MaterialNetwork { source, .. } => Some(source),
            _ => None,
        }
    }
}
