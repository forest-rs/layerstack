// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::string::String;
use alloc::vec::Vec;

use layerstack_usda::ast;
use layerstack_usda::parser::parse;

use crate::{
    ExportError, Faces, Interpolation, Mesh, MeshProblem, Primvar, PrimvarData, Scene,
    StageSettings, Transform, UpAxis, UsdzProfile, Value, Xform,
};

const TRI_POINTS: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 1.0, -0.5]];

fn tri_scene(mesh: Mesh<'_>) -> Scene<'_> {
    Scene::new(
        StageSettings::new(UpAxis::Z, 0.001),
        Xform::new("Root").with_kind("component").with_mesh(mesh),
    )
}

#[test]
fn golden_triangle() {
    let normals = [[0.0, 0.0, 1.0]];
    let st = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
    let colors = [[1.0, 0.5, 0.25]];
    let mesh = Mesh::new("Tri", &TRI_POINTS, Faces::triangles(&[0, 1, 2]))
        .with_normals(Primvar::uniform(&normals))
        .with_uvs(Primvar::face_varying(&st))
        .with_primvar(
            "displayColor",
            Primvar::constant(PrimvarData::color3(&colors)),
        )
        .with_transform(Transform::from_translation([1.0, 2.0, 3.0]))
        .with_attribute("exedra:path", Value::String("part/tri".into()));
    let text = tri_scene(mesh).to_usda().unwrap();
    let expected = r#"#usda 1.0
(
    defaultPrim = "Root"
    metersPerUnit = 0.001
    upAxis = "Z"
)

def Xform "Root" (
    kind = "component"
)
{
    def Mesh "Tri"
    {
        float3[] extent = [(0, 0, -0.5), (2, 1, 0)]
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        normal3f[] normals = [(0, 0, 1)] (
            interpolation = "uniform"
        )
        uniform token orientation = "rightHanded"
        point3f[] points = [(0, 0, 0), (2, 0, 0), (0, 1, -0.5)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (0, 1)] (
            interpolation = "faceVarying"
        )
        color3f[] primvars:displayColor = [(1, 0.5, 0.25)] (
            interpolation = "constant"
        )
        uniform token subdivisionScheme = "none"
        matrix4d xformOp:transform = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (1, 2, 3, 1) )
        uniform token[] xformOpOrder = ["xformOp:transform"]
        custom string exedra:path = "part/tri"
    }
}
"#;
    assert_eq!(text, expected, "USDA output");
    assert!(
        parse(&text).diagnostics.is_empty(),
        "output re-parses cleanly"
    );
}

#[test]
fn custom_attributes_must_be_attribute_types() {
    let mesh = Mesh::new("Tri", &TRI_POINTS, Faces::triangles(&[0, 1, 2])).with_attribute(
        "exedra:data",
        Value::Dictionary(alloc::vec![("k".into(), Value::Int(1))]),
    );
    assert_eq!(
        tri_scene(mesh).to_usda(),
        Err(ExportError::Usda(
            layerstack_usda::writer::WriteError::UnknownType {
                path: "/Root/Tri.exedra:data".into(),
                type_name: "dictionary".into()
            }
        )),
        "dictionaries are metadata, not attribute values"
    );
}

#[test]
fn indexed_normals_become_a_primvar() {
    let normals = [[0.0, 0.0, 1.0]];
    let mesh = Mesh::new("Tri", &TRI_POINTS, Faces::triangles(&[0, 1, 2]))
        .with_normals(Primvar::face_varying(&normals[..]).with_indices(&[0, 0, 0]));
    let text = tri_scene(mesh).to_usda().unwrap();
    let parsed = parse(&text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let ast::PrimChild::Prim(mesh) = &parsed.layer.prims[0].children[0] else {
        panic!("mesh prim");
    };
    let names: Vec<&str> = mesh
        .children
        .iter()
        .filter_map(|c| match c {
            ast::PrimChild::Attribute(a) => Some(a.name),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"primvars:normals"), "{names:?}");
    assert!(names.contains(&"primvars:normals:indices"), "{names:?}");
    assert!(
        !names.contains(&"normals"),
        "unindexable attribute not used"
    );
}

#[test]
fn polygon_topology_and_custom_primvars() {
    // A quad and a triangle sharing an edge.
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [2.0, 0.5, 0.0],
    ];
    let region = [7_u32, 9];
    let mesh = Mesh::new(
        "Poly",
        &points,
        Faces::polygons(&[4, 3], &[0, 1, 2, 3, 1, 4, 2]),
    )
    .with_primvar(
        "exedra:region",
        Primvar::uniform(PrimvarData::uint(&region)),
    );
    let text = tri_scene(mesh).to_usda().unwrap();
    assert!(text.contains("int[] faceVertexCounts = [4, 3]"), "{text}");
    assert!(
        text.contains(
            "uint[] primvars:exedra:region = [7, 9] (\n            interpolation = \"uniform\""
        ),
        "{text}"
    );
}

