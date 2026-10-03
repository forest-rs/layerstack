// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sparse programs and value clips share the ordinary temporal composition kernel.
//! AOUSD Core §12.3.4, §12.5; OpenUSD `stage.cpp::_GetResolveInfoWithClipsImpl`.

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, InterpolationType, LayerId, PathInterner,
    ResolvedAsset, Stage, StageOptions, TokenInterner, Value,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    process::Command,
    sync::{Arc, OnceLock},
};

type Layers = Vec<(String, String)>;
struct Assets {
    names: BTreeMap<String, LayerId>,
}
impl Assets {
    fn name(&self, id: LayerId) -> Option<&str> {
        self.names
            .iter()
            .find_map(|(name, found)| (*found == id).then_some(name.as_str()))
    }
    fn lookup(&self, asset: &str, anchor: Option<LayerId>) -> Option<(&str, LayerId)> {
        let parent = anchor
            .and_then(|id| self.name(id))
            .and_then(|name| name.rsplit_once('/'))
            .map_or("", |(p, _)| p);
        let joined = if parent.is_empty() {
            asset.to_owned()
        } else {
            format!("{parent}/{asset}")
        };
        let mut parts = Vec::new();
        for part in joined.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                _ => parts.push(part),
            }
        }
        self.names
            .get_key_value(&parts.join("/"))
            .map(|(name, id)| (name.as_str(), *id))
    }
}
impl AssetResolver for Assets {
    fn resolve(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let (name, id) = self
            .lookup(asset, anchor)
            .ok_or(AssetResolveError::NotFound)?;
        Ok(ResolvedAsset {
            layer_id: id,
            resolved_path: Arc::from(name),
            layer: None,
        })
    }
    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.name(id)
    }
}
fn relative(from: &str, to: &str) -> String {
    let mut from: Vec<_> = from.split('/').collect();
    from.pop();
    let to: Vec<_> = to.split('/').collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    std::iter::repeat_n("..", from.len() - shared)
        .chain(to[shared..].iter().copied())
        .collect::<Vec<_>>()
        .join("/")
}
fn compose(layers: &Layers) -> (InMemoryStore, Stage) {
    let mut store = InMemoryStore::default();
    let mut assets = Assets {
        names: layers
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (name.clone(), LayerId(u64::try_from(i + 1).unwrap())))
            .collect(),
    };
    for (name, text) in layers {
        let id = assets.names[name];
        let parsed = layerstack_usda::parser::parse(text);
        let emitted = layerstack_usda::emit::emit(
            &parsed.layer,
            id,
            &mut store.tokens,
            &mut store.paths,
            &mut assets,
        );
        assert!(
            parsed.diagnostics.is_empty(),
            "{name}: {:?}",
            parsed.diagnostics
        );
        assert!(
            emitted.diagnostics.is_empty(),
            "{name}: {:?}",
            emitted.diagnostics
        );
        store.insert_layer(emitted.layer);
    }
    for (anchor_name, &anchor) in &assets.names {
        for (name, &id) in &assets.names {
            let rel = relative(anchor_name, name);
            store.insert_asset_layer(anchor, &rel, id);
            store.insert_asset_layer(anchor, &format!("./{rel}"), id);
        }
    }
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    assert!(
        stage.composition_errors().is_empty(),
        "{:?}",
        stage.composition_errors()
    );
    (store, stage)
}

#[derive(Deserialize)]
struct Oracle {
    values: Vec<Option<Vec<f64>>>,
    samples: Vec<f64>,
}
fn numeric_array(value: Option<Value>) -> Option<Vec<f64>> {
    match value? {
        Value::TypedArray(layerstack::TypedArray::Float(items)) => {
            Some(items.iter().copied().map(f64::from).collect())
        }
        Value::Array(items) => items
            .iter()
            .map(|v| match v {
                Value::Float(v) => Some(f64::from(*v)),
                Value::Double(v) => Some(*v),
                _ => None,
            })
            .collect(),
        other => panic!("not a float array: {other:?}"),
    }
}
fn native(layers: &Layers, property: &str, queries: &[f64]) -> Option<Oracle> {
    native_with_interpolation(layers, property, queries, InterpolationType::Linear)
}
fn native_with_interpolation(
    layers: &Layers,
    property: &str,
    queries: &[f64],
    interp: InterpolationType,
) -> Option<Oracle> {
    static PYTHON: OnceLock<Option<String>> = OnceLock::new();
    let python = PYTHON
        .get_or_init(|| {
            let explicit = std::env::var("LAYERSTACK_USD_PYTHON").ok();
            let python = explicit.clone().unwrap_or_else(|| "python3".into());
            let available = Command::new(&python)
                .args(["-c", "from pxr import Usd"])
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(
                explicit.is_none() || available,
                "pinned OpenUSD Python unavailable"
            );
            available.then_some(python)
        })
        .as_ref()?;
    static NEXT_DIRECTORY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT_DIRECTORY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "layerstack-clip-sparse-{}-{unique}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    for (name, text) in layers {
        std::fs::write(directory.join(name), text).unwrap();
    }
    let script = r#"import json,sys
