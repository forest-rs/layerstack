// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{
    SchemaEdit, XformOpPrecision,
    usd_geom::{Cube, Sphere, Xform},
    usd_physics::{PhysicsArticulationRootApi, PhysicsFixedJoint},
};
use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions, edit::EditTarget};
fn setup() -> (InMemoryStore, LiveStage) {
    setup_with_units(0.01)
}
fn setup_with_units(units: f64) -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let mut layer = Layer::new(LayerId(1));
    layer.set_metadata(store.tokens.intern("metersPerUnit"), Value::Double(units));
    store.insert_layer(layer);
    let schemas = Arc::new(crate::openusd(&mut store.tokens));
    let stage = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    (store, stage)
}
#[test]
fn captures_scaled_shapes_bodies_scene_defaults_and_graph() {
    let (mut store, mut stage) = setup();
    let root = store.path("/World");
    let a = store.path("/World/A");
    let b = store.path("/World/B");
    let cube = store.path("/World/A/Shape");
    let sphere = store.path("/World/Static");
    let joint = store.path("/World/Joint");
    let physics = store.path("/World/Scene");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Xform::define(&mut edit, root);
    Xform::define(&mut edit, a);
    Xform::define(&mut edit, b);
    PhysicsRigidBodyApi::apply(&mut edit, a).unwrap();
    PhysicsRigidBodyApi::apply(&mut edit, b).unwrap();
    let c = Cube::define(&mut edit, cube);
    c.set_size(&mut edit, 4.);
    c.add_scale_op(&mut edit, XformOpPrecision::Double)
        .unwrap()
        .set(&mut edit, [2., 3., 4.])
        .unwrap();
    PhysicsCollisionApi::apply(&mut edit, cube).unwrap();
    Sphere::define(&mut edit, sphere).set_radius(&mut edit, 2.);
    PhysicsCollisionApi::apply(&mut edit, sphere).unwrap();
    let j = PhysicsFixedJoint::define(&mut edit, joint);
    j.set_body0(&mut edit, &[TargetPath::Prim(a)]);
    j.set_body1(&mut edit, &[TargetPath::Prim(b)]);
    PhysicsScene::define(&mut edit, physics);
    PhysicsArticulationRootApi::apply(&mut edit, root).unwrap();
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let snapshot = PhysicsSceneSnapshot::capture(
        &Scene::new(stage.stage(), &store),
        PhysicsSceneOptions {
            includes: &[root],
            excludes: &[],
            simulation_owners: None,
        },
    )
    .unwrap();
    assert_eq!(snapshot.rigid_bodies.len(), 2);
    assert_eq!(snapshot.shapes.len(), 2);
    assert_eq!(snapshot.joints.len(), 1);
    let body = snapshot
        .rigid_bodies
        .iter()
        .find(|r| r.path == a)
        .unwrap()
        .descriptor
        .as_ref()
        .unwrap();
    assert_eq!(body.collisions, [cube]);
    let shape = snapshot
        .shapes
        .iter()
        .find(|r| r.path == cube)
        .unwrap()
        .descriptor
        .as_ref()
        .unwrap();
    assert_eq!(shape.rigid_body, Some(a));
    assert_eq!(
        shape.geometry,
        ShapeGeometry::Box {
            half_extents: [4., 6., 8.]
        }
    );
    let scene = snapshot.scenes[0].descriptor.as_ref().unwrap();
    assert_eq!(scene.gravity_direction, [0., -1., 0.]);
    assert_eq!(scene.gravity_magnitude, 981.00006); // Float conversion before division.
    let articulation = snapshot.articulations[0].descriptor.as_ref().unwrap();
    assert_eq!(articulation.roots, [a]);
    assert_eq!(articulation.bodies, [Some(a), Some(b)]);
    assert_eq!(articulation.joints, [joint]);
    drop(stage);
    drop(store);
    assert!(matches!(
        snapshot.shapes[1].descriptor.as_ref().unwrap().geometry,
        ShapeGeometry::Sphere { radius: 2. }
    ));
}
#[test]
fn scope_exclusions_and_owner_filter_follow_body_for_attached_shapes() {
    let (mut store, mut stage) = setup();
    let root = store.path("/World");
    let a = store.path("/World/A");
    let b = store.path("/World/B");
    let shape = store.path("/World/A/Shape");
    let static_shape = store.path("/World/Static");
    let joint = store.path("/World/Joint");
    let s1 = store.path("/S1");
    let s2 = store.path("/S2");
    let material = store.path("/World/Material");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Xform::define(&mut edit, root);
    Xform::define(&mut edit, a);
    Xform::define(&mut edit, b);
    Sphere::define(&mut edit, shape);
    Sphere::define(&mut edit, static_shape);
    PhysicsScene::define(&mut edit, s1);
    PhysicsScene::define(&mut edit, s2);
    PhysicsRigidBodyApi::apply(&mut edit, a)
        .unwrap()
        .set_simulation_owner(&mut edit, &[TargetPath::Prim(s1)]);
    PhysicsRigidBodyApi::apply(&mut edit, b)
        .unwrap()
        .set_simulation_owner(&mut edit, &[TargetPath::Prim(s2)]);
    PhysicsCollisionApi::apply(&mut edit, shape)
        .unwrap()
        .set_simulation_owner(&mut edit, &[TargetPath::Prim(s2)]);
    PhysicsCollisionApi::apply(&mut edit, static_shape).unwrap();
    let j = PhysicsFixedJoint::define(&mut edit, joint);
    j.set_body0(&mut edit, &[TargetPath::Prim(a)]);
    j.set_body1(&mut edit, &[TargetPath::Prim(b)]);
    edit.define(material, "Material");
    PhysicsMaterialApi::apply(&mut edit, material).unwrap();
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(stage.stage(), &store);
    let snapshot = PhysicsSceneSnapshot::capture(
        &scene,
        PhysicsSceneOptions {
            includes: &[root, root, s1, s2],
            excludes: &[],
            simulation_owners: Some(&[SimulationOwner::Scene(s1)]),
        },
    )
    .unwrap();
    assert_eq!(snapshot.scenes.len(), 1);
    assert_eq!(snapshot.scenes[0].path, s1);
    assert_eq!(snapshot.rigid_bodies.len(), 1);
    assert_eq!(snapshot.shapes.len(), 1);
    assert_eq!(snapshot.shapes[0].path, shape);
    assert!(snapshot.joints.is_empty());
    assert_eq!(snapshot.materials.len(), 1);
    let excluded = PhysicsSceneSnapshot::capture(
        &scene,
        PhysicsSceneOptions {
            includes: &[root],
            excludes: &[a],
            simulation_owners: None,
        },
    )
    .unwrap();
    assert!(excluded.shapes.iter().all(|s| s.path != shape));
    assert_eq!(excluded.rigid_bodies.len(), 1);
    let direct = PhysicsSceneSnapshot::capture(
        &scene,
        PhysicsSceneOptions {
            includes: &[root, shape],
            excludes: &[a],
            simulation_owners: None,
        },
    )
    .unwrap();
    let direct_shape = direct
        .shapes
        .iter()
        .find(|s| s.path == shape)
        .unwrap()
        .descriptor
        .as_ref()
        .unwrap();
    assert_eq!(direct_shape.rigid_body, None); // Explicit include bypasses ancestor pruning.
    assert_eq!(
        PhysicsSceneSnapshot::capture(
            &scene,
            PhysicsSceneOptions {
                includes: &[],
                excludes: &[],
                simulation_owners: None
            }
        ),
        Err(PhysicsSceneError::EmptyIncludes)
    );
}
#[test]
fn unsupported_collisions_and_nested_articulations_retain_errors() {
    let (mut store, mut stage) = setup();
    let root = store.path("/Root");
    let bad = store.path("/Root/Bad");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Xform::define(&mut edit, root);
    Xform::define(&mut edit, bad);
    PhysicsCollisionApi::apply(&mut edit, bad).unwrap();
    PhysicsArticulationRootApi::apply(&mut edit, root).unwrap();
    PhysicsArticulationRootApi::apply(&mut edit, bad).unwrap();
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let snapshot = PhysicsSceneSnapshot::capture(
        &Scene::new(stage.stage(), &store),
        PhysicsSceneOptions {
            includes: &[root],
            excludes: &[],
            simulation_owners: None,
        },
    )
    .unwrap();
    assert_eq!(
        snapshot.shapes[0].descriptor,
        Err(PhysicsSceneError::UnsupportedShape)
    );
    assert_eq!(
        snapshot.articulations[0].descriptor,
        Err(PhysicsSceneError::EmptyArticulation)
    );
    assert_eq!(
        snapshot.articulations[1].descriptor,
        Err(PhysicsSceneError::NestedArticulation)
    );
}

