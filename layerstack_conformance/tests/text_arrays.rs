// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `token[]` and `string[]` values read, compose, resolve, flatten and save
//! as native arrays (`TypedArray::Token` and `TypedArray::String`), from
//! USDA and from USDC, eager and retained alike.
//!
//! Spec: AOUSD Core §6.2–§6.3 (array values), §12.3 (value resolution),
//! §12.5 (text holds between samples), §16.2–§16.3 (USDA and crate files).

#![allow(missing_docs, reason = "integration tests")]

use std::sync::Arc;

use layerstack::{
    AssetResolveError, AssetResolver, FieldValue, FlattenRequirements, InMemoryStore,
    InterpolationType, Layer, LayerId, PathInterner, PropertyPath, ResolvedAsset, Stage,
    StageOptions, StringArray, Time, TokenId, TokenInterner, TypedArray, Value,
};
use layerstack_usdc::{read_usdc, read_usdc_lazy, writer::save_layer};

/// Resolves `./weak.usda` to layer 2.
struct Weak;
impl AssetResolver for Weak {
    fn resolve(
        &mut self,
        asset_path: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        match asset_path {
            "./weak.usda" => Ok(ResolvedAsset {
                layer_id: LayerId(2),
                resolved_path: Arc::from("/weak.usda"),
                layer: None,
            }),
            _ => Err(AssetResolveError::NotFound),
        }
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

const WEAK: &str = r#"#usda 1.0
def "P" (
    customData = {
        string[] notes = ["a", "b"]
        token[] none = []
    }
)
{
    token[] tags = ["t0", "t1", "t2"]
    string[] labels = ["one", "two", ""]
    token[] many = ["m0", "m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8", "m9", "m10", "m11", "m12", "m13", "m14", "m15", "m16", "m17"]
    string[] texts = ["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "s12", "s13", "s14", "s15", "s16", "s17"]
    token[] steps.timeSamples = {
        1: ["a", "b"],
        3: ["c"],
    }
    string[] names.timeSamples = {
        1: ["x"],
        2: ["y", "z"],
    }
}
"#;

const STRONG: &str = r#"#usda 1.0
(
    subLayers = [
        @./weak.usda@
    ]
)

over "P"
{
    token[] tags = edit [write "w" to [0]; append "e"; erase [1]]
    string[] labels = edit [prepend "p"; resize 5]
}
"#;

fn read(store: &mut InMemoryStore, source: &str, id: LayerId) {
    let read = layerstack_usda::read_usda(source, id, &store.tokens, &store.paths, &mut Weak);
    assert!(
        read.parse_diagnostics.is_empty() && read.lower_diagnostics.is_empty(),
        "{:?} {:?}",
        read.parse_diagnostics,
        read.lower_diagnostics
    );
    store.insert_layer(read.emitted.layer);
}

fn scene() -> InMemoryStore {
    let mut store = InMemoryStore::default();
    read(&mut store, WEAK, LayerId(2));
    read(&mut store, STRONG, LayerId(1));
    store
}

fn tokens(store: &InMemoryStore, names: &[&str]) -> Value {
    let ids: Vec<TokenId> = names
        .iter()
        .map(|name| store.tokens.lookup(name).expect("an authored token"))
        .collect();
    Value::from(ids)
}

fn strings(texts: &[&str]) -> Value {
    Value::from(texts.iter().collect::<StringArray>())
}

fn property(store: &InMemoryStore, name: &str) -> PropertyPath {
    let prim = store
        .paths
        .lookup(&layerstack::Path::parse_absolute("/P", &store.tokens).unwrap())
        .unwrap();
    PropertyPath::new(prim, store.tokens.lookup(name).unwrap())
}

fn resolved(stage: &Stage, store: &InMemoryStore, name: &str, time: Time) -> Value {
    stage
        .view(store)
        .attribute(property(store, name))
        .and_then(|attribute| attribute.authored_value(time))
        .unwrap_or_else(|| panic!("{name} resolves"))
        .value
}

/// Whether `value` is a native token or string array.
fn native_text(value: &Value) -> bool {
    matches!(
        value,
        Value::TypedArray(TypedArray::Token(_) | TypedArray::String(_))
    )
}

fn at(code: f64, interpolation: InterpolationType) -> Time {
    Time::At {
        code,
        interpolation,
    }
}

#[test]
fn text_arrays_compose_resolve_and_flatten_natively() {
    let mut store = scene();
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let tags = resolved(&stage, &store, "tags", Time::Default);
    assert!(native_text(&tags), "{tags:?}");
    // Sparse edits over a native weaker array (Core §12.3).
    assert_eq!(tags, tokens(&store, &["w", "t2", "e"]));
    let labels = resolved(&stage, &store, "labels", Time::Default);
    assert!(native_text(&labels), "{labels:?}");
    assert_eq!(labels, strings(&["p", "one", "two", "", ""]));

    // Text holds between samples whatever the interpolation (Core §12.5.2).
    for interpolation in [InterpolationType::Held, InterpolationType::Linear] {
        let steps = resolved(&stage, &store, "steps", at(2.0, interpolation));
        assert!(native_text(&steps));
        assert_eq!(steps, tokens(&store, &["a", "b"]));
        assert_eq!(
            resolved(&stage, &store, "steps", at(3.5, interpolation)),
            tokens(&store, &["c"])
        );
        assert_eq!(
            resolved(&stage, &store, "names", at(1.5, interpolation)),
            strings(&["x"])
        );
    }

    // The flattened layer holds the composed arrays natively.
    let flat = stage
        .flatten(
            &mut store,
            LayerId(1),
            LayerId(9),
            &FlattenRequirements::default(),
        )
        .expect("nothing is lost")
        .layer;
    for name in ["tags", "labels", "many", "texts"] {
        let spec = flat.property(property(&store, name)).unwrap();
        let value = spec.default.as_ref().unwrap();
        assert!(native_text(value), "{name}: {value:?}");
        assert_eq!(
            *value,
            resolved(&stage, &store, name, Time::Default),
            "{name}"
        );
    }
}

#[test]
fn text_arrays_round_trip_through_usda_and_usdc() {
    let mut store = scene();
    let text = {
        let weak = store.layer(LayerId(2)).unwrap();
        layerstack_usda::save::save_usda(weak, &store.tokens, &store.paths).unwrap()
    };
    // USDA text reads back as the layer it was saved from.
    read(&mut store, &text, LayerId(3));
    let weak = store.layer(LayerId(2)).unwrap();
    let names = ["tags", "labels", "many", "texts", "steps", "names"];
    let prim = store
        .paths
        .lookup(&layerstack::Path::parse_absolute("/P", &store.tokens).unwrap())
        .unwrap();
    let custom_data = store.tokens.lookup("customData").unwrap();
    let custom = |layer: &Layer| layer.prim(prim).unwrap().field(custom_data).cloned();
    let notes = custom(weak);
    let Some(FieldValue::Value(Value::Dictionary(entries))) = &notes else {
        panic!("customData");
    };
    assert!(
        entries.iter().all(|(_, value)| native_text(value)),
        "{entries:?}"
    );

    let from_usda = store.layer(LayerId(3)).unwrap();

    // USDC, read eagerly and retained.
    let bytes = save_layer(weak, &store.tokens, &store.paths).unwrap();
    let eager = read_usdc(&bytes, LayerId(4), &store.tokens, &store.paths, &mut Weak)
        .unwrap()
        .layer;
    let retained = read_usdc_lazy(
        bytes.as_slice().into(),
        LayerId(5),
        &store.tokens,
        &store.paths,
        &mut Weak,
    )
    .unwrap();
    let retained_layer = &retained.assembled.layer;
    for name in names {
        let path = property(&store, name);
        let want = weak.property(path).unwrap();
        for (form, layer) in [
            ("USDA", from_usda),
            ("eager USDC", &eager),
            ("retained USDC", retained_layer),
        ] {
            let got = layer.property(path).unwrap();
            let (want_default, got_default) = (
                want.default.as_ref().map(Value::materialized),
                got.default.as_ref().map(Value::materialized),
            );
            assert_eq!(got_default, want_default, "{form} {name}");
            assert!(got_default.is_none_or(native_text), "{form} {name}");
            let samples = |spec: &layerstack::PropertySpec| {
                spec.time_samples
                    .as_ref()
                    .map(|samples| samples.iter().cloned().collect::<Vec<_>>())
            };
            assert_eq!(samples(got), samples(want), "{form} {name} samples");
            if let Some(samples) = samples(got) {
                assert!(
                    samples.iter().all(|(_, value)| native_text(value)),
                    "{form} {name}"
                );
            }
        }
    }
    // A crate dictionary is ordered by key, a USDA one as authored.
    let sorted = |field: Option<FieldValue>| match field {
        Some(FieldValue::Value(Value::Dictionary(mut entries))) => {
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            entries
        }
        other => panic!("customData, not {other:?}"),
    };
    for layer in [from_usda, &eager, retained_layer] {
        let entries = sorted(custom(layer));
        assert_eq!(entries, sorted(notes.clone()));
        for ((_, a), (_, b)) in entries.iter().zip(&sorted(notes.clone())) {
            assert!(a.same_representation(b), "{a:?} {b:?}");
        }
    }
    // Long arrays stay encoded in a retained read until first read, and
    // decode to what the eager read holds.
    let many = retained_layer.property(property(&store, "many")).unwrap();
    assert!(matches!(many.default, Some(Value::Deferred(_))));
    let stats = retained.values.stats();
    assert_eq!(stats.live_values, 2, "`many` and `texts`");
    assert_eq!(
        stats.materialized_values, 2,
        "decoded by the comparison above"
    );
    assert_eq!(stats.value_elements, 36);
}
