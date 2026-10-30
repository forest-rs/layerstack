// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reading a mesh or an implicit shape off a composed stage.

use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::{PathId, PrimPathArg, Time, Value};
use layerstack_schemas::primvar::Primvar as StagePrimvar;
use layerstack_schemas::subset::MeshTopology as StageTopology;
use layerstack_schemas::usd_geom::{
    Capsule, Capsule1, Cone, Cube, Cylinder, Cylinder1, GeomSubsetElementType, Imageable, Mesh,
    Plane, Sphere,
};
use layerstack_schemas::value::{read_float2_array_shared, read_float3_array_shared};
use layerstack_schemas::{PrimView, Scene};

use crate::input::{
    FaceSubsets, Interpolation, MeshInput, MeshTopology, Orientation, Primvar, SubdivisionScheme,
};
use crate::shapes::{Axis, Shape};

/// A primvar read off the stage: shared values and indices.
#[derive(Clone, Debug)]
struct Owned<T> {
    values: Arc<Vec<T>>,
    indices: Option<Arc<Vec<i32>>>,
    interpolation: Interpolation,
    element_size: u32,
}

impl<T> Owned<T> {
    fn primvar(&self) -> Primvar<'_, T> {
        Primvar {
            values: &self.values,
            indices: self.indices.as_deref().map(Vec::as_slice),
            interpolation: self.interpolation,
            element_size: self.element_size,
        }
    }
}

/// A `Mesh` read off a composed stage at one time: its topology (with
/// layerstack's validation of it), points, normals, `st`, `displayColor`
/// and `materialBind` face subsets, holding the stage's shared arrays.
/// [`Self::input`] lends them to a [`crate::MeshPlan`].
///
/// What it reads (OpenUSD `UsdGeomMesh`, `UsdGeomPrimvarsAPI`,
/// `UsdGeomSubset::GetGeomSubsets`):
///
/// - normals: the `primvars:normals` primvar when it has a value, else the
///   `normals` attribute with its `interpolation` metadata (`vertex` when
///   unauthored), as `UsdGeomPointBased` reads them;
/// - `st`: the `primvars:st` primvar; colors: `primvars:displayColor`, its
///   own or a constant one inherited from an ancestor;
/// - subsets: the active, defined `GeomSubset` children of family
///   `materialBind` and element type `face`, in namespace order.
///
/// A primvar whose values are not of the expected type (`float3[]` kinds
/// for normals and colors, `float2[]` kinds for `st`), or fail to decode,
/// is left out and named in [`Self::unread`].
#[derive(Clone, Debug)]
pub struct StageMesh {
    topology: StageTopology,
    points: Arc<Vec<[f32; 3]>>,
    normals: Option<Owned<[f32; 3]>>,
    st: Option<Owned<[f32; 2]>>,
    display_color: Option<Owned<[f32; 3]>>,
    subset_paths: Vec<PathId>,
    subset_faces: Vec<i32>,
    subset_ends: Vec<u32>,
    unread: Vec<&'static str>,
}

impl StageMesh {
    /// The mesh at `path` at `time`; `None` when there is no `Mesh` there.
    #[must_use]
    pub fn read(scene: &Scene<'_>, path: impl PrimPathArg, time: Time) -> Option<Self> {
        let mesh = Mesh::get(scene, path)?;
        let prim = scene.prim_path(mesh.path())?;
        let topology = mesh.topology(time);
        let points = mesh.points_at(time).unwrap_or_default();
        let mut unread = Vec::new();
        let normals = match primvar(
            scene,
            prim,
            "normals",
            false,
            time,
            read_float3_array_shared,
        ) {
            Read::Value(normals) => Some(normals),
            Read::Unread => {
                unread.push("primvars:normals");
                None
            }
            Read::Absent => mesh.normals_at(time).map(|values| Owned {
                values,
                indices: None,
                interpolation: mesh
                    .property_metadata("normals")
                    .and_then(|metadata| metadata.interpolation())
                    .and_then(Interpolation::from_token)
                    .unwrap_or(Interpolation::Vertex),
                element_size: 1,
            }),
        };
        let st = primvar(scene, prim, "st", false, time, read_float2_array_shared)
            .keep("primvars:st", &mut unread);
        let display_color = primvar(
            scene,
            prim,
            "displayColor",
            true,
            time,
            read_float3_array_shared,
        )
        .keep("primvars:displayColor", &mut unread);
        let mut subset_paths = Vec::new();
        let mut subset_faces = Vec::new();
        let mut subset_ends = Vec::new();
        if let Some(imageable) = Imageable::get(scene, prim) {
            for subset in
                imageable.geom_subsets(Some(&GeomSubsetElementType::Face), Some("materialBind"))
            {
                subset_paths.push(subset.path());
                subset_faces.extend_from_slice(&subset.indices_at(time).unwrap_or_default());
                subset_ends.push(u32::try_from(subset_faces.len()).unwrap_or(u32::MAX));
            }
        }
        Some(Self {
            topology,
            points,
            normals,
            st,
            display_color,
            subset_paths,
            subset_faces,
            subset_ends,
            unread,
        })
    }

