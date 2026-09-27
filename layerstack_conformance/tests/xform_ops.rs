// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authoring transform ops against OpenUSD.
//!
//! The test authors a scene through `XformableEdit`'s op API alone: every
//! convenience, suffixed and inverse ops, each precision, defaults and time
//! samples, `set_reset_xform_stack` both ways and `clear_xform_op_order`,
//! with one `SchemaEdit` applied to a `LiveStage`. The saved layer is
//! `fixtures/xform_ops/scene.usda`; undo and redo restore it exactly.
//! `scripts/xform_ops_oracle.py` records in `oracle.json` what OpenUSD
//! 26.08 reads from it (`GetOrderedXformOps`, with each op's type,
//! precision and inversion, whether the prim resets the transform stack,
//! and `GetLocalTransformation` at the default time and at time 5), and
//! the contract of every authoring call the op API has: each call in the
//! oracle's list, made alone on a fresh copy of the scene with OpenUSD's own
//! API (`AddXformOp`, `GetXformOp` and `UsdGeomXformOp::Set` on forward and
//! inverse ops, `ClearXformOpOrder`, `SetResetXformStack` on and off), is
//! accepted or rejected, and leaves the prim's `xformOpOrder` and
//! `xformOp:*` attributes (types, defaults, time samples) as OpenUSD's call
//! does. The op API makes the same calls on the authored stage, one at a
//! time, undoing each; a rejected call authors nothing. Everything must
//! agree.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::edit::EditTarget;
use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, PathId, StageOptions};
use layerstack::{PropertySpec, Value};
use layerstack_conformance::matrices::agree;
use layerstack_schemas::usd_geom::{Xform, Xformable, XformableEdit};
use layerstack_schemas::{
    Scene, SchemaEdit, Time, XformOpError, XformOpPrecision as P, XformOpType as T, XformOpValue,
};
use serde::Deserialize;
use serde_json::{Value as Json, json};

const SCENE: &str = include_str!("../fixtures/xform_ops/scene.usda");
const ORACLE: &str = include_str!("../fixtures/xform_ops/oracle.json");

const PRIMS: [&str; 6] = [
    "/Pivoted",
    "/Oriented",
    "/Matrix",
    "/Reset",
    "/Unreset",
    "/Cleared",
];

fn stage() -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut store, LayerId(1), options);
    (store, live)
}

