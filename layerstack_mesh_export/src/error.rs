// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Export errors.

use alloc::string::String;
use core::fmt;

use layerstack_usda::writer::WriteError;
use layerstack_usdz::UsdzWriteError;

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
    /// `metersPerUnit` is not finite and positive.
    InvalidStage,
    /// An authored asset path names no file in the package being written.
    UnpackagedAsset {
        /// The asset path as authored.
        asset: String,
    },
    /// The USDA writer rejected the document (e.g. a name that is not a USD
    /// identifier, or duplicate sibling names).
    Usda(WriteError),
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
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMesh { path, problem } => write!(f, "{path}: {problem}"),
            Self::InvalidStage => write!(f, "metersPerUnit must be finite and positive"),
            Self::UnpackagedAsset { asset } => {
                write!(f, "asset path {asset:?} names no file in the package")
            }
            Self::Usda(e) => write!(f, "USDA: {e}"),
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
        }
    }
}

impl core::error::Error for ExportError {}

impl From<WriteError> for ExportError {
    fn from(e: WriteError) -> Self {
        Self::Usda(e)
    }
}

impl From<UsdzWriteError> for ExportError {
    fn from(e: UsdzWriteError) -> Self {
        Self::Usdz(e)
    }
}
