// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use alloc::vec;
fn distance() -> DistanceHeuristicQuery {
    DistanceHeuristicQuery {
        domain: Arc::from("imaging"),
        center: [0.; 3],
        extent: None,
        bounding_volume: None,
        thresholds: vec![10., 50., 100.],
        blend_thresholds: vec![11., 55., 110.],
    }
}
fn screen(method: ProjectionMethod) -> ScreenSizeHeuristicQuery {
    ScreenSizeHeuristicQuery {
        domain: Arc::from("imaging"),
        extent: Some([[-1.5; 3], [1.5; 3]]),
        bounding_volume: None,
        projection_method: method,
        thresholds: vec![0.25, 0.10],
        blend_thresholds: vec![0.20, 0.05],
    }
}
fn frustum(projection: LodProjection) -> LodFrustum {
    LodFrustum {
        projection,
        view_matrix: gf::IDENTITY,
        window: [-1., -1., 1., 1.],
        clipping_range: [1., 100.],
    }
}
#[test]
fn transition_edges_blends_and_missing_blends() {
    let mut d = distance();
    for (value, expected) in [
        (0., 0.),
        (10., 0.),
        (10.5, 0.5),
        (11., 1.),
        (50., 1.),
        (52.5, 1.5),
        (105., 2.5),
        (110., 3.),
    ] {
        assert_eq!(d.compute_lod(value), Ok(expected));
    }
    d.blend_thresholds = vec![1000.];
    assert_eq!(d.compute_lod(30.), Ok(0.5)); // First blend clamps at next threshold.
    assert_eq!(d.compute_lod(75.), Ok(2.)); // No second blend entry.
    d.blend_thresholds.clear();
    assert_eq!(d.compute_lod(10.), Ok(1.));
    let s = screen(ProjectionMethod::Extent);
    assert_eq!(s.compute_lod(0.225).unwrap(), 0.50000006); // Float-authored thresholds.
    assert_eq!(s.compute_lod(0.075).unwrap(), 1.5);
    d.thresholds = vec![20., 10.];
    assert_eq!(
        d.validate(),
        Err(LodError::UnsortedThresholds("thresholds"))
    );
    assert_eq!(s.compute_lod(f64::NAN), Err(LodError::InvalidMetric));
}
#[test]
fn distance_uses_local_clamp_and_singular_center_fallback() {
    let mut d = distance();
    d.extent = Some([[-1.; 3], [1.; 3]]);
    d.center = [5., 0., 0.];
    let transform = gf::mul(&gf::scale([2., 1., 1.]), &gf::translate([10., 0., 0.]));
    assert_eq!(d.compute_distance([10., 0., 0.], &transform), Ok(0.));
    assert_eq!(d.compute_distance([14., 0., 0.], &transform), Ok(2.));
    assert_eq!(
        d.compute_distance([10., 0., 0.], &gf::scale([0., 1., 1.])),
        Ok(10.)
    );
    let h = LodHysteresis {
        previous: 10.,
        width: 2.,
    };
    assert_eq!(h.apply(11.), Ok(10.));
    assert_eq!(h.apply(15.), Ok(13.));
    assert_eq!(h.apply(5.), Ok(7.));
    assert_eq!(
        LodHysteresis { width: -1., ..h }.apply(1.),
        Err(LodError::InvalidHysteresis)
    );
    assert_eq!(
        d.compute_distance([f64::INFINITY, 0., 0.], &transform),
        Err(LodError::InvalidMetric)
    );
}
#[test]
fn projected_extent_near_clip_side_motion_and_complete_rejection() {
    let query = screen(ProjectionMethod::Extent);
    let f = frustum(LodProjection::Perspective);
    // OpenUSD fixture: cube occupies [0,3] x [0,3] x [-6,-3]. Area=9/36.
    assert!(
        (query
            .compute_screen_size(&f, &gf::translate([1.5, 1.5, -4.5]))
            .unwrap()
            - 0.25)
            .abs()
            < 1e-12
    );
    // Moving sideways partly offscreen retains the entire projected hull.
    assert!(
        (query
            .compute_screen_size(&f, &gf::translate([4., 1.5, -4.5]))
            .unwrap()
            - 0.328125)
            .abs()
            < 1e-12
    );
    assert_eq!(
        query.compute_screen_size(&f, &gf::translate([100., 0., -4.5])),
        Ok(0.)
    );
    assert_eq!(
        query.compute_screen_size(&f, &gf::translate([0., 0., 4.5])),
        Ok(0.)
    );
    // Near plane cuts through box: clipped hull spans [-1.5,1.5] at depth one.
    assert!(
        (query
            .compute_screen_size(&f, &gf::translate([0., 0., -1.5]))
            .unwrap()
            - 2.25)
            .abs()
            < 1e-12
    );
    let orthographic = frustum(LodProjection::Orthographic);
    assert_eq!(
        query.compute_screen_size(&orthographic, &gf::translate([0., 0., -4.5])),
        Ok(2.25)
    );
}
#[test]
fn projected_sphere_uses_euclidean_distance_and_diagonal_radius() {
    let query = screen(ProjectionMethod::Sphere);
    let f = frustum(LodProjection::Perspective);
    let expected = core::f64::consts::PI * 6.75 / (24.75 * 4.);
    assert!(
        (query
            .compute_screen_size(&f, &gf::translate([1.5, 1.5, -4.5]))
            .unwrap()
            - expected)
            .abs()
            < 1e-12
    );
    assert_eq!(query.compute_screen_size(&f, &gf::IDENTITY), Ok(f64::MAX));
    assert_eq!(
        query.compute_screen_size(&f, &gf::translate([100., 0., -4.5])),
        Ok(0.)
    );
    let mut empty = query;
    empty.extent = Some([[1.; 3], [-1.; 3]]);
    assert_eq!(empty.compute_screen_size(&f, &gf::IDENTITY), Ok(0.));
}
#[test]
fn captures_owned_uniform_and_sampled_inputs() {
    use crate::SchemaEdit;
    use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions, edit::EditTarget};
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let schemas = Arc::new(crate::openusd(&mut store.tokens));
    let mut stage = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let path = store.path("/Distance");
    let spath = store.path("/Screen");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = LODDistanceHeuristic::define(&mut edit, path);
    handle.set_lod_domain(&mut edit, "imaging");
    handle.set_thresholds(&mut edit, &[10., 20.]);
    handle.set_center_at(&mut edit, 1., [1., 2., 3.]);
    LODScreenSizeHeuristic::define(&mut edit, spath).set_lod_domain(&mut edit, "imaging");
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(stage.stage(), &store);
    let captured = LODDistanceHeuristic::new(&scene, path)
        .unwrap()
        .capture_query(Time::at(1.))
        .unwrap();
    assert_eq!(captured.center, [1., 2., 3.]);
    assert_eq!(captured.thresholds, [10., 20.]);
    let captured_screen = LODScreenSizeHeuristic::new(&scene, spath)
        .unwrap()
        .capture_query(Time::Default)
        .unwrap();
    assert_eq!(captured_screen.extent, Some([[-1.; 3], [1.; 3]]));
    drop(stage);
    drop(store);
    assert_eq!(
        captured.compute_distance([1., 2., 3.], &gf::IDENTITY),
        Ok(0.)
    );
}

