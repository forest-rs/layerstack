// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reopen the actual emitted clip bundle in pinned OpenUSD and sample playback.
//! The optional oracle follows the harness's `LAYERSTACK_USD_PYTHON` convention.
//! Clip playback belongs to OpenUSD; this crate only authors interchange layers.

use std::process::Command;

use layerstack::clip_authoring::{
    ClipAuthoringError, ClipBundleOptions, ClipSource, stitch_clip_sequence,
};
use layerstack::{
    FieldEntry, InMemoryStore, Layer, LayerId, LayerOffset, PrimSpec, PropertySpec, PropertyType,
    Value,
};

fn fixture() -> (InMemoryStore, Layer, Layer, ClipBundleOptions) {
    let mut store = InMemoryStore::default();
    let model = store.path("/Model");
    let root = store.path("/");
    let model_name = store.tokens.intern("Model");
    let animated = store.tokens.intern("animated");
    let static_name = store.tokens.intern("static");
    let start = store.tokens.intern("startTimeCode");
    let end = store.tokens.intern("endTimeCode");
    let mut layer = |id, begin, finish, initial, final_value, default, static_value| {
        let mut layer = Layer::new(LayerId(id));
        layer.metadata = vec![
            FieldEntry {
                name: start,
                value: Value::Double(begin).into(),
            },
            FieldEntry {
                name: end,
                value: Value::Double(finish).into(),
            },
        ];
        layer.insert_prim(root, PrimSpec::default().with_children(vec![model_name]));
        layer.insert_prim(
            model,
            PrimSpec::def()
                .with_type_name(store.tokens.intern("Xform"))
                .with_property(
                    animated,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "float",
                        false,
                        Value::Float(0.),
                    ))
                    .with_default(Value::Float(default))
                    .with_time_samples(vec![
                        (begin, Value::Float(initial)),
                        (finish, Value::Float(final_value)),
                    ]),
                )
                .with_property(
                    static_name,
                    PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                        .with_default(Value::Int(static_value)),
                ),
        );
        layer
    };
    let a = layer(1, 0., 10., 0., 10., 7., 42);
    let b = layer(2, 100., 110., 1000., 1010., 9., 99);
    let options = ClipBundleOptions {
        root_id: LayerId(10),
        topology_id: LayerId(11),
        manifest_id: LayerId(12),
        clip_prim_path: model,
        clip_set: "default".into(),
        topology_asset_path: "./root.topology.usda".into(),
        manifest_asset_path: "./root.manifest.usda".into(),
        start_time: None,
        end_time: None,
    };
    (store, a, b, options)
}

