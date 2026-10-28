// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored targets, connections and collection paths read layer by layer,
//! against OpenUSD 26.08.
//!
//! `fixtures/layer_dangling_targets/scene.usda` sublayers two scopes, the
//! stronger deleting a dangling path of the weaker; references a planter
//! whose paths name its own namespace three times, once replacing its
//! targets with an explicit list and once instanced, with a payload the
//! stage unloads; authors targets in a selected and an unselected variant,
//! beneath an inactive prim and beneath the instance; and a collection that
//! includes and excludes paths of each kind, a missing collection and a
//! collection on a prim without it.
//!
//! `scripts/layer_dangling_targets_oracle.py` records, with C++ calls
//! (`ComputeExpandedPrimIndex`, `PcpNodeRef::CanContributeSpecs` and
//! `GetMapToRoot().MapSourceToTarget`, the layer stack's specs, then
//! `GetObjectAtPath`, `IsActive`, `IsLoaded`, `HasAPI` and `GetTargets`),
//! how every composed prim reads each authored path, which specs nothing
//! reads, and the specs of each layer. [`Stage::authored_targets`] and
//! [`LayerDanglingReport::compute`] must give the same.
//!
//! Spec: AOUSD Core §10.3.2 (arcs map namespace), §11.3 (population),
//! §12.4 (target lists), §15 (collections).

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{
    DanglingKind, DropReason, LayerId, PayloadLoadRules, Stage, StageOptions, TargetRead,
    UnreadReason,
};
use layerstack_conformance::library_scene::{LibraryFormat, LibraryScene, LibraryShape};
use layerstack_conformance::usda_real::{LoadedStage, load_entry_usda};
use layerstack_conformance::usdc::load_entry_usdc;
use layerstack_conformance::workspace_root;
use layerstack_schemas::{AuthoredTargetProblem, LayerDanglingReport, Scene};
use serde::Deserialize;

/// The composed targets of the relationship, or connections of the
/// attribute, at `path`: `None` when no opinion authors any, except for an
/// authored relationship, whose list is then empty.
fn target_list(
    stage: &Stage,
    store: &layerstack::InMemoryStore,
    path: layerstack::PropertyPath,
) -> Option<Vec<layerstack::TargetPath>> {
    let property = stage.view(store).property(path)?;
    let opinions = property.opinions();
    if let Some(relationship) = property.as_relationship() {
        return (!opinions.is_empty()).then(|| relationship.targets());
    }
    let attribute = property.as_attribute()?;
    opinions
        .iter()
        .any(|opinion| opinion.value().targets().is_some())
        .then(|| attribute.connections())
}

const ORACLE: &str = include_str!("../../fixtures/layer_dangling_targets/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    unloaded: String,
    layers: BTreeMap<String, usize>,
    /// `[layer, spec, position, authored, source, read, problem, composed]`.
    reads: Vec<Read>,
    /// `[layer, spec]`.
    unread: Vec<[String; 2]>,
}

type Read = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    bool,
);

fn kind_name(kind: DanglingKind) -> &'static str {
    match kind {
        DanglingKind::NoPrim => "no_prim",
        DanglingKind::NoProperty => "no_property",
        DanglingKind::Inactive => "inactive",
        DanglingKind::Unloaded => "unloaded",
        DanglingKind::Masked => "masked",
        kind => panic!("no name for {kind:?}"),
    }
}

fn compose(unloaded: &str) -> (LoadedStage, Stage) {
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/layer_dangling_targets/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let mut load_rules = PayloadLoadRules::default();
    let unloaded = loaded.store.path(unloaded);
    load_rules.unload(loaded.store.paths.resolve(unloaded).clone());
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&loaded.store.tokens))),
        load_rules,
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    (loaded, stage)
}

/// The file name of `layer`.
fn layer_name(loaded: &LoadedStage, layer: LayerId) -> String {
    let name = loaded.layer_names.get(&layer).expect("a fixture layer");
    std::path::Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .expect("a file name")
        .to_owned()
}

