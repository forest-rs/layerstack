// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A crate file whose arrays of element types OpenUSD never compresses
//! carry the compressed bit, read as OpenUSD reads it: the bit is ignored.
//!
//! `scripts/usdc_compressed_bit_oracle.py` exports
//! `fixtures/usdc_compressed_bit/arrays.usda` with OpenUSD, sets the bit on
//! the `bool[]`, `uchar[]`, `string[]`, `token[]`, `asset[]`, `timecode[]`
//! and `pathExpression[]` values (`compressed_bit.usdc`), and records the
//! text OpenUSD reads from that file. Both of layerstack's readers, eager
//! and lazy, must read the same text.
//!
//! OpenUSD 26.11: `_ReadPossiblyCompressedArray`,
//! `pxr/usd/sdf/crateFile.cpp:2178-2290`. AOUSD Core §16.3.10.

use std::path::PathBuf;
use std::sync::Arc;

use layerstack::{LayerId, NoAssets, PathInterner, TokenInterner};
use layerstack_usda::save::save_usda;
use layerstack_usdc::{CrateFile, DecodeBudget, read_usdc, read_usdc_lazy};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/usdc_compressed_bit/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd: String,
    patched: Vec<String>,
    read: String,
    same_as_export: bool,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert!(
        oracle.openusd.starts_with("26."),
        "oracle from OpenUSD {}",
        oracle.openusd
    );
    oracle
}

fn fixture() -> Vec<u8> {
    let path = if cfg!(target_os = "wasi") {
        PathBuf::from("fixtures/usdc_compressed_bit/compressed_bit.usdc")
    } else {
        layerstack_conformance::workspace_root()
            .join("layerstack_conformance/fixtures/usdc_compressed_bit/compressed_bit.usdc")
    };
    std::fs::read(path).expect("compressed_bit.usdc")
}

#[test]
fn compressed_bit_where_openusd_never_compresses_is_ignored() {
    let oracle = oracle();
    assert!(oracle.same_as_export, "OpenUSD ignores the bit");
    let bytes = fixture();

    // The fixture really carries the bit where the oracle set it.
    let mut budget = DecodeBudget::for_input(bytes.len());
    let file = CrateFile::open(&bytes, &mut budget).expect("crate file");
    for path in &oracle.patched {
        let field = file
            .spec(path)
            .and_then(|spec| spec.field("default"))
            .unwrap_or_else(|| panic!("{path} default"));
        let rep = field.representation();
        assert!(rep.is_array() && rep.is_compressed(), "{path}");
    }

    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let eager = read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut NoAssets).expect("reads");
    assert!(eager.diagnostics.is_empty(), "{:?}", eager.diagnostics);
    let text = save_usda(&eager.layer, &tokens, &paths).expect("saves as USDA");
    assert_eq!(text, oracle.read, "eager read");

    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let lazy = read_usdc_lazy(
        Arc::from(bytes.as_slice()),
        LayerId(1),
        &tokens,
        &paths,
        &mut NoAssets,
    )
    .expect("reads lazily");
    let text = save_usda(&lazy.assembled.layer, &tokens, &paths).expect("saves as USDA");
    assert_eq!(text, oracle.read, "lazy read");
}
