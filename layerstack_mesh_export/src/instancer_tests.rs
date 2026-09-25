// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::borrow::Cow;
use alloc::string::ToString;
use alloc::vec;

use crate::{
    ExportError, Faces, InstancerProblem, Material, Mesh, OrientationPrecision, PointInstancer,
    Primvar, PrimvarData, Scene, StageSettings, Transform, UpAxis, UsdzProfile, Value, Xform,
};

const TRI_POINTS: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
const TRI: Faces<'static> = Faces::Triangles(Cow::Borrowed(&[0, 1, 2]));
const QUAD_POINTS: [[f32; 3]; 4] = [
    [-0.5, -0.5, 0.0],
    [0.5, -0.5, 0.0],
    [0.5, 0.5, 0.0],
    [-0.5, 0.5, 0.0],
];
const QUAD: Faces<'static> = Faces::Polygons {
    counts: Cow::Borrowed(&[4]),
    indices: Cow::Borrowed(&[0, 1, 2, 3]),
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
        .with_orientation_precision(OrientationPrecision::FloatAndHalf)
        .with_attribute("site:set", Value::String("stones".into()));
    let text = scene(instancer).to_usda().unwrap();
    // Extent: the triangle at the origin and scaled ×2 at (0, 5, 1); the
    // quad, lifted by its prototype's transform to z = 2, turned 90° about
    // Z at (10, 0, 0). The extent follows `orientationsf`, which readers
    // prefer when both are authored.
    assert_near(&extent(&text), &[0.0, -0.5, 0.0, 10.5, 7.0, 2.0], 1e-6);
    let expected = r#"def PointInstancer "Field"
    {
        float3[] extent = "#;
    assert!(text.contains(expected), "{text}");
    let expected = r#"
        int64[] ids = [7, 3, -1]
        quath[] orientations = [(1, 0, 0, 0), (0.70703125, 0, 0, 0.70703125), (1, 0, 0, 0)]
        quatf[] orientationsf = [(1, 0, 0, 0), (0.70710677, 0, 0, 0.70710677), (1, 0, 0, 0)]
        point3f[] positions = [(0, 0, 0), (10, 0, 0), (0, 5, 1)]
        int[] protoIndices = [0, 1, 0]
        float3[] scales = [(1, 1, 1), (1, 1, 1), (2, 2, 1)]
        custom string site:set = "stones"
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
    let text = scene(instancer.clone()).to_usda().unwrap();
    // Scaled: x in [0, 2], y in [0, 3]; turned: x in [-3, 0], y in [0, 2];
    // moved by (1, 2, 3).
    let want = [-2.0, 2.0, 3.0, 1.0, 4.0, 3.0];
    assert_near(&extent(&text), &want, 1e-6);
    // Stored at half precision, the quaternion is not exactly a quarter
    // turn, and the extent follows the rotation readers will apply.
    let half = instancer.with_orientation_precision(OrientationPrecision::Half);
    let got = extent(&scene(half).to_usda().unwrap());
    assert_near(&got, &want, 2e-3);
    assert!(
        got.iter().zip(&want).any(|(g, w)| (g - w).abs() > 1e-5),
        "{got:?}"
    );
}

#[test]
fn orientation_precision_picks_the_authored_attributes() {
    let one = [0];
    let origin = [[0.0; 3]];
    let turned = [QUARTER_TURN_Z];
    let base = PointInstancer::new("Field", &one, &origin)
        .with_prototype(tri())
        .with_orientations(&turned);
    for (precision, float, half) in [
        (OrientationPrecision::default(), true, false),
        (OrientationPrecision::Float, true, false),
        (OrientationPrecision::Half, false, true),
        (OrientationPrecision::FloatAndHalf, true, true),
    ] {
        let text = scene(base.clone().with_orientation_precision(precision))
            .to_usda()
            .unwrap();
        assert_eq!(
            text.contains("quatf[] orientationsf = [(0.70710677, 0, 0, 0.70710677)]"),
            float,
            "{precision:?}: {text}"
        );
        assert_eq!(
            text.contains("quath[] orientations = [(0.70703125, 0, 0, 0.70703125)]"),
            half,
            "{precision:?}: {text}"
        );
    }
}

