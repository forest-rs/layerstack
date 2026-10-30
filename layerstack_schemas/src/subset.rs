// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Subset discovery and topology validation; material evaluation stays in shading.
//! OpenUSD: `UsdGeomSubset::ValidateFamily`, `GetGeomSubsets`, `GetUnassignedIndices`
//! and `UsdGeomMesh::ValidateTopology`. Composition follows AOUSD Core §12.
use crate::diagnostic::{Describe, Names, with_store};
use crate::{
    PrimView, Time,
    usd_geom::{
        GeomSubset as Subset, GeomSubsetElementType as SubsetElementType, GprimOrientation,
        Imageable, Mesh, MeshSubdivisionScheme,
    },
};
use alloc::{collections::BTreeSet, format, string::String, sync::Arc, vec::Vec};
use layerstack::sort;
use layerstack::{ArrayReadError, LayerStore, PathId, PropertyPath, TokenInterner, Value};

/// The ownership contract authored for a geometry subset family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubsetFamilyType {
    /// Members may overlap and need not cover every element.
    Unrestricted,
    /// Members may not overlap, but may leave elements unassigned.
    NonOverlapping,
    /// Members assign every element exactly once.
    Partition,
}
impl SubsetFamilyType {
    fn token(self) -> &'static str {
        match self {
            Self::Unrestricted => "unrestricted",
            Self::NonOverlapping => "nonOverlapping",
            Self::Partition => "partition",
        }
    }
}

/// A subset edit could not be collected; its child group remains unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubsetAuthoringError {
    /// The subset cannot be defined or authored at its path, beneath an
    /// instance or in a prototype
    /// ([`SchemaEdit::can_edit`](crate::SchemaEdit::can_edit)).
    CannotEdit(crate::CannotEdit),
    /// The name must be a nonempty, single prim identifier.
    InvalidName,
    /// Geometry or its topology is absent, incompatible, or unreadable.
    Geometry(SubsetError),
    /// Supplied indices do not identify elements of the parent geometry.
    Indices(SubsetProblemKind),
    /// The family attribute already has a declaration other than scalar token.
    IncompatibleFamilyDeclaration,
    /// A prim that is not a `GeomSubset` already has the subset's name.
    NameTaken(PathId),
    /// A `materialBind` family may not be `unrestricted`: bound subsets
    /// must not overlap. OpenUSD's `SetMaterialBindSubsetsFamilyType`
    /// refuses it with a coding error.
    UnrestrictedMaterialBindFamily,
}
impl SubsetAuthoringError {
    /// The error in words, its paths written as text with `store`, the
    /// store the stage was composed from. `Display` writes the same with
    /// ids.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl core::fmt::Display + 'a {
        with_store(self, store)
    }
}
impl Describe for SubsetAuthoringError {
    fn describe(&self, f: &mut core::fmt::Formatter<'_>, names: Names<'_>) -> core::fmt::Result {
        f.write_str("cannot author geometry subset: ")?;
        match self {
            Self::CannotEdit(error) => error.describe(f, names),
            Self::InvalidName => f.write_str("the name is not a single prim identifier"),
            Self::Geometry(error) => error.describe(f, names),
            Self::Indices(problem) => write!(f, "the indices do not fit the geometry: {problem}"),
            Self::IncompatibleFamilyDeclaration => f.write_str(
                "the family attribute is declared as something other than a scalar `token`",
            ),
            Self::NameTaken(path) => write!(
                f,
                "{} already exists and is not a `GeomSubset`; choose another name",
                names.path(*path)
            ),
            Self::UnrestrictedMaterialBindFamily => f.write_str(
                "a `materialBind` family cannot be `unrestricted`: bound subsets must not overlap",
            ),
        }
    }
}
impl core::fmt::Display for SubsetAuthoringError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.describe(f, Names(None))
    }
}
impl core::error::Error for SubsetAuthoringError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Geometry(error) => Some(error),
            Self::CannotEdit(error) => Some(error),
            _ => None,
        }
    }
}
impl From<SubsetError> for SubsetAuthoringError {
    fn from(value: SubsetError) -> Self {
        Self::Geometry(value)
    }
}
impl From<crate::CannotEdit> for SubsetAuthoringError {
    fn from(value: crate::CannotEdit) -> Self {
        Self::CannotEdit(value)
    }
}

