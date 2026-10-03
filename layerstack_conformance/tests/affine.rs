// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Full affine factorization agrees with Gf without projecting away shear.
#![allow(missing_docs, reason = "integration tests")]
use layerstack_schemas::affine::{AffineError, AffineFactors, TrsError};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    matrix: [[f64; 4]; 4],
    epsilon: f64,
    singular: bool,
    scale: [f64; 3],
    translation: [f64; 3],
    orientation: [[f64; 3]; 3],
    rotation: [[f64; 3]; 3],
}
fn close(a: f64, b: f64) {
    assert!(
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.),
        "{a} != {b}"
    );
}
#[test]
fn full_factors_match_gf_and_reconstruct_reflections_shear_and_singular_data() {
    let oracle: Oracle = serde_json::from_str(include_str!("../fixtures/affine.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    for row in oracle.rows {
        let actual = AffineFactors::with_epsilon(&row.matrix, row.epsilon).unwrap();
        assert_eq!(actual.singular, row.singular);
        assert_eq!(actual.translation, row.translation);
        for (a, b) in actual.scale.iter().zip(row.scale.iter()) {
            close(*a, *b);
        }
        for (a, b) in actual
            .scale_orientation
            .iter()
            .flatten()
            .zip(row.orientation.iter().flatten())
        {
            close(*a, *b);
        }
        for (a, b) in actual
            .rotation
            .iter()
            .flatten()
            .zip(row.rotation.iter().flatten())
        {
            close(*a, *b);
        }
        for (a, b) in actual
            .recompose()
            .iter()
            .flatten()
            .zip(row.matrix.iter().flatten())
        {
            close(*a, *b);
        }
    }
}
#[test]
fn checked_trs_reports_loss_instead_of_discarding_shear() {
    let matrix = [
        [-2., 0., 0., 0.],
        [0., 3., 0., 0.],
        [0., 0., 4., 0.],
        [1., 2., 3., 1.],
    ];
    let factors = AffineFactors::compute(&matrix).unwrap();
    let trs = factors.to_trs(1e-12).unwrap();
    assert_eq!(trs.translation, [1., 2., 3.]);
    assert_eq!(trs.scale, [-2., -3., -4.]);
    assert!(factors.determinant < 0.);
    let mut shear = matrix;
    shear[0][1] = 0.5;
    let factors = AffineFactors::compute(&shear).unwrap();
    assert!(matches!(
        factors.to_trs(1e-12),
        Err(TrsError::Residual { .. })
    ));
    assert!(factors.to_trs(1.).unwrap().relative_error > 0.);
    assert_eq!(factors.to_trs(-1.), Err(TrsError::InvalidTolerance));
    let mut singular = matrix;
    singular[0] = [0.; 4];
    assert_eq!(
        AffineFactors::compute(&singular).unwrap().to_trs(1.),
        Err(TrsError::Singular)
    );
}
#[test]
fn perspective_nonfinite_and_invalid_tolerances_are_rejected() {
    let mut m = [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ];
    assert_eq!(
        AffineFactors::with_epsilon(&m, 0.),
        Err(AffineError::InvalidEpsilon)
    );
    let mut invalid_rotation = AffineFactors::compute(&m).unwrap();
    invalid_rotation.rotation[1] = invalid_rotation.rotation[0];
    assert_eq!(invalid_rotation.to_trs(1.), Err(TrsError::InvalidRotation));
    invalid_rotation.rotation = [[-1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
    assert_eq!(invalid_rotation.to_trs(1.), Err(TrsError::InvalidRotation));
    let mut overflowing = AffineFactors::compute(&m).unwrap();
    overflowing.scale = [f64::MAX; 3];
    overflowing.scale_orientation = [[f64::MAX; 3]; 3];
    assert_eq!(overflowing.to_trs(1.), Err(TrsError::NonFinite));
    m[0][3] = 0.1;
    assert_eq!(AffineFactors::compute(&m), Err(AffineError::NonAffine));
    m[0][0] = f64::NAN;
    assert_eq!(AffineFactors::compute(&m), Err(AffineError::NonFinite));
}