#[test]
fn half_orientation_error_bounds_the_rounding() {
    let one = [0];
    let origin = [[0.0; 3]];
    let identity = [[0.0, 0.0, 0.0, 1.0]];
    let unturned = PointInstancer::new("Field", &one, &origin).with_orientations(&identity);
    assert_eq!(unturned.half_orientation_error(), 0.0, "identity is exact");
    assert_eq!(
        PointInstancer::new("Field", &one, &origin).half_orientation_error(),
        0.0,
        "no orientations"
    );

    // A quarter turn about Z: the half quaternion (0.70703125 twice)
    // scales the rotation by 2 * 0.70703125^2, so +X lands at
    // (0, 0.99975586, 0) instead of (0, 1, 0).
    let turned = [QUARTER_TURN_Z];
    let error = PointInstancer::new("Field", &one, &origin)
        .with_orientations(&turned)
        .half_orientation_error();
    let moved = 1.0 - 2.0 * 0.707_031_25_f64 * 0.707_031_25;
    assert!(error >= moved, "{error} bounds the displacement {moved}");
    assert!(error < 1e-3, "{error}");
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
        .with_prototype(tri().with_attribute("site:tex", Value::Asset("t.png".into())));
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

/// `p · M` for a row-vector affine `M`.
fn apply(m: &[[f64; 4]; 4], p: [f64; 3]) -> [f64; 3] {
    core::array::from_fn(|j| p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j] + m[3][j])
}

#[test]
fn push_affine_splits_transforms_into_instance_arrays() {
    let mut field = PointInstancer::new("Field", vec![], vec![]).with_prototype(tri());
    let moved = Transform::from_translation([1.0, 2.0, 3.0]);
    assert_eq!(field.push_affine(0, &moved), Ok(0));
    assert!(
        field.orientations.is_none() && field.scales.is_none(),
        "a translation needs neither"
    );
    // A quarter turn about Z, scaled 2x along the prototype's X, then
    // mirrored in Y: column-vector rows `R · diag(2, -1, 1)`.
    let mirrored = Transform::from_affine_3x4([
        [0.0, 1.0, 0.0, 10.0],
        [2.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.5],
    ]);
    assert_eq!(field.push_affine(0, &mirrored), Ok(1));
    assert_eq!(field.proto_indices.as_ref(), [0, 0]);
    assert_eq!(
        field.positions.as_ref(),
        [[1.0, 2.0, 3.0], [10.0, 0.0, 0.5]]
    );
    let orientations = field.orientations.as_deref().unwrap();
    assert_eq!(orientations[0], [0.0, 0.0, 0.0, 1.0], "identity filled in");
    let scales = field.scales.as_deref().unwrap();
    assert_eq!(scales[0], [1.0; 3], "unit scale filled in");
    assert!(scales[1][0] < 0.0, "the mirror is a negative X scale");

    // The instance transforms the schema composes are the given ones.
    for (instance, source) in [moved, mirrored].iter().enumerate() {
        let composed = crate::instancer::instance_matrix(
            field.positions[instance],
            Some(orientations[instance].map(f64::from)),
            Some(scales[instance]),
        );
        for p in [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ] {
            let (got, want) = (apply(&composed, p), apply(&source.usd_rows(), p));
            for axis in 0..3 {
                assert!(
                    (got[axis] - want[axis]).abs() < 1e-6,
                    "instance {instance}, {p:?}: {got:?} vs {want:?}"
                );
            }
        }
    }
    // The instancer is valid as built.
    let text = scene(field).to_usda().unwrap();
    assert!(
        text.contains("float3[] scales = [(1, 1, 1), (-2, 1, 1)]"),
        "{text}"
    );
}

