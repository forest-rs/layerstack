// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Mesh description.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use layerstack_usda::writer::Value;

use crate::Transform;

/// Face topology.
///
/// Spec: `UsdGeomMesh` `faceVertexCounts` / `faceVertexIndices`
/// (<https://openusd.org/dev/api/class_usd_geom_mesh.html>).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Faces<'a> {
    /// A flat triangle index list; every three indices form one face.
    Triangles(Cow<'a, [u32]>),
    /// General polygons: `counts[f]` corners per face, whose point indices
    /// follow each other in `indices`. Every face needs at least 3 corners.
    Polygons {
        /// Number of corners of each face.
        counts: Cow<'a, [u32]>,
        /// Point index of each face corner, face after face.
        indices: Cow<'a, [u32]>,
    },
}

impl<'a> Faces<'a> {
    /// Triangles from a flat index list.
    pub fn triangles(indices: impl Into<Cow<'a, [u32]>>) -> Self {
        Self::Triangles(indices.into())
    }

    /// General polygons from corner counts and point indices.
    pub fn polygons(counts: impl Into<Cow<'a, [u32]>>, indices: impl Into<Cow<'a, [u32]>>) -> Self {
        Self::Polygons {
            counts: counts.into(),
            indices: indices.into(),
        }
    }

    /// Number of faces (`uniform` element count).
    pub fn face_count(&self) -> usize {
        match self {
            Self::Triangles(indices) => indices.len() / 3,
            Self::Polygons { counts, .. } => counts.len(),
        }
    }

    /// Number of face corners (`faceVarying` element count).
    pub fn corner_count(&self) -> usize {
        match self {
            Self::Triangles(indices) | Self::Polygons { indices, .. } => indices.len(),
        }
    }
}

/// How primvar elements map onto the mesh.
///
/// Spec: `UsdGeomPrimvar` interpolation
/// (<https://openusd.org/dev/api/class_usd_geom_primvar.html>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// One value for the whole mesh.
    Constant,
    /// One value per face.
    Uniform,
    /// One value per point, linearly interpolated.
    Varying,
    /// One value per point, interpolated like the surface (identical to
    /// `Varying` for `subdivisionScheme = "none"`).
    Vertex,
    /// One value per face corner: splits values across seams without
    /// duplicating points.
    FaceVarying,
}

impl Interpolation {
    /// The USD token for this interpolation.
    pub fn token(self) -> &'static str {
        match self {
            Self::Constant => "constant",
            Self::Uniform => "uniform",
            Self::Varying => "varying",
            Self::Vertex => "vertex",
            Self::FaceVarying => "faceVarying",
        }
    }
}

/// Values plus the interpolation (and optional indexing) that places them
/// on the mesh.
///
/// Without `indices`, `values` holds exactly one element per interpolation
/// site. With `indices`, `indices` holds one entry per site and each entry
/// selects an element of `values`.
///
/// For [`Interpolation::FaceVarying`], indices are not just compression:
/// corners that share an index are joined in the primvar's topology, and
/// corners with distinct indices are split (a UV seam or hard normal edge)
/// even when their values are equal (`pxr/usd/usdGeom/primvar.h:497`).
/// Indices are written exactly as given.
#[derive(Clone, Debug, PartialEq)]
pub struct Primvar<'a, V> {
    /// The element values.
    pub values: V,
    /// Where the values (or indices) apply.
    pub interpolation: Interpolation,
    /// Optional per-site indices into `values`.
    pub indices: Option<Cow<'a, [u32]>>,
}

impl<V> Primvar<'_, V> {
    /// Values with the given interpolation, unindexed.
    pub fn new(values: impl Into<V>, interpolation: Interpolation) -> Self {
        Self {
            values: values.into(),
            interpolation,
            indices: None,
        }
    }

    /// One value for the whole mesh.
    pub fn constant(values: impl Into<V>) -> Self {
        Self::new(values, Interpolation::Constant)
    }

    /// One value per face.
    pub fn uniform(values: impl Into<V>) -> Self {
        Self::new(values, Interpolation::Uniform)
    }

    /// One value per point.
    pub fn vertex(values: impl Into<V>) -> Self {
        Self::new(values, Interpolation::Vertex)
    }

    /// One value per face corner.
    pub fn face_varying(values: impl Into<V>) -> Self {
        Self::new(values, Interpolation::FaceVarying)
    }

    /// One value per instance of a
    /// [`PointInstancer`](crate::PointInstancer): `vertex` interpolation,
    /// which the schema reads as one element per instance
    /// (`pxr/usd/usdGeom/pointInstancer.h`, "Primvars on
    /// `PointInstancer`").
    pub fn per_instance(values: impl Into<V>) -> Self {
        Self::new(values, Interpolation::Vertex)
    }
}

