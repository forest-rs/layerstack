// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Runtime clip resolution against independently opened OpenUSD stages.
//! AOUSD Core §12.3.4; OpenUSD `usd/clip.cpp`, `clipSet.cpp`, and `stage.cpp`.
//! The fixed expectations run without Python; set `LAYERSTACK_USD_PYTHON` to
//! pinned OpenUSD 26.08 to also execute the differential oracle.

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
fn scalar(value: Option<Value>) -> Option<f64> {
    match value {
        None | Some(Value::Blocked) => None,
        Some(Value::Double(v) | Value::TimeCode(v)) => Some(v),
        Some(Value::Float(v)) => Some(f64::from(v)),
        other => panic!("expected numeric value, got {other:?}"),
    }
}
fn close(actual: Option<f64>, expected: Option<f64>, label: &str) {
    match (actual, expected) {
        (Some(a), Some(b)) => assert!(
            (a - b).abs() <= 1e-10 * b.abs().max(1.),
            "{label}: {a} != {b}"
        ),
        _ => assert_eq!(actual, expected, "{label}"),
    }
}
#[derive(Deserialize)]
struct Oracle {
    version: String,
    values: Vec<Option<f64>>,
    default: Option<f64>,
    samples: Vec<f64>,
}
fn python() -> Option<&'static str> {
    static PYTHON: OnceLock<Option<String>> = OnceLock::new();
    PYTHON
        .get_or_init(|| {
            let explicit = std::env::var("LAYERSTACK_USD_PYTHON").ok();
            let python = explicit.clone().unwrap_or_else(|| "python3".into());
            let available = Command::new(&python)
                .args(["-c", "from pxr import Usd"])
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(
                explicit.is_none() || available,
                "explicit LAYERSTACK_USD_PYTHON cannot import pxr"
            );
            available.then_some(python)
        })
        .as_deref()
}
#[allow(
    clippy::too_many_arguments,
    reason = "one differential fixture supplies both query and expected result"
)]
fn check(
    name: &str,
    layers: Layers,
    path: &str,
    queries: &[f64],
    expected: &[Option<f64>],
    default: Option<f64>,
    samples: &[f64],
    schema: bool,
    held: bool,
) {
    assert_eq!(
        queries.len(),
        expected.len(),
        "every query needs a fixed expectation"
    );
    let (mut store, stage) = compose(&layers);
    let property = store.property_path(path);
    let prim = property.prim_path();
    let field = property.property();
    let interpolation = if held {
        InterpolationType::Held
    } else {
        InterpolationType::Linear
    };
    let actual: Vec<_> = queries
        .iter()
        .map(|&t| {
            scalar(
                if schema {
                    stage.resolve_value_at_time_with_schema(prim, field, t, interpolation, &store)
                } else {
                    stage.resolve_property_path_at_time(property, t, interpolation)
                }
                .map(|r| r.value),
            )
        })
        .collect();
    let actual_default = scalar(if schema {
        stage
            .resolve_field_with_schema(prim, field, &store)
            .map(|r| r.value)
    } else {
        stage
            .resolve_property_path(property)
            .map(|r| match r.value {
                layerstack::ResolvedValue::Scalar(value) => value,
                other => panic!("unexpected default {other:?}"),
            })
    });
    if schema {
        assert!(
            !stage.authored_property_names(prim, &store).contains(&field),
            "clip layers must not introduce authored stage properties"
        );
        assert!(
            stage.property_names(prim, &store).contains(&field),
            "schema definition supplies the attribute"
        );
        let scene = layerstack_schemas::Scene::new(&stage, &store);
        let sphere = layerstack_schemas::usd_geom::Sphere::new(&scene, prim)
            .expect("schema fixture is a Sphere");
        for (&time, &value) in queries.iter().zip(expected) {
            let interpolation = if held {
                InterpolationType::Held
            } else {
                InterpolationType::Linear
            };
            close(
                sphere.radius_at(time, interpolation),
                value,
                &format!("{name} schema view at {time:?}"),
            );
        }
        close(
            sphere.radius(),
            default,
            &format!("{name} schema view default"),
        );
    }
    let actual_samples = stage.property_sample_times(prim, field);
    if let Some(python) = python() {
        let dir = std::env::temp_dir().join(format!(
            "layerstack-runtime-clips-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in &layers {
            let file = dir.join(file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        let output=Command::new(python).arg("-c").arg(r#"
import json,sys
from pxr import Usd
stage=Usd.Stage.Open(sys.argv[1])
assert stage
if sys.argv[4]=='held': stage.SetInterpolationType(Usd.InterpolationTypeHeld)
attr=stage.GetAttributeAtPath(sys.argv[2]); assert attr
number=lambda value: None if value is None else float(value)
v=Usd.GetVersion()
print(json.dumps({'version':f'{v[1]}.{v[2]}','values':[number(attr.Get(t)) for t in json.loads(sys.argv[3])], 'default':number(attr.Get()),'samples':attr.GetTimeSamples()}))
"#).arg(dir.join(&layers[0].0)).arg(path).arg(serde_json::to_string(queries).unwrap()).arg(if held {"held"} else {"linear"}).output().unwrap();
        assert!(
            output.status.success(),
            "{name} oracle: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let oracle: Oracle = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            oracle.version,
            layerstack_schemas::OPENUSD_VERSION,
            "oracle must match the pinned schema release"
        );
        for ((&a, &b), t) in oracle.values.iter().zip(expected).zip(queries) {
            close(a, b, &format!("{name} oracle at {t}"));
        }
        close(oracle.default, default, &format!("{name} oracle default"));
        compare_samples(&oracle.samples, samples, &format!("{name} oracle"));
        std::fs::remove_dir_all(dir).unwrap();
    }
    for ((a, &b), t) in actual.into_iter().zip(expected).zip(queries) {
        close(a, b, &format!("{name} Rust at {t}"));
    }
    close(actual_default, default, &format!("{name} Rust default"));
    compare_samples(&actual_samples, samples, &format!("{name} Rust"));
}
fn compare_samples(actual: &[f64], expected: &[f64], label: &str) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{label}: sample counts {actual:?} != {expected:?}"
    );
    for (&a, &b) in actual.iter().zip(expected) {
        assert!((a - b).abs() <= 1e-12, "{label}: sample {a} != {b}");
    }
}
fn text(body: &str) -> String {
    format!("#usda 1.0\n{body}\n")
}
fn clips(extra: &str) -> String {
    format!(
        r#"clips = {{
 dictionary default = {{
  asset[] assetPaths = [@a.usda@, @b.usda@]
  string primPath = "/C"
  double2[] active = [(0, 0), (10, 1)]
  asset manifestAssetPath = @manifest.usda@
  {extra}
 }}
}}"#
    )
}
fn basic(metadata: &str, attribute: &str, a: &str, b: &str, manifest: &str) -> Layers {
    vec![
        (
            "root.usda".into(),
            text(&format!("def \"P\" (\n{metadata}\n) {{\n{attribute}\n}}")),
        ),
        ("a.usda".into(), text(&format!("def \"C\" {{\n{a}\n}}"))),
        ("b.usda".into(), text(&format!("def \"C\" {{\n{b}\n}}"))),
        (
            "manifest.usda".into(),
            text(&format!("def \"C\" {{\n{manifest}\n}}")),
        ),
    ]
}
#[test]
fn activation_boundaries_interpolate_and_held_queries_do_not() {
    let layers = basic(
        &clips(""),
        "double x = 7",
        "double x.timeSamples = {0: 0}",
        "double x.timeSamples = {10: 100}",
        "double x",
    );
    // Same-anchor default wins; clips never contribute default-time opinions.
    check(
        "same-anchor-default",
        layers.clone(),
        "/P.x",
        &[0., 5., 10.],
        &[Some(7.); 3],
        Some(7.),
        &[],
        false,
        false,
    );
    let mut layers = layers;
    layers[0].1 = layers[0].1.replace("double x = 7", "double x");
    check(
        "switch-linear",
        layers.clone(),
        "/P.x",
        &[-1., 0., 2., 5., 9., 10., 20.],
        &[
            Some(0.),
            Some(0.),
            Some(20.),
            Some(50.),
            Some(90.),
            Some(100.),
            Some(100.),
        ],
        None,
        &[0., 10.],
        false,
        false,
    );
    check(
        "switch-held",
        layers,
        "/P.x",
        &[0., 5., 9., 10.],
        &[Some(0.), Some(0.), Some(0.), Some(100.)],
        None,
        &[0., 10.],
        false,
        true,
    );
}
#[test]
fn gaps_manifest_defaults_and_blocks_are_authoritative() {
    for (name, interpolate, manifest, expected, samples) in [
        (
            "gap-block",
            false,
            "double x",
            vec![Some(0.), None, None, Some(20.)],
            vec![0., 10., 20., 30.],
        ),
        (
            "gap-interpolate",
            true,
            "double x",
            vec![Some(5.), Some(10.), Some(15.), Some(20.)],
            vec![0., 20., 30.],
        ),
        (
            "gap-default",
            true,
            "double x = 100",
            vec![Some(50.), Some(100.), Some(60.), Some(20.)],
            vec![0., 10., 20., 30.],
        ),
    ] {
        let metadata = clips(&format!(
            "\nbool interpolateMissingClipValues = {interpolate}\n"
        ))
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@, @gap.usda@, @b.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0), (10, 1), (20, 2)]");
        let mut layers = basic(
            &metadata,
            "double x",
            "double x.timeSamples = {0: 0, 10: 10}",
            "double x.timeSamples = {20: 20, 30: 30}",
            manifest,
        );
        layers.push(("gap.usda".into(), text("def \"C\" {\n double x = 999\n}")));
        check(
            name,
            layers,
            "/P.x",
            &[5., 10., 15., 20.],
            &expected,
            None,
            &samples,
            false,
            false,
        );
    }
}
#[test]
fn eligible_clip_gaps_and_sample_blocks_do_not_use_schema_fallback() {
    for (name, clip, manifest, missing_asset, expected, samples) in [
        (
            "schema-gap",
            "double radius = 999",
            "double radius",
            false,
            vec![None, None, None],
            vec![0.],
        ),
        (
            "schema-block",
            "double radius.timeSamples = {0: None, 10: 10}",
            "double radius",
            false,
            vec![None, None, Some(10.)],
            vec![0., 10.],
        ),
        (
            "schema-missing-asset",
            "double radius",
            "double radius",
            true,
            vec![None, None, None],
            vec![0.],
        ),
        (
            "schema-missing-default",
            "double radius",
            "double radius = 77",
            true,
            vec![Some(77.); 3],
            vec![0.],
        ),
    ] {
        let metadata = clips("")
            .replace(
                "[@a.usda@, @b.usda@]",
                if missing_asset {
                    "[@missing.usda@]"
                } else {
                    "[@a.usda@]"
                },
            )
            .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
        let mut layers = basic(&metadata, "", clip, "", "double radius");
        layers[0].1 = layers[0].1.replace("def \"P\"", "def Sphere \"P\"");
        layers[3].1 = text(&format!("def \"C\" {{\n{manifest}\n}}"));
        check(
            name,
            layers,
            "/P.radius",
            &[0., 5., 10.],
            &expected,
            Some(1.),
            &samples,
            true,
            false,
        );
    }
}
#[test]
fn manifest_eligibility_and_auto_generation_control_weak_fallthrough() {
    for (name, manifest, manifest_path, automatic, expected, samples) in [
        (
            "manifest-absent-property",
            "",
            "manifest.usda",
            false,
            55.,
            vec![],
        ),
        (
            "manifest-uniform",
            "uniform double x",
            "manifest.usda",
            false,
            55.,
            vec![],
        ),
        (
            "manifest-missing",
            "double x",
            "missing.usda",
            false,
            55.,
            vec![],
        ),
        (
            "manifest-auto",
            "",
            "manifest.usda",
            true,
            5.,
            vec![0., 10.],
        ),
    ] {
        let mut metadata = clips("")
            .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
            .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
        if automatic {
            metadata = metadata.replace("asset manifestAssetPath = @manifest.usda@", "");
        } else {
            metadata = metadata.replace("@manifest.usda@", &format!("@{manifest_path}@"));
        }
        let mut layers = basic(
            &metadata,
            "double x",
            "double x.timeSamples = {0: 0, 10: 10}",
            "",
            manifest,
        );
        layers[0].1 =
            layers[0]
                .1
                .replacen("#usda 1.0", "#usda 1.0\n(subLayers = [@weak.usda@])", 1);
        layers.push(("weak.usda".into(), text("over \"P\" {\n double x = 55\n}")));
        check(
            name,
            layers,
            "/P.x",
            &[5.],
            &[Some(expected)],
            Some(55.),
            &samples,
            false,
            false,
        );
    }
}
#[test]
fn clip_times_support_discontinuities_reversal_holds_and_timecode_values() {
    let safe_step = 4.440_892_098_500_626e-9;
    for (name, times, queries, expected, samples) in [
        (
            "jump",
            "[(0,0),(10,10),(10,0),(20,10)]",
            vec![-1., 5., 10., 15., 25.],
            vec![0., 5., 0., 5., 10.],
            vec![0., 10. - safe_step, 10., 20.],
        ),
        (
            "reverse",
            "[(0,10),(10,0)]",
            vec![-1., 0., 3., 10., 11.],
            vec![10., 10., 7., 0., 0.],
            vec![0., 10.],
        ),
        (
            "hold",
            "[(0,5),(10,5)]",
            vec![-1., 0., 3., 10., 11.],
            vec![5.; 5],
            vec![0., 10.],
        ),
        (
            "loop",
            "[(0,0),(10,10),(20,0)]",
            vec![-1., 5., 15., 21.],
            vec![0., 5., 5., 0.],
            vec![0., 10., 20.],
        ),
    ] {
        let metadata = clips(&format!("\ndouble2[] times = {times}\n"))
            .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
            .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
        let layers = basic(
            &metadata,
            "double x",
            "double x.timeSamples = {0: 0, 10: 10}",
            "",
            "double x",
        );
        check(
            name,
            layers,
            "/P.x",
            &queries,
            &expected.into_iter().map(Some).collect::<Vec<_>>(),
            None,
            &samples,
            false,
            false,
        );
    }
    let metadata = clips("\ndouble2[] times = [(0,0),(20,10)]\n")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let layers = basic(
        &metadata,
        "timecode x",
        "timecode x.timeSamples = {0: 100, 10: 110}",
        "",
        "timecode x",
    );
    check(
        "timecode-shift",
        layers,
        "/P.x",
        &[0., 10., 20.],
        &[Some(100.), Some(110.), Some(120.)],
        None,
        &[0., 20.],
        false,
        false,
    );
}

