// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::types::*;
use crate::{
    BindingCache, GeneratedMesh, MeshSites, PrimView, Scene, XformCache,
    point_instancer::InstanceTransformOptions,
    usd_geom::{Imageable, ImageablePurpose, Mesh, PointInstancer},
    validate_mesh_primvar_cardinality,
};
use alloc::{collections::BTreeSet, string::ToString, sync::Arc, vec::Vec};
use layerstack::{Path, PathId, TargetPath, Time};

pub(super) fn paths(
    scene: &Scene<'_>,
    work: &mut SceneWork,
) -> Result<Vec<(PathId, SceneKind)>, SceneError> {
    let mut records = Vec::new();
    let mut namespace = Vec::new();
    if let Some(root) = scene.store().paths().lookup(&Path::root()) {
        for &child in scene.stage().children_of(root).unwrap_or_default() {
            namespace.extend(scene.stage().traverse(child));
        }
    }
    // Default USD traversal stops at native instance roots. Prototype inventories
    // address their populated descendants; query every occurrence for inheritance.
    // AOUSD Core §11.3.3; OpenUSD UsdPrim::GetPrimInPrototype.
    for prototype in scene.stage().prototypes() {
        for &instance in prototype.instances() {
            for member in prototype.prims() {
                let path = scene
                    .store()
                    .paths()
                    .resolve(instance)
                    .join(member.relative_path());
                namespace.extend(scene.store().paths().lookup(&path));
            }
        }
    }
    let mut seen = BTreeSet::new();
    for path in namespace {
        if !seen.insert(path) {
            continue;
        }
        work.namespace_prims += 1;
        if !scene.stage().is_active(path)
            || !scene.stage().is_defined(path, scene.store())
            || scene.stage().is_abstract(path, scene.store())
        {
            continue;
        }
        let kind = if scene.is_a(path, "Mesh") {
            Some(SceneKind::Mesh)
        } else if scene.is_a(path, "PointInstancer") {
            Some(SceneKind::PointInstancer)
        } else if scene.is_a(path, "Gprim") {
            return Err(SceneError::UnsupportedGeometry {
                prim: path,
                type_name: scene
                    .stage()
                    .resolve_type_name(path, scene.store())
                    .map(|t| scene.store().tokens().resolve(t))
                    .unwrap_or("")
                    .into(),
            });
        } else if scene.stage().is_instance(path) {
            Some(SceneKind::NativeInstance)
        } else {
            None
        };
        if let Some(kind) = kind {
            records.push((path, kind));
        }
    }
    Ok(records)
}

fn missing(prim: &PrimView<'_>, name: &str) -> SceneError {
    prim.property_path(name).map_or_else(
        || SceneError::MissingProperty {
            prim: prim.path(),
            name: name.into(),
        },
        SceneError::MissingAttribute,
    )
}
fn decode(prim: &PrimView<'_>, name: &str, source: layerstack::ArrayReadError) -> SceneError {
    prim.property_path(name).map_or_else(
        || missing(prim, name),
        |property| SceneError::Decode { property, source },
    )
}
fn checked<T>(
    prim: &PrimView<'_>,
    name: &str,
    value: Result<Option<T>, layerstack::ArrayReadError>,
) -> Result<T, SceneError> {
    value
        .map_err(|source| decode(prim, name, source))?
        .ok_or_else(|| missing(prim, name))
}

