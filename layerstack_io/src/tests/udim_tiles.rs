// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! UDIM tile discovery and its storage work.

use super::*;
use layerstack::asset::{UdimPattern, normalize_asset_path};

/// Files by relative path, resolved as OpenUSD's default resolver resolves
/// them without search paths: `./` and `../` beside the anchor, a search
/// path beside the anchor when the file is there and else from the root.
/// Counts every call discovery may make; lists directories only when asked.
#[derive(Debug, Default)]
struct Tree {
    files: BTreeMap<String, Vec<u8>>,
    lists: bool,
    /// Names match ignoring ASCII case, as on default APFS or NTFS; listings
    /// still spell them as stored.
    ignores_case: bool,
    reads: Count,
    identifies: Count,
    exists: Count,
    listings: Count,
}

impl Tree {
    fn with(files: &[&str]) -> Self {
        Self {
            files: files
                .iter()
                .map(|name| ((*name).into(), name.as_bytes().to_vec()))
                .collect(),
            ..Self::default()
        }
    }
    fn listing(mut self) -> Self {
        self.lists = true;
        self
    }
    /// The stored name `identifier` names, as the filesystem matches it.
    fn stored(&self, identifier: &str) -> Option<&String> {
        self.files.keys().find(|name| {
            if self.ignores_case {
                name.eq_ignore_ascii_case(identifier)
            } else {
                *name == identifier
            }
        })
    }
    fn calls(&self) -> [usize; 3] {
        [
            self.identifies.get(),
            self.exists.get(),
            self.listings.get(),
        ]
    }
}

fn normalized(path: &str) -> String {
    let path = normalize_asset_path(path);
    path.strip_prefix("./").unwrap_or(&path).into()
}

impl Storage for Tree {
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
        self.identifies.add();
        let directory = anchor.and_then(|a| a.rsplit_once('/')).map(|(d, _)| d);
        let beside = |directory: &str| normalized(&format!("{directory}/{asset}"));
        Ok(match directory {
            Some(directory) if asset.starts_with("./") || asset.starts_with("../") => {
                beside(directory)
            }
            Some(directory) if self.stored(&beside(directory)).is_some() => beside(directory),
            _ => normalized(asset),
        })
    }
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.reads.add();
        self.stored(identifier)
            .map(|name| self.files[name].clone())
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, format!("missing {identifier}")))
    }
    fn read_bounded(&self, identifier: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
        let bytes = self.read(identifier)?;
        if bytes.len() as u64 > max_bytes {
            return Err(IoError::new(IoErrorKind::Rejected, "too large"));
        }
        Ok(bytes)
    }
    fn exists(&self, identifier: &str) -> Result<bool, IoError> {
        self.exists.add();
        Ok(self.stored(identifier).is_some())
    }
    fn list_siblings(&self, identifier: &str) -> Result<Option<Vec<String>>, IoError> {
        if !self.lists {
            return Ok(None);
        }
        self.listings.add();
        let parent = |path: &str| path.rsplit_once('/').map(|(d, _)| String::from(d));
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
        self.files.insert(identifier.into(), bytes.to_vec());
        Ok(())
    }
}

const SCENE: &str = "#usda 1.0\ndef \"Model\" (references = @./models/model.usda@) {}\n";
const MODEL: &str = "#usda 1.0\ndef \"Model\" {}\n";

/// A root and a referenced layer one directory down, with tiles beside it.
fn scene(tiles: &[&str]) -> Tree {
    let mut tree = Tree::with(tiles);
    tree.files.insert("scene.usda".into(), SCENE.into());
    tree.files.insert("models/model.usda".into(), MODEL.into());
    tree
}

fn open(tree: Tree) -> (StageDocument<Tree>, LayerId) {
    let document = StageDocument::open(tree, "scene.usda", StageOptions::default()).unwrap();
    let model = document.catalog.ids["models/model.usda"];
    (document, model)
}