#[test]
fn ancestral_sets_are_not_dictionary_inherited_or_deleted_by_child_metadata() {
    let metadata = clips("")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let mut layers = basic(
        &metadata,
        "",
        "def \"Q\" {\n double x.timeSamples = {0:0,10:10}\n}",
        "",
        "def \"Q\" {\n double x\n}",
    );
    layers[0].1 = text(&format!(
        r#"def "P" (
{metadata}
) {{
 def "Q" (
  clips = {{
   dictionary default = {{ double2[] times = [(0,10),(10,0)] }}
  }}
  delete clipSets = ["default"]
 ) {{
  double x
 }}
}}"#
    ));
    check(
        "ancestral-sparse-override",
        layers,
        "/P/Q.x",
        &[0., 2., 5., 10.],
        &[Some(0.), Some(2.), Some(5.), Some(10.)],
        None,
        &[0., 10.],
        false,
        false,
    );
}

#[test]
fn anchor_strength_and_stronger_time_override_preserve_relative_asset_context() {
    let layers = vec![
        (
            "root.usda".into(),
            text(
                r#"(subLayers = [@nested/anchor.usda@ (offset = 20; scale = 2)])
over "P" (
 clips = {
  dictionary default = {
   double2[] times = [(20,0),(40,10)]
   asset manifestAssetPath = @manifest.usda@
  }
 }
) {}
"#,
            ),
        ),
        (
            "nested/anchor.usda".into(),
            text(
                r#"def "P" (
 references = @topology.usda@</T>
 clips = {
  dictionary default = {
   asset[] assetPaths = [@clip.usda@]
   string primPath = "/C"
   double2[] active = [(0,0)]
   double2[] times = [(0,0),(10,10)]
  }
 }
) {}
"#,
            ),
        ),
        (
            "nested/topology.usda".into(),
            text("def \"T\" {\n double x = 55\n}"),
        ),
        (
            "nested/clip.usda".into(),
            text("def \"C\" {\n double x.timeSamples = {0:0,10:10}\n}"),
        ),
        (
            "nested/manifest.usda".into(),
            text("def \"C\" {\n double x\n}"),
        ),
        (
            "clip.usda".into(),
            text("def \"C\" {\n double x.timeSamples = {0:999,10:999}\n}"),
        ),
        (
            "manifest.usda".into(),
            text("def \"C\" {\n uniform double x\n}"),
        ),
    ];
    check(
        "anchor-relative-assets",
        layers,
        "/P.x",
        &[0., 20., 30., 40., 50.],
        &[Some(0.), Some(0.), Some(5.), Some(10.), Some(10.)],
        Some(55.),
        &[20., 40.],
        false,
        false,
    );
}

