// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained numeric layers exercise ordinary composition, animation and saving.
//! AOUSD Core §16.3.9–§16.3.10, §12.3 and §12.5.

use layerstack::{
    ArrayReadError, AssetResolveError, AssetResolver, InMemoryStore, InterpolationType, LayerId,
    PathInterner, ResolvedAsset, Stage, StageOptions, TokenInterner, TypedArray, Value,
};
use layerstack_usdc::writer::{Spec, SpecForm, Specifier, Value as W, write_crate};
use layerstack_usdc::{DecodeBudget, read_usdc, read_usdc_lazy, read_usdc_lazy_within};
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
fn bytes() -> Arc<[u8]> {
    write_crate(&[
        Spec::new("/", SpecForm::PseudoRoot),
        Spec::new("/P", SpecForm::Prim).with_field("specifier", W::Specifier(Specifier::Def)),
        Spec::new("/P.a", SpecForm::Attribute)
            .with_field("typeName", W::Token("float[]".into()))
            .with_field("default", W::FloatArray(vec![1., 2., 3.])),
        Spec::new("/P.b", SpecForm::Attribute)
            .with_field("typeName", W::Token("float[]".into()))
            .with_field("default", W::FloatArray(vec![1., 2., 3.])),
        Spec::new("/P.anim", SpecForm::Attribute)
            .with_field("typeName", W::Token("float[]".into()))
            .with_field(
                "timeSamples",
                W::TimeSamples(vec![
                    (0., W::FloatArray(vec![0., 2.])),
                    (10., W::FloatArray(vec![10., 12.])),
                    (20., W::FloatArray(vec![20., 22.])),
                ]),
            ),
    ])
    .unwrap()
    .into()
}
#[test]
fn retained_defaults_and_selected_endpoints_share_one_cache() {
    let input = bytes();
    let mut store = InMemoryStore::default();
    let read = read_usdc_lazy(
        Arc::clone(&input),
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .unwrap();
    assert_eq!(read.values.stats().decode_attempts, 0);
    assert_eq!(read.values.stats().live_arrays, 4);
    store.insert_layer(read.assembled.layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        read.values.stats().decode_attempts,
        0,
        "composition must remain structural"
    );
    let a = store.property_path("/P.a");
    let b = store.property_path("/P.b");
    let anim = store.property_path("/P.anim");
    let raw = stage.resolve_property_path(a).unwrap();
    assert_eq!(
        read.values.stats().decode_attempts,
        0,
        "raw query returns retained data"
    );
    let layerstack::ResolvedValue::Scalar(Value::TypedArray(TypedArray::Deferred(_))) = raw.value
    else {
        panic!("retained source");
    };
    assert!(stage.try_resolve_property_path(a).unwrap().is_some());
    assert!(stage.try_resolve_property_path(b).unwrap().is_some());
    assert_eq!(read.values.stats().decode_attempts, 1);
    let held = stage
        .try_resolve_property_path_at_time(anim, 5., InterpolationType::Held)
        .unwrap()
        .unwrap();
    assert_eq!(held.value, Value::from(vec![0_f32, 2.]));
    assert_eq!(read.values.stats().decode_attempts, 2);
    let linear = stage
        .try_resolve_property_path_at_time(anim, 5., InterpolationType::Linear)
        .unwrap()
        .unwrap();
    assert_eq!(linear.value, Value::from(vec![5_f32, 7.]));
    assert_eq!(
        read.values.stats().decode_attempts,
        3,
        "unselected last frame stays encoded"
    );
    let snapshot = store.layers[&LayerId(1)].clone();
    drop(stage);
    drop(store);
    assert_eq!(read.values.stats().materialized_arrays, 3);
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let eager = read_usdc(&input, LayerId(1), &mut tokens, &mut paths, &mut NoAssets).unwrap();
    assert_eq!(snapshot.prims.len(), eager.layer.prims.len());
    drop(read.values);
    drop(input);
    // The last, still encoded frame outlives both the loader and its input.
    let frame = snapshot
        .prims
        .values()
        .flat_map(|prim| &prim.properties)
        .find_map(|property| property.spec.time_samples.as_ref())
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(
        frame
            .1
            .array_ref()
            .unwrap()
            .typed()
            .unwrap()
            .as_float()
            .unwrap(),
        &[20., 22.]
    );
}

#[test]
fn source_budget_failure_is_cached_across_threads() {
    let input = bytes();
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    // First obtain the exact structural cost without materializing arrays.
    let initial = read_usdc_lazy(
        Arc::clone(&input),
        LayerId(1),
        &mut tokens,
        &mut paths,
        &mut NoAssets,
    )
    .unwrap();
    let budget = DecodeBudget::for_input(input.len());
    let structural = budget.remaining() - initial.values.stats().remaining_units;
    let read = read_usdc_lazy_within(
        input,
        LayerId(1),
        &mut tokens,
        &mut paths,
        &mut NoAssets,
        DecodeBudget::with_limit(structural),
    )
    .unwrap();
    let p = paths
        .lookup(&layerstack::Path::root().join(&[tokens.lookup("P").unwrap()]))
        .unwrap();
    let a = tokens.lookup("a").unwrap();
    let Value::TypedArray(array) = read.assembled.layer.prims[&p]
        .property(a)
        .unwrap()
        .default
        .as_ref()
        .unwrap()
    else {
        panic!("array");
    };
    #[cfg(not(target_family = "wasm"))]
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                assert!(matches!(
                    array.try_materialize(),
                    Err(ArrayReadError::BudgetExceeded { .. })
                ));
            });
        }
    });
    #[cfg(target_family = "wasm")]
    for _ in 0..8 {
        assert!(array.try_materialize().is_err());
    }
    assert_eq!(read.values.stats().decode_attempts, 1);
    assert_eq!(read.values.stats().failed_arrays, 1);
}