/// Every read in the oracle's shape, its problem taken from the report's
/// examples (all of them).
fn reads(loaded: &LoadedStage, stage: &Stage, report: &LayerDanglingReport) -> Vec<Read> {
    let store = &loaded.store;
    let (paths, tokens) = (&store.paths, &store.tokens);
    let walk = stage.authored_targets(store);
    let mut out = Vec::new();
    for layer in &walk.layers {
        let examples = &report.layer(layer.layer).expect("the same layers").examples;
        for target in &layer.targets {
            let entry = (
                layer_name(loaded, layer.layer),
                target.spec.display(paths, tokens),
                target.position.as_str().to_owned(),
                target.authored.display(paths, tokens),
                target.source.display(paths, tokens),
            );
            let example = examples.iter().find(|example| {
                (
                    &example.spec,
                    example.position,
                    example.authored,
                    example.source,
                ) == (
                    &target.spec,
                    target.position,
                    target.authored,
                    target.source,
                )
            });
            let (read, problem, composed) = match target.read {
                TargetRead::Replaced => ("replaced".to_owned(), None, false),
                TargetRead::Dropped(DropReason::OutsideArc(_)) => {
                    assert!(example.is_some(), "a dropped path is an example");
                    ("dropped".to_owned(), Some("dropped".to_owned()), false)
                }
                TargetRead::Dropped(reason) => panic!("the fixture has no {reason:?}"),
                TargetRead::Mapped(mapped) => {
                    let (problem, composed) = match example {
                        Some(example) => (
                            Some(
                                match example.problem {
                                    AuthoredTargetProblem::Dangling(kind) => kind_name(kind),
                                    AuthoredTargetProblem::NoCollection => "no_collection",
                                    AuthoredTargetProblem::Dropped(_) => "dropped",
                                }
                                .to_owned(),
                            ),
                            example.composed,
                        ),
                        None => (
                            None,
                            target_list(stage, store, target.source)
                                .is_some_and(|list| list.contains(&mapped)),
                        ),
                    };
                    (mapped.display(paths, tokens), problem, composed)
                }
            };
            out.push((
                entry.0, entry.1, entry.2, entry.3, entry.4, read, problem, composed,
            ));
        }
    }
    out
}

#[test]
fn layer_reads_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (loaded, stage) = compose(&oracle.unloaded);
    let scene = Scene::new(&stage, &loaded.store);
    let report = LayerDanglingReport::compute(&scene, usize::MAX);
    let mut actual = reads(&loaded, &stage, &report);
    actual.sort();
    let mut expected = oracle.reads.clone();
    expected.sort();
    assert_eq!(actual, expected);

    let walk = stage.authored_targets(&loaded.store);
    let layers: BTreeMap<String, usize> = walk
        .layers
        .iter()
        .map(|layer| (layer_name(&loaded, layer.layer), layer.specs))
        .collect();
    assert_eq!(layers, oracle.layers);
    let (paths, tokens) = (&loaded.store.paths, &loaded.store.tokens);
    let mut unread: Vec<[String; 2]> = walk
        .layers
        .iter()
        .flat_map(|layer| &layer.unread)
        .map(|spec| {
            [
                layer_name(&loaded, spec.layer),
                spec.spec.display(paths, tokens),
            ]
        })
        .collect();
    unread.sort();
    assert_eq!(unread, oracle.unread);
}

/// Why each unread spec of the fixture is unread, and that every one is
/// counted where it is authored.
#[test]
fn unread_specs_say_why() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    let (loaded, stage) = compose(&oracle.unloaded);
    let report = LayerDanglingReport::compute(&Scene::new(&stage, &loaded.store), 8);
    let (paths, tokens) = (&loaded.store.paths, &loaded.store.tokens);
    let mut reasons: Vec<(String, String, UnreadReason)> = report
        .layers
        .iter()
        .flat_map(|layer| &layer.unread)
        .map(|spec| {
            (
                layer_name(&loaded, spec.layer),
                spec.spec.display(paths, tokens),
                spec.reason,
            )
        })
        .collect();
    reasons.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let expected = [
        ("planter.usda", "/Unused.water", UnreadReason::NotReached),
        (
            "scene.usda",
            "/Retired/Rock.shade",
            UnreadReason::PrimNotComposed(DanglingKind::Inactive),
        ),
        (
            "scene.usda",
            "/Spot{look=dim}.shade",
            UnreadReason::UnselectedVariant,
        ),
        (
            "scene.usda",
            "/World/Bed_C/Soil.feeds",
            UnreadReason::BeneathInstance,
        ),
    ];
    let expected: Vec<(String, String, UnreadReason)> = expected
        .into_iter()
        .map(|(layer, spec, reason)| (layer.to_owned(), spec.to_owned(), reason))
        .collect();
    assert_eq!(reasons, expected);
    let unread: usize = report.layers.iter().map(|layer| layer.unread_specs).sum();
    assert_eq!(unread, oracle.unread.len());
}