fn numbers(found: &UdimTiles) -> Vec<u16> {
    found.tiles.iter().map(|t| t.tile.number()).collect()
}

const GAPS: &[&str] = &[
    "models/tex/bark.1000.png",
    "models/tex/bark.1001.png",
    "models/tex/bark.1003.png",
    "models/tex/bark.1100.png",
    "models/tex/bark.1101.png",
    "models/tex/other.1002.png",
];

#[test]
fn tiles_resolve_beside_their_layer_in_order_skipping_gaps_and_the_range_ends() {
    for lists in [false, true] {
        let tree = scene(GAPS);
        let (mut document, model) = open(if lists { tree.listing() } else { tree });
        let before = document.storage.calls();
        let pattern = UdimPattern::parse("./tex/bark.<UDIM>.png").unwrap();
        let found = document
            .resolve_udim_tiles(pattern, Some(model), &mut UdimDiscovery::default())
            .unwrap();
        assert_eq!(numbers(&found), [1001, 1003, 1100], "lists {lists}");
        assert_eq!(
            found.resolved_pattern.as_deref(),
            Some("models/tex/bark.<UDIM>.png")
        );
        let tile = &found.tiles[1];
        assert_eq!(tile.asset, "./tex/bark.1003.png");
        assert_eq!(tile.identifier, "models/tex/bark.1003.png");
        let calls = document.storage.calls();
        let spent = [0, 1, 2].map(|i| calls[i] - before[i]);
        // One identify and one probe find tile 1001; the rest of the set is
        // one listing, or a probe for each of the 99 later tiles.
        let expected = if lists {
            UdimDiscoveryWork {
                identifies: 1,
                existence_checks: 1,
                listings: 1,
                listed_entries: 6,
                ..UdimDiscoveryWork::default()
            }
        } else {
            UdimDiscoveryWork {
                identifies: 1,
                existence_checks: 100,
                ..UdimDiscoveryWork::default()
            }
        };
        assert_eq!(found.work, expected);
        assert_eq!(
            spent,
            [
                expected.identifies,
                expected.existence_checks,
                expected.listings
            ]
        );
        let bytes = document.read_asset_bytes(&tile.identifier, None).unwrap();
        assert_eq!(&bytes.bytes[..], b"models/tex/bark.1003.png");
    }
}

#[test]
fn a_set_without_tile_1001_is_found_from_its_first_tile() {
    let (document, model) = open(scene(&["models/tex/bark.1012.png"]).listing());
    let pattern = UdimPattern::parse("./tex/bark.<UDIM>.png").unwrap();
    let found = document
        .resolve_udim_tiles(pattern, Some(model), &mut UdimDiscovery::default())
        .unwrap();
    assert_eq!(numbers(&found), [1012]);
    assert_eq!(
        (found.work.identifies, found.work.existence_checks),
        (12, 12)
    );
    assert_eq!(found.work.listings, 1);
}

#[test]
fn a_set_without_tiles_is_empty_after_probing_the_whole_range() {
    let (document, model) = open(scene(&["models/tex/bark.1101.png"]).listing());
    let pattern = UdimPattern::parse("./tex/bark.<UDIM>.png").unwrap();
    let found = document
        .resolve_udim_tiles(pattern, Some(model), &mut UdimDiscovery::default())
        .unwrap();
    assert!(found.tiles.is_empty());
    assert_eq!(found.resolved_pattern, None);
    assert_eq!(
        (found.work.identifies, found.work.existence_checks),
        (100, 100)
    );
    assert_eq!(found.work.listings, 0);
}

