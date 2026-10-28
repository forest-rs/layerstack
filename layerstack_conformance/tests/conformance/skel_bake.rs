// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Skinning bakes against OpenUSD's `UsdSkelBakeSkinning`.
//!
//! `scripts/skel_bake_oracle.py` bakes `fixtures/skel_bake/scene.usda` into
//! its root layer twice, over every `SkelRoot` at every time and over one
//! root between time codes 1 and 3, and records every prim spec the root
//! layer then holds. Here the same bakes run through `SkinningBake`, and
//! the root layer must hold the same specs: specifiers, types, applied
//! schemas, kinds, property names, types, variability and metadata
//! exactly, the same sample times, and every baked number (points,
//! normals, transforms, extents, extents hints) within 1e-5, relative above
//! one. That covers the retyped roots, the kept `skel:` bindings, the
//! rigid prims' `xformOp:transform` and `xformOpOrder`, inline mesh
//! extents, extents recomputed afterwards and the model's extents hint.
//!
//! OpenUSD: `pxr/usd/usdSkel/bakeSkinning.cpp`.

use std::sync::Arc;

use layerstack::doc::{Layer, SublayerEntry};
use layerstack::{EditTarget, LayerId, LiveStage, StageOptions, Time};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use layerstack_schemas::Scene;
use layerstack_schemas::skel::{
    BakeBinding, BakeDeformations, BakeProblem, BakeProblemReason, RetypedRoot, SkinningBake,
    SkinningBakeError, SkinningBakeReport,
};
use serde::Deserialize;
use serde_json::Value as Json;

use crate::support::layer_json;

const ORACLE: &str = include_str!("../../fixtures/skel_bake/oracle.json");
const TOLERANCE: f64 = 1e-5;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    stage: Json,
    root_interval: Json,
}

fn load() -> (LoadedStage, LiveStage) {
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/skel_bake/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(loaded.store.tokens()))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options);
    (loaded, live)
}

fn compare(loaded: &LoadedStage, expected: &Json, case: &str) {
    let got = layer_json::layer(&loaded.store, loaded.root_layer);
    if let Some(difference) = layer_json::difference(&got, expected, TOLERANCE) {
        panic!("{case}: {difference}");
    }
}

fn bake_stage() -> (LoadedStage, LiveStage, SkinningBakeReport) {
    let (mut loaded, mut live) = load();
    let target = EditTarget::for_layer(loaded.root_layer);
    let bake = {
        let scene = Scene::new(live.stage(), &loaded.store);
        let root = loaded.store.path("/");
        SkinningBake::for_subtree(&scene, root, target)
    };
    assert_eq!(bake.bindings.len(), 2, "both SkelRoots");
    let report = bake.bake(&mut live, &mut loaded.store).unwrap();
    (loaded, live, report)
}

#[test]
fn stage_bake_matches_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (loaded, _, report) = bake_stage();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    compare(&loaded, &oracle.stage, "stage");
}

#[test]
fn root_bake_over_an_interval_matches_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let (mut loaded, mut live) = load();
    let root = loaded.store.path("/Character/Root");
    let report = SkinningBake::for_root(root, EditTarget::for_layer(loaded.root_layer))
        .with_interval(1.0..=3.0)
        .bake(&mut live, &mut loaded.store)
        .unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    compare(&loaded, &oracle.root_interval, "root_interval");
}

/// The report names what was written where, and at which times.
#[test]
fn the_report_says_what_was_baked() {
    let (loaded, _, report) = bake_stage();
    let path = |text: &str| loaded.store.path(text);
    let [root_prim, statue, body, hat, morph, stone, dust, character] = [
        "/Character/Root",
        "/Statue",
        "/Character/Root/Body",
        "/Character/Root/Hat",
        "/Character/Root/Morph",
        "/Statue/Stone",
        "/Character/Root/Dust",
        "/Character",
    ]
    .map(path);
    let root = loaded.root_layer;
    assert_eq!(report.layers, [root]);
    assert_eq!(
        report.retyped_roots,
        [
            RetypedRoot {
                root: root_prim,
                layer: root
            },
            RetypedRoot {
                root: statue,
                layer: root
            }
        ]
    );
    let prim = |path| {
        report
            .prims
            .iter()
            .find(|baked| baked.prim == path)
            .expect("baked")
    };
    let skinned = prim(body);
    assert_eq!(
        skinned.deformations,
        BakeDeformations {
            xforms_with_skinning: false,
            ..BakeDeformations::ALL
        }
    );
    assert!(skinned.extent);
    let numeric: Vec<Time> = (0..=4).map(|t| Time::at(f64::from(t))).collect();
    assert_eq!(skinned.times, numeric);
    let rigid = prim(hat).deformations;
    assert!(rigid.xforms_with_skinning && !rigid.points_with_skinning);
    let shaped = prim(morph).deformations;
    assert!(shaped.points_with_blend_shapes && !shaped.points_with_skinning);
    assert_eq!(prim(stone).times, [Time::Default]);
    assert_eq!(report.extents, [dust]);
    assert_eq!(report.extents_hints, [character]);
    assert!(report.unbaked.is_empty());
    assert_eq!(report.times[0], Time::Default);
    assert_eq!(report.applied.len(), 3, "values, extents, hints");
}

