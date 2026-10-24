// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The common transform API agrees with C++ call by call.
//!
//! `fixtures/common_xform_api/oracle.json` is what
//! `scripts/common_xform_api_oracle.py` records with OpenUSD's
//! `UsdGeomXformCommonAPI` on 25 op stacks, each call alone on a fresh copy of the scene. A call OpenUSD
//! accepts must be accepted here and author the same layer content; a call
//! it refuses (returning `false` or raising a coding error) must be refused
//! here with nothing authored, where OpenUSD can leave ops it added before
//! refusing. Reads compare `GetXformVectors`,
//! `GetXformVectorsByAccumulation` and the compatibility check.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, LiveStage, PathId, PathInterner,
    ResolvedAsset, StageOptions, SublayerEntry, TokenInterner, Value, edit::EditTarget, half,
};
use layerstack_schemas::{
    CommonComponent, CommonTransform, CommonTransformSource, RotationOrder, Scene, SchemaEdit,
    Time, XformOpType,
    usd_geom::{Xformable, XformableEdit},
};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::{collections::BTreeMap, sync::Arc};

const SCENE: &str = include_str!("../fixtures/common_xform_api/scene.usda");
const WEAK: &str = include_str!("../fixtures/common_xform_api/weak.usda");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    reads: BTreeMap<String, Reads>,
    calls: Vec<Call>,
    rotation_orders: RotationOrders,
}
#[derive(Deserialize)]
struct Reads {
    compatible: bool,
    vectors_default: Vectors,
    vectors_2: Vectors,
    accumulated_default: Vectors,
    accumulated_2: Vectors,
}
#[derive(Debug, Deserialize)]
struct Vectors {
    translation: [f64; 3],
    rotation: [f64; 3],
    scale: [f64; 3],
    pivot: [f64; 3],
    rotation_order: String,
}
#[derive(Deserialize)]
struct Call {
    prim: String,
    call: Vec<Json>,
    result: Option<bool>,
    after: Json,
}
#[derive(Deserialize)]
struct RotationOrders {
    op_types: BTreeMap<String, String>,
    transforms: BTreeMap<String, [[f64; 4]; 4]>,
    convertible: BTreeMap<String, bool>,
}

fn oracle() -> Oracle {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/common_xform_api/oracle.json")).unwrap();
    assert_eq!(
        oracle.openusd_version,
        layerstack_schemas::OPENUSD_VERSION,
        "the oracle was recorded with this OpenUSD release"
    );
    oracle
}

struct NoAssets;
impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// The scene as layer 1 over `weak.usda` as layer 2, its sublayer.
fn scene() -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    for (id, text) in [(LayerId(1), SCENE), (LayerId(2), WEAK)] {
        let parsed = layerstack_usda::parser::parse(text);
        let emitted = layerstack_usda::emit::emit(
            &parsed.layer,
            id,
            &store.tokens,
            &store.paths,
            &mut NoAssets,
        );
        assert!(
            parsed.diagnostics.is_empty() && emitted.diagnostics.is_empty(),
            "{:?} {:?}",
            parsed.diagnostics,
            emitted.diagnostics
        );
        store.insert_layer(emitted.layer);
    }
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .sublayers_mut()
        .push(SublayerEntry::new(LayerId(2)));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&store.tokens))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut store, LayerId(1), options);
    (store, live)
}

fn order(name: &str) -> RotationOrder {
    match name {
        "XYZ" => RotationOrder::Xyz,
        "XZY" => RotationOrder::Xzy,
        "YXZ" => RotationOrder::Yxz,
        "YZX" => RotationOrder::Yzx,
        "ZXY" => RotationOrder::Zxy,
        "ZYX" => RotationOrder::Zyx,
        other => panic!("unknown order {other}"),
    }
}

fn vector<const N: usize>(value: &Json) -> [f64; N] {
    let items = value.as_array().unwrap();
    core::array::from_fn(|i| items[i].as_f64().unwrap())
}

#[allow(clippy::cast_possible_truncation, reason = "float3 values")]
fn float3(value: &Json) -> [f32; 3] {
    vector::<3>(value).map(|c| c as f32)
}

