// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::string::ToString;
use alloc::vec;

use crate::{
    ExportError, Faces, InstancerProblem, Material, Mesh, PointInstancer, Scene, StageSettings,
    Transform, UpAxis, UsdzProfile, Value, Xform,
};

const TRI_POINTS: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
const TRI: Faces<'static> = Faces::Triangles(&[0, 1, 2]);
const QUAD_POINTS: [[f32; 3]; 4] = [
    [-0.5, -0.5, 0.0],
    [0.5, -0.5, 0.0],
    [0.5, 0.5, 0.0],
    [-0.5, 0.5, 0.0],
];
const QUAD: Faces<'static> = Faces::Polygons {
    counts: &[4],
    indices: &[0, 1, 2, 3],
};

/// A 90° turn about +Z, `[x, y, z, w]`.
const QUARTER_TURN_Z: [f32; 4] = [0.0, 0.0, core::f32::consts::FRAC_1_SQRT_2, 0.707_106_77];

fn scene(instancer: PointInstancer<'_>) -> Scene<'_> {
    Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_point_instancer(instancer),
    )
    .with_material(Material::new("Bark"))
}

fn problem(instancer: PointInstancer<'_>) -> InstancerProblem {
    match scene(instancer).to_document() {
        Err(ExportError::InvalidInstancer { path, problem }) => {
            assert_eq!(path, "/Root/Field", "error path");
            problem
        }
        other => panic!("expected an instancer error, got {other:?}"),
    }
}

/// The authored `extent` as `[min x, min y, min z, max x, max y, max z]`.
fn extent(text: &str) -> vec::Vec<f64> {
    let line = text.lines().find(|l| l.contains("extent =")).unwrap();
    line.split_once('=')
        .unwrap()
        .1
        .split(['[', ']', '(', ')', ',', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| w.parse().unwrap())
        .collect()
}

fn assert_near(got: &[f64], want: &[f64], tolerance: f64) {
    assert_eq!(got.len(), want.len(), "{got:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() <= tolerance, "{got:?} vs {want:?}");
    }
}

fn tri() -> Mesh<'static> {
    Mesh::new("Tri", &TRI_POINTS, TRI)
}