pub(super) fn mesh(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
    previous: Option<Arc<SceneMesh>>,
    work: &mut SceneWork,
) -> Result<Arc<SceneMesh>, SceneError> {
    work.mesh_inputs += 1;
    let mesh = Mesh::new(scene, path).expect("inventory is a mesh");
    let points = checked(&mesh, "points", mesh.try_points(time))?;
    let counts = checked(&mesh, "faceVertexCounts", mesh.try_face_vertex_counts(time))?;
    let indices = checked(
        &mesh,
        "faceVertexIndices",
        mesh.try_face_vertex_indices(time),
    )?;
    let holes = checked(&mesh, "holeIndices", mesh.try_hole_indices(time))?;
    let normals = mesh
        .try_normals(time)
        .map_err(|source| decode(&mesh, "normals", source))?;
    let normals_interpolation = mesh
        .property_metadata("normals")
        .and_then(|m| m.interpolation())
        .unwrap_or("vertex")
        .to_string();
    let mut controls = Vec::new();
    // Retain subdivision control inputs, without claiming to evaluate subdivision.
    // OpenUSD UsdGeomMesh; AOUSD Core §12.3–12.5 (typed value resolution).
    for name in [
        "orientation",
        "doubleSided",
        "subdivisionScheme",
        "interpolateBoundary",
        "faceVaryingLinearInterpolation",
        "triangleSubdivisionRule",
        "cornerIndices",
        "cornerSharpnesses",
        "creaseIndices",
        "creaseLengths",
        "creaseSharpnesses",
    ] {
        if let Some(value) = mesh
            .try_read_value(name, time, |value, _| Some(value.clone()))
            .map_err(|source| decode(&mesh, name, source))?
        {
            controls.push((name.into(), value));
        }
    }
    if let Some(previous) = previous {
        let data = previous.polygons.mesh();
        if same_points(&points, &data.points, &mut work.mesh_comparison_entries)
            && same_ints(
                &counts,
                &data.face_vertex_counts,
                &mut work.mesh_comparison_entries,
            )
            && same_ints(
                &indices,
                &data.face_vertex_indices,
                &mut work.mesh_comparison_entries,
            )
            && same_ints(&holes, &previous.holes, &mut work.mesh_comparison_entries)
            && match (&normals, &previous.normals) {
                (None, None) => true,
                (Some(a), Some(b)) => same_points(a, b, &mut work.mesh_comparison_entries),
                _ => false,
            }
            && normals_interpolation == previous.normals_interpolation
            && controls.len() == previous.controls.len()
            && controls
                .iter()
                .zip(&previous.controls)
                .all(|((a, av), (b, bv))| a == b && av.same_representation(bv))
        {
            return Ok(previous);
        }
    }
    let polygons = GeneratedMesh {
        points,
        face_vertex_counts: counts,
        face_vertex_indices: indices,
        primvars: Vec::new(),
    }
    .into_validated()
    .map_err(|source| SceneError::Mesh { prim: path, source })?;
    let data = polygons.mesh();
    for &index in holes.iter() {
        if usize::try_from(index).map_or(true, |i| i >= data.face_vertex_counts.len()) {
            return Err(SceneError::Hole { prim: path, index });
        }
    }
    if let Some(normals) = &normals {
        validate_mesh_primvar_cardinality(
            normals.len(),
            1,
            &normals_interpolation,
            sites(data),
            None,
        )
        .map_err(|source| SceneError::Mesh { prim: path, source })?;
    }
    work.meshes += 1;
    let validation = polygons.validation_work();
    work.mesh_validation.points += validation.points;
    work.mesh_validation.faces += validation.faces;
    work.mesh_validation.corners += validation.corners;
    work.mesh_validation.extent_points += validation.extent_points;
    Ok(Arc::new(SceneMesh {
        polygons,
        holes,
        normals,
        normals_interpolation,
        controls,
    }))
}

