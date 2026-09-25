// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Mapping from the mesh description to an authored USDA document.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use layerstack_usda::writer::{
    Attribute, Document, ListOp, Metadatum, Prim, Property, Relationship, Value,
};

use crate::{
    CustomAttribute, ExportError, Faces, FamilyType, Interpolation, MATERIALS_SCOPE, Material,
    MaterialSubset, Mesh, MeshProblem, Node, Orientation, PROTOTYPES_SCOPE, PointInstancer,
    Primvar, PrimvarData, Scene, Transform, UpAxis, Xform,
};

/// What mesh prims need to know about the scene's materials.
struct Materials<'s, 'a> {
    defined: &'s [Material<'a>],
    /// Prim path of the materials scope.
    scope: String,
}

pub(crate) fn document(scene: &Scene<'_>) -> Result<Document, ExportError> {
    let stage = scene.stage;
    if !(stage.meters_per_unit.is_finite() && stage.meters_per_unit > 0.0) {
        return Err(ExportError::InvalidStage);
    }
    // Stage metadata: `metersPerUnit` and `upAxis` are layer metadata on
    // the root layer (`pxr/usd/usdGeom/metrics.h:23`, `:107`). The root prim is the
    // `defaultPrim` so references to the file need no prim path (AOUSD
    // Core §7.6.1.2.3).
    let up_axis = match stage.up_axis {
        UpAxis::Y => "Y",
        UpAxis::Z => "Z",
    };
    let materials = Materials {
        defined: &scene.materials,
        scope: format!("/{}/{MATERIALS_SCOPE}", scene.root.name),
    };
    // Materials live inside the root prim, so the `defaultPrim` carries
    // them into any referencing stage and bindings never point outside
    // the asset.
    let mut scope = Prim::def("Scope", MATERIALS_SCOPE);
    for material in &scene.materials {
        scope
            .children
            .push(crate::shading::material_prim(material, &materials.scope)?);
    }
    let mut root = xform_prim(&scene.root, "", &materials)?;
    if !scope.children.is_empty() {
        root.children.push(scope);
    }
    Ok(Document {
        default_prim: Some(scene.root.name.into()),
        metadata: vec![
            Metadatum::new("metersPerUnit", Value::Double(stage.meters_per_unit)),
            Metadatum::new("upAxis", Value::Token(up_axis.into())),
        ],
        prim_order: None,
        prims: vec![root],
    })
}

fn xform_prim(
    xform: &Xform<'_>,
    parent: &str,
    materials: &Materials<'_, '_>,
) -> Result<Prim, ExportError> {
    let path = format!("{parent}/{}", xform.name);
    let mut prim = Prim::def("Xform", xform.name);
    if let Some(kind) = xform.kind {
        prim.metadata
            .push(Metadatum::new("kind", Value::Token(kind.into())));
    }
    let mut attrs = Vec::new();
    push_transform(&mut attrs, xform.transform);
    push_custom(&mut attrs, &xform.attributes);
    prim.properties
        .extend(attrs.into_iter().map(Property::Attribute));
    for child in &xform.children {
        prim.children.push(node_prim(child, &path, materials)?);
    }
    Ok(prim)
}

fn node_prim(
    node: &Node<'_>,
    parent: &str,
    materials: &Materials<'_, '_>,
) -> Result<Prim, ExportError> {
    match node {
        Node::Xform(x) => xform_prim(x, parent, materials),
        Node::Mesh(m) => mesh_prim(m, parent, materials),
        Node::PointInstancer(p) => instancer_prim(p, parent, materials),
    }
}