impl crate::usd_geom::GeomSubsetEdit {
    /// Collects a new subset under existing composed geometry, using `name`,
    /// then `name_1`, `name_2`, … to avoid composed and pending prims.
    ///
    /// Checks the requested element type and indices against the parent's
    /// composed default-time topology, preserving deferred decode errors.
    /// Edges and segments use flattened pairs. Geometry/topology edits queued
    /// in this batch are not part of that snapshot. Unrestricted duplicates
    /// are preserved; this does not validate overlap or partition coverage.
    /// Apply the transaction, then use `Imageable::try_validate_subset_family`
    /// to inspect the complete family's contract and animated samples.
    ///
    /// With a nonempty `family` and a `family_type`, the parent's
    /// `subsetFamily:<family>:familyType` is authored too, as
    /// [`crate::usd_geom::ImageableEdit::set_subset_family_type`] authors it;
    /// `None` (or no family) leaves the family's type as it is, as C++ does
    /// for an empty `familyType` token. C++ takes any token; the
    /// [`SubsetFamilyType`] enum names the three the schema allows, so an
    /// unknown family type cannot be authored here.
    ///
    /// Rejection collects no edits. AOUSD Core §7.3, §12.3–12.5;
    /// OpenUSD `UsdGeomSubset::CreateUniqueGeomSubset`.
    ///
    /// # Errors
    ///
    /// [`SubsetAuthoringError::InvalidName`] for a bad name, then
    /// [`SubsetAuthoringError::CannotEdit`] for a subset beneath an instance
    /// (geometry that is an instance or an instance proxy) or in a prototype
    /// ([`SchemaEdit::can_edit`](crate::SchemaEdit::can_edit)), then why the
    /// geometry or indices do not fit, then why the family type cannot be
    /// authored ([`SubsetAuthoringError::InvalidName`] for a family that is
    /// not an identifier, [`SubsetAuthoringError::IncompatibleFamilyDeclaration`]).
    #[doc(alias = "CreateUniqueGeomSubset")]
    pub fn create_unique(
        edit: &mut crate::SchemaEdit<'_>,
        parent: PathId,
        name: &str,
        element: SubsetElementType,
        indices: &[i32],
        family: &str,
        family_type: Option<SubsetFamilyType>,
    ) -> Result<Self, SubsetAuthoringError> {
        edit.group(|edit| {
            let path = edit
                .unique_child_path(parent, name)
                .ok_or(SubsetAuthoringError::InvalidName)?;
            // OpenUSD: `DefinePrim` refuses a subset beneath an instance or
            // in a prototype; checked before the parent's geometry, which
            // a prototype prim does not have here.
            edit.can_edit(path)?;
            check_indices(edit, parent, &element, indices)?;
            let subset = Subset::define(edit, path)?;
            subset
                .set_element_type(edit, element)
                .set_family_name(edit, family)
                .set_indices(edit, indices);
            author_created_family_type(edit, parent, family, family_type)?;
            Ok(subset)
        })
    }

    /// Collects the subset `name` under existing composed geometry: a new
    /// one, or the subset already there (composed, or defined earlier in
    /// this edit) with its element type, indices and family re-authored, as
    /// OpenUSD's `CreateGeomSubset` reuses it. Its other opinions, such as a
    /// material binding, stay.
    ///
    /// Indices are checked as [`Self::create_unique`] checks them, where
    /// C++ authors any indices. A prim of another type at that name is
    /// refused with [`SubsetAuthoringError::NameTaken`]; C++ would retype it.
    /// `family_type` is authored on the parent as [`Self::create_unique`]
    /// authors it.
    /// Rejection collects no edits. AOUSD Core §7.3, §12.3–12.5;
    /// OpenUSD `UsdGeomSubset::CreateGeomSubset`.
    ///
    /// # Errors
    ///
    /// As [`Self::create_unique`], and
    /// [`SubsetAuthoringError::NameTaken`].
    #[doc(alias = "CreateGeomSubset")]
    pub fn create(
        edit: &mut crate::SchemaEdit<'_>,
        parent: PathId,
        name: &str,
        element: SubsetElementType,
        indices: &[i32],
        family: &str,
        family_type: Option<SubsetFamilyType>,
    ) -> Result<Self, SubsetAuthoringError> {
        edit.group(|edit| {
            let path = child_path(edit, parent, name)?;
            // OpenUSD: `DefinePrim` refuses a subset beneath an instance or
            // in a prototype, whether or not it is already there; checked
            // before the parent's geometry, as in `create_unique`.
            edit.can_edit(path)?;
            check_indices(edit, parent, &element, indices)?;
            let subset = if edit.exists(path) {
                if !edit.is_a(path, Subset::SCHEMA) {
                    return Err(SubsetAuthoringError::NameTaken(path));
                }
                Self::from_prim(crate::PrimEdit::from_path_unchecked(path))
            } else {
                Subset::define(edit, path)?
            };
            subset
                .set_element_type(edit, element)
                .set_family_name(edit, family)
                .set_indices(edit, indices);
            author_created_family_type(edit, parent, family, family_type)?;
            Ok(subset)
        })
    }
}

/// The child `name` of `parent`, for a single prim identifier.
fn child_path(
    edit: &mut crate::SchemaEdit<'_>,
    parent: PathId,
    name: &str,
) -> Result<PathId, SubsetAuthoringError> {
    let parsed = layerstack::Path::parse_absolute(&format!("/{name}"), edit.tokens())
        .map_err(|_| SubsetAuthoringError::InvalidName)?;
    if parsed.depth() != 1 {
        return Err(SubsetAuthoringError::InvalidName);
    }
    let token = edit.tokens().intern(name);
    Ok(edit.child_path(parent, token))
}

/// Checks a new subset's element type and indices against the parent's
/// composed default-time topology.
fn check_indices(
    edit: &crate::SchemaEdit<'_>,
    parent: PathId,
    element: &SubsetElementType,
    indices: &[i32],
) -> Result<(), SubsetAuthoringError> {
    let scene = edit.scene();
    let geom = Imageable::get(&scene, parent)
        .filter(|geom| valid_geom(geom, element))
        .ok_or(SubsetError::InvalidGeometry)?;
    match elements(&geom, element, Time::Default)? {
        Elements::Scalar(count) => {
            if let Some(&index) = indices
                .iter()
                .find(|&&index| usize::try_from(index).map_or(true, |index| index >= count))
            {
                return Err(SubsetAuthoringError::Indices(
                    SubsetProblemKind::InvalidIndex(index),
                ));
            }
        }
        Elements::Pairs(possible) => {
            if !indices.len().is_multiple_of(2) {
                return Err(SubsetAuthoringError::Indices(
                    SubsetProblemKind::OddPairCount,
                ));
            }
            for pair in indices.as_chunks::<2>().0 {
                let pair = if *element == SubsetElementType::Edge {
                    [pair[0].min(pair[1]), pair[0].max(pair[1])]
                } else {
                    *pair
                };
                if !possible.contains(&pair) {
                    return Err(SubsetAuthoringError::Indices(
                        SubsetProblemKind::InvalidPair(pair),
                    ));
                }
            }
            // Match ValidateFamily's native edge-count bound as
            // well as membership, including non-contiguous vertices.
            if *element == SubsetElementType::Edge
                && let Some(&index) = indices.iter().find(|&&index| {
                    usize::try_from(index).map_or(true, |index| index >= possible.len())
                })
            {
                return Err(SubsetAuthoringError::Indices(
                    SubsetProblemKind::InvalidIndex(index),
                ));
            }
        }
    }
    Ok(())
}

