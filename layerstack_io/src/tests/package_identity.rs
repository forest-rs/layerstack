// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use layerstack::asset_dependencies::{
    AssetDependencyKind, DependencyTarget, extract_layer_dependencies,
};

const ROOT: &str = r#"#usda 1.0
def "Model" (
    assetInfo = {
        asset identifier = @missing-identity.usda@
        asset preview = @image.png@
        dictionary nested = { asset identifier = @nested.png@ }
    }
) {}
"#;

#[test]
fn packaging_preserves_identity_without_loading_and_keeps_nested_resources() {
    let mut document = StageDocument::open(
        Memory::with(&[
            ("root.usda", ROOT),
            ("image.png", "image"),
            ("nested.png", "nested"),
        ]),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = document.stage.stage().root_layer().unwrap();
    let original = document.store.layers[&root].clone();
    let mut callbacks = Vec::new();
    let prepared = document
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |dependency| {
                callbacks.push(dependency.clone());
                PackageInclusion::Include
            },
        )
        .unwrap();
    assert!(
        prepared.report().is_complete(),
        "identity is not a required resource: {:?}",
        prepared.report()
    );
    assert_eq!(
        prepared.plan().unwrap().assets.len(),
        2,
        "other assetInfo values remain required resources"
    );
    assert_eq!(
        callbacks.len(),
        2,
        "identity does not request an inclusion decision"
    );
    assert!(
        !document
            .storage
            .reads
            .iter()
            .any(|path| path == "missing-identity.usda"),
        "identity does not trigger transport"
    );
    let identity = prepared
        .report()
        .dependencies
        .iter()
        .find(|use_| use_.source.kind == AssetDependencyKind::Identity)
        .unwrap();
    assert_eq!(
        identity.targets,
        vec![DependencyTarget::Ignored],
        "report explains the identity exclusion"
    );
    let uses = extract_layer_dependencies(
        &prepared.plan().unwrap().layers[0].layer,
        &document.store.paths,
        &document.store.tokens,
    );
    assert!(
        uses.iter()
            .any(|use_| use_.kind == AssetDependencyKind::Identity
                && use_.identifier.as_ref() == "missing-identity.usda"),
        "localized identity spelling is retained verbatim"
    );
    assert!(
        uses.iter()
            .filter(|use_| use_.kind == AssetDependencyKind::Value)
            .all(|use_| use_.identifier.starts_with("assets/")),
        "nested resources are localized"
    );
    assert_eq!(
        document.store.layers[&root], original,
        "preparation leaves authored source unchanged"
    );
    document.export_package("out.usdz", &prepared).unwrap();
    let mut output = Memory::default();
    output.files.insert(
        "out.usdz".into(),
        document.storage.files["out.usdz"].clone(),
    );
    let mut reopened = StageDocument::open(output, "out.usdz", StageOptions::default()).unwrap();
    let reopened_root = reopened.stage.stage().root_layer().unwrap();
    let uses = extract_layer_dependencies(
        &reopened.store.layers[&reopened_root],
        &reopened.store.paths,
        &reopened.store.tokens,
    );
    assert!(
        uses.iter()
            .any(|use_| use_.kind == AssetDependencyKind::Identity
                && use_.identifier.as_ref() == "missing-identity.usda"),
        "serialized identity survives reopening"
    );
    let mut payloads = Vec::new();
    for use_ in uses
        .into_iter()
        .filter(|use_| use_.kind == AssetDependencyKind::Value)
    {
        payloads.push(
            reopened
                .read_asset_bytes(&use_.identifier, Some(reopened_root))
                .unwrap()
                .bytes
                .as_ref()
                .to_vec(),
        );
    }
    payloads.sort();
    assert_eq!(
        payloads,
        vec![b"image".to_vec(), b"nested".to_vec()],
        "localized resources remain readable after reopen"
    );
    document.storage.files.remove("nested.png");
    let missing = document
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(
        !missing.report().is_complete(),
        "a nested identifier remains a required resource"
    );
    assert!(
        missing.plan().is_none(),
        "missing actual resources prevent publication"
    );
}

#[test]
fn resource_with_the_same_spelling_as_identity_still_blocks_incomplete_package() {
    let text = ROOT.replace("@nested.png@", "@missing-identity.usda@");
    let mut document = StageDocument::open(
        Memory::with(&[("root.usda", &text), ("image.png", "image")]),
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = document.stage.stage().root_layer().unwrap();
    let prepared = document
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(
        !prepared.report().is_complete(),
        "identity exclusion must not mask an identical resource use"
    );
    assert!(
        prepared
            .report()
            .dependencies
            .iter()
            .any(|use_| use_.source.kind == AssetDependencyKind::Value
                && use_.source.identifier.as_ref() == "missing-identity.usda"
                && matches!(use_.targets.first(), Some(DependencyTarget::Unresolved(_)))),
        "unresolved resource retains its distinct role"
    );
}
