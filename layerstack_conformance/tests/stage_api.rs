// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage APIs agree with native OpenUSD and fresh retained snapshots.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    FieldEntry, FieldValue, InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, Value,
};
use std::{process::Command, sync::Arc};

#[test]
fn fallback_types_select_concrete_schemas_without_changing_authored_type() {
    let mut store = InMemoryStore::default();
    let registry = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let future = store.tokens.intern("FutureMesh");
    let mesh = store.tokens.intern("Mesh");
    let unknown = store.tokens.intern("OtherFuture");
    let abstract_type = store.tokens.intern("Imageable");
    let sphere = store.tokens.intern("Sphere");
    let fallback = store.tokens.intern("fallbackPrimTypes");
    let prim = store.path("/Future");
    let known = store.path("/Known");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(prim, PrimSpec::def().with_type_name(future));
    layer.insert_prim(known, PrimSpec::def().with_type_name(mesh));
    layer.metadata.push(FieldEntry {
        name: fallback,
        value: FieldValue::Value(Value::Dictionary(vec![
            (
                "FutureMesh".into(),
                Value::Array(vec![
                    Value::Token(unknown),
                    Value::Token(abstract_type),
                    Value::Token(mesh),
                ]),
            ),
            ("Mesh".into(), Value::Array(vec![Value::Token(sphere)])),
        ])),
    });
    store.insert_layer(layer);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(registry),
            ..Default::default()
        },
    );
    assert_eq!(stage.resolve_type_name(prim, &store), Some(future));
    assert!(stage.prim_definition_ref(prim).unwrap().is_a(mesh));
    assert!(stage.prim_definition_ref(known).unwrap().is_a(mesh));
    assert!(!stage.prim_definition_ref(known).unwrap().is_a(sphere));
    let subdivision = store.tokens.lookup("subdivisionScheme").unwrap();
    assert_eq!(
        stage
            .resolve_field_with_schema(prim, subdivision, &store)
            .unwrap()
            .value,
        Value::Token(store.tokens.lookup("catmullClark").unwrap())
    );
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .status()
        .is_ok_and(|s| s.success())
    {
        return;
    }
    let output = Command::new(python).args(["-c", r#"
from pxr import Usd, UsdGeom, Vt
s=Usd.Stage.CreateInMemory()
s.SetMetadata('fallbackPrimTypes', {'FutureMesh':Vt.TokenArray(['OtherFuture','Imageable','Mesh']), 'Mesh':Vt.TokenArray(['Sphere'])})
p=s.DefinePrim('/Future','FutureMesh'); k=s.DefinePrim('/Known','Mesh')
assert p.GetTypeName()=='FutureMesh' and p.IsA(UsdGeom.Mesh)
assert k.IsA(UsdGeom.Mesh) and not k.IsA(UsdGeom.Sphere)
assert p.GetAttribute('subdivisionScheme').Get()=='catmullClark'
"#]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stage_metadata_edits_are_atomic_field_scoped_and_session_aware() {
    use layerstack::{EditTarget, LiveStage, Specifier, StageMetadataError, Transaction};
    let mut store = InMemoryStore::default();
    let prim = store.path("/World");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(prim, PrimSpec::def());
    store.insert_layer(root);
    store.insert_layer(Layer::new(LayerId(2)));
    store.insert_layer(Layer::new(LayerId(3)));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            session_layer: Some(LayerId(2)),
            ..Default::default()
        },
    );
    let data = store.tokens.intern("customLayerData");
    let comment = store.tokens.intern("comment");
    let root_edit = live
        .set_stage_metadata_dict_key(
            &mut store,
            LayerId(1),
            data,
            "render:exposure",
            Some(Value::Double(1.0)),
        )
        .unwrap();
    live.set_stage_metadata(
        &mut store,
        LayerId(1),
        comment,
        FieldValue::Value(Value::string("unrelated")),
    )
    .unwrap();
    live.apply(&mut store, &root_edit.inverse).unwrap();
    assert!(store.layers[&LayerId(1)].metadata(data).is_none());
    assert!(store.layers[&LayerId(1)].metadata(comment).is_some());
    let generation = store.layers[&LayerId(1)].generation();
    live.set_stage_metadata_dict_key(&mut store, LayerId(1), data, "absent:key", None)
        .unwrap();
    assert_eq!(store.layers[&LayerId(1)].generation(), generation);
    assert!(matches!(
        live.set_stage_metadata_dict_key(&mut store, LayerId(3), data, "key", None),
        Err(StageMetadataError::InvalidLayer(LayerId(3)))
    ));
    live.set_stage_metadata_dict_key(
        &mut store,
        LayerId(1),
        data,
        "render:exposure",
        Some(Value::Double(1.0)),
    )
    .unwrap();
    live.set_stage_metadata_dict_key(
        &mut store,
        LayerId(2),
        data,
        "render:quality",
        Some(Value::Int(4)),
    )
    .unwrap();
    assert_eq!(
        live.stage()
            .metadata_dict_key(data, "render:exposure", &store),
        Some(Value::Double(1.0))
    );
    assert!(
        live.stage()
            .has_authored_metadata_dict_key(data, "render:quality", &store)
    );
    live.set_start_time_code(&mut store, LayerId(1), 10.0)
        .unwrap();
    live.set_end_time_code(&mut store, LayerId(2), 50.0)
        .unwrap();
    assert!(live.stage().has_authored_time_code_range(&store));
    assert_eq!(live.stage().start_time_code(&store), 10.0);
    assert_eq!(live.stage().end_time_code(&store), 50.0);
    live.set_frames_per_second(&mut store, LayerId(2), 48.0)
        .unwrap();
    assert_eq!(live.stage().time_codes_per_second(&store), 48.0);
    let name = store.tokens.intern("World");
    let default = live.set_default_prim(&mut store, Some(name)).unwrap();
    assert_eq!(live.stage().default_prim(&mut store), Some(prim));
    live.apply(&mut store, &default.inverse).unwrap();
    assert!(!live.stage().has_authored_default_prim(&store));
    let mut rejected = Transaction::new();
    rejected.set_layer_metadata(
        LayerId(1),
        comment,
        FieldValue::Value(Value::string("rolled back")),
    );
    rejected.create_prim(
        EditTarget::for_layer(LayerId(1)).prim(prim),
        Specifier::Def,
        None,
    );
    let before = store.layers[&LayerId(1)].clone();
    assert!(live.apply(&mut store, &rejected).is_err());
    assert_eq!(store.layers[&LayerId(1)], before);
    let mut guard = Transaction::new();
    guard.expect_layer_metadata(LayerId(1), comment, None);
    guard.clear_layer_metadata(LayerId(1), comment);
    assert!(guard.apply(&mut store).is_err());
}