#[test]
fn joint_limits_drives_and_descendant_anchor_conversion() {
    use crate::usd_physics::{
        PhysicsDistanceJoint, PhysicsDriveApi, PhysicsDriveApiDriveType, PhysicsJoint,
        PhysicsLimitApi, PhysicsPrismaticJoint, PhysicsRevoluteJoint, PhysicsSphericalJoint,
    };
    let (mut store, mut stage) = setup();
    let root = store.path("/Root");
    let body = store.path("/Root/Body");
    let anchor = store.path("/Root/Body/Anchor");
    let fixed = store.path("/Root/Fixed");
    let revolute = store.path("/Root/Revolute");
    let prismatic = store.path("/Root/Prismatic");
    let distance = store.path("/Root/Distance");
    let spherical = store.path("/Root/Spherical");
    let d6 = store.path("/Root/D6");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Xform::define(&mut edit, root);
    let b = Xform::define(&mut edit, body);
    b.add_scale_op(&mut edit, XformOpPrecision::Double)
        .unwrap()
        .set(&mut edit, [2., 3., 4.])
        .unwrap();
    PhysicsRigidBodyApi::apply(&mut edit, body).unwrap();
    Xform::define(&mut edit, anchor)
        .add_translate_op(&mut edit, XformOpPrecision::Double)
        .unwrap()
        .set(&mut edit, [1., 2., 3.])
        .unwrap();
    let j = PhysicsFixedJoint::define(&mut edit, fixed);
    j.set_body0(&mut edit, &[TargetPath::Prim(anchor)]);
    j.set_local_pos0(&mut edit, [0.5; 3]);
    PhysicsRevoluteJoint::define(&mut edit, revolute).set_lower_limit(&mut edit, -30.);
    PhysicsPrismaticJoint::define(&mut edit, prismatic).set_lower_limit(&mut edit, -30.);
    PhysicsDistanceJoint::define(&mut edit, distance).set_min_distance(&mut edit, 2.);
    let j = PhysicsSphericalJoint::define(&mut edit, spherical);
    j.set_cone_angle0_limit(&mut edit, 20.);
    j.set_cone_angle1_limit(&mut edit, 40.);
    PhysicsJoint::define(&mut edit, d6);
    let l = PhysicsLimitApi::apply(&mut edit, d6, "transX").unwrap();
    l.set_low(&mut edit, 1.);
    l.set_high(&mut edit, -1.);
    let drive = PhysicsDriveApi::apply(&mut edit, d6, "rotZ").unwrap();
    drive.set_target_position(&mut edit, 12.);
    drive.set_drive_type(&mut edit, PhysicsDriveApiDriveType::Acceleration);
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(stage.stage(), &store);
    let snapshot = PhysicsSceneSnapshot::capture(
        &scene,
        PhysicsSceneOptions {
            includes: &[root],
            excludes: &[],
            simulation_owners: None,
        },
    )
    .unwrap();
    let find = |path| {
        snapshot
            .joints
            .iter()
            .find(|j| j.path == path)
            .unwrap()
            .descriptor
            .as_ref()
            .unwrap()
    };
    let fixed = find(fixed);
    assert_eq!(fixed.bodies, [Some(body), None]);
    assert_eq!(fixed.local_poses[0].position, [3., 7.5, 14.]);
    assert!(matches!(
        find(revolute).kind,
        JointKind::Revolute {
            limit: JointLimit { enabled: false, .. },
            ..
        }
    ));
    assert!(matches!(
        find(prismatic).kind,
        JointKind::Prismatic {
            limit: JointLimit { enabled: true, .. },
            ..
        }
    ));
    assert!(matches!(
        find(distance).kind,
        JointKind::Distance {
            minimum: 2.,
            minimum_enabled: true,
            maximum_enabled: false,
            ..
        }
    ));
    assert!(matches!(
        find(spherical).kind,
        JointKind::Spherical {
            limit: JointLimit {
                enabled: true,
                lower: 20.,
                upper: 40.
            },
            ..
        }
    ));
    let JointKind::D6 { limits, drives } = &find(d6).kind else {
        panic!("generic joint must be D6");
    };
    assert_eq!(
        limits,
        &[(
            JointDof::TranslationX,
            JointLimit {
                enabled: true,
                lower: 1.,
                upper: -1.
            }
        )]
    );
    assert_eq!(drives[0].0, JointDof::RotationZ);
    assert_eq!(drives[0].1.target_position, 12.);
    assert!(drives[0].1.acceleration);
}