#[derive(Debug)]
struct Failed(ArrayReadError);
impl layerstack::DeferredArraySource for Failed {
    fn element_kind(&self) -> Value {
        Value::Float(0.)
    }
    fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
        Err(&self.0)
    }
}
fn failed() -> Value {
    Value::TypedArray(TypedArray::Deferred(Arc::new(Failed(
        ArrayReadError::InvalidData("broken selected source".into()),
    ))))
}
#[test]
fn failures_never_select_weaker_defaults_or_hold_a_good_endpoint() {
    use layerstack::{ArrayEdit, ArrayEditOp, Layer, PrimSpec, PropertySpec, SublayerEntry};
    let mut store = InMemoryStore::default();
    let p = store.path("/P");
    let a = store.property_path("/P.a");
    let anim = store.property_path("/P.anim");
    let mut weak = Layer::new(LayerId(2));
    weak.insert_prim(p, PrimSpec::def());
    weak.set_property(
        a,
        PropertySpec::attribute()
            .with_type(layerstack::PropertyType::new(
                "float",
                true,
                Value::Float(0.),
            ))
            .with_default(Value::from(vec![99_f32])),
    );
    let mut strong = Layer::new(LayerId(1));
    strong.sublayers.push(SublayerEntry::new(LayerId(2)));
    strong.insert_prim(p, PrimSpec::def());
    strong.set_property(
        a,
        PropertySpec::attribute()
            .with_type(layerstack::PropertyType::new(
                "float",
                true,
                Value::Float(0.),
            ))
            .with_default(failed()),
    );
    strong.set_property(
        anim,
        PropertySpec::attribute()
            .with_type(layerstack::PropertyType::new(
                "float",
                true,
                Value::Float(0.),
            ))
            .with_time_samples(vec![(0., Value::from(vec![0_f32])), (10., failed())]),
    );
    store.insert_layer(weak);
    store.insert_layer(strong);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert!(stage.try_resolve_property_path(a).is_err());
    assert!(
        stage
            .read_property(a, layerstack::Time::Default, |value| value
                .array_ref()
                .map(|array| array.len()))
            .is_none(),
        "typed reads must not reveal the weak array"
    );
    assert!(
        stage
            .try_resolve_property_path_at_time(anim, 5., InterpolationType::Linear)
            .is_err()
    );
    assert_eq!(
        stage
            .try_resolve_property_path_at_time(anim, 5., InterpolationType::Held)
            .unwrap()
            .unwrap()
            .value,
        Value::from(vec![0_f32])
    );
    let mut to_save = store.layers[&LayerId(1)].clone();
    to_save.sublayers.clear();
    to_save.insert_prim(
        store.path("/"),
        PrimSpec::default().with_children(vec![store.tokens.lookup("P").unwrap()]),
    );
    let error =
        layerstack_usda::save::save_usda(&to_save, &store.tokens, &store.paths).unwrap_err();
    assert!(
        matches!(error, layerstack_usda::save::SaveError::ArrayRead { .. }),
        "{error:?}"
    );
    let mut edits = Layer::new(LayerId(3));
    edits.sublayers.push(SublayerEntry::new(LayerId(1)));
    edits.insert_prim(p, PrimSpec::def());
    edits.set_property(
        a,
        PropertySpec::attribute()
            .with_type(layerstack::PropertyType::new(
                "float",
                true,
                Value::Float(0.),
            ))
            .with_default(Value::ArrayEdit(ArrayEdit {
                ops: vec![ArrayEditOp::Resize { len: 2 }],
            })),
    );
    store.insert_layer(edits);
    let stage = Stage::compose(&mut store, LayerId(3), StageOptions::default());
    assert!(
        stage.try_resolve_property_path(a).is_err(),
        "edit over failed base preserves failure"
    );
}