#[test]
fn fallback_metadata_changes_rebuild_schema_definitions_and_undo() {
    use layerstack::{LiveStage, Transaction};
    let mut store = InMemoryStore::default();
    let registry = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let name = store.tokens.intern("Future");
    let mesh = store.tokens.intern("Mesh");
    let key = store.tokens.intern("fallbackPrimTypes");
    let prim = store.path("/Object");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(prim, PrimSpec::def().with_type_name(name));
    store.insert_layer(root);
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(registry),
            ..Default::default()
        },
    );
    assert!(!live.stage().prim_definition_ref(prim).unwrap().is_a(mesh));
    let mut edit = Transaction::new();
    edit.set_layer_metadata(
        LayerId(1),
        key,
        FieldValue::Value(Value::Dictionary(vec![(
            "Future".into(),
            Value::Array(vec![Value::Token(mesh)]),
        )])),
    );
    let inverse = live.apply(&mut store, &edit).unwrap().inverse;
    assert!(live.stage().prim_definition_ref(prim).unwrap().is_a(mesh));
    live.apply(&mut store, &inverse).unwrap();
    assert!(!live.stage().prim_definition_ref(prim).unwrap().is_a(mesh));
}

fn source_store(source: &str) -> InMemoryStore {
    struct NoAssets;
    impl layerstack::AssetResolver for NoAssets {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<LayerId>,
            _: &mut layerstack::TokenInterner,
            _: &mut layerstack::PathInterner,
        ) -> Result<layerstack::ResolvedAsset, layerstack::AssetResolveError> {
            Err(layerstack::AssetResolveError::NotFound)
        }
        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
    }
    let mut store = InMemoryStore::default();
    let result = layerstack_usda::read_usda(
        source,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(!result.emitted.rejected, "{:?}", result.emitted.diagnostics);
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    store.insert_layer(result.emitted.layer);
    store
}

