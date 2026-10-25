// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Time-valued values read through layer offsets and value clips, against
//! usd-core 26.11.
//!
//! A layer offset maps a `timecode` with its scale and offset and a
//! `duration` with its scale alone (OpenUSD 26.11
//! `SdfLayerOffset::operator*(GfDuration)`, `pxr/usd/sdf/layerOffset.cpp`,
//! and `Usd_ApplyLayerOffsetToValue`, `pxr/usd/usd/valueUtils.h`): in
//! defaults, time samples, arrays, sparse array edits and dictionaries. A
//! value clip maps a `duration` by its time mapping segment's slope
//! (`_ConvertValueForTime`, `pxr/usd/usd/clip.cpp`) and then by the clip
//! set's layer offset. The scene is composed by usd-core and by layerstack,
//! and every attribute is read at the default time and at numeric times.
//!
//! Spec: AOUSD Core §12.3.2.1 (layer offsets), §12.3.4 (value clips).
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use layerstack::{
    FlattenRequirements, InMemoryStore, LayerId, ResolvedValue, Stage, StageOptions, Time, Value,
};
use layerstack_io::{IoError, IoErrorKind, StageDocument, Storage};
use serde_json::{Map, Value as Json, json};

use crate::support::layer_json;

/// The scene's layers, by file name.
const LAYERS: &[(&str, &str)] = &[
    (
        "root.usda",
        r#"#usda 1.0
def "R" (
    references = [
        @sub.usda@</P> (offset = 10; scale = 2),
        @base.usda@</B>
    ]
)
{
}

def "C" (
    references = @clipped.usda@</C> (offset = 5; scale = 2)
)
{
}

def "N" (
    references = @clipped.usda@</N> (offset = 5; scale = 2)
)
{
}
"#,
    ),
    (
        "sub.usda",
        r#"#usda 1.0
def "P"
{
    custom duration d = 3 (
        customData = {
            duration span = 4
            timecode cue = 4
            dictionary nested = {
                duration[] spans = [1, -2]
            }
        }
    )
    custom duration s
    duration s.timeSamples = {
        0: 1,
        10: 5,
    }
    custom duration[] ds = [1, -2]
    duration[] ds.timeSamples = {
        5: [3, 0.5],
    }
    duration[] de = edit [append 9]
    custom timecode t = 3
    custom timecode[] ts = [1, -2]
}
"#,
    ),
    (
        "base.usda",
        r#"#usda 1.0
def "B"
{
    custom duration[] de = [1]
}
"#,
    ),
    (
        "clipped.usda",
        r#"#usda 1.0
def "C" (
    clips = {
        dictionary default = {
            double2[] active = [(0, 0)]
            asset[] assetPaths = [@clip.usda@]
            string primPath = "/P"
            double2[] times = [(0, 0), (10, 20), (20, 20)]
        }
    }
)
{
    custom duration c
    custom duration[] ca
    custom timecode tc
}

def "N" (
    clips = {
        dictionary default = {
            double2[] active = [(0, 0)]
            asset[] assetPaths = [@clip.usda@]
            string primPath = "/P"
        }
    }
)
{
    custom duration c
    custom duration[] ca
    custom timecode tc
}
"#,
    ),
    (
        "clip.usda",
        r#"#usda 1.0
def "P"
{
    custom duration c
    duration c.timeSamples = {
        0: 2,
        20: 4,
    }
    custom duration[] ca
    duration[] ca.timeSamples = {
        0: [1, 3],
        20: [3, 1],
    }
    custom timecode tc
    timecode tc.timeSamples = {
        0: 1,
        20: 7,
    }
}
"#,
    ),
];

/// The attributes read, by path.
const ATTRIBUTES: &[&str] = &[
    "/R.d", "/R.s", "/R.ds", "/R.de", "/R.t", "/R.ts", "/C.c", "/C.ca", "/C.tc", "/N.c", "/N.ca",
    "/N.tc",
];

/// The numeric times each attribute is read at, besides the default time.
const TIMES: &[f64] = &[
    -5., 0., 5., 10., 12.5, 15., 20., 25., 30., 35., 40., 45., 50., 60.,
];

/// usd-core's reads of the scene: per attribute, its value at `default`
/// and at each of [`TIMES`] (keyed by `repr(float(t))`), and its
/// `customData`.
const ORACLE: &str = r#"
import json, sys
from pxr import Gf, Usd, Vt

def value(v):
    if v is None:
        return None
    if isinstance(v, (Gf.Duration, Gf.TimeCode)):
        return float(v)
    if isinstance(v, dict):
        return {k: value(item) for k, item in v.items()}
    if hasattr(v, '__len__') and not isinstance(v, str):
        return [value(item) for item in v]
    return float(v)

