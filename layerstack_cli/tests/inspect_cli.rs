// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Navigate from the root to an authored property using the installed command.

#![cfg(not(target_family = "wasm"))]

#[test]
fn root_child_and_property_paths_can_be_used_for_the_next_query() {
    let run = |args: &[&str], status| {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_layerstack"))
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(status), "{o:?}");
        String::from_utf8(o.stdout).unwrap()
    };
    let stage = "tests/scenes/binding.usda";
    let root = run(&["inspect", "--json", stage], 0);
    assert!(root.contains(r#""kind":"child","path":"/World""#), "{root}");
    let bounded = run(&["inspect", "--limit", "0", stage, "/World"], 0);
    assert!(bounded.contains("children=1") && !bounded.contains("/World/Mesh"));
    let world = run(&["inspect", "--json", stage, "/World"], 0);
    assert!(
        world.contains(r#""kind":"relationship","path":"/World.material:binding""#),
        "{world}"
    );
    assert!(run(&["explain", stage, "/World.material:binding"], 0).contains("/Looks/Paint"));
    assert!(run(&["inspect", "--json", stage, "/Absent"], 2).contains("missing_prim"));
}
