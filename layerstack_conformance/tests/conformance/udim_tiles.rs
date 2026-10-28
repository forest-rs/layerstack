// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! UDIM texture tiles are found and packaged as OpenUSD 26.08 finds and
//! packages them.
//!
//! `scripts/udim_tiles_oracle.py` writes the layers and tiles of
//! `fixtures/udim_tiles` and records OpenUSD's answers (see the script).
//! Here the same files are read through `StageDocument`, and:
//!
//! - `UdimPattern` recognises and fills in the spellings
//!   `UsdShadeUdimUtils::IsUdimIdentifier` and `ReplaceUdimPattern` do;
//! - `resolve_udim_tiles` finds, for every authored pattern relative to
//!   its layer, the tiles and resolved pattern `ResolveUdimTilePaths` and
//!   `ResolveUdimPath` find: anchored to a referenced layer, through a
//!   `..` segment, skipping missing tiles and files outside 1001-1100, in
//!   a directory name, in time samples and in arrays;
//! - `prepare_package` lists the assets `UsdUtilsComputeAllDependencies`
//!   lists and leaves unresolved the same uses: a set without tiles, a bare
//!   `<UDIM>` and a variable expression, which OpenUSD evaluates but does
//!   not expand;
//! - packaging `package.usda` keeps every value's `<UDIM>` and carries
//!   every tile: inside the written package each value finds the tiles
//!   OpenUSD's package finds, with the same contents.
//!
//! Member names differ on purpose (`DELIBERATE`): OpenUSD keeps a relative
//! path inside the asset's directory (`textures/color.<UDIM>.png`) and
//! remaps others to numbered directories (`0/color.<UDIM>.png`); layerstack
//! names every set `assets/NNNNNN/` and keeps its file name, as OpenUSD keeps
//! it. `package.usda` leaves out the folder case, which OpenUSD packages as a
//! single member literally named `tiles/<UDIM>/color.png`; layerstack packages
//! each tile (`layerstack_io`'s unit tests).
//!
//! Spec: AOUSD Core §9.4 (anchored asset paths), §9.7 (packages), §16.4
//! (USDZ). The specification does not define UDIM patterns.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use layerstack::asset::{UdimPattern, UdimTile, normalize_asset_path};
use layerstack::asset_dependencies::{
    AssetDependencyKind, DependencyTarget, extract_layer_dependencies,
};
use layerstack::{LayerId, StageOptions};
use layerstack_conformance::workspace_root;
use layerstack_io::{
    IoError, IoErrorKind, PackageInclusion, PackagePreparationLimits, StageDocument, Storage,
    UdimDiscovery,
};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/udim_tiles/oracle.json");

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

/// The fixture's files by relative path, resolved as OpenUSD's default
/// resolver resolves them without search paths: `./` and `../` beside the
/// anchor; a search path beside the anchor when the file is there, else
/// from the fixture root. Lists directories.
#[derive(Clone, Debug, Default)]
struct Fixture {
    files: BTreeMap<String, Vec<u8>>,
}

impl Fixture {
    fn read() -> Self {
        fn walk(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(root, &path, files);
                } else if path.file_name().is_some_and(|name| name != "oracle.json") {
                    let name = path.strip_prefix(root).unwrap().to_str().unwrap();
                    files.insert(name.replace('\\', "/"), std::fs::read(&path).unwrap());
                }
            }
        }
        let root = workspace_root().join("layerstack_conformance/fixtures/udim_tiles");
        let mut files = BTreeMap::new();
        walk(&root, &root, &mut files);
        Self { files }
    }
}

fn normalized(path: &str) -> String {
    let path = normalize_asset_path(path);
    path.strip_prefix("./").unwrap_or(&path).to_owned()
}

impl Storage for Fixture {
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
        let directory = anchor.and_then(|a| a.rsplit_once('/')).map(|(d, _)| d);
        let beside = |directory: &str| normalized(&format!("{directory}/{asset}"));
        Ok(match directory {
            Some(directory) if asset.starts_with("./") || asset.starts_with("../") => {
                beside(directory)
            }
            Some(directory) if self.files.contains_key(&beside(directory)) => beside(directory),
            _ => normalized(asset),
        })
    }

    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.files
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, "no such file"))
    }

    fn read_bounded(&self, identifier: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
        let bytes = self.read(identifier)?;
        if bytes.len() as u64 > max_bytes {
            return Err(IoError::new(IoErrorKind::Rejected, "too large"));
        }
        Ok(bytes)
    }

    fn exists(&self, identifier: &str) -> Result<bool, IoError> {
        Ok(self.files.contains_key(identifier))
    }

    fn list_siblings(&self, identifier: &str) -> Result<Option<Vec<String>>, IoError> {
        let parent = |path: &str| path.rsplit_once('/').map(|(d, _)| d.to_owned());
        let directory = parent(identifier);
        Ok(Some(
            self.files
                .keys()
                .filter(|name| parent(name) == directory)
                .cloned()
                .collect(),
        ))
    }

    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.files.insert(identifier.to_owned(), bytes.to_vec());
        Ok(())
    }
}