stage = Usd.Stage.Open(sys.argv[1])
times = json.loads(sys.argv[2])
out = {}
for path in json.loads(sys.argv[3]):
    attr = stage.GetAttributeAtPath(path)
    reads = {'default': value(attr.Get(Usd.TimeCode.Default()))}
    for t in times:
        reads[repr(float(t))] = value(attr.Get(t))
    reads['customData'] = value(attr.GetCustomData())
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
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("time_values")
}

/// usd-core's Python, or `None` (after noting the skip) when it is not
/// available and `LAYERSTACK_USD_PYTHON` does not ask for it.
fn python() -> Option<String> {
    let explicit = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit.clone().unwrap_or_else(|| "python3".into());
    let available = Command::new(&python)
        .args(["-c", "from pxr import Gf; Gf.Duration"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr with Gf.Duration"
    );
    if !available {
        eprintln!("skipped time value oracle: set LAYERSTACK_USD_PYTHON");
    }
    available.then_some(python)
}

/// usd-core's reads of the scene written to `dir`.
fn openusd(python: &str, dir: &Path) -> Json {
    let output = Command::new(python)
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
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The scene opened by layerstack, with the value clips it reads loaded.
fn open() -> StageDocument<Files> {
    let files = Files(
        LAYERS
            .iter()
            .map(|(name, text)| ((*name).to_owned(), text.as_bytes().to_vec()))
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
    document
}

/// Layerstack's reads of `paths` on `stage`, shaped as [`ORACLE`]'s.
fn reads(stage: &Stage, store: &InMemoryStore, paths: &[&str]) -> Json {
    let view = stage.view(store);
    let mut out = Map::new();
    for path in paths {
        let attribute = view.attribute(*path).unwrap();
        let read = |time: Time| {
            attribute.get(time).map_or(Json::Null, |resolved| {
                layer_json::value(&resolved.value, store)
            })
        };
        let mut reads = Map::new();
        reads.insert("default".into(), read(Time::Default));
        for time in TIMES {
            reads.insert(layer_json::time_key(*time), read(Time::at(*time)));
        }
        let custom_data = match attribute.metadata("customData").map(|r| r.value) {
            Some(ResolvedValue::Dictionary(entries)) => dictionary(&entries, store),
            None => json!({}),
            Some(other) => panic!("customData resolved as {other:?}"),
        };
        reads.insert("customData".into(), custom_data);
        out.insert((*path).to_owned(), Json::Object(reads));
    }
    Json::Object(out)
}

/// The differences of `got` from `want` on `paths`, one line each.
fn mismatches(got: &Json, want: &Json, paths: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for path in paths {
        let (want, got) = (&want[*path], &got[*path]);
        for (key, expected) in want.as_object().unwrap() {
            if let Some(difference) = layer_json::difference(&got[key], expected, 1e-9) {
                out.push(format!("{path} at {key}: {difference}"));
            }
        }
    }
    out
}

/// usd-core's reads of the scene, or `None` without usd-core.
fn oracle() -> Option<Json> {
    let python = python()?;
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in LAYERS {
        std::fs::write(dir.join(name), text).unwrap();
    }
    Some(openusd(&python, &dir))
}

/// A resolved dictionary as [`ORACLE`] renders one.
fn dictionary(entries: &[(Arc<str>, Value)], store: &InMemoryStore) -> Json {
    Json::Object(
        entries
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    Value::Dictionary(inner) => dictionary(inner, store),
                    value => layer_json::value(value, store),
                };
                ((**key).to_owned(), value)
            })
            .collect(),
    )
}

/// Every attribute reads as usd-core reads it: durations scaled, not
/// shifted, by layer offsets and by value clip time mappings.
#[test]
fn time_values_read_through_offsets_and_clips_as_openusd_reads_them() {
    let Some(want) = oracle() else {
        return;
    };
    let document = open();
    let got = reads(document.stage().stage(), document.store(), ATTRIBUTES);
    let mismatches = mismatches(&got, &want, ATTRIBUTES);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Layerstack's flattened layer holds the values in stage time, so the
/// attributes the flatten keeps read from it as usd-core reads them
/// composed (`UsdStage::Flatten` maps them the same way).
#[test]
fn flattened_time_values_read_as_composed() {
    let Some(want) = oracle() else {
        return;
    };
    let mut document = open();
    let (store, stage) = document.parts_mut();
    let root = stage.stage().root_layer().unwrap();
    let flat_id = LayerId(1000);
    let flat = stage
        .stage()
        .flatten(store, root, flat_id, &FlattenRequirements::default())
        .unwrap();
    store.insert_layer(flat.layer);
    let flattened = Stage::compose(store, flat_id, StageOptions::default());
    let referenced: Vec<&str> = ATTRIBUTES
        .iter()
        .copied()
        .filter(|path| path.starts_with("/R."))
        .collect();
    let got = reads(&flattened, store, &referenced);
    let mismatches = mismatches(&got, &want, &referenced);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