#[test]
fn a_search_path_resolves_beside_the_layer_first_then_from_the_root() {
    let tiles = [
        "models/tex/bark.1001.png",
        "tex/bark.1001.png",
        "tex/bark.1002.png",
    ];
    let (document, model) = open(scene(&tiles).listing());
    let pattern = UdimPattern::parse("tex/bark.<UDIM>.png").unwrap();
    let found = document
        .resolve_udim_tiles(pattern, Some(model), &mut UdimDiscovery::default())
        .unwrap();
    assert_eq!(numbers(&found), [1001]);
    assert_eq!(found.tiles[0].identifier, "models/tex/bark.1001.png");
    let found = document
        .resolve_udim_tiles(pattern, None, &mut UdimDiscovery::default())
        .unwrap();
    assert_eq!(numbers(&found), [1001, 1002]);
    assert_eq!(found.tiles[1].identifier, "tex/bark.1002.png");
}

#[test]
fn a_tile_in_a_directory_name_is_probed_even_when_the_storage_lists() {
    let tiles = ["models/tiles/1001/c.png", "models/tiles/1002/c.png"];
    let (document, model) = open(scene(&tiles).listing());
    let pattern = UdimPattern::parse("./tiles/<UDIM>/c.png").unwrap();
    let found = document
        .resolve_udim_tiles(pattern, Some(model), &mut UdimDiscovery::default())
        .unwrap();
    assert_eq!(numbers(&found), [1001, 1002]);
    assert_eq!(found.tiles[1].identifier, "models/tiles/1002/c.png");
    assert_eq!((found.work.existence_checks, found.work.listings), (100, 0));
}

#[test]
fn a_dot_dot_after_the_marker_is_ambiguous_as_in_openusd() {
    // `ResolveUdimPath` warns that the first tile's resolution "gave
    // ambiguous result" when it no longer ends with the authored suffix.
    let (document, model) = open(scene(&["models/c.png"]));
    let pattern = UdimPattern::parse("./<UDIM>/../c.png").unwrap();
    let error = document
        .resolve_udim_tiles(pattern, Some(model), &mut UdimDiscovery::default())
        .unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Rejected);
    assert!(error.message.contains("does not spell"), "{error}");
    // A `..` before the marker is ordinary anchoring.
    let (document, model) = open(scene(&["shared/c.1001.png"]));
    let pattern = UdimPattern::parse("../shared/c.<UDIM>.png").unwrap();
    let found = document
        .resolve_udim_tiles(pattern, Some(model), &mut UdimDiscovery::default())
        .unwrap();
    assert_eq!(found.tiles[0].identifier, "shared/c.1001.png");
    assert_eq!(
        found.resolved_pattern.as_deref(),
        Some("shared/c.<UDIM>.png")
    );
}

#[test]
fn an_anchor_without_a_source_and_a_storage_without_probes_are_errors() {
    let (document, _) = open(scene(&[]));
    let pattern = UdimPattern::parse("c.<UDIM>.png").unwrap();
    let error = document
        .resolve_udim_tiles(pattern, Some(LayerId(9999)), &mut UdimDiscovery::default())
        .unwrap_err();
    assert_eq!(error.kind, IoErrorKind::MissingLayer);
    let document = StageDocument::open(
        Memory::with(&[("root.usda", "#usda 1.0\n")]),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let error = document
        .resolve_udim_tiles(pattern, None, &mut UdimDiscovery::default())
        .unwrap_err();
    assert_eq!(error.kind, IoErrorKind::Unsupported);
}

fn usdz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let files: Vec<_> = files
        .iter()
        .map(|(name, data)| layerstack_usdz::PackageFile::new(name, data))
        .collect();
    layerstack_usdz::write_usdz(&files).unwrap()
}