impl crate::usd_geom::ImageableEdit {
    /// Authors a family's declared ownership contract. An absent attribute is
    /// created as a uniform token; existing scalar-token declarations retain
    /// their metadata. Incompatible declarations reject without collecting edits.
    /// The name must be a nonempty identifier. This collects the declaration;
    /// it does not claim the family's indices satisfy it. After applying the
    /// batch, use `Imageable::try_validate_subset_family` for full validation.
    /// AOUSD Core §12.2–12.5; OpenUSD `UsdGeomSubset::SetFamilyType`.
    #[doc(alias = "SetFamilyType")]
    pub fn set_subset_family_type(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        family: &str,
        family_type: SubsetFamilyType,
    ) -> Result<(), SubsetAuthoringError> {
        edit.group(|edit| author_family_type(edit, self.path(), family, family_type))
    }
}

/// The family type a new subset's family gets: authored only for a
/// nonempty `family` and a type.
///
/// OpenUSD: `UsdGeomSubset::CreateGeomSubset` calls `SetFamilyType` when
/// neither `familyName` nor `familyType` is empty.
fn author_created_family_type(
    edit: &mut crate::SchemaEdit<'_>,
    parent: PathId,
    family: &str,
    family_type: Option<SubsetFamilyType>,
) -> Result<(), SubsetAuthoringError> {
    match family_type {
        Some(family_type) if !family.is_empty() => {
            author_family_type(edit, parent, family, family_type)
        }
        _ => Ok(()),
    }
}

/// Authors `subsetFamily:<family>:familyType` on `parent`.
///
/// OpenUSD: `UsdGeomSubset::SetFamilyType`.
fn author_family_type(
    edit: &mut crate::SchemaEdit<'_>,
    parent: PathId,
    family: &str,
    family_type: SubsetFamilyType,
) -> Result<(), SubsetAuthoringError> {
    let parsed = layerstack::Path::parse_absolute(&format!("/{family}"), edit.tokens())
        .map_err(|_| SubsetAuthoringError::InvalidName)?;
    if parsed.depth() != 1 {
        return Err(SubsetAuthoringError::InvalidName);
    }
    let name = format!("subsetFamily:{family}:familyType");
    let declared = edit.property_kind(parent, &name).is_some();
    match edit.attribute_type(parent, &name) {
        Some(ty) if ty.is_array() || !matches!(ty.default_scalar(), Value::Token(_)) => {
            return Err(SubsetAuthoringError::IncompatibleFamilyDeclaration);
        }
        Some(_) => {}
        None if declared => {
            return Err(SubsetAuthoringError::IncompatibleFamilyDeclaration);
        }
        None => {
            edit.create_uniform_attribute(
                parent,
                &name,
                layerstack::PropertyType::parse("token").expect("a registered value type"),
            );
        }
    }
    let value = crate::value::write_token(family_type.token(), edit.tokens());
    edit.set_value(parent, &name, None, value);
    Ok(())
}

/// Invalid or unreadable geometry needed for a subset query.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubsetError {
    /// The element type does not apply to the owning geometry.
    InvalidGeometry,
    /// Required topology is absent or has an incompatible value type.
    MissingTopology,
    /// Topology contains invalid counts, indices or curve tokens.
    InvalidTopology,
    /// Retained topology or subset indices failed to decode.
    Decode {
        /// The failed attribute.
        property: PropertyPath,
        /// The original deferred source error.
        error: ArrayReadError,
    },
}
impl SubsetError {
    /// The error in words, an undecodable attribute written as its path
    /// with `store`, the store the stage was composed from. `Display`
    /// writes the same with ids.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl core::fmt::Display + 'a {
        with_store(self, store)
    }
}
impl Describe for SubsetError {
    fn describe(&self, f: &mut core::fmt::Formatter<'_>, names: Names<'_>) -> core::fmt::Result {
        f.write_str("invalid subset geometry: ")?;
        match self {
            Self::InvalidGeometry => {
                f.write_str("the element type does not apply to the owning geometry")
            }
            Self::MissingTopology => {
                f.write_str("the geometry's topology is absent or has another value type")
            }
            Self::InvalidTopology => {
                f.write_str("the geometry's topology has invalid counts, indices or curve tokens")
            }
            Self::Decode { property, error } => {
                write!(f, "cannot decode {}: {error}", names.property(*property))
            }
        }
    }
}
impl core::fmt::Display for SubsetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.describe(f, Names(None))
    }
}
impl core::error::Error for SubsetError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Decode { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// One violation of a subset family's contract.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubsetProblemKind {
    /// Geometry cannot support the requested element type.
    InvalidGeometry,
    /// A family member declares a different element type.
    ElementTypeMismatch,
    /// Required topology is missing or malformed.
    InvalidTopology,
    /// No family member supplies indices at any evaluated time.
    NoIndices,
    /// An edge or segment has an incomplete index pair.
    OddPairCount,
    /// A restricted family repeats an element index, within or across members.
    DuplicateIndex(i32),
    /// A restricted family repeats an edge or segment pair.
    DuplicatePair([i32; 2]),
    /// An index is negative or outside the element range.
    InvalidIndex(i32),
    /// An edge or segment does not exist on the parent geometry.
    InvalidPair([i32; 2]),
    /// A partition leaves some of the geometry unassigned.
    IncompletePartition,
}
impl core::fmt::Display for SubsetProblemKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidGeometry => f.write_str("the geometry cannot support the element type"),
            Self::ElementTypeMismatch => {
                f.write_str("a family member declares a different element type")
            }
            Self::InvalidTopology => f.write_str("the geometry's topology is missing or malformed"),
            Self::NoIndices => {
                f.write_str("no family member supplies indices at any evaluated time")
            }
            Self::OddPairCount => f.write_str("an edge or segment has an incomplete index pair"),
            Self::DuplicateIndex(index) => {
                write!(f, "the restricted family repeats the element index {index}")
            }
            Self::DuplicatePair([a, b]) => {
                write!(f, "the restricted family repeats the pair ({a}, {b})")
            }
            Self::InvalidIndex(index) => {
                write!(
                    f,
                    "the index {index} is negative or past the geometry's elements"
                )
            }
            Self::InvalidPair([a, b]) => {
                write!(
                    f,
                    "the pair ({a}, {b}) is not an edge or segment of the geometry"
                )
            }
            Self::IncompletePartition => {
                f.write_str("the partition leaves some of the geometry unassigned")
            }
        }
    }
}
/// A validation problem with its source prim and stage time.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct SubsetProblem {
    /// Family member, or the owning geometry for family-wide problems.
    pub path: PathId,
    /// Default time or the numeric sample at which the problem occurs.
    pub time: Time,
    /// The violated contract.
    pub kind: SubsetProblemKind,
}
/// All problems found while validating a subset family.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct SubsetValidation {
    /// Ordered problems, including violations at different sample times.
    pub problems: Vec<SubsetProblem>,
}
impl SubsetValidation {
    /// Whether no violations were found.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.problems.is_empty()
    }
    fn push(&mut self, path: PathId, time: Time, kind: SubsetProblemKind) {
        self.problems.push(SubsetProblem { path, time, kind });
    }
}

fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &str,
    time: Time,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Result<Option<T>, SubsetError> {
    prim.try_read_value(name, time, decode)
        .map_err(|error| SubsetError::Decode {
            property: prim
                .property_path(name)
                .expect("failed attribute is interned"),
            error,
        })
}
fn sample_times(prim: &PrimView<'_>, name: &str) -> Vec<f64> {
    prim.attribute(name)
        .map_or_else(Vec::new, layerstack::Attribute::sample_times)
}
fn valid_geom(geom: &Imageable<'_>, element: &SubsetElementType) -> bool {
    let scene = geom.scene();
    let path = geom.path();
    match element {
        SubsetElementType::Face => scene.is_a(path, "Mesh") || scene.is_a(path, "TetMesh"),
        SubsetElementType::Point => scene.is_a(path, "PointBased"),
        SubsetElementType::Edge => scene.is_a(path, "Mesh"),
        SubsetElementType::Segment => scene.is_a(path, "BasisCurves"),
        SubsetElementType::Tetrahedron => scene.is_a(path, "TetMesh"),
        SubsetElementType::Other(_) => false,
    }
}
#[derive(Debug)]
enum Elements {
    Scalar(usize),
    Pairs(BTreeSet<[i32; 2]>),
}
impl Elements {
    fn len(&self) -> usize {
        match self {
            Self::Scalar(n) => *n,
            Self::Pairs(p) => p.len(),
        }
    }
}
fn elements(
    geom: &Imageable<'_>,
    element: &SubsetElementType,
    time: Time,
) -> Result<Elements, SubsetError> {
    let missing = SubsetError::MissingTopology;
    let ints =
        |name: &str| read(geom, name, time, crate::value::read_int_array)?.ok_or(missing.clone());
    match element {
        SubsetElementType::Face => Ok(Elements::Scalar(
            if geom.scene().is_a(geom.path(), "Mesh") {
                ints("faceVertexCounts")?.len()
            } else {
                read(
                    geom,
                    "surfaceFaceVertexIndices",
                    time,
                    crate::value::read_int3_array,
                )?
                .ok_or(missing.clone())?
                .len()
            },
        )),
        SubsetElementType::Point => Ok(Elements::Scalar(
            read(geom, "points", time, |v, t| {
                crate::value::read_float3_array(v, t).map(|a| a.len())
            })?
            .ok_or(missing)?,
        )),
        SubsetElementType::Tetrahedron => Ok(Elements::Scalar(
            read(
                geom,
                "tetVertexIndices",
                time,
                crate::value::read_int4_array,
            )?
            .ok_or(missing.clone())?
            .len(),
        )),
        SubsetElementType::Edge => {
            let counts = ints("faceVertexCounts")?;
            let indices = ints("faceVertexIndices")?;
            let n = read(geom, "points", time, crate::value::read_float3_array)?
                .ok_or(missing)?
                .len();
            validate_mesh_topology(&indices, &counts, n)
                .map_err(|_| SubsetError::InvalidTopology)?;
            let mut pairs = BTreeSet::new();
            let mut offset = 0;
            for count in counts {
                let count = usize::try_from(count).map_err(|_| SubsetError::InvalidTopology)?;
                if count != 0 {
                    for i in 0..count {
                        let a = indices[offset + i];
                        let b = indices[offset + (i + 1) % count];
                        pairs.insert([a.min(b), a.max(b)]);
                    }
                }
                offset += count;
            }
            Ok(Elements::Pairs(pairs))
        }
        SubsetElementType::Segment => {
            let counts = ints("curveVertexCounts")?;
            let token = |name| {
                read(geom, name, time, crate::value::read_token)?
                    .map(String::from)
                    .ok_or(SubsetError::MissingTopology)
            };
            let curve_type = token("type")?;
            let basis = token("basis")?;
            let wrap = token("wrap")?;
            let mut pairs = BTreeSet::new();
            for (i, n) in counts.into_iter().enumerate() {
                if n < 0 {
                    return Err(SubsetError::InvalidTopology);
                }
                // OpenUSD: UsdGeomBasisCurves::ComputeSegmentCounts (no tessellation).
                let segments = match (curve_type.as_str(), basis.as_str(), wrap.as_str()) {
                    ("linear", _, "periodic") => n,
                    ("linear", _, "nonperiodic" | "pinned") => n - 1,
                    ("cubic", "bezier", "periodic") => n / 3,
                    ("cubic", "bezier", "nonperiodic" | "pinned") => (n - 4) / 3 + 1,
                    ("cubic", "bspline" | "catmullRom", "periodic") => n,
                    ("cubic", "bspline" | "catmullRom", "nonperiodic") => n - 3,
                    ("cubic", "bspline" | "catmullRom", "pinned") => n - 1,
                    _ => return Err(SubsetError::InvalidTopology),
                };
                if segments < 0 {
                    return Err(SubsetError::InvalidTopology);
                }
                let i = i32::try_from(i).map_err(|_| SubsetError::InvalidTopology)?;
                pairs.extend((0..segments).map(|j| [i, j]));
            }
            Ok(Elements::Pairs(pairs))
        }
        SubsetElementType::Other(_) => Err(SubsetError::InvalidGeometry),
    }
}
fn topology_names(element: &SubsetElementType, geom: &Imageable<'_>) -> &'static [&'static str] {
    match element {
        SubsetElementType::Face if geom.scene().is_a(geom.path(), "TetMesh") => {
            &["surfaceFaceVertexIndices"]
        }
        SubsetElementType::Face => &["faceVertexCounts"],
        SubsetElementType::Point => &["points"],
        SubsetElementType::Edge => &["faceVertexCounts", "faceVertexIndices"],
        SubsetElementType::Segment => &["curveVertexCounts"],
        _ => &["tetVertexIndices"],
    }
}
// Ordinary invalid geometry remains a family diagnostic; failed deferred
// storage must stop checked validation before it can appear empty or unassigned.
fn validation_elements(
    geom: &Imageable<'_>,
    element: &SubsetElementType,
    time: Time,
) -> Result<Option<Elements>, SubsetError> {
    match elements(geom, element, time) {
        Ok(elements) => Ok(Some(elements)),
        Err(error @ SubsetError::Decode { .. }) => Err(error),
        Err(_) => Ok(None),
    }
}
impl<'a> Imageable<'a> {
    /// Direct, active, concrete defining subset children in composed child order.
    /// `None` or an empty filter matches every element type or family.
    /// OpenUSD: `UsdGeomSubset::GetGeomSubsets` (no recursive descendant search).
    #[must_use]
    #[doc(alias = "UsdGeomSubset::GetGeomSubsets")]
    #[doc(alias = "GetGeomSubsets")]
    pub fn geom_subsets(
        &self,
        element: Option<&SubsetElementType>,
        family: Option<&str>,
    ) -> Vec<Subset<'a>> {
        let scene = self.scene();
        let Some(prim) = scene.prim(self.path()) else {
            return Vec::new();
        };
        prim.children()
            .filter_map(|child| {
                if child.specifier() != layerstack::Specifier::Def || child.is_abstract() {
                    return None;
                }
                let path = child.path();
                let subset = Subset::get(&scene, path)?;
                if subset.metadata().value("active") == Some(Value::Bool(false)) {
                    return None;
                }
                if element.is_some_and(|e| subset.element_type().as_ref() != Some(e))
                    || family
                        .filter(|f| !f.is_empty())
                        .is_some_and(|f| subset.family_name() != Some(f))
                {
                    return None;
                }
                Some(subset)
            })
            .collect()
    }
    /// The family's type, defaulting to `unrestricted` for an absent or empty token.
    /// OpenUSD: `UsdGeomSubset::GetFamilyType`.
    #[must_use]
    #[doc(alias = "UsdGeomSubset::GetFamilyType")]
    #[doc(alias = "GetFamilyType")]
    pub fn subset_family_type(&self, family: &str) -> String {
        self.read_value(
            &format!("subsetFamily:{family}:familyType"),
            crate::value::read_token,
        )
        .filter(|s| !s.is_empty())
        .unwrap_or("unrestricted")
        .into()
    }
    /// Checks the family at default time and every composed subset-index sample.
    /// Validates faces, points, edges, curve segments and tetrahedrons, including
    /// overlaps, bounds, missing indices and partition coverage. Unknown family
    /// tokens enforce non-overlap, matching OpenUSD's restricted-family behavior.
    /// Malformed topology produces diagnostics instead of unsafe indexing.
    /// Deferred decode failures become one `InvalidTopology` problem; use
    /// [`Self::try_validate_subset_family`] to retain their property and error.
    #[must_use]
    #[doc(alias = "UsdGeomSubset::ValidateFamily")]
    #[doc(alias = "ValidateFamily")]
    pub fn validate_subset_family(
        &self,
        element: &SubsetElementType,
        family: &str,
    ) -> SubsetValidation {
        self.validate_subset_family_impl(element, family, None)
    }
    /// Checks a family against topology and indices at one requested stage time.
    /// Useful for importers evaluating a snapshot instead of auditing all samples.
    /// The same bounds, overlap and partition rules as `validate_subset_family`
    /// apply. AOUSD Core §12.3–12.5 (time-based attribute resolution).
    /// Deferred decode failures become one `InvalidTopology` problem; use
    /// [`Self::try_validate_subset_family_at`] to retain their property and error.
    #[must_use]
    pub fn validate_subset_family_at(
        &self,
        element: &SubsetElementType,
        family: &str,
        time: Time,
    ) -> SubsetValidation {
        self.validate_subset_family_impl(element, family, Some(time))
    }
    fn validate_subset_family_impl(
        &self,
        element: &SubsetElementType,
        family: &str,
        selected_time: Option<Time>,
    ) -> SubsetValidation {
        self.try_validate_subset_family_impl(element, family, selected_time)
            .unwrap_or_else(|error| {
                let path = match error {
                    SubsetError::Decode { property, .. } => property.prim_path(),
                    _ => self.path(),
                };
                let mut result = SubsetValidation::default();
                result.push(
                    path,
                    selected_time.unwrap_or(Time::Default),
                    SubsetProblemKind::InvalidTopology,
                );
                result
            })
    }
    /// Checks the family at default time and every composed subset-index sample,
    /// preserving deferred topology and index errors with their property paths.
    /// Ordinary missing or malformed geometry remains in the returned problems.
    /// Typed default selection and numeric interpolation follow attribute getters.
    /// OpenUSD: `UsdGeomSubset::ValidateFamily`; AOUSD Core §12.3–12.5.
    #[doc(alias = "UsdGeomSubset::ValidateFamily")]
    #[doc(alias = "ValidateFamily")]
    pub fn try_validate_subset_family(
        &self,
        element: &SubsetElementType,
        family: &str,
    ) -> Result<SubsetValidation, SubsetError> {
        self.try_validate_subset_family_impl(element, family, None)
    }
    /// Checks a family only at `time`, preserving deferred array failures.
    /// Samples outside that snapshot are not decoded. Ordinary topology and
    /// membership violations are reported in the successful validation result.
    /// See [`Self::try_validate_subset_family`] for the family rules.
    pub fn try_validate_subset_family_at(
        &self,
        element: &SubsetElementType,
        family: &str,
        time: Time,
    ) -> Result<SubsetValidation, SubsetError> {
        self.try_validate_subset_family_impl(element, family, Some(time))
    }
    fn try_validate_subset_family_impl(
        &self,
        element: &SubsetElementType,
        family: &str,
        selected_time: Option<Time>,
    ) -> Result<SubsetValidation, SubsetError> {
        let mut result = SubsetValidation::default();
        let diagnostic_time = selected_time.unwrap_or(Time::Default);
        if !valid_geom(self, element) {
            result.push(
                self.path(),
                diagnostic_time,
                SubsetProblemKind::InvalidGeometry,
            );
            return Ok(result);
        }
        let subsets = self.geom_subsets(None, Some(family));
        for subset in &subsets {
            if subset.element_type().as_ref() != Some(element) {
                result.push(
                    subset.path(),
                    diagnostic_time,
                    SubsetProblemKind::ElementTypeMismatch,
                );
                return Ok(result);
            }
        }
        let family_type = read(
            self,
            &format!("subsetFamily:{family}:familyType"),
            Time::Default,
            crate::value::read_token,
        )?
        .filter(|s| !s.is_empty())
        .unwrap_or("unrestricted");
        let restricted = family_type != "unrestricted";
        let partition = family_type == "partition";
        let varying = selected_time.is_some()
            || topology_names(element, self)
                .iter()
                .any(|name| sample_times(self, name).len() > 1);
        if !varying
            && validation_elements(self, element, Time::held(f64::MIN))?
                .as_ref()
                .is_none_or(|e| e.len() == 0)
        {
            result.push(
                self.path(),
                Time::held(f64::MIN),
                SubsetProblemKind::InvalidTopology,
            );
        }
        let mut times = Vec::new();
        if selected_time.is_none() {
            times.extend(subsets.iter().flat_map(|s| sample_times(s, "indices")));
            sort::sort_by(&mut times, f64::total_cmp);
            times.dedup_by(|a, b| *a == *b);
        }
        let mut any = false;
        let evaluated: Vec<_> = selected_time.map_or_else(
            || {
                core::iter::once(Time::Default)
                    .chain(times.into_iter().map(Time::held))
                    .collect()
            },
            |time| alloc::vec![time],
        );
        for time in evaluated {
            let topology = if varying {
                validation_elements(self, element, time)?
            } else {
                validation_elements(self, element, Time::held(f64::MIN))?
            };
            let count = topology.as_ref().map_or(0, Elements::len);
            let mut indices = BTreeSet::new();
            let mut pairs = BTreeSet::new();
            for subset in &subsets {
                let data = read(subset, "indices", time, crate::value::read_int_array)?
                    .unwrap_or_default();
                any |= !data.is_empty();
                if matches!(
                    element,
                    SubsetElementType::Edge | SubsetElementType::Segment
                ) {
                    if data.len() % 2 != 0 {
                        result.push(subset.path(), time, SubsetProblemKind::OddPairCount);
                    }
                    for pair in data.as_chunks::<2>().0.iter() {
                        let pair = if *element == SubsetElementType::Edge {
                            [pair[0].min(pair[1]), pair[0].max(pair[1])]
                        } else {
                            [pair[0], pair[1]]
                        };
                        if !pairs.insert(pair) && restricted {
                            result.push(
                                subset.path(),
                                time,
                                SubsetProblemKind::DuplicatePair(pair),
                            );
                        }
                    }
                    indices.extend(data);
                } else {
                    for i in data {
                        if !indices.insert(i) && restricted {
                            result.push(subset.path(), time, SubsetProblemKind::DuplicateIndex(i));
                        }
                    }
                }
            }
            if !indices.is_empty() && count == 0 {
                result.push(self.path(), time, SubsetProblemKind::InvalidTopology);
            }
            if let Some(Elements::Pairs(possible)) = &topology {
                for pair in &pairs {
                    if !possible.contains(pair) {
                        result.push(self.path(), time, SubsetProblemKind::InvalidPair(*pair));
                    }
                }
                if partition && !possible.is_subset(&pairs) {
                    result.push(self.path(), time, SubsetProblemKind::IncompletePartition);
                }
            } else if partition && indices.len() != count {
                result.push(self.path(), time, SubsetProblemKind::IncompletePartition);
            }
            // OpenUSD 26.8 also bounds raw edge indices by the edge count.
            for i in indices {
                if i < 0
                    || (*element != SubsetElementType::Segment
                        && count > 0
                        && usize::try_from(i).map_or(true, |i| i >= count))
                {
                    result.push(self.path(), time, SubsetProblemKind::InvalidIndex(i));
                }
            }
        }
        if !any {
            result.push(self.path(), diagnostic_time, SubsetProblemKind::NoIndices);
        }
        Ok(result)
    }
    /// Sorted indices not assigned by matching subsets at `time`. Edges and
    /// segments are flattened pairs; edges are canonicalized to ascending order.
    /// Pair set subtraction sorts both inputs, including unsorted authored arrays.
    /// Returns errors when element type or required topology is invalid, and
    /// preserves deferred topology/member decode errors and their property paths.
    #[doc(alias = "UsdGeomSubset::GetUnassignedIndices")]
    #[doc(alias = "GetUnassignedIndices")]
    pub fn unassigned_subset_indices(
        &self,
        element: &SubsetElementType,
        family: &str,
        time: Time,
    ) -> Result<Vec<i32>, SubsetError> {
        if !valid_geom(self, element) {
            return Err(SubsetError::InvalidGeometry);
        }
        let subsets = self.geom_subsets(Some(element), Some(family));
        let data: Vec<Vec<i32>> = subsets
            .iter()
            .map(|s| {
                read(s, "indices", time, crate::value::read_int_array)
                    .map(Option::unwrap_or_default)
            })
            .collect::<Result<_, _>>()?;
        match elements(self, element, time)? {
            Elements::Scalar(n) => {
                let assigned: BTreeSet<_> = data.into_iter().flatten().collect();
                (0..n)
                    .filter_map(|i| match i32::try_from(i) {
                        Ok(i) if !assigned.contains(&i) => Some(Ok(i)),
                        Ok(_) => None,
                        Err(_) => Some(Err(SubsetError::InvalidTopology)),
                    })
                    .collect()
            }
            Elements::Pairs(possible) => {
                let assigned: BTreeSet<_> = data
                    .iter()
                    .flat_map(|data| data.as_chunks::<2>().0.iter())
                    .map(|p| {
                        if *element == SubsetElementType::Edge {
                            [p[0].min(p[1]), p[0].max(p[1])]
                        } else {
                            [p[0], p[1]]
                        }
                    })
                    .collect();
                Ok(possible
                    .difference(&assigned)
                    .flat_map(|pair| *pair)
                    .collect())
            }
        }
    }
}

