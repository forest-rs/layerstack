// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dangling relationship targets, connections and collection paths
//! against OpenUSD 26.08.
//!
//! `fixtures/dangling_targets/scene.usda` has 60 placements, each with six
//! relationships and a connection into the terrain, the instanced plants
//! and the payloads, of which a known subset dangles: no prim (a missing
//! rock, a missing prim inside an instance), no property, inactive (the
//! retired rocks, their scope and its property), unloaded (inside
//! `/Terrain/Quarry`, whose payload the stage unloads, while the loaded
//! `/Terrain/Meadow` resolves), and instance proxies, which resolve. The
//! instanced asset's own relationships dangle once per instance, and one
//! names a path outside the asset, which composition drops and neither side
//! counts. The collections include and exclude paths of each kind, a
//! collection their prim lacks and a collection on a missing prim.
//!
//! `scripts/dangling_targets_oracle.py` records, with C++ calls
//! (`GetTargets`, `GetConnections`, `GetObjectAtPath`, `IsActive`,
//! `IsLoaded`, `UsdStagePopulationMask::Includes`, `HasAPI`), the counts
//! by relationship name, connection name and collection, by kind, with the
//! first three examples of each, for the stage and for the stage masked to
//! `/Placements` and `/Lights`. [`DanglingReport::compute`] must give the
//! same counts, kinds and examples.
//!
//! Spec: AOUSD Core §11.3 (population), §12.4 (target lists), §15
//! (collections).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{
    DanglingByName, DanglingCounts, DanglingKind, PayloadLoadRules, PopulationMask, Stage,
    StageOptions,
};
use layerstack_conformance::library_scene::{LibraryFormat, LibraryScene, LibraryShape};
use layerstack_conformance::usda_real::{LoadedStage, load_entry_usda};
use layerstack_conformance::workspace_root;
use layerstack_schemas::{CollectionList, CollectionTargetProblem, DanglingReport, Scene};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/dangling_targets/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    unloaded: String,
    mask: Vec<String>,
    examples: usize,
    stage: Record,
    masked: Record,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Record {
    relationships: BTreeMap<String, Group>,
    connections: BTreeMap<String, Group>,
    collections: Vec<Collection>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Group {
    properties: usize,
    targets: usize,
    dangling: BTreeMap<String, usize>,
    examples: Vec<[String; 3]>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Collection {
    owner: String,
    name: String,
    includes: usize,
    excludes: usize,
    dangling_includes: BTreeMap<String, usize>,
    dangling_excludes: BTreeMap<String, usize>,
    missing_collections: usize,
    examples: Vec<[String; 3]>,
}

fn kind_name(kind: DanglingKind) -> &'static str {
    match kind {
        DanglingKind::NoPrim => "no_prim",
        DanglingKind::NoProperty => "no_property",
        DanglingKind::Inactive => "inactive",
        DanglingKind::Unloaded => "unloaded",
        DanglingKind::Masked => "masked",
    }
}

fn counts(counts: DanglingCounts) -> BTreeMap<String, usize> {
    [
        DanglingKind::NoPrim,
        DanglingKind::NoProperty,
        DanglingKind::Inactive,
        DanglingKind::Unloaded,
        DanglingKind::Masked,
    ]
    .into_iter()
    .map(|kind| (kind_name(kind).to_owned(), counts.of(kind)))
    .collect()
}

/// The report in the oracle's shape.
fn record(loaded: &LoadedStage, report: &DanglingReport) -> Record {
    let store = &loaded.store;
    let (paths, tokens) = (&store.paths, &store.tokens);
    let groups = |groups: &[DanglingByName]| -> BTreeMap<String, Group> {
        groups
            .iter()
            .map(|group| {
                let examples = group
                    .examples
                    .iter()
                    .map(|example| {
                        [
                            example.source.display(paths, tokens),
                            example.target.display(paths, tokens),
                            kind_name(example.kind).to_owned(),
                        ]
                    })
                    .collect();
                (
                    tokens.resolve(group.name).to_owned(),
                    Group {
                        properties: group.properties,
                        targets: group.targets,
                        dangling: counts(group.dangling),
                        examples,
                    },
                )
            })
            .collect()
    };
    let collections = report
        .collections
        .iter()
        .map(|entry| Collection {
            owner: paths.display(entry.collection.owner, tokens),
            name: entry.collection.name.clone(),
            includes: entry.includes,
            excludes: entry.excludes,
            dangling_includes: counts(entry.dangling_includes),
            dangling_excludes: counts(entry.dangling_excludes),
            missing_collections: entry.missing_collections,
            examples: entry
                .examples
                .iter()
                .map(|example| {
                    [
                        match example.list {
                            CollectionList::Includes => "includes",
                            CollectionList::Excludes => "excludes",
                        }
                        .to_owned(),
                        example.target.display(paths, tokens),
                        match example.problem {
                            CollectionTargetProblem::Dangling(kind) => kind_name(kind),
                            CollectionTargetProblem::NoCollection => "no_collection",
                        }
                        .to_owned(),
                    ]
                })
                .collect(),
        })
        .collect();
    Record {
        relationships: groups(&report.targets.relationships),
        connections: groups(&report.targets.connections),
        collections,
    }
}

fn compose(oracle: &Oracle, masked: bool) -> (LoadedStage, Stage) {
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/dangling_targets/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let path = |loaded: &mut LoadedStage, text: &str| loaded.store.path(text);
    let mut load_rules = PayloadLoadRules::default();
    let unloaded = path(&mut loaded, &oracle.unloaded);
    load_rules.unload(loaded.store.paths.resolve(unloaded).clone());
    let mask = masked.then(|| PopulationMask {
        include: oracle
            .mask
            .iter()
            .map(|text| path(&mut loaded, text))
            .collect(),
    });
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&loaded.store.tokens))),
        load_rules,
        mask,
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    (loaded, stage)
}

