// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Export errors.

use alloc::string::String;
use core::fmt;

use layerstack_usda::writer::WriteError;
use layerstack_usdc::writer::UsdcWriteError;
use layerstack_usdz::UsdzWriteError;

use crate::UsdzProfile;

/// Why a scene could not be exported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    /// A mesh's buffers are inconsistent.
    InvalidMesh {
        /// Prim path of the mesh (e.g. `/Root/Body`).
        path: String,
        /// What is wrong.
        problem: MeshProblem,
    },
    /// A point instancer's prototypes or per-instance arrays are
    /// inconsistent, or it would author something the export does not
    /// support.
    InvalidInstancer {
        /// Prim path of the instancer (e.g. `/Root/Forest`).
        path: String,
        /// What is wrong.
        problem: InstancerProblem,
    },
    /// `metersPerUnit` is not finite and positive.
    InvalidStage,
    /// A material's inputs are unusable.
    InvalidMaterial {
        /// Prim path of the material (e.g. `/Root/Materials/Steel`).
        path: String,
        /// What is wrong.
        problem: MaterialProblem,
    },
    /// A mesh or material subset binds a material name that the scene does
    /// not define.
    UnknownMaterial {
        /// Prim path of the binding prim (the mesh or its `GeomSubset`).
        path: String,
        /// The material name.
        material: String,
    },
    /// An authored asset path names no file in the package being written.
    UnpackagedAsset {
        /// The asset path as authored.
        asset: String,
    },
    /// A package member is a file type the USDZ profile excludes (e.g. a
    /// second USD layer or an EXR image in an [`UsdzProfile::Arkit`]
    /// package).
    ProfileMember {
        /// The member's package path.
        path: String,
        /// The profile being written.
        profile: UsdzProfile,
    },
    /// The USDA writer rejected the document (e.g. a name that is not a USD
    /// identifier, or duplicate sibling names).
    Usda(WriteError),
    /// The USDC writer rejected the document.
    Usdc(UsdcWriteError),
    /// The package could not be written (e.g. an invalid or duplicate asset
    /// path).
    Usdz(UsdzWriteError),
}

/// A specific inconsistency in a mesh's buffers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeshProblem {
    /// A triangle index list whose length is not a multiple of 3.
    PartialTriangle {
        /// The index count.
        len: usize,
    },
    /// A polygon with fewer than 3 corners.
    DegenerateFace {
        /// Face number.
        face: usize,
        /// Its corner count.
        count: u32,
    },
    /// `faceVertexCounts` does not sum to the number of indices.
    CornerCountMismatch {
        /// Sum of the counts.
        expected: usize,
        /// Length of the index list.
        actual: usize,
    },
    /// A face corner refers to a point that does not exist.
    PointIndexOutOfRange {
        /// Corner number.
        corner: usize,
        /// The index.
        index: u32,
        /// Number of points.
        points: usize,
    },
    /// A point has a NaN or infinite coordinate (its extent would be
    /// meaningless).
    NonFinitePoint {
        /// Point number.
        point: usize,
    },
    /// A buffer is too large for USD's 32-bit signed `int` indices/counts.
    TooLarge,
    /// A primvar has the wrong number of values (or indices) for its
    /// interpolation.
    PrimvarLength {
        /// Attribute name (e.g. `primvars:st`).
        name: String,
        /// Elements required by the interpolation.
        expected: usize,
        /// Elements (or indices) supplied.
        actual: usize,
    },
    /// A primvar index refers to a value that does not exist.
    PrimvarIndexOutOfRange {
        /// Attribute name.
        name: String,
        /// The index.
        index: u32,
        /// Number of values.
        values: usize,
    },
    /// A material subset names a face the mesh does not have.
    SubsetFaceOutOfRange {
        /// Subset name.
        subset: String,
        /// The face index.
        face: u32,
        /// Number of faces.
        faces: usize,
    },
    /// A face appears twice among the material subsets (in one subset or
    /// in two), which neither family type allows.
    OverlappingSubsets {
        /// Name of the subset holding the second occurrence.
        subset: String,
        /// The face index.
        face: u32,
    },
    /// The subsets are declared a partition but leave a face uncovered.
    IncompletePartition {
        /// The first uncovered face.
        face: usize,
    },
    /// A bound material reads textures through a UV set the mesh does not
    /// author as a `texCoord2f[]` or `float2[]` primvar.
    MissingTexCoords {
        /// The material name.
        material: String,
        /// The UV set (primvar name without `primvars:`).
        uv_set: String,
    },
}

/// A specific problem with a [`PointInstancer`](crate::PointInstancer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstancerProblem {
    /// The instancer has no prototypes; `prototypes` is required.
    NoPrototypes,
    /// A `protoIndices` entry names a prototype that does not exist.
    ProtoIndexOutOfRange {
        /// Instance number.
        instance: usize,
        /// The prototype index.
        index: u32,
        /// Number of prototypes.
        prototypes: usize,
    },
    /// A per-instance array does not have one element per instance.
    LengthMismatch {
        /// Attribute name (e.g. `orientations`).
        name: &'static str,
        /// Number of instances (the length of `protoIndices`).
        expected: usize,
        /// Elements supplied.
        actual: usize,
    },
    /// Two instances have the same id.
    DuplicateId {
        /// The id.
        id: i64,
        /// The first instance with it.
        first: usize,
        /// The next instance with it.
        second: usize,
    },
    /// A position, orientation or scale has a NaN or infinite component.
    NonFinite {
        /// Attribute name (e.g. `positions`).
        name: &'static str,
        /// Instance number.
        instance: usize,
    },
    /// An orientation is not a unit quaternion (its squared norm is off by
    /// more than 1e-3). `UsdGeomPointInstancer` leaves unit length to the
    /// author, and a scaled quaternion would scale and shear the instance.
    NonUnitOrientation {
        /// Instance number.
        instance: usize,
    },
    /// A custom attribute would author a time-varying or masking property
    /// of the schema (`velocities`, `accelerations`, `angularVelocities`,
    /// `invisibleIds`), which this static export does not support.
    UnsupportedProperty {
        /// The attribute name.
        name: String,
    },
    /// A custom attribute has the name of a schema property that the
    /// exporter authors itself (e.g. `positions` or `extent`).
    ReservedProperty {
        /// The attribute name.
        name: String,
    },
}

