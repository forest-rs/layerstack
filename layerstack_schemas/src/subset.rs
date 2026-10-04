// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Subset discovery and topology validation; material evaluation stays in shading.
//! OpenUSD: `UsdGeomSubset::ValidateFamily`, `GetGeomSubsets`, `GetUnassignedIndices`
//! and `UsdGeomMesh::ValidateTopology`. Composition follows AOUSD Core §12.
use crate::{
    PrimView, Time,
    usd_geom::{GeomSubset as Subset, GeomSubsetElementType as SubsetElementType, Imageable, Mesh},
};
use alloc::{collections::BTreeSet, format, string::String, vec::Vec};
use layerstack::{PathId, TokenInterner, Value};

/// Invalid or unreadable geometry needed for a subset query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubsetError {
    /// The element type does not apply to the owning geometry.
    InvalidGeometry,
    /// Required topology cannot be read safely.
    MissingTopology,
    /// Topology contains invalid counts, indices or curve tokens.
    InvalidTopology,
}
impl core::fmt::Display for SubsetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid subset geometry: {self:?}")
    }
}
impl core::error::Error for SubsetError {}

/// One violation of a subset family's contract.
#[derive(Clone, Debug, PartialEq, Eq)]
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
/// A validation problem with its source prim and stage time.
#[derive(Clone, Debug, PartialEq)]
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
) -> Option<T> {
    match time {
        Time::Default => prim.read_value(name, decode),
        Time::At {
            code,
            interpolation,
        } => prim.read_value_at(name, code, interpolation, decode),
    }
}
fn sample_times(prim: &PrimView<'_>, name: &str) -> Vec<f64> {
    prim.property_path(name).map_or_else(Vec::new, |p| {
        prim.scene()
            .stage()
            .property_sample_times(p.prim_path(), p.property())
    })
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
        |name: &str| read(geom, name, time, crate::value::read_int_array).ok_or(missing.clone());
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
                )
                .ok_or(missing.clone())?
                .len()
            },
        )),
        SubsetElementType::Point => Ok(Elements::Scalar(
            read(geom, "points", time, |v, t| {
                crate::value::read_float3_array(v, t).map(|a| a.len())
            })
            .ok_or(missing)?,
        )),
        SubsetElementType::Tetrahedron => Ok(Elements::Scalar(
            read(
                geom,
                "tetVertexIndices",
                time,
                crate::value::read_int4_array,
            )
            .ok_or(missing.clone())?
            .len(),
        )),
        SubsetElementType::Edge => {
            let counts = ints("faceVertexCounts")?;
            let indices = ints("faceVertexIndices")?;
            let n = read(geom, "points", time, crate::value::read_float3_array)
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
                read(geom, name, time, crate::value::read_token)
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
impl<'a> Imageable<'a> {
    /// Direct, active, concrete defining subset children in composed child order.
    /// `None` or an empty filter matches every element type or family.
    /// OpenUSD: `UsdGeomSubset::GetGeomSubsets` (no recursive descendant search).
    #[must_use]
    pub fn geom_subsets(
        &self,
        element: Option<&SubsetElementType>,
        family: Option<&str>,
    ) -> Vec<Subset<'a>> {
        let scene = self.scene();
        scene
            .stage()
            .children_of(self.path())
            .unwrap_or_default()
            .iter()
            .filter_map(|&path| {
                if scene.stage().resolve_specifier(path, scene.store())
                    != Some(layerstack::Specifier::Def)
                    || scene.stage().is_abstract(path, scene.store())
                {
                    return None;
                }
                let subset = Subset::new(&scene, path)?;
                if subset.metadata_value("active") == Some(Value::Bool(false)) {
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
    #[must_use]
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
        let mut result = SubsetValidation::default();
        let diagnostic_time = selected_time.unwrap_or(Time::Default);
        if !valid_geom(self, element) {
            result.push(
                self.path(),
                diagnostic_time,
                SubsetProblemKind::InvalidGeometry,
            );
            return result;
        }
        let subsets = self.geom_subsets(None, Some(family));
        for subset in &subsets {
            if subset.element_type().as_ref() != Some(element) {
                result.push(
                    subset.path(),
                    diagnostic_time,
                    SubsetProblemKind::ElementTypeMismatch,
                );
                return result;
            }
        }
        let family_type = self.subset_family_type(family);
        let restricted = family_type != "unrestricted";
        let partition = family_type == "partition";
        let varying = selected_time.is_some()
            || topology_names(element, self)
                .iter()
                .any(|name| sample_times(self, name).len() > 1);
        if !varying
            && elements(self, element, Time::held(f64::MIN))
                .as_ref()
                .map_or(true, |e| e.len() == 0)
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
            times.sort_by(f64::total_cmp);
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
                elements(self, element, time)
            } else {
                elements(self, element, Time::held(f64::MIN))
            };
            let count = topology.as_ref().map_or(0, Elements::len);
            let mut indices = BTreeSet::new();
            let mut pairs = BTreeSet::new();
            for subset in &subsets {
                let data =
                    read(subset, "indices", time, crate::value::read_int_array).unwrap_or_default();
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
            if let Ok(Elements::Pairs(possible)) = &topology {
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
        result
    }
    /// Sorted indices not assigned by matching subsets at `time`. Edges and
    /// segments are flattened pairs; edges are canonicalized to ascending order.
    /// Pair set subtraction sorts both inputs, including unsorted authored arrays.
    /// Returns errors when element type or required topology is invalid.
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
            .map(|s| read(s, "indices", time, crate::value::read_int_array).unwrap_or_default())
            .collect();
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
pub enum MeshTopologyError {
    /// Required points, face counts or face indices are absent or unreadable.
    MissingAttribute,
    /// Face counts contain a negative value or overflow their total.
    InvalidFaceCounts,
    /// The face-count sum differs from the number of face indices.
    SizeMismatch,
    /// A face index does not address an existing point.
    InvalidVertexIndex(i32),
}
impl core::fmt::Display for MeshTopologyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid mesh topology: {self:?}")
    }
}
impl core::error::Error for MeshTopologyError {}
/// Checks count/index consistency and point-index bounds without scene reads.
/// Negative counts are rejected explicitly; no allocation depends on their sum.
/// OpenUSD: `UsdGeomMesh::ValidateTopology`.
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
    /// Validates this mesh's topology at `time`, including point-index bounds.
    /// Returns an error for missing required arrays or inconsistent topology.
    pub fn validate_topology(&self, time: Time) -> Result<(), MeshTopologyError> {
        let points = read(self, "points", time, crate::value::read_float3_array_shared)
            .ok_or(MeshTopologyError::MissingAttribute)?;
        let counts = read(
            self,
            "faceVertexCounts",
            time,
            crate::value::read_int_array_shared,
        )
        .ok_or(MeshTopologyError::MissingAttribute)?;
        let indices = read(
            self,
            "faceVertexIndices",
            time,
            crate::value::read_int_array_shared,
        )
        .ok_or(MeshTopologyError::MissingAttribute)?;
        validate_mesh_topology(&indices, &counts, points.len())
    }
}
