// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Mapping from the mesh description to an authored USDA document.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use layerstack_usda::writer::{Attribute, Document, Metadatum, Prim, Value};

use crate::{
    CustomAttribute, ExportError, Faces, Interpolation, Mesh, MeshProblem, Node, Orientation,
    Primvar, Scene, Transform, UpAxis, Xform,
};

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
    Ok(Document {
        default_prim: Some(scene.root.name.into()),
        metadata: vec![
            Metadatum::new("metersPerUnit", Value::Double(stage.meters_per_unit)),
            Metadatum::new("upAxis", Value::Token(up_axis.into())),
        ],
        prims: vec![xform_prim(&scene.root, "")?],
    })
}

fn xform_prim(xform: &Xform<'_>, parent: &str) -> Result<Prim, ExportError> {
    let path = format!("{parent}/{}", xform.name);
    let mut prim = Prim::def("Xform", xform.name);
    if let Some(kind) = xform.kind {
        prim.metadata
            .push(Metadatum::new("kind", Value::Token(kind.into())));
    }
    push_transform(&mut prim.attributes, xform.transform);
    push_custom(&mut prim.attributes, &xform.attributes);
    for child in &xform.children {
        prim.children.push(match child {
            Node::Xform(x) => xform_prim(x, &path)?,
            Node::Mesh(m) => mesh_prim(m, &path)?,
        });
    }
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

fn mesh_prim(mesh: &Mesh<'_>, parent: &str) -> Result<Prim, ExportError> {
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
    prim.attributes = attrs;
    Ok(prim)
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