#[test]
fn push_affine_rejects_shear_and_copies_borrowed_arrays() {
    let indices = [0];
    let positions = [[0.0; 3]];
    let mut field = PointInstancer::new("Field", &indices, &positions).with_prototype(tri());
    let sheared = Transform::from_affine_3x4([
        [1.0, 0.5, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]);
    assert!(matches!(
        field.push_affine(0, &sheared),
        Err(crate::NotRigid::Sheared { .. })
    ));
    assert_eq!(field.proto_indices.len(), 1, "nothing appended");
    assert_eq!(
        field.push_affine(0, &Transform::from_translation([4.0, 0.0, 0.0])),
        Ok(1)
    );
    assert_eq!(field.positions.as_ref(), [[0.0; 3], [4.0, 0.0, 0.0]]);
    assert_eq!(positions, [[0.0; 3]], "the borrowed input is untouched");
}

#[test]
fn names_and_instance_primvars_are_authored() {
    let indices = [0, 0, 0];
    let positions = [[0.0; 3], [2.0, 0.0, 0.0], [4.0, 0.0, 0.0]];
    let tints = vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let instancer = PointInstancer::new("Field", &indices, &positions)
        .with_prototype(tri())
        .with_names(["oak", "elm", "ash"])
        .with_primvar(
            "displayColor",
            Primvar::per_instance(PrimvarData::color3(tints)).with_indices(vec![0, 1, 0]),
        )
        .with_primvar(
            "site:age",
            Primvar::per_instance(PrimvarData::float(vec![10.0, 20.0, 30.0])),
        )
        .with_primvar("site:zone", Primvar::constant(PrimvarData::int(vec![4])));
    let text = scene(instancer).to_usda().unwrap();
    for line in [
        "color3f[] primvars:displayColor = [(1, 0, 0), (0, 1, 0)] (\n            interpolation = \"vertex\"",
        "int[] primvars:displayColor:indices = [0, 1, 0]",
        "float[] primvars:site:age = [10, 20, 30] (\n            interpolation = \"vertex\"",
        "int[] primvars:site:zone = [4] (\n            interpolation = \"constant\"",
        "custom token[] instancer:names = [\"oak\", \"elm\", \"ash\"]",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
}

#[test]
fn rejects_unusable_names_and_primvars() {
    let two = [0, 0];
    let positions = [[0.0; 3]; 2];
    let base = || PointInstancer::new("Field", &two, &positions).with_prototype(tri());
    assert_eq!(
        problem(base().with_names(["a"])),
        InstancerProblem::LengthMismatch {
            name: "names",
            expected: 2,
            actual: 1
        }
    );
    for bad in ["1st", "a b", "", "Prototypes"] {
        assert_eq!(
            problem(base().with_names(["ok", bad])),
            InstancerProblem::InvalidName {
                instance: 1,
                name: bad.into()
            },
            "{bad:?}"
        );
    }
    assert_eq!(
        problem(base().with_names(["same", "same"])),
        InstancerProblem::DuplicateName {
            name: "same".into(),
            first: 0,
            second: 1
        }
    );
    assert_eq!(
        problem(base().with_primvar(
            "displayColor",
            Primvar::uniform(PrimvarData::color3(vec![[1.0; 3]; 2]))
        )),
        InstancerProblem::PrimvarInterpolation {
            name: "primvars:displayColor".into(),
            interpolation: crate::Interpolation::Uniform
        }
    );
    assert_eq!(
        problem(base().with_primvar(
            "displayColor",
            Primvar::per_instance(PrimvarData::color3(vec![[1.0; 3]; 3]))
        )),
        InstancerProblem::PrimvarLength {
            name: "primvars:displayColor".into(),
            expected: 2,
            actual: 3
        }
    );
    assert_eq!(
        problem(base().with_primvar(
            "displayColor",
            Primvar::per_instance(PrimvarData::color3(vec![[1.0; 3]])).with_indices(vec![0, 1])
        )),
        InstancerProblem::PrimvarIndexOutOfRange {
            name: "primvars:displayColor".into(),
            index: 1,
            values: 1
        }
    );
    assert_eq!(
        problem(base().with_attribute("instancer:names", Value::TokenArray(vec![]))),
        InstancerProblem::ReservedProperty {
            name: "instancer:names".into()
        }
    );
}

/// A scene whose `Tri` prototype (lifted by 1 along Z by its own
/// transform) is shared by two instancers and one direct instance.
fn shared_scene() -> Scene<'static> {
    let lifted = tri().with_transform(Transform::from_translation([0.0, 0.0, 1.0]));
    let a = PointInstancer::new("A", vec![0, 0], vec![[0.0; 3], [10.0, 0.0, 0.0]])
        .with_prototype(crate::Instance::new("Tri", "Tri"));
    let b = PointInstancer::new("B", vec![0], vec![[0.0, 5.0, 0.0]])
        .with_prototype(crate::Instance::new("Shared", "Tri"));
    Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root")
            .with_point_instancer(a)
            .with_point_instancer(b)
            .with_instance(
                crate::Instance::new("Single", "Tri")
                    .with_transform(Transform::from_translation([0.0, 0.0, 7.0]))
                    .with_primvar(
                        "displayColor",
                        Primvar::constant(PrimvarData::color3(vec![[1.0, 0.5, 0.0]])),
                    ),
            ),
    )
    .with_prototype(lifted.with_material("Bark"))
    .with_material(Material::new("Bark"))
}

