// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! World transforms of every `Gprim`, with its visibility and purpose.
//!
//! A small orrery is authored through the transform op API: a sun, a planet
//! whose orbit turns over time (a time-sampled `rotateY`), a moon around the
//! planet, and a hidden guide. For two time codes the example walks the
//! stage, and for every `Gprim` (every piece of geometry) prints its world
//! position and its full local-to-world matrix, and whether it is visible
//! and what purpose it has.
//!
//! One `XformCache` per time code shares each ancestor's transform between
//! its descendants; its statistics show the work done.

use std::sync::Arc;

use layerstack::edit::EditTarget;
use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, Path, StageOptions};
use layerstack_schemas::usd_geom::{
    Cube, Gprim, Imageable, ImageablePurpose, ImageableVisibility, Sphere, Xform, XformableEdit,
};
use layerstack_schemas::{
    Scene, SchemaEdit, Time, XformCache, XformOpError, XformOpPrecision, XformOpType,
};

fn main() -> Result<(), XformOpError> {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    let [orrery, sun, orbit, planet, moon_orbit, moon, guide] = [
        "/Orrery",
        "/Orrery/Sun",
        "/Orrery/Orbit",
        "/Orrery/Orbit/Planet",
        "/Orrery/Orbit/Planet/MoonOrbit",
        "/Orrery/Orbit/Planet/MoonOrbit/Moon",
        "/Orrery/OrbitGuide",
    ]
    .map(|p| store.path(p));

    // --- Author the orrery. ---
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let double = XformOpPrecision::Double;
    let float = XformOpPrecision::Float;
    Xform::define(&mut edit, orrery);
    let handle = |edit: &SchemaEdit<'_>, path| XformableEdit::new(edit, path).expect("defined");
    handle(&edit, orrery)
        .add_translate_op(&mut edit, double)?
        .set(&mut edit, [0.0, 1.0, 0.0])?;

    Sphere::define(&mut edit, sun).set_radius(&mut edit, 2.0);

    // The orbit turns once over 24 time codes.
    Xform::define(&mut edit, orbit);
    let turn = handle(&edit, orbit).add_op(&mut edit, XformOpType::RotateY, float, None, false)?;
    turn.set_at(&mut edit, 0.0, 0.0)?
        .set_at(&mut edit, 24.0, 360.0)?;

    // The planet: out along X, half size. The last op listed applies
    // first, so the planet is scaled, then moved.
    Sphere::define(&mut edit, planet);
    let planet_ops = handle(&edit, planet);
    planet_ops
        .add_translate_op(&mut edit, double)?
        .set(&mut edit, [10.0, 0.0, 0.0])?;
    planet_ops
        .add_scale_op(&mut edit, float)?
        .set(&mut edit, [0.5, 0.5, 0.5])?;

    Xform::define(&mut edit, moon_orbit);
    handle(&edit, moon_orbit)
        .add_op(&mut edit, XformOpType::RotateZ, float, None, false)?
        .set(&mut edit, 30.0)?;

    Sphere::define(&mut edit, moon);
    handle(&edit, moon)
        .add_translate_op(&mut edit, double)?
        .set(&mut edit, [4.0, 0.0, 0.0])?;

    Cube::define(&mut edit, guide)
        .set_purpose(&mut edit, ImageablePurpose::Guide)
        .set_visibility(&mut edit, ImageableVisibility::Invisible);

    let transaction = edit.finish();
    live.apply(&mut store, &transaction)
        .expect("the orrery applies");

    // --- Read it back at two time codes. ---
    let stage = live.stage();
    let scene = Scene::new(stage, &store);
    let root = store.paths.lookup(&Path::root()).expect("the pseudo-root");
    for code in [0.0, 6.0] {
        println!("time code {code}");
        let time = Time::at(code);
        let mut cache = XformCache::new(time);
        for path in stage.traverse(root) {
            let Some(gprim) = Gprim::new(&scene, path) else {
                continue;
            };
            let world = cache
                .local_to_world(&scene, path)
                .expect("the prim is on the stage");
            // A row-vector matrix: the translation is the last row.
            let [x, y, z, _] = world[3];
            let imageable: &Imageable<'_> = &gprim;
            println!(
                "  {:<36} at ({x:6.2}, {y:6.2}, {z:6.2})  visibility {:<9}  purpose {}",
                store.paths.display(path, &store.tokens),
                imageable.compute_visibility(time).as_str(),
                imageable.compute_purpose().as_str(),
            );
            for row in world {
                println!(
                    "      [{:8.4} {:8.4} {:8.4} {:8.4}]",
                    row[0], row[1], row[2], row[3]
                );
            }
        }
        println!("  cache: {:?}", cache.stats());
    }
    Ok(())
}