/// Invalid mesh index/count storage.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MeshTopologyError {
    /// Face counts contain a negative value or overflow their total.
    InvalidFaceCounts,
    /// The face-count sum differs from the number of face indices.
    SizeMismatch,
    /// A face index does not address an existing point.
    InvalidVertexIndex(i32),
    /// Retained points, face counts or face indices failed to decode.
    Decode {
        /// The failed attribute.
        property: PropertyPath,
        /// The original deferred source error.
        error: ArrayReadError,
    },
}
impl MeshTopologyError {
    /// The error in words, an undecodable attribute written as its path
    /// with `store`, the store the stage was composed from. `Display`
    /// writes the same with ids.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl core::fmt::Display + 'a {
        with_store(self, store)
    }
}
impl Describe for MeshTopologyError {
    fn describe(&self, f: &mut core::fmt::Formatter<'_>, names: Names<'_>) -> core::fmt::Result {
        f.write_str("invalid mesh topology: ")?;
        match self {
            Self::InvalidFaceCounts => f.write_str(
                "a `faceVertexCounts` entry is negative or the counts overflow their total",
            ),
            Self::SizeMismatch => f.write_str(
                "the `faceVertexCounts` sum differs from the number of `faceVertexIndices`",
            ),
            Self::InvalidVertexIndex(index) => {
                write!(f, "the face vertex index {index} names no point")
            }
            Self::Decode { property, error } => {
                write!(f, "cannot decode {}: {error}", names.property(*property))
            }
        }
    }
}
impl core::fmt::Display for MeshTopologyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.describe(f, Names(None))
    }
}
impl core::error::Error for MeshTopologyError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Decode { error, .. } => Some(error),
            _ => None,
        }
    }
}
/// Checks count/index consistency and point-index bounds without scene reads.
/// Negative counts are rejected explicitly; no allocation depends on their sum.
/// OpenUSD: `UsdGeomMesh::ValidateTopology`.
#[doc(alias = "UsdGeomMesh::ValidateTopology")]
#[doc(alias = "ValidateTopology")]
pub fn validate_mesh_topology(
    indices: &[i32],
    counts: &[i32],
    num_points: usize,
) -> Result<(), MeshTopologyError> {
    let sum = counts
        .iter()
        .try_fold(0_usize, |sum, &n| sum.checked_add(usize::try_from(n).ok()?))
        .ok_or(MeshTopologyError::InvalidFaceCounts)?;
    if sum != indices.len() {
        return Err(MeshTopologyError::SizeMismatch);
    }
    for &i in indices {
        if usize::try_from(i).map_or(true, |i| i >= num_points) {
            return Err(MeshTopologyError::InvalidVertexIndex(i));
        }
    }
    Ok(())
}
impl Mesh<'_> {
    /// Validates this mesh's topology at `time`, including point-index
    /// bounds, as OpenUSD validates the arrays `Get` reads: an absent
    /// `points`, `faceVertexCounts` or `faceVertexIndices`, or one with
    /// another value type, reads as empty, so a mesh with none of them is
    /// valid, and one with faces but no points names points that are not
    /// there ([`MeshTopologyError::InvalidVertexIndex`]). An array that
    /// fails to decode is [`MeshTopologyError::Decode`], with its original
    /// error and attribute path. A negative face count is refused
    /// ([`validate_mesh_topology`]).
    ///
    /// This is [`Mesh::topology`]'s `validation`, without reading
    /// `holeIndices`.
    ///
    /// OpenUSD: `UsdGeomMesh::ValidateTopology` on the values of
    /// `GetPointsAttr`, `GetFaceVertexCountsAttr` and
    /// `GetFaceVertexIndicesAttr` (`pxr/usd/usdGeom/mesh.cpp`).
    #[doc(alias = "UsdGeomMesh::ValidateTopology")]
    #[doc(alias = "ValidateTopology")]
    pub fn validate_topology(&self, time: Time) -> Result<(), MeshTopologyError> {
        // The first attribute that fails to decode.
        let mut problem = None;
        let points = self.kept(
            &mut problem,
            "points",
            self.try_read_value("points", time, crate::value::read_float3_array_shared),
        );
        let counts = self.kept(
            &mut problem,
            "faceVertexCounts",
            self.try_face_vertex_counts(time),
        );
        let indices = self.kept(
            &mut problem,
            "faceVertexIndices",
            self.try_face_vertex_indices(time),
        );
        match problem {
            Some(problem) => Err(problem),
            None => validate_mesh_topology(&indices, &counts, points.len()),
        }
    }
}

