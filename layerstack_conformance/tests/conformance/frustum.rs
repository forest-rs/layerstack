// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Frustum picking and intersection against `GfFrustum`, pinned by
//! `scripts/frustum_oracle.py`.
use layerstack_schemas::camera::{
    CameraError, FieldOfViewAxis, FrustumProjection, Perspective, PreparedFrustum, Ray,
};
use serde::Deserialize;

type Matrix = [[f64; 4]; 4];

#[derive(Deserialize)]
struct Oracle {
    version: String,
    frusta: Vec<FrustumRow>,
    perspectives: Vec<PerspectiveRow>,
}

#[derive(Deserialize)]
struct FrustumRow {
    camera_to_world: Matrix,
    window: [f64; 4],
    clipping_range: [f64; 2],
    projection: String,
    view: Matrix,
    projection_matrix: Matrix,
    corners: [[f64; 3]; 8],
    picks: Vec<PickRow>,
    picks_through: Vec<PickThroughRow>,
    narrowed: Vec<NarrowedRow>,
    narrowed_around: Vec<NarrowedAroundRow>,
    points: Vec<PointRow>,
    perspective: Option<PerspectivePair>,
}

#[derive(Deserialize)]
struct PickRow {
    window_point: [f64; 2],
    origin: [f64; 3],
    direction: [f64; 3],
}

#[derive(Deserialize)]
struct PickThroughRow {
    point: [f64; 3],
    origin: [f64; 3],
    direction: [f64; 3],
}

#[derive(Deserialize)]
struct NarrowedRow {
    window_point: [f64; 2],
    size: [f64; 2],
    window: [f64; 4],
}

#[derive(Deserialize)]
struct NarrowedAroundRow {
    point: [f64; 3],
    size: [f64; 2],
    window: Option<[f64; 4]>,
}

#[derive(Deserialize)]
struct PointRow {
    point: [f64; 3],
    inside: bool,
}

#[derive(Deserialize)]
struct PerspectivePair {
    vertical: [f64; 4],
    horizontal: [f64; 4],
}

#[derive(Deserialize)]
struct PerspectiveRow {
    camera_to_world: Matrix,
    field_of_view: f64,
    vertical: bool,
    aspect_ratio: f64,
    clipping_range: [f64; 2],
    window: [f64; 4],
    view: Matrix,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(include_str!("../../fixtures/frustum.json")).unwrap();
    assert_eq!(
        oracle.version,
        layerstack_schemas::OPENUSD_VERSION,
        "the oracle was pinned by the supported OpenUSD"
    );
    oracle
}

/// Placed values carry `GfRotation`'s `acos`/`sin`/`cos` rounding, which
/// the conformed quaternion path does not share bit for bit.
fn close(actual: impl IntoIterator<Item = f64>, expected: impl IntoIterator<Item = f64>) {
    let actual: Vec<_> = actual.into_iter().collect();
    let expected: Vec<_> = expected.into_iter().collect();
    assert_eq!(actual.len(), expected.len(), "component counts agree");
    for (i, (a, b)) in actual.iter().zip(&expected).enumerate() {
        assert!(
            (a - b).abs() <= 1e-9 * b.abs().max(1.),
            "element {i}: {actual:?} != {expected:?}"
        );
    }
}

fn close_ray(ray: Ray, origin: [f64; 3], direction: [f64; 3]) {
    close(ray.origin, origin);
    close(ray.direction, direction);
}

fn frustum(row: &FrustumRow) -> PreparedFrustum {
    let projection = match row.projection.as_str() {
        "perspective" => FrustumProjection::Perspective,
        _ => FrustumProjection::Orthographic,
    };
    PreparedFrustum::new(
        &row.camera_to_world,
        row.window,
        row.clipping_range,
        projection,
    )
    .unwrap()
}

#[test]
fn frusta_placed_from_scaled_sheared_and_reflected_transforms_match_openusd() {
    for row in oracle().frusta {
        let frustum = frustum(&row);
        close(
            frustum.view_matrix().iter().flatten().copied(),
            row.view.into_iter().flatten(),
        );
        close(
            frustum.projection_matrix().into_iter().flatten(),
            row.projection_matrix.into_iter().flatten(),
        );
        close(
            frustum.corners().into_iter().flatten(),
            row.corners.into_iter().flatten(),
        );
        assert_eq!(frustum.window(), row.window);
        assert_eq!(frustum.clipping_range(), row.clipping_range);
        match row.perspective {
            Some(pair) => {
                for (axis, expected) in [
                    (FieldOfViewAxis::Vertical, pair.vertical),
                    (FieldOfViewAxis::Horizontal, pair.horizontal),
                ] {
                    let p = frustum.perspective(axis).unwrap();
                    close([p.field_of_view, p.aspect_ratio, p.near, p.far], expected);
                }
            }
            None => assert_eq!(frustum.perspective(FieldOfViewAxis::Vertical), None),
        }
    }
}