/// Undoing the bake restores the layer exactly.
#[test]
fn undo_restores_the_layer() {
    let (before, _) = load();
    let expected = layer_json::layer(&before.store, before.root_layer);
    let (mut loaded, mut live, report) = bake_stage();
    live.apply(&mut loaded.store, &report.undo()).unwrap();
    let restored = layer_json::layer(&loaded.store, loaded.root_layer);
    assert_eq!(layer_json::difference(&restored, &expected, 0.0), None);
}

/// The values a stage bake wrote for `prim`'s attribute `name` in OpenUSD.
fn oracle_attribute(oracle: &Oracle, prim: &str, name: &str) -> Json {
    oracle.stage[prim]["properties"][name].clone()
}

fn attribute(loaded: &LoadedStage, layer: LayerId, prim: &str, name: &str) -> Json {
    let path = layerstack::Path::parse_absolute(prim, loaded.store.tokens()).unwrap();
    let path = loaded.store.paths().lookup(&path).unwrap();
    let Some(spec) = loaded.store.layer(layer).unwrap().prim(path) else {
        return Json::Null;
    };
    layer_json::prim(spec, &loaded.store)["properties"][name].clone()
}

fn agrees(got: &Json, want: &Json, what: &str) {
    if let Some(difference) = layer_json::difference(got, want, TOLERANCE) {
        panic!("{what}: {difference}");
    }
}

/// Each binding's values go to its own layer: here `/Character/Root` to a
/// new root layer over the fixture and `/Statue` to a layer between them.
/// Extents recomputed afterwards and extents hints go to the extents
/// target; the fixture's layer is untouched.
#[test]
fn each_binding_writes_to_its_layer() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let (mut loaded, _) = load();
    let fixture = loaded.root_layer;
    let untouched = layer_json::layer(&loaded.store, fixture);
    let (top, middle) = (LayerId(1001), LayerId(1002));
    let mut root = Layer::new(top);
    root.sublayers_mut().push(SublayerEntry::new(middle));
    root.sublayers_mut().push(SublayerEntry::new(fixture));
    loaded.store.insert_layer(root);
    loaded.store.insert_layer(Layer::new(middle));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(loaded.store.tokens()))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut loaded.store, top, options);
    let character = loaded.store.path("/Character/Root");
    let statue = loaded.store.path("/Statue");
    let mut bake = SkinningBake::new(EditTarget::for_layer(top));
    bake.bindings = vec![
        BakeBinding {
            root: character,
            skeleton: None,
            layer: top,
        },
        BakeBinding {
            root: statue,
            skeleton: None,
            layer: middle,
        },
    ];
    let report = bake.bake(&mut live, &mut loaded.store).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.layers, [top, middle]);
    assert_eq!(
        report.retyped_roots,
        [
            RetypedRoot {
                root: character,
                layer: top
            },
            RetypedRoot {
                root: statue,
                layer: middle
            }
        ]
    );
    for (prim, name, layer) in [
        ("/Character/Root/Body", "points", top),
        ("/Character/Root/Body", "normals", top),
        ("/Character/Root/Body", "extent", top),
        ("/Character/Root/Hat", "xformOp:transform", top),
        ("/Character/Root/Faceted", "normals", top),
        ("/Character/Root/Dust", "extent", top),
        ("/Statue/Stone", "points", middle),
        ("/Statue/Stone", "extent", middle),
    ] {
        let mut got = attribute(&loaded, layer, prim, name);
        let mut want = oracle_attribute(&oracle, prim, name);
        // The new layers hold only what the bake wrote: no rest default,
        // no `interpolation` (which stays on the fixture's spec).
        for value in [&mut got, &mut want] {
            if layer == top
                || value["timeSamples"]
                    .as_object()
                    .is_some_and(|s| !s.is_empty())
            {
                value["default"] = Json::Null;
            }
            value["interpolation"] = Json::Null;
        }
        agrees(&got, &want, &format!("{prim}.{name}"));
    }
    let hint = loaded.store.path("/Character");
    let hints = layer_json::prim_attributes(&loaded.store, top, hint)["extentsHint"].clone();
    agrees(
        &hints["timeSamples"],
        &oracle_attribute(&oracle, "/Character", "extentsHint")["timeSamples"],
        "extentsHint",
    );
    assert_eq!(layer_json::layer(&loaded.store, fixture), untouched);
}

