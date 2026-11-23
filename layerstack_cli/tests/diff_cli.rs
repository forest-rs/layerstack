// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The command's process contract for scripts comparing authored layers.

#![cfg(not(target_family = "wasm"))]

use std::fs;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn diff_distinguishes_equality_changes_and_unreadable_input() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("layerstack-diff-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let binary =
        layerstack_usdc::writer::write_document(&layerstack_usda::writer::Document::new()).unwrap();
    fs::write(dir.join("before.usdc"), &binary).unwrap();
    fs::write(dir.join("empty.usda"), "#usda 1.0\n").unwrap();
    let text = "#usda 1.0\n(\n subLayers = [@missing.usda@]\n)\n";
    fs::write(dir.join("after.usda"), text).unwrap();

    let compare = |args: &[&str]| -> Output {
        Command::new(env!("CARGO_BIN_EXE_layerstack"))
            .arg("diff")
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap()
    };
    let equal = compare(&["--json", "empty.usda", "before.usdc"]);
    assert_eq!(equal.status.code(), Some(0), "{equal:?}");
    assert_eq!(
        String::from_utf8(equal.stdout).unwrap().trim_end(),
        r#"{"equal":true,"changes":[]}"#
    );

    // A dangling authored asset path is a change we can describe in full.
    let changed = compare(&["--json", "before.usdc", "after.usda"]);
    assert_eq!(changed.status.code(), Some(1), "{changed:?}");
    assert!(
        String::from_utf8(changed.stdout)
            .unwrap()
            .contains("missing.usda")
    );

    let missing = compare(&["before.usdc", "absent.usdc"]);
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("absent.usdc")
    );

    assert_eq!(fs::read(dir.join("before.usdc")).unwrap(), binary);
    assert_eq!(fs::read_to_string(dir.join("after.usda")).unwrap(), text);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn composed_diff_reports_changes_in_stage_namespace_and_rejects_missing_dependencies() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "layerstack-stage-diff-{}-{nonce}",
        std::process::id()
    ));
    for (side, sample, shadowed) in [("before", 4, 11), ("after", 8, 22)] {
        let side = dir.join(side);
        fs::create_dir_all(&side).unwrap();
        fs::write(
            side.join("scene.usda"),
            r#"#usda 1.0
def "Assembly" (references = @./model.usda@</Model>) {
    custom float weight = 3
}
"#,
        )
        .unwrap();
        fs::write(side.join("model.usda"), format!("#usda 1.0\ndef \"Model\" {{\n custom float weight = {shadowed}\n custom float size.timeSamples = {{0:2, 10:{sample}}}\n}}\n")).unwrap();
    }
    let run = |args: &[&str], code| {
        let o = Command::new(env!("CARGO_BIN_EXE_layerstack"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(code), "{o:?}");
        String::from_utf8(o.stdout).unwrap()
    };
    let roots = ["before/scene.usda", "after/scene.usda"];
    run(&["diff", roots[0], roots[1]], 0);
    let diff = run(&["diff", "--composed", "--json", roots[0], roots[1]], 1);
    assert!(
        diff.contains(r#""mode":"composed""#) && diff.contains(r#""path":"/Assembly.size""#),
        "{diff}"
    );
    assert!(
        diff.contains(r#""time":10"#) && !diff.contains("weight"),
        "{diff}"
    );
    let why = run(&["explain", "--time", "10", roots[1], "/Assembly.size"], 0);
    assert!(
        why.contains("model.usda  /Model.size") && why.contains(": 8"),
        "{why}"
    );

    // Different layer structures, with the same resolved fields and animation.
    fs::write(dir.join("flat.usda"), "#usda 1.0\ndef \"Assembly\" {\n custom float weight = 3\n custom float size.timeSamples = {0:2, 10:8}\n}\n").unwrap();
    run(&["diff", "--composed", roots[1], "flat.usda"], 0);
    fs::remove_file(dir.join("after/model.usda")).unwrap();
    let incomplete = run(&["diff", "--composed", "--json", roots[0], roots[1]], 2);
    assert!(
        incomplete.contains(r#""side":"after""#) && incomplete.contains("model.usda"),
        "{incomplete}"
    );
    assert!(!incomplete.contains(r#""equal""#));
    fs::remove_dir_all(dir).unwrap();
}
