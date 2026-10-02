// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Collision-group policy matches C++, including merged and inverted groups.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{LayerId, TargetPath, edit::EditTarget};
use layerstack_schemas::{
    Scene, SchemaEdit,
    physics::{CollisionGroupError, compute_collision_group_table},
    usd_physics::PhysicsCollisionGroup,
};
#[test]
fn filtering_matches_cpp_and_snapshots_follow_explicit_recomputation() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/collision_groups.json")).unwrap();
    let (mut store, mut live) = support::scene(include_str!("../fixtures/collision_groups.usda"));
    let a = store.path("/World/A");
    let b = store.path("/World/B");
    let collider = store.path("/World/Collider");
    let unknown = store.path("/Unknown");
    let scene = Scene::new(live.stage(), &store);
    let table = compute_collision_group_table(&scene).unwrap();
    let names: Vec<_> = table
        .groups()
        .iter()
        .map(|&p| store.paths.resolve(p).display(&store.tokens).to_string())
        .collect();
    assert_eq!(serde_json::json!(names), oracle["groups"]);
    for (a, &path_a) in table.groups().iter().enumerate() {
        for (b, &path_b) in table.groups().iter().enumerate() {
            assert_eq!(
                table.is_collision_enabled(path_a, path_b),
                oracle["enabled"][a][b]
            );
            assert_eq!(table.is_collision_enabled_at(a, b), oracle["enabled"][a][b]);
        }
    }
    assert_eq!(table.merged_group_count(), 5);
    assert_eq!(table.stored_pair_count(), 15);
    assert!(table.is_collision_enabled(a, unknown));
    assert!(table.is_collision_enabled_at(999, 0));
    let group = PhysicsCollisionGroup::new(&scene, a).unwrap();
    assert!(
        group
            .colliders_collection()
            .membership_query()
            .is_included(&scene, TargetPath::Prim(collider))
            .is_included()
    );
    let group = group.edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    group.set_filtered_groups(&mut edit, &[]);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    assert!(!table.is_collision_enabled(a, b));
    assert!(
        compute_collision_group_table(&Scene::new(live.stage(), &store))
            .unwrap()
            .is_collision_enabled(a, b)
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(
        compute_collision_group_table(&Scene::new(live.stage(), &store)).unwrap(),
        table
    );
}
#[test]
fn invalid_filter_targets_return_no_partial_table() {
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef PhysicsCollisionGroup \"Group\" {\nrel physics:filteredGroups = </Missing>\n}",
    );
    let group = store.path("/Group");
    let missing = store.path("/Missing");
    assert_eq!(
        compute_collision_group_table(&Scene::new(live.stage(), &store)),
        Err(CollisionGroupError::InvalidFilteredGroup {
            group,
            target: TargetPath::Prim(missing)
        })
    );
}