/// Authors the scene through the op API.
fn author(edit: &mut SchemaEdit<'_>, paths: &BTreeMap<&str, PathId>) {
    let xform = |edit: &mut SchemaEdit<'_>, name: &str| -> XformableEdit {
        Xform::define(edit, paths[name]);
        XformableEdit::new(edit, paths[name]).expect("defined")
    };

    let pivoted = xform(edit, "/Pivoted");
    pivoted
        .add_translate_op(edit, P::Double)
        .expect("added")
        .set(edit, [1.0, 2.0, 3.0])
        .expect("a vector");
    pivoted
        .add_op(edit, T::Translate, P::Float, Some("pivot"), false)
        .expect("added")
        .set(edit, [0.5, 0.0, -0.5])
        .expect("a vector");
    let rotate = pivoted.add_rotate_xyz_op(edit, P::Float).expect("added");
    rotate.set(edit, [10.0, 20.0, 30.0]).expect("a vector");
    rotate.set_at(edit, 0.0, [0.0, 0.0, 0.0]).expect("a vector");
    rotate
        .set_at(edit, 10.0, [90.0, 0.0, 45.0])
        .expect("a vector");
    pivoted
        .add_scale_op(edit, P::Half)
        .expect("added")
        .set(edit, [2.0, 2.0, 0.5])
        .expect("a vector");
    // The pivot's attribute exists: the inverse op keeps its precision.
    let inverse = pivoted
        .add_op(edit, T::Translate, P::Double, Some("pivot"), true)
        .expect("added");
    assert_eq!(inverse.precision(), P::Float, "the existing attribute's");
    assert_eq!(
        inverse.name(),
        "!invert!xformOp:translate:pivot",
        "the inverse op's name"
    );
    // A duplicate, a value of the wrong kind and any value for an inverse
    // op are refused, and author nothing.
    let unchanged = edit.transaction().clone();
    assert_eq!(
        pivoted.add_translate_op(edit, P::Float),
        Err(XformOpError::AlreadyInOrder {
            op: "xformOp:translate".into()
        }),
        "a duplicate is refused"
    );
    assert_eq!(
        rotate.set(edit, 2.0).err(),
        Some(XformOpError::ValueKind {
            op_type: T::RotateXyz
        }),
        "a rotateXYZ takes a vector"
    );
    for result in [
        inverse.set(edit, [9.0, 9.0, 9.0]).err(),
        inverse.set_at(edit, 5.0, [9.0, 9.0, 9.0]).err(),
    ] {
        assert_eq!(
            result,
            Some(XformOpError::InverseOp {
                op: "!invert!xformOp:translate:pivot".into()
            }),
            "an inverse op's value is its forward op's"
        );
    }
    assert_eq!(
        *edit.transaction(),
        unchanged,
        "nothing refused is authored"
    );

    let oriented = xform(edit, "/Oriented");
    oriented
        .add_orient_op(edit, P::Float)
        .expect("added")
        .set(
            edit,
            XformOpValue::Orientation([0.0, 0.70710677, 0.0, 0.70710677]),
        )
        .expect("a quaternion");
    let tilt = oriented
        .add_op(edit, T::RotateX, P::Half, Some("tilt"), false)
        .expect("added");
    tilt.set(edit, 30.0).expect("a scalar");
    tilt.set_at(edit, 10.0, 60.0).expect("a scalar");

    let matrix = xform(edit, "/Matrix");
    matrix
        .add_transform_op(edit)
        .expect("added")
        .set(
            edit,
            [
                [0.0, 1.0, 0.0, 0.0],
                [-1.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 2.0, 0.0],
                [3.0, 4.0, 5.0, 1.0],
            ],
        )
        .expect("a matrix");
    matrix
        .add_op(edit, T::ScaleZ, P::Double, None, false)
        .expect("added")
        .set(edit, 4.0)
        .expect("a scalar");
    // The order lists only the inverse, whose value is the forward op's.
    matrix.clear_xform_op_order(edit);
    matrix.add_transform_op(edit).expect("added again");
    matrix
        .add_op(edit, T::ScaleZ, P::Double, None, true)
        .expect("added");

    let reset = xform(edit, "/Reset");
    reset
        .add_translate_op(edit, P::Double)
        .expect("added")
        .set(edit, [0.0, 7.0, 0.0])
        .expect("a vector");
    reset.set_reset_xform_stack(edit, true);
    // Asking again changes nothing.
    reset.set_reset_xform_stack(edit, true);

    let unreset = xform(edit, "/Unreset");
    unreset
        .add_translate_op(edit, P::Float)
        .expect("added")
        .set(edit, [1.0, 0.0, 0.0])
        .expect("a vector");
    unreset.set_reset_xform_stack(edit, true);
    unreset
        .add_scale_op(edit, P::Float)
        .expect("added")
        .set(edit, [3.0, 3.0, 3.0])
        .expect("a vector");
    unreset.set_reset_xform_stack(edit, false);

    let cleared = xform(edit, "/Cleared");
    cleared
        .add_translate_op(edit, P::Double)
        .expect("added")
        .set(edit, [9.0, 9.0, 9.0])
        .expect("a vector");
    cleared.clear_xform_op_order(edit);
}

/// The authored stage, its prims' paths, what applying the edit did, and
/// the layer before it.
fn authored() -> (
    InMemoryStore,
    LiveStage,
    BTreeMap<&'static str, PathId>,
    layerstack::Applied,
    Layer,
) {
    let (mut store, mut live) = stage();
    let paths: BTreeMap<&str, PathId> = PRIMS.iter().map(|p| (*p, store.path(p))).collect();
    let before = store.layers[&LayerId(1)].clone();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    author(&mut edit, &paths);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).expect("applies");
    (store, live, paths, applied, before)
}

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    prims: BTreeMap<String, PrimRecord>,
    calls: Vec<CallRecord>,
}

#[derive(Deserialize)]
struct PrimRecord {
    ops: Vec<OpRecord>,
    resets_xform_stack: bool,
    local: BTreeMap<String, [[f64; 4]; 4]>,
}

#[derive(Deserialize, PartialEq, Debug)]
struct OpRecord {
    name: String,
    op_type: String,
    precision: String,
    inverse: bool,
}

/// One authoring call, as OpenUSD made it.
#[derive(Deserialize)]
struct CallRecord {
    call: Vec<Json>,
    accepted: bool,
    op_precision: Option<String>,
    after: Json,
}

fn precision(text: &str) -> P {
    match text {
        "double" => P::Double,
        "float" => P::Float,
        "half" => P::Half,
        other => panic!("no precision {other}"),
    }
}

fn precision_name(precision: P) -> &'static str {
    match precision {
        P::Double => "double",
        P::Float => "float",
        P::Half => "half",
    }
}

