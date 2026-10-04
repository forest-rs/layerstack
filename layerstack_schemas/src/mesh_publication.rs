// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validated polygon-mesh publication into a caller-owned stage and edit target.
//!
//! Producers retain [`GeneratedMesh`] buffers and the [`MeshPublication::properties`]
//! manifest after successful application. Pass that manifest into the next update
//! to remove obsolete producer-owned properties. Other properties and opinions in
//! other layers are preserved. There is no implicit stage, edit target or asset cache.
//! Geometry is published at an editable authored source; instance proxies are rejected.
//! Runtime prototype IDs are snapshot identities, not publication addresses or content hashes.

use crate::subset::{MeshTopologyError, validate_mesh_topology};
use alloc::{format, string::String, sync::Arc, vec::Vec};
use layerstack::edit::{EditTarget, Transaction};
use layerstack::{
    LayerStore, PathId, PropertyPath, PropertySpec, PropertyType, Specifier, Stage, TypedArray,
    Value,
};

// SDF_VALUE_TYPES (`pxr/usd/sdf/types.h`); roles share native storage but keep
// their authored type names. AOUSD Core §6.2 (values), §7.6.4.1.1 (typeName).
fn numeric_type_matches(name: &str, array: &TypedArray) -> bool {
    match array {
        TypedArray::Bool(_) => name == "bool",
        TypedArray::UChar(_) => name == "uchar",
        TypedArray::Int(_) => name == "int",
        TypedArray::UInt(_) => name == "uint",
        TypedArray::Int64(_) => name == "int64",
        TypedArray::UInt64(_) => name == "uint64",
        TypedArray::Half(_) => name == "half",
        TypedArray::Float(_) => name == "float",
        TypedArray::Double(_) => name == "double",
        TypedArray::TimeCode(_) => name == "timecode",
        TypedArray::Vec2f(_) => matches!(name, "float2" | "texCoord2f"),
        TypedArray::Vec2d(_) => matches!(name, "double2" | "texCoord2d"),
        TypedArray::Vec2h(_) => matches!(name, "half2" | "texCoord2h"),
        TypedArray::Vec3f(_) => matches!(
            name,
            "float3" | "point3f" | "vector3f" | "normal3f" | "color3f" | "texCoord3f"
        ),
        TypedArray::Vec3d(_) => matches!(
            name,
            "double3" | "point3d" | "vector3d" | "normal3d" | "color3d" | "texCoord3d"
        ),
        TypedArray::Vec3h(_) => matches!(
            name,
            "half3" | "point3h" | "vector3h" | "normal3h" | "color3h" | "texCoord3h"
        ),
        TypedArray::Vec4f(_) => matches!(name, "float4" | "color4f"),
        TypedArray::Vec4d(_) => matches!(name, "double4" | "color4d"),
        TypedArray::Vec4h(_) => matches!(name, "half4" | "color4h"),
        TypedArray::Vec2i(_) => name == "int2",
        TypedArray::Vec3i(_) => name == "int3",
        TypedArray::Vec4i(_) => name == "int4",
        TypedArray::Quatf(_) => name == "quatf",
        TypedArray::Quatd(_) => name == "quatd",
        TypedArray::Quath(_) => name == "quath",
        TypedArray::Matrix2d(_) => name == "matrix2d",
        TypedArray::Matrix3d(_) => name == "matrix3d",
        TypedArray::Matrix4d(_) => matches!(name, "matrix4d" | "frame4d"),
        TypedArray::Deferred(_) => false, // The caller materializes before checking.
    }
}

/// Interpolation-site counts of a polygon mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshSites {
    /// Number of points (`vertex` and `varying`).
    pub points: usize,
    /// Number of polygons (`uniform`).
    pub faces: usize,
    /// Number of polygon corners (`faceVarying`).
    pub corners: usize,
}

