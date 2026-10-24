// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Generates the `lab` module from its `schema.usda`, as a downstream build
//! script does. `lab` builds on the fixture's own `usd/schema.usda`, the two
//! base classes of OpenUSD's, so it needs no OpenUSD checkout. It builds on
//! the fixture's `plugInfo.json`, which declares the library's kinds.

use std::path::{Path, PathBuf};

use layerstack_schemagen::compile::{CompileOptions, compile};

fn main() {
    let fixtures = Path::new("../layerstack_conformance/fixtures/schema_compiler");
    // Built on the library's own `plugInfo.json`, as usdGenSchema builds on
    // the one in its output directory, so the module registers its `Kinds`.
    let plug_info = fixtures.join("lab/plugInfo.json");
    let existing = std::fs::read_to_string(&plug_info).expect("the lab plugInfo.json");
    let options = CompileOptions::new()
        .with_search_path(fixtures)
        .with_existing_plug_info(existing);
    let compiled =
        compile(&fixtures.join("lab/schema.usda"), &options).unwrap_or_else(|e| panic!("{e}"));
    let module = compiled.rust_module().unwrap_or_else(|e| panic!("{e}"));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("lab.rs"), module).expect("OUT_DIR is writable");

    // A library whose `plugInfo.json` declares metadata fields of every
    // type, for its typed metadata views.
    let registry = Path::new("../layerstack_conformance/fixtures/registry_metadata");
    let existing = std::fs::read_to_string(registry.join("plugInfo.json"))
        .expect("the registry_metadata plugInfo.json");
    let options = CompileOptions::new()
        .with_search_path(fixtures)
        .with_existing_plug_info(existing);
    let compiled =
        compile(&registry.join("schema.usda"), &options).unwrap_or_else(|e| panic!("{e}"));
    let module = compiled.rust_module().unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(out.join("fields.rs"), module).expect("OUT_DIR is writable");
    println!("cargo:rerun-if-changed={}", registry.display());
    println!("cargo:rerun-if-changed={}", fixtures.join("lab").display());
    println!("cargo:rerun-if-changed={}", fixtures.join("usd").display());
}