#[test]
fn golden_instancer() {
    let indices = [0, 1, 0];
    let positions = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 5.0, 1.0]];
    let orientations = [[0.0, 0.0, 0.0, 1.0], QUARTER_TURN_Z, [0.0, 0.0, 0.0, 1.0]];
    let scales = [[1.0; 3], [1.0; 3], [2.0, 2.0, 1.0]];
    let ids = [7, 3, -1];
    let instancer = PointInstancer::new("Field", &indices, &positions)
        .with_prototype(tri().with_material("Bark"))
        .with_prototype(
            Xform::new("Pair")
                .with_transform(Transform::from_translation([0.0, 0.0, 2.0]))
                .with_mesh(Mesh::new("Quad", &QUAD_POINTS, QUAD)),
        )
        .with_orientations(&orientations)
        .with_scales(&scales)
        .with_ids(&ids)
        .with_attribute("exedra:set", Value::String("stones".into()));
    let text = scene(instancer).to_usda().unwrap();
    // Extent: the triangle at the origin and scaled ×2 at (0, 5, 1); the
    // quad, lifted by its prototype's transform to z = 2, turned 90° about
    // Z at (10, 0, 0). The half-precision quarter turn is slightly short of
    // unit length, so the quad's corners move by a rounding error.
    assert_near(&extent(&text), &[0.0, -0.5, 0.0, 10.5, 7.0, 2.0], 1e-3);
    let expected = r#"def PointInstancer "Field"
    {
        float3[] extent = "#;
    assert!(text.contains(expected), "{text}");
    let expected = r#"
        int64[] ids = [7, 3, -1]
        quath[] orientations = [(1, 0, 0, 0), (0.70703125, 0, 0, 0.70703125), (1, 0, 0, 0)]
        point3f[] positions = [(0, 0, 0), (10, 0, 0), (0, 5, 1)]
        int[] protoIndices = [0, 1, 0]
        float3[] scales = [(1, 1, 1), (1, 1, 1), (2, 2, 1)]
        custom string exedra:set = "stones"
        rel prototypes = [</Root/Field/Prototypes/Tri>, </Root/Field/Prototypes/Pair>]

        def Scope "Prototypes"
        {
            def Mesh "Tri" (
                prepend apiSchemas = ["MaterialBindingAPI"]
            )
            {"#;
    assert!(text.contains(expected), "{text}");
    assert!(
        text.contains("rel material:binding = </Root/Materials/Bark>"),
        "the prototype keeps its binding"
    );
    assert!(
        text.contains(
            "def Xform \"Pair\"\n            {\n                matrix4d xformOp:transform"
        ),
        "the prototype keeps its transform: {text}"
    );
}

#[test]
fn optional_arrays_are_omitted_and_usdc_carries_the_instancer() {
    let indices = [0, 0];
    let positions = [[0.0; 3], [3.0, 0.0, 0.0]];
    let instancer = PointInstancer::new("Field", &indices, &positions).with_prototype(tri());
    let scene = scene(instancer);
    let text = scene.to_usda().unwrap();
    for absent in [
        "orientations",
        "scales",
        "ids",
        "velocities",
        "invisibleIds",
    ] {
        assert!(!text.contains(absent), "{absent} is not authored");
    }
    assert!(
        text.contains("float3[] extent = [(0, 0, 0), (4, 1, 0)]"),
        "{text}"
    );
    assert!(text.contains("rel prototypes = </Root/Field/Prototypes/Tri>"));
    assert_eq!(&scene.to_usdc().unwrap()[..8], b"PXR-USDC");
    let arkit = scene.to_usdz(UsdzProfile::Arkit, &[]).unwrap();
    assert_eq!(&arkit[..4], b"PK\x03\x04");
}

#[test]
fn extent_follows_rotation_scale_and_the_instancer_transform() {
    // The instancer's own transform is not part of its extent.
    let indices = [0];
    let positions = [[1.0, 2.0, 3.0]];
    let orientations = [QUARTER_TURN_Z];
    let scales = [[2.0, 3.0, 1.0]];
    let instancer = PointInstancer::new("Field", &indices, &positions)
        .with_prototype(tri())
        .with_orientations(&orientations)
        .with_scales(&scales)
        .with_transform(Transform::from_translation([100.0, 0.0, 0.0]));
    let text = scene(instancer).to_usda().unwrap();
    // Scaled: x in [0, 2], y in [0, 3]; turned: x in [-3, 0], y in [0, 2];
    // moved by (1, 2, 3). The half-precision quaternion is not exactly a
    // quarter turn, so the bounds are off by a rounding error.
    assert_near(&extent(&text), &[-2.0, 2.0, 3.0, 1.0, 4.0, 3.0], 2e-3);
}

#[test]
fn rejects_inconsistent_instancers() {
    let one = [0];
    let two = [0, 0];
    let p1 = [[0.0; 3]];
    let p2 = [[0.0; 3]; 2];
    let base = || PointInstancer::new("Field", &one, &p1).with_prototype(tri());

    assert_eq!(
        problem(PointInstancer::new("Field", &one, &p1)),
        InstancerProblem::NoPrototypes
    );
    assert_eq!(
        problem(PointInstancer::new("Field", &[0, 1], &p2).with_prototype(tri())),
        InstancerProblem::ProtoIndexOutOfRange {
            instance: 1,
            index: 1,
            prototypes: 1
        }
    );
    assert_eq!(
        problem(PointInstancer::new("Field", &two, &p1).with_prototype(tri())),
        InstancerProblem::LengthMismatch {
            name: "positions",
            expected: 2,
            actual: 1
        }
    );
    let q2 = [[0.0, 0.0, 0.0, 1.0]; 2];
    assert_eq!(
        problem(base().with_orientations(&q2)),
        InstancerProblem::LengthMismatch {
            name: "orientations",
            expected: 1,
            actual: 2
        }
    );
    assert_eq!(
        problem(base().with_scales(&p2)),
        InstancerProblem::LengthMismatch {
            name: "scales",
            expected: 1,
            actual: 2
        }
    );
    assert_eq!(
        problem(base().with_ids(&[])),
        InstancerProblem::LengthMismatch {
            name: "ids",
            expected: 1,
            actual: 0
        }
    );
    assert_eq!(
        problem(
            PointInstancer::new("Field", &[0, 0, 0], &[[0.0; 3]; 3])
                .with_prototype(tri())
                .with_ids(&[5, 9, 5])
        ),
        InstancerProblem::DuplicateId {
            id: 5,
            first: 0,
            second: 2
        }
    );
}

#[test]
fn rejects_non_finite_and_non_unit_values() {
    let one = [0];
    let origin = [[0.0; 3]];
    let base = || PointInstancer::new("Field", &one, &origin).with_prototype(tri());
    let nan_position = [[0.0, f32::NAN, 0.0]];
    assert_eq!(
        problem(PointInstancer::new("Field", &one, &nan_position).with_prototype(tri())),
        InstancerProblem::NonFinite {
            name: "positions",
            instance: 0
        }
    );
    let inf_scale = [[1.0, 1.0, f32::INFINITY]];
    assert_eq!(
        problem(base().with_scales(&inf_scale)),
        InstancerProblem::NonFinite {
            name: "scales",
            instance: 0
        }
    );
    let nan_quat = [[0.0, 0.0, 0.0, f32::NAN]];
    assert_eq!(
        problem(base().with_orientations(&nan_quat)),
        InstancerProblem::NonFinite {
            name: "orientations",
            instance: 0
        }
    );
    for bad in [[0.0; 4], [0.0, 0.0, 0.0, 2.0], [0.5, 0.5, 0.5, 0.6]] {
        let quats = [bad];
        assert_eq!(
            problem(base().with_orientations(&quats)),
            InstancerProblem::NonUnitOrientation { instance: 0 },
            "{bad:?}"
        );
    }
}

#[test]
fn rejects_motion_masking_and_schema_attributes() {
    let one = [0];
    let origin = [[0.0; 3]];
    let with = |name| {
        PointInstancer::new("Field", &one, &origin)
            .with_prototype(tri())
            .with_attribute(name, Value::Float3Array(vec![[0.0; 3]]))
    };
    for name in [
        "velocities",
        "accelerations",
        "angularVelocities",
        "invisibleIds",
    ] {
        let problem = problem(with(name));
        assert_eq!(
            problem,
            InstancerProblem::UnsupportedProperty { name: name.into() }
        );
        assert!(problem.to_string().contains("static"), "{problem}");
    }
    for name in [
        "prototypes",
        "protoIndices",
        "ids",
        "positions",
        "orientations",
        "orientationsf",
        "scales",
        "extent",
        "xformOpOrder",
        "xformOp:translate",
    ] {
        assert_eq!(
            problem(with(name)),
            InstancerProblem::ReservedProperty { name: name.into() }
        );
    }
}

#[test]
fn prototypes_are_checked_like_any_mesh() {
    let one = [0];
    let origin = [[0.0; 3]];
    let instancer =
        PointInstancer::new("Field", &one, &origin).with_prototype(tri().with_material("Missing"));
    assert!(matches!(
        scene(instancer).to_document(),
        Err(ExportError::UnknownMaterial { path, .. }) if path == "/Root/Field/Prototypes/Tri"
    ));
    let instancer = PointInstancer::new("Field", &one, &origin)
        .with_prototype(tri().with_attribute("exedra:tex", Value::Asset("t.png".into())));
    assert_eq!(
        scene(instancer).to_usdz(UsdzProfile::Generic, &[]),
        Err(ExportError::UnpackagedAsset {
            asset: "t.png".into()
        }),
        "prototype asset paths must be packaged"
    );
    let instancer = PointInstancer::new("Field", &one, &origin)
        .with_prototype(tri())
        .with_prototype(tri());
    assert!(
        matches!(scene(instancer).to_usda(), Err(ExportError::Usda(_))),
        "prototype names are sibling prim names"
    );
}

#[test]
fn nested_instancers_bound_their_placements() {
    let inner_indices = [0, 0];
    let inner_positions = [[0.0; 3], [0.0, 4.0, 0.0]];
    let inner =
        PointInstancer::new("Clump", &inner_indices, &inner_positions).with_prototype(tri());
    let outer_indices = [0, 0];
    let outer_positions = [[0.0; 3], [20.0, 0.0, 0.0]];
    let outer =
        PointInstancer::new("Field", &outer_indices, &outer_positions).with_prototype(inner);
    let text = scene(outer).to_usda().unwrap();
    assert!(
        text.contains("float3[] extent = [(0, 0, 0), (21, 5, 0)]"),
        "{text}"
    );
    assert!(text.contains("rel prototypes = </Root/Field/Prototypes/Clump/Prototypes/Tri>"));
}
