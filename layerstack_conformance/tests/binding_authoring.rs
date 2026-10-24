// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Material binding authoring against OpenUSD, call by call.
//!
//! `scripts/binding_authoring_oracle.py` calls C++
//! `UsdShadeMaterialBindingAPI::Bind`, `UnbindDirectBinding`,
//! `UnbindCollectionBinding` and `UnbindAllBindings` one at a time on a new
//! stage, recording each call's accept or reject and every binding
//! relationship spec after it: explicit targets, `bindMaterialAs` and
//! `custom`. The test replays the calls through `MaterialBindingApiEdit`,
//! applying each before the next, and compares the root layer's specs.

#![allow(missing_docs, reason = "integration tests")]

#[path = "support/schema_scene.rs"]
mod support;

use std::collections::BTreeMap;

use layerstack::{EditTarget, InMemoryStore, LayerId, PathId, PropertyKind, Value};
use layerstack_schemas::usd_shade::MaterialBindingApiEdit;
use layerstack_schemas::{BindingStrength, MaterialPurpose, MetadataError, SchemaEdit};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/binding_authoring/oracle.json");

const SCENE: &str = r#"#usda 1.0
def Xform "Set" (prepend apiSchemas = ["CollectionAPI:metal", "MaterialBindingAPI"])
{
    prepend rel collection:metal:includes = </Set/Mesh>

    def Mesh "Mesh" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
    }
}

def "Looks"
{
    def Material "A" {}
    def Material "B" {}
}
"#;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    calls: Vec<Call>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Call {
    call: String,
    accepted: bool,
    specs: BTreeMap<String, Spec>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Spec {
    targets: Vec<String>,
    explicit: bool,
    strength: Option<String>,
    custom: bool,
}

/// Every binding relationship spec of the root layer.
fn specs(store: &InMemoryStore) -> BTreeMap<String, Spec> {
    let layer = store.layer(LayerId(1)).unwrap();
    let strength = store.tokens.lookup("bindMaterialAs");
    let mut found = BTreeMap::new();
    for (path, prim) in layer.prims().iter() {
        for entry in &prim.properties {
            let name = store.tokens.resolve(entry.name);
            let spec = &entry.spec;
            if spec.kind != PropertyKind::Relationship || !name.starts_with("material:binding") {
                continue;
            }
            let targets = spec.targets.as_deref();
            found.insert(
                format!("{}.{name}", store.paths.display(*path, &store.tokens)),
                Spec {
                    targets: targets
                        .and_then(|op| op.explicit.as_ref())
                        .into_iter()
                        .flatten()
                        .map(|target| target.display(&store.paths, &store.tokens).to_string())
                        .collect(),
                    explicit: targets.is_some_and(|op| op.explicit.is_some()),
                    strength: strength
                        .and_then(|key| spec.metadata(key))
                        .and_then(|field| match field {
                            layerstack::FieldValue::Value(Value::Token(token)) => {
                                Some(store.tokens.resolve(*token).to_string())
                            }
                            _ => None,
                        }),
                    custom: spec.custom,
                },
            );
        }
    }
    found
}

