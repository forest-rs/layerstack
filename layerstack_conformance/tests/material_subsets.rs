// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Material subset queries validate selections and retain USD binding rules.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::Time;
use layerstack_schemas::{
    BindingCache, BindingOptions, MaterialPurpose, MaterialSubsetError, Scene,
    subset::SubsetProblemKind,
};
fn text(family: &str, b: &str, strength: &str) -> String {
    format!(
        r#"#usda 1.0
    def Material "A" {{}}
    def Material "B" {{}}
    def Mesh "M" {{
        int[] faceVertexCounts = [3, 3]
        uniform token subsetFamily:materialBind:familyType = "{family}"
        rel material:binding = </A> {strength}
        def GeomSubset "a" {{
            uniform token elementType = "face"
            uniform token familyName = "materialBind"
            int[] indices = [0]
            rel material:binding = </B>
        }}
        def GeomSubset "b" {{
            uniform token elementType = "face"
            uniform token familyName = "materialBind"
            int[] indices = [{b}]
        }}
    }}
    "#
    )
}
#[test]
fn material_subsets_keep_shared_indices_and_inherited_binding_provenance() {
    for stronger in [false, true] {
        let (mut store, live) = support::scene(&text(
            "partition",
            "1",
            if stronger {
                "(bindMaterialAs = \"strongerThanDescendants\")"
            } else {
                ""
            },
        ));
        let path = store.path("/M");
        let a = store.path("/A");
        let b = store.path("/B");
        let sa = store.path("/M/a");
        let scene = Scene::new(live.stage(), &store);
        let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
        let family = cache
            .material_binding_subsets(&scene, path, Time::Default)
            .unwrap();
        assert_eq!(family.family_type, "partition");
        assert_eq!(family.fallback.material, Some(a));
        assert_eq!(family.subsets.len(), 2);
        assert_eq!(
            family.subsets[0].material.material,
            Some(if stronger { a } else { b })
        );
        assert_eq!(family.subsets[1].material.material, Some(a));
        let native = layerstack_schemas::usd_geom::GeomSubset::new(&scene, sa)
            .unwrap()
            .indices()
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&native, &family.subsets[0].indices));
        let reads = cache.stats().inputs_read;
        cache
            .material_binding_subsets(&scene, path, Time::Default)
            .unwrap();
        assert_eq!(cache.stats().inputs_read, reads);
    }
}
#[test]
fn material_subsets_report_family_overlap_bounds_and_coverage() {
    for (family, index, expected) in [
        ("nonOverlapping", "0", SubsetProblemKind::DuplicateIndex(0)),
        ("partition", "", SubsetProblemKind::IncompletePartition),
        ("nonOverlapping", "2", SubsetProblemKind::InvalidIndex(2)),
    ] {
        let (mut store, live) = support::scene(&text(family, index, ""));
        let path = store.path("/M");
        let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
        let Err(MaterialSubsetError::InvalidFamily(problems)) =
            cache.material_binding_subsets(&Scene::new(live.stage(), &store), path, Time::Default)
        else {
            panic!("invalid family");
        };
        assert!(problems.problems.iter().any(|p| p.kind == expected));
    }
    let (mut store, live) = support::scene(&text("unrestricted", "1", ""));
    let path = store.path("/M");
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    assert_eq!(
        cache.material_binding_subsets(&Scene::new(live.stage(), &store), path, Time::Default),
        Err(MaterialSubsetError::InvalidFamilyType(
            "unrestricted".into()
        ))
    );
}
#[test]
fn material_subsets_validate_only_requested_time_and_allow_parent_only_meshes() {
    let source = text("partition", "1", "").replace(
        "int[] indices = [1]",
        "int[] indices.timeSamples = {1: [1], 2: [0]}",
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/M");
    let scene = Scene::new(live.stage(), &store);
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    assert!(
        cache
            .material_binding_subsets(&scene, path, Time::held(1.))
            .is_ok()
    );
    assert!(matches!(
        cache.material_binding_subsets(&scene, path, Time::held(2.)),
        Err(MaterialSubsetError::InvalidFamily(_))
    ));
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef Material \"A\" {}\ndef Mesh \"M\" { rel material:binding = </A> }",
    );
    let path = store.path("/M");
    let a = store.path("/A");
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let result = cache
        .material_binding_subsets(&Scene::new(live.stage(), &store), path, Time::Default)
        .unwrap();
    assert_eq!(result.family_type, "unrestricted");
    assert!(result.subsets.is_empty());
    assert_eq!(result.fallback.material, Some(a));
}