#[test]
fn reference_offsets_map_authored_external_times_but_not_implicit_identity() {
    for authored_times in [false, true] {
        let times = if authored_times {
            "double2[] times = [(0,0),(10,10)]"
        } else {
            ""
        };
        let layers = vec![
            (
                "root.usda".into(),
                text(
                    "def \"World\" (\n references = @model.usda@</P> (offset = 20; scale = 2)\n) {}",
                ),
            ),
            (
                "model.usda".into(),
                text(&format!(
                    r#"def "P" (
 clips = {{
  dictionary default = {{
   asset[] assetPaths = [@clip.usda@]
   string primPath = "/C"
   double2[] active = [(0,0)]
   {times}
  }}
 }}
) {{
 double x
}}"#
                )),
            ),
            (
                "clip.usda".into(),
                text("def \"C\" {\n double x.timeSamples = {0:0,10:10}\n}"),
            ),
        ];
        if authored_times {
            check(
                "reference-authored-times",
                layers,
                "/World.x",
                &[0., 5., 20., 30., 40.],
                &[Some(0.), Some(0.), Some(0.), Some(5.), Some(10.)],
                None,
                &[20., 40.],
                false,
                false,
            );
        } else {
            check(
                "reference-identity-times",
                layers,
                "/World.x",
                &[0., 5., 20., 30., 40.],
                &[Some(0.), Some(5.), Some(10.), Some(10.), Some(10.)],
                None,
                &[0., 10., 20.],
                false,
                false,
            );
        }
    }
}

