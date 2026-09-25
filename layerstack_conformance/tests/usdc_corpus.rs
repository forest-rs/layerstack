// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reads every crate file in the repository.
//!
//! The supplemental corpus and the conformance fixtures hold crate files
//! written by OpenUSD and by the supplemental writer, under `.usd` and
//! `.usdc` names. Each one found by its `PXR-USDC` magic must read into a
//! layer; the only failures allowed are the features this reader reports as
//! unsupported. A panic or an unexpected error is a decoding bug.
//!
//! Spec: AOUSD Core §16.3.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use layerstack::doc::LayerId;
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, ResolvedAsset};
use layerstack_conformance::workspace_root;
use layerstack_usdc::UsdcError;

/// Directories holding crate files, relative to the workspace root.
const ROOTS: &[&str] = &[
    "core-spec-supplemental-release_dec2025",
    "layerstack_conformance/fixtures",
];

/// Files that must fail, with the error they report: features of readable
/// versions that layerstack cannot represent.
const EXPECTED_ERRORS: &[(&str, UsdcError)] = &[
    (
        "layerstack_conformance/fixtures/usdc_versions/spline_loop_boundary.usdc",
        UsdcError::UnsupportedFeature {
            feature: "spline loopBoundaryTime",
        },
    ),
    (
        "layerstack_conformance/fixtures/usdc_versions/spline_time_valued.usdc",
        UsdcError::UnsupportedFeature {
            feature: "time-valued spline",
        },
    ),
];

/// A resolver that resolves nothing, so each file is read on its own.
struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// Collects the crate files under `dir`, found by their magic.
fn crate_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            crate_files(&path, out);
        } else if std::fs::read(&path).is_ok_and(|bytes| bytes.starts_with(b"PXR-USDC")) {
            out.push(path);
        }
    }
}

#[test]
fn every_crate_file_in_the_repository_reads() {
    let root = workspace_root();
    let mut files = Vec::new();
    for dir in ROOTS {
        crate_files(&root.join(dir), &mut files);
    }
    files.sort();
    assert!(files.len() > 600, "found only {} crate files", files.len());

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failures = Vec::new();
    for path in &files {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let data = std::fs::read(path).expect("crate file");
        let result = catch_unwind(AssertUnwindSafe(|| {
            layerstack_usdc::read_usdc(
                &data,
                LayerId(1),
                &mut TokenInterner::default(),
                &mut PathInterner::default(),
                &mut NoAssets,
            )
            .map(|_| ())
        }));
        let expected = EXPECTED_ERRORS
            .iter()
            .find(|(file, _)| *file == name)
            .map(|(_, error)| error);
        match (result, expected) {
            (Err(_), _) => failures.push(format!("{name}: panicked")),
            (Ok(Ok(())), None) => {}
            (Ok(Err(error)), Some(expected)) if error == *expected => {}
            (Ok(result), expected) => {
                failures.push(format!("{name}: {result:?}, expected {expected:?}"));
            }
        }
    }
    std::panic::set_hook(hook);
    assert!(
        failures.is_empty(),
        "{} of {} crate files failed:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}