#[test]
fn emitted_offset_bundle_plays_in_openusd() {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    let available = Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output();
    if !available.is_ok_and(|out| out.status.success()) {
        eprintln!("skipped: no Python with OpenUSD's pxr (set LAYERSTACK_USD_PYTHON)");
        return;
    }
    let (mut store, a, b, options) = fixture();
    // Asset order deliberately differs from activation order, and A scales time.
    let sources = [
        ClipSource {
            layer: &a,
            asset_path: "./a.usda",
            offset: LayerOffset {
                offset: 20.,
                scale: 2.,
            },
        },
        ClipSource {
            layer: &b,
            asset_path: "./b.usda",
            offset: LayerOffset {
                offset: -100.,
                scale: 1.,
            },
        },
    ];
    let bundle =
        stitch_clip_sequence(&sources, &options, &mut store.tokens, &mut store.paths).unwrap();
    let dir =
        std::env::temp_dir().join(format!("layerstack-clip-authoring-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, layer) in [
        ("a.usda", &a),
        ("b.usda", &b),
        ("root.usda", &bundle.root),
        ("root.topology.usda", &bundle.topology),
        ("root.manifest.usda", &bundle.manifest),
    ] {
        let text = layerstack_usda::save::save_usda(layer, &store.tokens, &store.paths)
            .unwrap_or_else(|error| panic!("cannot serialize {name}: {error:?}"));
        std::fs::write(dir.join(name), text).unwrap();
    }
    // Nothing here authors/reconstructs clip metadata: all five layers above are
    // serialized Rust API outputs, read by OpenUSD's independent implementation.
    let output = Command::new(&python)
        .arg("-c")
        .arg(
            r#"
import json, os, sys
from pxr import Sdf, Usd
root = sys.argv[1]
stage = Usd.Stage.Open(os.path.join(root, 'root.usda'))
assert stage
prim = stage.GetPrimAtPath('/Model')
attr = prim.GetAttribute('animated')
clips = Usd.ClipsAPI(prim)
manifest = Sdf.Layer.FindOrOpen(os.path.join(root, 'root.manifest.usda'))
topology = Sdf.Layer.FindOrOpen(os.path.join(root, 'root.topology.usda'))
assert manifest and topology
manifest_attr = manifest.GetAttributeAtPath('/Model.animated')
topology_attr = topology.GetAttributeAtPath('/Model.animated')
assert manifest_attr and topology_attr
v = Usd.GetVersion()
print(json.dumps({
    'version': f'{v[1]}.{v[2]}',
    'samples': [[t, attr.Get(t)] for t in [0, 3, 10, 20, 24, 39, 40]],
    'default': attr.Get(),
    'static': [prim.GetAttribute('static').Get(t) for t in [Usd.TimeCode.Default(), 3, 24]],
    'range': [stage.GetStartTimeCode(), stage.GetEndTimeCode()],
    'active': [list(p) for p in clips.GetClipActive()],
    'times': [list(p) for p in clips.GetClipTimes()],
    'assets': [a.path for a in clips.GetClipAssetPaths()],
    'manifest_asset': clips.GetClipManifestAssetPath().path,
    'manifest_default': manifest_attr.default,
    'manifest_type': str(manifest_attr.typeName),
    'manifest_custom': manifest_attr.custom,
    'manifest_samples': manifest.ListTimeSamplesForPath('/Model.animated'),
    'manifest_static': bool(manifest.GetAttributeAtPath('/Model.static')),
    'topology_samples': topology.ListTimeSamplesForPath('/Model.animated'),
    'topology_static': topology.GetAttributeAtPath('/Model.static').default,
}))
"#,
        )
        .arg(&dir)
        .output()
        .unwrap();
    let result = if output.status.success() {
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    } else {
        panic!(
            "OpenUSD rejected emitted bundle in {}:\n{}\n{}",
            dir.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    assert_eq!(result["version"], layerstack_schemas::OPENUSD_VERSION);
    assert_eq!(
        result["samples"],
        serde_json::json!([
            [0, 1000.],
            [3, 1003.],
            [10, 1010.],
            [20, 0.],
            [24, 2.],
            [39, 9.5],
            [40, 10.]
        ])
    );
    assert_eq!(result["default"], 7.);
    assert_eq!(result["static"], serde_json::json!([42, 42, 42]));
    assert_eq!(result["range"], serde_json::json!([0., 40.]));
    assert_eq!(result["active"], serde_json::json!([[0., 1.], [20., 0.]]));
    assert_eq!(
        result["times"],
        serde_json::json!([[0., 100.], [10., 110.], [20., 0.], [40., 10.]])
    );
    assert_eq!(
        result["assets"],
        serde_json::json!(["./a.usda", "./b.usda"])
    );
    assert_eq!(result["manifest_asset"], "./root.manifest.usda");
    assert_eq!(result["manifest_default"], 7.);
    assert_eq!(result["manifest_type"], "float");
    assert_eq!(result["manifest_custom"], false);
    assert_eq!(result["manifest_static"], false);
    assert_eq!(result["manifest_samples"], serde_json::json!([]));
    assert_eq!(result["topology_samples"], serde_json::json!([]));
    assert_eq!(result["topology_static"], 42);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn incompatible_overlapping_clip_maps_return_no_bundle() {
    let (mut store, a, b, options) = fixture();
    let before = (a.clone(), b.clone());
    // Map B's local100..110 onto stage5..15, overlapping A's stage0..10.
    // A global clip-time map cannot preserve both affine maps in the overlap.
    let sources = [
        ClipSource {
            layer: &a,
            asset_path: "a.usda",
            offset: LayerOffset::IDENTITY,
        },
        ClipSource {
            layer: &b,
            asset_path: "b.usda",
            offset: LayerOffset {
                offset: -95.,
                scale: 1.,
            },
        },
    ];
    assert_eq!(
        stitch_clip_sequence(&sources, &options, &mut store.tokens, &mut store.paths).unwrap_err(),
        ClipAuthoringError::ConflictingTimeMapping
    );
    // Inputs remain detached and unchanged by rejection.
    assert_eq!((a, b), before);
}