#[test]
fn stronger_default_beats_clip_anchor_even_when_timing_is_authored_above_it() {
    let mut layers = basic(
        &clips(""),
        "double x",
        "double x.timeSamples = {0:0}",
        "double x.timeSamples = {10:100}",
        "double x",
    );
    layers[0].0 = "anchor.usda".into();
    layers.insert(
        0,
        (
            "root.usda".into(),
            text(
                r#"(subLayers = [@middle.usda@])
over "P" (
 clips = { dictionary default = { double2[] times = [(0,0),(10,10)] } }
) {}
"#,
            ),
        ),
    );
    layers.push((
        "middle.usda".into(),
        text("(subLayers = [@anchor.usda@])\nover \"P\" {\n double x = 23\n}"),
    ));
    check(
        "stronger-default",
        layers,
        "/P.x",
        &[0., 5., 10.],
        &[Some(23.); 3],
        Some(23.),
        &[],
        false,
        false,
    );
}

#[test]
fn clip_set_order_and_membership_compose_over_weaker_dictionary_definitions() {
    for (name, list_op, expected) in [
        ("set-lexical", "", 10.),
        ("set-reorder", "reorder clipSets = [\"b\", \"a\"]", 20.),
        ("set-delete", "delete clipSets = [\"a\"]", 20.),
        ("set-clear", "clipSets = []", 55.),
    ] {
        let layers = vec![
            (
                "root.usda".into(),
                text(&format!(
                    "(subLayers = [@definitions.usda@, @weak.usda@])\nover \"P\" (\n{list_op}\n) {{}}"
                )),
            ),
            (
                "definitions.usda".into(),
                text(
                    r#"def "P" (
 clips = {
  dictionary a = {
   asset[] assetPaths = [@a.usda@]
   string primPath = "/C"
   double2[] active = [(0,0)]
  }
  dictionary b = {
   asset[] assetPaths = [@b.usda@]
   string primPath = "/C"
   double2[] active = [(0,0)]
  }
 }
) {
 double x
}"#,
                ),
            ),
            ("weak.usda".into(), text("over \"P\" {\n double x = 55\n}")),
            (
                "a.usda".into(),
                text("def \"C\" {\n double x.timeSamples = {0:10}\n}"),
            ),
            (
                "b.usda".into(),
                text("def \"C\" {\n double x.timeSamples = {0:20}\n}"),
            ),
        ];
        check(
            name,
            layers,
            "/P.x",
            &[0., 5.],
            &[Some(expected); 2],
            Some(55.),
            if list_op == "clipSets = []" {
                &[]
            } else {
                &[0.]
            },
            false,
            false,
        );
    }
}

#[test]
fn missing_or_ineligible_manifests_allow_schema_fallback() {
    for (name, automatic, manifest_path, manifest) in [
        ("schema-auto-empty", true, "manifest.usda", "double radius"),
        ("schema-absent-property", false, "manifest.usda", ""),
        (
            "schema-uniform",
            false,
            "manifest.usda",
            "uniform double radius",
        ),
        (
            "schema-missing-manifest",
            false,
            "missing.usda",
            "double radius",
        ),
    ] {
        let mut metadata = clips("")
            .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
            .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
        if automatic {
            metadata = metadata.replace("asset manifestAssetPath = @manifest.usda@", "");
        } else {
            metadata = metadata.replace("@manifest.usda@", &format!("@{manifest_path}@"));
        }
        let mut layers = basic(&metadata, "", "double radius = 999", "", manifest);
        layers[0].1 = layers[0].1.replace("def \"P\"", "def Sphere \"P\"");
        check(
            name,
            layers,
            "/P.radius",
            &[0., 5., 10.],
            &[Some(1.); 3],
            Some(1.),
            &[],
            true,
            false,
        );
    }
}

