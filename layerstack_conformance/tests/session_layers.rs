// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Session composition and independently synchronized collaboration clients.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LayerStack, LayerStackIdentifier, LiveStage, NodeId,
    PrimSpec, PropertySpec, PropertyType, Reference, ReferenceTarget, Stage, StageOptions,
    SublayerEntry, Transaction, Value,
};
use serde_json::json;
use std::process::Command;

fn attr(value: i32) -> PropertySpec {
    PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
        .with_default(Value::Int(value))
}
fn fixture() -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let asset = store.path("/Asset");
    let internal = store.path("/Internal");
    let external = store.path("/External");
    let x = store.tokens.intern("x");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(asset, PrimSpec::def().with_property(x, attr(2)));
    let mut spec = PrimSpec::def();
    spec.references.explicit = Some(vec![Reference::new(LayerId(1), asset)]);
    root.insert_prim(internal, spec);
    let mut spec = PrimSpec::def();
    spec.references.explicit = Some(vec![Reference {
        layer: LayerId(1),
        asset: Some("root.usda".into()),
        target: ReferenceTarget::Prim(asset),
        layer_offset: layerstack::LayerOffset::IDENTITY,
        custom_data: Vec::new(),
    }]);
    root.insert_prim(external, spec);
    store.insert_layer(root);
    let mut shared = Layer::new(LayerId(2));
    shared.insert_prim(asset, PrimSpec::over().with_property(x, attr(8)));
    store.insert_layer(shared);
    for id in [LayerId(3), LayerId(4)] {
        let mut session = Layer::new(id);
        session.sublayers.push(SublayerEntry::new(LayerId(2)));
        store.insert_layer(session);
    }
    store
}
fn values(stage: &Stage, store: &mut InMemoryStore) -> Vec<i32> {
    ["/Asset.x", "/Internal.x", "/External.x"]
        .map(|path| {
            match stage
                .resolve_field_path(store.property_path(path))
                .unwrap()
                .value
            {
                Value::Int(n) => n,
                _ => panic!("int expected"),
            }
        })
        .to_vec()
}

