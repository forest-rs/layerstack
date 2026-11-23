// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The command's process contract for scripts comparing authored layers.

#![cfg(all(feature = "std", not(target_family = "wasm")))]

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
