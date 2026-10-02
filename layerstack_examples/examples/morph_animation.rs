// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained animated morph targets with no skeleton or joint influences.
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, LiveStage, PathInterner,
    ResolvedAsset, StageOptions, TokenInterner,
};
use layerstack_schemas::{Scene, Time, skel::BlendShapeCache};
use std::sync::Arc;
struct NoAssets;
impl AssetResolver for NoAssets {
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
    let parsed = layerstack_usda::parser::parse(SOURCE);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(
        parsed.diagnostics.is_empty() && emitted.diagnostics.is_empty(),
        "the inline USD scene must load: {:?} {:?}",
        parsed.diagnostics,
        emitted.diagnostics
    );
    store.insert_layer(emitted.layer);
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    assert!(
        live.stage().composition_errors().is_empty(),
        "the inline scene must compose"
    );
    let mesh = store.path("/Face");
    let scene = Scene::new(live.stage(), &store);
    let mut cache = BlendShapeCache::new(Time::at(0.));
    let mut previous = None;
    for time in [0., 1., 1., 2.] {
        cache.set_time(Time::at(time));
        let inputs = cache.inputs(&scene, mesh).unwrap().unwrap();
        inputs.validate_point_count(2).unwrap();
        let revision = (inputs.definition_revision(), inputs.weight_revision());
        println!(
            "time {time}: weights {:?}, inputs changed {}",
            inputs.weights(),
            previous != Some(revision)
        );
        previous = Some(revision);
        println!(
            "local animated hull {:?}",
            cache.deformed_mesh_bounds(&scene, mesh).unwrap().unwrap()
        );
        println!(
            "points {:?}",
            cache.deformed_points(&scene, mesh).unwrap().unwrap()
        );
    }
    assert_eq!(
        cache.stats().definition_builds,
        1,
        "one retained local binding"
    );
    assert_eq!(
        cache.stats().weight_evaluations,
        3,
        "repeated times reuse weights"
    );
    assert_eq!(
        cache.stats().point_vertices,
        6,
        "two vertices at three distinct times"
    );
}
const SOURCE: &str = r#"#usda 1.0
def Mesh "Face" (prepend apiSchemas = ["SkelBindingAPI"]) {
    rel skel:animationSource = </Animation>
    uniform token[] skel:blendShapes = ["smile"]
    rel skel:blendShapeTargets = [</Smile>]
    point3f[] points = [(0,0,0), (1,0,0)]
}
def SkelAnimation "Animation" {
    uniform token[] blendShapes = ["smile"]
    float[] blendShapeWeights.timeSamples = {0: [0], 2: [1]}
}
def BlendShape "Smile" {
    uniform int[] pointIndices = [1]
    uniform vector3f[] offsets = [(0,1,0)]
}
"#;