#[test]
fn mesh_points_group_and_material_capture_are_owned_and_checked() {
    use crate::usd::CollectionApi;
    use crate::usd_geom::{Mesh, Points};
    let (mut store, mut stage) = setup();
    let root = store.path("/Root");
    let mesh = store.path("/Root/Mesh");
    let points = store.path("/Root/Points");
    let material = store.path("/Root/Material");
    let group = store.path("/Root/Group");
    let bad = store.path("/Root/BadMesh");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Xform::define(&mut edit, root);
    let m = Mesh::define(&mut edit, mesh);
    m.set_points(&mut edit, &[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]);
    m.set_face_vertex_counts(&mut edit, &[3]);
    m.set_face_vertex_indices(&mut edit, &[0, 1, 2]);
    PhysicsCollisionApi::apply(&mut edit, mesh).unwrap();
    let p = Points::define(&mut edit, points);
    p.set_points(&mut edit, &[[0., 0., 0.], [1., 1., 1.]]);
    p.set_widths(&mut edit, &[2., 4.]);
    PhysicsCollisionApi::apply(&mut edit, points).unwrap();
    edit.define(material, "Material");
    PhysicsMaterialApi::apply(&mut edit, material)
        .unwrap()
        .set_density(&mut edit, 5.);
    #[cfg(feature = "usd-shade")]
    edit.set_targets(
        mesh,
        "material:binding:physics",
        &[TargetPath::Prim(material)],
    );
    PhysicsCollisionGroup::define(&mut edit, group);
    CollectionApi::apply(&mut edit, group, "colliders")
        .unwrap()
        .set_includes(
            &mut edit,
            &[TargetPath::Prim(mesh), TargetPath::Prim(points)],
        );
    let b = Mesh::define(&mut edit, bad);
    b.set_points(&mut edit, &[[0.; 3]]);
    b.set_face_vertex_counts(&mut edit, &[3]);
    b.set_face_vertex_indices(&mut edit, &[0]);
    PhysicsCollisionApi::apply(&mut edit, bad).unwrap();
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let snapshot = PhysicsSceneSnapshot::capture(
        &Scene::new(stage.stage(), &store),
        PhysicsSceneOptions {
            includes: &[root],
            excludes: &[],
            simulation_owners: None,
        },
    )
    .unwrap();
    let mesh_shape = snapshot
        .shapes
        .iter()
        .find(|s| s.path == mesh)
        .unwrap()
        .descriptor
        .as_ref()
        .unwrap();
    assert_eq!(mesh_shape.collision_groups, [group]);
    assert!(
        matches!(&mesh_shape.geometry,ShapeGeometry::Mesh {face_vertex_counts,face_vertex_indices,..} if face_vertex_counts==&[3] && face_vertex_indices==&[0,1,2])
    );
    #[cfg(feature = "usd-shade")]
    assert_eq!(mesh_shape.materials, Some(alloc::vec![material]));
    #[cfg(not(feature = "usd-shade"))]
    assert_eq!(mesh_shape.materials, None);
    let point_shape = snapshot
        .shapes
        .iter()
        .find(|s| s.path == points)
        .unwrap()
        .descriptor
        .as_ref()
        .unwrap();
    assert_eq!(
        point_shape.geometry,
        ShapeGeometry::SpherePoints {
            spheres: alloc::vec![
                SpherePoint {
                    center: [0.; 3],
                    radius: 1.
                },
                SpherePoint {
                    center: [1.; 3],
                    radius: 2.
                }
            ]
        }
    );
    assert_eq!(
        snapshot
            .shapes
            .iter()
            .find(|s| s.path == bad)
            .unwrap()
            .descriptor,
        Err(PhysicsSceneError::InvalidAttribute("meshTopology"))
    );
    assert_eq!(
        snapshot.materials[0].descriptor.as_ref().unwrap().density,
        5.
    );
}