#[test]
fn tiles_inside_a_package_come_from_its_index_without_storage_work() {
    let package = usdz(&[
        ("scene.usda", b"#usda 1.0\n"),
        ("tex/c.1001.png", b"one"),
        ("tex/c.1002.png", b"two"),
        ("tex/c.1011.png", b"eleven"),
    ]);
    let mut tree = Tree::default().listing();
    tree.files.insert("pkg.usdz".into(), package.clone());
    let mut document = StageDocument::open(tree, "pkg.usdz", StageOptions::default()).unwrap();
    let root = document.stage.stage().root_layer().unwrap();
    let before = document.storage.calls();
    let pattern = UdimPattern::parse("./tex/c.<UDIM>.png").unwrap();
    let found = document
        .resolve_udim_tiles(pattern, Some(root), &mut UdimDiscovery::default())
        .unwrap();
    assert_eq!(numbers(&found), [1001, 1002, 1011]);
    assert_eq!(
        found.resolved_pattern.as_deref(),
        Some("pkg.usdz[tex/c.<UDIM>.png]")
    );
    assert_eq!(found.tiles[2].identifier, "pkg.usdz[tex/c.1011.png]");
    assert_eq!(
        found.work,
        UdimDiscoveryWork {
            member_lookups: 100,
            ..UdimDiscoveryWork::default()
        }
    );
    assert_eq!(document.storage.calls(), before);
    let bytes = document
        .read_asset_bytes(&found.tiles[2].identifier, None)
        .unwrap();
    assert_eq!(&bytes.bytes[..], b"eleven");

    // An explicit package path from a loose layer reads the package once.
    let mut tree = scene(&[]).listing();
    tree.files.insert("models/pkg.usdz".into(), package);
    let (document, model) = open(tree);
    let pattern = UdimPattern::parse("./pkg.usdz[tex/c.<UDIM>.png]").unwrap();
    let found = document
        .resolve_udim_tiles(pattern, Some(model), &mut UdimDiscovery::default())
        .unwrap();
    assert_eq!(numbers(&found), [1001, 1002, 1011]);
    assert_eq!(found.tiles[0].asset, "./pkg.usdz[tex/c.1001.png]");
    assert_eq!(found.tiles[0].identifier, "models/pkg.usdz[tex/c.1001.png]");
    assert_eq!(found.work.package_reads, 1);
    assert_eq!(found.work.existence_checks, 0);
}