    /// The topology as layerstack read and validated it
    /// (`layerstack_schemas::usd_geom::Mesh::topology`).
    #[must_use]
    pub fn topology(&self) -> &StageTopology {
        &self.topology
    }

    /// The `materialBind` face subsets' paths, in the order of
    /// [`MeshInput::subsets`] (and so of the draw ranges).
    #[must_use]
    pub fn subset_paths(&self) -> &[PathId] {
        &self.subset_paths
    }

    /// The primvars left out because their values are not of the drawn
    /// type or fail to decode (`primvars:normals`, `primvars:st`,
    /// `primvars:displayColor`).
    #[must_use]
    pub fn unread(&self) -> &[&'static str] {
        &self.unread
    }

    /// Whether `other` holds the same points and primvars, value for
    /// value (the same shared arrays, or equal ones).
    pub(crate) fn same_values(&self, other: &Self) -> bool {
        same(&self.points, &other.points) && self.same_primvars(other) && {
            let (a, b) = (self.input(), other.input());
            same_primvar(a.normals.as_ref(), b.normals.as_ref())
        }
    }

    /// Whether `other` holds the same `st` and colors, value for value:
    /// then new points and normals alone refill the buffers.
    pub(crate) fn same_primvars(&self, other: &Self) -> bool {
        let (a, b) = (self.input(), other.input());
        same_primvar(a.st.as_ref(), b.st.as_ref())
            && same_primvar(a.display_color.as_ref(), b.display_color.as_ref())
    }

    /// The mesh as a plan's input, borrowing the arrays.
    #[must_use]
    pub fn input(&self) -> MeshInput<'_> {
        MeshInput {
            topology: MeshTopology {
                face_vertex_counts: &self.topology.face_vertex_counts,
                face_vertex_indices: &self.topology.face_vertex_indices,
                hole_indices: &self.topology.hole_indices,
                orientation: Orientation::from_token(self.topology.orientation.as_str()),
                subdivision_scheme: SubdivisionScheme::from_token(
                    self.topology.subdivision_scheme.as_str(),
                ),
            },
            points: &self.points,
            normals: self.normals.as_ref().map(Owned::primvar),
            st: self.st.as_ref().map(Owned::primvar),
            display_color: self.display_color.as_ref().map(Owned::primvar),
            subsets: FaceSubsets {
                faces: &self.subset_faces,
                ends: &self.subset_ends,
            },
        }
    }
}

/// Whether two shared arrays hold the same values: the same allocation,
/// or equal elements.
fn same<T: PartialEq>(a: &Arc<Vec<T>>, b: &Arc<Vec<T>>) -> bool {
    Arc::ptr_eq(a, b) || a == b
}

/// Whether two primvars are laid out alike and hold the same values.
fn same_primvar<T: PartialEq>(a: Option<&Primvar<'_, T>>, b: Option<&Primvar<'_, T>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.interpolation == b.interpolation
                && a.element_size == b.element_size
                && a.indices == b.indices
                && (core::ptr::eq(a.values, b.values) || a.values == b.values)
        }
        _ => false,
    }
}

/// What reading one primvar found.
enum Read<T> {
    /// No primvar, or no value.
    Absent,
    /// A value of another type, or one that fails to decode.
    Unread,
    /// Its value.
    Value(Owned<T>),
}

impl<T> Read<T> {
    fn keep(self, name: &'static str, unread: &mut Vec<&'static str>) -> Option<Owned<T>> {
        match self {
            Self::Absent => None,
            Self::Unread => {
                unread.push(name);
                None
            }
            Self::Value(value) => Some(value),
        }
    }
}