/// A call's value as the op API takes it.
fn op_value(json: &Json) -> XformOpValue {
    if let Some(v) = json.as_f64() {
        return XformOpValue::Scalar(v);
    }
    let items = json.as_array().expect("a value");
    let floats = |items: &[Json]| -> Vec<f64> {
        items
            .iter()
            .map(|x| x.as_f64().expect("a number"))
            .collect()
    };
    if items[0].is_array() {
        let rows: Vec<Vec<f64>> = items
            .iter()
            .map(|row| floats(row.as_array().expect("a row")))
            .collect();
        return XformOpValue::Matrix(core::array::from_fn(|i| {
            core::array::from_fn(|j| rows[i][j])
        }));
    }
    let v = floats(items);
    match v.len() {
        4 => XformOpValue::Orientation([v[0], v[1], v[2], v[3]]),
        _ => XformOpValue::Vector([v[0], v[1], v[2]]),
    }
}

/// A value as the oracle records it.
fn canon(value: &Value, tokens: &layerstack::TokenInterner) -> Json {
    let half = |b: u16| f64::from(layerstack::half::to_f32(b));
    match value {
        Value::Double(v) => json!(v),
        Value::Float(v) => json!(f64::from(*v)),
        Value::Half(b) => json!(half(*b)),
        Value::Vec3d(v) => json!(v),
        Value::Vec3f(v) => json!(v.map(f64::from)),
        Value::Vec3h(v) => json!(v.map(half)),
        Value::Quatd(q) => json!(q),
        Value::Quatf(q) => json!(q.map(f64::from)),
        Value::Quath(q) => json!(q.map(half)),
        Value::Matrix4d(m) => json!(
            (0..4)
                .map(|i| m[i * 4..i * 4 + 4].to_vec())
                .collect::<Vec<_>>()
        ),
        Value::Token(t) => json!(tokens.resolve(*t)),
        Value::Array(items) => Json::Array(items.iter().map(|v| canon(v, tokens)).collect()),
        other => json!(format!("{other:?}")),
    }
}

/// The prim's `xformOpOrder` and `xformOp:*` attributes in the root layer,
/// as the oracle records them.
fn authored_ops(store: &InMemoryStore, path: PathId) -> Json {
    let tokens = &store.tokens;
    let mut order = Json::Null;
    let mut attributes = serde_json::Map::new();
    if let Some(prim) = store.layers[&LayerId(1)].prims.get(&path) {
        for entry in &prim.properties {
            let name = tokens.resolve(entry.name);
            let spec: &PropertySpec = &entry.spec;
            if name == "xformOpOrder" {
                order = spec
                    .default
                    .as_ref()
                    .map_or(Json::Null, |v| canon(v, tokens));
            } else if name.starts_with("xformOp:") {
                let samples: serde_json::Map<String, Json> = spec
                    .time_samples
                    .iter()
                    .flatten()
                    .map(|(t, v)| (format!("{t:?}"), canon(v, tokens)))
                    .collect();
                attributes.insert(
                    name.into(),
                    json!({
                        "type": spec.type_name.as_ref().map(|t| t.type_name.to_string()),
                        "default": spec.default.as_ref().map(|v| canon(v, tokens)),
                        "samples": samples,
                    }),
                );
            }
        }
    }
    json!({ "order": order, "attributes": attributes })
}

/// Whether two records agree, floats to within 1e-12 of their scale.
fn close(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| close(v, w)))
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| close(v, w))
        }
        (Json::Number(x), Json::Number(y)) => {
            let (x, y) = (
                x.as_f64().unwrap_or(f64::NAN),
                y.as_f64().unwrap_or(f64::NAN),
            );
            (x - y).abs() <= 1e-12 * y.abs().max(1.0)
        }
        _ => a == b,
    }
}

/// The op API authors `scene.usda`; undo and redo restore it exactly.
#[test]
fn the_op_api_authors_the_fixture_scene() {
    let (mut store, mut live, _, applied, before) = authored();
    let text =
        layerstack_usda::save::save_usda(&store.layers[&LayerId(1)], &store.tokens, &store.paths)
            .expect("saves");
    if text != SCENE {
        let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("xform_ops.usda");
        std::fs::write(&out, &text).expect("writes");
        panic!(
            "the op API no longer authors fixtures/xform_ops/scene.usda; the new scene is {}: copy \
             it there and rerun scripts/xform_ops_oracle.py",
            out.display()
        );
    }
    let after = store.layers[&LayerId(1)].clone();
    let redo = live
        .apply(&mut store, &applied.inverse)
        .expect("undoes")
        .inverse;
    assert_eq!(store.layers[&LayerId(1)], before);
    live.apply(&mut store, &redo).expect("redoes");
    assert_eq!(store.layers[&LayerId(1)], after);
}