/// A mesh's topology at one time: what a renderer needs to triangulate it,
/// with the result of validating it.
///
/// The arrays are the stage's shared buffers (no copy). An absent
/// attribute reads as an empty array, as OpenUSD's `Get` leaves it, so a
/// mesh without faces is valid and draws nothing; an attribute that fails
/// to decode also reads as empty, and `validation` is then
/// [`MeshTopologyError::Decode`]. Its `validation` is
/// [`Mesh::validate_topology`]'s, which the `MeshTopology` validation rule
/// reports.
/// `orientation` and `subdivision_scheme` are their schema fallbacks
/// (`rightHanded`, `catmullClark`) when unauthored.
///
/// OpenUSD: the `UsdGeomMesh` attribute reads and
/// `UsdGeomMesh::ValidateTopology`, except that a negative face count is
/// refused ([`validate_mesh_topology`]); `subdivisionScheme` says how a
/// renderer refines the faces, which this read does not do.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct MeshTopology {
    /// `faceVertexCounts`: the number of vertices of each face.
    pub face_vertex_counts: Arc<Vec<i32>>,
    /// `faceVertexIndices`: each face's vertices, as indices into `points`.
    pub face_vertex_indices: Arc<Vec<i32>>,
    /// `holeIndices`: the faces a renderer leaves out, by face index.
    pub hole_indices: Arc<Vec<i32>>,
    /// `orientation`: the winding of the faces' front sides.
    pub orientation: GprimOrientation,
    /// `subdivisionScheme`: how a renderer refines the faces.
    pub subdivision_scheme: MeshSubdivisionScheme,
    /// The number of `points` at the time, which `faceVertexIndices` must
    /// stay under.
    pub point_count: usize,
    /// The topology validated against `point_count`
    /// ([`validate_mesh_topology`]).
    pub validation: Result<(), MeshTopologyError>,
}

