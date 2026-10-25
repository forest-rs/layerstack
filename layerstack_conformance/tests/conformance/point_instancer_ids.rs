// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Point-instance ID authoring and masks agree with C++ call by call.
//!
//! `fixtures/point_instancer_ids/oracle.json` is what
//! `scripts/point_instancer_ids_oracle.py` records with OpenUSD's own
//! `DeactivateIds`, `ActivateIds`, `ActivateAllIds`, `InvisIds`, `VisIds`,
//! `VisAllIds` and `ComputeMaskAtTime`. Each call is replayed here in order,
//! once applying every call before the next and once collecting them all in
//! one `SchemaEdit`, and the root layer's `inactiveIds` list operation,
//! `invisibleIds` default and samples and the computed masks must match.
use crate::support::schema_scene as support;
use layerstack::{FieldValue, InMemoryStore, LayerId, ListOp, LiveStage, PathId, edit::EditTarget};
use layerstack_schemas::{
    Scene, SchemaEdit, Time,
    usd_geom::{PointInstancer, PointInstancerEdit},
    value::read_int64_array,
};
use serde::Deserialize;
use serde_json::Value as Json;
use std::collections::BTreeMap;

const SCENE: &str = include_str!("../../fixtures/point_instancer_ids/scene.usda");
const MASK_TIMES: [f64; 6] = [0.5, 1.0, 1.5, 2.0, 3.0, 4.0];

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    masks: Vec<MaskRecord>,
    calls: Vec<CallRecord>,
}
#[derive(Deserialize)]
struct MaskRecord {
    query: (String, Option<f64>, Option<Vec<i64>>),
    mask: Vec<bool>,
}
#[derive(Deserialize)]
struct CallRecord {
    call: Vec<Json>,
    accepted: bool,
    inactive: Option<ListOpRecord>,
    invisible: Option<InvisibleRecord>,
    masks: BTreeMap<String, Vec<bool>>,
}
#[derive(Debug, Deserialize, PartialEq)]
struct ListOpRecord {
    explicit: Option<Vec<i64>>,
    prepend: Vec<i64>,
    append: Vec<i64>,
    delete: Vec<i64>,
    add: Vec<i64>,
    reorder: Vec<i64>,
}
#[derive(Debug, Deserialize, PartialEq)]
struct InvisibleRecord {
    default: Option<Vec<i64>>,
    samples: BTreeMap<String, Vec<i64>>,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(include_str!(
        "../../fixtures/point_instancer_ids/oracle.json"
    ))
    .unwrap();
    assert_eq!(
        oracle.openusd_version,
        layerstack_schemas::OPENUSD_VERSION,
        "the oracle was recorded with this OpenUSD release"
    );
    oracle
}

fn ids(value: &Json) -> Vec<i64> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_i64().unwrap())
        .collect()
}

fn time(value: &Json) -> Option<f64> {
    value.as_f64()
}

/// Makes one oracle call through the edit API.
fn make(edit: &mut SchemaEdit<'_>, path: PathId, call: &[Json]) {
    let point = PointInstancerEdit::new(edit, path)
        .expect("outside any instance")
        .unwrap();
    match call[0].as_str().unwrap() {
        "deactivate" => {
            point.deactivate_ids(edit, &ids(&call[2])).unwrap();
        }
        "activate" => {
            point.activate_ids(edit, &ids(&call[2])).unwrap();
        }
        "activate_all" => {
            point.activate_all_ids(edit);
        }
        "invis" => {
            match time(&call[3]) {
                None => point.make_invisible(edit, &ids(&call[2])),
                Some(time) => point.make_invisible_at(edit, time, &ids(&call[2])),
            }
            .unwrap();
        }
        "vis" => {
            match time(&call[3]) {
                None => point.make_visible(edit, &ids(&call[2])),
                Some(time) => point.make_visible_at(edit, time, &ids(&call[2])),
            }
            .unwrap();
        }
        "vis_all" => match time(&call[2]) {
            None => {
                point.make_all_visible(edit);
            }
            Some(time) => {
                point.make_all_visible_at(edit, time);
            }
        },
        other => panic!("unknown call {other}"),
    }
}

