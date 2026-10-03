// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Schema consumers share Stage's clip values and temporal dependencies.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, ResolvedAsset, Stage,
    StageOptions, TokenInterner,
};
use layerstack_schemas::{
    LabelInterval, LabelsQuery, Scene, Time, XformCache,
    usd_geom::{PointBased, Xformable},
    usd_skel::SkelAnimation,
};
use std::sync::Arc;

struct NoArcs;
impl AssetResolver for NoArcs {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}
fn scene() -> (InMemoryStore, Stage) {
    let root = r#"#usda 1.0
(timeCodesPerSecond = 24)
def Xform "World" (
    clips = { dictionary default = {
        asset[] assetPaths = [@motion.usda@]
        string primPath = "/Clip"
        double2[] active = [(0,0)]
        double2[] times = [(0,0),(2,2)]
    }}
    prepend apiSchemas = ["SemanticsLabelsAPI:tags"]
) {
    uniform token[] xformOpOrder = ["xformOp:translate"]
    double3 xformOp:translate
    token[] semantics:labels:tags
    def Xform "Spline" {
        uniform token[] xformOpOrder = ["xformOp:rotateZ"]
        double xformOp:rotateZ
    }
    def Points "Points" {
        point3f[] points
        vector3f[] velocities
    }
    def SkelAnimation "Animation" {
        uniform token[] joints = ["root"]
        float3[] translations
        quatf[] rotations = [(1,0,0,0)]
        half3[] scales = [(1,1,1)]
    }
}"#;
    let clip = r#"#usda 1.0
def "Clip" {
    double3 xformOp:translate.timeSamples = {0:(0,0,0), 2:(20,0,0)}
    token[] semantics:labels:tags.timeSamples = {0:["warm"], 2:["cool"]}
    def "Spline" {
        double xformOp:rotateZ.spline = {0:0; post linear, 2:90; post held}
    }
    def "Points" {
        point3f[] points.timeSamples = {0:[(0,0,0)], 2:[(20,0,0)]}
        vector3f[] velocities.timeSamples = {0:[(24,0,0)], 2:[(48,0,0)]}
    }
    def "Animation" {
        float3[] translations.timeSamples = {0:[(0,0,0)], 2:[(20,0,0)]}
    }
}"#;
    let mut store = InMemoryStore::default();
    for (id, text) in [(LayerId(1), root), (LayerId(2), clip)] {
        let parsed = layerstack_usda::parser::parse(text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let emitted = layerstack_usda::emit::emit(
            &parsed.layer,
            id,
            &mut store.tokens,
            &mut store.paths,
            &mut NoArcs,
        );
        assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
        store.insert_layer(emitted.layer);
    }
    store.insert_asset_layer(LayerId(1), "motion.usda", LayerId(2));
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    assert!(stage.clip_issues().is_empty(), "{:?}", stage.clip_issues());
    (store, stage)
}
#[test]
fn transform_sample_discovery_and_retained_cache_follow_clips() {
    let (mut store, stage) = scene();
    let world = store.path("/World");
    let scene = Scene::new(&stage, &store);
    let xform = Xformable::new(&scene, world).unwrap();
    assert!(xform.transform_might_be_time_varying());
    assert_eq!(xform.transform_time_samples(), [0., 2.]);
    let mut cache = XformCache::new(Time::at(0.));
    assert_eq!(cache.local_to_world(&scene, world).unwrap()[3][0], 0.);
    cache.set_time(Time::at(1.));
    assert_eq!(cache.local_to_world(&scene, world).unwrap()[3][0], 10.);
}
#[test]
fn point_motion_anchors_clipped_points_and_velocities_together() {
    let (mut store, stage) = scene();
    let path = store.path("/World/Points");
    let scene = Scene::new(&stage, &store);
    let points = PointBased::new(&scene, path).unwrap();
    let inputs = points.motion_inputs(Time::at(1.)).unwrap();
    assert_eq!(inputs.points, [[0., 0., 0.]]);
    assert_eq!(inputs.velocities, [[24., 0., 0.]]);
    assert_eq!(inputs.velocity_sample, Some(0.));
    assert_eq!(
        points
            .compute_points_at_time(Time::at(1.), Time::at(1.))
            .unwrap(),
        [[1., 0., 0.]]
    );
}
#[test]
fn skeleton_sample_discovery_reads_inherited_clip_grids() {
    let (mut store, stage) = scene();
    let path = store.path("/World/Animation");
    let scene = Scene::new(&stage, &store);
    let animation = SkelAnimation::new(&scene, path).unwrap();
    assert_eq!(animation.joint_transform_time_samples(), [0., 2.]);
    assert!(animation.joint_transforms_might_be_time_varying());
    assert_eq!(
        animation
            .compute_joint_local_transform_components(Time::at(1.))
            .unwrap()
            .unwrap()
            .translations,
        [[10., 0., 0.]]
    );
}
#[test]
fn label_interval_includes_clip_switches() {
    let (mut store, stage) = scene();
    let world = store.path("/World");
    let scene = Scene::new(&stage, &store);
    let mut labels = LabelsQuery::in_interval(
        scene,
        "tags",
        LabelInterval::new(0., 2., true, true).unwrap(),
    )
    .unwrap();
    assert_eq!(labels.direct_labels(world), ["cool", "warm"]);
}

#[test]
fn spline_clip_variability_invalidates_retained_transforms_without_sample_knots() {
    let (mut store, stage) = scene();
    let path = store.path("/World/Spline");
    let scene = Scene::new(&stage, &store);
    let xform = Xformable::new(&scene, path).unwrap();
    assert!(xform.transform_time_samples().is_empty());
    assert!(xform.transform_might_be_time_varying());
    let mut cache = XformCache::new(Time::at(0.));
    assert_eq!(cache.local_to_world(&scene, path).unwrap()[0][0], 1.);
    cache.set_time(Time::at(1.));
    let value = cache.local_to_world(&scene, path).unwrap()[0][0];
    assert!(
        (value - 0.5_f64.sqrt()).abs() < 1e-12,
        "rotation at one: {value}"
    );
}

#[test]
fn invalid_clip_queries_remain_masked_and_report_the_evaluation_error() {
    let (mut store, stage) = scene();
    let property = store.property_path("/World.xformOp:translate");
    for time in [f64::INFINITY, f64::NAN] {
        assert_eq!(
            stage.property_clip_evaluation_error(
                property.prim_path(),
                property.property(),
                time,
                layerstack::InterpolationType::Held
            ),
            Some(layerstack::value_clips::ClipEvalError::InvalidQuery)
        );
        assert!(
            stage
                .resolve_property_path_at_time(property, time, layerstack::InterpolationType::Held)
                .is_none()
        );
        let explained = stage
            .explain_property_value_at_time(property, time, layerstack::InterpolationType::Held)
            .unwrap();
        assert_eq!(explained.source, layerstack::ValueSource::ValueClips);
        assert!(explained.value.is_none());
    }
}
