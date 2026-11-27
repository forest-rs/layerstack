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
        layerstack_io::usdc::writer::write_document(&layerstack_io::usda::writer::Document::new())
            .unwrap();
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

    let stat = compare(&["--stat", "--json", "before.usdc", "after.usda"]);
    assert_eq!(stat.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(stat.stdout).unwrap().trim_end(),
        r#"{"mode":"authored","equal":false,"stat":{"records":1,"paths":1,"added":1,"removed":0,"changed":0}}"#
    );

    for (after, expected) in [("empty.usda", 0), ("before.usdc", 0), ("after.usda", 1)] {
        let quiet = compare(&["--quiet", "before.usdc", after]);
        assert_eq!(quiet.status.code(), Some(expected), "{quiet:?}");
        assert!(quiet.stdout.is_empty() && quiet.stderr.is_empty());
    }
    for flag in ["--stat", "--json"] {
        assert_eq!(
            compare(&["--quiet", flag, "before.usdc", "empty.usda"])
                .status
                .code(),
            Some(2)
        );
    }
    fs::write(dir.join("invalid.usda"), b"invalid").unwrap();
    let invalid = compare(&["--quiet", "after.usda", "invalid.usda"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("after layer")
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
    // Exercise both readers with distinct inputs large enough for concurrent decoding.
    let padding = "#".repeat(256 * 1024);
    for (path, value) in [("a.usda", 1), ("b.usda", 2)] {
        fs::write(
            dir.join(path),
            format!("#usda 1.0\n{padding}\ndef \"P\" {{\n int x = {value}\n}}\n"),
        )
        .unwrap();
    }
    let serial = compare(&["--jobs", "1", "a.usda", "b.usda"]);
    let parallel = compare(&["--jobs", "2", "a.usda", "b.usda"]);
    assert_eq!(serial.status.code(), Some(1), "{serial:?}");
    assert_eq!(parallel.status.code(), Some(1), "{parallel:?}");
    assert_eq!(parallel.stdout, serial.stdout);
    for (path, suffix) in [("a.usda", "a"), ("b.usda", "b")] {
        fs::write(dir.join(path), format!("{padding}{suffix}")).unwrap();
    }
    for other in ["a.usda", "b.usda"] {
        let invalid = compare(&["--jobs", "2", "a.usda", other]);
        assert_eq!(invalid.status.code(), Some(2));
        assert!(
            String::from_utf8(invalid.stderr)
                .unwrap()
                .contains("a.usda: before layer")
        );
    }
    for options in [["--jobs", "0"], ["--jobs", "3"]] {
        let invalid = compare(&[options[0], options[1], "a.usda", "b.usda"]);
        assert_eq!(invalid.status.code(), Some(2));
        assert!(
            String::from_utf8(invalid.stderr)
                .unwrap()
                .contains("--jobs expects 1 or 2")
        );
    }
    let incompatible = compare(&["--composed", "--jobs", "1", "a.usda", "b.usda"]);
    assert_eq!(incompatible.status.code(), Some(2));
    assert!(
        String::from_utf8(incompatible.stderr)
            .unwrap()
            .contains("--jobs applies only to authored comparisons")
    );
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
    assert!(run(&["diff", "--composed", "--quiet", roots[0], roots[1]], 1).is_empty());
    let diff = run(&["diff", "--composed", "--json", roots[0], roots[1]], 1);
    assert!(
        diff.contains(r#""mode":"composed""#) && diff.contains(r#""path":"/Assembly.size""#),
        "{diff}"
    );
    assert!(
        diff.contains(r#""time":10"#) && !diff.contains("weight"),
        "{diff}"
    );
    run(
        &[
            "diff",
            "--composed",
            "--path",
            "/Assembly",
            "--stat",
            roots[0],
            roots[1],
        ],
        1,
    );
    run(
        &[
            "diff",
            "--composed",
            "--path",
            "/Model",
            "--quiet",
            roots[0],
            roots[1],
        ],
        0,
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
    run(
        &[
            "diff",
            "--composed",
            "--path",
            "/Absent",
            "--quiet",
            roots[0],
            roots[1],
        ],
        2,
    );
    let incomplete = run(&["diff", "--composed", "--json", roots[0], roots[1]], 2);
    assert!(
        incomplete.contains(r#""side":"after""#) && incomplete.contains("model.usda"),
        "{incomplete}"
    );
    assert!(!incomplete.contains(r#""equal""#));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn subtree_scope_matches_scene_names_inside_authored_variants() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "layerstack-diff-scope-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&dir).unwrap();
    let before = r#"#usda 1.0
def "World" (
    variants = { string look = "active" }
    prepend variantSets = "look"
) {
    def "MeshBackup" { custom int x = 0; }
    variantSet "look" = {
        "active" {
            def "Mesh" { custom int unchanged = 0; }
        }
        "other" {
            def "Mesh" {
                def "Child" { custom int x = 0; }
            }
        }
    }
}
"#;
    fs::write(dir.join("a"), before).unwrap();
    fs::write(dir.join("b"), before.replace("int x = 0", "int x = 1")).unwrap();
    let run = |args: &[&str], status| {
        let output = Command::new(env!("CARGO_BIN_EXE_layerstack"))
            .arg("diff")
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(status), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    for (from, to) in [("a", "b"), ("b", "a")] {
        let report = run(&["--json", "--path", "/World/Mesh", from, to], 1);
        assert!(report.contains(r#""scope":"/World/Mesh""#));
        assert!(
            report.contains("/World{look=other}Mesh/Child.x"),
            "{report}"
        );
        assert!(!report.contains("MeshBackup"), "{report}");
        let counts = run(&["--stat", "--json", "--path", "/World/Mesh", from, to], 1);
        assert!(counts.contains(r#""records":1,"paths":1"#), "{counts}");
        assert!(run(&["--quiet", "--path", "/World/Mesh/Absent", from, to], 0).is_empty());
        run(&["--quiet", "--path", "/World/Mesh/Child", from, to], 1);
        assert_eq!(run(&["--path", "/", from, to], 1), run(&[from, to], 1));
    }
    fs::write(dir.join("empty"), "#usda 1.0\n").unwrap();
    for (from, to, kind) in [("empty", "a", "added"), ("a", "empty", "removed")] {
        let report = run(&["--path", "/World/Mesh", from, to], 1);
        assert!(
            report
                .lines()
                .all(|line| line.starts_with(kind) && line.contains("}Mesh")),
            "{report}"
        );
        assert!(report.contains("}Mesh/Child.x"), "{report}");
    }
    for invalid in [
        "World",
        "/World//Mesh",
        "/World/",
        "/World.id",
        "/World{look=active}",
    ] {
        run(&["--path", invalid, "a", "b"], 2);
    }
    fs::write(dir.join("b"), "#usda 1.0\ndef \"Outside\" { int v = }").unwrap();
    run(&["--quiet", "--path", "/Absent", "a", "b"], 2);
    fs::remove_dir_all(dir).unwrap();
}
