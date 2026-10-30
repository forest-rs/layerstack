// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Base materials against OpenUSD: reads, and authoring call by call.
//!
//! `scripts/base_material_oracle.py` records C++
//! `UsdShadeMaterial::GetBaseMaterialPath`, `HasBaseMaterial` and
//! `GetBaseMaterial` for every prim of `fixtures/base_material/scene.usda`
//! (specializes of materials and of other prims, chains, inherits,
//! variants, external and internal references, an instance), then calls
//! `SetBaseMaterialPath`, `SetBaseMaterial` and `ClearBaseMaterial` one at
//! a time on a new stage, recording the root layer's `specializes` and the
//! base material after each. The test reads the scene through `Material`
//! and replays the calls through `MaterialEdit`.

use crate::support::schema_scene as support;

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{EditTarget, InMemoryStore, LayerId, Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::usd_shade::{Material, MaterialEdit};
use layerstack_schemas::{PrimView, Scene, SchemaEdit};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/base_material/oracle.json");

const AUTHORING: &str = r#"#usda 1.0
def "Looks"
{
    def Material "A" {}
    def Material "B" {}
    def Material "M" {}
}
"#;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    reads: BTreeMap<String, Read>,
    calls: Vec<Call>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Read {
    path: Option<String>,
    has: bool,
    material: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Call {
    call: String,
    specializes: Option<Specializes>,
    path: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Specializes {
    explicit: bool,
    explicit_items: Vec<String>,
    prepended: Vec<String>,
}

/// C++ names a prim of an instance by its prototype's path; this stage
/// names it by the instance proxy's.
fn proxy_path(path: &str) -> String {
    path.replace("/__Prototype_1/", "/Instanced/")
}

#[test]
fn base_material_reads_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/base_material/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(loaded.store.tokens()))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);
    let display = |path| store.paths().display(path, store.tokens());
    for (path, expected) in &oracle.reads {
        let prim = store.path(path);
        let material = Material::from_prim_unchecked(PrimView::new(scene, prim));
        let actual = Read {
            path: material.base_material_path().map(display),
            has: material.has_base_material(),
            material: material.base_material().map(|base| display(base.path())),
        };
        let expected = Read {
            path: expected.path.as_deref().map(proxy_path),
            has: expected.has,
            material: expected.material.as_deref().map(proxy_path),
        };
        assert_eq!(actual, expected, "{path}");
    }
}

/// The root layer's `specializes` on `/Looks/M`.
fn specializes(store: &InMemoryStore) -> Option<Specializes> {
    let layer = store.layer(LayerId(1)).unwrap();
    let spec = layer.prim(store.path("/Looks/M"))?;
    let op = spec.specializes();
    if *op == layerstack::ListOp::default() {
        return None;
    }
    let display = |paths: &[layerstack::PathId]| -> Vec<String> {
        paths
            .iter()
            .map(|path| store.paths().display(*path, store.tokens()))
            .collect()
    };
    Some(Specializes {
        explicit: op.explicit.is_some(),
        explicit_items: op.explicit.as_deref().map(display).unwrap_or_default(),
        prepended: display(&op.prepend),
    })
}

#[test]
fn base_material_authoring_matches_openusd_call_by_call() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let (mut store, mut live) = support::scene(AUTHORING);
    let m = store.path("/Looks/M");
    let a = store.path("/Looks/A");
    let b = store.path("/Looks/B");
    let nowhere = store.path("/Looks/Nowhere");
    // `SetBaseMaterial` with an invalid material and `SetBaseMaterialPath`
    // with an empty path both pass no base.
    let calls = [
        ("set path A", Some(a)),
        ("set material B", Some(b)),
        ("set path to a missing prim", Some(nowhere)),
        ("set invalid material", None),
        ("set path A again", Some(a)),
        ("set empty path", None),
        ("set material B again", Some(b)),
        ("clear", None),
    ];
    assert_eq!(calls.len(), oracle.calls.len());
    for ((label, base), expected) in calls.into_iter().zip(&oracle.calls) {
        assert_eq!(label, expected.call);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let api = MaterialEdit::new(&edit, m)
            .expect("outside any instance")
            .unwrap();
        if label == "clear" {
            api.clear_base_material(&mut edit);
        } else {
            api.set_base_material(&mut edit, base);
        }
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let material = Material::get(&scene, m).unwrap();
        let actual = Call {
            call: label.into(),
            specializes: specializes(&store),
            path: material
                .base_material_path()
                .map(|path| store.paths().display(path, store.tokens())),
        };
        assert_eq!(&actual, expected, "{label}");
    }
}