#[test]
fn binding_authoring_matches_openusd_call_by_call() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, mut live) = support::scene(SCENE);
    let set = store.path("/Set");
    let mesh = store.path("/Set/Mesh");
    let a = store.path("/Looks/A");
    let b = store.path("/Looks/B");
    let collection = store.property_path("/Set.collection:metal");
    let stronger = Some(BindingStrength::StrongerThanDescendants);
    let weaker = Some(BindingStrength::WeakerThanDescendants);
    let spaced = MaterialPurpose::Other("a b".into());
    enum Author {
        Bind(PathId, Option<BindingStrength>, MaterialPurpose),
        BindCollection(Option<&'static str>, MaterialPurpose),
        Unbind(MaterialPurpose),
        UnbindCollection(&'static str),
        UnbindAll(PathId),
    }
    use Author::{Bind, BindCollection, Unbind, UnbindAll, UnbindCollection};
    use MaterialPurpose::{All, Full, Preview};
    let calls = [
        ("bind A", Bind(a, None, All)),
        ("bind A stronger", Bind(a, stronger, All)),
        ("bind B fallback", Bind(b, None, All)),
        ("bind A weaker preview", Bind(a, weaker, Preview)),
        ("bind A purpose 'a b'", Bind(a, None, spaced)),
        (
            "bind collection metalBits",
            BindCollection(Some("metalBits"), All),
        ),
        (
            "bind collection unnamed preview",
            BindCollection(None, Preview),
        ),
        (
            "bind collection bad:name",
            BindCollection(Some("bad:name"), All),
        ),
        ("bind collection '1x'", BindCollection(Some("1x"), All)),
        ("unbind", Unbind(All)),
        ("unbind full", Unbind(Full)),
        ("unbind collection metalBits", UnbindCollection("metalBits")),
        ("unbind all /Set", UnbindAll(set)),
        ("unbind all /Set/Mesh", UnbindAll(mesh)),
    ];
    assert_eq!(calls.len(), oracle.calls.len());
    for ((label, author), expected) in calls.into_iter().zip(&oracle.calls) {
        assert_eq!(label, expected.call);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let (on_mesh, on_set) = (
            MaterialBindingApiEdit::new(&edit, mesh)
                .expect("outside any instance")
                .unwrap(),
            MaterialBindingApiEdit::new(&edit, set)
                .expect("outside any instance")
                .unwrap(),
        );
        let result = match author {
            Bind(material, strength, purpose) => {
                on_mesh.bind(&mut edit, material, strength, &purpose)
            }
            BindCollection(name, purpose) => {
                on_set.bind_collection(&mut edit, collection, b, name, None, &purpose)
            }
            Unbind(purpose) => on_mesh.unbind(&mut edit, &purpose),
            UnbindCollection(name) => on_set.unbind_collection(&mut edit, name, &All),
            UnbindAll(path) => {
                MaterialBindingApiEdit::new(&edit, path)
                    .expect("outside any instance")
                    .unwrap()
                    .unbind_all(&mut edit);
                Ok(())
            }
        };
        assert_eq!(result.is_ok(), expected.accepted, "{label}: {result:?}");
        if result.is_err() {
            assert!(
                edit.transaction().is_empty(),
                "{label}: a refusal collects nothing"
            );
        }
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let actual = Call {
            call: label.into(),
            accepted: result.is_ok(),
            specs: specs(&store),
        };
        assert_eq!(&actual, expected, "{label}");
    }
}

/// Calls in one edit see each other: a fallback strength after a stronger
/// one in the same edit weakens it, as it would applied call by call.
#[test]
fn binding_strength_reads_earlier_calls_in_the_same_edit() {
    let (mut store, mut live) = support::scene(SCENE);
    let mesh = store.path("/Set/Mesh");
    let a = store.path("/Looks/A");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let api = MaterialBindingApiEdit::new(&edit, mesh)
        .expect("outside any instance")
        .unwrap();
    api.bind(
        &mut edit,
        a,
        Some(BindingStrength::StrongerThanDescendants),
        &MaterialPurpose::All,
    )
    .unwrap();
    api.bind(&mut edit, a, None, &MaterialPurpose::All).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let specs = specs(&store);
    let binding = &specs["/Set/Mesh.material:binding"];
    assert_eq!(binding.strength.as_deref(), Some("weakerThanDescendants"));
    assert!(!binding.custom);
}

/// Binding strength is ordinary property metadata, so the public metadata
/// edit authors it too, and refuses it on an attribute.
#[test]
fn binding_strength_through_property_metadata() {
    let (mut store, mut live) = support::scene(SCENE);
    let mesh = store.path("/Set/Mesh");
    let a = store.path("/Looks/A");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let api = MaterialBindingApiEdit::new(&edit, mesh)
        .expect("outside any instance")
        .unwrap();
    api.bind(&mut edit, a, None, &MaterialPurpose::All).unwrap();
    let stronger = Value::Token(edit.tokens().intern("strongerThanDescendants"));
    api.write_property_metadata(
        &mut edit,
        "material:binding",
        "bindMaterialAs",
        stronger.clone(),
    )
    .unwrap();
    assert_eq!(
        api.write_property_metadata(&mut edit, "doubleSided", "bindMaterialAs", stronger),
        Err(MetadataError::Field(layerstack::MetadataEditError {
            field: "bindMaterialAs".into(),
            target: layerstack::MetadataTarget::Attribute,
            reason: layerstack::MetadataEditReason::NotForSpec,
        }))
    );
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = layerstack_schemas::Scene::new(live.stage(), &store);
    let metadata = layerstack_schemas::PrimView::new(scene, mesh)
        .property_metadata("material:binding")
        .unwrap();
    assert_eq!(metadata.bind_material_as(), Some("strongerThanDescendants"));
}