/// The primvar `name` of `prim` at `time`, its values read by `read`; with
/// `inherited`, a constant one of an ancestor when the prim has none
/// (`UsdGeomPrimvarsAPI::FindPrimvarWithInheritance`).
fn primvar<T>(
    scene: &Scene<'_>,
    prim: PathId,
    name: &str,
    inherited: bool,
    time: Time,
    read: impl Fn(&Value, &layerstack::TokenInterner) -> Option<Arc<Vec<T>>>,
) -> Read<T> {
    let found = if inherited {
        PrimView::new(*scene, prim).find_primvar_with_inheritance(name)
    } else {
        StagePrimvar::new(scene, prim, name)
    };
    let Some(primvar) = found else {
        return Read::Absent;
    };
    let value = match primvar.value(time) {
        Ok(Some(value)) => value,
        Ok(None) => return Read::Absent,
        Err(_) => return Read::Unread,
    };
    let Some(values) = read(&value, scene.store().tokens()) else {
        return Read::Unread;
    };
    let Some(interpolation) = Interpolation::from_token(primvar.interpolation()) else {
        return Read::Unread;
    };
    Read::Value(Owned {
        values,
        indices: primvar.indices(time),
        interpolation,
        element_size: u32::try_from(primvar.element_size()).unwrap_or(0),
    })
}

/// The implicit shape at `path` at `time` (`Cube`, `Sphere`, `Cylinder`,
/// `Cone`, `Capsule`, `Plane`, and the `_1` revisions `Cylinder_1` and
/// `Capsule_1` with `radiusTop` and `radiusBottom`), with its size
/// attributes, schema fallbacks included; `None` for any other prim.
#[must_use]
pub fn read_shape(scene: &Scene<'_>, path: impl PrimPathArg, time: Time) -> Option<Shape> {
    let path = scene.prim_path(path)?;
    let f = |value: Option<f64>, fallback: f32| value.map_or(fallback, narrow);
    let axis = |token: Option<&str>| token.and_then(Axis::from_token).unwrap_or_default();
    if let Some(cube) = Cube::get(scene, path) {
        return Some(Shape::Cube {
            size: f(cube.size_at(time), 2.0),
        });
    }
    if let Some(sphere) = Sphere::get(scene, path) {
        return Some(Shape::Sphere {
            radius: f(sphere.radius_at(time), 1.0),
        });
    }
    if let Some(cylinder) = Cylinder::get(scene, path) {
        return Some(Shape::Cylinder {
            radius: f(cylinder.radius_at(time), 1.0),
            height: f(cylinder.height_at(time), 2.0),
            axis: axis(cylinder.axis().as_ref().map(|a| a.as_str())),
        });
    }
    if let Some(cone) = Cone::get(scene, path) {
        return Some(Shape::Cone {
            radius: f(cone.radius_at(time), 1.0),
            height: f(cone.height_at(time), 2.0),
            axis: axis(cone.axis().as_ref().map(|a| a.as_str())),
        });
    }
    if let Some(capsule) = Capsule::get(scene, path) {
        return Some(Shape::Capsule {
            radius: f(capsule.radius_at(time), 0.5),
            height: f(capsule.height_at(time), 1.0),
            axis: axis(capsule.axis().as_ref().map(|a| a.as_str())),
        });
    }
    if let Some(cylinder) = Cylinder1::get(scene, path) {
        return Some(Shape::Cylinder1 {
            radius_top: f(cylinder.radius_top_at(time), 1.0),
            radius_bottom: f(cylinder.radius_bottom_at(time), 1.0),
            height: f(cylinder.height_at(time), 2.0),
            axis: axis(cylinder.axis().as_ref().map(|a| a.as_str())),
        });
    }
    if let Some(capsule) = Capsule1::get(scene, path) {
        return Some(Shape::Capsule1 {
            radius_top: f(capsule.radius_top_at(time), 0.5),
            radius_bottom: f(capsule.radius_bottom_at(time), 0.5),
            height: f(capsule.height_at(time), 1.0),
            axis: axis(capsule.axis().as_ref().map(|a| a.as_str())),
        });
    }
    if let Some(plane) = Plane::get(scene, path) {
        return Some(Shape::Plane {
            width: f(plane.width_at(time), 2.0),
            length: f(plane.length_at(time), 2.0),
            axis: axis(plane.axis().as_ref().map(|a| a.as_str())),
        });
    }
    None
}

/// A size attribute's `double` as the `f32` the buffers hold.
#[expect(
    clippy::cast_possible_truncation,
    reason = "sizes are drawn in single precision, as the points are"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use layerstack::{InMemoryStore, LayerId, LiveStage, NoAssets, StageOptions, Time};
    use layerstack_schemas::Scene;

    use super::{StageMesh, read_shape};
    use crate::{
        Axis, ChannelState, Interpolation, MeshOptions, MeshPlan, NormalsSource, Orientation,
        Shape, SubdivisionScheme,
    };

    const SCENE: &str = r#"#usda 1.0