#[test]
fn capture_samples_uniform_settings_at_requested_time() {
    use crate::SchemaEdit;
    use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions, edit::EditTarget};
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let schemas = Arc::new(crate::openusd(&mut store.tokens));
    let mut stage = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let distance = store.path("/Distance");
    let screen = store.path("/Screen");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let d = LODDistanceHeuristic::define(&mut edit, distance);
    d.set_thresholds(&mut edit, &[10.]);
    d.set_blend_thresholds(&mut edit, &[20.]);
    d.set_lod_domain(&mut edit, "default");
    let s = LODScreenSizeHeuristic::define(&mut edit, screen);
    s.set_thresholds(&mut edit, &[0.5]);
    s.set_blend_thresholds(&mut edit, &[0.4]);
    s.set_lod_domain(&mut edit, "default");
    s.set_projection_method(
        &mut edit,
        crate::usd_lod::LODScreenSizeHeuristicProjectionMethod::ProjectedSphere,
    );
    let mut transaction = edit.finish();
    let target = EditTarget::for_layer(LayerId(1));
    for (path, thresholds, blends) in [
        (distance, &[100.][..], &[200.][..]),
        (screen, &[0.8][..], &[0.7][..]),
    ] {
        for (name, values) in [("thresholds", thresholds), ("blendThresholds", blends)] {
            let token = store.tokens.intern(name);
            let value = crate::value::write_float_array(values, &mut store.tokens);
            transaction.set_time_sample(target.property(PropertyPath::new(path, token)), 1., value);
        }
        let token = store.tokens.intern("lod:domain");
        let value = crate::value::write_token("sampled", &mut store.tokens);
        transaction.set_time_sample(target.property(PropertyPath::new(path, token)), 1., value);
    }
    let token = store.tokens.intern("projectionMethod");
    let value = crate::value::write_token("projectedExtent", &mut store.tokens);
    transaction.set_time_sample(target.property(PropertyPath::new(screen, token)), 1., value);
    stage.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(stage.stage(), &store);
    let d = LODDistanceHeuristic::new(&scene, distance).unwrap();
    assert_eq!(
        d.capture_query(Time::Default).unwrap().compute_lod(50.),
        Ok(1.)
    );
    let sampled = d.capture_query(Time::at(1.)).unwrap();
    assert_eq!(sampled.compute_lod(50.), Ok(0.));
    assert_eq!(&*sampled.domain, "sampled");
    assert_eq!(sampled.blend_thresholds, [200.]);
    let s = LODScreenSizeHeuristic::new(&scene, screen).unwrap();
    assert_eq!(
        s.capture_query(Time::Default).unwrap().projection_method,
        ProjectionMethod::Sphere
    );
    let sampled = s.capture_query(Time::at(1.)).unwrap();
    assert_eq!(sampled.projection_method, ProjectionMethod::Extent);
    assert_eq!(sampled.thresholds, [0.8]);
    assert_eq!(sampled.blend_thresholds, [0.7]);
    assert_eq!(&*sampled.domain, "sampled");
}
