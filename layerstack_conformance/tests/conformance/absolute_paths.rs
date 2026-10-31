// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asset paths made absolute from the layer that authors them, as OpenUSD's
//! `SdfLayer::ComputeAbsolutePath` makes them, replayed call by call from
//! `absolute_paths_oracle.py` (see the script for the files and cases):
//! from a plain layer, a layer in a package and the package itself,
//! format arguments included, through [`StageDocument::layer_locations`].
//!
//! From an anonymous layer, OpenUSD turns `./` and `../` paths into search
//! paths (`./wood.png` becomes `wood.png`), which name other assets;
//! layerstack does not anchor them (`None`), and reads every other path as
//! OpenUSD does.
//!
//! OpenUSD: `SdfComputeAssetPathRelativeToLayer`
//! (`pxr/usd/sdf/layerUtils.cpp`). AOUSD Core §9.4, §9.5.

use std::path::{Path, PathBuf};

use layerstack::{AssetResolver, LayerId, StageOptions};
use layerstack_io::{Filesystem, StageDocument};
use serde::Deserialize;
use std::collections::BTreeMap;

const ORACLE: &str = include_str!("../../fixtures/absolute_paths/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd: String,
    cases: Vec<String>,
    anchors: BTreeMap<String, Vec<String>>,
}

/// The fixture directory, absolute, as the filesystem storage names the
/// layers it reads (see `flatten_anchoring.rs`).
fn directory() -> PathBuf {
    if cfg!(target_os = "wasi") {
        return std::env::current_dir()
            .unwrap()
            .join("fixtures/absolute_paths");
    }
    let canonical = layerstack_conformance::workspace_root()
        .join("layerstack_conformance/fixtures/absolute_paths")
        .canonicalize()
        .unwrap();
    match canonical.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => canonical,
    }
}

/// `path` with `/` separators.
fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[test]
fn absolute_paths_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert!(oracle.openusd.starts_with("26."), "{}", oracle.openusd);
    let directory = directory();
    let storage = Filesystem::new(&directory, []).unwrap();
    let root = directory.join("scene/scene.usda");
    let mut document =
        StageDocument::open(storage, root.to_str().unwrap(), StageOptions::default()).unwrap();
    let anonymous = document.create_anonymous_layer().expect("anonymous layer");
    let find = |suffix: &str| -> LayerId {
        document
            .store()
            .layer_ids()
            .into_iter()
            .find(|&id| {
                document
                    .identifier(id)
                    .is_some_and(|identifier| identifier.replace('\\', "/").ends_with(suffix))
            })
            .unwrap_or_else(|| panic!("no layer {suffix}"))
    };
    let anchors = [
        ("scene", find("/scene/scene.usda")),
        ("inner", find("/pkg.usdz[sub/inner.usda]")),
        ("package", find("/pkg.usdz")),
        ("anonymous", anonymous),
    ];
    let prefix = format!("{}/", slashed(&directory));
    let locations = document.layer_locations();
    for (name, id) in anchors {
        for (case, expected) in oracle.cases.iter().zip(&oracle.anchors[name]) {
            let anchored = locations
                .anchor_asset_path(case, id)
                .map(|path| path.replace('\\', "/").replace(&prefix, "<dir>/"));
            if name == "anonymous" && (case.starts_with("./") || case.starts_with("../")) {
                assert_eq!(anchored, None, "{name} {case:?}: OpenUSD gives {expected}");
            } else {
                assert_eq!(
                    anchored.as_deref(),
                    Some(expected.as_str()),
                    "{name} {case:?}"
                );
            }
        }
    }
}
