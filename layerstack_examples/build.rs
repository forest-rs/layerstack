// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Compile a downstream shader library with the example crate's `no_std` library target.
fn main() {
    let directory = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let source = layerstack_schemagen::generate_shader_library("fixtures/matrix_shader.usda")
        .expect("generate matrix shader interfaces");
    let module = directory.join("matrix_shader.rs");
    std::fs::write(&module, source).unwrap();
    let bridge = format!("#[path = {module:?}] pub mod matrix_shader;");
    std::fs::write(directory.join("shader_modules.rs"), bridge).unwrap();
    println!("cargo:rerun-if-changed=fixtures/matrix_shader.usda");
}