impl<'a, V> Primvar<'a, V> {
    /// Adds per-site indices into the values.
    #[must_use]
    pub fn with_indices(mut self, indices: impl Into<Cow<'a, [u32]>>) -> Self {
        self.indices = Some(indices.into());
        self
    }
}

/// Element data of an additional primvar. The variant fixes the USD value
/// type written for it.
#[derive(Clone, Debug, PartialEq)]
pub enum PrimvarData<'a> {
    /// `float[]`.
    Float(Cow<'a, [f32]>),
    /// `float2[]`.
    Float2(Cow<'a, [[f32; 2]]>),
    /// `float3[]`.
    Float3(Cow<'a, [[f32; 3]]>),
    /// `float4[]`.
    Float4(Cow<'a, [[f32; 4]]>),
    /// `int[]`.
    Int(Cow<'a, [i32]>),
    /// `uint[]`.
    UInt(Cow<'a, [u32]>),
    /// `color3f[]` (e.g. `primvars:displayColor`).
    Color3(Cow<'a, [[f32; 3]]>),
    /// `color4f[]`.
    Color4(Cow<'a, [[f32; 4]]>),
    /// `vector3f[]` (directions, e.g. tangents).
    Vector3(Cow<'a, [[f32; 3]]>),
    /// `texCoord2f[]` (an additional UV set).
    TexCoord2(Cow<'a, [[f32; 2]]>),
}

impl<'a> PrimvarData<'a> {
    /// `float[]` values.
    pub fn float(values: impl Into<Cow<'a, [f32]>>) -> Self {
        Self::Float(values.into())
    }

    /// `float2[]` values.
    pub fn float2(values: impl Into<Cow<'a, [[f32; 2]]>>) -> Self {
        Self::Float2(values.into())
    }

    /// `float3[]` values.
    pub fn float3(values: impl Into<Cow<'a, [[f32; 3]]>>) -> Self {
        Self::Float3(values.into())
    }

    /// `float4[]` values.
    pub fn float4(values: impl Into<Cow<'a, [[f32; 4]]>>) -> Self {
        Self::Float4(values.into())
    }

    /// `int[]` values.
    pub fn int(values: impl Into<Cow<'a, [i32]>>) -> Self {
        Self::Int(values.into())
    }

    /// `uint[]` values.
    pub fn uint(values: impl Into<Cow<'a, [u32]>>) -> Self {
        Self::UInt(values.into())
    }

    /// `color3f[]` values.
    pub fn color3(values: impl Into<Cow<'a, [[f32; 3]]>>) -> Self {
        Self::Color3(values.into())
    }

    /// `color4f[]` values.
    pub fn color4(values: impl Into<Cow<'a, [[f32; 4]]>>) -> Self {
        Self::Color4(values.into())
    }

    /// `vector3f[]` values.
    pub fn vector3(values: impl Into<Cow<'a, [[f32; 3]]>>) -> Self {
        Self::Vector3(values.into())
    }

    /// `texCoord2f[]` values.
    pub fn tex_coord2(values: impl Into<Cow<'a, [[f32; 2]]>>) -> Self {
        Self::TexCoord2(values.into())
    }
}

impl PrimvarData<'_> {
    /// Number of elements.
    pub fn len(&self) -> usize {
        match self {
            Self::Float(v) => v.len(),
            Self::Float2(v) | Self::TexCoord2(v) => v.len(),
            Self::Float3(v) | Self::Color3(v) | Self::Vector3(v) => v.len(),
            Self::Float4(v) | Self::Color4(v) => v.len(),
            Self::Int(v) => v.len(),
            Self::UInt(v) => v.len(),
        }
    }

    /// Whether there are no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Self::Float(_) => "float[]",
            Self::Float2(_) => "float2[]",
            Self::Float3(_) => "float3[]",
            Self::Float4(_) => "float4[]",
            Self::Int(_) => "int[]",
            Self::UInt(_) => "uint[]",
            Self::Color3(_) => "color3f[]",
            Self::Color4(_) => "color4f[]",
            Self::Vector3(_) => "vector3f[]",
            Self::TexCoord2(_) => "texCoord2f[]",
        }
    }

    /// Element `i` alone, as a one-element array of the same type.
    pub(crate) fn element(&self, i: usize) -> Value {
        match self {
            Self::Float(v) => Value::FloatArray(alloc::vec![v[i]]),
            Self::Float2(v) | Self::TexCoord2(v) => Value::Float2Array(alloc::vec![v[i]]),
            Self::Float3(v) | Self::Color3(v) | Self::Vector3(v) => {
                Value::Float3Array(alloc::vec![v[i]])
            }
            Self::Float4(v) | Self::Color4(v) => Value::Float4Array(alloc::vec![v[i]]),
            Self::Int(v) => Value::IntArray(alloc::vec![v[i]]),
            Self::UInt(v) => Value::UIntArray(alloc::vec![v[i]]),
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Float(v) => Value::FloatArray(v.to_vec()),
            Self::Float2(v) | Self::TexCoord2(v) => Value::Float2Array(v.to_vec()),
            Self::Float3(v) | Self::Color3(v) | Self::Vector3(v) => Value::Float3Array(v.to_vec()),
            Self::Float4(v) | Self::Color4(v) => Value::Float4Array(v.to_vec()),
            Self::Int(v) => Value::IntArray(v.to_vec()),
            Self::UInt(v) => Value::UIntArray(v.to_vec()),
        }
    }
}

/// An additional primvar, written as `primvars:<name>`.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomPrimvar<'a> {
    /// Name after the `primvars:` namespace (may itself be namespaced,
    /// e.g. `scan:region`).
    pub name: Cow<'a, str>,
    /// Values, interpolation and optional indices.
    pub primvar: Primvar<'a, PrimvarData<'a>>,
}

/// A custom (non-schema) attribute, written with the `custom` qualifier.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomAttribute<'a> {
    /// Namespaced attribute name (e.g. `scan:sourcePath`). Use a
    /// namespace so it cannot collide with schema attributes.
    pub name: Cow<'a, str>,
    /// The value; its canonical USD type is declared (e.g. `string`).
    /// [`Value::Dictionary`] is metadata-only in USD and is rejected at
    /// export ([`ExportError::Usda`](crate::ExportError::Usda)).
    pub value: Value,
}

impl<'a> CustomAttribute<'a> {
    /// Creates a custom attribute.
    pub fn new(name: impl Into<Cow<'a, str>>, value: Value) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

/// Which side of a face is the front.
///
/// Spec: `UsdGeomGprim` `orientation`
/// (<https://openusd.org/dev/api/class_usd_geom_gprim.html>).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    /// Counter-clockwise corners face the viewer (USD's fallback, glTF's
    /// convention).
    #[default]
    RightHanded,
    /// Clockwise corners face the viewer.
    LeftHanded,
}

/// How a mesh's material subsets divide its faces: the
/// `subsetFamily:materialBind:familyType` authored on the mesh.
///
/// Both types forbid a face from appearing twice across the family, since
/// a face cannot be bound to two materials (`unrestricted` is invalid for
/// `materialBind`).
///
/// Spec: `pxr/usd/usdGeom/subset.h:254` (family types),
/// `pxr/usd/usdShade/materialBindingAPI.h:867` (`materialBind` family).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FamilyType {
    /// Subsets never share a face; faces outside every subset keep the
    /// mesh's own binding, if any (USD's fallback for `materialBind`).
    #[default]
    NonOverlapping,
    /// Every face is in exactly one subset.
    Partition,
}

impl FamilyType {
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::NonOverlapping => "nonOverlapping",
            Self::Partition => "partition",
        }
    }
}

/// Faces bound to one material, written as a `GeomSubset` child of the
/// mesh (`elementType = "face"`, `familyName = "materialBind"`) with its
/// own direct binding.
///
/// Spec: `pxr/usd/usdGeom/subset.h` (`GeomSubset`),
/// `pxr/usd/usdShade/materialBindingAPI.h:867` (per-face bindings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialSubset<'a> {
    /// Prim name; must be a USD identifier, unique among the mesh's
    /// subsets.
    pub name: Cow<'a, str>,
    /// Face indices (`indices`), written in the given order.
    pub faces: Cow<'a, [u32]>,
    /// Name of the scene material bound to these faces.
    pub material: Cow<'a, str>,
}

/// One polygon mesh, written as a `Mesh` prim.
///
/// Names and buffers are [`Cow`]s: borrow a kernel's buffers where they
/// outlive the scene, or hand over owned ones built for the export.
/// Borrowed data is not copied until the scene is serialized.
#[derive(Clone, Debug, PartialEq)]
pub struct Mesh<'a> {
    /// Prim name; must be a USD identifier.
    pub name: Cow<'a, str>,
    /// Local transform relative to the parent prim.
    pub transform: Option<Transform>,
    /// Point positions in the mesh's local space.
    pub points: Cow<'a, [[f32; 3]]>,
    /// Face topology.
    pub faces: Faces<'a>,
    /// Surface normals.
    pub normals: Option<Primvar<'a, Cow<'a, [[f32; 3]]>>>,
    /// Primary texture coordinates, written as `primvars:st`.
    pub uvs: Option<Primvar<'a, Cow<'a, [[f32; 2]]>>>,
    /// Additional primvars.
    pub primvars: Vec<CustomPrimvar<'a>>,
    /// Custom attributes.
    pub attributes: Vec<CustomAttribute<'a>>,
    /// Front-face winding.
    pub orientation: Orientation,
    /// Whether back faces should be rendered too (`doubleSided`).
    pub double_sided: bool,
    /// Name of the scene [`Material`](crate::Material) bound to the whole
    /// mesh, written as a direct `material:binding` with the
    /// `MaterialBindingAPI` applied. With [`Self::material_subsets`], it
    /// covers the faces outside every subset.
    pub material: Option<Cow<'a, str>>,
    /// Per-face material bindings.
    pub material_subsets: Vec<MaterialSubset<'a>>,
    /// How [`Self::material_subsets`] divide the faces; authored whenever
    /// there are subsets, and checked before anything is written.
    pub subset_family: FamilyType,
}