/// Without normals, the bake writes the same points and no normals; without
/// transforms, rigid prims are not baked.
#[test]
fn deformations_select_what_is_written() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let (mut loaded, mut live) = load();
    let root = loaded.root_layer;
    let character = loaded.store.path("/Character/Root");
    let hat = loaded.store.path("/Character/Root/Hat");
    let badge = loaded.store.path("/Character/Root/Badge");
    let report = SkinningBake::for_root(character, EditTarget::for_layer(root))
        .with_deformations(BakeDeformations {
            normals_with_skinning: false,
            normals_with_blend_shapes: false,
            xforms_with_skinning: false,
            ..BakeDeformations::ALL
        })
        .bake(&mut live, &mut loaded.store)
        .unwrap();
    assert_eq!(report.unbaked, [hat, badge]);
    for prim in [
        "/Character/Root/Body",
        "/Character/Root/Faceted",
        "/Character/Root/Morph",
    ] {
        agrees(
            &attribute(&loaded, root, prim, "points"),
            &oracle_attribute(&oracle, prim, "points"),
            prim,
        );
        let normals = attribute(&loaded, root, prim, "normals");
        assert!(
            normals.is_null() || normals["timeSamples"].as_object().unwrap().is_empty(),
            "{prim} normals unbaked"
        );
    }
    for prim in ["/Character/Root/Hat", "/Character/Root/Badge"] {
        assert!(attribute(&loaded, root, prim, "xformOp:transform").is_null());
    }
}

/// With no deformation nothing is baked, yet the root is retyped, as
/// OpenUSD retypes the root of every binding.
#[test]
fn no_deformation_still_retypes_the_root() {
    let (mut loaded, mut live) = load();
    let root = loaded.root_layer;
    let character = loaded.store.path("/Character/Root");
    let report = SkinningBake::for_root(character, EditTarget::for_layer(root))
        .with_deformations(BakeDeformations::NONE)
        .bake(&mut live, &mut loaded.store)
        .unwrap();
    assert!(report.prims.is_empty());
    assert_eq!(report.unbaked.len(), 6);
    assert_eq!(
        report.retyped_roots,
        [RetypedRoot {
            root: character,
            layer: root
        }]
    );
    let scene = Scene::new(live.stage(), &loaded.store);
    assert!(scene.is_a(character, "Xform") && !scene.is_a(character, "SkelRoot"));
}

/// Without extents, no `extent` is written, and the extents hint, computed
/// from the baked points, is the same.
#[test]
fn extents_are_optional() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let (mut loaded, mut live) = load();
    let root = loaded.root_layer;
    let character = loaded.store.path("/Character/Root");
    let mut bake = SkinningBake::for_root(character, EditTarget::for_layer(root));
    bake.update_extents = false;
    let report = bake.bake(&mut live, &mut loaded.store).unwrap();
    assert!(report.extents.is_empty());
    assert!(report.prims.iter().all(|prim| !prim.extent));
    for prim in ["/Character/Root/Body", "/Character/Root/Dust"] {
        assert!(attribute(&loaded, root, prim, "extent").is_null(), "{prim}");
    }
    agrees(
        &attribute(&loaded, root, "/Character", "extentsHint"),
        &oracle_attribute(&oracle, "/Character", "extentsHint"),
        "extentsHint",
    );
}

use crate::support::schema_scene as support;

