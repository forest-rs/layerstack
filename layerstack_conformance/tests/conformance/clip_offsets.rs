// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Value clip timing when a clip set's metadata comes from layers with
//! different offsets, against usd-core 26.11.
//!
//! OpenUSD 26.11 places a clip set in the time of the strongest layer of
//! its anchoring node that authors `active` (or the template, or the asset
//! paths when no stronger layer authors `active`), and re-expresses `times`
//! read in another layer through `_ApplyOffsetToTimes`
//! (`pxr/usd/usd/clipSetDefinition.cpp`). Each prim here splits its clip
//! metadata across two sublayers, one shifted and one scaled, and every
//! read and sample time is compared.
//!
//! Spec: AOUSD Core §12.3.4 (value clips), §12.3.2.1 (layer offsets).
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use layerstack::{StageOptions, Time};
use layerstack_io::{IoError, IoErrorKind, StageDocument, Storage};
use serde_json::{Map, Value as Json, json};

use crate::support::layer_json;

/// Clip metadata for one layer: asset paths, `active` and the primary path.
const ASSETS: &str = r#"asset[] assetPaths = [@clip.usda@]
            string primPath = "/Q""#;

/// One prim's clip metadata, split between the stronger and the weaker
/// sublayer.
struct Case {
    name: &'static str,
    strong: String,
    weak: String,
}

/// A sublayer authoring `fields(case)` as each case's default clip set.
fn sublayer(cases: &[Case], fields: impl Fn(&Case) -> &str) -> String {
    let mut text = String::from("#usda 1.0\n");
    for case in cases {
        let fields = fields(case);
        if fields.is_empty() {
            continue;
        }
        text.push_str(&format!(
            "over \"{}\" (\n    clips = {{\n        dictionary default = {{\n            {fields}\n        }}\n    }}\n)\n{{\n}}\n\n",
            case.name
        ));
    }
    text
}

/// The scene's layers, by file name.
fn layers() -> Vec<(&'static str, String)> {
    let times = "double2[] times = [(0, 0), (10, 20)]";
    let active = "double2[] active = [(0, 0)]";
    let template = r#"string templateAssetPath = "clipt.#.usda"
            double templateStartTime = 0
            double templateEndTime = 10
            double templateStride = 10
            string primPath = "/Q""#;
    let case = |name, strong: &str, weak: &str| Case {
        name,
        strong: strong.to_owned(),
        weak: weak.to_owned(),
    };
    let assets_active = format!("{ASSETS}\n            {active}");
    let assets_times = format!("{ASSETS}\n            {times}");
    let cases = [
        case("Same", &format!("{assets_active}\n            {times}"), ""),
        case("TimesWeaker", &assets_active, times),
        case("TimesStronger", times, &assets_active),
        case("ActiveStronger", active, &assets_times),
        case("TemplateTimesStronger", times, template),
    ];
    let mut root = String::from(
        "#usda 1.0\n(\n    subLayers = [\n        @strong.usda@ (offset = 10),\n        @weak.usda@ (scale = 2)\n    ]\n)\n\n",
    );
    for case in &cases {
        root.push_str(&format!(
            "def \"{}\"\n{{\n    custom double x\n}}\n\n",
            case.name
        ));
    }
    let clip = |base: f64| {
        format!(
            "#usda 1.0\ndef \"Q\"\n{{\n    double x.timeSamples = {{\n        0: {base},\n        100: {},\n    }}\n}}\n",
            base + 100.
        )
    };
    vec![
        ("root.usda", root),
        ("strong.usda", sublayer(&cases, |case| &case.strong)),
        ("weak.usda", sublayer(&cases, |case| &case.weak)),
        ("clip.usda", clip(0.)),
        ("clipt.0.usda", clip(1000.)),
        ("clipt.10.usda", clip(2000.)),
    ]
}