#[cfg(feature = "std")]
#[test]
fn the_filesystem_lists_files_beside_an_identifier_and_answers_existence() {
    use std::fs;
    #[cfg(target_os = "wasi")]
    let base = std::env::current_dir().unwrap();
    #[cfg(not(target_os = "wasi"))]
    let base = std::env::temp_dir();
    let directory = base.join("layerstack-udim-listing");
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(directory.join("tex/sub.1002.png")).unwrap();
    fs::write(directory.join("tex/c.1001.png"), b"one").unwrap();
    fs::write(directory.join("tex/c.1003.png"), b"three").unwrap();
    let storage = Filesystem::new(&directory, []).unwrap();
    let tile = storage.identify("tex/c.1001.png", None).unwrap();
    assert!(storage.exists(&tile).unwrap());
    let missing = storage.identify("tex/c.1002.png", None).unwrap();
    assert!(!storage.exists(&missing).unwrap());
    let folder = storage.identify("tex/sub.1002.png", None).unwrap();
    assert!(
        !storage.exists(&folder).unwrap(),
        "a directory is no source"
    );
    let mut listed = storage.list_siblings(&missing).unwrap().unwrap();
    listed.sort();
    let expected =
        ["tex/c.1001.png", "tex/c.1003.png"].map(|name| storage.identify(name, None).unwrap());
    assert_eq!(listed, expected);
    let absent = storage.identify("none/c.1001.png", None).unwrap();
    assert_eq!(storage.list_siblings(&absent).unwrap(), Some(Vec::new()));
    let mut document =
        StageDocument::create(storage, "scene.usda", StageOptions::default()).unwrap();
    assert!(document.save().failures.is_empty());
    let root = document.stage.stage().root_layer().unwrap();
    let pattern = UdimPattern::parse("./tex/c.<UDIM>.png").unwrap();
    let found = document
        .resolve_udim_tiles(pattern, Some(root), &mut UdimDiscovery::default())
        .unwrap();
    assert_eq!(numbers(&found), [1001, 1003]);
    assert_eq!(found.work.listings, 1);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_kept_discovery_answers_again_without_storage_work() {
    let tiles = [
        "models/tex/bark.1001.png",
        "models/tex/bark.1002.png",
        "models/tex/leaf.1001.png",
    ];
    let (document, model) = open(scene(&tiles).listing());
    let root = document.stage.stage().root_layer().unwrap();
    let mut discovery = UdimDiscovery::default();
    let bark = UdimPattern::parse("./tex/bark.<UDIM>.png").unwrap();
    let first = document
        .resolve_udim_tiles(bark, Some(model), &mut discovery)
        .unwrap();
    assert_eq!(first.work.listings, 1);
    // The same use again: the kept answer, and no storage call.
    let before = document.storage.calls();
    let again = document
        .resolve_udim_tiles(bark, Some(model), &mut discovery)
        .unwrap();
    assert_eq!(again.tiles, first.tiles);
    assert_eq!(document.storage.calls(), before);
    assert_eq!(
        again.work,
        UdimDiscoveryWork {
            sets_reused: 1,
            ..UdimDiscoveryWork::default()
        }
    );
    // The same set spelled from another layer finds its first tile, then
    // reuses the set; another set in the directory reuses the listing.
    let from_root = UdimPattern::parse("./models/tex/bark.<UDIM>.png").unwrap();
    let found = document
        .resolve_udim_tiles(from_root, Some(root), &mut discovery)
        .unwrap();
    assert_eq!(found.tiles.len(), 2);
    assert_eq!(
        (found.work.existence_checks, found.work.sets_reused),
        (1, 1)
    );
    let leaf = UdimPattern::parse("./tex/leaf.<UDIM>.png").unwrap();
    let found = document
        .resolve_udim_tiles(leaf, Some(model), &mut discovery)
        .unwrap();
    assert_eq!(found.tiles.len(), 1);
    assert_eq!((found.work.listings, found.work.listings_reused), (0, 1));
    assert_eq!(discovery.work().listings, 1);
    assert_eq!(discovery.work().sets_reused, 2);
    discovery.clear();
    assert_eq!(discovery.work(), UdimDiscoveryWork::default());
    let mut probing = UdimDiscovery::probing();
    let found = document
        .resolve_udim_tiles(bark, Some(model), &mut probing)
        .unwrap();
    assert_eq!(found.tiles.len(), 2);
    assert_eq!((found.work.listings, found.work.existence_checks), (0, 100));
    probing.clear();
    let found = document
        .resolve_udim_tiles(leaf, Some(model), &mut probing)
        .unwrap();
    assert_eq!(found.work.listings, 0, "clearing keeps probing");
    let found = document
        .resolve_udim_tiles(bark, Some(model), &mut discovery)
        .unwrap();
    assert_eq!(found.work.listings, 1, "a cleared discovery asks again");
}

#[test]
fn a_listing_spelled_in_another_case_is_confirmed_by_probes() {
    for (files, authored) in [
        // Every tile stored in another case than authored.
        (
            ["models/tex/bark.1001.png", "models/tex/bark.1002.png"],
            "./tex/Bark.<UDIM>.png",
        ),
        // The first tile as authored, a later one in another case.
        (
            ["models/tex/Bark.1001.png", "models/tex/bark.1002.png"],
            "./tex/Bark.<UDIM>.png",
        ),
    ] {
        let mut tree = scene(&files).listing();
        tree.ignores_case = true;
        let (document, model) = open(tree);
        let pattern = UdimPattern::parse(authored).unwrap();
        for mut discovery in [UdimDiscovery::default(), UdimDiscovery::probing()] {
            let found = document
                .resolve_udim_tiles(pattern, Some(model), &mut discovery)
                .unwrap();
            assert_eq!(numbers(&found), [1001, 1002], "{files:?}");
            assert_eq!(found.tiles[1].identifier, "models/tex/Bark.1002.png");
        }
    }
}
