// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reference-backed mass evaluation and pure parallel-axis invariants.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack_schemas::{
    Scene,
    mass::{
        CollisionMassInformation, MassContribution, MassError, MassProperties, diagonalize_inertia,
        rotate_inertia, translate_inertia,
    },
    usd_physics::PhysicsRigidBodyApi,
};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    path: String,
    mass: f32,
    inertia: [f32; 3],
    center: [f32; 3],
    axes: Option<[f32; 4]>,
    inputs: Vec<Input>,
}
#[derive(Deserialize)]
struct Input {
    path: String,
    volume: f32,
    inertia: [[f32; 3]; 3],
    center: [f32; 3],
    position: [f32; 3],
    orientation: [f32; 4],
}
fn close(actual: f32, expected: f32, context: &str) {
    assert!(
        (actual - expected).abs() <= 2e-4 * expected.abs().max(1.),
        "{context}: {actual} != {expected}"
    );
}
#[test]
fn rigid_body_mass_matches_openusd_callback_evaluation() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/mass/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, live) = support::scene(include_str!("../fixtures/mass/scene.usda"));
    for row in oracle.rows {
        let path = store.path(&row.path);
        let scene = Scene::new(live.stage(), &store);
        let body = PhysicsRigidBodyApi::get(&scene, path).unwrap();
        let mut called = Vec::new();
        let actual = body
            .compute_mass_properties(|prim| {
                let path = store.paths.display(prim.path(), &store.tokens);
                let input = row.inputs.iter().find(|i| i.path == path).unwrap();
                called.push(path);
                Ok(CollisionMassInformation {
                    volume: input.volume,
                    inertia: input.inertia,
                    center_of_mass: input.center,
                    local_position: input.position,
                    local_orientation: input.orientation,
                })
            })
            .unwrap();
        close(actual.mass, row.mass, &row.path);
        for i in 0..3 {
            close(actual.center_of_mass[i], row.center[i], &row.path);
            close(actual.diagonal_inertia[i], row.inertia[i], &row.path);
        }
        if let Some(axes) = row.axes {
            let sign = if actual
                .principal_axes
                .iter()
                .zip(axes)
                .map(|(a, b)| a * b)
                .sum::<f32>()
                < 0.
            {
                -1.
            } else {
                1.
            };
            for (a, b) in actual.principal_axes.iter().zip(axes) {
                close(*a * sign, b, &row.path);
            }
        } else {
            // C++ leaves axes uninitialized without collider data or authored
            // axes. Our deterministic identity is a deliberate improvement.
            assert_eq!(actual.principal_axes, [0., 0., 0., 1.]);
        }
        assert_eq!(
            called,
            row.inputs
                .iter()
                .map(|i| i.path.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(actual.collider_count, called.len());
        assert_eq!(
            actual.used_sphere_approximation,
            row.path == "/SphereFallback"
        );
    }
}
#[test]
fn pure_aggregation_and_principal_frame_reconstruct_inertia() {
    let props = MassProperties {
        mass: 2.,
        inertia: [[1., 0., 0.], [0., 2., 0.], [0., 0., 3.]],
        center_of_mass: [0.; 3],
    };
    let a = MassContribution {
        properties: props,
        position: [-1., 0., 0.],
        orientation: [0., 0., 0., 1.],
    };
    let b = MassContribution {
        position: [1., 0., 0.],
        ..a
    };
    let total = MassProperties::sum(&[a, b]).unwrap();
    assert_eq!(total.mass, 4.);
    assert_eq!(total.center_of_mass, [0.; 3]);
    assert_eq!(total.inertia, [[2., 0., 0.], [0., 8., 0.], [0., 0., 10.]]);
    let moved = translate_inertia(props.inertia, props.mass, [1., 2., 3.]);
    assert_eq!(moved, [[27., -4., -6.], [-4., 22., -12.], [-6., -12., 13.]]);
    let principal = diagonalize_inertia(moved).unwrap();
    let diagonal = [
        [principal.diagonal[0], 0., 0.],
        [0., principal.diagonal[1], 0.],
        [0., 0., principal.diagonal[2]],
    ];
    let reconstructed = rotate_inertia(diagonal, principal.axes);
    for (a, b) in reconstructed.iter().flatten().zip(moved.iter().flatten()) {
        close(*a, *b, "reconstruct");
    }
    assert_eq!(MassProperties::sum(&[]).unwrap().mass, 0.);
    let bad = MassContribution {
        properties: MassProperties { mass: -1., ..props },
        ..a
    };
    assert_eq!(MassProperties::sum(&[bad]), Err(MassError::InvalidMass));
    let bad = MassContribution {
        orientation: [0.; 4],
        ..a
    };
    assert_eq!(
        MassProperties::sum(&[bad]),
        Err(MassError::InvalidTransform)
    );
    assert_eq!(
        diagonalize_inertia([[-1., 0., 0.], [0., 1., 0.], [0., 0., 1.]]),
        Err(MassError::InvalidInertia)
    );
}
#[test]
fn invalid_geometry_and_missing_mass_return_errors_without_nonfinite_results() {
    let (mut store, live) = support::scene(include_str!("../fixtures/mass/scene.usda"));
    let path = store.path("/Default");
    let scene = Scene::new(live.stage(), &store);
    let body = PhysicsRigidBodyApi::get(&scene, path).unwrap();
    let info = CollisionMassInformation {
        volume: -1.,
        inertia: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
        center_of_mass: [0.; 3],
        local_position: [0.; 3],
        local_orientation: [0., 0., 0., 1.],
    };
    assert!(matches!(
        body.compute_mass_properties(|_| Ok(info)),
        Err(MassError::InvalidCollider(_))
    ));
    assert_eq!(
        body.compute_mass_properties(|_| Ok(CollisionMassInformation { volume: 0., ..info })),
        Err(MassError::InvalidMass)
    );
    assert_eq!(
        body.compute_mass_properties(|_| Err(MassError::InvalidTransform)),
        Err(MassError::InvalidTransform)
    );
}

#[test]
fn missing_mass_and_invalid_units_are_reported_explicitly() {
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef Xform \"Body\" (prepend apiSchemas = [\"PhysicsRigidBodyAPI\"]) {}\n",
    );
    let path = store.path("/Body");
    let scene = Scene::new(live.stage(), &store);
    let body = PhysicsRigidBodyApi::get(&scene, path).unwrap();
    assert_eq!(
        body.compute_mass_properties(|_| panic!("no colliders")),
        Err(MassError::InvalidMass)
    );
    let (mut store, live) = support::scene(
        "#usda 1.0\n( metersPerUnit = 0 )\ndef Xform \"Body\" (prepend apiSchemas = [\"PhysicsRigidBodyAPI\",\"PhysicsMassAPI\"]) {\n float physics:mass = 3\n}\n",
    );
    let path = store.path("/Body");
    let scene = Scene::new(live.stage(), &store);
    let body = PhysicsRigidBodyApi::get(&scene, path).unwrap();
    assert_eq!(
        body.compute_mass_properties(|_| panic!("invalid units before callbacks")),
        Err(MassError::InvalidUnits)
    );
}
