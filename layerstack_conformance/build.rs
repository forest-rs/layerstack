// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Compile the same shader API a downstream build script generates.
fn main() {
    let directory = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let source =
        layerstack_schemagen::generate_shader_library("fixtures/custom_nodes/shaderDefs.usda")
            .expect("generate custom shaders");
    let module = directory.join("custom_nodes.rs");
    std::fs::write(&module, source).unwrap();
    std::fs::write(
        directory.join("custom_modules.rs"),
        format!("#[path = {:?}] pub mod custom;", module),
    )
    .unwrap();
    println!("cargo:rerun-if-changed=fixtures/custom_nodes");
}