#[test]
fn shared_prototypes_are_written_once() {
    let text = shared_scene().to_usda().unwrap();
    assert_eq!(text.matches("def Mesh").count(), 1, "{text}");
    let expected = r#"
    class "Prototypes"
    {
        def Mesh "Tri" ("#;
    assert!(text.contains(expected), "{text}");
    let expected = r#"
        def Scope "Prototypes"
        {
            def "Tri" (
                instanceable = true
                prepend references = </Root/Prototypes/Tri>
            )
            {
            }
        }"#;
    assert!(text.contains(expected), "{text}");
    assert!(
        text.contains("rel prototypes = </Root/B/Prototypes/Shared>"),
        "{text}"
    );
    // Both instancers bound the shared triangle, lifted by its own
    // transform.
    let extents: vec::Vec<&str> = text.lines().filter(|l| l.contains("extent =")).collect();
    assert!(
        extents[0].contains("[(0, 0, 1), (11, 1, 1)]"),
        "{extents:?}"
    );
    assert!(extents[1].contains("[(0, 5, 1), (1, 6, 1)]"), "{extents:?}");
    // The direct instance's transform follows the prototype root's.
    let expected = r#"
    def "Single" (
        instanceable = true
        prepend references = </Root/Prototypes/Tri>
    )
    {
        color3f[] primvars:displayColor = [(1, 0.5, 0)] (
            interpolation = "constant"
        )
        matrix4d xformOp:transform = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 8, 1) )"#;
    assert!(text.contains(expected), "{text}");
}

#[test]
fn instances_need_a_defined_acyclic_prototype() {
    let unknown = Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_instance(crate::Instance::new("I", "Missing")),
    );
    assert_eq!(
        unknown.to_usda(),
        Err(ExportError::UnknownPrototype {
            path: "/Root/I".into(),
            prototype: "Missing".into()
        })
    );
    let cycle = Scene::new(StageSettings::new(UpAxis::Z, 1.0), Xform::new("Root"))
        .with_prototype(Xform::new("A").with_instance(crate::Instance::new("ToB", "B")))
        .with_prototype(Xform::new("B").with_instance(crate::Instance::new("ToA", "A")));
    assert_eq!(
        cycle.to_usda(),
        Err(ExportError::PrototypeCycle {
            prototype: "A".into()
        })
    );
    let varying = Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_instance(crate::Instance::new("I", "Tri").with_primvar(
            "displayColor",
            Primvar::per_instance(PrimvarData::color3(vec![[1.0; 3]])),
        )),
    )
    .with_prototype(tri());
    assert!(matches!(
        varying.to_usda(),
        Err(ExportError::InvalidInstancer {
            problem: InstancerProblem::PrimvarInterpolation { .. },
            ..
        })
    ));
}