from pxr import Usd
s=Usd.Stage.Open(sys.argv[1]);a=s.GetAttributeAtPath(sys.argv[2])
assert Usd.GetVersion()==(0,26,8), Usd.GetVersion()
if sys.argv[4]=="held": s.SetInterpolationType(Usd.InterpolationTypeHeld)
q=json.loads(sys.argv[3])
print(json.dumps({'values':[None if a.Get(t) is None else list(a.Get(t)) for t in q], 'samples':a.GetTimeSamples()}))
"#;
    let output = Command::new(python)
        .args(["-c", script])
        .arg(directory.join("root.usda"))
        .arg(property)
        .arg(serde_json::to_string(queries).unwrap())
        .arg(if interp == InterpolationType::Held {
            "held"
        } else {
            "linear"
        })
        .output()
        .unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}
fn text(body: &str) -> String {
    format!("#usda 1.0\n{body}\n")
}
fn metadata() -> &'static str {
    r#"clips = { dictionary default = {
 asset[] assetPaths = [@a.usda@]
 string primPath = "/C"
 double2[] active = [(0,0)]
 asset manifestAssetPath = @manifest.usda@
 }}"#
}
fn clip(body: &str) -> String {
    text(&format!("def \"C\" {{\n{body}\n}}"))
}
fn check(
    layers: &Layers,
    property: &str,
    queries: &[f64],
    expected: &[Vec<f64>],
    samples: &[f64],
) -> (InMemoryStore, Stage) {
    let (mut store, stage) = compose(layers);
    let path = store.property_path(property);
    for (&time, expected) in queries.iter().zip(expected) {
        let actual = numeric_array(
            stage
                .resolve_property_path_at_time(path, time, InterpolationType::Linear)
                .map(|r| r.value),
        )
        .unwrap();
        assert_eq!(
            actual.len(),
            expected.len(),
            "array length at {time}: {actual:?} != {expected:?}"
        );
        for (a, b) in actual.iter().zip(expected) {
            assert!(
                (a - b).abs() < 1e-6,
                "time {time}: {actual:?} != {expected:?}"
            );
        }
    }
    assert_eq!(
        stage.property_sample_times(path.prim_path(), path.property()),
        samples,
        "effective sample times for {property}"
    );
    if let Some(oracle) = native(layers, property, queries) {
        assert_eq!(
            oracle.samples, samples,
            "native effective sample times for {property}"
        );
        for (actual, expected) in oracle.values.iter().zip(expected) {
            assert_eq!(
                actual.as_ref(),
                Some(expected),
                "native sparse payload for {property}"
            );
        }
    }
    (store, stage)
}
#[test]
fn stronger_sparse_samples_interpolate_over_actual_clip_endpoints() {
    let layers = vec![
        (
            "root.usda".into(),
            text(
                "(subLayers=[@anchor.usda@])\nover \"P\" {\nfloat[] x.timeSamples={0:edit[write 100 to [0]],10:edit[write 200 to [0]]}\n}",
            ),
        ),
        (
            "anchor.usda".into(),
            text(&format!("def \"P\" (\n{}\n) {{\nfloat[] x\n}}", metadata())),
        ),
        (
            "a.usda".into(),
            clip("float[] x.timeSamples={0:[0,0],10:[10,10]}"),
        ),
        ("manifest.usda".into(), clip("float[] x")),
    ];
    check(
        &layers,
        "/P.x",
        &[2., 5., 8.],
        &[vec![120., 2.], vec![150., 5.], vec![180., 8.]],
        &[0., 10.],
    );
}
#[test]
fn raw_sparse_clips_interpolate_and_explain_the_weaker_default_base() {
    let layers = vec![
        (
            "root.usda".into(),
            text(&format!(
                "(subLayers=[@base.usda@])\ndef \"P\" (\n{}\n) {{\nfloat[] x\n}}",
                metadata()
            )),
        ),
        (
            "a.usda".into(),
            clip("float[] x.timeSamples={0:edit[write 100 to [0]],10:edit[write 200 to [0]]}"),
        ),
        ("manifest.usda".into(), clip("float[] x")),
        ("base.usda".into(), text("over \"P\" {\nfloat[] x=[0,0]\n}")),
    ];
    let (mut store, stage) = check(
        &layers,
        "/P.x",
        &[2., 5., 8.],
        &[vec![120., 0.], vec![150., 0.], vec![180., 0.]],
        &[0., 10.],
    );
    let path = store.property_path("/P.x");
    let source = stage
        .property_clip_source(
            path.prim_path(),
            path.property(),
            5.,
            InterpolationType::Linear,
        )
        .unwrap();
    assert_eq!(source.lower.layer, Some(LayerId(2)));
    let resolved = stage
        .read_property_with_provenance(path, layerstack::Time::at(5.), |v| Some(v.clone()))
        .unwrap();
    assert_eq!(resolved.provenance.unwrap().layer, LayerId(2));
    let explanation = stage
        .explain_property_value_at_time(path, 5., InterpolationType::Linear)
        .unwrap();
    assert_eq!(explanation.source, layerstack::ValueSource::ValueClips);
    assert_eq!(numeric_array(explanation.value), Some(vec![150., 0.]));
    assert!(
        explanation
            .opinions
            .iter()
            .any(|op| op.layer() == LayerId(4)
                && matches!(
                    op.role,
                    layerstack::OpinionRole::Contributed(layerstack::Contribution::Value)
                ))
    );
}
#[test]
fn authored_sparse_samples_at_the_clip_anchor_exclude_that_sites_clips() {
    let layers = vec![
        (
            "root.usda".into(),
            text(&format!(
                "def \"P\" (\n{}\n) {{\nfloat[] x.timeSamples={{0:edit[write 100 to [0]],10:edit[write 200 to [0]]}}\n}}",
                metadata()
            )),
        ),
        (
            "a.usda".into(),
            clip("float[] x.timeSamples={0:[1000,1000],10:[1010,1010]}"),
        ),
        ("manifest.usda".into(), clip("float[] x")),
    ];
    let (mut store, stage) = check(
        &layers,
        "/P.x",
        &[0., 5., 10.],
        &[vec![], vec![], vec![]],
        &[0., 10.],
    );
    let path = store.property_path("/P.x");
    assert!(
        stage
            .property_clip_source(
                path.prim_path(),
                path.property(),
                5.,
                InterpolationType::Linear
            )
            .is_none()
    );
}
#[test]
fn skeleton_discovery_keeps_clip_samples_after_a_dense_to_sparse_transition() {
    let layers = vec![
        (
            "root.usda".into(),
            text(
                "(subLayers=[@anchor.usda@])\nover \"P\" {\nfloat[] blendShapeWeights.timeSamples={0:[100,100],10:edit[write 200 to [0]]}\n}",
            ),
        ),
        (
            "anchor.usda".into(),
            text(&format!(
                "def SkelAnimation \"P\" (\n{}\n) {{\nfloat[] blendShapeWeights\n}}",
                metadata()
            )),
        ),
        (
            "a.usda".into(),
            clip("float[] blendShapeWeights.timeSamples={0:[0,0],15:[15,15],20:[20,20]}"),
        ),
        ("manifest.usda".into(), clip("float[] blendShapeWeights")),
    ];
    let (mut store, stage) = check(
        &layers,
        "/P.blendShapeWeights",
        &[5., 15., 20.],
        &[vec![150., 50.], vec![200., 15.], vec![200., 20.]],
        &[0., 10., 15., 20.],
    );
    let property = store.property_path("/P.blendShapeWeights");
    assert!(
        stage
            .property_clip_source(
                property.prim_path(),
                property.property(),
                5.,
                InterpolationType::Linear
            )
            .is_some()
    );
    let explanation = stage
        .explain_property_value_at_time(property, 5., InterpolationType::Linear)
        .unwrap();
    assert_eq!(numeric_array(explanation.value), Some(vec![150., 50.]));
    let resolved = stage
        .read_property_with_provenance(property, layerstack::Time::at(5.), |v| Some(v.clone()))
        .unwrap();
    assert_eq!(resolved.provenance.unwrap().layer, LayerId(1));
    let path = store.path("/P");
    let scene = layerstack_schemas::Scene::new(&stage, &store);
    let animation = layerstack_schemas::usd_skel::SkelAnimation::new(&scene, path).unwrap();
    assert_eq!(
        animation.blend_shape_weight_time_samples(),
        [0., 10., 15., 20.]
    );
}

