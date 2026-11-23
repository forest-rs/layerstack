// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asset inventory keeps usable source paths even when dependencies are missing.

#![cfg(not(target_family = "wasm"))]

#[test]
fn lists_loaded_layers_and_missing_opinions_together() {
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_layerstack"))
        .args(["deps", "--json", "tests/scenes/asset-uses.usda"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    let text = String::from_utf8(o.stdout).unwrap();
    assert!(
        text.contains(r#""asset":"./binding.usda","status":"loaded""#),
        "{text}"
    );
    assert!(text.contains(r#""spec":"/Broken","asset":"./absent-model.usda","status":"missing""#));
    assert!(text.contains(r#""spec":"/Broken.albedo","field":"albedo","asset":"./absent-image.png","status":"missing""#));
}
