// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Model extents hints agree with C++: computed, read and authored.
//!
//! `fixtures/extents_hint/oracle.json` is what
//! `scripts/extents_hint_oracle.py` records with OpenUSD's
//! `UsdGeomModelAPI::ComputeExtentsHint` (per purpose, with and without
//! hint-reading bounds caches, at two times), `GetExtentsHint` and
//! `SetExtentsHint`, each set alone on a fresh copy of the scene.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{EditTarget, LayerId, PrimSpec};
use layerstack_schemas::{
    PrimView, Scene, SchemaEdit, Time,
    bounds::{BoundsCache, BoundsOptions, Extent},
    usd_geom::{GeomModelApi, GeomModelApiEdit, ImageablePurpose},
    value::read_float3_array,
};
use serde::Deserialize;
use std::collections::BTreeMap;

const SCENE: &str = include_str!("../fixtures/extents_hint/scene.usda");

/// Computed hints by case (`<time>_<hints|plain>`), as flat corner lists.
type Computed = BTreeMap<String, Option<Vec<[f64; 3]>>>;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    computed: BTreeMap<String, Computed>,
    authored: BTreeMap<String, Vec<[f64; 3]>>,
    sets: Vec<Set>,
}
#[derive(Deserialize)]
struct Set {
    set: (String, usize, Option<f64>),
    result: Option<bool>,
    after: Option<Authored>,
}
#[derive(Debug, Deserialize, PartialEq)]
struct Authored {
    #[serde(rename = "type")]
    type_name: String,
    custom: bool,
    default: Option<Vec<[f64; 3]>>,
    samples: BTreeMap<String, Vec<[f64; 3]>>,
}

fn oracle() -> Oracle {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/extents_hint/oracle.json")).unwrap();
    assert_eq!(
        oracle.openusd_version,
        layerstack_schemas::OPENUSD_VERSION,
        "the oracle was recorded with this OpenUSD release"
    );
    oracle
}

fn flat(extents: &[Extent]) -> Vec<[f64; 3]> {
    extents
        .iter()
        .flatten()
        .map(|corner| corner.map(f64::from))
        .collect()
}

#[test]
fn computed_hints_match_cpp_per_purpose_time_and_policy() {
    let oracle = oracle();
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for (path, cases) in &oracle.computed {
        for (label, expected) in cases {
            let (time, hints) = label.split_once('_').unwrap();
            let time = if time == "default" {
                Time::Default
            } else {
                Time::at(time.parse().unwrap())
            };
            let mut cache = BoundsCache::new(
                time,
                BoundsOptions {
                    included_purposes: vec![ImageablePurpose::Default],
                    use_extents_hint: hints == "hints",
                    ..BoundsOptions::default()
                },
            );
            let actual = cache
                .compute_extents_hint(&scene, store.path(path))
                .unwrap_or_else(|error| panic!("{path} {label}: {error:?}"));
            assert_eq!(Some(flat(&actual)), *expected, "{path} {label}: {actual:?}");
        }
    }
}

#[test]
fn authored_hints_read_as_cpp_reads_them() {
    let oracle = oracle();
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for (path, expected) in &oracle.authored {
        let model = GeomModelApi::from_prim_unchecked(PrimView::new(scene, store.path(path)));
        let actual = model.extents_hint(Time::Default);
        assert_eq!(
            actual.as_deref().map(flat).unwrap_or_default(),
            *expected,
            "{path}"
        );
    }
}

fn entries(count: usize) -> Vec<[f32; 3]> {
    (0..count)
        .map(|i| {
            #[allow(clippy::cast_precision_loss, reason = "small indices")]
            let i = i as f32;
            [i, -i, 0.5 * i]
        })
        .collect()
}

#[test]
fn hint_authoring_accepts_refuses_and_authors_as_cpp() {
    let oracle = oracle();
    let mut replayed = 0;
    for record in &oracle.sets {
        let (path, count, time) = &record.set;
        if count % 2 == 1 {
            // An odd number of corners has no extent-pair form to pass.
            assert_eq!(record.result, None, "OpenUSD refuses odd counts too");
            continue;
        }
        replayed += 1;
        let (mut store, mut live) = support::scene(SCENE);
        let path = store.path(path);
        let extents: Vec<Extent> = entries(*count)
            .chunks(2)
            .map(|pair| [pair[0], pair[1]])
            .collect();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let model = GeomModelApiEdit::new(&edit, path)
            .expect("outside any instance")
            .unwrap();
        let result = match time {
            None => model.set_extents_hint(&mut edit, &extents),
            Some(time) => model.set_extents_hint_at(&mut edit, *time, &extents),
        };
        assert_eq!(
            result.is_ok(),
            record.result == Some(true),
            "{:?}",
            record.set
        );
        let transaction = edit.finish();
        if result.is_err() {
            assert!(
                transaction.is_empty(),
                "{:?}: a refusal authors nothing",
                record.set
            );
            continue;
        }
        live.apply(&mut store, &transaction).unwrap();
        let token = store.tokens.lookup("extentsHint").unwrap();
        let spec: &PrimSpec = &store.layer(LayerId(1)).unwrap().prims()[&path];
        let property = spec.property(token).unwrap();
        let read = |value| {
            read_float3_array(value, &store.tokens)
                .unwrap()
                .into_iter()
                .map(|corner| corner.map(f64::from))
                .collect::<Vec<_>>()
        };
        let actual = Authored {
            type_name: property.type_name.as_ref().unwrap().type_name().into(),
            custom: property.custom,
            default: property.default.as_ref().map(read),
            samples: property
                .time_samples
                .iter()
                .flat_map(|samples| samples.iter())
                .map(|(time, value)| (format!("{time:?}"), read(value)))
                .collect(),
        };
        assert_eq!(Some(actual), record.after, "{:?}", record.set);
    }
    assert_eq!(replayed, 6, "every representable call was replayed");
}
