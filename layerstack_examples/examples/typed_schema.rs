// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed schema views: reading and authoring OpenUSD's schemas as Rust.
//!
//! `layerstack_schemas` generates a view of every OpenUSD schema over a
//! composed stage, and an edit handle that authors it. This example:
//!
//! 1. authors a robot in a base layer through edit handles: a `Mesh` arm, a
//!    `SphereLight` with its built-in `LightAPI`, and a `CollectionAPI`
//!    instance naming the light;
//! 2. overrides the arm in a stronger shot layer, through the same handles
//!    and another edit target;
//! 3. reads the composed result through the views: the strongest opinion,
//!    the schema fallback where nothing is authored, enums for
//!    `allowedTokens`, and inherited properties through `Deref`;
//! 4. computes what inherits down namespace: the arm's purpose, which the
//!    shot layer authors on the robot, its visibility and its world
//!    transform;
//! 5. drops to the raw resolved value, with its provenance, for what the
//!    views do not show.

use std::sync::Arc;

use layerstack::edit::EditTarget;
use layerstack::{
    InMemoryStore, InterpolationType, Layer, LayerId, LiveStage, StageOptions, SublayerEntry,
    TargetPath,
};
use layerstack_schemas::usd::CollectionApi;
use layerstack_schemas::usd_geom::{
    Gprim, ImageablePurpose, Mesh, MeshEdit, MeshSubdivisionScheme, Xform, XformEdit,
};
use layerstack_schemas::usd_lux::SphereLight;
use layerstack_schemas::{Scene, SchemaEdit, Time};

fn main() {
    // The shot layer (1) is the root; the base layer (2) is its weaker
    // sublayer.
    let mut store = InMemoryStore::default();
    let mut shot = Layer::new(LayerId(1));
    shot.sublayers = vec![SublayerEntry::new(LayerId(2))];
    store.insert_layer(shot);
    store.insert_layer(Layer::new(LayerId(2)));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        with_provenance: true,
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);

    let robot = store.path("/Robot");
    let arm = store.path("/Robot/Arm");
    let key = store.path("/Robot/Key");
    let key_target = TargetPath::prim(key);

    // --- The base layer: define the robot. ---
    let base = EditTarget::for_layer(LayerId(2));
    let mut edit = SchemaEdit::new(live.stage(), &mut store, base);
    Xform::define(&mut edit, robot);
    Mesh::define(&mut edit, arm)
        .set_face_vertex_counts(&mut edit, &[4])
        .set_face_vertex_indices(&mut edit, &[0, 1, 2, 3])
        .set_points(
            &mut edit,
            &[
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 2.0, 0.0],
                [0.0, 2.0, 0.0],
            ],
        )
        .set_display_color(&mut edit, &[[0.5, 0.5, 0.5]]);
    SphereLight::define(&mut edit, key)
        .set_radius(&mut edit, 0.25)
        .light_api()
        .set_intensity(&mut edit, 800.0)
        .set_intensity_at(&mut edit, 24.0, 1200.0);
    // A multiple-apply schema is applied with an instance name, which
    // names its properties (`collection:lights:includes`).
    CollectionApi::apply(&mut edit, robot, "lights")
        .expect("any prim may have a collection")
        .set_includes(&mut edit, &[key_target]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction)
        .expect("the base edits apply");

    // --- The shot layer: override the arm. ---
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    // Setters return the handle of the schema that defines the property,
    // so chain a derived schema's setters before its bases'.
    MeshEdit::new(&edit, arm)
        .expect("the arm is on the stage")
        .set_subdivision_scheme(&mut edit, MeshSubdivisionScheme::None)
        .set_display_color(&mut edit, &[[1.0, 0.0, 0.0]]);
    // Purpose inherits: the whole robot is for final renders.
    XformEdit::new(&edit, robot)
        .expect("the robot is on the stage")
        .set_purpose(&mut edit, ImageablePurpose::Render);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction)
        .expect("the shot edits apply");

    // --- Read the composed robot through the views. ---
    let scene = Scene::new(live.stage(), &store);
    let mesh = Mesh::new(&scene, arm).expect("the arm is a mesh");
    println!("/Robot/Arm");
    // The shot layer's opinion is the strongest.
    println!("  displayColor      = {:?}", mesh.display_color());
    // Only the base layer authors the points.
    println!("  points            = {:?}", mesh.points());
    println!("  subdivisionScheme = {:?}", mesh.subdivision_scheme());
    // Nothing authors these: the schema's fallbacks. `orientation` is a
    // `Gprim` property, which the mesh view reaches through `Deref`.
    println!("  orientation       = {:?}", mesh.orientation());
    println!("  purpose           = {:?}", mesh.purpose());
    // Any mesh is a `Gprim`, so the abstract view reads it too.
    let gprim = Gprim::new(&scene, arm).expect("a mesh is a gprim");
    println!("  doubleSided       = {:?}", gprim.double_sided());

    // What inherits down namespace. The arm authors no purpose; the robot's
    // is inherited, and the result says where it is authored.
    let info = mesh.compute_purpose_info();
    println!(
        "  computed purpose  = {:?} (authored on {:?})",
        info.purpose,
        info.authored_on
            .map(|p| store.paths.display(p, &store.tokens))
    );
    println!(
        "  visibility        = {:?}",
        mesh.compute_visibility(Time::Default)
    );
    println!(
        "  guide visibility  = {:?}",
        mesh.compute_effective_visibility(&ImageablePurpose::Guide, Time::Default)
    );
    // Nothing authors transform ops here, so the world transform is the
    // identity; `world_transforms` shows transforms in motion.
    println!(
        "  local-to-world    = {:?}",
        mesh.compute_local_to_world(Time::at(24.0))
    );

    let light = SphereLight::new(&scene, key).expect("the key is a sphere light");
    println!("/Robot/Key");
    println!("  radius            = {:?}", light.radius());
    println!("  intensity         = {:?}", light.light_api().intensity());
    println!(
        "  intensity at 24   = {:?}",
        light
            .light_api()
            .intensity_at(24.0, InterpolationType::Linear)
    );

    for collection in CollectionApi::instances(&scene, robot) {
        let includes: Vec<String> = collection
            .includes()
            .into_iter()
            .map(|t| t.display(&store.paths, &store.tokens))
            .collect();
        println!(
            "/Robot collection {:?} includes {includes:?} ({:?})",
            collection.instance(),
            collection.expansion_rule()
        );
    }

    // --- The raw value, with provenance, for what views do not show. ---
    let name = store
        .tokens
        .lookup(Gprim::DISPLAY_COLOR)
        .expect("interned by the edits");
    let resolved = live
        .stage()
        .resolve_value_with_schema(arm, name, &store)
        .expect("authored");
    let provenance = resolved.provenance.expect("composed with provenance");
    println!(
        "displayColor comes from layer {} (1 = shot, 2 = base)",
        provenance.layer.0
    );
}
