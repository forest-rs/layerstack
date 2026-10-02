// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Renderer-owned upload buffers from retained USD inputs, without CPU vertices.
//! Two mesh parts share one rig palette. Repeated times skip uploads; previous
//! submitted-frame palettes belong to the renderer, independently of cache time.
//! The buffers preserve USD row-vector `f64` matrices. A real backend chooses its
//! own packing, precision, resources and shader conventions.
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, LiveStage, PathId, PathInterner,
    ResolvedAsset, StageOptions, TokenInterner,
};
use layerstack_schemas::{
    Scene, Time,
    skel::{DeformationInputs, SkelCache, SkinningBindingInputs},
};
use std::{collections::HashMap, sync::Arc};

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

#[derive(Default)]
struct RigBuffers {
    revision: u64,
    current: Vec<[[f64; 4]; 4]>,
    previous: Vec<[[f64; 4]; 4]>,
}
struct BindingBuffers {
    revision: u64,
    mapping: Option<Vec<Option<usize>>>,
    values: SkinningBindingInputs,
}
#[derive(Default)]
struct Renderer {
    rigs: HashMap<PathId, RigBuffers>,
    bindings: HashMap<PathId, BindingBuffers>,
    palette_uploads: usize,
    binding_uploads: usize,
}
impl Renderer {
    fn begin_frame(&mut self) {
        for rig in self.rigs.values_mut() {
            rig.previous.clone_from(&rig.current);
        }
    }
    fn prepare(&mut self, inputs: DeformationInputs<'_>, point_count: usize) {
        inputs
            .validate_point_count(point_count)
            .expect("valid adapter vertex count");
        let rig = self.rigs.entry(inputs.skeleton_path()).or_default();
        if rig.revision != inputs.revisions().pose {
            rig.current.clear();
            rig.current
                .extend_from_slice(inputs.shared_skinning_transforms());
            if rig.previous.is_empty() {
                rig.previous.clone_from(&rig.current);
            }
            rig.revision = inputs.revisions().pose;
            self.palette_uploads += 1;
        }
        // This example uses LBS and no blend shapes. DQS consumers can instead
        // pack `shared_dual_quaternions`; shape definitions and contributions
        // have separate `binding_definition` and `blend_weights` revisions.
        assert!(
            inputs.shared_dual_quaternions().is_none(),
            "this adapter example uses LBS"
        );
        assert!(
            inputs.blend_shapes().is_none(),
            "this scene has no blend shapes"
        );
        let path = inputs.geometry_path();
        let old = self.bindings.get(&path);
        if old.is_none_or(|b| b.revision != inputs.revisions().inputs) {
            self.bindings.insert(
                path,
                BindingBuffers {
                    revision: inputs.revisions().inputs,
                    mapping: inputs.joint_mapping().map(<[_]>::to_vec),
                    values: inputs.binding().clone(),
                },
            );
            self.binding_uploads += 1;
        }
        let binding = &self.bindings[&path];
        assert!(
            binding.mapping.is_none(),
            "both parts use Skeleton joint order"
        );
        assert_eq!(
            binding.values.influences().indices,
            [0],
            "both parts address the root joint"
        );
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
    let parts = ["/Rig/Body", "/Rig/Trim"].map(|p| store.path(p));
    let scene = Scene::new(live.stage(), &store);
    let mut cache = SkelCache::new(Time::at(0.));
    let mut renderer = Renderer::default();
    for time in [0., 1., 1., 2.] {
        renderer.begin_frame();
        cache.set_time(Time::at(time));
        for part in parts {
            renderer.prepare(cache.deformation_inputs(&scene, part).unwrap().unwrap(), 2);
        }
        println!(
            "time {time}: {} palette uploads, {} binding uploads",
            renderer.palette_uploads, renderer.binding_uploads
        );
    }
    assert_eq!(
        renderer.palette_uploads, 3,
        "one shared palette upload per distinct time"
    );
    assert_eq!(
        renderer.binding_uploads, 2,
        "one static binding upload per part"
    );
    assert_eq!(cache.stats().point_vertices, 0, "no CPU point deformation");
    assert_eq!(cache.stats().normal_vectors, 0, "no CPU normal deformation");
    assert_eq!(
        cache.stats().influence_resolutions,
        2,
        "static influences resolve once per part"
    );
    let rig = renderer.rigs.values().next().unwrap();
    assert_eq!(
        rig.current[0][3],
        [2., 0., 0., 1.],
        "current submitted frame"
    );
    assert_eq!(
        rig.previous[0][3],
        [1., 0., 0., 1.],
        "previous submitted frame"
    );
}
const SOURCE: &str = r#"#usda 1.0
def SkelRoot "Rig" (prepend apiSchemas = ["SkelBindingAPI"]) {
    rel skel:skeleton = </Rig/Skeleton>
    rel skel:animationSource = </Rig/Animation>
    def Skeleton "Skeleton" {
        uniform token[] joints = ["root"]
        uniform matrix4d[] restTransforms = [((1,0,0,0),(0,1,0,0),(0,0,1,0),(0,0,0,1))]
        uniform matrix4d[] bindTransforms = [((1,0,0,0),(0,1,0,0),(0,0,1,0),(0,0,0,1))]
    }
    def SkelAnimation "Animation" {
        uniform token[] joints = ["root"]
        float3[] translations.timeSamples = {0: [(0,0,0)], 2: [(2,0,0)]}
        quatf[] rotations = [(1,0,0,0)]
        half3[] scales = [(1,1,1)]
    }
    int[] primvars:skel:jointIndices = [0]
    float[] primvars:skel:jointWeights = [1]
    def Mesh "Body" {}
    def Mesh "Trim" {}
}
"#;