#[test]
fn manifest_activation_blocks_exclude_clip_samples_only_when_interpolating_gaps() {
    for interpolate in [false, true] {
        let metadata = clips(&format!(
            "\nbool interpolateMissingClipValues = {interpolate}\n"
        ))
        .replace(
            "[@a.usda@, @b.usda@]",
            "[@a.usda@, @middle.usda@, @b.usda@]",
        )
        .replace("[(0, 0), (10, 1)]", "[(0, 0), (10, 1), (20, 2)]");
        let mut layers = basic(
            &metadata,
            "double x",
            "double x.timeSamples = {0:0}",
            "double x.timeSamples = {20:20}",
            "double x.timeSamples = {10: None}",
        );
        layers.push((
            "middle.usda".into(),
            text("def \"C\" {\n double x.timeSamples = {10:100}\n}"),
        ));
        check(
            if interpolate {
                "manifest-block-enabled"
            } else {
                "manifest-block-disabled"
            },
            layers,
            "/P.x",
            &[5., 10., 15.],
            if interpolate {
                &[Some(5.), Some(10.), Some(15.)]
            } else {
                &[Some(50.), Some(100.), Some(60.)]
            },
            None,
            if interpolate {
                &[0., 20.]
            } else {
                &[0., 10., 20.]
            },
            false,
            false,
        );
    }
}

fn live(store: &mut InMemoryStore) -> layerstack::LiveStage {
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    layerstack::LiveStage::compose(
        store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    )
}
fn at(stage: &Stage, property: layerstack::PropertyPath, time: f64) -> Option<f64> {
    scalar(
        stage
            .resolve_property_path_at_time(property, time, InterpolationType::Linear)
            .map(|r| r.value),
    )
}
#[test]
fn clip_sources_explain_both_actual_layers_and_do_not_define_properties() {
    let layers = basic(
        &clips(""),
        "double x",
        "double x.timeSamples = {0:0}",
        "double x.timeSamples = {10:100}",
        "double x",
    );
    let (mut store, stage) = compose(&layers);
    let property = store.property_path("/P.x");
    let source = stage
        .property_clip_source(
            property.prim_path(),
            property.property(),
            5.,
            InterpolationType::Linear,
        )
        .unwrap();
    assert_eq!(source.anchor_layer, LayerId(1));
    assert_eq!(source.lower.layer, Some(LayerId(2)));
    assert_eq!(source.upper.layer, Some(LayerId(3)));
    assert_eq!(source.lower.stage_time, 0.);
    assert_eq!(source.upper.stage_time, 10.);
    let raw = store.property_path("/C.x");
    assert_eq!(
        source.lower.spec_path,
        layerstack::SpecPath::from_property_path(raw, &store.paths)
    );
    let resolved = stage
        .resolve_property_path_at_time(property, 5., InterpolationType::Linear)
        .unwrap();
    assert_eq!(resolved.value, Value::Double(50.));
    let explanation = stage
        .explain_property_value_at_time(property, 5., InterpolationType::Linear)
        .unwrap();
    assert_eq!(explanation.clip, Some(source));
    assert!(matches!(
        explanation.source,
        layerstack::ValueSource::ValueClips
    ));

    let mut undeclared = layers;
    undeclared[0].1 = undeclared[0].1.replace("double x", "");
    let (mut store, stage) = compose(&undeclared);
    let property = store.property_path("/P.x");
    assert!(!stage.has_property_path(property));
    assert!(
        stage
            .property_sample_times(property.prim_path(), property.property())
            .is_empty()
    );
    assert_eq!(at(&stage, property, 5.), None);
    assert!(
        stage
            .property_clip_source(
                property.prim_path(),
                property.property(),
                5.,
                InterpolationType::Linear
            )
            .is_none()
    );
}

#[test]
fn live_raw_clip_and_manifest_edits_refresh_descendants_but_preserve_snapshots() {
    let metadata = clips("")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let mut layers = basic(
        &metadata,
        "def \"Q\" {\n double x\n}",
        "def \"Q\" {\n double x.timeSamples = {0:0,10:10}\n}",
        "",
        "def \"Q\" {\n double x\n}",
    );
    layers[0]
        .1
        .push_str("\ndef \"Unrelated\" {\n double x = 99\n}\n");
    let (mut store, snapshot) = compose(&layers);
    let mut live = live(&mut store);
    let property = store.property_path("/P/Q.x");
    let raw = store.property_path("/C/Q.x");
    let unrelated = store.path("/Unrelated");
    assert_eq!(at(live.stage(), property, 5.), Some(5.));
    store
        .layers
        .get_mut(&LayerId(2))
        .unwrap()
        .property_mut(raw)
        .unwrap()
        .time_samples = Some(vec![(0., Value::Double(0.)), (10., Value::Double(50.))].into());
    assert_eq!(
        at(&snapshot, property, 5.),
        Some(5.),
        "prepared snapshot owns raw clip samples"
    );
    assert_eq!(
        at(live.stage(), property, 5.),
        Some(5.),
        "live stage updates explicitly"
    );
    assert_eq!(live.notify_changed_layers(&store), [LayerId(2)]);
    let changed = live.recompose(&mut store);
    assert!(
        changed.contains(&property.prim_path()),
        "affected descendant is recomposed: {changed:?}"
    );
    assert!(
        !changed.contains(&unrelated),
        "unrelated prim must not be rebuilt"
    );
    assert_eq!(at(live.stage(), property, 5.), Some(25.));
    assert_eq!(at(&snapshot, property, 5.), Some(5.));
    store
        .layers
        .get_mut(&LayerId(4))
        .unwrap()
        .property_mut(raw)
        .unwrap()
        .variability = layerstack::Variability::Uniform;
    assert_eq!(live.notify_changed_layers(&store), [LayerId(4)]);
    let changed = live.recompose(&mut store);
    assert!(changed.contains(&property.prim_path()));
    assert!(!changed.contains(&unrelated));
    assert_eq!(
        at(live.stage(), property, 5.),
        None,
        "uniform manifest no longer supplies values"
    );
    assert_eq!(at(&snapshot, property, 5.), Some(5.));
}