#[test]
fn predicate_ranges_prune_and_balance_visits_with_explicit_instance_proxies() {
    use layerstack::PrimPredicate;
    let source = r#"#usda 1.0
def "A" {
    def "Child" {
        def "Leaf" {}
    }
}
over "Over" {
    def "Hidden" {}
}
class "Class" {
    def "Hidden" {}
}
def "Off" (active = false) {
    def "Hidden" {}
}
def "Prototype" {
    def "Leaf" {}
}
def "Instance" (
    instanceable = true
    prepend references = </Prototype>
) {}
"#;
    let mut store = source_store(source);
    let root = store.path("/");
    let a = store.path("/A");
    let child = store.path("/A/Child");
    let leaf = store.path("/A/Child/Leaf");
    let proxy = store.path("/Instance/Leaf");
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let default: Vec<_> = stage
        .prim_range(root, &store, PrimPredicate::DEFAULT)
        .collect();
    assert!(default.contains(&leaf));
    assert!(!default.contains(&proxy));
    assert!(!default.contains(&store.path("/Over/Hidden")));
    assert!(!default.contains(&store.path("/Class/Hidden")));
    let proxy_predicate = PrimPredicate {
        instance_proxies: true,
        ..PrimPredicate::ALL
    };
    assert!(
        stage
            .prim_range(root, &store, proxy_predicate)
            .any(|p| p == proxy)
    );
    assert!(stage.prim_status(proxy, &store).unwrap().instance_proxy);
    let mut range = stage.prim_range(a, &store, PrimPredicate::DEFAULT);
    assert!(!range.prune_children());
    assert_eq!(range.next(), Some(a));
    assert_eq!(range.next(), Some(child));
    assert!(range.prune_children());
    assert_eq!(range.next(), None);
    assert!(!range.prune_children());
    let mut visits = stage
        .prim_range(a, &store, PrimPredicate::DEFAULT)
        .pre_and_post();
    assert_eq!(visits.next().unwrap().prim, a);
    assert_eq!(visits.next().unwrap().prim, child);
    assert!(visits.prune_children());
    let exit = visits.next().unwrap();
    assert_eq!(exit.prim, child);
    assert!(exit.is_post_visit);
    assert!(!visits.prune_children());
    assert_eq!(visits.next().unwrap().prim, a);
    assert!(visits.next().is_none());
    // Compare the default and explicit proxy namespaces to native C++ USD.
    let actual: Vec<_> = default
        .into_iter()
        .filter(|p| *p != root)
        .map(|p| store.paths.resolve(p).display(&store.tokens))
        .collect();
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .status()
        .is_ok_and(|s| s.success())
    {
        return;
    }
    let output = Command::new(python).args(["-c", "from pxr import Usd; import json,sys; s=Usd.Stage.CreateInMemory(); assert s.GetRootLayer().ImportFromString(sys.argv[1]); print(json.dumps([str(p.GetPath()) for p in s.Traverse()]))", source]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn mask_expansion_follows_connections_and_relationships_to_a_fixed_point() {
    use layerstack::{LiveStage, PopulationMask, PrimPredicate};
    let source = r#"#usda 1.0
def "A" {
    rel links = </B>
    rel skip = </D>
}
def "B" {
    float input.connect = </C.output>
}
def "C" {
    rel links = </A>
    float output = 1
}
def "D" {}
"#;
    let mut store = source_store(source);
    let a = store.path("/A");
    let b = store.path("/B");
    let c = store.path("/C");
    let d = store.path("/D");
    let skip = store.tokens.intern("skip");
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            mask: Some(PopulationMask { include: vec![a] }),
            ..Default::default()
        },
    );
    assert!(!live.stage().has_prim(b));
    let report = live.expand_population_mask_with(
        &mut store,
        PrimPredicate::DEFAULT,
        |_, _, property| property.property() != skip,
        |_, _, _| true,
    );
    assert_eq!(report.added_roots, vec![b, c]);
    assert_eq!(report.changes.len(), 2);
    assert!(live.stage().has_prim(c));
    assert!(!live.stage().has_prim(d));
    let again = live.expand_population_mask_with(
        &mut store,
        PrimPredicate::DEFAULT,
        |_, _, property| property.property() != skip,
        |_, _, _| true,
    );
    assert!(again.added_roots.is_empty());
    assert!(again.changes.is_empty());
    let unfiltered = live.expand_population_mask(&mut store);
    assert_eq!(unfiltered.added_roots, vec![d]);
    assert!(live.stage().has_prim(d));
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .status()
        .is_ok_and(|s| s.success())
    {
        return;
    }
    let output = Command::new(python).args(["-c", "from pxr import Usd,Sdf; import sys; l=Sdf.Layer.CreateAnonymous(); assert l.ImportFromString(sys.argv[1]); s=Usd.Stage.OpenMasked(l, Usd.StagePopulationMask(['/A'])); s.ExpandPopulationMask(lambda r:r.GetName()!='skip', lambda a:True); assert [str(p.GetPath()) for p in s.Traverse()]==['/A','/B','/C']; s.ExpandPopulationMask(); assert [str(p.GetPath()) for p in s.Traverse()]==['/A','/B','/C','/D']", source]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn selected_relationships_forward_through_unselected_relationship_properties() {
    use layerstack::{LiveStage, PopulationMask, PrimPredicate};
    let mut store = source_store(
        "#usda 1.0\ndef \"A\" {\n rel links = </B.forward>\n}\ndef \"B\" {\n rel forward = </C>\n}\ndef \"C\" {}\n",
    );
    let a = store.path("/A");
    let c = store.path("/C");
    let links = store.tokens.intern("links");
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            mask: Some(PopulationMask { include: vec![a] }),
            ..Default::default()
        },
    );
    live.expand_population_mask_with(
        &mut store,
        PrimPredicate::DEFAULT,
        |_, _, p| p.property() == links,
        |_, _, _| false,
    );
    assert!(live.stage().has_prim(c));
}
#[test]
fn generic_object_queries_retain_values_across_unrelated_edits_and_refresh_after_changes() {
    use layerstack::{
        AttributeQuery, EditTarget, LiveStage, Object, PropertySpec, Time, Transaction,
    };
    let mut store = source_store(
        "#usda 1.0\ndef \"A\" {\n int value = 1\n rel links = </B.forward>\n}\ndef \"B\" {\n int value = 2\n rel forward = </A>\n}\n",
    );
    let a = store.path("/A");
    let a_value = store.property_path("/A.value");
    let b_value = store.property_path("/B.value");
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let prim = live.stage().prim(a, &store).unwrap();
    assert!(prim.attribute("links").is_none());
    assert!(prim.relationship("value").is_none());
    assert_eq!(
        prim.relationship("links").unwrap().forwarded_targets(),
        vec![layerstack::TargetPath::Prim(a)]
    );
    assert!(matches!(
        live.stage()
            .object(layerstack::TargetPath::Property(a_value), &store),
        Some(Object::Attribute(_))
    ));
    let mut query = AttributeQuery::new(a_value);
    assert_eq!(
        query.get(live.stage(), Time::Default).unwrap().value,
        Value::Int(1)
    );
    assert_eq!(
        query.get(live.stage(), Time::Default).unwrap().value,
        Value::Int(1)
    );
    assert_eq!(query.work().cache_hits, 1);
    let mut unrelated = Transaction::new();
    unrelated.set_default(
        EditTarget::for_layer(LayerId(1)).property(b_value),
        Value::Int(9),
    );
    live.apply(&mut store, &unrelated).unwrap();
    query.get(live.stage(), Time::Default);
    assert_eq!(query.work().cache_hits, 2);
    let mut relevant = Transaction::new();
    relevant.set_default(
        EditTarget::for_layer(LayerId(1)).property(a_value),
        Value::Int(4),
    );
    let applied = live.apply(&mut store, &relevant).unwrap();
    assert_eq!(
        query.get(live.stage(), Time::Default).unwrap().value,
        Value::Int(4)
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(
        query.get(live.stage(), Time::Default).unwrap().value,
        Value::Int(1)
    );
    // A direct replacement and a fresh snapshot cannot reuse old source identity.
    store.layers.get_mut(&LayerId(1)).unwrap().set_property(
        a_value,
        PropertySpec::attribute().with_default(Value::Int(8)),
    );
    live.synchronize(&mut store);
    assert_eq!(
        query.get(live.stage(), Time::Default).unwrap().value,
        Value::Int(8)
    );
    let fresh = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    query.get(&fresh, Time::Default);
    assert_eq!(query.work().evaluations, 5);
}

#[test]
fn checked_object_queries_preserve_deferred_decode_failures_and_cache_them() {
    use layerstack::{
        ArrayReadError, AttributeQuery, DeferredArraySource, PropertySpec, PropertyType, Time,
        TypedArray,
    };
    #[derive(Debug)]
    struct Failed(ArrayReadError);
    impl DeferredArraySource for Failed {
        fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
            Err(&self.0)
        }
        fn element_kind(&self) -> Value {
            Value::Float(0.0)
        }
    }
    let mut store = InMemoryStore::default();
    let property = store.property_path("/A.values");
    let error = ArrayReadError::InvalidData("invalid backing data".into());
    let mut layer = Layer::new(LayerId(1));
    layer.set_property(
        property,
        PropertySpec::typed_attribute(PropertyType::new("float", true, Value::Float(0.0)))
            .with_default(Value::TypedArray(TypedArray::Deferred(Arc::new(Failed(
                error.clone(),
            ))))),
    );
    store.insert_layer(layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let attribute = stage
        .prim(property.prim_path(), &store)
        .unwrap()
        .attribute("values")
        .unwrap();
    assert_eq!(attribute.try_get(Time::Default), Err(error.clone()));
    let mut query = AttributeQuery::new(property);
    assert_eq!(query.try_get(&stage, Time::Default), Err(error.clone()));
    assert_eq!(query.try_get(&stage, Time::Default), Err(error));
    assert_eq!(query.work().evaluations, 1);
    assert_eq!(query.work().cache_hits, 1);
}
#[test]
fn schema_property_kind_overrides_conflicting_authored_kind() {
    let mut store = source_store("#usda 1.0\ndef Sphere \"S\" {\n rel radius = </Other>\n}\n");
    let registry = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let prim = store.path("/S");
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(registry),
            ..Default::default()
        },
    );
    let view = stage.prim(prim, &store).unwrap();
    assert!(view.attribute("radius").is_some());
    assert!(view.relationship("radius").is_none());
}