#[test]
fn pick_rays_match_openusd_for_window_and_world_points() {
    for row in oracle().frusta {
        let frustum = frustum(&row);
        for pick in &row.picks {
            close_ray(
                frustum.pick_ray(pick.window_point),
                pick.origin,
                pick.direction,
            );
        }
        for pick in &row.picks_through {
            close_ray(
                frustum.pick_ray_through(pick.point),
                pick.origin,
                pick.direction,
            );
        }
    }
}

#[test]
fn narrowed_frusta_match_openusd_including_clamped_and_behind_eye_regions() {
    for row in oracle().frusta {
        let frustum = frustum(&row);
        for region in &row.narrowed {
            let narrowed = frustum.narrowed(region.window_point, region.size);
            close(narrowed.window(), region.window);
            assert_eq!(narrowed.view_matrix(), frustum.view_matrix());
            assert_eq!(narrowed.clipping_range(), frustum.clipping_range());
        }
        for region in &row.narrowed_around {
            let narrowed = frustum.narrowed_around(region.point, region.size);
            match (narrowed, region.window) {
                (Some(narrowed), Some(window)) => close(narrowed.window(), window),
                (None, None) => {}
                (narrowed, window) => panic!("{:?} != {window:?}", narrowed.map(|n| n.window())),
            }
        }
    }
}

#[test]
fn point_intersections_match_openusd_including_the_boundary() {
    for (f, row) in oracle().frusta.iter().enumerate() {
        let frustum = frustum(row);
        for (i, point) in row.points.iter().enumerate() {
            assert_eq!(
                frustum.contains_world_point(point.point),
                point.inside,
                "frustum {f} point {i}"
            );
        }
    }
}

#[test]
fn perspective_frusta_match_set_perspective() {
    for row in oracle().perspectives {
        let axis = if row.vertical {
            FieldOfViewAxis::Vertical
        } else {
            FieldOfViewAxis::Horizontal
        };
        let perspective = Perspective {
            field_of_view: row.field_of_view,
            aspect_ratio: row.aspect_ratio,
            near: row.clipping_range[0],
            far: row.clipping_range[1],
        };
        let frustum =
            PreparedFrustum::from_perspective(&row.camera_to_world, axis, perspective).unwrap();
        close(frustum.window(), row.window);
        close(
            frustum.view_matrix().iter().flatten().copied(),
            row.view.into_iter().flatten(),
        );
        // `GetPerspective` returns what `SetPerspective` took, except a zero
        // aspect ratio, which it built as 1.
        let back = frustum.perspective(axis).unwrap();
        let aspect = if row.aspect_ratio == 0. {
            1.
        } else {
            row.aspect_ratio
        };
        close(
            [back.field_of_view, back.aspect_ratio, back.near, back.far],
            [row.field_of_view, aspect, perspective.near, perspective.far],
        );
    }
}

#[test]
fn invalid_frustum_inputs_error_instead_of_preparing_unusable_planes() {
    let identity = [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ];
    let window = [-1., -1., 1., 1.];
    let perspective = FrustumProjection::Perspective;
    let mut singular = identity;
    singular[1] = [0.; 4];
    assert_eq!(
        PreparedFrustum::new(&singular, window, [1., 10.], perspective).unwrap_err(),
        CameraError::InvalidTransform
    );
    assert_eq!(
        PreparedFrustum::new(&identity, [1., -1., 1., 1.], [1., 10.], perspective).unwrap_err(),
        CameraError::InvalidAttribute("window")
    );
    for range in [[0., 10.], [10., 1.], [1., f64::INFINITY]] {
        assert_eq!(
            PreparedFrustum::new(&identity, window, range, perspective).unwrap_err(),
            CameraError::InvalidAttribute("clippingRange")
        );
    }
    // Orthographic frusta may start at or behind the eye, as OpenUSD's do.
    let ortho = PreparedFrustum::new(
        &identity,
        window,
        [-1., 10.],
        FrustumProjection::Orthographic,
    )
    .unwrap();
    assert!(ortho.contains_world_point([0., 0., 0.5]));
    assert_eq!(
        ortho.narrowed_around([0., 0., 0.5], [0.1, 0.1]).map(|_| ()),
        None
    );
}

#[test]
fn pinned_frustum_oracle_matches_configured_openusd() {
    let Ok(python) = std::env::var("LAYERSTACK_USD_PYTHON") else {
        return;
    };
    let output = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/frustum_oracle.py"
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