fn open(storage: Fixture, root: &str) -> StageDocument<Fixture> {
    StageDocument::open(storage, root, StageOptions::default()).unwrap()
}

/// The loaded layer bound to `identifier`.
fn layer(document: &StageDocument<Fixture>, identifier: &str) -> LayerId {
    document
        .load_report()
        .layers
        .iter()
        .copied()
        .find(|&id| document.identifier(id) == Some(identifier))
        .unwrap_or_else(|| panic!("{identifier} is loaded"))
}

#[test]
fn spellings_parse_and_fill_in_as_in_openusd() {
    for record in oracle()["spellings"].as_array().unwrap() {
        let path = record["path"].as_str().unwrap();
        let pattern = UdimPattern::parse(path);
        assert_eq!(pattern.is_some(), record["is_udim"], "{path}");
        let tile = UdimTile::new(1011).unwrap();
        let filled = pattern.map_or_else(|| path.to_owned(), |p| p.tile_path(tile));
        assert_eq!(filled, record["tile_1011"].as_str().unwrap(), "{path}");
    }
}

#[test]
fn every_authored_pattern_finds_the_tiles_openusd_finds() {
    let document = open(Fixture::read(), "scene.usda");
    for record in oracle()["tiles"].as_array().unwrap() {
        let pattern = record["pattern"].as_str().unwrap();
        let anchor = layer(&document, record["layer"].as_str().unwrap());
        let expected: Vec<(String, u16)> = record["tiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tile| {
                let number = tile[1].as_str().unwrap().parse().unwrap();
                (tile[0].as_str().unwrap().to_owned(), number)
            })
            .collect();
        let resolved = record["resolved"].as_str().unwrap();
        let Some(udim) = UdimPattern::parse(pattern) else {
            assert!(expected.is_empty(), "{pattern} is no pattern");
            continue;
        };
        let found = document
            .resolve_udim_tiles(udim, Some(anchor), &mut UdimDiscovery::default())
            .unwrap();
        let tiles: Vec<_> = found
            .tiles
            .iter()
            .map(|tile| (tile.identifier.clone(), tile.tile.number()))
            .collect();
        assert_eq!(tiles, expected, "{pattern}");
        assert_eq!(
            found.resolved_pattern.as_deref().unwrap_or(""),
            resolved,
            "{pattern}"
        );
        for tile in &found.tiles {
            assert_eq!(tile.asset_path, udim.tile_path(tile.tile));
        }
    }
}