#[test]
fn session_stack_and_external_self_reference_match_openusd() {
    let mut store = fixture();
    let prop = store.property_path("/Asset.x");
    store.layers.get_mut(&LayerId(3)).unwrap().insert_prim(
        prop.prim_path(),
        PrimSpec::over().with_property(prop.property(), attr(10)),
    );
    let options = StageOptions {
        session_layer: Some(LayerId(3)),
        with_provenance: true,
        ..Default::default()
    };
    let stage = Stage::compose(&mut store, LayerId(1), options);
    assert_eq!(values(&stage, &mut store), [10, 10, 2]);
    assert!(
        stage.composition_errors().is_empty(),
        "{:?}",
        stage.composition_errors()
    );
    assert_eq!(stage.layer_stack(), &[LayerId(3), LayerId(2), LayerId(1)]);
    let identifier = LayerStackIdentifier {
        root: LayerId(1),
        session: Some(LayerId(3)),
    };
    assert_eq!(
        LayerStack::gather_identifier(&store, identifier).layers,
        stage.layer_stack()
    );
    let target =
        EditTarget::for_node_layer(&stage, &store, prop.prim_path(), NodeId::ROOT, LayerId(3));
    assert!(
        target.is_some(),
        "session root is an ordinary editable layer"
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
import json
from pxr import Usd,Sdf
r=Sdf.Layer.CreateAnonymous('root.usda')
r.ImportFromString('#usda 1.0\ndef "Asset" {\n int x = 2\n}\ndef "Internal" (references = </Asset>) {}\ndef "External" (references = @'+r.identifier+'@</Asset>) {}')
l=Sdf.Layer.CreateAnonymous('live.usda');l.ImportFromString('#usda 1.0\nover "Asset" {\n int x = 8\n}')
s=Sdf.Layer.CreateAnonymous('session.usda');s.ImportFromString('#usda 1.0\nover "Asset" {\n int x = 10\n}');s.subLayerPaths=[l.identifier]
st=Usd.Stage.Open(r,s)
print(json.dumps([st.GetPrimAtPath(p).GetAttribute('x').Get() for p in ['/Asset','/Internal','/External']]))
"#;
    let output = Command::new(python).args(["-c", script]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json!(values(&stage, &mut store)), expected);
}

#[test]
fn two_clients_keep_private_overrides_and_reject_stale_shared_edits() {
    let mut store = fixture();
    let prop = store.property_path("/Asset.x");
    let opts = |session| StageOptions {
        session_layer: Some(session),
        ..Default::default()
    };
    let mut a = LiveStage::compose(&mut store, LayerId(1), opts(LayerId(3)));
    let mut b = LiveStage::compose(&mut store, LayerId(1), opts(LayerId(4)));
    let mut ca = a.change_cursor();
    let mut cb = b.change_cursor();
    let mut private = Transaction::new();
    private.set_default(
        EditTarget::for_layer(LayerId(3)).property(prop),
        Value::Int(10),
    );
    a.apply(&mut store, &private).unwrap();
    b.synchronize(&mut store);
    assert_eq!(values(a.stage(), &mut store), [10, 10, 2]);
    assert_eq!(values(b.stage(), &mut store), [8, 8, 2]);
    assert_eq!(b.changes_since(&mut cb).unwrap().count(), 0);
    let generation = store.layers[&LayerId(2)].generation();
    let mut shared = Transaction::new();
    shared.expect_generation(LayerId(2), generation);
    shared.set_default(
        EditTarget::for_layer(LayerId(2)).property(prop),
        Value::Int(12),
    );
    shared.apply(&mut store).unwrap();
    assert!(
        shared.apply(&mut store).is_err(),
        "a stale incoming batch is rejected atomically"
    );
    a.synchronize(&mut store);
    b.synchronize(&mut store);
    assert_eq!(values(a.stage(), &mut store), [10, 10, 2]);
    assert_eq!(values(b.stage(), &mut store), [12, 12, 2]);
    assert_eq!(a.changes_since(&mut ca).unwrap().count(), 2);
    assert_eq!(b.changes_since(&mut cb).unwrap().count(), 1);
    assert!(a.mute_layer(LayerId(3)).unwrap());
    a.synchronize(&mut store);
    assert_eq!(values(a.stage(), &mut store), [2, 2, 2]);
    assert_eq!(values(b.stage(), &mut store), [12, 12, 2]);
    assert!(a.unmute_layer(LayerId(3)));
    a.synchronize(&mut store);
    assert_eq!(values(a.stage(), &mut store), [10, 10, 2]);
    assert!(a.set_session_layer(None));
    a.synchronize(&mut store);
    assert_eq!(values(a.stage(), &mut store), [2, 2, 2]);
}

#[test]
fn session_time_domains_include_authoring_offsets_and_root_default_prim() {
    for (root_tcps, root_fps, session_tcps, session_fps, rate) in [
        (None, None, None, None, 24.0),
        (Some(48.0), None, None, None, 48.0),
        (Some(48.0), None, Some(96.0), None, 96.0),
        (Some(48.0), None, None, Some(96.0), 48.0),
        (None, Some(48.0), None, Some(96.0), 96.0),
    ] {
        let mut store = fixture();
        let tcps = store.tokens.intern("timeCodesPerSecond");
        let fps = store.tokens.intern("framesPerSecond");
        for (id, tc, fr) in [
            (LayerId(1), root_tcps, root_fps),
            (LayerId(3), session_tcps, session_fps),
        ] {
            let layer = store.layers.get_mut(&id).unwrap();
            if let Some(value) = tc {
                layer.set_metadata(tcps, Value::Double(value));
            }
            if let Some(value) = fr {
                layer.set_metadata(fps, Value::Double(value));
            }
        }
        let root_rate = root_tcps.or(root_fps).unwrap_or(24.0);
        let x = store.tokens.intern("animated");
        let mut prop =
            PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0)));
        prop.time_samples = Some(vec![(root_rate, Value::Double(2.0))].into());
        let asset = store.path("/Asset");
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .set_property(layerstack::PropertyPath::new(asset, x), prop);
        let root_default = store.tokens.intern("Asset");
        let session_default = store.tokens.intern("Internal");
        store.layers.get_mut(&LayerId(1)).unwrap().default_prim = Some(root_default);
        store.layers.get_mut(&LayerId(3)).unwrap().default_prim = Some(session_default);
        let stage = Stage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                session_layer: Some(LayerId(3)),
                ..Default::default()
            },
        );
        assert_eq!(stage.time_codes_per_second(&store), rate);
        assert_eq!(stage.default_prim(&mut store), Some(asset));
        for path in ["/Asset", "/Internal"] {
            let prim = store.path(path);
            assert_eq!(
                stage.property_sample_times(prim, x),
                [rate],
                "{path} rates {root_tcps:?}/{session_tcps:?}"
            );
            let target = EditTarget::for_node(&stage, prim, NodeId::ROOT).unwrap();
            assert_eq!(target.map_to_spec_time(rate), root_rate);
        }
        let prim = store.path("/External");
        assert_eq!(stage.property_sample_times(prim, x), [rate]);
    }
}

