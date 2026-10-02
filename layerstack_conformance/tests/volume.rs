// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `UsdVol` field helpers against OpenUSD 26.8, with mapped transactional authoring.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    LayerId, PathId, PropertyPath, PropertySpec, SpecPath, TargetPath,
    edit::{EditTarget, Transaction},
};
use layerstack_schemas::{Scene, SchemaEdit, usd_vol::Volume, volume::VolumeError};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    fields: BTreeMap<String, String>,
    queries: Vec<Query>,
    edits: Vec<Edit>,
}
#[derive(Deserialize)]
struct Query {
    name: String,
    exists: bool,
    path: String,
}
#[derive(Deserialize)]
struct Edit {
    name: String,
    result: bool,
    exists: bool,
    custom: bool,
    targets: Vec<String>,
}
fn oracle() -> Oracle {
    serde_json::from_str(include_str!("../fixtures/volume/oracle.json")).unwrap()
}
fn path_string(store: &layerstack::InMemoryStore, path: Option<PathId>) -> String {
    path.map_or_else(String::new, |path| {
        store.paths.resolve(path).display(&store.tokens)
    })
}
#[test]
fn field_queries_match_cpp_with_forwarding_cycles_and_basename_collisions() {
    let oracle = oracle();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, live) = support::scene(include_str!("../fixtures/volume/scene.usda"));
    let path = store.path("/Volume");
    let scene = Scene::new(live.stage(), &store);
    let volume = Volume::new(&scene, path).unwrap();
    let fields: BTreeMap<_, _> = volume
        .field_paths()
        .into_iter()
        .map(|(name, path)| (name.to_owned(), path_string(&store, Some(path))))
        .collect();
    assert_eq!(fields, oracle.fields);
    for row in oracle.queries {
        assert_eq!(
            volume.has_field_relationship(&row.name),
            row.exists,
            "{}",
            row.name
        );
        assert_eq!(
            path_string(&store, volume.field_path(&row.name)),
            row.path,
            "{}",
            row.name
        );
    }
}
#[test]
fn field_edit_results_match_cpp_and_preserve_existing_custom_qualifier() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/volume/scene.usda"));
    let volume = store.path("/Volume");
    let field = store.path("/Field");
    let other = store.path("/Other");
    let root = store.path("/");
    let chain = PropertyPath::new(store.path("/Links"), store.tokens.intern("chain"));
    let target = EditTarget::for_layer(LayerId(1));
    let mut setup = Transaction::new();
    setup.create_property(
        target.property(PropertyPath::new(
            volume,
            store.tokens.intern("field:noncustom"),
        )),
        PropertySpec::relationship(),
    );
    live.apply(&mut store, &setup).unwrap();
    for (i, row) in oracle().edits.into_iter().enumerate() {
        let mut edit = SchemaEdit::new(live.stage(), &mut store, target.clone());
        let handle = layerstack_schemas::usd_vol::VolumeEdit::new(&edit, volume).unwrap();
        let result = match i {
            0 => handle
                .create_field_relationship(&mut edit, "new", TargetPath::Prim(field))
                .is_ok(),
            1 => handle
                .create_field_relationship(&mut edit, "field:new", TargetPath::Prim(other))
                .is_ok(),
            2 => handle.block_field_relationship(&mut edit, "new").unwrap(),
            3 => handle
                .block_field_relationship(&mut edit, "absent")
                .unwrap(),
            4 => handle
                .create_field_relationship(&mut edit, "noncustom", TargetPath::Prim(field))
                .is_ok(),
            5 => handle
                .create_field_relationship(&mut edit, "forwarded", TargetPath::Property(chain))
                .is_ok(),
            6 => handle
                .create_field_relationship(&mut edit, "root", TargetPath::Prim(root))
                .is_ok(),
            _ => unreachable!(),
        };
        assert_eq!(result, row.result, "{}", row.name);
        {
            let transaction = edit.finish();
            live.apply(&mut store, &transaction)
        }
        .unwrap();
        let name = store.tokens.intern(format!("field:{}", row.name));
        let scene = Scene::new(live.stage(), &store);
        let view = Volume::new(&scene, volume).unwrap();
        assert_eq!(view.has_field_relationship(&row.name), row.exists);
        let declaration = live.stage().resolve_property_declaration(volume, name);
        assert_eq!(declaration.is_some_and(|d| d.custom), row.custom);
        let targets: Vec<_> = live
            .stage()
            .resolve_target_list_path(PropertyPath::new(volume, name))
            .map(|r| r.value)
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.display(&store.paths, &store.tokens))
            .collect();
        assert_eq!(targets, row.targets);
    }
}
#[test]
fn same_batch_definition_retarget_block_and_undo_are_coherent() {
    let (mut store, mut live) = support::scene("#usda 1.0\n");
    let volume = store.path("/New");
    let field = store.path("/AbsentField");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = Volume::define(&mut edit, volume);
    handle
        .create_field_relationship(&mut edit, "density", TargetPath::Prim(field))
        .unwrap();
    handle
        .create_field_relationship(&mut edit, "field:density", TargetPath::Prim(field))
        .unwrap();
    assert!(
        handle
            .block_field_relationship(&mut edit, "density")
            .unwrap()
    );
    assert!(
        !handle
            .block_field_relationship(&mut edit, "missing")
            .unwrap()
    );
    let applied = {
        let transaction = edit.finish();
        live.apply(&mut store, &transaction)
    }
    .unwrap();
    let scene = Scene::new(live.stage(), &store);
    let view = Volume::new(&scene, volume).unwrap();
    assert!(view.has_field_relationship("density"));
    assert_eq!(view.field_path("density"), None);
    let undone = live.apply(&mut store, &applied.inverse).unwrap();
    assert!(!live.stage().has_prim(volume));
    live.apply(&mut store, &undone.inverse).unwrap();
    assert!(
        Volume::new(&Scene::new(live.stage(), &store), volume)
            .unwrap()
            .has_field_relationship("density")
    );
}
#[test]
fn rejected_edits_and_absent_blocks_collect_no_operations() {
    let (mut store, live) = support::scene(include_str!("../fixtures/volume/scene.usda"));
    let volume = store.path("/Volume");
    let field = store.path("/Field");
    let root = store.path("/");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = layerstack_schemas::usd_vol::VolumeEdit::new(&edit, volume).unwrap();
    for name in ["", "field:", "a::b", "1bad", "a.b"] {
        assert!(matches!(
            handle.create_field_relationship(&mut edit, name, TargetPath::Prim(field)),
            Err(VolumeError::InvalidName(_))
        ));
    }
    assert!(matches!(
        handle.create_field_relationship(&mut edit, "attribute", TargetPath::Prim(field)),
        Err(VolumeError::WrongPropertyKind(_))
    ));
    assert!(matches!(
        handle.block_field_relationship(&mut edit, "attribute"),
        Err(VolumeError::WrongPropertyKind(_))
    ));
    assert!(matches!(
        handle.create_field_relationship(&mut edit, "root", TargetPath::Prim(root)),
        Err(VolumeError::InvalidTarget(_))
    ));
    assert!(
        !handle
            .block_field_relationship(&mut edit, "missingBinding")
            .unwrap()
    );
    assert_eq!(edit.finish(), Transaction::new());
}
#[test]
fn weaker_custom_binding_is_blocked_without_losing_declaration_and_restored_by_undo() {
    let (mut store, mut live) = support::scene(
        "#usda 1.0\ndef Volume \"Asset\" {\n custom rel field:density = </Field> (doc = \"preserve\")\n}\ndef Volume \"Instance\" (references = </Asset>) {}\ndef Scope \"Field\" {}\n",
    );
    let path = store.path("/Instance");
    let name = store.tokens.intern("field:density");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = layerstack_schemas::usd_vol::VolumeEdit::new(&edit, path).unwrap();
    assert!(
        handle
            .block_field_relationship(&mut edit, "density")
            .unwrap()
    );
    let applied = {
        let transaction = edit.finish();
        live.apply(&mut store, &transaction)
    }
    .unwrap();
    assert!(
        live.stage()
            .resolve_property_declaration(path, name)
            .unwrap()
            .custom
    );
    assert_eq!(
        Volume::new(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .field_path("density"),
        None
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert!(
        Volume::new(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .field_path("density")
            .is_some()
    );
}
#[test]
fn variant_target_preserves_local_relationship_metadata_and_child_branch_specs() {
    let (mut store, mut live) = support::scene(
        "#usda 1.0\ndef Volume \"V\" (variants = { string look = \"a\" } prepend variantSets = \"look\") {\n variantSet \"look\" = {\n  \"a\" {\n   custom rel field:density = </Field> (doc = \"preserve\")\n   def Volume \"Child\" {\n    rel field:density = </Field>\n   }\n  }\n }\n}\ndef Scope \"Field\" {}\n",
    );
    let field = store.path("/Field");
    for (path_text, spec_text) in [("/V", "/V{look=a}"), ("/V/Child", "/V{look=a}Child")] {
        let path = store.path(path_text);
        let spec = SpecPath::parse(spec_text, &mut store.tokens, &mut store.paths).unwrap();
        let target = EditTarget::for_local_variant(LayerId(1), &spec);
        let name = store.tokens.intern("field:density");
        let before = live
            .stage()
            .resolve_property_declaration(path, name)
            .unwrap();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
        let handle = layerstack_schemas::usd_vol::VolumeEdit::new(&edit, path).unwrap();
        handle
            .create_field_relationship(&mut edit, "density", TargetPath::Prim(field))
            .unwrap();
        assert!(
            handle
                .block_field_relationship(&mut edit, "density")
                .unwrap()
        );
        let applied = {
            let transaction = edit.finish();
            live.apply(&mut store, &transaction)
        }
        .unwrap();
        assert_eq!(
            live.stage()
                .resolve_property_declaration(path, name)
                .unwrap()
                .custom,
            before.custom
        );
        assert_eq!(
            Volume::new(&Scene::new(live.stage(), &store), path)
                .unwrap()
                .field_path("density"),
            None
        );
        live.apply(&mut store, &applied.inverse).unwrap();
    }
}
#[test]
fn reference_edit_target_maps_relationship_and_target_paths_and_undo() {
    let (mut store, mut live) = support::scene(
        "#usda 1.0\ndef Volume \"Asset\" {\n custom rel field:density = </Asset/Field> (doc = \"preserve\")\n def Scope \"Field\" {}\n}\ndef Volume \"Mounted\" (references = </Asset>) {}\n",
    );
    let path = store.path("/Mounted");
    let field = store.path("/Mounted/Field");
    let graph = live.stage().explain_prim_graph(path).unwrap();
    let node = graph
        .depth_first()
        .into_iter()
        .find(|id| graph.node(*id).unwrap().arc_kind() == layerstack::ArcKind::References)
        .unwrap();
    let target = EditTarget::for_node(live.stage(), path, node).unwrap();
    let before = store.layers[&LayerId(1)].clone();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    let handle = layerstack_schemas::usd_vol::VolumeEdit::new(&edit, path).unwrap();
    handle
        .create_field_relationship(&mut edit, "density", TargetPath::Prim(field))
        .unwrap();
    handle
        .create_field_relationship(&mut edit, "new", TargetPath::Prim(field))
        .unwrap();
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let source = store.path("/Asset");
    let source_field = store.path("/Asset/Field");
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        Volume::new(&scene, path).unwrap().field_path("new"),
        Some(field)
    );
    assert_eq!(
        Volume::new(&scene, source).unwrap().field_path("new"),
        Some(source_field)
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(store.layers[&LayerId(1)], before);
}
