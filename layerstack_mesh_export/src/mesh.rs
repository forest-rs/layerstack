// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Borrowed mesh description.

use alloc::vec::Vec;

use layerstack_usda::writer::Value;

use crate::Transform;

/// Face topology.
///
/// Spec: `UsdGeomMesh` `faceVertexCounts` / `faceVertexIndices`
/// (<https://openusd.org/dev/api/class_usd_geom_mesh.html>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Faces<'a> {
    /// A flat triangle index list; every three indices form one face.
    Triangles(&'a [u32]),
    /// General polygons: `counts[f]` corners per face, whose point indices
    /// follow each other in `indices`. Every face needs at least 3 corners.
    Polygons {
        /// Number of corners of each face.
        counts: &'a [u32],
        /// Point index of each face corner, face after face.
        indices: &'a [u32],
    },
}

impl Faces<'_> {
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
/// selects an element of `values`, so shared values are stored once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Primvar<'a, V> {
    /// The element values.
    pub values: V,
    /// Where the values (or indices) apply.
    pub interpolation: Interpolation,
    /// Optional per-site indices into `values`.
    pub indices: Option<&'a [u32]>,
}

impl<V> Primvar<'_, V> {
    /// Values with the given interpolation, unindexed.
    pub fn new(values: V, interpolation: Interpolation) -> Self {
        Self {
            values,
            interpolation,
            indices: None,
        }
    }

    /// One value for the whole mesh.
    pub fn constant(values: V) -> Self {
        Self::new(values, Interpolation::Constant)
    }

    /// One value per face.
    pub fn uniform(values: V) -> Self {
        Self::new(values, Interpolation::Uniform)
    }

    /// One value per point.
    pub fn vertex(values: V) -> Self {
        Self::new(values, Interpolation::Vertex)
    }

    /// One value per face corner.
    pub fn face_varying(values: V) -> Self {
        Self::new(values, Interpolation::FaceVarying)
    }
}

impl<'a, V> Primvar<'a, V> {
    /// Adds per-site indices into the values.
    #[must_use]
    pub fn with_indices(mut self, indices: &'a [u32]) -> Self {
        self.indices = Some(indices);
        self
    }
}

/// Element data of an additional primvar. The variant fixes the USD value
/// type written for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrimvarData<'a> {
    /// `float[]`.
    Float(&'a [f32]),
    /// `float2[]`.
    Float2(&'a [[f32; 2]]),
    /// `float3[]`.
    Float3(&'a [[f32; 3]]),
    /// `float4[]`.
    Float4(&'a [[f32; 4]]),
    /// `int[]`.
    Int(&'a [i32]),
    /// `uint[]`.
    UInt(&'a [u32]),
    /// `color3f[]` (e.g. `primvars:displayColor`).
    Color3(&'a [[f32; 3]]),
    /// `color4f[]`.
    Color4(&'a [[f32; 4]]),
    /// `vector3f[]` (directions, e.g. tangents).
    Vector3(&'a [[f32; 3]]),
    /// `texCoord2f[]` (an additional UV set).
    TexCoord2(&'a [[f32; 2]]),
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

    pub(crate) fn to_value(self) -> Value {
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CustomPrimvar<'a> {
    /// Name after the `primvars:` namespace (may itself be namespaced,
    /// e.g. `exedra:region`).
    pub name: &'a str,
    /// Values, interpolation and optional indices.
    pub primvar: Primvar<'a, PrimvarData<'a>>,
}

/// A custom (non-schema) attribute, written with the `custom` qualifier.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomAttribute<'a> {
    /// Namespaced attribute name (e.g. `exedra:sourcePath`). Use a
    /// namespace so it cannot collide with schema attributes.
    pub name: &'a str,
    /// The value; its canonical USD type is declared (e.g. `string`).
    /// [`Value::Dictionary`] is metadata-only in USD and is rejected at
    /// export ([`ExportError::Usda`](crate::ExportError::Usda)).
    pub value: Value,
}

impl<'a> CustomAttribute<'a> {
    /// Creates a custom attribute.
    pub fn new(name: &'a str, value: Value) -> Self {
        Self { name, value }
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

/// One polygon mesh, written as a `Mesh` prim.
///
/// All buffers are borrowed; nothing is copied until the scene is
/// serialized.
#[derive(Clone, Debug, PartialEq)]
pub struct Mesh<'a> {
    /// Prim name; must be a USD identifier.
    pub name: &'a str,
    /// Local transform relative to the parent prim.
    pub transform: Option<Transform>,
    /// Point positions in the mesh's local space.
    pub points: &'a [[f32; 3]],
    /// Face topology.
    pub faces: Faces<'a>,
    /// Surface normals.
    pub normals: Option<Primvar<'a, &'a [[f32; 3]]>>,
    /// Primary texture coordinates, written as `primvars:st`.
    pub uvs: Option<Primvar<'a, &'a [[f32; 2]]>>,
    /// Additional primvars.
    pub primvars: Vec<CustomPrimvar<'a>>,
    /// Custom attributes.
    pub attributes: Vec<CustomAttribute<'a>>,
    /// Front-face winding.
    pub orientation: Orientation,
    /// Whether back faces should be rendered too (`doubleSided`).
    pub double_sided: bool,
}

impl<'a> Mesh<'a> {
    /// A mesh with only points and topology.
    pub fn new(name: &'a str, points: &'a [[f32; 3]], faces: Faces<'a>) -> Self {
        Self {
            name,
            transform: None,
            points,
            faces,
            normals: None,
            uvs: None,
            primvars: Vec::new(),
            attributes: Vec::new(),
            orientation: Orientation::RightHanded,
            double_sided: false,
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
    pub fn with_normals(mut self, normals: Primvar<'a, &'a [[f32; 3]]>) -> Self {
        self.normals = Some(normals);
        self
    }

    /// Sets the primary UV set (`primvars:st`).
    #[must_use]
    pub fn with_uvs(mut self, uvs: Primvar<'a, &'a [[f32; 2]]>) -> Self {
        self.uvs = Some(uvs);
        self
    }

    /// Adds a primvar written as `primvars:<name>`.
    #[must_use]
    pub fn with_primvar(mut self, name: &'a str, primvar: Primvar<'a, PrimvarData<'a>>) -> Self {
        self.primvars.push(CustomPrimvar { name, primvar });
        self
    }

    /// Adds a custom attribute.
    #[must_use]
    pub fn with_attribute(mut self, name: &'a str, value: Value) -> Self {
        self.attributes.push(CustomAttribute::new(name, value));
        self
    }
}