/// A `PointInstancer` prim (`pxr/usd/usdGeom/pointInstancer.h`) with its
/// prototypes in a `Prototypes` scope below it, targeted in order by the
/// `prototypes` relationship.
fn instancer_prim(
    instancer: &PointInstancer<'_>,
    parent: &str,
    materials: &Materials<'_, '_>,
) -> Result<Prim, ExportError> {
    let path = format!("{parent}/{}", instancer.name);
    let checked =
        crate::instancer::check(instancer).map_err(|problem| ExportError::InvalidInstancer {
            path: path.clone(),
            problem,
        })?;
    let scope_path = format!("{path}/{PROTOTYPES_SCOPE}");
    let mut scope = Prim::def("Scope", PROTOTYPES_SCOPE);
    let mut targets = Vec::with_capacity(instancer.prototypes.len());
    for prototype in &instancer.prototypes {
        scope
            .children
            .push(node_prim(prototype, &scope_path, materials)?);
        targets.push(format!("{scope_path}/{}", prototype.name()));
    }

    let mut attrs = Vec::new();
    // Prototype bounds come from the validated meshes built above.
    if let Some([lo, hi]) = crate::instancer::extent(instancer, &checked) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "`extent` is `float3[]`; OpenUSD rounds its double range the same way"
        )]
        let extent = vec![lo.map(|c| c as f32), hi.map(|c| c as f32)];
        attrs.push(Attribute::new(
            "extent",
            "float3[]",
            Value::Float3Array(extent),
        ));
    }
    if let Some(ids) = instancer.ids {
        attrs.push(Attribute::new(
            "ids",
            "int64[]",
            Value::Int64Array(ids.to_vec()),
        ));
    }
    if let Some(orientations) = checked.orientations {
        attrs.push(Attribute::new(
            "orientations",
            "quath[]",
            Value::QuathArray(orientations),
        ));
    }
    attrs.push(Attribute::new(
        "positions",
        "point3f[]",
        Value::Float3Array(instancer.positions.to_vec()),
    ));
    attrs.push(Attribute::new(
        "protoIndices",
        "int[]",
        Value::IntArray(checked.proto_indices),
    ));
    if let Some(scales) = instancer.scales {
        attrs.push(Attribute::new(
            "scales",
            "float3[]",
            Value::Float3Array(scales.to_vec()),
        ));
    }
    push_transform(&mut attrs, instancer.transform);
    push_custom(&mut attrs, &instancer.attributes);

    let mut prim = Prim::def("PointInstancer", instancer.name);
    prim.properties
        .extend(attrs.into_iter().map(Property::Attribute));
    // `prototypes` is an ordered target list: a prototype's position in it
    // is the value `protoIndices` uses for it.
    prim.push_property(Relationship {
        name: "prototypes".into(),
        custom: false,
        targets: Some(ListOp::explicit(targets)),
        metadata: Vec::new(),
    });
    prim.children.push(scope);
    Ok(prim)
}

/// Element counts for each interpolation on one mesh.
#[derive(Clone, Copy)]
struct Sites {
    points: usize,
    faces: usize,
    corners: usize,
}

impl Sites {
    /// Spec: `pxr/usd/usdGeom/mesh.h:75` — constant: 1; uniform: one
    /// per face; varying/vertex: one per point; faceVarying: one per face
    /// corner (<https://openusd.org/dev/api/class_usd_geom_primvar.html>).
    fn count(self, interpolation: Interpolation) -> usize {
        match interpolation {
            Interpolation::Constant => 1,
            Interpolation::Uniform => self.faces,
            Interpolation::Varying | Interpolation::Vertex => self.points,
            Interpolation::FaceVarying => self.corners,
        }
    }
}

