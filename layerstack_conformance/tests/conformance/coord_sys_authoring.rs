// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Coordinate-system binding authoring against OpenUSD, call by call.
//!
//! `scripts/coord_sys_authoring_oracle.py` calls C++
//! `UsdShadeCoordSysAPI::Bind`, `BlockBinding` and `ClearBinding` one at a
//! time on a new stage, recording each call's accept or reject, every
//! `coordSys:` relationship spec of the root layer and the local bindings
//! each prim then reads (`HasLocalBindingsForPrim`,
//! `GetLocalBindingsForPrim`, `GetLocalBinding` per instance). The test
//! replays the calls through `CoordSysApiEdit`, applying each before the
//! next, and compares them; it also compares the static
//! `GetBindingBaseName` and `CanContainPropertyName`.

use crate::support::schema_scene as support;

use std::collections::BTreeMap;

use layerstack::{EditTarget, InMemoryStore, LayerId, PropertyKind, TargetPath};
use layerstack_schemas::usd_shade::{CoordSysApi, CoordSysApiEdit};
use layerstack_schemas::{
    CoordSysBinding, InstanceView, PrimView, Scene, SchemaEdit,
    can_contain_coord_sys_property_name, coord_sys_binding_base_name, has_local_coord_sys_bindings,
    local_coord_sys_bindings,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/coord_sys_authoring/oracle.json");

const SCENE: &str = r#"#usda 1.0
def Xform "World"
{
    def Xform "Space" {}
    def Xform "Other" {}
    def Xform "Model" (prepend apiSchemas = ["CoordSysAPI:modelSpace"]) {}
    def Xform "Plain" {}
}
"#;

const PRIMS: [&str; 2] = ["/World/Model", "/World/Plain"];
const INSTANCES: [&str; 4] = ["modelSpace", "paint", "fresh", "missing"];

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    calls: Vec<Call>,
    base_names: BTreeMap<String, String>,
    can_contain: BTreeMap<String, bool>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Call {
    call: String,
    accepted: bool,
    specs: BTreeMap<String, Spec>,
    reads: BTreeMap<String, Reads>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Spec {
    custom: bool,
    explicit: bool,
    explicit_targets: Vec<String>,
    prepended: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Reads {
    has_local: bool,
    local: Vec<Binding>,
    instance: BTreeMap<String, Option<Binding>>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Binding {
    name: String,
    relationship: String,
    target: String,
}

fn binding(binding: &CoordSysBinding, store: &InMemoryStore) -> Binding {
    Binding {
        name: binding.base_name(),
        relationship: binding
            .relationship
            .display(store.paths(), store.tokens())
            .to_string(),
        target: binding.target.display(store.paths(), store.tokens()),
    }
}

/// Every `coordSys:` relationship spec of the root layer.
fn specs(store: &InMemoryStore) -> BTreeMap<String, Spec> {
    let layer = store.layer(LayerId(1)).unwrap();
    let display = |targets: &[TargetPath]| -> Vec<String> {
        targets
            .iter()
            .map(|target| target.display(store.paths(), store.tokens()))
            .collect()
    };
    let mut found = BTreeMap::new();
    for (path, prim) in layer.prims().iter() {
        for entry in &prim.properties {
            let name = store.tokens().resolve(entry.name);
            let spec = &entry.spec;
            if spec.kind != PropertyKind::Relationship || !name.starts_with("coordSys:") {
                continue;
            }
            let op = spec.targets.as_deref();
            found.insert(
                format!("{}.{name}", store.paths().display(*path, store.tokens())),
                Spec {
                    custom: spec.custom,
                    explicit: op.is_some_and(|op| op.explicit.is_some()),
                    explicit_targets: op
                        .and_then(|op| op.explicit.as_deref())
                        .map(display)
                        .unwrap_or_default(),
                    prepended: op.map(|op| display(&op.prepend)).unwrap_or_default(),
                },
            );
        }
    }
    found
}

fn reads(scene: &Scene<'_>, store: &InMemoryStore) -> BTreeMap<String, Reads> {
    PRIMS
        .iter()
        .map(|path| {
            let prim = store.path(path);
            let reads = Reads {
                has_local: has_local_coord_sys_bindings(scene, prim),
                local: local_coord_sys_bindings(scene, prim)
                    .iter()
                    .map(|found| binding(found, store))
                    .collect(),
                instance: INSTANCES
                    .iter()
                    .map(|instance| {
                        let api = CoordSysApi::from_instance_unchecked(InstanceView::new(
                            PrimView::new(*scene, prim),
                            instance,
                        ));
                        assert_eq!(
                            api.binding_base_name(),
                            format!("{instance}:binding"),
                            "the relationship name without coordSys:"
                        );
                        let found = api.local_binding().map(|found| binding(&found, store));
                        ((*instance).to_owned(), found)
                    })
                    .collect(),
            };
            ((*path).to_owned(), reads)
        })
        .collect()
}

/// One authoring call: `Bind`, `BlockBinding` or `ClearBinding`.
enum Author {
    Bind(&'static str),
    Block,
    Clear { remove_spec: bool },
}

#[test]
fn coord_sys_binding_authoring_matches_openusd_call_by_call() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, mut live) = support::scene(SCENE);
    use Author::{Bind, Block, Clear};
    let calls = [
        (
            "bind modelSpace",
            "/World/Model",
            "modelSpace",
            Bind("/World/Space"),
        ),
        (
            "bind paint unapplied",
            "/World/Plain",
            "paint",
            Bind("/World/Space"),
        ),
        (
            "bind modelSpace again",
            "/World/Model",
            "modelSpace",
            Bind("/World/Other"),
        ),
        ("block modelSpace", "/World/Model", "modelSpace", Block),
        (
            "clear modelSpace",
            "/World/Model",
            "modelSpace",
            Clear { remove_spec: false },
        ),
        (
            "bind modelSpace after clear",
            "/World/Model",
            "modelSpace",
            Bind("/World/Space"),
        ),
        (
            "clear paint removing spec",
            "/World/Plain",
            "paint",
            Clear { remove_spec: true },
        ),
        (
            "clear missing",
            "/World/Plain",
            "missing",
            Clear { remove_spec: false },
        ),
        ("block fresh unapplied", "/World/Plain", "fresh", Block),
        (
            "clear fresh removing spec",
            "/World/Plain",
            "fresh",
            Clear { remove_spec: true },
        ),
        (
            "clear modelSpace removing spec",
            "/World/Model",
            "modelSpace",
            Clear { remove_spec: true },
        ),
    ];
    assert_eq!(calls.len(), oracle.calls.len());
    for ((label, prim, instance, author), expected) in calls.into_iter().zip(&oracle.calls) {
        assert_eq!(label, expected.call);
        let prim = store.path(prim);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let api = CoordSysApiEdit::new(&edit, prim, instance)
            .expect("outside any instance")
            .unwrap();
        let accepted = match author {
            Bind(target) => {
                let target = TargetPath::Prim(edit.prim_path(target).unwrap());
                api.bind(&mut edit, target);
                true
            }
            Block => {
                api.block_binding(&mut edit);
                true
            }
            Clear { remove_spec } => {
                let result = api.clear_binding(&mut edit, remove_spec);
                if let Err(error) = &result {
                    assert!(edit.transaction().is_empty(), "{label}: refusal authors");
                    assert!(
                        error
                            .to_string()
                            .starts_with("cannot clear the coordinate-system binding: "),
                        "{label}: {error}"
                    );
                }
                result.is_ok()
            }
        };
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let actual = Call {
            call: label.into(),
            accepted,
            specs: specs(&store),
            reads: reads(&scene, &store),
        };
        assert_eq!(&actual, expected, "{label}");
    }
}

#[test]
fn coord_sys_names_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    for (name, expected) in &oracle.base_names {
        assert_eq!(coord_sys_binding_base_name(name), expected, "{name:?}");
    }
    for (name, expected) in &oracle.can_contain {
        assert_eq!(
            can_contain_coord_sys_property_name(name),
            *expected,
            "{name:?}"
        );
    }
}
