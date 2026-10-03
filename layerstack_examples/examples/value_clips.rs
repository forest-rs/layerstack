// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Author a clip bundle, satisfy its explicit asset requests, and query it.
//! Hosts can replace the in-memory catalog with their own asynchronous loader.

use layerstack::{
    InMemoryStore, InterpolationType, Layer, LayerId, LayerOffset, PrimSpec, PropertyPath,
    PropertySpec, PropertyType, Stage, StageOptions, Value,
    clip_authoring::{ClipBundleOptions, ClipSource, stitch_clip_sequence},
    value_clips::{ClipAssetStatus, ClipAssetUnavailable},
};

fn main() {
    let mut store = InMemoryStore::default();
    let model = store.path("/Model");
    let height = store.tokens.intern("height");
    let mut clip = Layer::new(LayerId(10));
    for (name, value) in [("startTimeCode", 0.), ("endTimeCode", 10.)] {
        clip.metadata.push(layerstack::FieldEntry {
            name: store.tokens.intern(name),
            value: Value::Double(value).into(),
        });
    }
    let mut spec = PrimSpec::def();
    spec.set_property(
        height,
        PropertySpec {
            type_name: Some(PropertyType::new("double", false, Value::Double(0.))),
            time_samples: Some(vec![(0., Value::Double(0.)), (10., Value::Double(100.))].into()),
            ..PropertySpec::attribute()
        },
    );
    clip.insert_prim(model, spec);
    let options = ClipBundleOptions {
        root_id: LayerId(1),
        topology_id: LayerId(2),
        manifest_id: LayerId(3),
        clip_prim_path: model,
        clip_set: "default".into(),
        topology_asset_path: "motion.topology.usda".into(),
        manifest_asset_path: "motion.manifest.usda".into(),
        start_time: None,
        end_time: None,
    };
    let bundle = stitch_clip_sequence(
        &[ClipSource {
            layer: &clip,
            asset_path: "motion.usda",
            offset: LayerOffset::IDENTITY,
        }],
        &options,
        &mut store.tokens,
        &mut store.paths,
    )
    .expect("valid clip sequence");
    store.insert_layer(bundle.root);
    store.insert_layer(bundle.topology);

    // Composition enumerates clip assets without resolving or loading them.
    let pending = Stage::compose(&mut store, options.root_id, StageOptions::default());
    assert!(
        pending.clip_asset_requests().iter().all(|request| {
            request.status == ClipAssetStatus::Unavailable(ClipAssetUnavailable::Unresolved)
        }),
        "preparation must request host resolution"
    );
    assert_eq!(
        pending.clip_asset_requests().len(),
        2,
        "value and manifest assets"
    );

    // An engine owns this catalog and can load requests on its own schedule.
    for request in pending.clip_asset_requests() {
        let layer = match request.identifier.as_ref() {
            "motion.usda" => clip.clone(),
            "motion.manifest.usda" => bundle.manifest.clone(),
            unknown => panic!("unknown requested asset: {unknown}"),
        };
        store.insert_asset_layer(request.anchor, &request.identifier, layer.id);
        store.insert_layer(layer);
    }
    let stage = Stage::compose(&mut store, options.root_id, StageOptions::default());
    assert!(
        stage.clip_issues().is_empty(),
        "the resident bundle must be valid"
    );
    assert!(
        stage
            .clip_asset_requests()
            .iter()
            .all(|request| { matches!(request.status, ClipAssetStatus::Loaded(_)) }),
        "the host must satisfy every clip asset request"
    );
    let at_five = stage
        .resolve_property_path_at_time(
            PropertyPath::new(model, height),
            5.,
            InterpolationType::Linear,
        )
        .expect("sampled height");
    assert_eq!(
        at_five.value,
        Value::Double(50.),
        "linear clip interpolation"
    );
    assert_eq!(
        stage.property_sample_times(model, height),
        [0., 10.],
        "mapped clip endpoints"
    );
    let source = stage
        .property_clip_source(model, height, 5., InterpolationType::Linear)
        .expect("clip sample provenance");
    assert_eq!(source.lower.layer, Some(clip.id), "lower raw clip source");
    assert_eq!(source.upper.layer, Some(clip.id), "upper raw clip source");
    println!(
        "height at 5: {:?}; clip samples: {}..{}",
        at_five.value, source.lower.stage_time, source.upper.stage_time
    );
}