fn mesh_prim(
    mesh: &Mesh<'_>,
    parent: &str,
    materials: &Materials<'_, '_>,
) -> Result<Prim, ExportError> {
    let path = format!("{parent}/{}", mesh.name);
    let fail = |problem| ExportError::InvalidMesh {
        path: path.clone(),
        problem,
    };

    let (counts, indices) = topology(mesh.faces, mesh.points.len()).map_err(fail)?;
    let sites = Sites {
        points: mesh.points.len(),
        faces: counts.len(),
        corners: indices.len(),
    };

    let mut attrs = Vec::new();
    // `extent` is required on boundable prims for correct culling and
    // bounds; it is the local-space AABB of `points`, before this prim's
    // own transform (`pxr/usd/usdGeom/boundable.h:42`). Empty meshes have
    // no extent.
    if let Some(extent) = extent(mesh.points).map_err(fail)? {
        attrs.push(Attribute::new(
            "extent",
            "float3[]",
            Value::Float3Array(extent.to_vec()),
        ));
    }
    attrs.push(Attribute::new(
        "faceVertexCounts",
        "int[]",
        Value::IntArray(counts),
    ));
    attrs.push(Attribute::new(
        "faceVertexIndices",
        "int[]",
        Value::IntArray(indices),
    ));

    if let Some(normals) = mesh.normals {
        let values = Value::Float3Array(normals.values.to_vec());
        match normals.indices {
            // `normals` is a plain attribute with an `interpolation`, not a
            // primvar, so it cannot be indexed (`pxr/usd/usdGeom/pointBased.h`).
            None => {
                check_primvar("normals", normals.values.len(), &normals, sites).map_err(fail)?;
                attrs.push(
                    Attribute::new("normals", "normal3f[]", values).with_metadata(
                        "interpolation",
                        Value::Token(normals.interpolation.token().into()),
                    ),
                );
            }
            // Indexed normals go to `primvars:normals`, which takes
            // precedence over `normals` (`pxr/usd/usdGeom/pointBased.h:204`).
            Some(_) => push_primvar(
                &mut attrs,
                "primvars:normals",
                "normal3f[]",
                values,
                normals.values.len(),
                &normals,
                sites,
            )
            .map_err(fail)?,
        }
    }

    // `orientation` is authored even when it equals the `rightHanded`
    // fallback, so the winding convention is explicit in the file
    // (`pxr/usd/usdGeom/gprim.h:212`).
    let orientation = match mesh.orientation {
        Orientation::RightHanded => "rightHanded",
        Orientation::LeftHanded => "leftHanded",
    };
    attrs.push(Attribute::new("orientation", "token", Value::Token(orientation.into())).uniform());
    if mesh.double_sided {
        attrs.push(Attribute::new("doubleSided", "bool", Value::Bool(true)).uniform());
    }
    attrs.push(Attribute::new(
        "points",
        "point3f[]",
        Value::Float3Array(mesh.points.to_vec()),
    ));

    if let Some(uvs) = mesh.uvs {
        // The conventional primary UV set is the `st` primvar of role
        // `texCoord2f` (`pxr/usd/usdGeom/primvar.h`).
        push_primvar(
            &mut attrs,
            "primvars:st",
            "texCoord2f[]",
            Value::Float2Array(uvs.values.to_vec()),
            uvs.values.len(),
            &uvs,
            sites,
        )
        .map_err(fail)?;
    }
    for custom in &mesh.primvars {
        let data = custom.primvar.values;
        push_primvar(
            &mut attrs,
            &format!("primvars:{}", custom.name),
            data.type_name(),
            data.to_value(),
            data.len(),
            &custom.primvar,
            sites,
        )
        .map_err(fail)?;
    }

    // The schema fallback is `catmullClark`; polygonal kernel output must
    // opt out explicitly or consumers will smooth it (`pxr/usd/usdGeom/mesh.h:62`).
    attrs.push(Attribute::new("subdivisionScheme", "token", Value::Token("none".into())).uniform());
    push_transform(&mut attrs, mesh.transform);
    push_custom(&mut attrs, &mesh.attributes);

    let mut prim = Prim::def("Mesh", mesh.name);
    prim.properties
        .extend(attrs.into_iter().map(Property::Attribute));
    let binding = mesh
        .material
        .map(|name| material_binding(mesh, &path, &path, name, materials))
        .transpose()?;
    if !mesh.material_subsets.is_empty() {
        check_family(&mesh.material_subsets, mesh.subset_family, sites.faces).map_err(fail)?;
        // `UsdShadeMaterialBindingAPI::SetMaterialBindSubsetsFamilyType`
        // (materialBindingAPI.h:934) authors this attribute on the mesh.
        prim.push_property(
            Attribute::new(
                "subsetFamily:materialBind:familyType",
                "token",
                Value::Token(mesh.subset_family.token().into()),
            )
            .uniform(),
        );
        for subset in &mesh.material_subsets {
            let subset_path = format!("{path}/{}", subset.name);
            let binding = material_binding(mesh, &path, &subset_path, subset.material, materials)?;
            prim.children
                .push(subset_prim(subset, binding).map_err(fail)?);
        }
    }
    // Relationships follow the attributes.
    if let Some(binding) = binding {
        apply_binding(&mut prim, binding);
    }
    Ok(prim)
}

