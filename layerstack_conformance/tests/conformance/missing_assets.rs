// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A scene whose dependencies are missing opens as OpenUSD 26.08 opens it.
//!
//! `scripts/missing_assets_oracle.py` writes the layers of
//! `fixtures/missing_assets` and records what `UsdStage::Open` composes
//! from them (see the script for every case). Here each case opens the
//! same layers through `StageDocument`, with both import policies, and must
//! agree with the record:
//!
//! - a missing sublayer, reference, payload, reference inside the selected
//!   variant, or reference inside a referenced layer does not fail the
//!   open: the arc contributes nothing, valid siblings still load, and the
//!   stage reports `UnresolvedAsset` / `UnresolvedSublayer` where OpenUSD
//!   reports `PcpErrorInvalidAssetPath` / `PcpErrorInvalidSublayerPath`, for
//!   the same prim, arc, asset and introducing layer and spec; each miss is
//!   kept in the load report as an `AssetFailure` that `is_missing`;
//! - an unloaded payload reports nothing;
//! - reloading after the assets appear composes them, as `UsdStage::Reload`
//!   does;
//! - a missing root layer fails the open.
//!
//! Two cases differ on purpose (`DELIBERATE`). A dependency that exists but
//! does not parse opens in OpenUSD with an invalid asset error; strict
//! import rejects it, and recovering import keeps what parses. A payload
//! loaded after its asset appears composes it in OpenUSD; layerstack reads
//! a payload's asset when it reads the layer authoring it, so the asset is
//! recorded missing until a reload.
//!
//! Spec: AOUSD Core §9 (asset resolution), §10.3.1 (sublayers), §10.3.2.1
//! (references), §10.3.2.2 (payloads), §10.3.2.5 (variants), §10.6
//! (composition errors).

use std::collections::BTreeMap;

use layerstack::{ArcKind, CompositionError, Path, PayloadLoadRules, StageOptions, Value};
use layerstack_conformance::workspace_root;
use layerstack_io::{
    ImportPolicy, IoError, IoErrorKind, LoadOptions, ReloadPolicy, StageDocument, Storage,
};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/missing_assets/oracle.json");

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

/// The fixture's layers, by file name: every asset path in them is a file
/// beside its layer.
#[derive(Clone, Debug, Default)]
struct Fixture {
    files: BTreeMap<String, Vec<u8>>,
}

impl Fixture {
    fn read() -> Self {
        let dir = workspace_root().join("layerstack_conformance/fixtures/missing_assets");
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "usda") {
                let name = path.file_name().unwrap().to_str().unwrap().to_owned();
                files.insert(name, std::fs::read(&path).unwrap());
            }
        }
        Self { files }
    }

    /// Writes the assets the oracle's later cases make appear.
    fn add_appearing(&mut self) {
        for (name, text) in oracle()["appearing"].as_object().unwrap() {
            let text = text.as_str().unwrap().as_bytes().to_vec();
            self.files.insert(name.clone(), text);
        }
    }
}

impl Storage for Fixture {
    fn identify(&self, asset: &str, _anchor: Option<&str>) -> Result<String, IoError> {
        Ok(asset.trim_start_matches("./").to_owned())
    }

    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.files
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, "no such file"))
    }

    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.files.insert(identifier.to_owned(), bytes.to_vec());
        Ok(())
    }
}

fn options(load_none: bool, threads: usize) -> StageOptions {
    StageOptions {
        with_provenance: true,
        threads,
        load_rules: if load_none {
            PayloadLoadRules::load_none()
        } else {
            PayloadLoadRules::default()
        },
        ..StageOptions::default()
    }
}

fn open(
    storage: Fixture,
    root: &str,
    policy: ImportPolicy,
    load_none: bool,
) -> Result<StageDocument<Fixture>, IoError> {
    open_on(storage, root, policy, load_none, 1)
}

/// Opens `root`, reading its layers on up to `threads` threads.
fn open_on(
    storage: Fixture,
    root: &str,
    policy: ImportPolicy,
    load_none: bool,
    threads: usize,
) -> Result<StageDocument<Fixture>, IoError> {
    let loading = LoadOptions {
        import_policy: policy,
        ..LoadOptions::default()
    };
    StageDocument::open_with(storage, root, options(load_none, threads), loading)
}

/// Errors in the oracle's order: by their JSON text.
fn sorted(mut errors: Vec<Json>) -> Vec<Json> {
    errors.sort_by_key(Json::to_string);
    errors
}