/// Invalid generated geometry or publication site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeshPublicationError {
    /// Count/index consistency or point-index bounds failed.
    Topology(MeshTopologyError),
    /// A polygon has fewer than three corners.
    DegenerateFace(usize),
    /// A point contains a nonfinite coordinate.
    NonFinitePoint(usize),
    /// The primvar name is malformed or repeated.
    PrimvarName(String),
    /// Primvar storage does not match its declared numeric array type.
    PrimvarType(String),
    /// Interpolation must name a USD geometry interpolation.
    Interpolation(String),
    /// Element size is zero, too large or does not divide the values.
    ElementSize,
    /// Values or indices do not match interpolation sites.
    Cardinality {
        /// Required number of interpolation elements.
        expected: usize,
        /// Supplied number of elements.
        actual: usize,
    },
    /// A primvar index is negative or out of bounds.
    Index {
        /// Position of the invalid index.
        position: usize,
        /// Authored index.
        index: i64,
    },
    /// The target layer is missing or the edit target does not map this prim.
    InvalidTarget,
    /// A native-instance descendant is a read-only instance proxy.
    InstanceProxy(PathId),
    /// The mapped authored site explicitly has another type, or an untyped
    /// existing site cannot be validated as a composed `Mesh`.
    NotMesh(PathId),
    /// A producer-owned property has another declaration at this authored site.
    PropertyConflict(String),
}
impl core::fmt::Display for MeshPublicationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid mesh publication: {self:?}")
    }
}
impl core::error::Error for MeshPublicationError {}

/// Checks primvar cardinality and returns the number of complete value elements.
/// An indexed primvar has one index per site and `element_size` values per selected
/// element. No allocation or flattening occurs. `constant` requires one element,
/// including for an empty mesh. AOUSD Core §7.6.4 (attribute specs);
/// OpenUSD `UsdGeomPrimvar::GetElementSize` and mesh interpolation rules.
pub fn validate_mesh_primvar_cardinality(
    values: usize,
    element_size: usize,
    interpolation: &str,
    sites: MeshSites,
    indices: Option<usize>,
) -> Result<usize, MeshPublicationError> {
    let expected = match interpolation {
        "constant" => 1,
        "uniform" => sites.faces,
        "vertex" | "varying" => sites.points,
        "faceVarying" => sites.corners,
        other => return Err(MeshPublicationError::Interpolation(other.into())),
    };
    if element_size == 0
        || i32::try_from(element_size).is_err()
        || !values.is_multiple_of(element_size)
    {
        return Err(MeshPublicationError::ElementSize);
    }
    let elements = values / element_size;
    let actual = indices.unwrap_or(elements);
    if actual != expected {
        return Err(MeshPublicationError::Cardinality { expected, actual });
    }
    Ok(elements)
}

/// Validates borrowed signed or unsigned primvar indices without copying them.
/// Equal-valued elements with different indices retain their distinct seams.
/// OpenUSD `UsdGeomPrimvar::ComputeFlattened`; AOUSD Core §7.6.4 (attribute specs).
pub fn validate_mesh_primvar_indices<I>(
    indices: I,
    elements: usize,
) -> Result<(), MeshPublicationError>
where
    I: IntoIterator,
    I::Item: Into<i64>,
{
    for (position, index) in indices.into_iter().enumerate() {
        let index = index.into();
        if usize::try_from(index).map_or(true, |i| i >= elements) {
            return Err(MeshPublicationError::Index { position, index });
        }
    }
    Ok(())
}

/// A numeric primvar, including indexed normal and UV seams.
/// `value` should hold a [`TypedArray`]; cloning this description shares its buffers.
#[derive(Clone, Debug)]
pub struct MeshPrimvar {
    /// Full USD name, such as `primvars:st` or `primvars:normals`.
    pub name: String,
    /// Array declaration, including its semantic role (`normal3f`, `texCoord2f`, …).
    pub type_name: PropertyType,
    /// Native numeric array matching the declaration's scalar representation.
    pub value: Value,
    /// `constant`, `uniform`, `vertex`, `varying` or `faceVarying`.
    pub interpolation: String,
    /// Scalar-array entries grouped into one interpolation element; normally one.
    pub element_size: usize,
    /// Optional index per interpolation site, transferred by shared owner.
    pub indices: Option<Arc<Vec<i32>>>,
}