#[test]
fn dependency_discovery_lists_every_tile_and_the_same_unresolved_uses() {
    let oracle = oracle();
    let dependencies = &oracle["dependencies"];
    let mut document = open(Fixture::read(), "scene.usda");
    let root = layer(&document, "scene.usda");
    let prepared = document
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    let report = prepared.report();
    let layers: BTreeSet<_> = report
        .layers
        .iter()
        .map(|&id| document.identifier(id).unwrap().to_owned())
        .collect();
    let expected: BTreeSet<_> = dependencies["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(layers, expected);
    let assets: Vec<String> = report.assets().iter().map(|a| a.to_string()).collect();
    let expected: Vec<_> = dependencies["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(assets, expected);

    // OpenUSD reports anchored paths; trace each back to its authored use.
    let authored: BTreeMap<&str, &str> = oracle["tiles"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["layer"] == "scene.usda")
        .map(|r| {
            (
                r["property"].as_str().unwrap(),
                r["pattern"].as_str().unwrap(),
            )
        })
        .collect();
    let expected: BTreeSet<&str> = dependencies["unresolved_properties"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| authored[p.as_str().unwrap()])
        .collect();
    let unresolved: BTreeSet<&str> = report
        .dependencies
        .iter()
        .filter(|d| {
            d.targets
                .iter()
                .any(|t| matches!(t, DependencyTarget::Unresolved(_)))
        })
        .map(|d| &*d.source.identifier)
        .collect();
    assert_eq!(unresolved, expected);
    assert_eq!(unresolved.len() as u64, dependencies["unresolved"]);
    assert!(prepared.plan().is_none(), "unresolved uses prevent a plan");

    // Each set's later tiles are looked for once, and each directory is
    // listed once. Of twelve pattern uses, the array's `color` repeats
    // `dotted`'s spelling and costs nothing; every other use finds its
    // first tile with one identify and one probe per tile up to it, 100
    // for `none`, which has no tile; `search` and `Model.stone` then reuse
    // a set found before (`sets_reused` counts all three). Five sets share
    // `textures`, listed once (15 entries); `shared` and `models/tex` are
    // listed once each; the folder set probes its 99 later tiles. OpenUSD
    // probes 100 tiles for each use after finding its first: 1100 more
    // resolves here.
    let work = prepared.udim_work();
    assert_eq!(
        (
            work.identifies,
            work.existence_checks,
            work.listings,
            work.listings_reused,
            work.listed_entries,
            work.member_lookups,
            work.package_reads,
            work.sets_reused,
        ),
        (112, 112 + 99, 3, 4, 15 + 1 + 2, 0, 0, 3)
    );
}

fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap().to_owned()
}

/// For every asset value of a packaged layer, by property: the tiles it
/// finds inside the package and their contents.
type Values = BTreeSet<(String, String, BTreeMap<u16, String>)>;

#[test]
fn packaging_keeps_every_pattern_and_carries_every_tile() {
    let oracle = oracle();
    let mut document = open(Fixture::read(), "package.usda");
    let root = layer(&document, "package.usda");
    let prepared = document
        .prepare_package(
            root,
            "package.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(prepared.report().is_complete(), "{:?}", prepared.report());
    let plan = prepared.plan().unwrap();
    let tiles = oracle["package"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m.as_str().unwrap().ends_with(".png"))
        .count();
    // OpenUSD stores the `search` set twice, once under its remapped name;
    // layerstack stores each resolved set once, `stone` shared by two layers.
    assert_eq!(tiles, 17);
    assert_eq!(plan.assets.len(), 14);
    let usdz = prepared.write_usdz().unwrap();

    let mut storage = Fixture::default();
    storage.files.insert("out.usdz".into(), usdz);
    let mut packaged = open(storage, "out.usdz");
    let store = packaged.store().snapshot();
    let mut values = BTreeMap::<&str, Values>::new();
    for &id in &packaged.load_report().layers.clone() {
        let name = if Some(id) == packaged.stage().stage().root_layer() {
            "root"
        } else {
            "models/model.usda"
        };
        let layer = store.layer(id).unwrap();
        for dependency in extract_layer_dependencies(layer, store.paths(), store.tokens()) {
            let spec = dependency.spec.as_ref().unwrap();
            let mut property = spec.display(store.paths(), store.tokens());
            if let Some(field) = dependency.field.filter(|_| !property.contains('.')) {
                property = format!("{property}.{}", store.tokens().resolve(field));
            }
            let Some(pattern) = UdimPattern::parse(&dependency.identifier) else {
                assert_eq!(dependency.kind, AssetDependencyKind::Reference);
                continue;
            };
            let found = packaged
                .resolve_udim_tiles(pattern, Some(id), &mut UdimDiscovery::default())
                .unwrap();
            let mut tiles = BTreeMap::new();
            for tile in found.tiles {
                let bytes = packaged.read_asset_bytes(&tile.identifier, None).unwrap();
                let text = String::from_utf8(bytes.bytes.to_vec()).unwrap();
                tiles.insert(tile.tile.number(), text);
            }
            // The value keeps the authored file name around the marker.
            values.entry(name).or_default().insert((
                property,
                file_name(&dependency.identifier),
                tiles,
            ));
        }
    }
    for (name, records) in oracle["package"]["layers"].as_object().unwrap() {
        let expected: Values = records
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let value = r["value"].as_str().unwrap();
                let tiles = r["tiles"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(tile, text)| (tile.parse().unwrap(), text.as_str().unwrap().into()))
                    .collect();
                (
                    r["property"].as_str().unwrap().to_owned(),
                    file_name(value),
                    tiles,
                )
            })
            .collect();
        assert_eq!(values[name.as_str()], expected, "{name}");
    }
}
