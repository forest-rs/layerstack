// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asset path values authored as variable expressions read as OpenUSD
//! 26.11 reads them.
//!
//! `scripts/asset_expressions_oracle.py` writes `fixtures/asset_expressions`:
//! `scene.usda` sets `expressionVariables` and sublayers `sub/sub.usda`;
//! `/Local`, `/Sub` and `/Root` (which references `ref/ref.usda</Ref>`)
//! author the same asset defaults, time samples, arrays and dictionary
//! entries as expressions of every kind, and `/Clipped` reads expression
//! samples from a value clip. It records what OpenUSD reads
//! for each (`authoredPath`, `evaluatedPath`, `resolvedPath`), the
//! warnings each read issues, `UsdStage::Flatten`'s text, and
//! `UsdUtils.FlattenLayerStack`'s.
//!
//! Here the stage must read each asset path as `SdfAssetPath::GetAssetPath`
//! returns it: the evaluated path, or the authored text when the
//! expression evaluates to no path or fails. `AssetReference::anchor` must
//! anchor each path OpenUSD resolves to the file OpenUSD resolves, and
//! refuse each expression that evaluated to no path. Each expression
//! OpenUSD warns about must be a `VariableExpressionError` composition
//! error (for a clip's value, a `ClipIssueKind::AssetExpression`) of the
//! layer and with the message OpenUSD reports. The stage's
//! flattened layer must save as OpenUSD's `UsdStage::Flatten` text, and
//! `flatten_layer_stack`'s as `UsdUtils.FlattenLayerStack`'s, reporting
//! the expressions OpenUSD warns about. The cases in `DIFFERING` are the
//! ones layerstack does not yet do as OpenUSD does.
//!
//! Spec: AOUSD Core §6.2 (the `asset` type), §9 (asset resolution).
//! OpenUSD: `_AssetPathToStage` in `pxr/usd/usd/stage.cpp`,
//! `SdfResolveAssetPaths` and `SdfAnchorAssetPaths` in
//! `pxr/usd/sdf/assetPath.cpp`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use layerstack::edit::{EditTarget, Transaction};
use layerstack::stage::flatten::{AssetPaths, FlattenRequirements};
use layerstack::value_clips::ClipIssueKind;
use layerstack::{
    AssetResolveError, AssetResolver, CompositionError, ExpressionContext, FieldValue,
    InMemoryStore, LayerId, LiveStage, PathInterner, PropertyPath, ResolvedAsset, ResolvedValue,
    Stage, StageOptions, Time as LsTime, TokenInterner, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use layerstack_schemas::{
    Scene, Time,
    assets::{AssetAnchorError, AssetReference},
};
use serde_json::{Map, Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/asset_expressions/oracle.json");

/// The checks where layerstack does not yet do what OpenUSD does. Each
/// must still differ, so that the list stays current.
///
/// `flatten`: OpenUSD's flatten writes an evaluated path only where
/// anchoring changes it (`SdfAnchorAssetPaths` keeps the `SdfAssetPath`
/// otherwise, and the text format writes its authored path), so an
/// expression evaluating to an absolute path, or to a search path that
/// names no file (the clip sample `` `"${UNSET}"` ``), is written as
/// authored; and
/// it stops anchoring an array at its first empty element (`return` in
/// `_AnchorOrResolveAssetPaths`), so `[@@, @`"./${NAME}.png"`@]` is
/// written as authored. Layerstack's composed values hold the evaluated
/// paths, which it anchors and writes.
const DIFFERING: &[&str] = &["flatten"];

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn fixtures() -> PathBuf {
    workspace_root().join("layerstack_conformance/fixtures/asset_expressions")
}

fn load() -> (LoadedStage, Stage) {
    let mut loaded = load_entry_usda(&fixtures().join("scene.usda"));
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    (loaded, stage)
}

/// The path OpenUSD passes to its resolver for one recorded asset path
/// (`SdfAssetPath::GetAssetPath`): the evaluated path, or the authored one
/// when that is empty; recorded values as the stage reads them.
fn asset_paths(recorded: &Json) -> Json {
    match recorded {
        Json::Null => Json::Null,
        Json::Array(items) => Json::Array(items.iter().map(asset_paths).collect()),
        Json::Object(map) if map.contains_key("dictionary") => {
            let entries: Map<String, Json> = map["dictionary"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, item)| (key.clone(), asset_paths(item)))
                .collect();
            json!({ "dictionary": entries })
        }
        Json::Object(map) => {
            let evaluated = map["evaluated"].as_str().unwrap();
            if evaluated.is_empty() {
                map["authored"].clone()
            } else {
                json!(evaluated)
            }
        }
        other => panic!("no asset path for {other}"),
    }
}

/// `value` as [`asset_paths`] writes the oracle's.
fn encode(value: &Value) -> Json {
    match value {
        Value::Asset(path) => json!(&**path),
        Value::Dictionary(entries) => dictionary(entries),
        other => match other.array_ref() {
            Some(items) => Json::Array(items.iter().map(|item| encode(&item)).collect()),
            None => panic!("no JSON rendering for {other:?}"),
        },
    }
}

fn dictionary(entries: &[(Arc<str>, FieldValue)]) -> Json {
    let entries: Map<String, Json> = entries
        .iter()
        .map(|(key, entry)| {
            let value = entry.as_value().expect("a plain entry");
            (key.to_string(), encode(value))
        })
        .collect();
    json!({ "dictionary": entries })
}

fn resolved_dictionary(value: Option<ResolvedValue>) -> Json {
    match value {
        Some(ResolvedValue::Dictionary(entries)) => dictionary(&entries),
        None => json!({ "dictionary": {} }),
        Some(other) => panic!("not a dictionary: {other:?}"),
    }
}

#[test]
fn the_stage_reads_the_paths_openusd_evaluates() {
    let oracle = oracle();
    let (loaded, stage) = load();
    let store = &loaded.store;
    let mut checked = 0;
    for (path, record) in oracle["read"].as_object().unwrap() {
        let prim = stage.prim(store.path(path), store).expect("a prim");
        for key in ["customData", "assetInfo"] {
            assert_eq!(
                resolved_dictionary(prim.metadata(key).map(|r| r.value)),
                asset_paths(&record[key]["value"]),
                "{path} {key}"
            );
        }
        let attributes = record["attributes"].as_object().unwrap();
        for property in prim.properties() {
            let Some(attribute) = property.as_attribute() else {
                continue;
            };
            let name = property.name().to_string();
            let expected = &attributes[&name];
            assert_eq!(
                attribute
                    .get(LsTime::Default)
                    .map_or(Json::Null, |r| encode(&r.value)),
                asset_paths(&expected["default"]["value"]),
                "{path}.{name} default"
            );
            for sample in expected["samples"].as_array().unwrap() {
                let time = sample[0].as_f64().unwrap();
                assert_eq!(
                    attribute
                        .get(LsTime::at(time))
                        .map_or(Json::Null, |r| encode(&r.value)),
                    asset_paths(&sample[1]["value"]),
                    "{path}.{name} at {time}"
                );
            }
            assert_eq!(
                resolved_dictionary(property.metadata("customData").map(|r| r.value)),
                asset_paths(&expected["customData"]["value"]),
                "{path}.{name} customData"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 47, "every attribute is read");
}

/// Knows where each layer of the fixture is, and anchors a search path
/// only when the asset exists beside its layer, as OpenUSD's default
/// resolver does (`ArDefaultResolver::_CreateIdentifier`).
struct Locations {
    layers: HashMap<LayerId, String>,
}

impl AssetResolver for Locations {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.layers.get(&id).map(String::as_str)
    }

    fn anchor_asset_path(&self, asset_path: &str, anchor: LayerId) -> Option<String> {
        use layerstack::asset::{anchor_asset_path, normalize_asset_path};
        let location = self.resolved_path(anchor)?;
        let anchored = anchor_asset_path(asset_path, location)?;
        let search = anchored == normalize_asset_path(asset_path)
            && !asset_path.starts_with('/')
            && !asset_path.contains(':');
        if search
            && let Some(beside) = anchor_asset_path(&format!("./{asset_path}"), location)
            && Path::new(&beside).exists()
        {
            return Some(beside);
        }
        Some(anchored)
    }
}

fn locations(loaded: &LoadedStage) -> Locations {
    let directory = fixtures();
    Locations {
        layers: loaded
            .layer_names
            .iter()
            .map(|(&id, name)| (id, directory.join(name).to_string_lossy().into_owned()))
            .collect(),
    }
}

/// Anchors the asset `reference` names, as `<dir>/...` for a file of the
/// fixture.
fn anchored(reference: &AssetReference, resolver: &Locations) -> Result<String, AssetAnchorError> {
    let directory = format!("{}/", fixtures().to_string_lossy());
    reference
        .anchor(resolver)
        .map(|asset| asset.identifier.replace(&directory, "<dir>/"))
}

/// Checks one recorded scalar asset path against what `AssetReference`
/// anchors: the file OpenUSD resolves, or, for an expression that
/// evaluated to no path, an explicit refusal.
fn check_anchor(
    recorded: &Json,
    reference: Option<AssetReference>,
    resolver: &Locations,
    at: &str,
) {
    let reference = reference.unwrap_or_else(|| panic!("{at}: an asset"));
    let resolved = recorded["resolved"].as_str().unwrap();
    let authored = recorded["authored"].as_str().unwrap();
    let evaluated = recorded["evaluated"].as_str().unwrap();
    if !resolved.is_empty() {
        assert_eq!(anchored(&reference, resolver), Ok(resolved.into()), "{at}");
    } else if authored.starts_with('`') && evaluated.is_empty() {
        assert_eq!(
            anchored(&reference, resolver),
            Err(AssetAnchorError::UnevaluatedExpression),
            "{at}"
        );
    }
}

#[test]
fn asset_references_anchor_the_files_openusd_resolves() {
    let oracle = oracle();
    let (loaded, stage) = load();
    let store = &loaded.store;
    let resolver = locations(&loaded);
    let scene = Scene::new(&stage, store);
    for (path, record) in oracle["read"].as_object().unwrap() {
        for (name, expected) in record["attributes"].as_object().unwrap() {
            let property = store.property_path(&format!("{path}.{name}"));
            let property: PropertyPath = property;
            let default = &expected["default"]["value"];
            if default.is_object() {
                let reference = AssetReference::read(&scene, property, Time::Default)
                    .expect("an asset attribute");
                check_anchor(default, reference, &resolver, &format!("{path}.{name}"));
            }
            for sample in expected["samples"].as_array().unwrap() {
                let time = sample[0].as_f64().unwrap();
                if sample[1]["value"].is_object() {
                    let reference = AssetReference::read(&scene, property, Time::at(time))
                        .expect("an asset attribute");
                    check_anchor(
                        &sample[1]["value"],
                        reference,
                        &resolver,
                        &format!("{path}.{name} at {time}"),
                    );
                }
            }
        }
    }
}

#[test]
fn expressions_openusd_warns_about_are_composition_errors() {
    let oracle = oracle();
    let (loaded, stage) = load();
    let mut warned = BTreeSet::new();
    for record in oracle["read"].as_object().unwrap().values() {
        let mut reads: Vec<&Json> = vec![&record["customData"], &record["assetInfo"]];
        for attribute in record["attributes"].as_object().unwrap().values() {
            reads.push(&attribute["default"]);
            reads.push(&attribute["customData"]);
            reads.extend(
                attribute["samples"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| &s[1]),
            );
        }
        for read in reads {
            for warning in read["warnings"].as_array().unwrap() {
                // `MESSAGE (resolving asset path at @LAYER@<SPEC> on stage @...@)`
                let warning = warning.as_str().unwrap();
                let (message, context) = warning.split_once(" (resolving asset path at @").unwrap();
                let layer = context.split('@').next().unwrap();
                warned.insert((layer.to_string(), message.to_string()));
            }
        }
    }
    assert!(!warned.is_empty(), "the oracle records warnings");
    let mut reported: BTreeSet<(String, String)> = stage
        .composition_errors()
        .iter()
        .filter_map(|error| match error {
            CompositionError::VariableExpressionError(error)
                if error.context == ExpressionContext::AssetValue =>
            {
                Some((
                    loaded.layer_names[&error.layer].clone(),
                    error.error.clone(),
                ))
            }
            _ => None,
        })
        .collect();
    // A clip value's expression is evaluated as the clips are prepared.
    reported.extend(
        stage
            .clip_issues()
            .iter()
            .filter_map(|issue| match &issue.kind {
                ClipIssueKind::AssetExpression { layer, error, .. } => {
                    Some((loaded.layer_names[layer].clone(), error.clone()))
                }
                _ => None,
            }),
    );
    assert_eq!(reported, warned);
    // Nothing else is an error: the fixture composes cleanly in OpenUSD.
    assert!(
        stage.composition_errors().iter().all(|error| matches!(
            error,
            CompositionError::VariableExpressionError(error)
                if error.context == ExpressionContext::AssetValue
        )),
        "{:?}",
        stage.composition_errors()
    );
}

fn flatten_text(loaded: &mut LoadedStage, stage: &Stage) -> String {
    let resolver = locations(loaded);
    let requirements = FlattenRequirements {
        asset_paths: AssetPaths::Anchored(&resolver),
        ..FlattenRequirements::default()
    };
    let flat = stage
        .flatten(
            &mut loaded.store,
            loaded.root_layer,
            LayerId(999_999),
            &requirements,
        )
        .expect("flattens");
    let store: &InMemoryStore = &loaded.store;
    let text =
        layerstack_usda::save_usda(&flat.layer, store.tokens(), store.paths()).expect("saves");
    text.replace(&format!("{}/", fixtures().to_string_lossy()), "<dir>/")
}

#[test]
fn flattening_writes_the_paths_openusd_writes() {
    let oracle = oracle();
    let differs = DIFFERING.contains(&"flatten");
    // A differing check panics, which aborts where panics cannot unwind
    // (WASI), so it runs only where its panic can be caught.
    if differs && cfg!(panic = "abort") {
        return;
    }
    let result = std::panic::catch_unwind(|| {
        let (mut loaded, stage) = load();
        assert_eq!(
            flatten_text(&mut loaded, &stage),
            oracle["flatten"].as_str().unwrap()
        );
    });
    assert_eq!(
        result.is_ok(),
        !differs,
        "flatten {}",
        if differs {
            "agrees, so it leaves `DIFFERING`"
        } else {
            "differs from OpenUSD's"
        }
    );
}

/// The layer [`layerstack::flatten_layer_stack`] writes for the fixture,
/// anchoring each asset path as `UsdFlattenLayerStackResolveAssetPath`
/// does with OpenUSD's default resolver: anchored to its layer, unless
/// anchoring leaves it as it is.
fn flatten_layer_stack_text(loaded: &LoadedStage) -> (String, Vec<(String, String)>) {
    let resolver = locations(loaded);
    let flat = layerstack::flatten_layer_stack(
        &loaded.store,
        loaded.root_layer.into(),
        LayerId(999_999),
        &mut |layer, path| {
            let anchored = resolver.anchor_asset_path(path, layer)?;
            Some(
                if anchored == layerstack::asset::normalize_asset_path(path) {
                    path.to_string()
                } else {
                    anchored
                },
            )
        },
    )
    .expect("flattens");
    let store: &InMemoryStore = &loaded.store;
    let text =
        layerstack_usda::save_usda(&flat.layer, store.tokens(), store.paths()).expect("saves");
    let errors = flat
        .report
        .expression_errors
        .iter()
        .map(|error| (error.expression.clone(), error.error.clone()))
        .collect();
    (
        text.replace(&format!("{}/", fixtures().to_string_lossy()), "<dir>/"),
        errors,
    )
}

/// `UsdUtils.FlattenLayerStack` evaluates the asset path expressions of
/// attribute defaults and samples and of arcs with the layer stack's own
/// variables, writing one that evaluates to no path or fails as `@@`
/// with a warning, and keeps the asset paths of dictionaries as authored.
#[test]
fn flattening_the_layer_stack_writes_what_openusd_writes() {
    let oracle = oracle();
    let (loaded, _) = load();
    let (text, errors) = flatten_layer_stack_text(&loaded);
    assert_eq!(text, oracle["flatten_layer_stack"].as_str().unwrap());
    let warned: BTreeSet<(String, String)> = oracle["flatten_layer_stack_warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| {
            // `Error evaluating expression EXPRESSION: MESSAGE`
            let warning = warning.as_str().unwrap();
            let rest = warning
                .strip_prefix("Error evaluating expression ")
                .unwrap();
            let (expression, message) = rest.split_once("`: ").unwrap();
            (format!("{expression}`"), message.to_string())
        })
        .collect();
    assert_eq!(errors.into_iter().collect::<BTreeSet<_>>(), warned);
}

/// A live stage reads an asset path expression with the variables as they
/// are now: editing a variable it reads recomposes, as OpenUSD resyncs
/// the prims of a layer stack whose variables change
/// (`UsdStage::_ProcessPendingChanges` on `didChangeExpressionVariables`).
#[test]
fn a_live_stage_reads_expressions_with_edited_variables() {
    let (mut loaded, _) = load();
    let root = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root, StageOptions::default());
    let named = loaded.store.property_path("/Local.named");
    let clipped = loaded.store.property_path("/Clipped.tex");
    let read = |live: &LiveStage, store: &InMemoryStore, property, time| {
        live.stage()
            .view(store)
            .attribute(property)
            .and_then(|a| a.get(time))
            .map(|resolved| resolved.value)
    };
    for (property, time) in [(named, LsTime::Default), (clipped, LsTime::at(0.0))] {
        assert_eq!(
            read(&live, &loaded.store, property, time),
            Some(Value::Asset("./wood.png".into()))
        );
    }
    let field = loaded.store.tokens().intern("expressionVariables");
    let string = |text: &str| Value::String(text.into());
    let variables = Value::Dictionary(
        [
            ("DIR", string("maps")),
            ("FLAG", Value::Bool(true)),
            ("NAME", string("stone")),
            ("NUM", Value::Int(3)),
            ("TEX", string("")),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect(),
    );
    loaded
        .store
        .layer_mut(root)
        .expect("root")
        .set_metadata(field, variables);
    live.notify_expression_variables_edit(&loaded.store, root);
    assert!(!live.recompose(&mut loaded.store).is_empty());
    for (property, time) in [(named, LsTime::Default), (clipped, LsTime::at(0.0))] {
        assert_eq!(
            read(&live, &loaded.store, property, time),
            Some(Value::Asset("./stone.png".into()))
        );
    }
}

/// An asset path expression a live edit authors reads evaluated, in the
/// node's context, as one the layer held when the stage was composed.
#[test]
fn a_live_edit_authoring_an_expression_reads_it_evaluated() {
    let (mut loaded, _) = load();
    let root = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root, StageOptions::default());
    let sub = *loaded
        .layer_names
        .iter()
        .find(|(_, name)| name.as_str() == "sub/sub.usda")
        .expect("the sublayer")
        .0;
    for (layer, property, expected) in [
        (root, "/Local.plain", "./wood.png"),
        (sub, "/Sub.plain", "./wood.png"),
        (root, "/Local.number", "`\"${TEX}\"`"),
    ] {
        let property = loaded.store.property_path(property);
        let mut transaction = Transaction::new();
        let authored = if expected.starts_with('`') {
            expected
        } else {
            "`\"./${NAME}.png\"`"
        };
        transaction.set_default(
            EditTarget::for_layer(layer).property(property),
            Value::Asset(authored.into()),
        );
        live.apply(&mut loaded.store, &transaction)
            .expect("applies");
        assert_eq!(
            live.stage()
                .view(&loaded.store)
                .attribute(property)
                .and_then(|a| a.get(LsTime::Default))
                .map(|resolved| resolved.value),
            Some(Value::Asset(expected.into())),
            "{property:?}"
        );
    }
}

