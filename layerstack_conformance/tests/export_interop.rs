// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Independent check of exporter output with the `OpenUSD` tools `usdcat` and
//! `usdchecker`, when they are on `PATH`.
//!
//! The tools are optional: without them this test reports that it skipped
//! and passes, so CI needs no USD installation. The fuller gate (tool
//! versions, `--arkit` validators, ZIP layout checks) is
//! `layerstack_conformance/scripts/export_interop.sh`.

use std::process::Command;

use layerstack_conformance::export_fixtures::{Expect, write_all};

fn tool(name: &str) -> Option<String> {
    Command::new(name)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// A per-test directory under Cargo's integration-test scratch space.
///
/// Under WASI only the crate directory and its parent are preopened (see
/// `.cargo/config.toml`), so the path is made relative to them there.
fn scratch_dir(name: &str) -> std::path::PathBuf {
    let tmp = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let base = if cfg!(target_os = "wasi") {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate is inside the workspace");
        std::path::Path::new("..").join(
            tmp.strip_prefix(workspace)
                .expect("target directory is inside the workspace"),
        )
    } else {
        tmp.to_path_buf()
    };
    base.join(format!("export-interop-{name}"))
}

#[test]
fn external_usd_tools_accept_exporter_output() {
    let Some(usdcat) = tool("usdcat") else {
        eprintln!("skipped: usdcat is not on PATH");
        return;
    };
    let usdchecker = tool("usdchecker");
    eprintln!("usdcat: {usdcat}; usdchecker: {usdchecker:?}");

    let dir = scratch_dir("fixtures");
    let _ = std::fs::remove_dir_all(&dir);
    let fixtures = write_all(&dir);
    let mut failures = Vec::new();
    for fixture in &fixtures {
        let path = fixture.path.display().to_string();
        let cat = Command::new("usdcat").arg(&fixture.path).output().unwrap();
        let checked = usdchecker.as_ref().map(|_| {
            Command::new("usdchecker")
                .arg(&fixture.path)
                .output()
                .unwrap()
        });
        match fixture.expect {
            Expect::Valid => {
                if !cat.status.success() {
                    failures.push(format!(
                        "usdcat rejected {path}: {}",
                        String::from_utf8_lossy(&cat.stderr)
                    ));
                }
                if let Some(out) = checked.filter(|o| !o.status.success()) {
                    failures.push(format!(
                        "usdchecker rejected {path}: {}",
                        String::from_utf8_lossy(&out.stdout)
                    ));
                }
            }
            Expect::Invalid(validator) => {
                if let Some(out) = checked {
                    let report = String::from_utf8_lossy(&out.stdout);
                    if out.status.success() || !report.contains(validator) {
                        failures.push(format!(
                            "usdchecker did not report {validator} for control {path}"
                        ));
                    }
                }
            }
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