#[test]
fn session_expression_variables_override_root_and_flow_into_external_stacks() {
    let mut store = fixture();
    let variables = store.tokens.intern("expressionVariables");
    let root = store.layers.get_mut(&LayerId(1)).unwrap();
    root.set_metadata(
        variables,
        Value::Dictionary(vec![("ASSET".into(), Value::String("red.usda".into()))]),
    );
    root.sublayers.push(SublayerEntry::unresolved(
        "`${ASSET}`",
        layerstack::LayerOffset::IDENTITY,
    ));
    store.layers.get_mut(&LayerId(3)).unwrap().set_metadata(
        variables,
        Value::Dictionary(vec![("ASSET".into(), Value::String("blue.usda".into()))]),
    );
    let asset = store.path("/Asset");
    let tint = store.tokens.intern("tint");
    for (id, value) in [(LayerId(5), 5), (LayerId(6), 6)] {
        let mut layer = Layer::new(id);
        layer.insert_prim(asset, PrimSpec::def().with_property(tint, attr(value)));
        store.insert_layer(layer);
    }
    store.insert_asset_layer(LayerId(1), "red.usda", LayerId(5));
    store.insert_asset_layer(LayerId(1), "blue.usda", LayerId(6));
    let options = StageOptions {
        session_layer: Some(LayerId(3)),
        ..Default::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    for path in ["/Asset", "/Internal", "/External"] {
        assert_eq!(
            live.stage()
                .resolve_field_path(store.property_path(&format!("{path}.tint")))
                .unwrap()
                .value,
            Value::Int(6)
        );
    }
    let custom = store.tokens.intern("customLayerData");
    store.layers.get_mut(&LayerId(1)).unwrap().set_metadata(
        custom,
        Value::Dictionary(vec![("shared".into(), Value::Int(1))]),
    );
    store.layers.get_mut(&LayerId(3)).unwrap().set_metadata(
        custom,
        Value::Dictionary(vec![("private".into(), Value::Int(2))]),
    );
    live.synchronize(&mut store);
    assert_eq!(
        live.stage().layer_metadata(custom, &store),
        Some(Value::Dictionary(vec![
            ("private".into(), Value::Int(2)),
            ("shared".into(), Value::Int(1))
        ]))
    );
    store.layers.get_mut(&LayerId(3)).unwrap().set_metadata(
        variables,
        Value::Dictionary(vec![("ASSET".into(), Value::String("red.usda".into()))]),
    );
    live.synchronize(&mut store);
    for path in ["/Asset", "/Internal", "/External"] {
        assert_eq!(
            live.stage()
                .resolve_field_path(store.property_path(&format!("{path}.tint")))
                .unwrap()
                .value,
            Value::Int(5)
        );
    }
}

#[test]
fn flatten_preserves_session_metadata_and_effective_time_domain() {
    let mut store = fixture();
    let tcps = store.tokens.intern("timeCodesPerSecond");
    let custom = store.tokens.intern("customLayerData");
    store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .set_metadata(tcps, Value::Double(48.0))
        .set_metadata(
            custom,
            Value::Dictionary(vec![("shared".into(), Value::Int(1))]),
        );
    store
        .layers
        .get_mut(&LayerId(3))
        .unwrap()
        .set_metadata(tcps, Value::Double(96.0))
        .set_metadata(
            custom,
            Value::Dictionary(vec![("private".into(), Value::Int(2))]),
        );
    let session_default = store.tokens.intern("Internal");
    store.layers.get_mut(&LayerId(3)).unwrap().default_prim = Some(session_default);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            session_layer: Some(LayerId(3)),
            ..Default::default()
        },
    );
    let flat = stage
        .flatten(
            &mut store,
            LayerId(1),
            LayerId(20),
            &layerstack::stage::flatten::FlattenRequirements::default(),
        )
        .unwrap();
    assert_eq!(flat.layer.default_prim, Some(session_default));
    store.insert_layer(flat.layer);
    let reopened = Stage::compose(&mut store, LayerId(20), StageOptions::default());
    assert_eq!(reopened.time_codes_per_second(&store), 96.0);
    assert_eq!(
        reopened.layer_metadata(custom, &store),
        stage.layer_metadata(custom, &store)
    );
    assert_eq!(values(&reopened, &mut store), values(&stage, &mut store));
}

#[test]
fn edit_targets_use_captured_offsets_and_exclude_muted_session_layers() {
    let mut store = fixture();
    let tcps = store.tokens.intern("timeCodesPerSecond");
    store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .set_metadata(tcps, Value::Double(48.0));
    store
        .layers
        .get_mut(&LayerId(3))
        .unwrap()
        .set_metadata(tcps, Value::Double(96.0));
    let options = StageOptions {
        session_layer: Some(LayerId(3)),
        ..Default::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    let asset = store.path("/Asset");
    let target =
        EditTarget::for_node_layer(live.stage(), &store, asset, NodeId::ROOT, LayerId(1)).unwrap();
    assert_eq!(target.map_to_spec_time(96.0), 48.0);
    live.mute_layer(LayerId(3)).unwrap();
    live.synchronize(&mut store);
    assert!(
        EditTarget::for_node_layer(live.stage(), &store, asset, NodeId::ROOT, LayerId(3)).is_none()
    );
    assert!(
        EditTarget::for_node_layer(live.stage(), &store, asset, NodeId::ROOT, LayerId(2)).is_none()
    );
    let target =
        EditTarget::for_node_layer(live.stage(), &store, asset, NodeId::ROOT, LayerId(1)).unwrap();
    assert_eq!(target.map_to_spec_time(48.0), 48.0);
    // Direct mutation of source rates cannot retime an existing snapshot's edit
    // map. A host synchronizes before choosing current targets and guards writes.
    store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .set_metadata(tcps, Value::Double(12.0));
    assert_eq!(target.map_to_spec_time(48.0), 48.0);
}

#[test]
fn retained_session_snapshots_can_cross_host_thread_boundaries() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Stage>();
    assert_send_sync::<LiveStage>();
}
