// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for references and payloads that target `defaultPrim`.
//!
//! `tests/data/default_prim.json` records what OpenUSD 26.08 composes from
//! `tests/assets/default_prim/{usda,usdc}/root.*`
//! (`scripts/default_prim_oracle.py`, which also writes those layers and
//! checks that OpenUSD composes both formats identically). Layerstack must
//! compose each format to the same prims, children, prim stacks and values,
//! and report the same unresolved `defaultPrim` arcs as composition errors.
//!
//! Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim`), §10.3.2.1 (references),
//! §10.3.2.2 (payloads).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{
    ArcKind, CompositionError, Layer, LayerId, PathId, PropertyPath, Stage, StageOptions,
    TokenInterner, UnresolvedAsset, UnresolvedDefaultPrim, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    usdc::load_entry_usdc,
    workspace_root,
};
use serde::Deserialize;

const VECTORS: &str = include_str!("data/default_prim.json");

#[derive(Deserialize)]
struct Vectors {
    openusd_version: String,
    formats: Vec<String>,
    root: String,
    prims: Vec<Prim>,
    values: BTreeMap<String, Option<f64>>,
    errors: Vec<Error>,
    default_prim_paths: Vec<(String, Option<String>)>,
}

#[derive(Deserialize)]
struct Prim {
    path: String,
    children: Vec<String>,
    prim_stack: Vec<(String, String)>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Error {
    prim: String,
    arc: String,
    /// The layer whose `defaultPrim` did not resolve.
    #[serde(default)]
    layer: Option<String>,
    /// `<defaultPrim>`, or the path `defaultPrim` names.
    #[serde(default)]
    unresolved: Option<String>,
    /// The asset that could not be opened.
    #[serde(default)]
    asset: Option<String>,
}

fn vectors() -> Vectors {
    let vectors: Vectors = serde_json::from_str(VECTORS).expect("default_prim.json");
    assert!(
        vectors.openusd_version.starts_with("0.26."),
        "vectors from OpenUSD {}",
        vectors.openusd_version
    );
    vectors
}

fn load(format: &str, root: &str) -> LoadedStage {
    let entry = workspace_root()
        .join("layerstack_conformance/tests/assets/default_prim")
        .join(format)
        .join(format!("{root}.{format}"));
    match format {
        "usda" => load_entry_usda(&entry),
        "usdc" => load_entry_usdc(&entry),
        other => panic!("unknown format {other}"),
    }
}

/// A loaded layer's name without directory or extension, as the vectors
/// record it.
fn layer_name(loaded: &LoadedStage, layer: LayerId) -> String {
    file_stem(&loaded.layer_names[&layer])
}

fn display(loaded: &LoadedStage, path: PathId) -> String {
    loaded.store.paths.display(path, &loaded.store.tokens)
}

/// Renders an error in the vectors' shape.
fn render_error(loaded: &LoadedStage, error: &CompositionError) -> Error {
    let arc_name = |arc: &ArcKind| {
        match arc {
            ArcKind::References => "reference",
            ArcKind::Payloads => "payload",
            other => panic!("unexpected arc {other:?}"),
        }
        .to_string()
    };
    match error {
        CompositionError::UnresolvedDefaultPrim(UnresolvedDefaultPrim {
            prim,
            arc,
            layer,
            path,
        }) => Error {
            prim: display(loaded, *prim),
            arc: arc_name(arc),
            layer: Some(layer_name(loaded, *layer)),
            unresolved: Some(match path {
                Some(path) => format!("<{}>", display(loaded, *path)),
                None => "<defaultPrim>".to_string(),
            }),
            asset: None,
        },
        CompositionError::UnresolvedAsset(UnresolvedAsset { prim, arc, asset }) => Error {
            prim: display(loaded, *prim),
            arc: arc_name(arc),
            layer: None,
            unresolved: None,
            asset: Some(file_stem(asset)),
        },
        other => panic!("unexpected composition error {other:?}"),
    }
}

/// A file name without directory or extension.
fn file_stem(name: &str) -> String {
    let file = name.rsplit('/').next().unwrap_or(name);
    file.rsplit_once('.')
        .map_or(file, |(stem, _)| stem)
        .to_string()
}

#[test]
fn default_prim_arcs_match_openusd() {
    let vectors = vectors();
    assert_eq!(vectors.formats, ["usda", "usdc"]);
    for format in &vectors.formats {
        let mut loaded = load(format, &vectors.root);
        let stage = Stage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let pseudo_root = loaded.store.path("/");

        let composed: Vec<String> = stage
            .traverse(pseudo_root)
            .filter(|prim| *prim != pseudo_root)
            .map(|prim| display(&loaded, prim))
            .collect();
        let expected: Vec<&str> = vectors.prims.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(composed, expected, "{format}: composed prims");

        for prim in &vectors.prims {
            let id = loaded.store.path(&prim.path);
            let children: Vec<String> = stage
                .children_of(id)
                .unwrap_or(&[])
                .iter()
                .map(|child| {
                    let leaf = loaded.store.paths.resolve(*child).leaf().expect("name");
                    loaded.store.tokens.resolve(leaf).to_string()
                })
                .collect();
            assert_eq!(
                children, prim.children,
                "{format}: children of {}",
                prim.path
            );
            let prim_stack: Vec<(String, String)> = stage
                .prim_stack(id)
                .expect("composed prim")
                .into_iter()
                .map(|(layer, spec)| {
                    (
                        layer_name(&loaded, layer),
                        spec.display(&loaded.store.tokens),
                    )
                })
                .collect();
            assert_eq!(
                prim_stack, prim.prim_stack,
                "{format}: prim stack of {}",
                prim.path
            );
        }

        for (attr, expected) in &vectors.values {
            let property = loaded.store.property_path(attr);
            let value = stage
                .resolve_field_path(property)
                .map(|resolved| resolved.value);
            assert_eq!(
                value,
                expected.map(Value::Double),
                "{format}: value of {attr}"
            );
        }

        let mut errors: Vec<Error> = stage
            .composition_errors()
            .iter()
            .map(|error| render_error(&loaded, error))
            .collect();
        errors.sort();
        let mut expected_errors: Vec<&Error> = vectors.errors.iter().collect();
        expected_errors.sort();
        assert_eq!(
            errors.iter().collect::<Vec<_>>(),
            expected_errors,
            "{format}: composition errors"
        );
    }
}

#[test]
fn default_prim_paths_match_openusd() {
    let mut tokens = TokenInterner::default();
    let mut layer = Layer::new(LayerId(1));
    for (token, expected) in vectors().default_prim_paths {
        layer.default_prim = Some(tokens.intern(&token));
        let actual = layer
            .default_prim_path(&mut tokens)
            .map(|path| path.display(&tokens));
        assert_eq!(actual, expected, "defaultPrim = {token:?}");
    }
}

/// The two placements share the asset's layers but compose only the subtree
/// under its `defaultPrim`: no root prim of the asset other than `/Model`
/// reaches the stage, under the placements or at the root.
#[test]
fn placements_do_not_copy_other_root_prims() {
    for format in ["usda", "usdc"] {
        let mut loaded = load(format, "root");
        let stage = Stage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        for path in [
            "/Model",
            "/Other",
            "/PlacementA/OtherChild",
            "/PlacementB/OtherChild",
            "/PlacementA/Model",
        ] {
            let id = loaded.store.path(path);
            assert!(!stage.has_prim(id), "{format}: {path} is not composed");
        }
        let placement = loaded.store.path("/PlacementA");
        let sources: Vec<String> = stage
            .prim_stack(placement)
            .expect("composed")
            .into_iter()
            .map(|(layer, spec)| {
                format!(
                    "{} {}",
                    layer_name(&loaded, layer),
                    spec.display(&loaded.store.tokens)
                )
            })
            .collect();
        assert_eq!(sources, ["root /PlacementA", "asset /Model"]);
    }
}

#[test]
fn values_resolve_through_the_selected_default_prim() {
    let mut loaded = load("usda", "root");
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let size = |loaded: &mut LoadedStage, path: &str| -> PropertyPath {
        loaded.store.property_path(&format!("{path}.size"))
    };
    let a = size(&mut loaded, "/PlacementA");
    let b = size(&mut loaded, "/PlacementB");
    let resolved = |property| stage.resolve_field_path(property).map(|r| r.value);
    assert_eq!(resolved(a), Some(Value::Double(2.0)), "from the asset");
    assert_eq!(resolved(b), Some(Value::Double(3.0)), "local opinion wins");
}