#[test]
fn every_native_numeric_kind_and_empty_kind_survives_lazy_save() {
    let kinds = [
        "bool", "uchar", "int", "uint", "int64", "uint64", "half", "float", "double", "timecode",
        "float2", "float3", "float4", "double2", "double3", "double4", "half2", "half3", "half4",
        "int2", "int3", "int4", "quatf", "quatd", "quath", "matrix2d", "matrix3d", "matrix4d",
    ];
    let mut text = String::from("#usda 1.0\ndef \"P\" {\n");
    for (n, kind) in kinds.iter().enumerate() {
        text.push_str(&format!(
            "    {kind}[] a{n} = []\n    {kind}[] empty{n} = []\n"
        ));
    }
    text.push_str("}\n");
    let mut imported = layerstack_conformance::save_corpus::Imported::usda(&text);
    let p = imported
        .paths
        .lookup(&layerstack::Path::root().join(&[imported.tokens.lookup("P").unwrap()]))
        .unwrap();
    for (n, _) in kinds.iter().enumerate() {
        let name = imported.tokens.lookup(&format!("a{n}")).unwrap();
        let spec = imported
            .layer
            .prims
            .get_mut(&p)
            .unwrap()
            .properties
            .iter_mut()
            .find(|entry| entry.name == name)
            .unwrap();
        let spec = Arc::make_mut(&mut spec.spec);
        let kind = &spec.type_name.as_ref().unwrap().default_scalar;
        spec.default = Some(Value::array_with_element(vec![kind.clone()], Some(kind)));
    }
    let expected = imported.save_usda().unwrap();
    let input: Arc<[u8]> = imported.save_usdc().unwrap().into();
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let read = read_usdc_lazy(input, LayerId(1), &mut tokens, &mut paths, &mut NoAssets).unwrap();
    assert_eq!(read.values.stats().decode_attempts, 0);
    assert_eq!(read.values.stats().live_arrays, kinds.len() * 2);
    assert_eq!(
        layerstack_usda::save::save_usda(&read.assembled.layer, &tokens, &paths).unwrap(),
        expected
    );
    assert_eq!(read.values.stats().materialized_arrays, kinds.len() * 2);
}