impl Mesh<'_> {
    /// Its topology at `time` ([`MeshTopology`]): the face counts, face
    /// indices and hole indices at `time`, its orientation and subdivision
    /// scheme, and the topology's validation. It never fails: an absent
    /// array reads as empty, as does an undecodable one, which
    /// `validation` reports.
    #[doc(alias = "UsdGeomMesh::ValidateTopology")]
    #[must_use]
    pub fn topology(&self, time: Time) -> MeshTopology {
        // The first attribute that fails to decode.
        let mut problem = None;
        let points = self.kept(
            &mut problem,
            "points",
            self.try_read_value("points", time, crate::value::read_float3_array_shared),
        );
        let face_vertex_counts = self.kept(
            &mut problem,
            "faceVertexCounts",
            self.try_face_vertex_counts(time),
        );
        let face_vertex_indices = self.kept(
            &mut problem,
            "faceVertexIndices",
            self.try_face_vertex_indices(time),
        );
        let hole_indices = self.kept(&mut problem, "holeIndices", self.try_hole_indices(time));
        let points = points.len();
        let validation = match problem {
            Some(problem) => Err(problem),
            None => validate_mesh_topology(&face_vertex_indices, &face_vertex_counts, points),
        };
        MeshTopology {
            face_vertex_counts,
            face_vertex_indices,
            hole_indices,
            orientation: self.orientation().unwrap_or(GprimOrientation::RightHanded),
            subdivision_scheme: self
                .subdivision_scheme()
                .unwrap_or(MeshSubdivisionScheme::CatmullClark),
            point_count: points,
            validation,
        }
    }

    /// The array `read` gave, or an empty one when it is absent or fails
    /// to decode, noting the failure in `problem` unless it already holds
    /// one.
    fn kept<T: Default>(
        &self,
        problem: &mut Option<MeshTopologyError>,
        name: &str,
        read: Result<Option<T>, ArrayReadError>,
    ) -> T {
        match read {
            Ok(Some(values)) => values,
            Ok(None) => T::default(),
            Err(error) => {
                problem.get_or_insert_with(|| MeshTopologyError::Decode {
                    property: self
                        .property_path(name)
                        .expect("failed attribute is interned"),
                    error,
                });
                T::default()
            }
        }
    }
}