/// Checks that `subsets` form a valid `family` over `faces` faces, as
/// `UsdGeomSubset::ValidateFamily` does (`pxr/usd/usdGeom/subset.cpp`):
/// indices in range, no index twice (both restricted types), and full
/// coverage for a partition.
fn check_family(
    subsets: &[MaterialSubset<'_>],
    family: FamilyType,
    faces: usize,
) -> Result<(), MeshProblem> {
    let mut covered = vec![false; faces];
    for subset in subsets {
        for &face in subset.faces {
            let Some(seen) = covered.get_mut(face as usize) else {
                return Err(MeshProblem::SubsetFaceOutOfRange {
                    subset: subset.name.into(),
                    face,
                    faces,
                });
            };
            if core::mem::replace(seen, true) {
                return Err(MeshProblem::OverlappingSubsets {
                    subset: subset.name.into(),
                    face,
                });
            }
        }
    }
    if family == FamilyType::Partition
        && let Some(face) = covered.iter().position(|c| !c)
    {
        return Err(MeshProblem::IncompletePartition { face });
    }
    Ok(())
}

/// A `GeomSubset` of faces in the `materialBind` family, with its own
/// direct binding (`pxr/usd/usdGeom/subset.h`: `elementType` and
/// `familyName` are uniform tokens, `indices` an `int[]`).
fn subset_prim(subset: &MaterialSubset<'_>, material: String) -> Result<Prim, MeshProblem> {
    let indices = subset
        .faces
        .iter()
        .map(|&face| to_int(face))
        .collect::<Result<Vec<_>, _>>()?;
    let mut prim = Prim::def("GeomSubset", subset.name);
    prim.push_property(
        Attribute::new("elementType", "token", Value::Token("face".into())).uniform(),
    );
    prim.push_property(
        Attribute::new("familyName", "token", Value::Token("materialBind".into())).uniform(),
    );
    prim.push_property(Attribute::new("indices", "int[]", Value::IntArray(indices)));
    apply_binding(&mut prim, material);
    Ok(prim)
}

/// Resolves the binding on `binding_path` (the mesh or one of its
/// subsets) by material name, checking that the mesh authors every UV set
/// the material's textures read. Returns the material path.
fn material_binding(
    mesh: &Mesh<'_>,
    mesh_path: &str,
    binding_path: &str,
    name: &str,
    materials: &Materials<'_, '_>,
) -> Result<String, ExportError> {
    let material = materials
        .defined
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| ExportError::UnknownMaterial {
            path: binding_path.into(),
            material: name.into(),
        })?;
    let has_uv_set = |uv_set: &str| {
        (uv_set == "st" && mesh.uvs.is_some())
            || mesh.primvars.iter().any(|p| {
                p.name == uv_set
                    && matches!(
                        p.primvar.values,
                        PrimvarData::TexCoord2(_) | PrimvarData::Float2(_)
                    )
            })
    };
    if let Some(texture) = material.textures().find(|t| !has_uv_set(t.uv_set)) {
        return Err(ExportError::InvalidMesh {
            path: mesh_path.into(),
            problem: MeshProblem::MissingTexCoords {
                material: name.into(),
                uv_set: texture.uv_set.into(),
            },
        });
    }
    Ok(format!("{}/{name}", materials.scope))
}

/// A direct, all-purpose binding at the default strength: the
/// `MaterialBindingAPI` applied through `prepend apiSchemas`, and the
/// `material:binding` relationship targeting the material
/// (`pxr/usd/usdShade/materialBindingAPI.h:82`). `usdShadeValidators`'
/// `MaterialBindingApiAppliedValidator` rejects the relationship without
/// the applied schema.
fn apply_binding(prim: &mut Prim, material: String) {
    prim.metadata.push(Metadatum::new(
        "apiSchemas",
        Value::TokenListOp(ListOp::prepend(vec![String::from("MaterialBindingAPI")])),
    ));
    prim.push_property(Relationship::new("material:binding", material));
}

/// Converts topology to USD's `int[]` counts and indices, checking counts,
/// index ranges and the 32-bit signed limit, as `UsdGeomMesh::ValidateTopology`
/// does (`pxr/usd/usdGeom/mesh.h:575`).
fn topology(faces: Faces<'_>, points: usize) -> Result<(Vec<i32>, Vec<i32>), MeshProblem> {
    let (counts, raw) = match faces {
        Faces::Triangles(indices) => {
            if indices.len() % 3 != 0 {
                return Err(MeshProblem::PartialTriangle { len: indices.len() });
            }
            (vec![3; indices.len() / 3], indices)
        }
        Faces::Polygons { counts, indices } => {
            let mut out = Vec::with_capacity(counts.len());
            let mut total = 0_usize;
            for (face, &count) in counts.iter().enumerate() {
                if count < 3 {
                    return Err(MeshProblem::DegenerateFace { face, count });
                }
                total = total.saturating_add(count as usize);
                out.push(to_int(count)?);
            }
            if total != indices.len() {
                return Err(MeshProblem::CornerCountMismatch {
                    expected: total,
                    actual: indices.len(),
                });
            }
            (out, indices)
        }
    };
    let mut indices = Vec::with_capacity(raw.len());
    for (corner, &index) in raw.iter().enumerate() {
        if index as usize >= points {
            return Err(MeshProblem::PointIndexOutOfRange {
                corner,
                index,
                points,
            });
        }
        indices.push(to_int(index)?);
    }
    Ok((counts, indices))
}