impl<'a> Mesh<'a> {
    /// A mesh with only points and topology.
    pub fn new(
        name: impl Into<Cow<'a, str>>,
        points: impl Into<Cow<'a, [[f32; 3]]>>,
        faces: Faces<'a>,
    ) -> Self {
        Self {
            name: name.into(),
            transform: None,
            points: points.into(),
            faces,
            normals: None,
            uvs: None,
            primvars: Vec::new(),
            attributes: Vec::new(),
            orientation: Orientation::RightHanded,
            double_sided: false,
            material: None,
            material_subsets: Vec::new(),
            subset_family: FamilyType::NonOverlapping,
        }
    }

    /// Sets the local transform.
    #[must_use]
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = Some(transform);
        self
    }

    /// Sets the normals.
    #[must_use]
    pub fn with_normals(mut self, normals: Primvar<'a, Cow<'a, [[f32; 3]]>>) -> Self {
        self.normals = Some(normals);
        self
    }

    /// Sets the primary UV set (`primvars:st`).
    #[must_use]
    pub fn with_uvs(mut self, uvs: Primvar<'a, Cow<'a, [[f32; 2]]>>) -> Self {
        self.uvs = Some(uvs);
        self
    }

    /// Adds a primvar written as `primvars:<name>`.
    #[must_use]
    pub fn with_primvar(
        mut self,
        name: impl Into<Cow<'a, str>>,
        primvar: Primvar<'a, PrimvarData<'a>>,
    ) -> Self {
        self.primvars.push(CustomPrimvar {
            name: name.into(),
            primvar,
        });
        self
    }

    /// Adds a custom attribute.
    #[must_use]
    pub fn with_attribute(mut self, name: impl Into<Cow<'a, str>>, value: Value) -> Self {
        self.attributes.push(CustomAttribute::new(name, value));
        self
    }

    /// Binds a scene material to the whole mesh, by name.
    #[must_use]
    pub fn with_material(mut self, material: impl Into<Cow<'a, str>>) -> Self {
        self.material = Some(material.into());
        self
    }

    /// Binds a scene material to some faces, through a `GeomSubset` named
    /// `name`.
    #[must_use]
    pub fn with_material_subset(
        mut self,
        name: impl Into<Cow<'a, str>>,
        faces: impl Into<Cow<'a, [u32]>>,
        material: impl Into<Cow<'a, str>>,
    ) -> Self {
        self.material_subsets.push(MaterialSubset {
            name: name.into(),
            faces: faces.into(),
            material: material.into(),
        });
        self
    }

    /// Sets how the material subsets divide the faces.
    #[must_use]
    pub fn with_subset_family(mut self, family: FamilyType) -> Self {
        self.subset_family = family;
        self
    }
}