fn problem(result: Result<String, ExportError>) -> MeshProblem {
    match result {
        Err(ExportError::InvalidMesh { path, problem }) => {
            assert_eq!(path, "/Root/Tri", "error path");
            problem
        }
        other => panic!("expected InvalidMesh, got {other:?}"),
    }
}

#[test]
fn rejects_inconsistent_meshes() {
    let tri = |faces| Mesh::new("Tri", &TRI_POINTS, faces);
    assert_eq!(
        problem(tri_scene(tri(Faces::triangles(&[0, 1]))).to_usda()),
        MeshProblem::PartialTriangle { len: 2 },
        "partial triangle"
    );
    assert_eq!(
        problem(tri_scene(tri(Faces::triangles(&[0, 1, 3]))).to_usda()),
        MeshProblem::PointIndexOutOfRange {
            corner: 2,
            index: 3,
            points: 3
        },
        "index range"
    );
    assert_eq!(
        problem(tri_scene(tri(Faces::polygons(&[2], &[0, 1]))).to_usda()),
        MeshProblem::DegenerateFace { face: 0, count: 2 },
        "degenerate face"
    );
    assert_eq!(
        problem(tri_scene(tri(Faces::polygons(&[3], &[0, 1, 2, 0]))).to_usda()),
        MeshProblem::CornerCountMismatch {
            expected: 3,
            actual: 4
        },
        "corner count"
    );

    let st = [[0.0, 0.0]; 2];
    let short = tri(Faces::triangles(&[0, 1, 2])).with_uvs(Primvar::vertex(&st));
    assert_eq!(
        problem(tri_scene(short).to_usda()),
        MeshProblem::PrimvarLength {
            name: "primvars:st".into(),
            expected: 3,
            actual: 2
        },
        "primvar length"
    );
    let bad_index = tri(Faces::triangles(&[0, 1, 2]))
        .with_uvs(Primvar::new(&st[..], Interpolation::FaceVarying).with_indices(&[0, 1, 2]));
    assert_eq!(
        problem(tri_scene(bad_index).to_usda()),
        MeshProblem::PrimvarIndexOutOfRange {
            name: "primvars:st".into(),
            index: 2,
            values: 2
        },
        "primvar index range"
    );

    let nan = [[0.0, f32::NAN, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mesh = Mesh::new("Tri", &nan, Faces::triangles(&[0, 1, 2]));
    assert_eq!(
        problem(tri_scene(mesh).to_usda()),
        MeshProblem::NonFinitePoint { point: 0 },
        "non-finite point"
    );

    let mut scene = tri_scene(tri(Faces::triangles(&[0, 1, 2])));
    scene.stage.meters_per_unit = 0.0;
    assert_eq!(
        scene.to_usda(),
        Err(ExportError::InvalidStage),
        "stage units"
    );

    let bad_name = tri_scene(Mesh::new(
        "no-dash",
        &TRI_POINTS,
        Faces::triangles(&[0, 1, 2]),
    ));
    assert!(
        matches!(bad_name.to_usda(), Err(ExportError::Usda(_))),
        "names are validated by the writer"
    );
}

#[test]
fn usdz_requires_authored_assets_to_be_packaged() {
    let png = b"png";
    let mesh = Mesh::new("Tri", &TRI_POINTS, Faces::triangles(&[0, 1, 2]))
        .with_attribute("exedra:albedo", Value::Asset("./textures/a.png".into()));
    let scene = tri_scene(mesh);
    for profile in [UsdzProfile::Generic, UsdzProfile::Arkit] {
        assert_eq!(
            scene.to_usdz(profile, &[]),
            Err(ExportError::UnpackagedAsset {
                asset: "./textures/a.png".into()
            }),
            "missing texture ({profile:?})"
        );
        let bytes = scene
            .to_usdz(profile, &[crate::PackageFile::new("textures/a.png", png)])
            .expect("texture packaged");
        let archive = layerstack_usdz::zip::ZipArchive::parse(&bytes).unwrap();
        let names: Vec<&str> = archive.entries().iter().map(|e| &*e.name).collect();
        assert_eq!(
            names,
            [profile.root_layer_path(), "textures/a.png"],
            "entries ({profile:?})"
        );
    }

    assert_eq!(
        scene.to_usdz(
            UsdzProfile::Generic,
            &[
                crate::PackageFile::new("textures/a.png", png),
                crate::PackageFile::new("data.json", b"{}"),
            ]
        ),
        Err(ExportError::Usdz(
            layerstack_usdz::UsdzWriteError::UnsupportedMemberType {
                path: "data.json".into()
            }
        )),
        "packages hold only USD, image and audio members"
    );
}

#[test]
fn profiles_choose_the_root_layer_format() {
    let scene = tri_scene(Mesh::new("Tri", &TRI_POINTS, Faces::triangles(&[0, 1, 2])));
    let root_layer = |profile| {
        let bytes = scene.to_usdz(profile, &[]).unwrap();
        let archive = layerstack_usdz::zip::ZipArchive::parse(&bytes).unwrap();
        archive.entry_data(&archive.entries()[0]).to_vec()
    };
    assert_eq!(
        root_layer(UsdzProfile::Generic),
        scene.to_usda().unwrap().into_bytes(),
        "generic: USDA text"
    );
    assert_eq!(
        root_layer(UsdzProfile::Arkit),
        scene.to_usdc().unwrap(),
        "ARKit: the USDC layer"
    );
    assert_eq!(UsdzProfile::Generic.root_layer_path(), "scene.usda", "name");
    assert_eq!(UsdzProfile::Arkit.root_layer_path(), "scene.usdc", "name");

    let png = b"png";
    for (path, allowed) in [
        ("textures/a.png", true),
        ("textures/a.jpg", true),
        ("audio/a.wav", true),
        ("textures/a.exr", false),
        ("layers/extra.usda", false),
        ("layers/extra.usdc", false),
    ] {
        let result = scene.to_usdz(UsdzProfile::Arkit, &[crate::PackageFile::new(path, png)]);
        if allowed {
            assert!(result.is_ok(), "{path} allowed: {result:?}");
        } else {
            assert_eq!(
                result,
                Err(ExportError::ProfileMember {
                    path: path.into(),
                    profile: UsdzProfile::Arkit
                }),
                "{path} excluded"
            );
            assert!(
                scene
                    .to_usdz(UsdzProfile::Generic, &[crate::PackageFile::new(path, png)])
                    .is_ok(),
                "{path} allowed in the generic profile"
            );
        }
    }
}

/// A scene assembled at runtime by a function can own every name and
/// buffer it creates, so it outlives the data it was built from.
fn owned_scene(parts: u16) -> Scene<'static> {
    let mut root = Xform::new(alloc::format!("Site{parts}"));
    let mut materials = Vec::new();
    for part in 0..parts {
        let lift = f32::from(part);
        let points: Vec<[f32; 3]> = TRI_POINTS
            .iter()
            .map(|p| [p[0], p[1], p[2] + lift])
            .collect();
        let tint = alloc::vec![[lift / 4.0, 0.5, 0.25]];
        let material = alloc::format!("Paint{part}");
        root = root.with_mesh(
            Mesh::new(
                crate::sanitize_name(&alloc::format!("part.{part}")),
                points,
                Faces::triangles(alloc::vec![0, 1, 2]),
            )
            .with_primvar("displayColor", Primvar::constant(PrimvarData::color3(tint)))
            .with_material(material.clone()),
        );
        materials.push(crate::Material::new(material));
    }
    let mut scene = Scene::new(StageSettings::new(UpAxis::Z, 1.0), root);
    scene.materials = materials;
    scene
}

#[test]
fn scenes_can_own_their_names_and_buffers() {
    let text = owned_scene(2).to_usda().unwrap();
    assert!(text.contains("def Xform \"Site2\""), "{text}");
    assert!(text.contains("def Mesh \"part_1\""), "{text}");
    assert!(
        text.contains("point3f[] points = [(0, 0, 1), (2, 0, 1), (0, 1, 0.5)]"),
        "{text}"
    );
    assert!(text.contains("rel material:binding = </Site2/Materials/Paint1>"));
}