#[test]
fn mixed_material_api_order_matches_native_parser_classification() {
    let (mut store, mut stage) = setup();
    let body_first = store.path("/BodyFirst");
    let material_first = store.path("/MaterialFirst");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    for path in [body_first, material_first] {
        Sphere::define(&mut edit, path);
        if path == material_first {
            PhysicsMaterialApi::apply(&mut edit, path).unwrap();
        }
        PhysicsRigidBodyApi::apply(&mut edit, path).unwrap();
        PhysicsCollisionApi::apply(&mut edit, path).unwrap();
        if path == body_first {
            PhysicsMaterialApi::apply(&mut edit, path).unwrap();
        }
    }
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let snapshot = PhysicsSceneSnapshot::capture(
        &Scene::new(stage.stage(), &store),
        PhysicsSceneOptions {
            includes: &[body_first, material_first],
            excludes: &[],
            simulation_owners: None,
        },
    )
    .unwrap();
    assert_eq!(
        snapshot
            .materials
            .iter()
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        [material_first]
    );
    assert_eq!(
        snapshot
            .rigid_bodies
            .iter()
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        [body_first]
    );
    assert_eq!(
        snapshot.shapes.iter().map(|r| r.path).collect::<Vec<_>>(),
        [body_first]
    );
    assert_eq!(
        snapshot.shapes[0].descriptor.as_ref().unwrap().rigid_body,
        Some(body_first)
    );
}