/// `value` as the oracle writes a value: numbers, vectors, quaternions
/// `[i, j, k, r]`, matrices as rows, arrays of them.
fn json_value(value: &Value) -> Json {
    let half3 = |v: &[u16; 3]| json!(v.map(|bits| f64::from(half::to_f32(bits))));
    match value {
        Value::Double(v) => json!(v),
        Value::Float(v) => json!(f64::from(*v)),
        Value::Vec3d(v) => json!(v),
        Value::Vec3f(v) => json!(v.map(f64::from)),
        Value::Vec3h(v) => half3(v),
        Value::Quatf(q) => json!(q.map(f64::from)),
        Value::Quatd(q) => json!(q),
        Value::Matrix4d(m) => json!(
            (0..4)
                .map(|i| m[i * 4..i * 4 + 4].to_vec())
                .collect::<Vec<_>>()
        ),
        Value::Token(_) => panic!("tokens are read as the order"),
        other => match other.array_ref() {
            Some(items) => Json::Array(items.iter().map(|item| json_value(&item)).collect()),
            None => panic!("unexpected value {other:?}"),
        },
    }
}

/// What the layer authors for the prim, as the oracle records it.
fn authored(store: &InMemoryStore, layer: LayerId, path: PathId) -> Json {
    let mut out = json!({"order": null, "attributes": {}});
    let Some(spec) = store.layer(layer).unwrap().prims().get(&path) else {
        return out;
    };
    for entry in &spec.properties {
        let (name, property) = (store.tokens.resolve(entry.name), &*entry.spec);
        if name == "xformOpOrder" {
            out["order"] = property.default.as_ref().map_or(Json::Null, |value| {
                Json::Array(
                    value
                        .array_ref()
                        .unwrap()
                        .iter()
                        .map(|token| match &*token {
                            Value::Token(token) => json!(store.tokens.resolve(*token)),
                            other => panic!("order entry {other:?}"),
                        })
                        .collect(),
                )
            });
        } else if name.starts_with("xformOp:") {
            let samples: serde_json::Map<String, Json> = property
                .time_samples
                .iter()
                .flat_map(|samples| samples.iter())
                .map(|(time, value)| (format!("{time:?}"), json_value(value)))
                .collect();
            out["attributes"][name] = json!({
                "type": property.type_name.as_ref().map(|ty| ty.type_name().to_string()),
                "default": property.default.as_ref().map_or(Json::Null, json_value),
                "samples": samples,
            });
        }
    }
    out
}

fn close(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| close(v, w)))
        }
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| close(a, b))
        }
        (Json::Number(a), Json::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            (a - b).abs() <= 1e-9 * b.abs().max(1.0)
        }
        _ => a == b,
    }
}

/// Makes one oracle call; `true` when it is accepted.
fn make(edit: &mut SchemaEdit<'_>, path: PathId, call: &[Json]) -> bool {
    let xform = XformableEdit::new(edit, path)
        .expect("outside any instance")
        .unwrap();
    let time = |value: &Json| value.as_f64();
    let component = |edit: &mut SchemaEdit<'_>, component, at: &Json| match time(at) {
        None => xform.set_common_component(edit, component).is_ok(),
        Some(t) => xform.set_common_component_at(edit, t, component).is_ok(),
    };
    match call[0].as_str().unwrap() {
        "vectors" => {
            let value = CommonTransform {
                translation: vector(&call[1]),
                rotation: float3(&call[2]),
                scale: float3(&call[3]),
                pivot: float3(&call[4]),
                rotation_order: order(call[5].as_str().unwrap()),
            };
            match time(&call[6]) {
                None => xform.set_common_transform(edit, &value).is_ok(),
                Some(t) => xform.set_common_transform_at(edit, t, &value).is_ok(),
            }
        }
        "translate" => component(edit, CommonComponent::Translate(vector(&call[1])), &call[2]),
        "rotate" => component(
            edit,
            CommonComponent::Rotate {
                degrees: float3(&call[1]),
                order: order(call[2].as_str().unwrap()),
            },
            &call[3],
        ),
        "scale" => component(edit, CommonComponent::Scale(float3(&call[1])), &call[2]),
        "pivot" => component(edit, CommonComponent::Pivot(float3(&call[1])), &call[2]),
        other => panic!("unknown call {other}"),
    }
}

