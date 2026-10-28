// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Values the USDC writer once refused, saved as OpenUSD 26.11 saves them.
//!
//! `scripts/usdc_encodings_oracle.py` records, for each case under
//! `fixtures/usdc_encodings`, what OpenUSD does call by call: it opens the
//! case's USDA, exports the layer to USDC (keeping the file as
//! `<case>.usdc` when the export succeeds) and reads that file back. Here
//! each case is read with [`read_usda`] and saved with [`save_usdc`]: an
//! export OpenUSD accepts must give OpenUSD's file but for its LZ4 blocks
//! ([`crate_sections`]), and both files must read back with [`read_usdc`]
//! as the text OpenUSD reads back.
//!
//! Spec: AOUSD Core §16.3 (USDC), §7.4 (metadata); OpenUSD 26.11
//! `pxr/usd/sdf/crateFile.cpp`.

use std::path::PathBuf;

use layerstack::{LayerId, NoAssets, PathInterner, TokenInterner};
use layerstack_conformance::usdc::crate_sections;
use layerstack_conformance::workspace_root;
use layerstack_usda::read_usda;
use layerstack_usda::save::save_usda;
use layerstack_usdc::{read_usdc, save_usdc};
use serde_json::Value as Json;

fn fixtures() -> PathBuf {
    workspace_root().join("layerstack_conformance/fixtures/usdc_encodings")
}

/// The oracle's record of `case`, after checking which OpenUSD made it.
fn oracle(case: &str) -> Json {
    let text = std::fs::read_to_string(fixtures().join("oracle.json")).unwrap();
    let oracle: Json = serde_json::from_str(&text).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    let record = oracle["cases"][case].clone();
    assert_eq!(
        record["open_usda"]["accepted"],
        Json::Bool(true),
        "{case}: OpenUSD reads the source"
    );
    record
}

/// Saves `case`'s USDA source as USDC.
fn save(case: &str) -> Result<Vec<u8>, layerstack_usdc::writer::UsdcWriteError> {
    let text = std::fs::read_to_string(fixtures().join(format!("{case}.usda"))).unwrap();
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let read = read_usda(&text, LayerId(1), &tokens, &paths, &mut NoAssets);
    let diagnostics: Vec<_> = read.diagnostics().collect();
    assert!(diagnostics.is_empty(), "{case}: {diagnostics:?}");
    save_usdc(&read.emitted.layer, &tokens, &paths)
}

/// The USDA text of the layer `data` reads back as.
fn read_back(data: &[u8]) -> String {
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let read = read_usdc(data, LayerId(2), &tokens, &paths, &mut NoAssets).expect("reads");
    assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
    save_usda(&read.layer, &tokens, &paths).expect("saves as USDA")
}

/// Asserts that `case` saves as OpenUSD exports it, and that OpenUSD's
/// file and ours both read back as OpenUSD reads its file.
fn assert_saves_as_openusd(case: &str) {
    let record = oracle(case);
    let export = &record["export_usdc"];
    assert_eq!(export["accepted"], Json::Bool(true), "{case}: {export}");
    let openusd = std::fs::read(fixtures().join(format!("{case}.usdc"))).unwrap();
    assert_eq!(
        Some(openusd.len() as u64),
        export["bytes"].as_u64(),
        "{case}: fixture"
    );
    let ours = save(case).unwrap_or_else(|e| panic!("{case}: {e}"));
    assert!(
        crate_sections(&ours).unwrap() == crate_sections(&openusd).unwrap(),
        "{case}: the file OpenUSD writes"
    );
    let read = &record["open_usdc"];
    assert_eq!(read["same_text"], Json::Bool(true), "{case}: {read}");
    let text = read["text"].as_str().unwrap();
    assert_eq!(read_back(&openusd), text, "{case}: OpenUSD's file");
    assert_eq!(read_back(&ours), text, "{case}: our file");
}

/// List-op metadata no schema registers, on the layer, a prim, an
/// attribute and a relationship, is an `SdfUnregisteredValue` holding an
/// `SdfUnregisteredValueListOp` of each item's text; two equal ones share
/// one value, and an unregistered dictionary beside them is laid out as
/// OpenUSD lays it out.
#[test]
fn unregistered_list_ops_save_as_openusd_saves_them() {
    assert_saves_as_openusd("unregistered_list_ops");
}
