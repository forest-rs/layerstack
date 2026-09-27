// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! World transforms of every `Gprim`, with its visibility and purpose.
//!
//! A small orrery, a sun with a planet whose orbit turns over time and a
//! moon around the planet, is read from USDA text. For two time codes the
//! example walks the stage, and for every `Gprim` (every piece of geometry)
//! prints its world position and its full local-to-world matrix, and
//! whether it is visible and what purpose it has.
//!
//! One `XformCache` per time code shares each ancestor's transform between
//! its descendants; its statistics show the work done.

use std::sync::Arc;

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, LiveStage, Path, PathInterner,
    ResolvedAsset, StageOptions, TokenInterner,
};
use layerstack_schemas::usd_geom::{Gprim, Imageable};
use layerstack_schemas::{Scene, Time, XformCache};

const ORRERY: &str = r#"#usda 1.0
(
    startTimeCode = 0
    endTimeCode = 24
)

def Xform "Orrery"
{
    double3 xformOp:translate = (0, 1, 0)
    uniform token[] xformOpOrder = ["xformOp:translate"]

    def Sphere "Sun"
    {
        double radius = 2
    }

    def Xform "Orbit"
    {
        float xformOp:rotateY.timeSamples = {
            0: 0,
            24: 360,
        }
        uniform token[] xformOpOrder = ["xformOp:rotateY"]

        def Sphere "Planet"
        {
            double3 xformOp:translate = (10, 0, 0)
            float3 xformOp:scale = (0.5, 0.5, 0.5)
            uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]

            def Xform "MoonOrbit"
            {
                float xformOp:rotateZ = 30
                uniform token[] xformOpOrder = ["xformOp:rotateZ"]

                def Sphere "Moon"
                {
                    double3 xformOp:translate = (4, 0, 0)
                    uniform token[] xformOpOrder = ["xformOp:translate"]
                }
            }
        }
    }

    def Cube "OrbitGuide"
    {
        uniform token purpose = "guide"
        token visibility = "invisible"
    }
}
"#;

/// The orrery names no other file.
struct NoFiles;

impl AssetResolver for NoFiles {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

fn main() {
    let mut store = InMemoryStore::default();
    let parsed = layerstack_usda::parser::parse(ORRERY);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoFiles,
    );
    store.insert_layer(emitted.layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut store, LayerId(1), options);
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
}