#[test]
fn snapshot_subset_query_does_not_decode_unselected_topology_samples() {
    use layerstack::{ArrayReadError, DeferredArraySource, LayerId, TypedArray, Value};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Debug)]
    struct Cold {
        calls: Arc<AtomicUsize>,
        error: ArrayReadError,
    }
    impl DeferredArraySource for Cold {
        fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(&self.error)
        }
        fn element_kind(&self) -> Value {
            Value::Int(0)
        }
    }
    let source = text("partition", "1", "").replace(
        "int[] faceVertexCounts = [3, 3]",
        "int[] faceVertexCounts.timeSamples = {0: [3, 3], 1: [3, 3]}",
    );
    let (mut store, _) = support::scene(&source);
    let path = store.path("/M");
    let counts = store.tokens.lookup("faceVertexCounts").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let spec = store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .prims
        .get_mut(&path)
        .unwrap()
        .property_mut(counts)
        .unwrap();
    spec.time_samples = Some(
        vec![
            (
                0.,
                Value::TypedArray(TypedArray::Deferred(Arc::new(Cold {
                    calls: calls.clone(),
                    error: ArrayReadError::BudgetExceeded { limit: 0 },
                }))),
            ),
            (1., Value::from(vec![3_i32, 3])),
        ]
        .into(),
    );
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let live = layerstack::LiveStage::compose(
        &mut store,
        LayerId(1),
        layerstack::StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    calls.store(0, Ordering::Relaxed);
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    cache
        .material_binding_subsets(&Scene::new(live.stage(), &store), path, Time::held(1.))
        .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn material_subsets_preserve_decode_failure_paths() {
    use layerstack::{
        ArrayReadError, DeferredArraySource, LayerId, PropertyPath, TypedArray, Value,
    };
    use std::sync::Arc;
    #[derive(Debug)]
    struct Failed(ArrayReadError);
    impl DeferredArraySource for Failed {
        fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
            Err(&self.0)
        }
        fn element_kind(&self) -> Value {
            Value::Int(0)
        }
    }
    for (prim, name) in [("/M", "faceVertexCounts"), ("/M/a", "indices")] {
        let (mut store, _) = support::scene(&text("partition", "1", ""));
        let path = store.path(prim);
        let token = store.tokens.lookup(name).unwrap();
        let property = PropertyPath::new(path, token);
        let error = ArrayReadError::InvalidData("invalid retained subset input".into());
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .prims
            .get_mut(&path)
            .unwrap()
            .property_mut(token)
            .unwrap()
            .default = Some(Value::TypedArray(TypedArray::Deferred(Arc::new(Failed(
            error.clone(),
        )))));
        let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
        let live = layerstack::LiveStage::compose(
            &mut store,
            LayerId(1),
            layerstack::StageOptions {
                schemas: Some(schemas),
                ..Default::default()
            },
        );
        let mesh = store.path("/M");
        let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
        assert_eq!(
            cache.material_binding_subsets(&Scene::new(live.stage(), &store), mesh, Time::Default),
            Err(MaterialSubsetError::Decode { property, error })
        );
    }
}

#[test]
fn material_subsets_reject_unauthored_unrestricted_families_and_nonface_members() {
    for (case, source) in [
        text("nonOverlapping", "1", "").replace(
            "uniform token subsetFamily:materialBind:familyType = \"nonOverlapping\"",
            "",
        ),
        text("nonOverlapping", "1", "").replace(
            "uniform token elementType = \"face\"",
            "uniform token elementType = \"point\"",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut store, live) = support::scene(&source);
        let path = store.path("/M");
        let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
        let error = cache
            .material_binding_subsets(&Scene::new(live.stage(), &store), path, Time::Default)
            .unwrap_err();
        if case == 0 {
            assert_eq!(
                error,
                MaterialSubsetError::InvalidFamilyType("unrestricted".into())
            );
        } else {
            assert_eq!(
                error,
                MaterialSubsetError::InvalidElementType(store.path("/M/a"))
            );
        }
    }
}

#[test]
fn snapshot_subset_diagnostics_report_requested_time_for_empty_indices() {
    let source =
        text("nonOverlapping", "", "").replace("int[] indices = [0]", "int[] indices = []");
    let (mut store, live) = support::scene(&source);
    let path = store.path("/M");
    let time = Time::held(7.);
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let Err(MaterialSubsetError::InvalidFamily(validation)) =
        cache.material_binding_subsets(&Scene::new(live.stage(), &store), path, time)
    else {
        panic!("empty family is invalid")
    };
    let problem = validation
        .problems
        .iter()
        .find(|p| p.kind == SubsetProblemKind::NoIndices)
        .unwrap();
    assert_eq!(problem.time, time);
}