fn to_int(v: u32) -> Result<i32, MeshProblem> {
    i32::try_from(v).map_err(|_| MeshProblem::TooLarge)
}

fn extent(points: &[[f32; 3]]) -> Result<Option<[[f32; 3]; 2]>, MeshProblem> {
    let mut bounds: Option<[[f32; 3]; 2]> = None;
    for (point, p) in points.iter().enumerate() {
        if !p.iter().all(|c| c.is_finite()) {
            return Err(MeshProblem::NonFinitePoint { point });
        }
        let [lo, hi] = bounds.get_or_insert([*p, *p]);
        for axis in 0..3 {
            lo[axis] = lo[axis].min(p[axis]);
            hi[axis] = hi[axis].max(p[axis]);
        }
    }
    Ok(bounds)
}

/// Checks that a primvar's values (or indices, when indexed) match its
/// interpolation's element count and that indices are in range.
fn check_primvar<V>(
    name: &str,
    value_count: usize,
    primvar: &Primvar<'_, V>,
    sites: Sites,
) -> Result<(), MeshProblem> {
    let expected = sites.count(primvar.interpolation);
    let Some(indices) = primvar.indices else {
        if value_count != expected {
            return Err(MeshProblem::PrimvarLength {
                name: name.into(),
                expected,
                actual: value_count,
            });
        }
        return Ok(());
    };
    if indices.len() != expected {
        return Err(MeshProblem::PrimvarLength {
            name: format!("{name}:indices"),
            expected,
            actual: indices.len(),
        });
    }
    if let Some(&index) = indices.iter().find(|&&i| i as usize >= value_count) {
        return Err(MeshProblem::PrimvarIndexOutOfRange {
            name: name.into(),
            index,
            values: value_count,
        });
    }
    Ok(())
}

/// Writes `name` with its `interpolation` metadata and, when indexed, the
/// companion `name:indices` `int[]` attribute (`pxr/usd/usdGeom/primvar.h:497`).
/// Indices are copied verbatim: for face-varying data they define the
/// primvar's topology, so they must not be deduplicated by value.
fn push_primvar<V>(
    attrs: &mut Vec<Attribute>,
    name: &str,
    type_name: &str,
    values: Value,
    value_count: usize,
    primvar: &Primvar<'_, V>,
    sites: Sites,
) -> Result<(), MeshProblem> {
    check_primvar(name, value_count, primvar, sites)?;
    attrs.push(Attribute::new(name, type_name, values).with_metadata(
        "interpolation",
        Value::Token(primvar.interpolation.token().into()),
    ));
    if let Some(indices) = primvar.indices {
        let indices = indices
            .iter()
            .map(|&i| to_int(i))
            .collect::<Result<Vec<_>, _>>()?;
        attrs.push(Attribute::new(
            format!("{name}:indices"),
            "int[]",
            Value::IntArray(indices),
        ));
    }
    Ok(())
}

/// A single `xformOp:transform` op, listed in the `uniform token[]
/// xformOpOrder` (`pxr/usd/usdGeom/xformable.h:136`). Nothing is written without a
/// transform, which composes as identity.
fn push_transform(attrs: &mut Vec<Attribute>, transform: Option<Transform>) {
    let Some(transform) = transform else {
        return;
    };
    attrs.push(Attribute::new(
        "xformOp:transform",
        "matrix4d",
        Value::Matrix4d(transform.usd_rows()),
    ));
    attrs.push(
        Attribute::new(
            "xformOpOrder",
            "token[]",
            Value::TokenArray(vec![String::from("xformOp:transform")]),
        )
        .uniform(),
    );
}

fn push_custom(attrs: &mut Vec<Attribute>, custom: &[CustomAttribute<'_>]) {
    for attribute in custom {
        attrs.push(
            Attribute::new(
                attribute.name,
                attribute.value.canonical_type_name(),
                attribute.value.clone(),
            )
            .custom(),
        );
    }
}