/// A complete polygon-mesh snapshot. Native storage is shared through publication.
/// Use a new snapshot for point or topology changes; unchanged buffers can retain
/// their owners. No geometry-content identity is inferred from a path or prototype ID.
#[derive(Clone, Debug, Default)]
pub struct GeneratedMesh {
    /// Object-space points.
    pub points: Arc<Vec<[f32; 3]>>,
    /// Polygon corner counts; each polygon needs at least three corners.
    pub face_vertex_counts: Arc<Vec<i32>>,
    /// Point index per polygon corner.
    pub face_vertex_indices: Arc<Vec<i32>>,
    /// Producer-owned numeric primvars; indexed normals use `primvars:normals`.
    pub primvars: Vec<MeshPrimvar>,
}

impl GeneratedMesh {
    /// Consumes and validates one immutable snapshot for repeated publication.
    /// Numeric owners are retained; deferred primvars decode once. Subsequent
    /// preparations do not scan geometry or derive extent. The cached two-point
    /// extent can be compared by value; geometry arrays use owner identity.
    pub fn into_validated(mut self) -> Result<ValidatedMesh, MeshPublicationError> {
        for primvar in &mut self.primvars {
            if let Value::TypedArray(array) = &mut primvar.value {
                *array = array
                    .try_materialize()
                    .map_err(|_| MeshPublicationError::PrimvarType(primvar.name.clone()))?
                    .clone();
            }
        }
        self.validate()?;
        let extent = derive_extent(&self.points);
        let work = MeshValidationWork {
            points: self.points.len(),
            faces: self.face_vertex_counts.len(),
            corners: self.face_vertex_indices.len(),
            primvar_arrays: self.primvars.len(),
            primvar_indices: self
                .primvars
                .iter()
                .filter_map(|p| p.indices.as_ref())
                .map(|i| i.len())
                .sum(),
            extent_points: self.points.len(),
        };
        Ok(ValidatedMesh(Arc::new(ValidatedMeshData {
            mesh: self,
            extent,
            work,
        })))
    }