#[test]
fn world_scaled_shape_dimension_overflow_is_explicit() {
    use crate::usd_geom::{Capsule, Points};
    let (mut store, mut stage) = setup();
    let sphere = store.path("/Sphere");
    let cube = store.path("/Cube");
    let capsule = store.path("/Capsule");
    let points = store.path("/Points");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let s = Sphere::define(&mut edit, sphere);
    s.set_radius(&mut edit, 1e30);
    s.add_scale_op(&mut edit, XformOpPrecision::Double)
        .unwrap()
        .set(&mut edit, [1e20; 3])
        .unwrap();
    let c = Cube::define(&mut edit, cube);
    c.set_size(&mut edit, 1e30);
    c.add_scale_op(&mut edit, XformOpPrecision::Double)
        .unwrap()
        .set(&mut edit, [1e20; 3])
        .unwrap();
    let c = Capsule::define(&mut edit, capsule);
    c.set_radius(&mut edit, 1e30);
    c.add_scale_op(&mut edit, XformOpPrecision::Double)
        .unwrap()
        .set(&mut edit, [1e20; 3])
        .unwrap();
    let p = Points::define(&mut edit, points);
    p.set_points(&mut edit, &[[0.; 3]]);
    p.set_widths(&mut edit, &[1e30]);
    p.add_scale_op(&mut edit, XformOpPrecision::Double)
        .unwrap()
        .set(&mut edit, [1e20; 3])
        .unwrap();
    for path in [sphere, cube, capsule, points] {
        PhysicsCollisionApi::apply(&mut edit, path).unwrap();
    }
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let snapshot = PhysicsSceneSnapshot::capture(
        &Scene::new(stage.stage(), &store),
        PhysicsSceneOptions {
            includes: &[sphere, cube, capsule, points],
            excludes: &[],
            simulation_owners: None,
        },
    )
    .unwrap();
    for (record, name) in snapshot
        .shapes
        .iter()
        .zip(["radius", "size", "radius", "widths"])
    {
        assert_eq!(
            record.descriptor,
            Err(PhysicsSceneError::InvalidAttribute(name))
        );
    }
}

