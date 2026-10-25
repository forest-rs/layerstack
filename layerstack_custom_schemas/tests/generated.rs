// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The checked-in `observatory` module is what `layerstack_schemagen compile
//! --rust` writes for the observatory fixture today, as a downstream crate's CI
//! checks with `compile --rust <file> --check`. `observatory` builds on OpenUSD's
//! `Xformable`, so this needs `LAYERSTACK_OPENUSD_SOURCE` naming an OpenUSD
//! 26.11 checkout, and `rustfmt`; without the checkout it is skipped.

#![cfg(not(target_arch = "wasm32"))]
#![allow(missing_docs, reason = "integration tests")]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use layerstack_schemagen::compile::{CompileOptions, compile};

fn rustfmt(source: &str) -> String {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("rustfmt runs");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(source.as_bytes())
        .expect("rustfmt reads the module");
    let output = child.wait_with_output().expect("rustfmt finishes");
    assert!(output.status.success(), "rustfmt accepts the module");
    String::from_utf8(output.stdout).expect("UTF-8")
}

#[test]
fn the_observatory_module_is_current() {
    let Some(source) = std::env::var_os("LAYERSTACK_OPENUSD_SOURCE").map(PathBuf::from) else {
        eprintln!("skipped: set LAYERSTACK_OPENUSD_SOURCE to an OpenUSD 26.11 checkout");
        return;
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let schema =
        root.join("../layerstack_conformance/fixtures/schema_compiler/observatory/schema.usda");
    let compiled = compile(&schema, &CompileOptions::new().with_openusd_source(source))
        .expect("the observatory schema compiles");
    let module = rustfmt(
        &compiled
            .rust_module()
            .expect("the observatory views generate"),
    );
    let checked_in = std::fs::read_to_string(root.join("src/generated/observatory.rs"))
        .expect("src/generated/observatory.rs");
    assert!(
        module == checked_in,
        "src/generated/observatory.rs is stale; regenerate it with `cargo run -p \
         layerstack_schemagen -- compile \
         layerstack_conformance/fixtures/schema_compiler/observatory/schema.usda \
         --openusd-source <OpenUSD> --rust layerstack_custom_schemas/src/generated/observatory.rs`"
    );
}