/// The attributes read, by path.
const ATTRIBUTES: &[&str] = &[
    "/Same.x",
    "/TimesWeaker.x",
    "/TimesStronger.x",
    "/ActiveStronger.x",
    "/TemplateTimesStronger.x",
];

/// The stage times each attribute is read at.
const TIMES: &[f64] = &[
    -20., -10., -5., 0., 2.5, 5., 7.5, 10., 12.5, 15., 20., 25., 30., 40., 50., 60.,
];

/// usd-core's reads: per attribute, its value at each of [`TIMES`] (keyed by
/// `repr(float(t))`) and its time samples.
const ORACLE: &str = r#"
import json, sys
from pxr import Usd

stage = Usd.Stage.Open(sys.argv[1])
times = json.loads(sys.argv[2])
out = {}
for path in json.loads(sys.argv[3]):
    attr = stage.GetAttributeAtPath(path)
    reads = {repr(float(t)): attr.Get(t) for t in times}
    reads['samples'] = list(attr.GetTimeSamples())
    out[path] = reads
print(json.dumps(out, sort_keys=True))
"#;

/// The scene's files, in memory.
#[derive(Clone, Debug, Default)]
struct Files(BTreeMap<String, Vec<u8>>);

impl Storage for Files {
    fn identify(&self, asset: &str, _anchor: Option<&str>) -> Result<String, IoError> {
        Ok(asset.trim_start_matches("./").to_owned())
    }

    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.0
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, "no such file"))
    }

    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.0.insert(identifier.to_owned(), bytes.to_vec());
        Ok(())
    }
}

/// A per-test directory under Cargo's integration-test scratch space.
fn scratch_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("clip_offsets")
}

/// usd-core's reads of the scene, or `None` without usd-core.
fn oracle() -> Option<Json> {
    let explicit = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit.clone().unwrap_or_else(|| "python3".into());
    let available = Command::new(&python)
        .args(["-c", "from pxr import Gf; Gf.Duration"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr 26.11"
    );
    if !available {
        eprintln!("skipped clip offset oracle: set LAYERSTACK_USD_PYTHON");
        return None;
    }
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in layers() {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let output = Command::new(&python)
        .args(["-c", ORACLE])
        .arg(dir.join("root.usda"))
        .arg(serde_json::to_string(TIMES).unwrap())
        .arg(serde_json::to_string(ATTRIBUTES).unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

/// Every read and sample time matches usd-core's.
#[test]
fn clip_sets_split_across_offset_layers_time_as_openusd_does() {
    let Some(want) = oracle() else {
        return;
    };
    let files = Files(
        layers()
            .into_iter()
            .map(|(name, text)| (name.to_owned(), text.into_bytes()))
            .collect(),
    );
    let mut document = StageDocument::open(files, "root.usda", StageOptions::default()).unwrap();
    // Value clips load on demand.
    for path in ATTRIBUTES {
        let property = document.store_mut().property_path(path);
        for time in TIMES {
            let prepared = document.prepare_clip_query(property, Time::at(*time), 8, 32);
            assert!(
                prepared.plan.is_complete(),
                "{path} at {time}: {prepared:?}"
            );
        }
    }
    let store = document.store();
    let view = document.stage().view(store);
    let mut mismatches = Vec::new();
    for path in ATTRIBUTES {
        let attribute = view.attribute(*path).unwrap();
        let mut reads = Map::new();
        for time in TIMES {
            let value = attribute
                .get(Time::at(*time))
                .map_or(Json::Null, |resolved| {
                    layer_json::value(&resolved.value, store)
                });
            reads.insert(layer_json::time_key(*time), value);
        }
        reads.insert("samples".into(), json!(attribute.sample_times()));
        let got = Json::Object(reads);
        for (key, expected) in want[*path].as_object().unwrap() {
            if let Some(difference) = layer_json::difference(&got[key], expected, 1e-6) {
                mismatches.push(format!("{path} at {key}: {difference}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