fn authored_list_op(store: &InMemoryStore, path: PathId) -> Option<ListOpRecord> {
    let token = store.tokens.lookup("inactiveIds")?;
    let spec = store.layer(LayerId(1)).unwrap().prims().get(&path)?;
    let Some(FieldValue::Int64ListOp(op)) = spec.field(token) else {
        return None;
    };
    let ListOp {
        explicit,
        prepend,
        append,
        delete,
        add,
        reorder,
    } = (**op).clone();
    Some(ListOpRecord {
        explicit,
        prepend,
        append,
        delete,
        add,
        reorder,
    })
}

fn authored_invisible(store: &InMemoryStore, path: PathId) -> Option<InvisibleRecord> {
    let token = store.tokens.lookup("invisibleIds")?;
    let spec = store.layer(LayerId(1)).unwrap().prims().get(&path)?;
    let property = spec.property(token)?;
    let read = |value| read_int64_array(value, &store.tokens).expect("int64[] value");
    Some(InvisibleRecord {
        default: property.default.as_ref().map(read),
        samples: property
            .time_samples
            .get()
            .into_iter()
            .flat_map(|samples| samples.iter())
            .map(|(time, value)| (format!("{time:?}"), read(value)))
            .collect(),
    })
}

fn masks(store: &InMemoryStore, live: &LiveStage, path: PathId) -> BTreeMap<String, Vec<bool>> {
    let scene = Scene::new(live.stage(), store);
    let point = PointInstancer::new(&scene, path).unwrap();
    let mut out = BTreeMap::new();
    out.insert(
        "default".into(),
        point.try_compute_mask(Time::Default).unwrap(),
    );
    for at in MASK_TIMES {
        out.insert(
            format!("{at:?}"),
            point.try_compute_mask(Time::at(at)).unwrap(),
        );
    }
    out
}

fn check(store: &InMemoryStore, live: &LiveStage, record: &CallRecord) {
    let path = store.path(record.call[1].as_str().unwrap());
    assert!(record.accepted, "{:?}", record.call);
    assert_eq!(
        authored_list_op(store, path),
        record.inactive,
        "inactiveIds after {:?}",
        record.call
    );
    assert_eq!(
        authored_invisible(store, path),
        record.invisible,
        "invisibleIds after {:?}",
        record.call
    );
    assert_eq!(
        masks(store, live, path),
        record.masks,
        "masks after {:?}",
        record.call
    );
}

#[test]
fn id_authoring_matches_cpp_call_by_call() {
    let oracle = oracle();
    let (mut store, mut live) = support::scene(SCENE);
    for record in &oracle.calls {
        let path = store.path(record.call[1].as_str().unwrap());
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        make(&mut edit, path, &record.call);
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        check(&store, &live, record);
    }
}

#[test]
fn id_authoring_in_one_edit_sees_its_own_earlier_calls() {
    let oracle = oracle();
    let (mut store, mut live) = support::scene(SCENE);
    let paths: Vec<_> = oracle
        .calls
        .iter()
        .map(|record| store.path(record.call[1].as_str().unwrap()))
        .collect();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    for (record, &path) in oracle.calls.iter().zip(&paths) {
        make(&mut edit, path, &record.call);
    }
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    // The last call on each prim holds its final state.
    let mut last = BTreeMap::new();
    for (record, &path) in oracle.calls.iter().zip(&paths) {
        last.insert(path, record);
    }
    for record in last.values() {
        check(&store, &live, record);
    }
}

#[test]
fn explicit_id_masks_match_cpp() {
    let oracle = oracle();
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for record in &oracle.masks {
        let (path, time, ids) = &record.query;
        let point = PointInstancer::new(&scene, store.path(path)).unwrap();
        let time = time.map_or(Time::Default, Time::at);
        let mask = match ids {
            None => point.try_compute_mask(time).unwrap(),
            Some(ids) => point.try_compute_mask_for_ids(time, ids).unwrap(),
        };
        assert_eq!(mask, record.mask, "{:?}", record.query);
    }
}