fn file_name(asset: &str) -> String {
    asset.rsplit('/').next().unwrap().to_owned()
}

/// What the stage composes, as the oracle records it.
fn record(doc: &StageDocument<Fixture>) -> Json {
    let store = doc.store();
    let stage = doc.stage().stage();
    let identifier = |layer| file_name(doc.identifier(layer).expect("layer identifier"));
    let errors = stage
        .composition_errors()
        .iter()
        .map(|error| match error {
            CompositionError::UnresolvedAsset(error) => json!({
                "type": "InvalidAssetPath",
                "prim": store.paths().display(error.prim, store.tokens()),
                "arc": match error.arc {
                    ArcKind::References => "reference",
                    ArcKind::Payloads => "payload",
                    other => panic!("arc {other:?}"),
                },
                "asset": file_name(&error.asset),
                "introducing_layer": identifier(error.introducing_layer),
                "introducing_spec": error.introducing_spec.display(store.paths(), store.tokens()),
            }),
            CompositionError::UnresolvedSublayer(error) => json!({
                "type": "InvalidSublayerPath",
                "asset": file_name(&error.asset),
                "layer": identifier(error.layer),
            }),
            other => panic!("unexpected composition error {other:?}"),
        })
        .collect();
    let root = store.paths().lookup(&Path::root()).unwrap();
    let mut prims = serde_json::Map::new();
    for prim in stage.traverse_all(root).filter(|prim| *prim != root) {
        let handle = stage.prim(prim, store).expect("traversed");
        let stack: Vec<Json> = handle
            .sources()
            .iter()
            .map(|key| {
                json!([
                    identifier(key.key.layer_id),
                    key.key.spec_path.display(store.paths(), store.tokens())
                ])
            })
            .collect();
        let mut attributes = serde_json::Map::new();
        for property in handle.properties() {
            let value = property
                .as_attribute()
                .and_then(|attribute| attribute.authored_value(layerstack::Time::Default))
                .map(|resolved| resolved.value);
            let value = match value {
                Some(Value::Int(value)) => json!(value),
                None => Json::Null,
                other => panic!("unexpected value {other:?}"),
            };
            attributes.insert(property.name().to_owned(), value);
        }
        prims.insert(
            store.paths().display(prim, store.tokens()),
            json!({"stack": stack, "attributes": attributes}),
        );
    }
    json!({"opened": true, "errors": sorted(errors), "prims": prims})
}

/// The case as the oracle records it, its errors in the same order.
fn expected(case: &str) -> Json {
    let mut case = oracle()["cases"][case].clone();
    if let Some(errors) = case.get_mut("errors") {
        *errors = Json::Array(sorted(errors.as_array().unwrap().clone()));
    }
    case
}

const POLICIES: [ImportPolicy; 2] = [ImportPolicy::Strict, ImportPolicy::AllowRecovery];

/// Opening reads the layers on one thread, or on several
/// (`StageOptions::threads`), which must decide the same.
const THREADS: [usize; 2] = [1, 4];

#[test]
fn missing_dependencies_open_and_report_composition_errors() {
    for (policy, threads) in POLICIES.into_iter().flat_map(|p| THREADS.map(|t| (p, t))) {
        let doc = open_on(Fixture::read(), "scene.usda", policy, false, threads).unwrap();
        assert_eq!(record(&doc), expected("load_all"), "{policy:?} {threads}");
        // Each miss is evidence in the load report, named by its authored
        // asset path and the layer that authors it.
        let report = doc.load_report();
        assert!(report.has_errors());
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        let mut missing: Vec<(String, String)> = report
            .asset_failures
            .iter()
            .map(|failure| {
                assert!(failure.is_missing(), "{failure}");
                let anchor = failure.anchor.expect("authoring layer");
                (
                    failure.asset.clone(),
                    file_name(doc.identifier(anchor).unwrap()),
                )
            })
            .collect();
        missing.sort();
        missing.dedup();
        assert_eq!(
            missing,
            [
                ("./absent_layer.usda", "scene.usda"),
                ("./absent_moss.usda", "scene.usda"),
                ("./absent_rock.usda", "scene.usda"),
                ("./absent_seed.usda", "scene.usda"),
                ("./absent_soil.usda", "bed.usda"),
            ]
            .map(|(asset, layer)| (asset.to_owned(), layer.to_owned()))
        );
        let failure = &report.asset_failures[0];
        assert_eq!(
            failure.to_string(),
            format!(
                "could not open asset @{}@: {}: no such file",
                failure.asset,
                file_name(&failure.asset)
            )
        );
    }
}