#[test]
fn authoring_accepts_and_refuses_as_cpp_and_authors_what_it_authors() {
    let oracle = oracle();
    let (mut accepted_calls, mut refused_calls) = (0, 0);
    for record in &oracle.calls {
        let (mut store, mut live) = scene();
        let path = store.path(&record.prim);
        let layer = LayerId(1);
        let before = authored(&store, layer, path);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(layer));
        let accepted = make(&mut edit, path, &record.call);
        let what = format!("{} {:?}", record.prim, record.call);
        assert_eq!(accepted, record.result == Some(true), "{what}");
        let transaction = edit.finish();
        if !accepted {
            assert!(transaction.is_empty(), "{what}: a refusal authors nothing");
            refused_calls += 1;
            continue;
        }
        accepted_calls += 1;
        live.apply(&mut store, &transaction).unwrap();
        let after = authored(&store, layer, path);
        assert!(
            close(&after, &record.after),
            "{what}:\n{after:#}\nagainst\n{:#}\n(before {before:#})",
            record.after
        );
    }
    assert_eq!(
        (accepted_calls, refused_calls),
        (76, 124),
        "every recorded call was replayed"
    );
}

fn agree(actual: &CommonTransform, expected: &Vectors, what: &str) {
    let near = |a: f64, b: f64| (a - b).abs() <= 1e-5 * b.abs().max(1.0);
    let all = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(&a, b)| near(a, b));
    let wide = |v: [f32; 3]| v.map(f64::from);
    assert!(
        all(actual.translation, expected.translation)
            && all(wide(actual.rotation), expected.rotation)
            && all(wide(actual.scale), expected.scale)
            && all(wide(actual.pivot), expected.pivot)
            && actual.rotation_order == order(&expected.rotation_order),
        "{what}: {actual:?} against {expected:?}"
    );
}

#[test]
fn reads_match_cpp_with_their_source() {
    let oracle = oracle();
    let (store, live) = scene();
    let scene = Scene::new(live.stage(), &store);
    for (path, expected) in &oracle.reads {
        let prim = Xformable::new(&scene, store.path(path)).unwrap();
        assert_eq!(
            prim.is_common_transform_compatible(),
            expected.compatible,
            "{path}"
        );
        for (time, vectors, accumulated) in [
            (
                Time::Default,
                &expected.vectors_default,
                &expected.accumulated_default,
            ),
            (Time::at(2.0), &expected.vectors_2, &expected.accumulated_2),
        ] {
            let reading = prim.common_transform(time);
            agree(&reading.value, vectors, &format!("{path} {time:?}"));
            assert_eq!(
                reading.source,
                if expected.compatible {
                    CommonTransformSource::Ops
                } else {
                    CommonTransformSource::Decomposed
                },
                "{path}"
            );
            let reading = prim.common_transform_by_accumulation(time);
            agree(
                &reading.value,
                accumulated,
                &format!("{path} {time:?} accumulated"),
            );
        }
    }
    let reduced = Xformable::new(&scene, store.path("/Pivots")).unwrap();
    assert_eq!(
        reduced
            .common_transform_by_accumulation(Time::Default)
            .source,
        CommonTransformSource::Accumulated,
        "paired rotation and scale pivots reduce"
    );
}

#[test]
fn rotation_orders_match_cpp() {
    let oracle = oracle();
    let types: BTreeMap<&str, XformOpType> = [
        "translate",
        "scale",
        "rotateX",
        "rotateY",
        "rotateZ",
        "rotateXYZ",
        "rotateXZY",
        "rotateYXZ",
        "rotateYZX",
        "rotateZXY",
        "rotateZYX",
        "orient",
        "transform",
        "translateX",
        "scaleX",
    ]
    .into_iter()
    .map(|name| (name, XformOpType::from_token(name).unwrap()))
    .collect();
    for (name, convertible) in &oracle.rotation_orders.convertible {
        assert_eq!(
            RotationOrder::from_op_type(types[name.as_str()]).is_some(),
            *convertible,
            "{name}"
        );
    }
    for (name, op_type) in &oracle.rotation_orders.op_types {
        assert_eq!(order(name).op_type().as_str(), op_type, "{name}");
        let actual = order(name).rotation_matrix([10.0, 20.0, 30.0]);
        let expected = oracle.rotation_orders.transforms[name];
        assert!(
            actual
                .iter()
                .flatten()
                .zip(expected.iter().flatten())
                .all(|(a, b)| (a - b).abs() <= 1e-12),
            "{name}"
        );
    }
}
