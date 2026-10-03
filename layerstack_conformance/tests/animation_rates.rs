// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Nested time-rate conversion across authoring, evaluation and live edits.
//! AOUSD Core §12.3.2.1; OpenUSD `PcpLayerStack::_BuildLayerStack` and
//! `Pcp_BuildPrimIndex` (reference/payload TCPS scaling).

use layerstack::stage::ResolvedValue;
use layerstack::{InMemoryStore, InterpolationType, LiveStage, Stage, StageOptions, Value};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde_json::{Value as Json, json};
use std::process::Command;

const TIMES: &[f64] = &[
    0.0, 7.0, 10.0, 14.0, 20.0, 24.0, 34.0, 55.0, 74.0, 86.0, 92.0,
];
const PROPERTIES: &[&str] = &[
    "/Local.own",
    "/Local.ownClock",
    "/Internal.own",
    "/Internal.ownClock",
    "/Referenced.rootAnim",
    "/Referenced.rootClock",
    "/Referenced.subAnim",
    "/Referenced.subClock",
    "/Referenced.sparseClock",
    "/Referenced.curve",
    "/Referenced/Nested.nestedAnim",
    "/Referenced/Nested.nestedClock",
    "/Payload.rootAnim",
    "/Payload.rootClock",
    "/Payload.subAnim",
    "/Payload.subClock",
    "/Payload.sparseClock",
    "/Payload.curve",
    "/Payload/Nested.nestedAnim",
    "/Payload/Nested.nestedClock",
];
fn number(value: &Value) -> Json {
    if let Some(array) = value.array_ref() {
        return Json::Array(array.iter().map(|v| number(&v)).collect());
    }
    match value {
        Value::Double(v) | Value::TimeCode(v) => json!(v),
        _ => panic!("unexpected value {value:?}"),
    }
}
fn snapshot(stage: &Stage, store: &mut InMemoryStore) -> Json {
    let mut out = serde_json::Map::new();
    for &path in PROPERTIES {
        let property = store.property_path(path);
        let default = stage
            .resolve_property_path(property)
            .map_or(Json::Null, |v| {
                let ResolvedValue::Scalar(value) = v.value else {
                    panic!("attribute value");
                };
                number(&value)
            });
        let values: Vec<_> = TIMES
            .iter()
            .map(|&time| {
                stage
                    .resolve_property_path_at_time(property, time, InterpolationType::Linear)
                    .map_or(Json::Null, |v| number(&v.value))
            })
            .collect();
        out.insert(path.into(), json!({"default": default, "times": stage.property_sample_times(property.prim_path(), property.property()), "values": values}));
    }
    Json::Object(out)
}
fn close(actual: &Json, expected: &Json) {
    match (actual, expected) {
        (Json::Number(a), Json::Number(b)) => assert!(
            (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() < 1e-10,
            "{actual} != {expected}"
        ),
        (Json::Array(a), Json::Array(b)) => {
            assert_eq!(a.len(), b.len(), "snapshot array length");
            for (a, b) in a.iter().zip(b) {
                close(a, b);
            }
        }
        (Json::Object(a), Json::Object(b)) => {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "snapshot object keys"
            );
            for (key, value) in a {
                close(value, &b[key]);
            }
        }
        _ => assert_eq!(actual, expected, "snapshot value"),
    }
}
#[test]
fn nested_rates_match_openusd_before_and_after_source_edits() {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/flatten/rates_nested/root.usda");
    let mut loaded = load_entry_usda(&path);
    assert!(loaded.invalid.is_empty());
    let mut live = LiveStage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    assert!(live.stage().composition_errors().is_empty());
    let before = snapshot(live.stage(), &mut loaded.store);
    assert_eq!(before["/Referenced.rootAnim"]["times"], json!([10.0, 34.0]));
    close(
        &before["/Referenced.subAnim"]["times"],
        &json!([20.0, 92.0]),
    );
    close(
        &before["/Referenced/Nested.nestedAnim"]["times"],
        &json!([14.0, 86.0]),
    );
    assert_eq!(before["/Referenced.rootClock"]["default"], json!(22.0));
    assert_eq!(before["/Internal.ownClock"]["default"], json!(6.0));

    let key = loaded.store.tokens.intern("timeCodesPerSecond");
    loaded
        .store
        .layers
        .get_mut(&loaded.root_layer)
        .unwrap()
        .set_metadata(key, Value::Double(48.0));
    live.synchronize(&mut loaded.store);
    let root_edited = snapshot(live.stage(), &mut loaded.store);
    assert_eq!(
        root_edited["/Referenced.rootAnim"]["times"],
        json!([13.0, 61.0])
    );
    assert_eq!(root_edited["/Internal.own"]["times"], json!([0.0, 24.0]));
    let library = loaded
        .layer_names
        .iter()
        .find(|(_, name)| name.ends_with("library.usda"))
        .map(|(&id, _)| id)
        .unwrap();
    loaded
        .store
        .layers
        .get_mut(&library)
        .unwrap()
        .set_metadata(key, Value::Double(6.0));
    live.synchronize(&mut loaded.store);
    let library_edited = snapshot(live.stage(), &mut loaded.store);
    assert_eq!(
        library_edited["/Referenced.rootAnim"]["times"],
        json!([13.0, 109.0])
    );

    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("skipped native oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }
    let script = r#"
import json, sys
from pxr import Usd, Gf
stage = Usd.Stage.Open(sys.argv[1])
paths, times = json.loads(sys.argv[2]), json.loads(sys.argv[3])
def number(value):
    if value is None: return None
    if isinstance(value, Gf.TimeCode): return value.GetValue()
    if isinstance(value, (float, int)): return value
    return [number(v) for v in value]
def snapshot():
    out = {}
    for path in paths:
        attr = stage.GetAttributeAtPath(path)
        out[path] = {'default': number(attr.Get()), 'times': attr.GetTimeSamples(), 'values': [number(attr.Get(t)) for t in times]}
    return out
before = snapshot()
stage.GetRootLayer().timeCodesPerSecond = 48
root = snapshot()
library = next(l for l in stage.GetUsedLayers() if l.identifier.endswith('library.usda'))
library.timeCodesPerSecond = 6
print(json.dumps([before, root, snapshot()]))
"#;
    let result = Command::new(&python)
        .arg("-c")
        .arg(script)
        .arg(&path)
        .arg(serde_json::to_string(PROPERTIES).unwrap())
        .arg(serde_json::to_string(TIMES).unwrap())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let expected: Vec<Json> = serde_json::from_slice(&result.stdout).unwrap();
    for (actual, expected) in [before, root_edited, library_edited].iter().zip(&expected) {
        close(actual, expected);
    }
}

#[test]
fn animation_block_distinguishes_weaker_defaults_and_same_site_samples() {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/flatten/animation_block/root.usda");
    let mut loaded = load_entry_usda(&path);
    assert!(
        loaded.invalid.is_empty(),
        "the fixture must import faithfully"
    );
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let prim = loaded.store.path("/P");
    for (name, expected, grid) in [
        ("x", Some(Value::Double(5.)), vec![]),
        ("empty", None, vec![]),
        ("same", Some(Value::Double(150.)), vec![0., 10.]),
    ] {
        let token = loaded.store.tokens.intern(name);
        assert_eq!(
            stage.property_sample_times(prim, token),
            grid,
            "{name} sample inventory"
        );
        assert_eq!(
            stage
                .resolve_property_path_at_time(
                    layerstack::PropertyPath::new(prim, token),
                    5.,
                    InterpolationType::Linear
                )
                .map(|r| r.value),
            expected,
            "{name} numeric value"
        );
    }
}
