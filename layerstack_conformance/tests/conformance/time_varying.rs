// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Whether attributes might vary with time, against usd-core 26.11.
//!
//! `UsdAttribute::ValueMightBeTimeVarying` answers whether numeric-time
//! reads can differ; OpenUSD 26.11's
//! `UsdAttributeQuery::ValueSourceMightBeTimeVarying` also answers `true`
//! where a numeric-time read can differ from the default-time read, as for
//! a single time sample (`UsdResolveInfo::ValueSourceMightBeTimeVarying`,
//! `pxr/usd/usd/resolveInfo.h`). The scene covers defaults, blocks,
//! animation blocks, empty, single and several samples, splines, sparse
//! edits over weaker sources and value clips. Where the strongest value is
//! a sparse default, OpenUSD's `ValueMightBeTimeVarying` gives the source
//! answer, so a sparse edit over a single weaker sample might vary; a single
//! sample whose type composes over weaker values (a path expression, an
//! array edit) might vary too, alone or over anything
//! (`_ValueMightBeTimeVaryingFromResolveInfo`, `pxr/usd/usd/stage.cpp`).
//!
//! Spec: AOUSD Core §12.3.2 (value sources), §12.3.4 (value clips),
//! §12.3.6 (blocks).
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use layerstack::{StageOptions, Time};
use layerstack_io::{IoError, IoErrorKind, StageDocument, Storage};
use serde_json::{Value as Json, json};

/// The scene's layers, by file name.
const LAYERS: &[(&str, &str)] = &[
    (
        "root.usda",
        r#"#usda 1.0
def "P" (
    references = @weak.usda@</W>
)
{
    double none
    double dflt = 1
    double one.timeSamples = { 1: 2 }
    double two.timeSamples = { 1: 2, 2: 3 }
    double spl.spline = { 0: 1, }
    double blocked = None
    double emptys.timeSamples = { }
    double oneblock.timeSamples = { 1: None }
    double dflt_over = 4
    float[] edit_over = edit [append 9]
    float[] edit_over_one = edit [append 9]
    float[] edit_over_dflt = edit [append 9]
    double anim_over = AnimationBlock
    double one_over.timeSamples = { 1: 2 }
    pathExpression expr_one.timeSamples = { 1: "/A" }
    pathExpression expr_over_one.timeSamples = { 1: "%_ /B" }
    pathExpression expr_dflt_over_one = "%_ /B"
    float[] edit_sample_over_one.timeSamples = { 1: edit [append 9] }
    float[] edit_sample_over_dflt.timeSamples = { 1: edit [append 9] }
}

def "C" (
    clips = {
        dictionary default = {
            double2[] active = [(0, 0)]
            asset[] assetPaths = [@clip.usda@]
            string primPath = "/Q"
        }
    }
)
{
    double one
    double two
    double dflt_over = 4
    double missing
}
"#,
    ),
    (
        "weak.usda",
        r#"#usda 1.0
def "W"
{
    double dflt_over.timeSamples = { 1: 2, 2: 3 }
    float[] edit_over.timeSamples = { 1: [1], 2: [2] }
    float[] edit_over_one.timeSamples = { 1: [1] }
    float[] edit_over_dflt = [1]
    double anim_over.timeSamples = { 1: 2, 2: 3 }
    double one_over.timeSamples = { 1: 2, 2: 3 }
    pathExpression expr_over_one.timeSamples = { 1: "/A" }
    pathExpression expr_dflt_over_one.timeSamples = { 1: "/A" }
    float[] edit_sample_over_one.timeSamples = { 1: [1] }
    float[] edit_sample_over_dflt = [1]
}
"#,
    ),
    (
        "clip.usda",
        r#"#usda 1.0
def "Q"
{
    double one.timeSamples = { 1: 2 }
    double two.timeSamples = { 1: 2, 2: 3 }
    double dflt_over.timeSamples = { 1: 2, 2: 3 }
}
"#,
    ),
];

/// The attributes asked about, by path.
const ATTRIBUTES: &[&str] = &[
    "/P.none",
    "/P.dflt",
    "/P.one",
    "/P.two",
    "/P.spl",
    "/P.blocked",
    "/P.emptys",
    "/P.oneblock",
    "/P.dflt_over",
    "/P.edit_over",
    "/P.edit_over_one",
    "/P.edit_over_dflt",
    "/P.anim_over",
    "/P.one_over",
    "/P.expr_one",
    "/P.expr_over_one",
    "/P.expr_dflt_over_one",
    "/P.edit_sample_over_one",
    "/P.edit_sample_over_dflt",
    "/C.one",
    "/C.two",
    "/C.dflt_over",
    "/C.missing",
];

/// usd-core's answers, per attribute: `[ValueMightBeTimeVarying,
/// ValueSourceMightBeTimeVarying]`.
const ORACLE: &str = r#"
import json, sys
from pxr import Usd

stage = Usd.Stage.Open(sys.argv[1])
out = {}
for path in json.loads(sys.argv[2]):
    attr = stage.GetAttributeAtPath(path)
    query = Usd.AttributeQuery(attr)
    out[path] = [attr.ValueMightBeTimeVarying(), query.ValueSourceMightBeTimeVarying()]
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
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("time_varying")
}

/// usd-core's answers for the scene, or `None` without usd-core.
fn oracle() -> Option<Json> {
    let explicit = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit.clone().unwrap_or_else(|| "python3".into());
    let available = Command::new(&python)
        .args([
            "-c",
            "from pxr import Usd; Usd.AttributeQuery.ValueSourceMightBeTimeVarying",
        ])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr 26.11"
    );
    if !available {
        eprintln!("skipped time-varying oracle: set LAYERSTACK_USD_PYTHON");
        return None;
    }
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in LAYERS {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let output = Command::new(&python)
        .args(["-c", ORACLE])
        .arg(dir.join("root.usda"))
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

/// Layerstack answers both questions as usd-core does.
#[test]
fn time_varying_queries_answer_as_openusd_does() {
    let Some(want) = oracle() else {
        return;
    };
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
        let prepared = document.prepare_clip_query(property, Time::at(1.), 8, 32);
        assert!(prepared.plan.is_complete(), "{path}: {prepared:?}");
    }
    let view = document.stage().view(document.store());
    let mut mismatches = Vec::new();
    for path in ATTRIBUTES {
        let attribute = view.attribute(*path).unwrap();
        let got = json!([
            attribute.might_be_time_varying(),
            attribute.value_source_might_be_time_varying()
        ]);
        if got != want[*path] {
            mismatches.push(format!("{path}: {got} != {}", want[*path]));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
