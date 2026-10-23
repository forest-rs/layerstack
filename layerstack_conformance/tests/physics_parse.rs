// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The physics scene parse against OpenUSD's `LoadUsdPhysicsFromRange`.
//!
//! `scripts/physics_parse_oracle.py` parses each scene in
//! `fixtures/physics_parse` with C++ 26.08 and records rigid bodies, shapes
//! (kind, custom token, body, collision groups, materials), joints and
//! collision groups (merged groups, filters, inversion). `custom.usda` is
//! parsed twice, the second time with custom tokens for a shape API, a
//! shape type and an instancer type, as `CustomPhysicsTypes` takes them.
//! The test captures each scene with `PhysicsSceneSnapshot::capture` and
//! compares. Records
//! layerstack keeps as explicit errors where C++ drops the prim (a collider
//! of a type with no physics geometry) are compared as absent.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{Stage, StageOptions, TargetPath};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{
    Scene,
    physics_scene::{
        CustomPhysicsTypes, JointKind, PhysicsSceneError, PhysicsSceneOptions,
        PhysicsSceneSnapshot, ShapeGeometry,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};

const ORACLE: &str = include_str!("../fixtures/physics_parse/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    scenes: BTreeMap<String, BTreeMap<String, BTreeMap<String, Value>>>,
}

fn kind(geometry: &ShapeGeometry) -> (String, Option<String>) {
    match geometry {
        ShapeGeometry::Sphere { .. } => ("sphere".into(), None),
        ShapeGeometry::Box { .. } => ("cube".into(), None),
        ShapeGeometry::Axial { kind, .. } => (kind.to_lowercase(), None),
        ShapeGeometry::Plane { .. } => ("plane".into(), None),
        ShapeGeometry::Mesh { .. } => ("mesh".into(), None),
        ShapeGeometry::SpherePoints { .. } => ("sphere_points".into(), None),
        ShapeGeometry::Custom { token } => ("custom".into(), Some(token.to_string())),
    }
}

/// The snapshot in the oracle's form.
fn parse(scene_name: &str) -> BTreeMap<String, BTreeMap<String, Value>> {
    let file = scene_name.split('+').next().unwrap();
    let mut loaded = load_entry_usda(&workspace_root().join(format!(
        "layerstack_conformance/fixtures/physics_parse/{file}"
    )));
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&loaded.store.tokens))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);
    let snapshot = PhysicsSceneSnapshot::capture(
        &scene,
        PhysicsSceneOptions {
            includes: &[store.path("/")],
            excludes: &[],
            simulation_owners: None,
            // As the oracle's `CustomUsdPhysicsTokens` for `+tokens`.
            custom: if scene_name.ends_with("+tokens") {
                CustomPhysicsTypes {
                    shapes: &["MyConvexAPI", "MyShape"],
                    joints: &[],
                    instancers: &["MyInstancer"],
                }
            } else {
                CustomPhysicsTypes::default()
            },
        },
    )
    .unwrap();
    let text = |path| store.paths.display(path, &store.tokens).to_string();
    let sorted = |mut values: Vec<String>| {
        values.sort();
        values
    };
    let target = |target: &TargetPath| target.display(&store.paths, &store.tokens).to_string();
    let mut out: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for key in ["rigid_bodies", "shapes", "joints", "collision_groups"] {
        out.insert(key.into(), BTreeMap::new());
    }
    for body in &snapshot.rigid_bodies {
        let body_desc = body.descriptor.as_ref().unwrap();
        out.get_mut("rigid_bodies").unwrap().insert(
            text(body.path),
            json!({"collisions": sorted(body_desc.collisions.iter().map(|&p| text(p)).collect())}),
        );
    }
    for shape in &snapshot.shapes {
        let desc = match &shape.descriptor {
            Ok(desc) => desc,
            Err(PhysicsSceneError::UnsupportedShape) => continue,
            Err(error) => panic!("{}: {error:?}", text(shape.path)),
        };
        let (kind, token) = kind(&desc.geometry);
        out.get_mut("shapes").unwrap().insert(
            text(shape.path),
            json!({
                "kind": kind,
                "token": token,
                "rigid_body": desc.rigid_body.map(text),
                "collision_groups": sorted(desc.collision_groups.iter().map(|&p| text(p)).collect()),
                "materials": desc.materials.as_ref().unwrap().iter().map(|&p| text(p)).collect::<Vec<_>>(),
            }),
        );
    }
    for group in &snapshot.collision_groups {
        let desc = group.descriptor.as_ref().unwrap();
        out.get_mut("collision_groups").unwrap().insert(
            text(group.path),
            json!({
                "merged_groups": sorted(desc.merged_groups.iter().map(|&p| text(p)).collect()),
                "filtered_groups": sorted(desc.filtered_groups.iter().map(target).collect()),
                "invert": desc.invert_filtered_groups,
            }),
        );
    }
    for joint in &snapshot.joints {
        let kind = match &joint.descriptor.as_ref().unwrap().kind {
            JointKind::Fixed => "fixed",
            JointKind::Revolute { .. } => "revolute",
            JointKind::Prismatic { .. } => "prismatic",
            JointKind::Spherical { .. } => "spherical",
            JointKind::Distance { .. } => "distance",
            JointKind::Custom => "custom",
            JointKind::D6 { .. } => "d6",
        };
        out.get_mut("joints")
            .unwrap()
            .insert(text(joint.path), json!({ "kind": kind }));
    }
    out
}

#[test]
fn physics_parse_matches_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    for (name, expected) in &oracle.scenes {
        let actual = parse(name);
        for (key, records) in expected {
            assert_eq!(&actual[key], records, "{name} {key}");
        }
    }
}
