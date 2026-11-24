// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validate the check command's reports and process statuses.

#![cfg(not(target_family = "wasm"))]

use std::process::{Command, Output};

fn check(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_layerstack"))
        .arg("check")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap()
}

#[test]
fn checks_composed_meshes_and_authored_asset_sites() {
    let out = check(&["tests/scenes/check/scene.usda", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    for fragment in [
        r#""kind":"meshTopology","path":"/World/MeshCopy""#,
        r#""code":"invalidVertexIndex""#,
        r#""indicesSpec":"/Source.faceVertexIndices""#,
        r#""pointsSpec":"/Source.points""#,
        r#""kind":"composition","path":"/World/BrokenArc""#,
        r#""spec":"/World/BrokenArc""#,
        r#""asset":"./missing.png","status":"missing""#,
        r#""spec":"/World/WithVariant{detail=absent}","asset":"./missing.usda","status":"missing""#,
        r#""asset":"memory://site/image.png","status":"unchecked""#,
    ] {
        assert!(text.contains(fragment), "Missing {fragment} in {text}");
    }
}

#[test]
fn default_time_and_unchecked_uses_are_visible_on_success() {
    let out = check(&["tests/scenes/check/default-time.usda"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("meshTime=default  meshEvaluations=1  errors=0  incomplete=0  unchecked=1"),
        "{text}"
    );
    let help = check(&["--help"]);
    assert!(help.status.success(), "{help:?}");
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("default time only")
    );
}

#[test]
fn usage_and_missing_roots_return_structured_errors() {
    let usage = check(&["--json"]);
    assert_eq!(usage.status.code(), Some(2), "{usage:?}");
    assert!(
        String::from_utf8(usage.stdout)
            .unwrap()
            .contains(r#""error":{"code":"usage""#)
    );
    let missing = check(&["--json", "tests/scenes/check/missing.usda"]);
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    assert!(
        String::from_utf8(missing.stdout)
            .unwrap()
            .contains(r#""error":{"code":"open""#)
    );
}

#[test]
fn malformed_root_and_dependency_produce_incomplete_checks() {
    for path in [
        "tests/scenes/check/malformed.usda",
        "tests/scenes/check/malformed-dependency.usda",
    ] {
        let out = check(&["--json", path]);
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains(r#""error":{"code":"open""#), "{text}");
        assert!(text.contains("malformed.usda"), "{text}");
    }
}