#[test]
fn large_finite_rotation_and_gravity_are_normalized_without_overflow() {
    let (mut store, mut stage) = setup();
    let physics = store.path("/Scene");
    let joint = store.path("/Joint");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    PhysicsScene::define(&mut edit, physics).set_gravity_direction(&mut edit, [1e30, 0., 0.]);
    PhysicsFixedJoint::define(&mut edit, joint).set_local_rot0(&mut edit, [1e30, 0., 0., 0.]);
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let snapshot = PhysicsSceneSnapshot::capture(
        &Scene::new(stage.stage(), &store),
        PhysicsSceneOptions {
            includes: &[physics, joint],
            excludes: &[],
            simulation_owners: None,
        },
    )
    .unwrap();
    assert_eq!(
        snapshot.scenes[0]
            .descriptor
            .as_ref()
            .unwrap()
            .gravity_direction,
        [1., 0., 0.]
    );
    assert_eq!(
        snapshot.joints[0].descriptor.as_ref().unwrap().local_poses[0].orientation,
        [1., 0., 0., 0.]
    );
}

#[test]
fn units_that_underflow_physics_descriptor_precision_are_errors() {
    let (mut store, mut stage) = setup_with_units(1e-100);
    let physics = store.path("/Scene");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    PhysicsScene::define(&mut edit, physics);
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let snapshot = PhysicsSceneSnapshot::capture(
        &Scene::new(stage.stage(), &store),
        PhysicsSceneOptions {
            includes: &[physics],
            excludes: &[],
            simulation_owners: None,
        },
    )
    .unwrap();
    assert_eq!(
        snapshot.scenes[0].descriptor,
        Err(PhysicsSceneError::InvalidAttribute("metersPerUnit"))
    );
}

#[test]
fn articulation_joint_root_ties_use_lexical_body_order_and_errors_survive_owners() {
    let (mut store, mut stage) = setup();
    let root = store.path("/Root");
    let b = store.path("/Root/B");
    let a = store.path("/Root/A");
    let jb = store.path("/Root/JB");
    let ja = store.path("/Root/JA");
    let ab = store.path("/Root/AB");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Xform::define(&mut edit, root);
    // Namespace traversal encounters B first; C++ ordered map chooses A's joint.
    for path in [b, a] {
        Xform::define(&mut edit, path);
        PhysicsRigidBodyApi::apply(&mut edit, path).unwrap();
    }
    PhysicsArticulationRootApi::apply(&mut edit, root).unwrap();
    for (path, body) in [(jb, b), (ja, a)] {
        PhysicsFixedJoint::define(&mut edit, path).set_body0(&mut edit, &[TargetPath::Prim(body)]);
    }
    let j = PhysicsFixedJoint::define(&mut edit, ab);
    j.set_body0(&mut edit, &[TargetPath::Prim(a)]);
    j.set_body1(&mut edit, &[TargetPath::Prim(b)]);
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let snapshot = PhysicsSceneSnapshot::capture(
        &Scene::new(stage.stage(), &store),
        PhysicsSceneOptions {
            includes: &[root],
            excludes: &[],
            simulation_owners: None,
        },
    )
    .unwrap();
    assert_eq!(
        snapshot.articulations[0].descriptor.as_ref().unwrap().roots,
        [ja]
    );
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    PhysicsArticulationRootApi::apply(&mut edit, b).unwrap();
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let snapshot = PhysicsSceneSnapshot::capture(
        &Scene::new(stage.stage(), &store),
        PhysicsSceneOptions {
            includes: &[root],
            excludes: &[],
            simulation_owners: Some(&[SimulationOwner::Default]),
        },
    )
    .unwrap();
    assert_eq!(
        snapshot
            .articulations
            .iter()
            .find(|r| r.path == b)
            .unwrap()
            .descriptor,
        Err(PhysicsSceneError::NestedArticulation)
    );
}
