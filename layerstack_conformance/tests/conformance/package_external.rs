// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An asset a package leaves outside it, replayed from
//! `package_external_oracle.py` (see the script for the files and what it
//! records).

use std::collections::BTreeMap;

use layerstack::asset_dependencies::extract_layer_dependencies;
use layerstack::{AssetResolver, StageOptions};
use layerstack_io::{
    IoError, IoErrorKind, PackageInclusion, PackagePreparationLimits, StageDocument, Storage,
};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/package_external/oracle.json");

/// The fixture's files, by their paths relative to its directory.
const FILES: [(&str, &[u8]); 6] = [
    (
        "set/rock.usda",
        include_bytes!("../../fixtures/package_external/set/rock.usda"),
    ),
    (
        "set/albedo.png",
        include_bytes!("../../fixtures/package_external/set/albedo.png"),
    ),
    (
        "far/outside.png",
        include_bytes!("../../fixtures/package_external/far/outside.png"),
    ),
    (
        "out/set/near.png",
        include_bytes!("../../fixtures/package_external/out/set/near.png"),
    ),
    (
        "pkg.usdz",
        include_bytes!("../../fixtures/package_external/pkg.usdz"),
    ),
    (
        "out/pkg.usdz",
        include_bytes!("../../fixtures/package_external/out/pkg.usdz"),
    ),
];

/// The fixture's files keyed by paths relative to the host's root, as the
/// browser bindings name sources: a relative asset path is anchored to the
/// directory of its layer.
#[derive(Default)]
struct HostRoot(BTreeMap<String, Vec<u8>>);

impl HostRoot {
    fn fixture() -> Self {
        Self(
            FILES
                .iter()
                .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
                .collect(),
        )
    }
}

impl Storage for HostRoot {
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
        let anchored = anchor
            .and_then(|anchor| layerstack::asset::anchor_asset_path(asset, anchor))
            .unwrap_or_else(|| asset.to_owned());
        Ok(anchored
            .strip_prefix("./")
            .map_or_else(|| anchored.clone(), str::to_owned))
    }
    fn exists(&self, identifier: &str) -> Result<bool, IoError> {
        Ok(self.0.contains_key(identifier))
    }
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.0
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, "no such file"))
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.0.insert(identifier.to_owned(), bytes.to_vec());
        Ok(())
    }
}

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    assert_eq!(oracle["openusd"], "26.11", "oracle from OpenUSD 26.11");
    oracle
}

/// The file an anchored identifier names, relative to the host's root:
/// OpenUSD's absolute `<dir>/set/albedo.png` and layerstack's
/// `./set/albedo.png`, marked relative to the host's root, are both
/// `set/albedo.png`; a kept search path is itself.
fn named(identifier: &str) -> &str {
    identifier
        .strip_prefix("<dir>/")
        .or_else(|| identifier.strip_prefix("./"))
        .unwrap_or(identifier)
}

/// A path authored in a packaged layer anchors as OpenUSD anchors it: one
/// starting with `./` names a member, which is not there, and a search
/// path naming no member is anchored beside the package file when a file
/// is there and kept otherwise, to resolve from the host's root.
///
/// `../set/albedo.png` names no file in either: OpenUSD anchors it to the
/// member `pkg.usdz[../set/albedo.png]`, which never resolves, and
/// layerstack anchors it to nothing.
#[test]
fn paths_in_a_package_anchor_as_openusd_anchors_them() {
    let oracle = oracle();
    let anchored = oracle["anchored"].as_object().unwrap();
    assert_eq!(anchored.len(), 2, "both packages are recorded");
    for (package, attributes) in anchored {
        let document =
            StageDocument::open(HostRoot::fixture(), package, StageOptions::default()).unwrap();
        let layer = document.stage().stage().root_layer().unwrap();
        for (name, expected) in attributes.as_object().unwrap() {
            let authored = expected["authored"].as_str().unwrap();
            let openusd = expected["anchored"].as_str().unwrap();
            let anchored = document
                .layer_locations()
                .anchor_asset_path(authored, layer);
            if authored.starts_with("../") {
                assert_eq!(expected["resolved"], "", "{package} {name}");
                assert_eq!(anchored, None, "{package} {name}");
                continue;
            }
            let anchored = anchored.unwrap_or_else(|| panic!("{package} {name} anchors"));
            assert_eq!(named(&anchored), named(openusd), "{package} {name}");
        }
    }
}

/// A kept asset is written as storage identifies it, relative to the
/// host's root (`set/albedo.png`), and not marked `./`, which inside a
/// package would name a member. Read back from the package, at the root
/// or below it, it names the file the source did, as OpenUSD resolves the
/// same search path from a package there.
#[test]
fn a_kept_asset_names_its_source_from_the_package() {
    let oracle = oracle();
    let mut document = StageDocument::open(
        HostRoot::fixture(),
        "set/rock.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = document.stage().stage().root_layer().unwrap();
    let package = document
        .prepare_package(
            root,
            "rock.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::KeepExternal,
        )
        .unwrap();
    assert!(package.report().is_complete(), "{:?}", package.report());
    let plan = package.plan().unwrap();
    assert!(plan.assets.is_empty(), "nothing is copied");
    let written: Vec<String> = extract_layer_dependencies(
        &plan.layers[0].layer,
        document.store().paths(),
        document.store().tokens(),
    )
    .into_iter()
    .map(|dependency| dependency.identifier.to_string())
    .collect();
    for (source, identifier) in [
        ("set/albedo.png", "set/albedo.png"),
        ("far/outside.png", "far/outside.png"),
    ] {
        assert!(
            written.iter().any(|written| written == identifier),
            "{source} written as {identifier} in {written:?}"
        );
    }
    for (at, searched) in [("rock.usdz", "pkg.usdz"), ("out/rock.usdz", "out/pkg.usdz")] {
        document.export_package(at, &package).unwrap();
        let reread = StageDocument::open(
            HostRoot(document.storage().0.clone()),
            at,
            StageOptions::default(),
        )
        .unwrap();
        let layer = reread.stage().stage().root_layer().unwrap();
        let openusd = &oracle["anchored"][searched]["searchPath"];
        assert_eq!(openusd["resolved"], "<dir>/set/albedo.png", "{searched}");
        let anchored = reread
            .layer_locations()
            .anchor_asset_path("set/albedo.png", layer)
            .unwrap();
        assert_eq!(named(&anchored), "set/albedo.png", "{at}");
        assert_eq!(
            named(&anchored),
            named(openusd["anchored"].as_str().unwrap()),
            "{at}"
        );
    }
}