const SMALL: &str = r#"#usda 1.0
def SkelRoot "Root" (prepend apiSchemas = ["SkelBindingAPI"])
{
    rel skel:skeleton = </Root/Skel>
    def Skeleton "Skel"
    {
        uniform token[] joints = ["A"]
        uniform matrix4d[] bindTransforms = [( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) )]
        uniform matrix4d[] restTransforms = [( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 1, 0, 1) )]
    }
    def Mesh "M" (prepend apiSchemas = ["SkelBindingAPI"])
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        float3[] normals = [(0, 0, 1), (0, 0, 1), (0, 0, 1)] (interpolation = "vertex")
        point3f[] points.timeSamples = {
            10: [(0, 0, 0), (1, 0, 0), (0, 1, 0)],
            20: [(0, 0, 2), (1, 0, 2), (0, 1, 2)],
        }
        int[] primvars:skel:jointIndices = [0, 0, 0] (elementSize = 1
            interpolation = "vertex")
        float[] primvars:skel:jointWeights = [1, 1, 1] (elementSize = 1
            interpolation = "vertex")
    }
}
class SkelRoot "Proto"
{
}
def SkelRoot "Instance" (
    instanceable = true
    prepend references = </Proto>
)
{
}
"#;

/// An attribute of another type in the layer is not written, and said so;
/// samples outside the interval are cleared, as OpenUSD clears an
/// attribute's samples before baking it.
#[test]
fn mismatched_types_are_reported_and_stale_samples_cleared() {
    let (mut store, mut live) = support::scene(SMALL);
    let root = store.path("/Root");
    let mesh = store.path("/Root/M");
    let report = SkinningBake::for_root(root, EditTarget::for_layer(LayerId(1)))
        .with_interval(0.0..15.0)
        .bake(&mut live, &mut store)
        .unwrap();
    assert!(matches!(
        &report.problems[..],
        [BakeProblem {
            reason: BakeProblemReason::AttributeType { name: "normals", existing },
            ..
        }] if existing == "float3[]"
    ));
    let points = layer_json::prim_attributes(&store, LayerId(1), mesh)["points"].clone();
    let samples = points["timeSamples"].as_object().unwrap();
    assert_eq!(samples.keys().collect::<Vec<_>>(), ["10.0"]);
    assert_eq!(
        samples["10.0"],
        serde_json::json!([[0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [0.0, 2.0, 0.0]])
    );
    let normals = layer_json::prim_attributes(&store, LayerId(1), mesh)["normals"].clone();
    assert!(normals["timeSamples"].as_object().unwrap().is_empty());
}

#[test]
fn bakes_refuse_bad_bindings_before_writing() {
    let (mut store, mut live) = support::scene(SMALL);
    let mesh = store.path("/Root/M");
    let instance = store.path("/Instance");
    let root = store.path("/Root");
    let target = EditTarget::for_layer(LayerId(1));
    let before = layer_json::layer(&store, LayerId(1));
    assert_eq!(
        SkinningBake::for_root(mesh, target.clone())
            .bake(&mut live, &mut store)
            .unwrap_err(),
        SkinningBakeError::NotSkelRoot(mesh)
    );
    assert_eq!(
        SkinningBake::for_root(instance, target.clone())
            .bake(&mut live, &mut store)
            .unwrap_err(),
        SkinningBakeError::Instanced(instance)
    );
    assert_eq!(
        SkinningBake::for_root(root, EditTarget::for_layer(LayerId(99)))
            .bake(&mut live, &mut store)
            .unwrap_err(),
        SkinningBakeError::MissingLayer(LayerId(99))
    );
    assert_eq!(layer_json::layer(&store, LayerId(1)), before);
}

/// Deforming on threads writes exactly what deforming on one thread does.
#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
fn threaded_bakes_write_the_same_layer() {
    let (one, _, _) = bake_stage();
    let (mut loaded, mut live) = load();
    let bake = {
        let scene = Scene::new(live.stage(), &loaded.store);
        let root = loaded.store.path("/");
        SkinningBake::for_subtree(&scene, root, EditTarget::for_layer(loaded.root_layer))
            .with_threads(4)
    };
    bake.bake(&mut live, &mut loaded.store).unwrap();
    assert_eq!(
        layer_json::layer(&loaded.store, loaded.root_layer),
        layer_json::layer(&one.store, one.root_layer)
    );
}

#[test]
fn pinned_bake_oracle_matches_configured_openusd() {
    let Ok(python) = std::env::var("LAYERSTACK_USD_PYTHON") else {
        return;
    };
    let output = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/skel_bake_oracle.py"
        ))
        .arg("--check")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
