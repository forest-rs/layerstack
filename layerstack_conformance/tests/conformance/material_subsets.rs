// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Material subset queries validate selections and retain USD binding rules.
use crate::support::schema_scene as support;
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
        let (store, live) = support::scene(&text(
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
        let (store, live) = support::scene(&text(family, index, ""));
        let path = store.path("/M");
        let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
        let family = cache
            .material_binding_subsets(&Scene::new(live.stage(), &store), path, Time::Default)
            .expect("an invalid family still reads");
        assert_eq!(family.subsets.len(), 2, "problems never remove subsets");
        assert!(
            family
                .validation
                .problems
                .iter()
                .any(|p| p.kind == expected)
        );
    }
    let (store, live) = support::scene(&text("unrestricted", "0", ""));
    let path = store.path("/M");
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let family = cache
        .material_binding_subsets(&Scene::new(live.stage(), &store), path, Time::Default)
        .unwrap();
    assert_eq!(family.family_type, "unrestricted");
    assert_eq!(family.subsets.len(), 2);
    assert!(
        family.validation.is_valid(),
        "an unrestricted family may overlap"
    );
}
#[test]
fn material_subsets_validate_only_requested_time_and_allow_parent_only_meshes() {
    let source = text("partition", "1", "").replace(
        "int[] indices = [1]",
        "int[] indices.timeSamples = {1: [1], 2: [0]}",
    );
    let (store, live) = support::scene(&source);
    let path = store.path("/M");
    let scene = Scene::new(live.stage(), &store);
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    assert!(
        cache
            .material_binding_subsets(&scene, path, Time::held(1.))
            .unwrap()
            .validation
            .is_valid()
    );
    let overlapping = cache
        .material_binding_subsets(&scene, path, Time::held(2.))
        .unwrap();
    assert_eq!(*overlapping.subsets[1].indices, [0]);
    assert!(!overlapping.validation.is_valid());
    let (store, live) = support::scene(
        "#usda 1.0\ndef Material \"A\" {}\ndef Mesh \"M\" { rel material:binding = </A>; }",
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
    let counts = store.tokens().lookup("faceVertexCounts").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let spec = store
        .layer_mut(LayerId(1))
        .unwrap()
        .prim_mut(path)
        .unwrap()
        .property_mut(counts)
        .unwrap();
    spec.time_samples = vec![
        (
            0.,
            Value::TypedArray(TypedArray::Deferred(Arc::new(Cold {
                calls: calls.clone(),
                error: ArrayReadError::BudgetExceeded { limit: 0 },
            }))),
        ),
        (1., Value::from(vec![3_i32, 3])),
    ]
    .into();
    let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
    let live = layerstack::LiveStage::compose(
        &mut store,
        LayerId(1),
        layerstack::StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    calls.store(0, Ordering::Relaxed);
    let scene = Scene::new(live.stage(), &store);
    let geometry = layerstack_schemas::usd_geom::Imageable::new(&scene, path).unwrap();
    assert!(
        geometry
            .try_validate_subset_family_at(
                &layerstack_schemas::usd_geom::GeomSubsetElementType::Face,
                "materialBind",
                Time::held(1.),
            )
            .unwrap()
            .is_valid(),
        "checked snapshot validation uses only the selected topology sample"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "generic checked validation leaves cold samples undecoded"
    );
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
    for time in [Time::Default, Time::held(1.), Time::at(1.5)] {
        for (prim, name) in [("/M", "faceVertexCounts"), ("/M/a", "indices")] {
            let (mut store, _) = support::scene(&text("partition", "1", ""));
            let path = store.path(prim);
            let token = store.tokens().lookup(name).unwrap();
            let property = PropertyPath::new(path, token);
            let error = ArrayReadError::InvalidData("invalid retained subset input".into());
            let value = Value::TypedArray(TypedArray::Deferred(Arc::new(Failed(error.clone()))));
            let attribute = store
                .layer_mut(LayerId(1))
                .unwrap()
                .prim_mut(path)
                .unwrap()
                .property_mut(token)
                .unwrap();
            if time == Time::Default {
                attribute.default = Some(value);
            } else {
                attribute.time_samples = vec![(1., value.clone()), (2., value)].into();
            }
            let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
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
                cache.material_binding_subsets(&Scene::new(live.stage(), &store), mesh, time),
                Err(MaterialSubsetError::Decode { property, error }),
                "material validation preserves the requested {name} source at {time:?}"
            );
        }
    }
}

#[test]
fn material_subsets_read_untyped_families_and_report_nonface_members() {
    // C++ `GetMaterialBindSubsets` returns an untyped family's members, and
    // `ComputeBoundMaterial` binds them; `ValidateFamily` calls it valid.
    let untyped = text("nonOverlapping", "1", "").replace(
        "uniform token subsetFamily:materialBind:familyType = \"nonOverlapping\"",
        "",
    );
    let (store, live) = support::scene(&untyped);
    let path = store.path("/M");
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let family = cache
        .material_binding_subsets(&Scene::new(live.stage(), &store), path, Time::Default)
        .unwrap();
    assert_eq!(family.family_type, "unrestricted");
    assert_eq!(family.subsets.len(), 2);
    assert_eq!(family.subsets[0].material.material, Some(store.path("/B")));
    assert!(family.validation.is_valid());
    // A point member is not a face set: the renderer leaves it out, and
    // validation reports the mismatch.
    let mixed = text("nonOverlapping", "1", "").replacen(
        "uniform token elementType = \"face\"",
        "uniform token elementType = \"point\"",
        1,
    );
    let (store, live) = support::scene(&mixed);
    let path = store.path("/M");
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let family = cache
        .material_binding_subsets(&Scene::new(live.stage(), &store), path, Time::Default)
        .unwrap();
    assert_eq!(
        family
            .subsets
            .iter()
            .map(|subset| subset.path)
            .collect::<Vec<_>>(),
        [store.path("/M/b")]
    );
    assert_eq!(
        family.validation.problems[0].kind,
        SubsetProblemKind::ElementTypeMismatch
    );
    assert_eq!(family.validation.problems[0].path, store.path("/M/a"));
}

#[test]
fn snapshot_subset_diagnostics_report_requested_time_for_empty_indices() {
    let source =
        text("nonOverlapping", "", "").replace("int[] indices = [0]", "int[] indices = []");
    let (store, live) = support::scene(&source);
    let path = store.path("/M");
    let time = Time::held(7.);
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let family = cache
        .material_binding_subsets(&Scene::new(live.stage(), &store), path, time)
        .unwrap();
    let problem = family
        .validation
        .problems
        .iter()
        .find(|p| p.kind == SubsetProblemKind::NoIndices)
        .unwrap();
    assert_eq!(problem.time, time);
}

const ORACLE: &str = include_str!("../../fixtures/material_subsets/oracle.json");
const SCENE: &str = include_str!("../../fixtures/material_subsets/scene.usda");

/// `fixtures/material_subsets/scene.usda` read as OpenUSD 26.08 reads it
/// (`scripts/material_subsets_oracle.py`): every face member of the family
/// with readable indices, bound whether or not the family validates, with
/// `ValidateFamily` beside it. Untyped, overlapping, out-of-range, mixed,
/// unindexed and mistyped families all read.
#[test]
fn material_subsets_read_as_openusd_renders_them() {
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(
        oracle["openusd_version"],
        layerstack_schemas::OPENUSD_VERSION
    );
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    let display = |path| store.paths().display(path, store.tokens()).to_string();
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let meshes = oracle["meshes"].as_object().unwrap();
    assert_eq!(meshes.len(), 8);
    for (mesh, expected) in meshes {
        let family = cache
            .material_binding_subsets(&scene, store.path(mesh), Time::Default)
            .unwrap_or_else(|error| panic!("{mesh}: {error}"));
        assert_eq!(family.family_type, expected["family_type"], "{mesh}");
        let material = |bound: &layerstack_schemas::BoundMaterial| {
            serde_json::to_value(bound.material.map(display)).unwrap()
        };
        assert_eq!(material(&family.fallback), expected["fallback"], "{mesh}");
        let subsets: Vec<_> = family
            .subsets
            .iter()
            .map(|subset| {
                serde_json::json!({
                    "path": display(subset.path),
                    "indices": *subset.indices,
                    "material": material(&subset.material),
                })
            })
            .collect();
        assert_eq!(
            serde_json::Value::from(subsets),
            expected["subsets"],
            "{mesh}"
        );
        // A family without members has nothing to validate; C++ reports it
        // invalid for having no indices.
        if !expected["members"].as_array().unwrap().is_empty() {
            assert_eq!(
                family.validation.is_valid(),
                expected["valid"],
                "{mesh}: {:?}",
                family.validation
            );
        } else {
            assert!(family.validation.is_valid());
        }
    }
}

/// `CreateMaterialBindSubset` and `SetMaterialBindSubsetsFamilyType`
/// replayed call by call, each applied before the next, against the C++
/// accept/reject results, returned paths and family types.
#[test]
fn material_bind_subset_authoring_matches_openusd_call_by_call() {
    use layerstack::{EditTarget, LayerId, PropertyKind, Specifier, Variability};
    use layerstack_schemas::{
        SchemaEdit,
        subset::{SubsetAuthoringError, SubsetFamilyType},
        usd_geom::{GeomSubsetElementType, Imageable, ImageableEdit},
        usd_shade::MaterialBindingApiEdit,
    };
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    let expected = &oracle["authoring"];
    let (mut store, mut live) = support::scene(
        r#"#usda 1.0
def Mesh "M" (prepend apiSchemas = ["MaterialBindingAPI"])
{
    int[] faceVertexCounts = [3, 3, 3, 3]
    int[] faceVertexIndices = [0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
}
"#,
    );
    let mesh = store.path("/M");
    enum Call {
        Create(&'static str, &'static [i32]),
        SetType(SubsetFamilyType),
        GenericUnrestricted,
    }
    let calls = [
        ("create red [0, 1]", Call::Create("red", &[0, 1])),
        ("create red [2]", Call::Create("red", &[2])),
        (
            "set unrestricted",
            Call::SetType(SubsetFamilyType::Unrestricted),
        ),
        ("set partition", Call::SetType(SubsetFamilyType::Partition)),
        ("create blue [3]", Call::Create("blue", &[3])),
        ("", Call::GenericUnrestricted),
        ("create green [0]", Call::Create("green", &[0])),
        (
            "set nonOverlapping",
            Call::SetType(SubsetFamilyType::NonOverlapping),
        ),
    ];
    let mut log = Vec::new();
    for (label, call) in calls {
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let api = MaterialBindingApiEdit::new(&edit, mesh)
            .expect("outside any instance")
            .unwrap();
        let created = match call {
            Call::Create(name, indices) => Some(
                api.create_material_bind_subset(
                    &mut edit,
                    name,
                    GeomSubsetElementType::Face,
                    indices,
                )
                .map(|subset| subset.path()),
            ),
            Call::SetType(family_type) => {
                let result = api.set_material_bind_subsets_family_type(&mut edit, family_type);
                if family_type == SubsetFamilyType::Unrestricted {
                    assert_eq!(
                        result,
                        Err(SubsetAuthoringError::UnrestrictedMaterialBindFamily)
                    );
                    assert!(edit.transaction().is_empty());
                }
                result.err().map(Err)
            }
            Call::GenericUnrestricted => {
                ImageableEdit::new(&edit, mesh)
                    .expect("outside any instance")
                    .unwrap()
                    .set_subset_family_type(
                        &mut edit,
                        "materialBind",
                        SubsetFamilyType::Unrestricted,
                    )
                    .unwrap();
                None
            }
        };
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        if label.is_empty() {
            continue;
        }
        let scene = Scene::new(live.stage(), &store);
        let family = Imageable::new(&scene, mesh)
            .unwrap()
            .subset_family_type("materialBind");
        let entry = match created {
            Some(Err(_)) => serde_json::json!({"call": label, "accepted": false, "result": null}),
            Some(Ok(path)) => {
                let indices = layerstack_schemas::usd_geom::GeomSubset::new(&scene, path)
                    .unwrap()
                    .indices()
                    .unwrap();
                serde_json::json!({"call": label, "accepted": true, "result": {
                    "path": store.paths().display(path, store.tokens()).to_string(),
                    "indices": *indices,
                    "family_type": family,
                }})
            }
            None => serde_json::json!({"call": label, "accepted": true, "result": {
                "family_type": family,
            }}),
        };
        log.push(entry);
    }
    assert_eq!(serde_json::Value::from(log), expected["calls"]);

    let scene = Scene::new(live.stage(), &store);
    let subsets: Vec<_> = Imageable::new(&scene, mesh)
        .unwrap()
        .geom_subsets(None, Some("materialBind"))
        .iter()
        .map(|subset| {
            store
                .paths()
                .display(subset.path(), store.tokens())
                .to_string()
        })
        .collect();
    assert_eq!(serde_json::to_value(subsets).unwrap(), expected["subsets"]);
    let family = store.property_path("/M.subsetFamily:materialBind:familyType");
    let declaration = live
        .view(&store)
        .property(family)
        .and_then(|property| property.declaration())
        .unwrap();
    assert_eq!(declaration.kind, PropertyKind::Attribute);
    assert_eq!(declaration.variability, Variability::Uniform);
    assert_eq!(expected["family_variability"], "Sdf.VariabilityUniform");
    assert_eq!(declaration.custom, expected["family_custom"]);
    let red = live.stage().prim(store.path("/M/red"), &store).unwrap();
    assert_eq!(red.specifier(), Specifier::Def);
    assert_eq!(expected["subset_specifier"], "Sdf.SpecifierDef");
    assert_eq!(red.type_name(), expected["subset_type"].as_str());
}

/// Authoring departs from C++ in two refusals: indices outside the mesh,
/// and a name another prim type holds (C++ authors both, retyping the prim).
/// Several calls in one edit see each other's family type.
#[test]
fn material_bind_subset_authoring_refuses_bad_indices_and_taken_names() {
    use layerstack::{EditTarget, LayerId};
    use layerstack_schemas::{
        SchemaEdit,
        subset::{SubsetAuthoringError, SubsetFamilyType},
        usd_geom::{GeomSubsetElementType as Element, Imageable},
        usd_shade::MaterialBindingApiEdit,
    };
    let (mut store, mut live) = support::scene(
        r#"#usda 1.0
def Mesh "M"
{
    int[] faceVertexCounts = [3, 3]
    int[] faceVertexIndices = [0, 1, 2, 0, 1, 2]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
    def Xform "taken" {}
}
"#,
    );
    let mesh = store.path("/M");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let api = MaterialBindingApiEdit::new(&edit, mesh)
        .expect("outside any instance")
        .unwrap();
    assert_eq!(
        api.create_material_bind_subset(&mut edit, "far", Element::Face, &[2]),
        Err(SubsetAuthoringError::Indices(
            SubsetProblemKind::InvalidIndex(2)
        ))
    );
    let taken = store_path(&mut edit, "/M/taken");
    assert_eq!(
        api.create_material_bind_subset(&mut edit, "taken", Element::Face, &[0]),
        Err(SubsetAuthoringError::NameTaken(taken))
    );
    assert_eq!(
        api.create_material_bind_subset(&mut edit, "a/b", Element::Face, &[0]),
        Err(SubsetAuthoringError::InvalidName)
    );
    assert!(edit.transaction().is_empty(), "refusals collect nothing");
    api.set_material_bind_subsets_family_type(&mut edit, SubsetFamilyType::Partition)
        .unwrap();
    let a = api
        .create_material_bind_subset(&mut edit, "a", Element::Face, &[0])
        .unwrap();
    let again = api
        .create_material_bind_subset(&mut edit, "a", Element::Face, &[0, 1])
        .unwrap();
    assert_eq!(
        a.path(),
        again.path(),
        "a subset defined in this edit is reused"
    );
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let geom = Imageable::new(&scene, mesh).unwrap();
    assert_eq!(geom.subset_family_type("materialBind"), "partition");
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let family = cache
        .material_binding_subsets(&scene, mesh, Time::Default)
        .unwrap();
    assert_eq!(*family.subsets[0].indices, [0, 1]);
    assert!(family.validation.is_valid());
}

fn store_path(edit: &mut layerstack_schemas::SchemaEdit<'_>, path: &str) -> layerstack::PathId {
    edit.prim_path(path).unwrap()
}