#[test]
fn dangling_targets_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    assert_eq!(oracle.openusd_version, "26.8");
    for (masked, expected) in [(false, &oracle.stage), (true, &oracle.masked)] {
        let (loaded, stage) = compose(&oracle, masked);
        let scene = Scene::new(&stage, &loaded.store);
        let report = DanglingReport::compute(&scene, oracle.examples);
        let actual = record(&loaded, &report);
        for (name, group) in &expected.relationships {
            assert_eq!(
                actual.relationships.get(name),
                Some(group),
                "relationship {name} (masked: {masked})"
            );
        }
        for (name, group) in &expected.connections {
            assert_eq!(
                actual.connections.get(name),
                Some(group),
                "connection {name} (masked: {masked})"
            );
        }
        assert_eq!(actual.collections, expected.collections, "masked: {masked}");
        assert_eq!(actual, *expected, "masked: {masked}");
        assert!(!report.is_clean());
    }
}

/// Examples read as text, for an importer to quote.
#[test]
fn examples_display_as_paths() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    let (loaded, stage) = compose(&oracle, false);
    let report = DanglingReport::compute(&Scene::new(&stage, &loaded.store), 3);
    let (paths, tokens) = (&loaded.store.paths, &loaded.store.tokens);
    let shade = report
        .targets
        .relationships
        .iter()
        .find(|group| tokens.resolve(group.name) == "shade")
        .expect("the shade relationships");
    assert_eq!(
        shade.examples[0].display(paths, tokens),
        "/Placements/Spot_005.shade -> /Plants/Fern_05/Petal (NoPrim)"
    );
    let shadow = report
        .collections
        .iter()
        .find(|c| c.collection.name == "shadowLink")
        .expect("the sun's shadow link");
    let shown: Vec<String> = shadow
        .examples
        .iter()
        .map(|example| example.display(paths, tokens))
        .collect();
    assert!(
        shown.contains(&"includes /Gone.collection:lightLink (NoPrim)".to_owned())
            && shown.contains(&"includes /Lights/Fill.collection:glare (NoCollection)".to_owned()),
        "{shown:?}"
    );
}

/// The counts the fixture was written to have, so a regenerated oracle
/// that lost a category fails here too.
#[test]
fn fixture_covers_every_kind() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    let (loaded, stage) = compose(&oracle, false);
    let report = DanglingReport::compute(&Scene::new(&stage, &loaded.store), 0);
    let total = report.targets.total();
    assert_eq!(
        total,
        DanglingCounts {
            no_prim: 32,
            no_property: 20,
            inactive: 23,
            unloaded: 16,
            masked: 0,
        }
    );
    let relationships: usize = report
        .targets
        .relationships
        .iter()
        .map(|group| group.properties)
        .sum();
    assert!(relationships > 400, "{relationships} relationships");
    let missing: usize = report
        .collections
        .iter()
        .map(|entry| entry.missing_collections)
        .sum();
    assert_eq!(missing, 1);
    let (loaded, stage) = compose(&oracle, true);
    let report = DanglingReport::compute(&Scene::new(&stage, &loaded.store), 0);
    assert!(report.targets.total().masked > 0);
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
    base.join(format!("dangling-targets-{name}"))
}

/// The generated parts library's dangling knob injects exactly the paths it
/// counts, and leaves the rest of the library as the seed made it.
#[test]
fn library_dangling_knob() {
    let shape = LibraryShape {
        dangling: 30,
        ..LibraryShape::SMALL
    };
    let library = LibraryScene::generate(&shape);
    let clean = LibraryScene::generate(&LibraryShape::SMALL);
    let counts = library.counts;
    assert_eq!(counts.dangling_prims + counts.dangling_properties, 30);
    let rule_maps = counts.collections - counts.expression_collections;
    assert_eq!(counts.dangling_collection_paths, 2 * rule_maps);
    assert_eq!(
        (counts.relationships, counts.prims, counts.array_cells),
        (
            clean.counts.relationships,
            clean.counts.prims,
            clean.counts.array_cells
        )
    );
    assert_eq!(
        counts.include_targets,
        clean.counts.include_targets + rule_maps
    );

    let dir = scratch_dir("library");
    let _ = std::fs::remove_dir_all(&dir);
    let written = library.write(&dir, LibraryFormat::Usda).expect("writes");
    let mut loaded = load_entry_usda(&written.root);
    written.remove();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&loaded.store.tokens))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let report = DanglingReport::compute(&Scene::new(&stage, &loaded.store), 1);
    assert_eq!(
        report.targets.total(),
        DanglingCounts {
            no_prim: counts.dangling_prims + counts.dangling_collection_paths,
            no_property: counts.dangling_properties,
            ..DanglingCounts::default()
        }
    );
    let by_collection: usize = report.collections.iter().map(|c| c.total()).sum();
    assert_eq!(by_collection, counts.dangling_collection_paths);
}