#[test]
fn live_anchor_metadata_retimes_and_new_asset_bindings_become_visible() {
    let metadata = clips("\ndouble2[] times = [(0,0),(10,10)]\n")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let layers = basic(
        &metadata,
        "double x",
        "double x.timeSamples = {0:0,10:10}",
        "double x.timeSamples = {0:42}",
        "double x",
    );
    let (mut store, _) = compose(&layers);
    let mut live = live(&mut store);
    let property = store.property_path("/P.x");
    let clips_token = store.tokens.lookup("clips").unwrap();
    let layer = store.layers.get_mut(&LayerId(1)).unwrap();
    let prim = layer.prims.get_mut(&property.prim_path()).unwrap();
    let layerstack::FieldValue::Value(Value::Dictionary(mut sets)) =
        prim.field(clips_token).unwrap().clone()
    else {
        panic!("clips dictionary")
    };
    let Value::Dictionary(fields) = &mut sets
        .iter_mut()
        .find(|(name, _)| &**name == "default")
        .unwrap()
        .1
    else {
        panic!("default dictionary")
    };
    fields
        .iter_mut()
        .find(|(name, _)| &**name == "times")
        .unwrap()
        .1 = Value::Array(vec![Value::Vec2d([0., 10.]), Value::Vec2d([10., 0.])]);
    prim.set_field(clips_token, Value::Dictionary(sets));
    live.notify_prim_edit(property.prim_path());
    live.recompose(&mut store);
    assert_eq!(at(live.stage(), property, 2.), Some(8.));

    let mut layers = layers;
    layers[0].1 = layers[0].1.replace("@a.usda@", "@late.usda@");
    let (mut store, snapshot) = compose(&layers);
    let mut live = crate::live(&mut store);
    let property = store.property_path("/P.x");
    assert_eq!(at(live.stage(), property, 5.), None);
    assert!(
        live.stage()
            .clip_asset_requests()
            .iter()
            .any(|r| &*r.identifier == "late.usda"
                && matches!(
                    r.status,
                    layerstack::value_clips::ClipAssetStatus::Unavailable(_)
                ))
    );
    store.insert_asset_layer(LayerId(1), "late.usda", LayerId(3));
    assert_eq!(live.notify_changed_layers(&store), [LayerId(1)]);
    live.recompose(&mut store);
    assert_eq!(at(live.stage(), property, 5.), Some(42.));
    assert_eq!(at(&snapshot, property, 5.), None);
    assert!(
        live.stage()
            .clip_asset_requests()
            .iter()
            .any(|r| &*r.identifier == "late.usda"
                && r.status == layerstack::value_clips::ClipAssetStatus::Loaded(LayerId(3)))
    );
}

#[test]
fn spline_clips_evaluate_directly_without_discrete_sample_grids() {
    let metadata = clips("").replace("asset manifestAssetPath = @manifest.usda@", "");
    let layers = basic(
        &metadata,
        "double x",
        "double x.spline = {\n 0:0; post linear,\n 10:10; post held\n}",
        "double x.spline = {\n 10:100; post held\n}",
        "",
    );
    check(
        "spline-switch",
        layers,
        "/P.x",
        &[-1., 0., 5., 9., 10., 15.],
        &[
            Some(0.),
            Some(0.),
            Some(5.),
            Some(9.),
            Some(100.),
            Some(100.),
        ],
        None,
        &[],
        false,
        false,
    );
    let metadata = clips("\ndouble2[] times = [(0,10),(10,0)]\n")
        .replace("asset manifestAssetPath = @manifest.usda@", "")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let layers = basic(
        &metadata,
        "double x",
        "double x.spline = {\n 0:1; post held,\n 5:4 & 5; post linear,\n 10:9 & 10; post held,\n post: held\n}",
        "",
        "",
    );
    check(
        "spline-reverse-prevalues",
        layers,
        "/P.x",
        &[-1., 0., 5., 10., 11.],
        &[Some(10.), Some(9.), Some(1.), Some(1.), Some(1.)],
        None,
        &[],
        false,
        false,
    );
}

#[test]
fn spline_gaps_use_clip_boundaries_and_manifest_defaults_without_sample_interpolation() {
    for (name, interpolate, manifest, expected) in [
        (
            "spline-gap-block",
            false,
            "double x.spline = {}",
            vec![Some(5.), None, None, Some(20.)],
        ),
        (
            "spline-gap-interpolate",
            true,
            "double x.spline = {}",
            vec![Some(5.), Some(10.), Some(15.), Some(20.)],
        ),
        (
            "spline-gap-default",
            true,
            "double x = 100\ndouble x.spline = {}",
            vec![Some(5.), Some(100.), Some(100.), Some(20.)],
        ),
    ] {
        let metadata = clips(&format!(
            "\nbool interpolateMissingClipValues = {interpolate}\n"
        ))
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@, @gap.usda@, @b.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0), (10, 1), (20, 2)]");
        let mut layers = basic(
            &metadata,
            "double x",
            "double x.spline = {\n 0:0; post linear,\n 10:10; post held\n}",
            "double x.spline = {\n 20:20; post linear,\n 30:30; post held\n}",
            manifest,
        );
        layers.push(("gap.usda".into(), text("def \"C\" {\n double x = 999\n}")));
        check(
            name,
            layers,
            "/P.x",
            &[5., 10., 15., 20.],
            &expected,
            None,
            &[],
            false,
            false,
        );
    }
}