#[test]
fn raw_sparse_clip_discovery_retains_weaker_sampled_grids() {
    let layers = vec![
        (
            "root.usda".into(),
            text(&format!(
                "(subLayers=[@base.usda@])\ndef \"P\" (\n{}\n) {{\nfloat[] x\n}}",
                metadata()
            )),
        ),
        (
            "a.usda".into(),
            clip("float[] x.timeSamples={0:edit[write 100 to [0]],10:edit[write 200 to [0]]}"),
        ),
        ("manifest.usda".into(), clip("float[] x")),
        (
            "base.usda".into(),
            text("over \"P\" {\nfloat[] x.timeSamples={0:[0,0],5:[5,5],20:[20,20]}\n}"),
        ),
    ];
    check(
        &layers,
        "/P.x",
        &[2., 5., 8., 10., 15., 20.],
        &[
            vec![100., 2.],
            vec![100., 5.],
            vec![160., 5.],
            vec![200., 5.],
            vec![200., 12.5],
            vec![200., 20.],
        ],
        &[0., 5., 10., 20.],
    );
}
#[test]
fn synthetic_sparse_activation_uses_exact_sample_type_and_value_semantics() {
    let layers = vec![
        (
            "root.usda".into(),
            text(&format!(
                "(subLayers=[@base.usda@])\ndef \"P\" (\n{}\n) {{\nfloat[] x\n}}",
                metadata().replace("[(0,0)]", "[(5,0)]")
            )),
        ),
        (
            "a.usda".into(),
            clip("float[] x.timeSamples={0:edit[write 100 to [0]],10:edit[write 200 to [0]]}"),
        ),
        ("manifest.usda".into(), clip("float[] x")),
        (
            "base.usda".into(),
            text("over \"P\" {\nfloat[] x.timeSamples={2:[2,2],7:[7,7]}\n}"),
        ),
    ];
    check(
        &layers,
        "/P.x",
        &[2., 5., 7.],
        &[vec![100., 2.], vec![], vec![100., 7.]],
        &[0., 2., 5., 10.],
    );
}