def Mesh "Rock" (
    prepend apiSchemas = ["MaterialBindingAPI"]
) {
    int[] faceVertexCounts = [4, 3]
    int[] faceVertexIndices = [0, 1, 2, 3, 1, 4, 2]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0), (2, 0, 0)]
    normal3f[] normals = [(0, 0, 1), (0, 0, 1)] (
        interpolation = "uniform"
    )
    texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
        interpolation = "faceVarying"
    )
    int[] primvars:st:indices = [0, 1, 2, 3, 1, 0, 2]
    color3f[] primvars:displayColor = [(0.5, 0.25, 0.125)]
    uniform token orientation = "leftHanded"
    uniform token subdivisionScheme = "none"
    def GeomSubset "Top" {
        uniform token elementType = "face"
        uniform token familyName = "materialBind"
        int[] indices = [1]
    }
}
def Capsule "Lamp" {
    double radius = 0.25
    uniform token axis = "Y"
}
def Plane "Floor" {
}
def Cylinder_1 "Post" {
    double radiusTop = 0.25
    uniform token axis = "Y"
}
def Capsule_1 "Beacon" {
    double radiusBottom = 0.75
}
"#;

    fn stage() -> (InMemoryStore, LiveStage) {
        let mut store = InMemoryStore::default();
        let read = layerstack_usda::read_usda(
            SCENE,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
        );
        assert!(!read.has_errors());
        store.insert_layer(read.emitted.layer);
        let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
        let live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(schemas),
                ..StageOptions::default()
            },
        );
        (store, live)
    }

    #[test]
    fn a_mesh_is_read_with_its_primvars_and_subsets() {
        let (store, live) = stage();
        let scene = Scene::new(live.stage(), &store);
        let mesh = StageMesh::read(&scene, "/Rock", Time::Default).unwrap();
        assert!(mesh.topology().validation.is_ok());
        assert!(mesh.unread().is_empty());
        assert_eq!(mesh.subset_paths().len(), 1);
        let input = mesh.input();
        assert_eq!(input.topology.orientation, Orientation::LeftHanded);
        assert_eq!(input.topology.subdivision_scheme, SubdivisionScheme::None);
        assert_eq!(input.normals.unwrap().interpolation, Interpolation::Uniform);
        assert_eq!(input.st.unwrap().indices, Some(&[0, 1, 2, 3, 1, 0, 2][..]));
        assert_eq!(
            input.display_color.unwrap().interpolation,
            Interpolation::Constant
        );
        let plan = MeshPlan::new(&input, &MeshOptions::default());
        let report = plan.report();
        assert_eq!(report.triangles, 3);
        assert_eq!(report.normals, NormalsSource::Authored);
        assert_eq!(
            report.st,
            ChannelState::Used {
                interpolation: Interpolation::FaceVarying,
                indexed: true
            }
        );
        let buffers = plan.buffers(&input).unwrap();
        // The subset's face first, then the rest.
        assert_eq!(buffers.triangle_faces, [1, 0, 0]);
        assert_eq!(buffers.colors.unwrap()[0], [0.5, 0.25, 0.125]);
        assert!(StageMesh::read(&scene, "/Lamp", Time::Default).is_none());
    }

    #[test]
    fn implicit_shapes_are_read_with_their_fallbacks() {
        let (store, live) = stage();
        let scene = Scene::new(live.stage(), &store);
        assert_eq!(
            read_shape(&scene, "/Lamp", Time::Default),
            Some(Shape::Capsule {
                radius: 0.25,
                height: 1.0,
                axis: Axis::Y
            })
        );
        assert_eq!(
            read_shape(&scene, "/Floor", Time::Default),
            Some(Shape::Plane {
                width: 2.0,
                length: 2.0,
                axis: Axis::Z
            })
        );
        assert_eq!(
            read_shape(&scene, "/Post", Time::Default),
            Some(Shape::Cylinder1 {
                radius_top: 0.25,
                radius_bottom: 1.0,
                height: 2.0,
                axis: Axis::Y
            })
        );
        assert_eq!(
            read_shape(&scene, "/Beacon", Time::Default),
            Some(Shape::Capsule1 {
                radius_top: 0.5,
                radius_bottom: 0.75,
                height: 1.0,
                axis: Axis::Z
            })
        );
        assert_eq!(read_shape(&scene, "/Rock", Time::Default), None);
    }
}