/// A specific problem with a material's inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaterialProblem {
    /// A constant, scale, bias or threshold is NaN or infinite.
    NonFinite {
        /// The shader input (e.g. `inputs:roughness`).
        input: &'static str,
    },
    /// A texture has an empty file path.
    EmptyTexturePath,
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMesh { path, problem } => write!(f, "{path}: {problem}"),
            Self::InvalidInstancer { path, problem } => write!(f, "{path}: {problem}"),
            Self::InvalidStage => write!(f, "metersPerUnit must be finite and positive"),
            Self::InvalidMaterial { path, problem } => write!(f, "{path}: {problem}"),
            Self::UnknownMaterial { path, material } => {
                write!(f, "{path}: binds undefined material {material:?}")
            }
            Self::UnpackagedAsset { asset } => {
                write!(f, "asset path {asset:?} names no file in the package")
            }
            Self::ProfileMember { path, profile } => {
                write!(f, "{path:?} is not allowed in a {profile:?} USDZ package")
            }
            Self::Usda(e) => write!(f, "USDA: {e}"),
            Self::Usdc(e) => write!(f, "USDC: {e}"),
            Self::Usdz(e) => write!(f, "USDZ: {e}"),
        }
    }
}

impl fmt::Display for MeshProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PartialTriangle { len } => {
                write!(f, "{len} triangle indices is not a multiple of 3")
            }
            Self::DegenerateFace { face, count } => {
                write!(
                    f,
                    "face {face} has {count} corners; at least 3 are required"
                )
            }
            Self::CornerCountMismatch { expected, actual } => write!(
                f,
                "faceVertexCounts sum to {expected} but there are {actual} indices"
            ),
            Self::PointIndexOutOfRange {
                corner,
                index,
                points,
            } => write!(f, "corner {corner} refers to point {index} of {points}"),
            Self::NonFinitePoint { point } => write!(f, "point {point} is not finite"),
            Self::TooLarge => write!(f, "buffer exceeds 32-bit USD int range"),
            Self::PrimvarLength {
                name,
                expected,
                actual,
            } => write!(f, "{name} needs {expected} elements, got {actual}"),
            Self::PrimvarIndexOutOfRange {
                name,
                index,
                values,
            } => write!(
                f,
                "{name} index {index} is out of range for {values} values"
            ),
            Self::SubsetFaceOutOfRange {
                subset,
                face,
                faces,
            } => write!(
                f,
                "subset {subset:?} names face {face}, but the mesh has {faces}"
            ),
            Self::OverlappingSubsets { subset, face } => {
                write!(f, "face {face} appears again in subset {subset:?}")
            }
            Self::IncompletePartition { face } => {
                write!(f, "face {face} is in no subset of the partition")
            }
            Self::MissingTexCoords { material, uv_set } => write!(
                f,
                "material {material:?} reads UV set {uv_set:?}, which the mesh does not author"
            ),
        }
    }
}

impl fmt::Display for InstancerProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPrototypes => write!(f, "a point instancer needs at least one prototype"),
            Self::ProtoIndexOutOfRange {
                instance,
                index,
                prototypes,
            } => write!(
                f,
                "instance {instance} uses prototype {index}, but there are {prototypes}"
            ),
            Self::LengthMismatch {
                name,
                expected,
                actual,
            } => write!(f, "{name} has {actual} elements for {expected} instances"),
            Self::DuplicateId { id, first, second } => {
                write!(f, "instances {first} and {second} both have id {id}")
            }
            Self::NonFinite { name, instance } => {
                write!(f, "{name} of instance {instance} is not finite")
            }
            Self::NonUnitOrientation { instance } => write!(
                f,
                "orientation of instance {instance} is not a unit quaternion"
            ),
            Self::UnsupportedProperty { name } => write!(
                f,
                "{name} is not supported: point instancers are exported static, \
                 without motion, time samples or masked ids"
            ),
            Self::ReservedProperty { name } => write!(
                f,
                "{name} is a point instancer property the exporter authors"
            ),
        }
    }
}

impl fmt::Display for MaterialProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite { input } => write!(f, "{input} is not finite"),
            Self::EmptyTexturePath => write!(f, "texture file path is empty"),
        }
    }
}

impl core::error::Error for ExportError {}

impl From<WriteError> for ExportError {
    fn from(e: WriteError) -> Self {
        Self::Usda(e)
    }
}

impl From<UsdcWriteError> for ExportError {
    fn from(e: UsdcWriteError) -> Self {
        Self::Usdc(e)
    }
}

impl From<UsdzWriteError> for ExportError {
    fn from(e: UsdzWriteError) -> Self {
        Self::Usdz(e)
    }
}