/// Replacing an expression that fails to evaluate with a path drops its
/// error, as the stage then reads no expression there.
#[test]
fn a_live_edit_replacing_a_failing_expression_drops_its_error() {
    let (mut loaded, _) = load();
    let root = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root, StageOptions::default());
    let failures = |live: &LiveStage| {
        live.stage()
            .composition_errors()
            .iter()
            .filter(|error| {
                matches!(
                    error,
                    CompositionError::VariableExpressionError(error)
                        if error.context == ExpressionContext::AssetValue
                            && error.expression == "`${NUM}`"
                            && error.layer == root
                )
            })
            .count()
    };
    assert_eq!(
        failures(&live),
        1,
        "`/Local.number` and `.arraySamples` fail"
    );
    let property = loaded.store.property_path("/Local.number");
    let mut transaction = Transaction::new();
    transaction.set_default(
        EditTarget::for_layer(root).property(property),
        Value::Asset("./plain.png".into()),
    );
    live.apply(&mut loaded.store, &transaction)
        .expect("applies");
    // `.arraySamples` at 1 still fails.
    assert_eq!(failures(&live), 1);
    let samples = loaded.store.property_path("/Local.arraySamples");
    let mut transaction = Transaction::new();
    transaction.set_time_sample(
        EditTarget::for_layer(root).property(samples),
        1.0,
        Value::Array(vec![Value::Asset("./plain.png".into())]),
    );
    live.apply(&mut loaded.store, &transaction)
        .expect("applies");
    assert_eq!(failures(&live), 0);
}