/// The report's counts and examples per layer, as an importer quotes them.
#[test]
fn layer_report_counts_and_displays() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    let (loaded, stage) = compose(&oracle.unloaded);
    let report = LayerDanglingReport::compute(&Scene::new(&stage, &loaded.store), 8);
    assert!(!report.is_clean());
    let by_name = |name: &str| {
        report
            .layers
            .iter()
            .find(|layer| layer_name(&loaded, layer.layer) == name)
            .expect("the layer")
    };
    let shown = |name: &str| -> Vec<String> {
        by_name(name)
            .examples
            .iter()
            .map(|example| example.display(&loaded.store).to_string())
            .collect()
    };
    let planter = by_name("planter.usda");
    assert_eq!(
        (
            planter.targets,
            planter.dangling.no_prim,
            planter.dropped,
            planter.replaced
        ),
        (9, 5, 2, 3)
    );
    assert_eq!(
        shown("planter.usda"),
        [
            "/Planter.water (as /World/Bed_A.water) explicit /Planter/Gone -> /World/Bed_A/Gone (NoPrim)",
            "/Planter.water (as /World/Bed_A.water) explicit /Outside (OutsideArc(References))",
            "/Planter.water (as /World/Bed_C.water) explicit /Planter/Gone -> /World/Bed_C/Gone (NoPrim)",
            "/Planter.water (as /World/Bed_C.water) explicit /Outside (OutsideArc(References))",
            "/Planter/Soil.feeds (as /World/Bed_A/Soil.feeds) explicit /Planter/Roots -> /World/Bed_A/Roots (NoPrim)",
            "/Planter/Soil.feeds (as /World/Bed_B/Soil.feeds) explicit /Planter/Roots -> /World/Bed_B/Roots (NoPrim)",
            "/Planter/Soil.feeds (as /World/Bed_C/Soil.feeds) explicit /Planter/Roots -> /World/Bed_C/Roots (NoPrim)",
        ]
    );
    let weak = by_name("scope_weak.usda");
    assert_eq!(
        shown("scope_weak.usda"),
        [
            "/World/Bed_A.shade appended /World/Lost (NoPrim)",
            "/Path_1.shade appended /World/Lost (NoPrim)",
            "/Path_1.shade appended /World/Shed/Soil (Unloaded)",
            "/Lights.collection:lit:includes appended /World/Gone_weak (NoPrim)",
        ]
    );
    // The stronger scope deletes `/Path_1`'s `/World/Lost` and
    // `/World/Gone_weak`: the composed report counts neither.
    let composed: Vec<bool> = weak.examples.iter().map(|e| e.composed).collect();
    assert_eq!(composed, [true, false, true, false]);
    assert_eq!(
        shown("scene.usda"),
        [
            "/World/Bed_B.water explicit /World/Bed_B/Dry (NoPrim)",
            "/Lights.collection:lit:excludes explicit /World/Bed_A/Soil/Gone (NoPrim)",
            "/Lights.collection:lit:includes prepended /World/Gone_lit (NoPrim)",
            "/Lights.collection:lit:includes prepended /Lights.collection:glare (NoCollection)",
            "/Lights.collection:lit:includes prepended /Spot.collection:lit (NoCollection)",
            "/Spot{look=bright}.shade explicit /World/Gone_bright (NoPrim)",
        ]
    );
    let scene_layer = by_name("scene.usda");
    assert_eq!(
        (scene_layer.missing_collections, scene_layer.unread_specs),
        (2, 3)
    );
    assert_eq!(
        shown("scope_strong.usda"),
        ["/Path_1.shade prepended /World/Bed_B/Soil.wetness (NoProperty)"]
    );
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
    base.join(format!("layer-dangling-targets-{name}"))
}

/// Each layer's report, as text, by the layer's file stem.
fn library_report(mut loaded: LoadedStage) -> BTreeMap<String, (usize, usize, usize, Vec<String>)> {
    let store = &mut loaded.store;
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&store.tokens))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(store, loaded.root_layer, options);
    let store = &loaded.store;
    let report = LayerDanglingReport::compute(&Scene::new(&stage, store), usize::MAX);
    report
        .layers
        .iter()
        .map(|layer| {
            let name = loaded
                .layer_names
                .get(&layer.layer)
                .expect("a library layer");
            let stem = std::path::Path::new(name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("a file stem")
                .to_owned();
            let shown = layer
                .examples
                .iter()
                .map(|example| example.display(store).to_string())
                .collect();
            (stem, (layer.specs, layer.targets, layer.total(), shown))
        })
        .collect()
}

/// The parts library, sublayered and list-edited, reports the same from
/// its USDA and its USDC files: every injected path, each in the layer
/// that authors it.
#[test]
fn library_usda_and_usdc_agree() {
    let library = LibraryScene::generate(&LibraryShape {
        dangling: 30,
        ..LibraryShape::SMALL
    });
    let dir = scratch_dir("library");
    let _ = std::fs::remove_dir_all(&dir);
    let usda = library
        .write(&dir.join("usda"), LibraryFormat::Usda)
        .expect("writes");
    let usdc = library
        .write(&dir.join("usdc"), LibraryFormat::Usdc)
        .expect("writes");
    let from_usda = load_entry_usda(&usda.root);
    let from_usdc = load_entry_usdc(&usdc.root);
    usda.remove();
    usdc.remove();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(from_usda.invalid.is_empty(), "{:?}", from_usda.invalid);
    let text = library_report(from_usda);
    assert_eq!(text, library_report(from_usdc));
    let counts = library.counts;
    let found: usize = text.values().map(|(_, _, total, _)| *total).sum();
    assert_eq!(
        found,
        counts.dangling_prims + counts.dangling_properties + counts.dangling_collection_paths,
        "{text:#?}"
    );
}