    /// Validates topology, finite points, declaration/storage compatibility,
    /// interpolation, element size, indices and cardinality before publication.
    /// Deferred arrays may decode here; decoding failures reject the update.
    pub fn validate(&self) -> Result<(), MeshPublicationError> {
        validate_mesh_topology(
            &self.face_vertex_indices,
            &self.face_vertex_counts,
            self.points.len(),
        )
        .map_err(MeshPublicationError::Topology)?;
        if let Some(face) = self.face_vertex_counts.iter().position(|&n| n < 3) {
            return Err(MeshPublicationError::DegenerateFace(face));
        }
        if let Some(point) = self
            .points
            .iter()
            .position(|p| p.iter().any(|v| !v.is_finite()))
        {
            return Err(MeshPublicationError::NonFinitePoint(point));
        }
        let sites = MeshSites {
            points: self.points.len(),
            faces: self.face_vertex_counts.len(),
            corners: self.face_vertex_indices.len(),
        };
        for (i, primvar) in self.primvars.iter().enumerate() {
            if !primvar.name.starts_with("primvars:")
                || primvar.name.ends_with(":indices")
                || primvar.name.split(':').any(|s| {
                    s.is_empty()
                        || s.starts_with(|c: char| c.is_ascii_digit())
                        || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
                || self.primvars[..i].iter().any(|p| p.name == primvar.name)
            {
                return Err(MeshPublicationError::PrimvarName(primvar.name.clone()));
            }
            let Value::TypedArray(array) = &primvar.value else {
                return Err(MeshPublicationError::PrimvarType(primvar.name.clone()));
            };
            let array = array
                .try_materialize()
                .map_err(|_| MeshPublicationError::PrimvarType(primvar.name.clone()))?;
            if !primvar.type_name.is_array
                || !numeric_type_matches(&primvar.type_name.type_name, array)
                || core::mem::discriminant(&array.element_kind())
                    != core::mem::discriminant(&primvar.type_name.default_scalar)
            {
                return Err(MeshPublicationError::PrimvarType(primvar.name.clone()));
            }
            let elements = validate_mesh_primvar_cardinality(
                array.len(),
                primvar.element_size,
                &primvar.interpolation,
                sites,
                primvar.indices.as_ref().map(|i| i.len()),
            )?;
            if let Some(indices) = &primvar.indices {
                validate_mesh_primvar_indices(indices.iter().copied(), elements)?;
            }
        }
        Ok(())
    }

    /// Prepares creation or replacement through an existing edit target.
    /// `previous_properties` is this producer's manifest from its last successfully
    /// applied publication at the same authored site. Omitted names are removed
    /// only from that site, including their old samples and metadata. Do not claim
    /// properties another producer owns. Material bindings and unrelated attributes
    /// are outside this manifest and remain editable by other producers.
    ///
    /// This publishes **defaults**, not time samples. Existing samples on retained
    /// owned properties are removed; use schema `_owned_at`/`_shared_at` setters for
    /// animation after publication. Topology and primvar cardinality must also hold
    /// at each sampled time. Edit-target mappings support referenced and variant sites.
    /// An explicit target-local type is authoritative, so an unselected `Mesh`
    /// branch can be updated while a different type is selected. Untyped existing
    /// sites require a composed `Mesh`; explicit non-mesh targets are rejected.
    /// A layer-generation guard rejects application after intervening target edits.
    /// Planning allocates small property/transaction records; buffers are not copied.
    /// A new two-point extent buffer is derived from the points on each preparation.
    /// Changed arrays are compared by representation when owners differ (O(elements));
    /// validation still scans points, topology and indices on unchanged updates.
    /// AOUSD Core §7.6 (authored specs), §12.3 (values), §7.6.4 (attribute specs).
    pub fn prepare(
        &self,
        stage: &Stage,
        store: &mut dyn LayerStore,
        target: &EditTarget,
        path: PathId,
        previous_properties: &[String],
    ) -> Result<MeshPublication, MeshPublicationError> {
        self.validate()?;
        self.prepare_impl(stage, store, target, path, previous_properties, None)
    }

    fn prepare_impl(
        &self,
        stage: &Stage,
        store: &mut dyn LayerStore,
        target: &EditTarget,
        path: PathId,
        previous_properties: &[String],
        retained_extent: Option<&Arc<Vec<[f32; 3]>>>,
    ) -> Result<MeshPublication, MeshPublicationError> {
        let mut ancestor = store.paths().parent(path);
        while let Some(parent) = ancestor {
            if stage.is_instance(parent) {
                return Err(MeshPublicationError::InstanceProxy(path));
            }
            ancestor = store.paths().parent(parent);
        }
        let mapped = target
            .map_to_spec_path(path, store.paths_mut())
            .ok_or(MeshPublicationError::InvalidTarget)?;
        let layer = store
            .layer(target.layer())
            .ok_or(MeshPublicationError::InvalidTarget)?;
        let generation = layer.generation();
        let exists = layer.has_spec_at(&mapped, store.paths());
        // AOUSD Core §8 (spec paths) and §10.5 (variant selection): the
        // mapped authored branch can differ from the selected composed branch.
        let local = layer.prim_at(&mapped, store.paths());
        let type_name = local
            .and_then(|spec| spec.type_name)
            .or_else(|| stage.resolve_type_name(path, store));
        if (local.is_some() || stage.has_prim(path))
            && type_name.map(|name| store.tokens().resolve(name)) != Some("Mesh")
        {
            return Err(MeshPublicationError::NotMesh(path));
        }
        let mut transaction = Transaction::new();
        if !exists {
            let mesh = store.tokens_mut().intern("Mesh");
            transaction.create_prim(target.prim(path), Specifier::Def, Some(mesh));
        }
        let mut attributes = Vec::new();
        let attribute = |ty, value| PropertySpec::typed_attribute(ty).with_default(value);
        let typed = |name, zero, array| {
            attribute(
                PropertyType::new(name, true, zero),
                Value::TypedArray(array),
            )
        };
        attributes.push((
            String::from("points"),
            typed(
                "point3f",
                Value::Vec3f([0.; 3]),
                TypedArray::Vec3f(self.points.clone()),
            ),
        ));
        attributes.push((
            String::from("faceVertexCounts"),
            typed(
                "int",
                Value::Int(0),
                TypedArray::Int(self.face_vertex_counts.clone()),
            ),
        ));
        attributes.push((
            String::from("faceVertexIndices"),
            typed(
                "int",
                Value::Int(0),
                TypedArray::Int(self.face_vertex_indices.clone()),
            ),
        ));
        let extent = retained_extent
            .cloned()
            .unwrap_or_else(|| derive_extent(&self.points));
        attributes.push((
            String::from("extent"),
            typed("float3", Value::Vec3f([0.; 3]), TypedArray::Vec3f(extent)),
        ));
        let subdivision = store.tokens_mut().intern("none");
        attributes.push((
            String::from("subdivisionScheme"),
            attribute(
                PropertyType::new("token", false, Value::Token(subdivision)),
                Value::Token(subdivision),
            ),
        ));
        attributes.last_mut().expect("subdivision").1.variability =
            layerstack::Variability::Uniform;
        for primvar in &self.primvars {
            let mut spec = attribute(primvar.type_name.clone(), primvar.value.clone());
            let interpolation = store.tokens_mut().intern("interpolation");
            let token = store.tokens_mut().intern(&primvar.interpolation);
            spec.set_metadata(interpolation, Value::Token(token));
            let element_size = store.tokens_mut().intern("elementSize");
            spec.set_metadata(
                element_size,
                Value::Int(i32::try_from(primvar.element_size).expect("validated element size")),
            );
            attributes.push((primvar.name.clone(), spec));
            if let Some(indices) = &primvar.indices {
                attributes.push((
                    format!("{}:indices", primvar.name),
                    typed("int", Value::Int(0), TypedArray::Int(indices.clone())),
                ));
            }
        }
        let properties: Vec<String> = attributes.iter().map(|(name, _)| name.clone()).collect();
        for name in previous_properties
            .iter()
            .filter(|name| !properties.contains(name))
        {
            let name = store.tokens_mut().intern(name);
            transaction.remove_spec(target.property(PropertyPath::new(path, name)));
        }
        for (name, desired) in attributes {
            let token = store.tokens_mut().intern(&name);
            let property = PropertyPath::new(path, token);
            let site = target
                .map_property_to_spec_path(property, store.paths_mut())
                .ok_or(MeshPublicationError::InvalidTarget)?;
            let local = store
                .layer(target.layer())
                .and_then(|l| l.property_at(&site, store.paths()));
            if let Some(local) = local {
                if local.kind != desired.kind
                    || local.type_name.as_ref().map(|t| (&t.type_name, t.is_array))
                        != desired
                            .type_name
                            .as_ref()
                            .map(|t| (&t.type_name, t.is_array))
                {
                    return Err(MeshPublicationError::PropertyConflict(name));
                }
                if !local
                    .default
                    .as_ref()
                    .zip(desired.default.as_ref())
                    .is_some_and(|(a, b)| match (a, b) {
                        (Value::TypedArray(a), Value::TypedArray(b))
                            if retained_extent.is_some() && name != "extent" =>
                        {
                            a.shares_storage(b)
                        }
                        _ => a.same_representation(b),
                    })
                {
                    transaction.set_default(
                        target.property(property),
                        desired.default.clone().expect("publication default"),
                    );
                }
                if let Some(samples) = &local.time_samples {
                    for (time, _) in samples.as_slice() {
                        // Transactions take stage time; this authored sample is already in layer time.
                        transaction.remove_time_sample(
                            layerstack::edit::Address::spec(target.layer(), site.clone()),
                            *time,
                        );
                    }
                }
                for field in &desired.metadata {
                    if local.metadata(field.name) != Some(&field.value) {
                        transaction.set_metadata(
                            target.property(property),
                            field.name,
                            field.value.clone(),
                        );
                    }
                }
            } else {
                transaction.create_property(target.property(property), desired);
            }
        }
        if retained_extent.is_some() || !transaction.is_empty() {
            transaction.expect_generation(target.layer(), generation);
        }
        Ok(MeshPublication {
            transaction,
            work: MeshPublicationWork {
                geometry_validations: usize::from(retained_extent.is_none()),
                extent_points: if retained_extent.is_none() {
                    self.points.len()
                } else {
                    0
                },
                authored_properties: properties.len(),
            },
            properties,
        })
    }
}

fn derive_extent(points: &[[f32; 3]]) -> Arc<Vec<[f32; 3]>> {
    let mut extent = Vec::new();
    if let Some(first) = points.first() {
        let (mut lo, mut hi) = (*first, *first);
        for point in points.iter().skip(1) {
            for axis in 0..3 {
                lo[axis] = lo[axis].min(point[axis]);
                hi[axis] = hi[axis].max(point[axis]);
            }
        }
        extent.extend([lo, hi]);
    }
    Arc::new(extent)
}

/// Geometry covered by one successful immutable snapshot validation.
/// Counts describe payload entries, not CPU instructions or allocations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshValidationWork {
    /// Points checked for finite coordinates.
    pub points: usize,
    /// Polygon counts validated.
    pub faces: usize,
    /// Topology indices validated.
    pub corners: usize,
    /// Numeric primvar declarations and cardinalities validated.
    pub primvar_arrays: usize,
    /// Indexed primvar entries validated.
    pub primvar_indices: usize,
    /// Points visited when deriving the retained extent.
    pub extent_points: usize,
}

#[derive(Debug)]
struct ValidatedMeshData {
    mesh: GeneratedMesh,
    extent: Arc<Vec<[f32; 3]>>,
    work: MeshValidationWork,
}

/// Immutable, reusable polygon snapshot with retained validation and extent.
/// Clones share one snapshot. Mutating an exported owner detaches its buffer;
/// it cannot change this validated snapshot. No content hash is inferred.
#[derive(Clone, Debug)]
pub struct ValidatedMesh(Arc<ValidatedMeshData>);
impl ValidatedMesh {
    /// Borrows the immutable generated geometry.
    pub fn mesh(&self) -> &GeneratedMesh {
        &self.0.mesh
    }
    /// Work performed once when this snapshot was validated.
    pub fn validation_work(&self) -> MeshValidationWork {
        self.0.work
    }
    /// Prepares a default publication by checking the current authored site and
    /// owned properties. Geometry is not rescanned and extent is not recomputed.
    /// Independently allocated payload arrays are replaced by these shared owners
    /// even when their contents are equal. The two-point extent compares by value.
    /// Deletion/replacement is detected each time.
    /// No-op transactions also guard target-layer generation. Other publication
    /// and ownership rules are those of [`GeneratedMesh::prepare`].
    pub fn prepare(
        &self,
        stage: &Stage,
        store: &mut dyn LayerStore,
        target: &EditTarget,
        path: PathId,
        previous_properties: &[String],
    ) -> Result<MeshPublication, MeshPublicationError> {
        self.0.mesh.prepare_impl(
            stage,
            store,
            target,
            path,
            previous_properties,
            Some(&self.0.extent),
        )
    }
}

/// Planning work of one successfully prepared publication.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshPublicationWork {
    /// Complete geometry validations during this preparation (zero for retained snapshots).
    pub geometry_validations: usize,
    /// Points visited to derive extent during this preparation.
    pub extent_points: usize,
    /// Desired authored property declarations/defaults checked.
    pub authored_properties: usize,
}