#[test]
fn malformed_payload_is_deferred_and_failure_is_cached() {
    let input = bytes();
    let mut budget = DecodeBudget::for_input(input.len());
    let file = layerstack_usdc::CrateFile::open(&input, &mut budget).unwrap();
    let offset = usize::try_from(
        file.spec("/P.a")
            .unwrap()
            .field("default")
            .unwrap()
            .representation()
            .payload_offset(),
    )
    .unwrap();
    let mut damaged = input.to_vec();
    damaged[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    assert!(read_usdc(&damaged, LayerId(1), &mut tokens, &mut paths, &mut NoAssets).is_err());
    let read = read_usdc_lazy(
        damaged.into(),
        LayerId(1),
        &mut tokens,
        &mut paths,
        &mut NoAssets,
    )
    .unwrap();
    assert_eq!(read.values.stats().decode_attempts, 0);
    let p = paths
        .lookup(&layerstack::Path::root().join(&[tokens.lookup("P").unwrap()]))
        .unwrap();
    let a = tokens.lookup("a").unwrap();
    let Value::TypedArray(array) = read.assembled.layer.prims[&p]
        .property(a)
        .unwrap()
        .default
        .as_ref()
        .unwrap()
    else {
        panic!("array");
    };
    let first = array.try_materialize().unwrap_err();
    assert!(std::ptr::eq(first, array.try_materialize().unwrap_err()));
    assert_eq!(read.values.stats().decode_attempts, 1);
    assert_eq!(read.values.stats().failed_arrays, 1);
}

#[test]
fn deferred_equality_keeps_nan_semantics_and_bitwise_authored_identity() {
    let input: Arc<[u8]> = write_crate(&[
        Spec::new("/", SpecForm::PseudoRoot),
        Spec::new("/P", SpecForm::Prim).with_field("specifier", W::Specifier(Specifier::Def)),
        Spec::new("/P.a", SpecForm::Attribute)
            .with_field("typeName", W::Token("float[]".into()))
            .with_field(
                "default",
                W::FloatArray(vec![f32::from_bits(0x7fc0_002a), -0.]),
            ),
    ])
    .unwrap()
    .into();
    let mut store = InMemoryStore::default();
    let read = read_usdc_lazy(
        input,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .unwrap();
    store.insert_layer(read.assembled.layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let property = store.property_path("/P.a");
    let layerstack::ResolvedValue::Scalar(value) =
        stage.resolve_property_path(property).unwrap().value
    else {
        panic!("array");
    };
    assert_eq!(read.values.stats().decode_attempts, 0);
    assert!(
        value.same_representation(&value),
        "identity checks must not demand payloads"
    );
    assert_eq!(read.values.stats().decode_attempts, 0);
    assert_ne!(
        value,
        value.clone(),
        "NaNs remain unequal under public content equality"
    );
    assert_eq!(read.values.stats().decode_attempts, 1);
    let array = value
        .array_ref()
        .unwrap()
        .typed()
        .unwrap()
        .as_float()
        .unwrap();
    assert_eq!(array[0].to_bits(), 0x7fc0_002a);
    assert_eq!(array[1].to_bits(), (-0_f32).to_bits());
}

#[test]
fn retimed_timecode_arrays_only_demand_the_bracketing_frames() {
    use layerstack::{Layer, LayerOffset, SublayerEntry};
    let input: Arc<[u8]> = write_crate(&[
        Spec::new("/", SpecForm::PseudoRoot),
        Spec::new("/P", SpecForm::Prim).with_field("specifier", W::Specifier(Specifier::Def)),
        Spec::new("/P.clock", SpecForm::Attribute)
            .with_field("typeName", W::Token("timecode[]".into()))
            .with_field(
                "timeSamples",
                W::TimeSamples(vec![
                    (0., W::TimeCodeArray(vec![0.])),
                    (10., W::TimeCodeArray(vec![10.])),
                    (20., W::TimeCodeArray(vec![20.])),
                ]),
            ),
    ])
    .unwrap()
    .into();
    let mut store = InMemoryStore::default();
    let read = read_usdc_lazy(
        input,
        LayerId(2),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .unwrap();
    store.insert_layer(read.assembled.layer);
    let mut root = Layer::new(LayerId(1));
    root.sublayers.push(SublayerEntry {
        layer: LayerId(2),
        offset: LayerOffset {
            offset: 8.,
            scale: 2.,
        },
        asset: None,
    });
    store.insert_layer(root);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let property = store.property_path("/P.clock");
    let held = stage
        .try_resolve_property_path_at_time(property, 18., InterpolationType::Held)
        .unwrap()
        .unwrap();
    assert_eq!(
        held.value
            .array_ref()
            .unwrap()
            .typed()
            .unwrap()
            .as_timecode()
            .unwrap(),
        &[8.]
    );
    assert_eq!(read.values.stats().decode_attempts, 1);
    let linear = stage
        .try_resolve_property_path_at_time(property, 18., InterpolationType::Linear)
        .unwrap()
        .unwrap();
    assert_eq!(
        linear
            .value
            .array_ref()
            .unwrap()
            .typed()
            .unwrap()
            .as_timecode()
            .unwrap(),
        &[18.]
    );
    assert_eq!(read.values.stats().decode_attempts, 2);
    assert_eq!(
        read.values.stats().materialized_arrays,
        2,
        "third frame remains encoded"
    );
}

#[test]
fn mixed_array_storage_holds_without_demanding_unselected_endpoints() {
    use layerstack::{DeferredArraySource, Layer, PrimSpec, PropertySpec, PropertyType};
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Debug)]
    struct Failed {
        kind: Value,
        calls: AtomicUsize,
    }
    impl DeferredArraySource for Failed {
        fn element_kind(&self) -> Value {
            self.kind.clone()
        }
        fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            static ERROR: ArrayReadError = ArrayReadError::BudgetExceeded { limit: 0 };
            Err(&ERROR)
        }
    }
    for (name, lower, upper) in [
        ("int", Value::Int(1), Value::Int(0)),
        ("bool", Value::Bool(true), Value::Bool(false)),
        ("float", Value::Float(1.), Value::Double(0.)),
        ("float", Value::Float(1.), Value::Int(0)),
        ("double", Value::Double(1.), Value::Double(0.)),
    ] {
        let source = Arc::new(Failed {
            kind: upper,
            calls: AtomicUsize::new(0),
        });
        let mut store = InMemoryStore::default();
        let p = store.path("/P");
        let x = store.property_path("/P.x");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(p, PrimSpec::def());
        let expected = Value::Array(vec![lower.clone()]);
        layer.set_property(
            x,
            PropertySpec::attribute()
                .with_type(PropertyType::new(name, true, lower))
                .with_time_samples(vec![
                    (0., expected.clone()),
                    (10., Value::TypedArray(TypedArray::Deferred(source.clone()))),
                ]),
        );
        store.insert_layer(layer);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let held = stage
            .try_resolve_property_path_at_time(x, 5., InterpolationType::Held)
            .unwrap()
            .unwrap();
        assert_eq!(held.value, expected);
        assert_eq!(source.calls.load(Ordering::Relaxed), 0);
        let linear = stage.try_resolve_property_path_at_time(x, 5., InterpolationType::Linear);
        if name == "double" {
            assert!(
                linear.is_err(),
                "selected floating endpoint must report failure"
            );
            assert!(source.calls.load(Ordering::Relaxed) > 0);
        } else {
            assert_eq!(linear.unwrap().unwrap().value, expected);
            assert_eq!(
                source.calls.load(Ordering::Relaxed),
                0,
                "upper endpoint is unselected"
            );
        }
    }
}