#[test]
fn manifest_blocks_are_per_activation_when_an_asset_is_reused() {
    let metadata = clips("\nbool interpolateMissingClipValues = true\n")
        .replace("[(0, 0), (10, 1)]", "[(0, 0), (10, 0), (20, 1)]");
    let layers = basic(
        &metadata,
        "double x",
        "double x.timeSamples = {0:0,10:100}",
        "double x.timeSamples = {20:20}",
        "double x.timeSamples = {10: None}",
    );
    check(
        "reused-asset-manifest-block",
        layers,
        "/P.x",
        &[0., 5., 10., 15., 20.],
        &[Some(0.), Some(5.), Some(10.), Some(15.), Some(20.)],
        None,
        &[0., 20.],
        false,
        false,
    );
}

#[test]
fn manifest_annotation_selects_spline_or_samples_for_the_whole_set() {
    for (name, manifest, expected, samples) in [
        ("manifest-spline", "double x.spline = {}", Some(5.), vec![]),
        ("manifest-default-samples", "double x", None, vec![0.]),
        (
            "manifest-empty-samples-win",
            "double x.spline = {}\ndouble x.timeSamples = {}",
            None,
            vec![0.],
        ),
    ] {
        let metadata = clips("")
            .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
            .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
        let layers = basic(
            &metadata,
            "double x",
            "double x.spline = {\n 0:0; post linear,\n 10:10; post held\n}",
            "",
            manifest,
        );
        check(
            name,
            layers,
            "/P.x",
            &[5.],
            &[expected],
            None,
            &samples,
            false,
            false,
        );
    }
}

#[test]
fn live_newly_selected_resident_clip_is_watched_on_the_next_edit() {
    let metadata = clips("")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let layers = basic(
        &metadata,
        "double x",
        "double x.timeSamples = {0:1}",
        "double x.timeSamples = {0:2}",
        "double x",
    );
    let (mut store, snapshot) = compose(&layers);
    let mut live = live(&mut store);
    let property = store.property_path("/P.x");
    assert_eq!(at(live.stage(), property, 0.), Some(1.));
    let clips_token = store.tokens.lookup("clips").unwrap();
    let prim = store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .prims
        .get_mut(&property.prim_path())
        .unwrap();
    let layerstack::FieldValue::Value(Value::Dictionary(mut sets)) =
        prim.field(clips_token).unwrap().clone()
    else {
        panic!("clips dictionary")
    };
    let Value::Dictionary(fields) = &mut sets
        .iter_mut()
        .find(|(name, _)| &**name == "default")
        .unwrap()
        .1
    else {
        panic!("default dictionary")
    };
    fields
        .iter_mut()
        .find(|(name, _)| &**name == "assetPaths")
        .unwrap()
        .1 = Value::Array(vec![Value::Asset("b.usda".into())]);
    prim.set_field(clips_token, Value::Dictionary(sets));
    live.notify_prim_edit(property.prim_path());
    live.recompose(&mut store);
    assert_eq!(at(live.stage(), property, 0.), Some(2.));
    let raw = store.property_path("/C.x");
    store
        .layers
        .get_mut(&LayerId(3))
        .unwrap()
        .property_mut(raw)
        .unwrap()
        .time_samples = Some(vec![(0., Value::Double(3.))].into());
    assert_eq!(
        live.notify_changed_layers(&store),
        [LayerId(3)],
        "new dependency must be watched after partial recomposition"
    );
    live.recompose(&mut store);
    assert_eq!(at(live.stage(), property, 0.), Some(3.));
    assert_eq!(at(&snapshot, property, 0.), Some(1.));
}

#[test]
fn missing_clip_property_reports_manifest_default_source() {
    let metadata = clips("")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let layers = basic(
        &metadata,
        "double x",
        "double unrelated.timeSamples = {0:1}",
        "",
        "double x = 42",
    );
    let (mut store, stage) = compose(&layers);
    let property = store.property_path("/P.x");
    assert_eq!(at(&stage, property, 0.), Some(42.));
    let source = stage
        .property_clip_source(
            property.prim_path(),
            property.property(),
            0.,
            InterpolationType::Linear,
        )
        .unwrap();
    assert_eq!(source.lower.layer, Some(LayerId(4)));
    assert_eq!(source.upper.layer, Some(LayerId(4)));
    assert_eq!(
        source.lower.spec_path,
        layerstack::SpecPath::from_property_path(store.property_path("/C.x"), &store.paths)
    );
}

#[test]
fn review_sparse_anchor_keeps_authoring_provenance() {
    let metadata = clips("")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let (mut store, _) = compose(&basic(
        &metadata,
        "int[] x",
        "int[] x.timeSamples = {0:[1,2]}",
        "",
        "int[] x",
    ));
    let property = store.property_path("/P.x");
    store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .property_mut(property)
        .unwrap()
        .default = Some(Value::ArrayEdit(layerstack::ArrayEdit {
        ops: vec![layerstack::ArrayEditOp::Write {
            src: layerstack::ArrayEditOperand::Literal(Value::Int(9)),
            index: layerstack::ArrayIndex::Position(0),
        }],
    }));
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );
    let resolved = stage
        .resolve_property_path_at_time(property, 0., InterpolationType::Linear)
        .unwrap();
    assert_eq!(resolved.provenance.unwrap().layer, LayerId(1));
}