fn same_points(a: &Arc<Vec<[f32; 3]>>, b: &Arc<Vec<[f32; 3]>>, compared: &mut usize) -> bool {
    if Arc::ptr_eq(a, b) {
        return true;
    }
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(a, b)| {
        *compared += 1;
        a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
    })
}
fn same_ints(a: &Arc<Vec<i32>>, b: &Arc<Vec<i32>>, compared: &mut usize) -> bool {
    if Arc::ptr_eq(a, b) {
        return true;
    }
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(a, b)| {
        *compared += 1;
        a == b
    })
}
pub(super) fn sites(mesh: &GeneratedMesh) -> MeshSites {
    MeshSites {
        points: mesh.points.len(),
        faces: mesh.face_vertex_counts.len(),
        corners: mesh.face_vertex_indices.len(),
    }
}
pub(super) fn primvars(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
    sites: MeshSites,
    work: &mut SceneWork,
) -> Result<(Vec<ScenePrimvar>, bool), SceneError> {
    let prim = PrimView::new(*scene, path);
    let mut output = Vec::new();
    let mut has_id_targets = false;
    // Inheritance stays in occurrence namespace, outside shared prototype geometry.
    // OpenUSD UsdGeomPrimvarsAPI::FindPrimvarsWithInheritance; AOUSD Core §12.
    for primvar in prim.primvars_with_inheritance() {
        has_id_targets |= primvar.is_id_target();
        let Some(values) =
            primvar
                .validated_values(time)
                .map_err(|source| SceneError::Primvar {
                    prim: path,
                    name: primvar.name().into(),
                    source,
                })?
        else {
            continue;
        };
        let width = usize::try_from(primvar.element_size())
            .ok()
            .filter(|v| *v > 0)
            .ok_or_else(|| SceneError::Primvar {
                prim: path,
                name: primvar.name().into(),
                source: crate::primvar::PrimvarError::InvalidElementSize(primvar.element_size()),
            })?;
        let interpolation = primvar.interpolation();
        let count = values.values().array_ref().map_or(1, |array| array.len());
        validate_mesh_primvar_cardinality(
            count,
            width,
            interpolation,
            sites,
            values.indices().map(|i| i.len()),
        )
        .map_err(|source| SceneError::Mesh { prim: path, source })?;
        output.push(ScenePrimvar {
            name: primvar.name().into(),
            source: primvar.property(),
            interpolation: interpolation.into(),
            element_size: width,
            value: values.values().clone(),
            indices: values.indices().cloned(),
            unauthored_values_index: primvar.unauthored_values_index(),
        });
        work.primvars += 1;
    }
    Ok((output, has_id_targets))
}
pub(super) fn appearance(scene: &Scene<'_>, path: PathId, time: Time) -> SceneAppearance {
    Imageable::new(scene, path).map_or_else(
        || SceneAppearance {
            visibility: crate::Visibility::Inherited,
            purpose: ImageablePurpose::Default,
            effective_visibility: crate::Visibility::Visible,
        },
        |prim| {
            let purpose = prim.compute_purpose();
            SceneAppearance {
                visibility: prim.compute_visibility(time),
                effective_visibility: prim.compute_effective_visibility(&purpose, time),
                purpose,
            }
        },
    )
}
pub(super) fn transform(
    scene: &Scene<'_>,
    path: PathId,
    cache: &mut XformCache,
) -> Result<SceneTransform, SceneError> {
    let local = cache
        .local_transform(scene, path)
        .cloned()
        .ok_or(SceneError::Transform(path))?;
    let local_to_world = cache
        .local_to_world(scene, path)
        .ok_or(SceneError::Transform(path))?;
    Ok(SceneTransform {
        local,
        local_to_world,
    })
}
pub(super) fn materials(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
    is_mesh: bool,
    cache: &mut BindingCache,
    work: &mut SceneWork,
) -> Result<SceneMaterials, SceneError> {
    work.material_assignments += 1;
    let binding = cache.compute_bound_material(scene, path);
    let subsets = if is_mesh {
        Some(
            cache
                .material_binding_subsets(scene, path, time)
                .map_err(|source| SceneError::Materials { prim: path, source })?,
        )
    } else {
        None
    };
    Ok(SceneMaterials { binding, subsets })
}
pub(super) fn point_instances(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
    work: &mut SceneWork,
) -> Result<ScenePointInstances, SceneError> {
    let prim = PointInstancer::new(scene, path).expect("inventory is a point instancer");
    let error = |source| SceneError::PointInstancer { prim: path, source };
    let transforms = prim
        .prepare_instance_transforms(
            time,
            time,
            InstanceTransformOptions {
                include_prototype_transform: true,
                apply_mask: false,
            },
        )
        .map_err(error)?;
    let mask = Arc::new(prim.try_compute_mask(time).map_err(error)?);
    let prototypes = prim
        .prototypes()
        .into_iter()
        .map(|target| match target {
            TargetPath::Prim(path) => Ok(path),
            _ => Err(error(
                crate::point_instancer::PointInstancerError::InvalidPrototypes,
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    work.point_instancers += 1;
    work.point_instances += transforms.source_len();
    Ok(ScenePointInstances {
        transforms,
        prototypes,
        mask,
    })
}