#[test]
fn mixed_stronger_clip_advances_the_weaker_clip_query() {
    let sets = ["a", "b"].map(|name| format!(
        "dictionary {name} = {{\nasset[] assetPaths=[@{name}.usda@]\nstring primPath=\"/C\"\ndouble2[] active=[(0,0)]\nasset manifestAssetPath=@manifest.usda@\n}}"
    )).join("\n");
    let layers = vec![
        (
            "root.usda".into(),
            text(&format!(
                "def \"P\" (\nclips={{\n{sets}\n}}\n) {{\nfloat[] x\n}}"
            )),
        ),
        (
            "a.usda".into(),
            clip("float[] x.timeSamples={0:[100,100],10:edit[write 200 to [0]]}"),
        ),
        (
            "b.usda".into(),
            clip("float[] x.timeSamples={0:[0,0],15:[15,15],20:[20,20]}"),
        ),
        ("manifest.usda".into(), clip("float[] x")),
    ];
    // Native26.08 reaches the second set at the stronger sparse upper
    // endpoint, and both clip sets retain independent ordinal identities.
    check(
        &layers,
        "/P.x",
        &[2., 5., 8., 10., 15., 20.],
        &[
            vec![120., 80.],
            vec![150., 50.],
            vec![180., 20.],
            vec![200., 0.],
            vec![200., 15.],
            vec![200., 20.],
        ],
        &[0., 10., 15., 20.],
    );
}