#[test]
fn checked_numeric_queries_decode_only_the_selected_interpolation_endpoints() {
    use layerstack::{
        ArrayReadError, AttributeQuery, DeferredArraySource, InterpolationType, PropertySpec,
        PropertyType, Time, TypedArray,
    };
    #[derive(Debug)]
    struct Failed(ArrayReadError);
    impl DeferredArraySource for Failed {
        fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
            Err(&self.0)
        }
        fn element_kind(&self) -> Value {
            Value::Float(0.0)
        }
    }
    let mut store = InMemoryStore::default();
    let property = store.property_path("/A.values");
    let bad = Value::TypedArray(TypedArray::Deferred(Arc::new(Failed(
        ArrayReadError::InvalidData("invalid upper endpoint".into()),
    ))));
    let good = Value::TypedArray(TypedArray::Float(Arc::new(vec![1.0])));
    let mut layer = Layer::new(LayerId(1));
    layer.set_property(
        property,
        PropertySpec::typed_attribute(PropertyType::new("float", true, Value::Float(0.0)))
            .with_time_samples(vec![(0.0, good), (1.0, bad)]),
    );
    store.insert_layer(layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let mut query = AttributeQuery::new(property);
    assert!(
        query
            .try_get(
                &stage,
                Time::At {
                    code: 0.5,
                    interpolation: InterpolationType::Linear
                }
            )
            .is_err()
    );
    assert!(
        query
            .try_get(
                &stage,
                Time::At {
                    code: 0.5,
                    interpolation: InterpolationType::Held
                }
            )
            .unwrap()
            .is_some()
    );
    assert!(
        query
            .try_get(
                &stage,
                Time::At {
                    code: 0.0,
                    interpolation: InterpolationType::Linear
                }
            )
            .unwrap()
            .is_some()
    );
}
