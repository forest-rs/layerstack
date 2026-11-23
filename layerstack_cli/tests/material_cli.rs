// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Exercise inherited binding and terminal resolution through the command.

#![cfg(not(target_family = "wasm"))]

use std::process::Command;

#[test]
fn inherited_binding_points_to_the_relationship_and_terminal_source() {
    let run = |path| {
        Command::new(env!("CARGO_BIN_EXE_layerstack"))
            .args([
                "material",
                "--json",
                "--purpose",
                "preview",
                "tests/scenes/binding.usda",
                path,
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap()
    };
    let result = run("/World/Mesh");
    assert!(result.status.success(), "{result:?}");
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(
        text.contains(r#""relationship":"/World.material:binding""#),
        "{text}"
    );
    assert!(text.contains(r#""kind":"surfaceShader","prim":"/Looks/Paint/Surface""#));
    assert!(text.contains(
        r#""from":"/Looks/Paint.outputs:surface","to":"/Looks/Paint/Surface.outputs:surface""#
    ));
    let unbound = run("/Looks");
    assert!(unbound.status.success());
    assert!(
        String::from_utf8(unbound.stdout)
            .unwrap()
            .contains(r#""kind":"unbound""#)
    );
    assert_eq!(run("/Missing").status.code(), Some(2));
}
