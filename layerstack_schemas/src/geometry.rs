// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checked, renderer-independent OpenUSD geometry algorithms.
//!
//! Owns topology/layout calculations and model geometry queries. Composition
//! and transforms remain owned by `Scene` and `XformCache`.
//! AOUSD Core §12.3 (attribute values), §11.4 (models); OpenUSD 26.8
//! `UsdGeomTetMesh`, `UsdGeomBasisCurves`, and `UsdGeomConstraintTarget`.
use crate::{
    PrimView, Scene, XformCache,
    usd_geom::{BasisCurves, GeomModelApi, TetMesh},
};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use layerstack::{PathId, PropertyPath, Time, Value};

/// Why a geometry calculation cannot consume its input safely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeometryError {
    /// An attribute is absent, blocked, or incompatible.
    MissingAttribute(&'static str),
    /// A topology index is negative or outside the point array.
    InvalidVertexIndex {
        /// Tetrahedron containing the index.
        element: usize,
        /// Invalid authored index.
        index: i32,
        /// Point count, if available for validation.
        point_count: Option<usize>,
    },
    /// A tetrahedron repeats a vertex index.
    RepeatedVertex {
        /// Tetrahedron with repeated indices.
        element: usize,
    },
    /// A point or intermediate geometric result is not finite.
    NonFinite,
    /// An unsupported orientation token.
    InvalidOrientation(String),
    /// An unsupported curve type, basis, or wrap token.
    InvalidCurveToken {
        /// Attribute containing the token.
        attribute: &'static str,
        /// Authored token.
        token: String,
    },
    /// A curve has a negative number of vertices.
    NegativeCurveCount {
        /// Curve index.
        curve: usize,
        /// Authored count.
        count: i32,
    },
    /// A summed layout size does not fit usize.
    SizeOverflow,
    /// A target lacks the required model, namespace, or matrix type.
    InvalidConstraintTarget(PropertyPath),
    /// An inversion query requires four points and one tetrahedron.
    EmptyTetMesh,
}
impl core::fmt::Display for GeometryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "geometry input: {self:?}")
    }
}
impl core::error::Error for GeometryError {}
const FACES: [[usize; 3]; 4] = [[1, 2, 3], [0, 3, 2], [0, 1, 3], [0, 2, 1]];
fn checked_tets(indices: &[[i32; 4]], points: Option<usize>) -> Result<(), GeometryError> {
    for (element, tet) in indices.iter().enumerate() {
        for (i, &index) in tet.iter().enumerate() {
            if index < 0 || points.is_some_and(|n| index as usize >= n) {
                return Err(GeometryError::InvalidVertexIndex {
                    element,
                    index,
                    point_count: points,
                });
            }
            if tet[..i].contains(&index) {
                return Err(GeometryError::RepeatedVertex { element });
            }
        }
    }
    Ok(())
}
/// Extracts faces occurring exactly once, preserving winding and sorting
/// oriented triples lexicographically. Faces shared by three or more elements
/// are excluded. Rejects negative and repeated indices; bounds require points.
/// Empty topology has an empty surface.
/// OpenUSD `ComputeSurfaceFaces`; AOUSD Core §12.3.
pub fn compute_surface_faces(indices: &[[i32; 4]]) -> Result<Vec<[i32; 3]>, GeometryError> {
    checked_tets(indices, None)?;
    let mut counts: BTreeMap<[i32; 3], (usize, [i32; 3])> = BTreeMap::new();
    for tet in indices {
        for face in FACES {
            let triangle = face.map(|i| tet[i]);
            let mut signature = triangle;
            signature.sort_unstable();
            counts.entry(signature).or_insert((0, triangle)).0 += 1;
        }
    }
    let mut result: Vec<_> = counts
        .into_values()
        .filter_map(|(n, face)| (n == 1).then_some(face))
        .collect();
    result.sort_unstable();
    Ok(result)
}
/// Finds elements whose face normals disagree with orientation, in element
/// order. Coplanar elements are not inverted. Uses f32 arithmetic and all four
/// faces, matching OpenUSD. Requires four points and one element. Rejects
/// malformed indices and nonfinite data before indexing.
/// OpenUSD `FindInvertedElements`; AOUSD Core §12.3.
pub fn find_inverted_elements(
    points: &[[f32; 3]],
    indices: &[[i32; 4]],
    orientation: &str,
) -> Result<Vec<usize>, GeometryError> {
    let sign = match orientation {
        "leftHanded" => 1.,
        "rightHanded" => -1.,
        _ => return Err(GeometryError::InvalidOrientation(orientation.into())),
    };
    if points.len() < 4 || indices.is_empty() {
        return Err(GeometryError::EmptyTetMesh);
    }
    checked_tets(indices, Some(points.len()))?;
    if !points.iter().flatten().all(|v| v.is_finite()) {
        return Err(GeometryError::NonFinite);
    }
    let mut inverted = Vec::new();
    for (element, tet) in indices.iter().enumerate() {
        let p = tet.map(|i| points[i as usize]);
        let center: [f32; 3] =
            core::array::from_fn(|i| ((p[0][i] + p[1][i]) + p[2][i] + p[3][i]) * 0.25);
        for face in FACES {
            let [a, b, c] = face.map(|i| p[i]);
            let u: [f32; 3] = core::array::from_fn(|i| b[i] - a[i]);
            let v: [f32; 3] = core::array::from_fn(|i| c[i] - a[i]);
            let normal = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let delta: [f32; 3] = core::array::from_fn(|i| center[i] - a[i]);
            let dot = (normal[0] * delta[0] + normal[1] * delta[1]) + normal[2] * delta[2];
            if !dot.is_finite() {
                return Err(GeometryError::NonFinite);
            }
            if sign * dot < 0. {
                inverted.push(element);
                break;
            }
        }
    }
    Ok(inverted)
}
fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &'static str,
    time: Time,
    convert: impl Fn(&Value, &'a layerstack::TokenInterner) -> Option<T>,
) -> Result<T, GeometryError> {
    match time {
        Time::Default => prim.read_value(name, convert),
        Time::At {
            code,
            interpolation,
        } => prim.read_value_at(name, code, interpolation, convert),
    }
    .ok_or(GeometryError::MissingAttribute(name))
}
impl TetMesh<'_> {
    /// Computes sorted boundary triangles at the requested time.
    pub fn compute_surface_faces(&self, time: Time) -> Result<Vec<[i32; 3]>, GeometryError> {
        compute_surface_faces(&read(
            self,
            "tetVertexIndices",
            time,
            crate::value::read_int4_array,
        )?)
    }
    /// Finds inverted elements. Orientation is uniform, read at default time.
    pub fn find_inverted_elements(&self, time: Time) -> Result<Vec<usize>, GeometryError> {
        let points = read(self, "points", time, crate::value::read_float3_array)?;
        let indices = read(
            self,
            "tetVertexIndices",
            time,
            crate::value::read_int4_array,
        )?;
        let orientation = self
            .orientation()
            .ok_or(GeometryError::MissingAttribute("orientation"))?;
        find_inverted_elements(&points, &indices, orientation.as_str())
    }
}
/// Owned curve topology and uniform tokens, usable independently of a stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasisCurvesTopology {
    /// Authored control-point count per curve.
    pub vertex_counts: Vec<i32>,
    /// linear or cubic.
    pub curve_type: String,
    /// bezier, bspline, or catmullRom; ignored for linear curves.
    pub basis: String,
    /// nonperiodic, periodic, or pinned.
    pub wrap: String,
}
/// Primvar interpolation selected by OpenUSD precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveInterpolation {
    /// One value for all curves.
    Constant,
    /// One value per curve.
    Uniform,
    /// Segment-boundary values.
    Varying,
    /// One value per control point.
    Vertex,
}
impl CurveInterpolation {
    /// USD interpolation token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Constant => "constant",
            Self::Uniform => "uniform",
            Self::Varying => "varying",
            Self::Vertex => "vertex",
        }
    }
}
/// Computed curve primvar layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveDataSizes {
    /// One value per curve.
    pub uniform: usize,
    /// Number of segment-boundary values expected by OpenUSD.
    pub varying: usize,
    /// Number of control points.
    pub vertex: usize,
}
impl CurveDataSizes {
    /// Candidates in constant, uniform, varying, vertex precedence order.
    #[must_use]
    pub fn interpolation_info(self) -> [(CurveInterpolation, usize); 4] {
        [
            (CurveInterpolation::Constant, 1),
            (CurveInterpolation::Uniform, self.uniform),
            (CurveInterpolation::Varying, self.varying),
            (CurveInterpolation::Vertex, self.vertex),
        ]
    }
    /// First interpolation whose size matches; None for unmatched sizes.
    #[must_use]
    pub fn interpolation_for_size(self, size: usize) -> Option<CurveInterpolation> {
        self.interpolation_info()
            .into_iter()
            .find_map(|(i, n)| (n == size).then_some(i))
    }
}
impl BasisCurvesTopology {
    fn parameters(&self) -> Result<(bool, bool, bool, i32), GeometryError> {
        for (curve, &count) in self.vertex_counts.iter().enumerate() {
            if count < 0 {
                return Err(GeometryError::NegativeCurveCount { curve, count });
            }
        }
        let error = |attribute, token: &str| GeometryError::InvalidCurveToken {
            attribute,
            token: token.into(),
        };
        let linear = match self.curve_type.as_str() {
            "linear" => true,
            "cubic" => false,
            t => return Err(error("type", t)),
        };
        let (periodic, pinned) = match self.wrap.as_str() {
            "nonperiodic" => (false, false),
            "periodic" => (true, false),
            "pinned" => (false, true),
            t => return Err(error("wrap", t)),
        };
        let step = if linear {
            1
        } else {
            match self.basis.as_str() {
                "bezier" => 3,
                "bspline" | "catmullRom" => 1,
                t => return Err(error("basis", t)),
            }
        };
        Ok((linear, periodic, pinned, step))
    }
    /// Computes segment counts with C++ integer truncation. Counts below the
    /// basis minimum retain authored intent (including negative segment counts).
    /// Rejects negative vertex counts and unsupported tokens.
    /// OpenUSD `ComputeSegmentCounts`; AOUSD Core §12.3.
    pub fn segment_counts(&self) -> Result<Vec<i32>, GeometryError> {
        let (linear, periodic, pinned, step) = self.parameters()?;
        Ok(self
            .vertex_counts
            .iter()
            .map(|&n| {
                if linear {
                    if periodic { n } else { n - 1 }
                } else if step == 3 {
                    if periodic { n / 3 } else { (n - 4) / 3 + 1 }
                } else if periodic {
                    n
                } else if pinned {
                    n - 1
                } else {
                    n - 3
                }
            })
            .collect())
    }
    /// Computes uniform/varying/vertex sizes. Linear periodic varying data
    /// includes a closing value. Pinned cubic varying data uses the nonperiodic
    /// formula. Below four cubic vertices, requires two varying values,
    /// following OpenUSD's documented intent and avoiding unsigned underflow.
    /// OpenUSD `Compute*DataSize`; AOUSD Core §12.3.
    pub fn data_sizes(&self) -> Result<CurveDataSizes, GeometryError> {
        let (linear, periodic, _, step) = self.parameters()?;
        let mut sizes = CurveDataSizes {
            uniform: self.vertex_counts.len(),
            varying: 0,
            vertex: 0,
        };
        for &n in &self.vertex_counts {
            let varying = if linear {
                n as usize + usize::from(periodic)
            } else if periodic {
                (n / step) as usize
            } else {
                ((n - 4).max(0) / step + 2) as usize
            };
            sizes.vertex = sizes
                .vertex
                .checked_add(n as usize)
                .ok_or(GeometryError::SizeOverflow)?;
            sizes.varying = sizes
                .varying
                .checked_add(varying)
                .ok_or(GeometryError::SizeOverflow)?;
        }
        Ok(sizes)
    }
}
impl BasisCurves<'_> {
    /// Reads owned topology and uniform type, basis and wrap tokens.
    pub fn topology(&self, time: Time) -> Result<BasisCurvesTopology, GeometryError> {
        Ok(BasisCurvesTopology {
            vertex_counts: read(
                self,
                "curveVertexCounts",
                time,
                crate::value::read_int_array,
            )?,
            curve_type: read(self, "type", time, crate::value::read_token)?.into(),
            basis: read(self, "basis", time, crate::value::read_token)?.into(),
            wrap: read(self, "wrap", time, crate::value::read_token)?.into(),
        })
    }
    /// Computes segment counts from resolved topology.
    pub fn compute_segment_counts(&self, time: Time) -> Result<Vec<i32>, GeometryError> {
        self.topology(time)?.segment_counts()
    }
    /// Computes the complete curve data layout.
    pub fn compute_data_sizes(&self, time: Time) -> Result<CurveDataSizes, GeometryError> {
        self.topology(time)?.data_sizes()
    }
    /// Computes the number of curves, without requiring type/basis/wrap.
    pub fn compute_uniform_data_size(&self, time: Time) -> Result<usize, GeometryError> {
        let counts = read(
            self,
            "curveVertexCounts",
            time,
            crate::value::read_int_array,
        )?;
        Ok(counts.len())
    }
    /// Computes the varying data size from resolved curve topology.
    pub fn compute_varying_data_size(&self, time: Time) -> Result<usize, GeometryError> {
        Ok(self.compute_data_sizes(time)?.varying)
    }
    /// Sums control-point counts, without requiring type/basis/wrap.
    pub fn compute_vertex_data_size(&self, time: Time) -> Result<usize, GeometryError> {
        let counts = read(
            self,
            "curveVertexCounts",
            time,
            crate::value::read_int_array,
        )?;
        counts
            .iter()
            .enumerate()
            .try_fold(0_usize, |sum, (curve, &count)| {
                let count = usize::try_from(count)
                    .map_err(|_| GeometryError::NegativeCurveCount { curve, count })?;
                sum.checked_add(count).ok_or(GeometryError::SizeOverflow)
            })
    }
    /// Selects constant/uniform/varying/vertex interpolation, in that order.
    pub fn compute_interpolation_for_size(
        &self,
        size: usize,
        time: Time,
    ) -> Result<Option<CurveInterpolation>, GeometryError> {
        if size == 1 {
            return Ok(Some(CurveInterpolation::Constant));
        }
        if size == self.compute_uniform_data_size(time)? {
            return Ok(Some(CurveInterpolation::Uniform));
        }
        Ok(self.compute_data_sizes(time)?.interpolation_for_size(size))
    }
}
impl Scene<'_> {
    /// Computes inherited model:drawMode. Only models contribute; pseudo-root
    /// is excluded. A nonempty parent mode skips ancestor traversal after the
    /// prim itself is tested. Unknown tokens are preserved.
    /// OpenUSD `ComputeModelDrawMode`; AOUSD Core §11.4, §12.3.
    #[must_use]
    pub fn compute_model_draw_mode(&self, path: PathId, parent_draw_mode: Option<&str>) -> String {
        let authored = |path| {
            if self.parent(path).is_none() || !self.is_model(path) {
                return None;
            }
            PrimView::new(*self, path)
                .read_value("model:drawMode", crate::value::read_token)
                .filter(|mode| *mode != "inherited")
        };
        if let Some(mode) = authored(path) {
            return mode.into();
        }
        if let Some(mode) = parent_draw_mode.filter(|mode| !mode.is_empty()) {
            return mode.into();
        }
        let mut at = self.parent(path);
        while let Some(path) = at {
            if let Some(mode) = authored(path) {
                return mode.into();
            }
            at = self.parent(path);
        }
        "default".into()
    }
}
impl GeomModelApi<'_> {
    /// Computes inherited draw mode, optionally using a known parent mode.
    #[must_use]
    pub fn compute_model_draw_mode(&self, parent_draw_mode: Option<&str>) -> String {
        self.scene()
            .compute_model_draw_mode(self.path(), parent_draw_mode)
    }
}
/// Checked matrix-valued attribute in a model's constraintTargets namespace.
#[derive(Clone, Copy, Debug)]
pub struct ConstraintTarget<'a> {
    prim: PrimView<'a>,
    property: PropertyPath,
}
impl<'a> ConstraintTarget<'a> {
    /// Gets an attribute with matrix4d or frame4d type on a model prim, whose first
    /// namespace component is constraintTargets.
    /// OpenUSD `IsValid`; AOUSD Core §11.4, §12.3.
    #[must_use]
    pub fn new(scene: &Scene<'a>, property: PropertyPath) -> Option<Self> {
        let name = scene.store().tokens().resolve(property.property());
        let prim = PrimView::new(*scene, property.prim_path());
        let declared = scene
            .stage()
            .resolve_property_declaration(property.prim_path(), property.property());
        let ty = declared
            .as_ref()
            .and_then(|d| d.type_name.clone())
            .or_else(|| {
                scene
                    .stage()
                    .property_definition_ref(property.prim_path(), property.property())
                    .and_then(|d| d.type_name.clone())
            });
        let matrix = ty
            .is_some_and(|t| matches!(t.type_name.as_ref(), "matrix4d" | "frame4d") && !t.is_array);
        (scene.is_model(property.prim_path())
            && name.split(':').next() == Some("constraintTargets")
            && prim.has_attribute(name)
            && matrix)
            .then_some(Self { prim, property })
    }
    /// The underlying attribute path.
    #[must_use]
    pub fn path(&self) -> PropertyPath {
        self.property
    }
    /// Reads the local matrix; absent values and nonfinite values are errors.
    pub fn local_matrix(&self, time: Time) -> Result<[[f64; 4]; 4], GeometryError> {
        let name = self
            .prim
            .scene()
            .store()
            .tokens()
            .resolve(self.property.property());
        let matrix = match time {
            Time::Default => self.prim.read_value(name, crate::value::read_matrix4d),
            Time::At {
                code,
                interpolation,
            } => self
                .prim
                .read_value_at(name, code, interpolation, crate::value::read_matrix4d),
        }
        .ok_or(GeometryError::MissingAttribute("constraint target"))?;
        if !matrix.iter().flatten().all(|v| v.is_finite()) {
            return Err(GeometryError::NonFinite);
        }
        Ok(matrix)
    }
    /// Computes local constraint space times model-to-world in row-vector
    /// convention. Updates supplied cache time. Failed reads produce an error
    /// rather than OpenUSD's identity fallback.
    /// OpenUSD `ComputeInWorldSpace`; AOUSD Core §12.3.
    pub fn compute_in_world_space(
        &self,
        time: Time,
        cache: Option<&mut XformCache>,
    ) -> Result<[[f64; 4]; 4], GeometryError> {
        let local = self.local_matrix(time)?;
        let mut fresh;
        let cache = match cache {
            Some(cache) => {
                cache.set_time(time);
                cache
            }
            None => {
                fresh = XformCache::new(time);
                &mut fresh
            }
        };
        let world = cache
            .local_to_world(&self.prim.scene(), self.prim.path())
            .ok_or(GeometryError::InvalidConstraintTarget(self.property))?;
        let result = crate::gf::mul(&local, &world);
        if !result.iter().flatten().all(|v| v.is_finite()) {
            return Err(GeometryError::NonFinite);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn tet_surface_cpp_fixture() {
        let tets = [[0, 1, 2, 3], [0, 2, 1, 4]];
        assert_eq!(
            compute_surface_faces(&tets).unwrap(),
            vec![
                [0, 1, 3],
                [0, 2, 4],
                [0, 3, 2],
                [0, 4, 1],
                [1, 2, 3],
                [2, 1, 4]
            ]
        );
        assert!(
            compute_surface_faces(&[[0, 1, 2, 3]; 2])
                .unwrap()
                .is_empty()
        );
        assert!(
            compute_surface_faces(&[[0, 1, 2, 3]; 3])
                .unwrap()
                .is_empty()
        );
        assert!(compute_surface_faces(&[]).unwrap().is_empty());
        assert_eq!(
            compute_surface_faces(&[[0, 1, 1, 3]]),
            Err(GeometryError::RepeatedVertex { element: 0 })
        );
        assert!(matches!(
            compute_surface_faces(&[[-1, 1, 2, 3]]),
            Err(GeometryError::InvalidVertexIndex { index: -1, .. })
        ));
    }
    #[test]
    fn tet_inversion_cpp_fixture_and_checked_failures() {
        let p = [
            [0., 0., 0.],
            [1., 0., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            [0., 0., -1.],
        ];
        let t = [[0, 1, 2, 3], [0, 2, 1, 4]];
        assert_eq!(
            find_inverted_elements(&p, &t, "rightHanded").unwrap(),
            vec![]
        );
        assert_eq!(
            find_inverted_elements(&p, &t, "leftHanded").unwrap(),
            vec![0, 1]
        );
        assert_eq!(
            find_inverted_elements(&p, &[[0, 2, 1, 3]], "rightHanded").unwrap(),
            vec![0]
        );
        assert!(matches!(
            find_inverted_elements(&p, &[[0, 1, 2, 8]], "rightHanded"),
            Err(GeometryError::InvalidVertexIndex { index: 8, .. })
        ));
        assert_eq!(
            find_inverted_elements(&p, &[], "rightHanded"),
            Err(GeometryError::EmptyTetMesh)
        );
        let mut invalid = p;
        invalid[0][0] = f32::NAN;
        assert_eq!(
            find_inverted_elements(&invalid, &t, "rightHanded"),
            Err(GeometryError::NonFinite)
        );
        let flat = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [1., 1., 0.]];
        assert!(
            find_inverted_elements(&flat, &[[0, 1, 2, 3]], "rightHanded")
                .unwrap()
                .is_empty()
        );
    }
    fn curves(kind: &str, basis: &str, wrap: &str, counts: &[i32]) -> BasisCurvesTopology {
        BasisCurvesTopology {
            vertex_counts: counts.into(),
            curve_type: kind.into(),
            basis: basis.into(),
            wrap: wrap.into(),
        }
    }
    #[test]
    fn curve_cpp_fixtures() {
        for (kind, basis, wrap, counts, segments, varying, vertex) in [
            ("linear", "bezier", "periodic", [3, 5], [3, 5], 10, 8),
            ("cubic", "bezier", "nonperiodic", [4, 7], [1, 2], 5, 11),
            ("cubic", "bspline", "pinned", [4, 5], [3, 4], 5, 9),
            ("cubic", "catmullRom", "periodic", [4, 7], [4, 7], 11, 11),
        ] {
            let c = curves(kind, basis, wrap, &counts);
            assert_eq!(c.segment_counts().unwrap(), segments);
            assert_eq!(
                c.data_sizes().unwrap(),
                CurveDataSizes {
                    uniform: 2,
                    varying,
                    vertex
                }
            );
        }
    }
    #[test]
    fn curve_interpolation_precedence_and_small_counts() {
        let sizes = curves("linear", "unused", "nonperiodic", &[2, 2])
            .data_sizes()
            .unwrap();
        assert_eq!(
            sizes.interpolation_for_size(1),
            Some(CurveInterpolation::Constant)
        );
        assert_eq!(
            sizes.interpolation_for_size(2),
            Some(CurveInterpolation::Uniform)
        );
        assert_eq!(
            sizes.interpolation_for_size(4),
            Some(CurveInterpolation::Varying)
        );
        assert_eq!(sizes.interpolation_for_size(5), None);
        assert_eq!(
            curves("cubic", "bspline", "pinned", &[2, 3])
                .data_sizes()
                .unwrap()
                .varying,
            4
        );
        assert!(matches!(
            curves("linear", "bezier", "nonperiodic", &[-1]).data_sizes(),
            Err(GeometryError::NegativeCurveCount {
                curve: 0,
                count: -1
            })
        ));
        assert!(matches!(
            curves("cubic", "unknown", "nonperiodic", &[4]).segment_counts(),
            Err(GeometryError::InvalidCurveToken {
                attribute: "basis",
                ..
            })
        ));
    }
    #[test]
    fn composed_draw_modes_and_constraint_world_transform() {
        use alloc::sync::Arc;
        use layerstack::{
            InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage,
            StageOptions,
        };
        let mut store = InMemoryStore::default();
        let parent = store.path("/Group");
        let model = store.path("/Group/Model");
        let child = store.path("/Group/Model/Child");
        let kind = store.tokens.intern("kind");
        let group = store.tokens.intern("assembly");
        let component = store.tokens.intern("component");
        let draw = store.tokens.intern("model:drawMode");
        let cards = store.tokens.intern("cards");
        let xform_type = store.tokens.intern("Xform");
        let translate = store.tokens.intern("xformOp:translate");
        let order = store.tokens.intern("xformOpOrder");
        let target = store.tokens.intern("constraintTargets:grip");
        let frame = store.tokens.intern("constraintTargets:frame");
        let mut local = crate::gf::IDENTITY;
        local[3][0] = 2.;
        let matrix = crate::value::write_matrix4d(local, &mut store.tokens);
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            parent,
            PrimSpec::def()
                .with_type_name(xform_type)
                .with_field(kind, Value::Token(group))
                .with_property(
                    draw,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "token",
                        false,
                        Value::Token(cards),
                    ))
                    .with_default(Value::Token(cards)),
                ),
        );
        layer.insert_prim(
            model,
            PrimSpec::def()
                .with_type_name(xform_type)
                .with_field(kind, Value::Token(component))
                .with_property(
                    translate,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "double3",
                        false,
                        Value::Vec3d([0.; 3]),
                    ))
                    .with_default(Value::Vec3d([10., 20., 30.])),
                )
                .with_property(
                    order,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "token",
                        true,
                        Value::Token(translate),
                    ))
                    .with_default(Value::array(vec![Value::Token(translate)])),
                )
                .with_property(
                    frame,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "frame4d",
                        false,
                        matrix.clone(),
                    ))
                    .with_default(matrix.clone()),
                )
                .with_property(
                    target,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "matrix4d",
                        false,
                        matrix.clone(),
                    ))
                    .with_default(matrix),
                ),
        );
        layer.insert_prim(
            child,
            PrimSpec::def().with_type_name(xform_type).with_property(
                draw,
                PropertySpec::typed_attribute(PropertyType::new(
                    "token",
                    false,
                    Value::Token(cards),
                ))
                .with_default(Value::Token(store.tokens.intern("bounds"))),
            ),
        );
        store.insert_layer(layer);
        let schemas = crate::openusd(&mut store.tokens);
        let stage = Stage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(Arc::new(schemas)),
                ..StageOptions::default()
            },
        );
        let scene = Scene::new(&stage, &store);
        assert_eq!(scene.compute_model_draw_mode(child, None), "cards");
        assert_eq!(
            scene.compute_model_draw_mode(model, Some("origin")),
            "origin"
        );
        assert_eq!(
            scene.compute_model_draw_mode(parent, Some("origin")),
            "cards"
        );
        assert!(ConstraintTarget::new(&scene, PropertyPath::new(model, frame)).is_some());
        let constraint = ConstraintTarget::new(&scene, PropertyPath::new(model, target)).unwrap();
        let world = constraint
            .compute_in_world_space(Time::Default, None)
            .unwrap();
        assert_eq!(world[3], [12., 20., 30., 1.]);
        let mut cache = XformCache::new(Time::At {
            code: 9.,
            interpolation: layerstack::InterpolationType::Held,
        });
        assert_eq!(
            constraint
                .compute_in_world_space(Time::Default, Some(&mut cache))
                .unwrap(),
            world
        );
        assert_eq!(cache.time(), Time::Default);
        assert!(ConstraintTarget::new(&scene, PropertyPath::new(child, draw)).is_none());
    }
}