/// A fully validated publication, ready for `LiveStage::apply`.
/// Retain its manifest only after application succeeds. A no-op has an empty transaction.
#[derive(Debug)]
pub struct MeshPublication {
    /// Transaction to apply atomically through the caller's live stage.
    pub transaction: Transaction,
    /// Producer-owned property names for the next update at this same authored site.
    pub properties: Vec<String>,
    /// Geometry and authored-property planning work for this preparation.
    pub work: MeshPublicationWork,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Scene, usd_geom::Mesh};
    use alloc::vec;
    use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions};

    fn triangle() -> GeneratedMesh {
        GeneratedMesh {
            points: Arc::new(vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]),
            face_vertex_counts: Arc::new(vec![3]),
            face_vertex_indices: Arc::new(vec![0, 1, 2]),
            primvars: Vec::new(),
        }
    }
    fn uv() -> MeshPrimvar {
        MeshPrimvar {
            name: "primvars:st".into(),
            type_name: PropertyType::new("texCoord2f", true, Value::Vec2f([0.; 2])),
            value: Value::TypedArray(TypedArray::Vec2f(Arc::new(vec![[0.; 2]; 3]))),
            interpolation: "faceVarying".into(),
            element_size: 1,
            indices: Some(Arc::new(vec![0, 1, 2])),
        }
    }
    #[test]
    fn validated_snapshots_repair_authored_output_without_scanning_geometry() {
        let mut store = InMemoryStore::default();
        let root = LayerId(1);
        store.insert_layer(Layer::new(root));
        let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
        let path = store.path("/Mesh");
        let target = EditTarget::for_layer(root);
        let mut external = triangle();
        external.primvars.push(uv());
        let snapshot = external.clone().into_validated().unwrap();
        Arc::make_mut(&mut external.points)[1][0] = 99.;
        assert_eq!(
            snapshot.mesh().points[1][0],
            1.,
            "immutable snapshot survives caller COW edits"
        );
        assert_eq!(snapshot.validation_work().extent_points, 3);
        let initial = snapshot
            .prepare(live.stage(), &mut store, &target, path, &[])
            .unwrap();
        live.apply(&mut store, &initial.transaction).unwrap();
        let unchanged = snapshot
            .prepare(live.stage(), &mut store, &target, path, &initial.properties)
            .unwrap();
        assert!(unchanged.transaction.is_empty());
        assert_eq!(unchanged.work.geometry_validations, 0);
        assert_eq!(unchanged.work.extent_points, 0);
        let points = store.property_path("/Mesh.points");
        let mut replacement = Transaction::new();
        replacement.set_default(
            target.property(points),
            Value::TypedArray(TypedArray::Vec3f(Arc::new(
                snapshot.mesh().points.as_ref().clone(),
            ))),
        );
        live.apply(&mut store, &replacement).unwrap();
        // Even an empty retained publication carries the authored-site guard.
        assert!(live.apply(&mut store, &unchanged.transaction).is_err());
        let repaired = snapshot
            .prepare(live.stage(), &mut store, &target, path, &initial.properties)
            .unwrap();
        assert!(
            !repaired.transaction.is_empty(),
            "equal contents in another owner are rebound explicitly"
        );
        live.apply(&mut store, &repaired.transaction).unwrap();
        let local = store.layers[&root]
            .property(points)
            .unwrap()
            .default
            .as_ref()
            .unwrap();
        assert!(
            matches!(local, Value::TypedArray(a) if a.shares_storage(&TypedArray::Vec3f(snapshot.mesh().points.clone())))
        );
        let mut deletion = Transaction::new();
        deletion.remove_spec(target.prim(path));
        live.apply(&mut store, &deletion).unwrap();
        let recreated = snapshot
            .prepare(live.stage(), &mut store, &target, path, &initial.properties)
            .unwrap();
        assert_eq!(recreated.work.geometry_validations, 0);
        live.apply(&mut store, &recreated.transaction).unwrap();
        assert!(live.stage().has_prim(path));
    }
    #[test]
    fn updates_are_atomic_share_storage_and_prune_only_owned_properties() {
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(LayerId(1)));
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
            ..Default::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let path = store.path("/Mesh");
        let target = EditTarget::for_layer(LayerId(1));
        let mut data = triangle();
        data.primvars.push(uv());
        let published = data
            .prepare(live.stage(), &mut store, &target, path, &[])
            .unwrap();
        live.apply(&mut store, &published.transaction).unwrap();
        let points = Mesh::new(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .points()
            .unwrap();
        assert!(Arc::ptr_eq(&points, &data.points));
        let unchanged = data
            .prepare(
                live.stage(),
                &mut store,
                &target,
                path,
                &published.properties,
            )
            .unwrap();
        assert!(unchanged.transaction.is_empty());
        let custom = store.property_path("/Mesh.user:tag");
        let mut user_edit = Transaction::new();
        user_edit.create_property(
            target.property(custom),
            PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                .with_default(Value::Int(17)),
        );
        live.apply(&mut store, &user_edit).unwrap();
        data.primvars.clear();
        data.points = Arc::new(vec![[0., 0., 0.], [2., 0., 0.], [0., 2., 0.]]);
        let update = data
            .prepare(
                live.stage(),
                &mut store,
                &target,
                path,
                &published.properties,
            )
            .unwrap();
        let indices_before = Mesh::new(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .face_vertex_indices()
            .unwrap();
        live.apply(&mut store, &update.transaction).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let mesh = Mesh::new(&scene, path).unwrap();
        assert!(Arc::ptr_eq(
            &indices_before,
            &mesh.face_vertex_indices().unwrap()
        ));
        assert_eq!(
            Mesh::new(&Scene::new(live.stage(), &store), path)
                .unwrap()
                .points()
                .unwrap()[1],
            [2., 0., 0.]
        );
        assert!(!mesh.has_attribute("primvars:st"));
        assert!(!mesh.has_attribute("primvars:st:indices"));
        assert_eq!(
            mesh.read_value("user:tag", crate::value::read_int),
            Some(17)
        );
        // Invalid topology never produces a publishable transaction.
        data.face_vertex_indices = Arc::new(vec![0, 1, 3]);
        assert!(matches!(
            data.prepare(live.stage(), &mut store, &target, path, &update.properties),
            Err(MeshPublicationError::Topology(_))
        ));
        assert_eq!(
            Mesh::new(&Scene::new(live.stage(), &store), path)
                .unwrap()
                .points()
                .unwrap()[1],
            [2., 0., 0.]
        );
    }
    #[test]
    fn validates_seams_element_groups_and_cardinality() {
        let mut data = triangle();
        data.primvars.push(uv());
        assert!(data.validate().is_ok());
        data.primvars[0].indices = Some(Arc::new(vec![0, 1, 3]));
        assert!(matches!(
            data.validate(),
            Err(MeshPublicationError::Index {
                position: 2,
                index: 3
            })
        ));
        data.primvars[0].indices = None;
        data.primvars[0].element_size = 2;
        assert_eq!(data.validate(), Err(MeshPublicationError::ElementSize));
        data.primvars[0].element_size = 1;
        data.primvars[0].interpolation = "uniform".into();
        assert_eq!(
            data.validate(),
            Err(MeshPublicationError::Cardinality {
                expected: 1,
                actual: 3
            })
        );
        data.primvars[0].interpolation = "invalid".into();
        assert!(matches!(
            data.validate(),
            Err(MeshPublicationError::Interpolation(_))
        ));
        let sites = MeshSites {
            points: 3,
            faces: 1,
            corners: 3,
        };
        assert_eq!(
            validate_mesh_primvar_cardinality(6, 2, "vertex", sites, None),
            Ok(3)
        );
        assert_eq!(
            validate_mesh_primvar_cardinality(2, 2, "constant", sites, Some(1)),
            Ok(1)
        );
    }
}