/// OpenUSD reads the authored ops and local transforms as the views do,
/// and `AddXformOp` accepts, refuses and types each further op as
/// `add_op` does.
#[test]
fn authored_ops_read_and_add_as_in_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, mut live, paths, _, _) = authored();
    let mut failures = Vec::new();
    {
        let scene = Scene::new(live.stage(), &store);
        assert_eq!(oracle.prims.len(), PRIMS.len());
        for (text, record) in &oracle.prims {
            let xformable = Xformable::new(&scene, paths[text.as_str()]).expect("an Xform");
            let ordered = xformable.ordered_xform_ops();
            let ops: Vec<OpRecord> = ordered
                .ops
                .iter()
                .map(|op| {
                    let ty = scene
                        .stage()
                        .resolve_property_declaration(
                            paths[text.as_str()],
                            store.tokens.lookup(op.attribute).expect("interned"),
                        )
                        .and_then(|d| d.type_name)
                        .expect("a typed attribute");
                    OpRecord {
                        name: op.name.into(),
                        op_type: op.op_type.expect("known").as_str().into(),
                        precision: precision_name(
                            layerstack_schemas::XformOpPrecision::of_type(&ty.type_name)
                                .expect("an op type"),
                        )
                        .into(),
                        inverse: op.inverse,
                    }
                })
                .collect();
            if ops != record.ops {
                failures.push(format!("{text}: ops {ops:?}, OpenUSD {:?}", record.ops));
            }
            if ordered.resets_xform_stack != record.resets_xform_stack {
                failures.push(format!("{text}: resets {}", ordered.resets_xform_stack));
            }
            for (key, expected) in &record.local {
                let time = if key == "default" {
                    Time::Default
                } else {
                    Time::at(key.parse().expect("a time code"))
                };
                let local = xformable.local_transform(time);
                if !agree(&local.matrix, expected) {
                    failures.push(format!(
                        "{text} {key}: {:?}, OpenUSD {expected:?}",
                        local.matrix
                    ));
                }
            }
        }
    }

    // Each call, made alone on the authored scene.
    assert_eq!(oracle.calls.len(), 29, "every recorded call");
    for record in &oracle.calls {
        let call = &record.call;
        let kind = call[0].as_str().expect("a kind");
        let path = paths[call[1].as_str().expect("a prim")];
        let text = |i: usize| call[i].as_str().expect("text");
        let suffix = |i: usize| Some(text(i)).filter(|s| !s.is_empty());
        let context = format!("{call:?}");
        let before = store.layers[&LayerId(1)].clone();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let handle = XformableEdit::new(&edit, path).expect("on the stage");
        let untouched = edit.transaction().clone();
        let (accepted, op_precision) = match kind {
            "add" => match handle.add_op(
                &mut edit,
                T::from_token(text(2)).expect("an op type"),
                precision(text(3)),
                suffix(4),
                call[5].as_bool().expect("inverse"),
            ) {
                Ok(op) => (true, Some(precision_name(op.precision()))),
                Err(_) => (false, None),
            },
            "set" => {
                let op = handle.get_op(
                    &mut edit,
                    T::from_token(text(2)).expect("an op type"),
                    suffix(3),
                    call[4].as_bool().expect("inverse"),
                );
                let value = op_value(&call[5]);
                let result = op.map(|op| match call[6].as_f64() {
                    None => op.set(&mut edit, value).map(|_| ()),
                    Some(time) => op.set_at(&mut edit, time, value).map(|_| ()),
                });
                (matches!(result, Some(Ok(()))), None)
            }
            "clear" => {
                handle.clear_xform_op_order(&mut edit);
                (true, None)
            }
            "reset" => {
                handle.set_reset_xform_stack(&mut edit, call[2].as_bool().expect("on"));
                (true, None)
            }
            other => panic!("no call {other}"),
        };
        if !accepted && *edit.transaction() != untouched {
            failures.push(format!("{context}: a rejected call authored something"));
        }
        if accepted != record.accepted {
            failures.push(format!(
                "{context}: accepted {accepted}, OpenUSD {}",
                record.accepted
            ));
        }
        if op_precision != record.op_precision.as_deref() {
            failures.push(format!(
                "{context}: precision {op_precision:?}, OpenUSD {:?}",
                record.op_precision
            ));
        }
        let transaction = edit.finish();
        let applied = live.apply(&mut store, &transaction).expect("applies");
        let after = authored_ops(&store, path);
        if !close(&after, &record.after) {
            failures.push(format!(
                "{context}: leaves {after}, OpenUSD {}",
                record.after
            ));
        }
        live.apply(&mut store, &applied.inverse).expect("undoes");
        assert_eq!(
            store.layers[&LayerId(1)].prims,
            before.prims,
            "{context}: undone"
        );
    }
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
