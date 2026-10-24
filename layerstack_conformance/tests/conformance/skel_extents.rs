// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `Skeleton` and `SkelRoot` extents, and the bounds above them, against OpenUSD.
//!
//! `scripts/skel_extents_oracle.py` records, at default time and times 0
//! and 1, `ComputeExtentFromPlugins` for every `Skeleton` and `SkelRoot` of
//! `fixtures/skel_extents/scene.usda` and `BBoxCache::ComputeWorldBound`
//! for every prim. A Skeleton is bounded by its posed joint pivots; a
//! `SkelRoot` by its skeletons' pivots placed by their default-time transform
//! relative to the root, padded by the skinned geometry's reach. A skeleton
//! that cannot be posed has no extent in C++ (it posts a coding error); here
//! it is empty, so its ancestors bound without it, as C++'s do.
//!
//! One retained cache follows the time changes, and each value must also
//! agree with a fresh cache at that time.
//!
//! Spec: AOUSD Core §12.3 (attribute values at a time); OpenUSD's
//! `usdSkel` `skeleton.cpp` and `root.cpp` extent plugins.

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{Stage, StageOptions, Time};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{
    Scene,
    bounds::{BoundsCache, BoundsOptions, Range3d},
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/skel_extents/oracle.json");

/// A range, or `"error"` where C++'s plugin produced no extent.
#[derive(Deserialize)]
#[serde(untagged)]
enum Extent {
    Range([[f64; 3]; 2]),
    Error(#[allow(dead_code, reason = "always \"error\"")] String),
}

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    extents: BTreeMap<String, BTreeMap<String, Extent>>,
    bounds: BTreeMap<String, BTreeMap<String, Option<Corners>>>,
}

/// A range's minimum and maximum corners.
type Corners = [[f64; 3]; 2];

fn range(range: Range3d) -> Option<Corners> {
    (!range.is_empty()).then_some([range.min, range.max])
}

#[test]
fn skel_extents_and_bounds_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/skel_extents/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&loaded.store.tokens))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);
    let mut retained = BoundsCache::new(Time::Default, BoundsOptions::default());
    let mut checks = 0;
    for label in ["default", "0", "1", "0", "default"] {
        let time = label.parse().map_or(Time::Default, Time::at);
        retained.set_time(time);
        let mut fresh = BoundsCache::new(time, BoundsOptions::default());
        for (path, expected) in &oracle.extents[label] {
            let expected = match expected {
                Extent::Range(range) => Some(*range),
                Extent::Error(_) => None,
            };
            let prim = store.path(path);
            for cache in [&mut retained, &mut fresh] {
                let own = cache.untransformed_bound(&scene, prim).unwrap();
                assert_eq!(range(own.range), expected, "{path} extent at {label}");
                checks += 1;
            }
        }
        for (path, expected) in &oracle.bounds[label] {
            let prim = store.path(path);
            for cache in [&mut retained, &mut fresh] {
                let world = cache
                    .world_bound(&scene, prim)
                    .unwrap_or_else(|error| panic!("{path} at {label}: {error}"));
                assert_eq!(range(world.aligned_range()), *expected, "{path} at {label}");
                checks += 1;
            }
        }
    }
    assert!(checks > 100, "{checks}");
}

use crate::support::schema_scene as support;

/// Edits to an animation outside the `SkelRoot`'s namespace, and to a skinned
/// prim's extent, reach a retained cache through the extent's dependencies.
#[test]
fn skel_extents_follow_edits_outside_the_root() {
    use layerstack::{EditTarget, LayerId, Transaction, Value};
    let (mut store, mut live) = support::scene(
        r#"#usda 1.0
def SkelRoot "Root"
{
    def Skeleton "Skel" (prepend apiSchemas = ["SkelBindingAPI"])
    {
        uniform token[] joints = ["A", "A/B"]
        uniform matrix4d[] restTransforms = [( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) ), ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 2, 0, 1) )]
        uniform matrix4d[] bindTransforms = [( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) ), ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 2, 0, 1) )]
        rel skel:animationSource = </Anims/Walk>
    }

    def Mesh "Mesh" (prepend apiSchemas = ["SkelBindingAPI"])
    {
        int[] faceVertexIndices = [0, 1, 2]
        int[] faceVertexCounts = [3]
        point3f[] points = [(-1, 0, 0), (1, 0, 0), (0, 3, 0)]
        float3[] extent = [(-1, 0, 0), (1, 3, 0)]
        int[] primvars:skel:jointIndices = [0, 1, 1] (interpolation = "vertex")
        float[] primvars:skel:jointWeights = [1, 1, 1] (interpolation = "vertex")
        rel skel:skeleton = </Root/Skel>
    }
}

def Scope "Anims"
{
    def SkelAnimation "Walk"
    {
        uniform token[] joints = ["A", "A/B"]
        float3[] translations = [(0, 0, 0), (0, 2, 0)]
        quatf[] rotations = [(1, 0, 0, 0), (1, 0, 0, 0)]
        half3[] scales = [(1, 1, 1), (1, 1, 1)]
    }
}
"#,
    );
    let root = store.path("/Root");
    let walk = store.property_path("/Anims/Walk.translations");
    let extent = store.property_path("/Root/Mesh.extent");
    let mut retained = BoundsCache::new(Time::Default, BoundsOptions::default());
    fn bound(
        cache: &mut BoundsCache,
        live: &layerstack::LiveStage,
        store: &layerstack::InMemoryStore,
        root: layerstack::PathId,
    ) -> Option<[[f64; 3]; 2]> {
        range(
            cache
                .untransformed_bound(&Scene::new(live.stage(), store), root)
                .unwrap()
                .range,
        )
    }
    // Rest pivots (0,0,0)-(0,2,0), padded by the mesh's reach of 1.
    assert_eq!(
        bound(&mut retained, &live, &store, root),
        Some([[-1., -1., -1.], [1., 3., 1.]])
    );
    let target = EditTarget::for_layer(LayerId(1));
    for (property, value, expected) in [
        (
            walk,
            Value::from(vec![[3_f32, 0., 0.], [0., 2., 0.]]),
            [[2., -1., -1.], [4., 3., 1.]],
        ),
        (
            extent,
            Value::from(vec![[-1_f32, 0., 0.], [1., 5., 0.]]),
            [[0., -3., -3.], [6., 5., 3.]],
        ),
    ] {
        let mut transaction = Transaction::new();
        transaction.set_default(target.property(property), value);
        let applied = live.apply(&mut store, &transaction).unwrap();
        retained.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
        let mut fresh = BoundsCache::new(Time::Default, BoundsOptions::default());
        assert_eq!(bound(&mut fresh, &live, &store, root), Some(expected));
        assert_eq!(bound(&mut retained, &live, &store, root), Some(expected));
    }
}