#[test]
fn an_unloaded_missing_payload_reports_nothing() {
    for policy in POLICIES {
        let doc = open(Fixture::read(), "scene.usda", policy, true).unwrap();
        assert_eq!(record(&doc), expected("load_none"), "{policy:?}");
    }
}

#[test]
fn reloading_composes_the_assets_that_appeared() {
    for policy in POLICIES {
        let mut doc = open(Fixture::read(), "scene.usda", policy, false).unwrap();
        doc.storage_mut().add_appearing();
        let report = doc.reload(ReloadPolicy::PreserveDirty).unwrap();
        assert!(
            report.asset_failures.is_empty(),
            "{:?}",
            report.asset_failures
        );
        assert!(doc.stage().stage().asset_requests().is_empty());
        assert_eq!(record(&doc), expected("reloaded"), "{policy:?}");
    }
}

#[test]
fn a_payload_loaded_after_its_asset_appears_needs_a_reload() {
    // DELIBERATE: OpenUSD opens the payload's asset when the payload is
    // loaded; layerstack read it, and found it missing, with the layer
    // authoring the payload.
    let oracle = expected("loaded_later");
    for policy in POLICIES {
        let mut doc = open(Fixture::read(), "scene.usda", policy, true).unwrap();
        doc.storage_mut().add_appearing();
        let (store, stage) = doc.parts_mut();
        stage.set_load_rules(PayloadLoadRules::default());
        stage.synchronize(store);
        let report = doc.load_requested_assets().unwrap();
        assert!(report.layers.is_empty());
        let mut loaded = record(&doc);
        let seed = &loaded["prims"]["/Seed"];
        assert_eq!(seed["attributes"], json!({"count": 2}), "{policy:?}");
        let errors = loaded["errors"].as_array_mut().unwrap();
        let payload = errors
            .iter()
            .position(|error| error["arc"] == "payload")
            .expect("missing payload");
        assert_eq!(errors.remove(payload)["prim"], "/Seed");
        assert_eq!(loaded["errors"], oracle["errors"], "{policy:?}");

        // A reload finds it, with the other assets.
        doc.reload(ReloadPolicy::PreserveDirty).unwrap();
        assert_eq!(record(&doc), expected("reloaded"), "{policy:?}");
    }
}

#[test]
fn a_malformed_dependency_is_rejected_or_recovered() {
    // DELIBERATE: OpenUSD opens `broken.usda` and reports the layer that
    // does not parse as an invalid asset path.
    let oracle = expected("broken");
    assert_eq!(oracle["opened"], true);
    for threads in THREADS {
        let error = open_on(
            Fixture::read(),
            "broken.usda",
            ImportPolicy::Strict,
            false,
            threads,
        )
        .unwrap_err();
        assert_eq!(error.kind, IoErrorKind::Rejected, "{error}");
    }

    let doc = open(
        Fixture::read(),
        "broken.usda",
        ImportPolicy::AllowRecovery,
        false,
    )
    .unwrap();
    let recovered = record(&doc);
    // The valid sibling loads, as in OpenUSD; what parses of the malformed
    // layer contributes too, and its parse errors are in the load report
    // instead of a composition error.
    assert_eq!(
        recovered["prims"]["/Cairn"]["attributes"],
        json!({"leaves": 5, "weight": null})
    );
    assert_eq!(
        recovered["prims"]["/Cairn"]["stack"],
        json!([
            ["broken.usda", "/Cairn"],
            ["malformed.usda", "/Stone"],
            ["plant.usda", "/Plant"]
        ])
    );
    assert_eq!(recovered["errors"], json!([]));
    let report = doc.load_report();
    assert!(report.asset_failures.is_empty());
    assert!(!report.diagnostics.is_empty());
    assert!(report.diagnostics.iter().all(|diagnostic| {
        &*diagnostic.member == "malformed.usda" && diagnostic.diagnostic.is_error()
    }));
}

#[test]
fn a_missing_root_layer_fails_to_open() {
    assert_eq!(expected("missing_root"), json!({"opened": false}));
    for policy in POLICIES {
        let error = open(Fixture::read(), "missing.usda", policy, false).unwrap_err();
        assert_eq!(error.kind, IoErrorKind::NotFound, "{policy:?}");
    }
}