#[test]
fn stronger_sparse_upper_endpoint_lands_on_a_synthetic_clip_activation() {
    let layers = vec![
("root.usda".into(), "#usda 1.0\n(subLayers=[@anchor.usda@,@base.usda@])\nover \"P\" {\nfloat[] x.timeSamples={0:[1000,1000],5:edit[write 200 to [0]]}\n}\n".into()),
("anchor.usda".into(), "#usda 1.0\n\ndef \"P\" (clips = { dictionary default = {\nasset[] assetPaths=[@a.usda@]\ndouble2[] active=[(5,0)]\nasset manifestAssetPath=@manifest.usda@ \nstring primPath=\"/C\"\n} }) {\nfloat[] x\n}\n".into()),
("a.usda".into(), "#usda 1.0\ndef \"C\" {\nfloat[] x.timeSamples={0:edit[write 100 to [0]],10:edit[write 200 to [0]]}\n}\n".into()),
("manifest.usda".into(), "#usda 1.0\ndef \"C\" {\nfloat[] x\n}\n".into()),
("base.usda".into(), "#usda 1.0\nover \"P\" {\nfloat[] x.timeSamples={2:[2,2],7:[7,7]}\n}\n".into())
    ];
    let (mut store, stage) = check(
        &layers,
        "/P.x",
        &[2., 5., 7.],
        &[vec![1000., 1000.], vec![], vec![200., 7.]],
        &[0., 5., 10.],
    );

    // The numeric source chain can reach the clip through the sparse upper
    // endpoint even though Held output uses only the stronger ordinary lower.
    let path = store.property_path("/P.x");
    assert!(
        stage
            .property_clip_source(
                path.prim_path(),
                path.property(),
                2.,
                InterpolationType::Held
            )
            .is_some()
    );
    let explanation = stage
        .explain_property_value_at_time(path, 2., InterpolationType::Held)
        .unwrap();
    assert_eq!(explanation.source, layerstack::ValueSource::ValueClips);
    assert!(explanation.clip.is_some());
    assert_eq!(
        numeric_array(explanation.value.clone()),
        Some(vec![1000., 1000.])
    );
    let contributors: Vec<_> = explanation
        .opinions
        .iter()
        .filter(|op| matches!(op.role, layerstack::OpinionRole::Contributed(_)))
        .collect();
    assert_eq!(contributors.len(), 1);
    assert_eq!(contributors[0].layer(), LayerId(1));
    assert_eq!(
        contributors[0].role,
        layerstack::OpinionRole::Contributed(layerstack::Contribution::Value)
    );
    assert_eq!(contributors[0].samples.len(), 1);
    assert_eq!(contributors[0].samples[0].time, 0.);
    assert_eq!(
        contributors[0].samples[0].role,
        layerstack::OpinionRole::Contributed(layerstack::Contribution::Value)
    );
    let resolved = stage
        .read_property_with_provenance(path, layerstack::Time::held(2.), |value| {
            Some(value.clone())
        })
        .unwrap();
    assert_eq!(
        numeric_array(Some(resolved.value)),
        Some(vec![1000., 1000.])
    );
    assert_eq!(resolved.provenance.unwrap().layer, LayerId(1));
    if let Some(oracle) = native_with_interpolation(&layers, "/P.x", &[2.], InterpolationType::Held)
    {
        assert_eq!(oracle.values, [Some(vec![1000., 1000.])]);
    }
}

#[test]
fn synthetic_sparse_clip_lower_metadata_advances_the_weaker_sample_query() {
    let layers = vec![
("root.usda".into(), "#usda 1.0\n(subLayers=[@base.usda@])\ndef \"P\" (clips = { dictionary default = {\nasset[] assetPaths=[@a.usda@]\ndouble2[] active=[(5,0)]\nasset manifestAssetPath=@manifest.usda@ \nstring primPath=\"/C\"\n} }) {\nfloat[] x\n}\n".into()),
("a.usda".into(), "#usda 1.0\ndef \"C\" {\nfloat[] x.timeSamples={0:edit[write 100 to [0]],10:edit[write 200 to [0]]}\n}\n".into()),
("manifest.usda".into(), "#usda 1.0\ndef \"C\" {\nfloat[] x\n}\n".into()),
("base.usda".into(), "#usda 1.0\nover \"P\" {\nfloat[] x.timeSamples={2:[2,2],8:[8,8]}\n}\n".into())
    ];
    let (mut store, stage) = check(
        &layers,
        "/P.x",
        &[2., 5., 7., 8., 9.],
        &[
            vec![100., 2.],
            vec![],
            vec![100., 8.],
            vec![100., 8.],
            vec![150., 8.],
        ],
        &[0., 2., 5., 10.],
    );
    let queries = [2., 5., 7., 8., 9.];
    let expected = [
        vec![100., 2.],
        vec![],
        vec![100., 8.],
        vec![100., 8.],
        vec![100., 8.],
    ];
    let path = store.property_path("/P.x");
    for (&time, value) in queries.iter().zip(&expected) {
        let actual = numeric_array(
            stage
                .resolve_property_path_at_time(path, time, InterpolationType::Held)
                .map(|r| r.value),
        );
        assert_eq!(actual.as_ref(), Some(value), "held time {time}");
    }
    if let Some(native) =
        native_with_interpolation(&layers, "/P.x", &queries, InterpolationType::Held)
    {
        assert_eq!(
            native.values,
            expected.into_iter().map(Some).collect::<Vec<_>>()
        );
    }
}
