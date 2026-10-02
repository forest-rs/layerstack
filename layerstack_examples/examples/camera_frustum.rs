// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Camera frusta and shutter-sampled instance transforms, without a graphics backend.
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, LiveStage, PathInterner,
    ResolvedAsset, StageOptions, TokenInterner,
};
use layerstack_schemas::{
    Scene, Time, XformCache,
    bounds::{BoundingBox, Range3d},
    point_instancer::InstanceTransformOptions,
    usd_geom::{Camera, PointInstancer},
};
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
    let camera_path = store.path("/Camera");
    let instancer_path = store.path("/Instances");
    let scene = Scene::new(live.stage(), &store);
    let frame = 0.;
    let mut xforms = XformCache::new(Time::at(frame));
    let camera = Camera::new(&scene, camera_path)
        .unwrap()
        .compute_camera(&mut xforms)
        .unwrap();
    let [open, close] = camera.shutter_interval(frame).unwrap();
    let times = [open, (open + close) / 2., close].map(Time::at);
    println!("USD row-vector view {:?}", camera.view_matrix());
    println!("OpenGL [-1,1] projection {:?}", camera.projection_matrix());
    println!(
        "shutter [{open}, {close}], corners {:?}",
        camera.frustum_corners()
    );
    let instancer = PointInstancer::new(&scene, instancer_path).unwrap();
    let samples = instancer
        .compute_instance_transforms_at_times(
            &times,
            Time::at(frame),
            InstanceTransformOptions::default(),
        )
        .unwrap();
    // These instances have identity prototypes and the instancer has no world
    // transform. With transformed instancers, append local_to_world here.
    for (time, sample) in times.iter().zip(samples) {
        let visible: Vec<_> = sample
            .iter()
            .filter(|instance| {
                camera.intersects_world_bound(&BoundingBox {
                    range: Range3d {
                        min: [-0.5; 3],
                        max: [0.5; 3],
                    },
                    matrix: instance.matrix,
                })
            })
            .map(|instance| instance.id)
            .collect();
        println!("{time:?}: visible IDs {visible:?}");
        assert_eq!(
            visible,
            [10],
            "only the centered instance intersects the primary frustum"
        );
    }
}
const SOURCE: &str = r#"#usda 1.0
(timeCodesPerSecond = 24)
def Camera "Camera" {
    double3 xformOp:translate = (0,0,10)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    float2 clippingRange = (1,100)
    double shutter:open = -0.25
    double shutter:close = 0.25
}
def Cube "Prototype" {
    double size = 1
}
def PointInstancer "Instances" {
    rel prototypes = [</Prototype>]
    int[] protoIndices = [0,0]
    int64[] ids = [10,20]
    point3f[] positions.timeSamples = {0: [(0,0,0),(40,0,0)], 2: [(0,0,0),(40,0,0)]}
    vector3f[] velocities.timeSamples = {0: [(24,0,0),(24,0,0)], 2: [(24,0,0),(24,0,0)]}
}
"#;