#[test]
fn review_invalid_clip_query_does_not_explain_weak_default_as_contributing() {
    let metadata = clips("")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let mut layers = basic(
        &metadata,
        "double x",
        "double x.timeSamples = {0:0,10:10}",
        "",
        "double x",
    );
    layers[0].1 = layers[0]
        .1
        .replacen("#usda 1.0", "#usda 1.0\n(subLayers = [@weak.usda@])", 1);
    layers.push(("weak.usda".into(), text("over \"P\" {\n double x = 55\n}")));
    let (mut store, stage) = compose(&layers);
    let property = store.property_path("/P.x");
    let explanation = stage
        .explain_property_value_at_time(property, f64::NAN, InterpolationType::Linear)
        .unwrap();
    assert_eq!(explanation.value, None);
    assert!(
        explanation
            .opinions
            .iter()
            .all(|o| !matches!(o.role, layerstack::OpinionRole::Contributed(_))),
        "{explanation:?}"
    );
}

#[test]
fn review_variant_clip_anchor_stays_weaker_than_local_sublayer_default() {
    let mut layers = basic(
        &clips("")
            .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
            .replace("[(0, 0), (10, 1)]", "[(0, 0)]"),
        "double x",
        "double x.timeSamples = {0:0,10:10}",
        "",
        "double x",
    );
    let metadata = clips("")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    layers[0].1 = text(&format!(
        r#"(subLayers = [@weak.usda@])
def "P" (
 variants = {{ string mode = "a" }}
 prepend variantSets = "mode"
) {{
 variantSet "mode" = {{
  "a" ({metadata}) {{
   double x
  }}
 }}
}}
"#
    ));
    layers.push(("weak.usda".into(), text("over \"P\" {\n double x = 23\n}")));
    check(
        "review-variant-anchor",
        layers,
        "/P.x",
        &[5.],
        &[Some(23.)],
        Some(23.),
        &[],
        false,
        false,
    );
}

#[test]
fn review_manifest_blocked_spline_reports_manifest_default_source() {
    let metadata = clips("")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let layers = basic(
        &metadata,
        "double x",
        "double x.spline = {\n0:7; post held\n}",
        "",
        "double x = 42\ndouble x.spline = {\n0:0; post held\n}",
    );
    check(
        "review-blocked-spline-source",
        layers.clone(),
        "/P.x",
        &[5.],
        &[Some(42.)],
        None,
        &[],
        false,
        false,
    );
    let (mut store, stage) = compose(&layers);
    let property = store.property_path("/P.x");
    let source = stage
        .property_clip_source(
            property.prim_path(),
            property.property(),
            5.,
            InterpolationType::Linear,
        )
        .unwrap();
    assert_eq!(source.lower.layer, Some(LayerId(4)));
}

#[test]
fn ancestral_variant_clips_apply_without_variant_child_specs() {
    let metadata = clips("")
        .replace("[@a.usda@, @b.usda@]", "[@a.usda@]")
        .replace("[(0, 0), (10, 1)]", "[(0, 0)]");
    let mut layers = basic(
        "",
        "",
        "def \"Q\" {\n double x.timeSamples = {0:0,10:10}\n}",
        "",
        "def \"Q\" {\n double x\n}",
    );
    layers[0].1 = text(&format!(
        r#"
def "P" (
 variants = {{ string mode = "a" }}
 prepend variantSets = "mode"
) {{
 def "Q" {{
  double x
 }}
 variantSet "mode" = {{
  "a" ({metadata}) {{}}
 }}
}}
"#
    ));
    check(
        "ancestral-variant-no-spec",
        layers.clone(),
        "/P/Q.x",
        &[5.],
        &[Some(5.)],
        None,
        &[0., 10.],
        false,
        false,
    );
    layers[0].0 = "model.usda".into();
    layers.insert(
        0,
        (
            "root.usda".into(),
            text("def \"World\" (references = @model.usda@</P>) {}"),
        ),
    );
    check(
        "referenced-ancestral-variant-no-spec",
        layers.clone(),
        "/World/Q.x",
        &[5.],
        &[Some(5.)],
        None,
        &[0., 10.],
        false,
        false,
    );
    layers[1].1 = layers[1].1.replace(" def \"Q\" {\n  double x\n }\n", "");
    layers[0].1 =
        text("def \"World\" (references = @model.usda@</P>) {\n def \"Q\" {\n double x\n }\n}");
    check(
        "referenced-variant-no-source-child-spec",
        layers.clone(),
        "/World/Q.x",
        &[5.],
        &[Some(5.)],
        None,
        &[0., 10.],
        false,
        false,
    );
    layers[1].1 = text(&format!("def \"P\" ({metadata}) {{}}"));
    check(
        "referenced-clips-no-source-child-spec",
        layers,
        "/World/Q.x",
        &[5.],
        &[Some(5.)],
        None,
        &[0., 10.],
        false,
        false,
    );
}

#[test]
fn animation_blocks_stop_clips_but_keep_weaker_defaults() {
    let mut layers = basic(
        &clips(""),
        "double x = AnimationBlock",
        "double x.timeSamples = {0:0, 10:100}",
        "double x.timeSamples = {0:200, 10:300}",
        "double x",
    );
    layers[0].1 = layers[0]
        .1
        .replacen("#usda 1.0", "#usda 1.0\n(subLayers = [@weak.usda@])", 1);
    layers.push((
        "weak.usda".into(),
        text("def \"P\" {\n double x = 55\n double x.timeSamples = {0:1, 10:2}\n}"),
    ));
    check(
        "animation-block",
        layers,
        "/P.x",
        &[0., 5., 10., 15.],
        &[Some(55.); 4],
        Some(55.),
        &[],
        false,
        false,
    );
}
