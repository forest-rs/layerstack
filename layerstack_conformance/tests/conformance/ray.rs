// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ray intersection tests against `GfRay`, pinned by `scripts/ray_oracle.py`.
use layerstack_schemas::{
    bounds::{BoundingBox, Range3d},
    camera::{Ray, RayInterval},
};
use serde::Deserialize;

type Matrix = [[f64; 4]; 4];

#[derive(Deserialize)]
struct Oracle {
    version: String,
    triangles: Vec<TriangleRow>,
    planes: Vec<PlaneRow>,
    ranges: Vec<RangeRow>,
    bounds: Vec<BoundRow>,
    spheres: Vec<SphereRow>,
    cylinders: Vec<CylinderRow>,
    cones: Vec<ConeRow>,
}

#[derive(Deserialize)]
struct TriangleHitRow {
    distance: f64,
    barycentric: [f64; 3],
    front_facing: bool,
}

#[derive(Deserialize)]
struct TriangleRow {
    origin: [f64; 3],
    direction: [f64; 3],
    points: [[f64; 3]; 3],
    hit: Option<TriangleHitRow>,
}

#[derive(Deserialize)]
struct PlaneHitRow {
    distance: f64,
    front_facing: bool,
}

#[derive(Deserialize)]
struct PlaneRow {
    origin: [f64; 3],
    direction: [f64; 3],
    equation: [f64; 4],
    hit: Option<PlaneHitRow>,
}

#[derive(Deserialize)]
struct RangeRow {
    origin: [f64; 3],
    direction: [f64; 3],
    min: [f64; 3],
    max: [f64; 3],
    hit: Option<[f64; 2]>,
}

#[derive(Deserialize)]
struct BoundRow {
    origin: [f64; 3],
    direction: [f64; 3],
    min: [f64; 3],
    max: [f64; 3],
    matrix: Matrix,
    hit: Option<[f64; 2]>,
}

#[derive(Deserialize)]
struct SphereRow {
    origin: [f64; 3],
    direction: [f64; 3],
    center: [f64; 3],
    radius: f64,
    hit: Option<[f64; 2]>,
}

#[derive(Deserialize)]
struct CylinderRow {
    origin: [f64; 3],
    direction: [f64; 3],
    base: [f64; 3],
    axis: [f64; 3],
    radius: f64,
    hit: Option<[f64; 2]>,
}

#[derive(Deserialize)]
struct ConeRow {
    origin: [f64; 3],
    direction: [f64; 3],
    base: [f64; 3],
    axis: [f64; 3],
    radius: f64,
    height: f64,
    hit: Option<[f64; 2]>,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(include_str!("../../fixtures/ray.json")).unwrap();
    assert_eq!(
        oracle.version,
        layerstack_schemas::OPENUSD_VERSION,
        "the oracle was pinned by the supported OpenUSD"
    );
    oracle
}

fn close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= 1e-12 * expected.abs().max(1.),
        "{what}: {actual} != {expected}"
    );
}

fn interval(actual: Option<RayInterval>, expected: Option<[f64; 2]>, what: &str) {
    match (actual, expected) {
        (None, None) => {}
        (Some(actual), Some([enter, exit])) => {
            close(actual.enter, enter, what);
            close(actual.exit, exit, what);
        }
        (actual, expected) => panic!("{what}: {actual:?} != {expected:?}"),
    }
}

#[test]
fn triangle_hits_match_openusd_including_degenerate_and_parallel_cases() {
    let rows = oracle().triangles;
    assert!(rows.iter().any(|r| r.hit.is_some()) && rows.iter().any(|r| r.hit.is_none()));
    for (i, row) in rows.into_iter().enumerate() {
        let ray = Ray {
            origin: row.origin,
            direction: row.direction,
        };
        let hit = ray.intersect_triangle(row.points);
        match (hit, row.hit) {
            (None, None) => {}
            (Some(hit), Some(expected)) => {
                close(hit.distance, expected.distance, "distance");
                for (a, b) in hit.barycentric.into_iter().zip(expected.barycentric) {
                    close(a, b, "barycentric");
                }
                assert_eq!(hit.front_facing, expected.front_facing, "row {i}");
            }
            (hit, expected) => panic!("row {i}: {hit:?} != {:?}", expected.is_some()),
        }
    }
}

#[test]
fn plane_hits_match_openusd_including_parallel_and_zero_normals() {
    for (i, row) in oracle().planes.into_iter().enumerate() {
        let ray = Ray {
            origin: row.origin,
            direction: row.direction,
        };
        match (ray.intersect_plane(row.equation), row.hit) {
            (None, None) => {}
            (Some(hit), Some(expected)) => {
                close(hit.distance, expected.distance, "distance");
                assert_eq!(hit.front_facing, expected.front_facing, "row {i}");
            }
            (hit, expected) => panic!("row {i}: {hit:?} != {:?}", expected.is_some()),
        }
    }
}

#[test]
fn solid_hits_match_openusd() {
    let oracle = oracle();
    for row in oracle.ranges {
        let ray = Ray {
            origin: row.origin,
            direction: row.direction,
        };
        let range = Range3d {
            min: row.min,
            max: row.max,
        };
        interval(ray.intersect_range(&range), row.hit, "range");
    }
    for row in oracle.bounds {
        let ray = Ray {
            origin: row.origin,
            direction: row.direction,
        };
        let bound = BoundingBox {
            range: Range3d {
                min: row.min,
                max: row.max,
            },
            matrix: row.matrix,
        };
        interval(ray.intersect_bound(&bound), row.hit, "bound");
    }
    for row in oracle.spheres {
        let ray = Ray {
            origin: row.origin,
            direction: row.direction,
        };
        interval(
            ray.intersect_sphere(row.center, row.radius),
            row.hit,
            "sphere",
        );
    }
    for row in oracle.cylinders {
        let ray = Ray {
            origin: row.origin,
            direction: row.direction,
        };
        interval(
            ray.intersect_cylinder(row.base, row.axis, row.radius),
            row.hit,
            "cylinder",
        );
    }
    for row in oracle.cones {
        let ray = Ray {
            origin: row.origin,
            direction: row.direction,
        };
        interval(
            ray.intersect_cone(row.base, row.axis, row.radius, row.height),
            row.hit,
            "cone",
        );
    }
}

#[test]
fn pinned_ray_oracle_matches_configured_openusd() {
    let Ok(python) = std::env::var("LAYERSTACK_USD_PYTHON") else {
        return;
    };
    let output = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/ray_oracle.py"
        ))
        .arg("--check")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
