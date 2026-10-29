// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{
    DeferredArraySource, DeferredValue, DeferredValueSource, FieldValue, InMemoryStore, Layer,
    PrimSpec, PropertySpec, PropertyType, Reference, StageOptions, SublayerEntry, TypedArray,
    spline::{CurveType, Extrapolation, SplineDataType},
};
use alloc::vec;

fn fixture(spec: PropertySpec) -> (Stage, InMemoryStore, PropertyPath) {
    let mut store = InMemoryStore::default();
    let path = store.path("/P");
    let name = store.tokens.intern("x");
    let mut root = Layer::new(LayerId(1));
    root.sublayers.push(SublayerEntry::new(LayerId(2)));
    root.insert_prim(path, PrimSpec::def().with_property(name, spec));
    store.insert_layer(root);
    store.insert_layer(Layer::new(LayerId(2)));
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    (stage, store, PropertyPath::new(path, name))
}

#[test]
fn specless_layer_targets_have_strength_and_default_queries_ignore_samples() {
    let (stage, mut store, path) =
        fixture(PropertySpec::attribute().with_time_samples(vec![(0., Value::Double(4.))]));
    let original = store.layers.clone();
    let target = EditTarget::for_layer(LayerId(2));
    let default = stage
        .inspect_property_authoring(&mut store, path, &target, AuthoringQuery::default_value())
        .unwrap();
    assert!(default.is_strongest());
    assert_eq!(default.target.unwrap().layer_strength, 1);
    let numeric = stage
        .inspect_property_authoring(&mut store, path, &target, AuthoringQuery::time_sample(10.))
        .unwrap();
    assert!(!numeric.is_strongest());
    assert_eq!(
        numeric.stronger_sources[0].kind,
        AuthoringSourceKind::TimeSamples
    );
    assert_eq!(
        store.layers, original,
        "inspection does not author missing specs"
    );
}

#[test]
fn dense_sparse_and_animation_sources_are_distinct() {
    for (value, expected) in [
        (Value::Double(2.), SourceComposability::Dense),
        (Value::Blocked, SourceComposability::Dense),
        (
            Value::Dictionary(vec![("a".into(), Value::Int(1).into())]),
            SourceComposability::Composable,
        ),
        (
            Value::PathExpression("%_ /P".into()),
            SourceComposability::Composable,
        ),
    ] {
        let (stage, mut store, path) = fixture(PropertySpec::attribute().with_default(value));
        let result = stage
            .inspect_property_authoring(
                &mut store,
                path,
                &EditTarget::for_layer(LayerId(2)),
                AuthoringQuery::default_value(),
            )
            .unwrap();
        assert_eq!(result.stronger_sources[0].composability, expected);
    }
    let (stage, mut store, path) =
        fixture(PropertySpec::attribute().with_default(Value::AnimationBlock));
    let target = EditTarget::for_layer(LayerId(2));
    assert!(
        stage
            .inspect_property_authoring(&mut store, path, &target, AuthoringQuery::default_value())
            .unwrap()
            .is_strongest()
    );
    let numeric = stage
        .inspect_property_authoring(&mut store, path, &target, AuthoringQuery::time_sample(0.))
        .unwrap();
    assert_eq!(
        numeric.stronger_sources[0].kind,
        AuthoringSourceKind::AnimationBlock
    );
}

#[test]
fn referenced_specless_target_retains_source_namespace_and_stronger_provenance() {
    let mut store = InMemoryStore::default();
    let dest = store.path("/Use");
    let source = store.path("/Asset");
    let x = store.tokens.intern("x");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        dest,
        PrimSpec::def()
            .with_reference(Reference::new(LayerId(2), source))
            .with_property(x, PropertySpec::attribute().with_default(Value::Double(9.))),
    );
    let mut asset = Layer::new(LayerId(2));
    asset.sublayers.push(SublayerEntry::new(LayerId(3)));
    asset.insert_prim(source, PrimSpec::def());
    store.insert_layer(root);
    store.insert_layer(asset);
    store.insert_layer(Layer::new(LayerId(3)));
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let node = stage
        .explain_prim_graph(dest)
        .unwrap()
        .nodes()
        .find(|(_, node)| node.arc_kind() == ArcKind::References)
        .unwrap()
        .0;
    let target = EditTarget::for_node_layer(&stage, &store, dest, node, LayerId(3)).unwrap();
    let result = stage
        .inspect_property_authoring(
            &mut store,
            PropertyPath::new(dest, x),
            &target,
            AuthoringQuery::default_value(),
        )
        .unwrap();
    let site = result.target.unwrap();
    assert_eq!(site.layer_id, LayerId(3));
    assert_eq!(site.lookup_path, source);
    assert_eq!(result.stronger_sources[0].key.layer_id, LayerId(1));
    assert_eq!(result.stronger_sources[0].key.spec_path.prim_path(), dest);
}

#[test]
fn inactive_targets_and_invalid_queries_cannot_be_reported_strongest() {
    let (stage, mut store, path) = fixture(PropertySpec::attribute());
    store.insert_layer(Layer::new(LayerId(3)));
    let result = stage
        .inspect_property_authoring(
            &mut store,
            path,
            &EditTarget::for_layer(LayerId(3)),
            AuthoringQuery::default_value(),
        )
        .unwrap();
    assert!(result.target.is_none());
    assert!(!result.is_strongest());
    assert_eq!(
        stage.inspect_property_authoring(
            &mut store,
            path,
            &EditTarget::for_layer(LayerId(1)),
            AuthoringQuery::time_sample(f64::NAN)
        ),
        Err(AuthoringInspectionError::InvalidTime)
    );
}

#[test]
fn variant_targets_participate_only_when_the_branch_is_selected() {
    let mut store = InMemoryStore::default();
    let path = store.path("/P");
    let x = store.tokens.intern("x");
    let mode = store.tokens.intern("mode");
    let selected = store.tokens.intern("selected");
    let hidden = store.tokens.intern("hidden");
    let mut prim = PrimSpec::def();
    prim.rare_mut()
        .variant_set_names
        .make_mut()
        .prepend
        .push(mode);
    prim.rare_mut().variant_selections.insert(mode, selected);
    prim.rare_mut().variant_sets.insert(
        mode,
        crate::VariantSetSpec {
            variants: crate::Variants::from([
                (selected, crate::VariantSpec::default()),
                (hidden, crate::VariantSpec::default()),
            ]),
        },
    );
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(path, prim);
    store.insert_layer(root);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let selected_spec = SpecPath::parse("/P{mode=selected}", &store.tokens, &store.paths).unwrap();
    let hidden_spec = SpecPath::parse("/P{mode=hidden}", &store.tokens, &store.paths).unwrap();
    for (spec, active) in [(selected_spec, true), (hidden_spec, false)] {
        let target = EditTarget::for_local_variant(LayerId(1), &spec);
        let result = stage
            .inspect_property_authoring(
                &mut store,
                PropertyPath::new(path, x),
                &target,
                AuthoringQuery::default_value(),
            )
            .unwrap();
        assert_eq!(result.target.is_some(), active);
        assert_eq!(result.is_strongest(), active);
    }
}

#[derive(Debug)]
struct NeverDecode;
impl DeferredArraySource for NeverDecode {
    fn materialize(&self) -> Result<&TypedArray, &crate::ArrayReadError> {
        panic!("authoring inspection must not decode numeric storage")
    }
    fn element_kind(&self) -> Value {
        Value::Float(0.)
    }
}

#[test]
fn sample_inspection_is_metadata_only_even_for_retained_arrays() {
    let retained = Value::TypedArray(TypedArray::Deferred(Arc::new(NeverDecode)));
    let sparse = Value::ArrayEdit(crate::ArrayEdit::default());
    let (stage, mut store, path) =
        fixture(PropertySpec::attribute().with_time_samples(vec![(0., retained), (10., sparse)]));
    let target = EditTarget::for_layer(LayerId(2));
    let held = stage
        .inspect_property_authoring(
            &mut store,
            path,
            &target,
            AuthoringQuery::time_sample(5.).held(),
        )
        .unwrap();
    assert_eq!(
        held.stronger_sources[0].composability,
        SourceComposability::Dense
    );
    // Interpolating, the sparse upper sample composes over the write
    // (`_GetSamplesComposability` chains when either bracket composes).
    let linear = stage
        .inspect_property_authoring(&mut store, path, &target, AuthoringQuery::time_sample(5.))
        .unwrap();
    assert_eq!(
        linear.stronger_sources[0].composability,
        SourceComposability::Composable
    );
    assert_eq!(linear.spans[0].verdict, AuthoringVerdict::ComposedOver);
}

#[test]
fn instance_proxy_edits_cannot_reintroduce_local_opinions() {
    let mut store = InMemoryStore::default();
    let use_ = store.path("/Use");
    let proxy = store.path("/Use/Child");
    let asset = store.path("/Asset");
    let source = store.path("/Asset/Child");
    let child = store.tokens.intern("Child");
    let x = store.tokens.intern("x");
    let mut instance = PrimSpec::def().with_reference(Reference::new(LayerId(2), asset));
    instance.instanceable = Some(true);
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(use_, instance);
    let mut referenced = Layer::new(LayerId(2));
    referenced.insert_prim(asset, PrimSpec::def().with_children(vec![child]));
    referenced.insert_prim(
        source,
        PrimSpec::def().with_property(x, PropertySpec::attribute().with_default(Value::Int(2))),
    );
    store.insert_layer(root);
    store.insert_layer(referenced);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert!(stage.is_instance(use_));
    let property = PropertyPath::new(proxy, x);
    let local = stage
        .inspect_property_authoring(
            &mut store,
            property,
            &EditTarget::for_layer(LayerId(1)),
            AuthoringQuery::default_value(),
        )
        .unwrap();
    assert!(local.target.is_none());
    let node = stage
        .explain_prim_graph(proxy)
        .unwrap()
        .nodes()
        .find(|(_, node)| node.arc_kind() == ArcKind::References)
        .unwrap()
        .0;
    let target = EditTarget::for_node(&stage, proxy, node).unwrap();
    let shared = stage
        .inspect_property_authoring(
            &mut store,
            property,
            &target,
            AuthoringQuery::default_value(),
        )
        .unwrap();
    assert!(shared.is_strongest());
    assert_eq!(shared.target.unwrap().lookup_path, source);
}

#[test]
fn muted_layers_have_no_prospective_site() {
    let (_, mut store, property) = fixture(PropertySpec::attribute());
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            muted_layers: alloc::collections::BTreeSet::from([LayerId(2)]),
            ..StageOptions::default()
        },
    );
    let result = stage
        .inspect_property_authoring(
            &mut store,
            property,
            &EditTarget::for_layer(LayerId(2)),
            AuthoringQuery::default_value(),
        )
        .unwrap();
    assert!(result.target.is_none());
}

#[test]
fn foreign_store_is_rejected_before_mapping_or_interning() {
    let (stage, store, property) = fixture(PropertySpec::attribute());
    let mut foreign = store.snapshot();
    let target = EditTarget::for_layer(LayerId(2));
    assert_eq!(
        stage.inspect_property_authoring(
            &mut foreign,
            property,
            &target,
            AuthoringQuery::default_value()
        ),
        Err(AuthoringInspectionError::ForeignStore)
    );
}

#[test]
fn specialized_target_uses_propagated_source_strength_not_its_inert_placeholder() {
    let mut store = InMemoryStore::default();
    let dest = store.path("/Use");
    let asset = store.path("/Asset");
    let class = store.path("/Class");
    let other = store.path("/Other");
    let x = store.tokens.intern("x");
    let mut root = Layer::new(LayerId(1));
    let mut prim = PrimSpec::def();
    prim.references = (crate::ListOp::prepended(vec![
        Reference::new(LayerId(2), asset),
        Reference::new(LayerId(3), other),
    ]))
    .into();
    root.insert_prim(dest, prim);
    let mut first = Layer::new(LayerId(2));
    first.sublayers.push(SublayerEntry::new(LayerId(4)));
    first.insert_prim(asset, PrimSpec::def().with_specialize(class));
    first.insert_prim(
        class,
        PrimSpec::class().with_property(x, PropertySpec::attribute().with_default(Value::Int(1))),
    );
    let mut second = Layer::new(LayerId(3));
    second.insert_prim(
        other,
        PrimSpec::def().with_property(x, PropertySpec::attribute().with_default(Value::Int(10))),
    );
    for layer in [root, first, second, Layer::new(LayerId(4))] {
        store.insert_layer(layer);
    }
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let actual = stage
        .explain_prim(dest)
        .unwrap()
        .iter()
        .find(|source| source.key.lookup_path == class && source.key.layer_id == LayerId(2))
        .unwrap()
        .key
        .node;
    for layer in [LayerId(2), LayerId(4)] {
        let target = EditTarget::for_node_layer(&stage, &store, dest, actual, layer).unwrap();
        let result = stage
            .inspect_property_authoring(
                &mut store,
                PropertyPath::new(dest, x),
                &target,
                AuthoringQuery::default_value(),
            )
            .unwrap();
        assert_eq!(result.target.unwrap().node, actual);
        assert!(
            !result.stronger_sources.is_empty(),
            "the other reference outranks propagated specializes"
        );
        assert_eq!(result.stronger_sources[0].key.layer_id, LayerId(3));
    }
}

#[test]
fn clip_inspection_uses_manifest_metadata_without_decoding_payloads() {
    let mut store = InMemoryStore::default();
    let path = store.path("/P");
    let clip_path = store.path("/C");
    let x = store.tokens.intern("x");
    let clips = store.tokens.intern("clips");
    let ty = || PropertyType::parse("float[]").unwrap();
    let mut root = Layer::new(LayerId(1));
    root.sublayers.push(SublayerEntry::new(LayerId(2)));
    root.insert_prim(
        path,
        PrimSpec::def()
            .with_property(x, PropertySpec::typed_attribute(ty()))
            .with_field(
                clips,
                FieldValue::Value(Value::Dictionary(vec![(
                    "default".into(),
                    Value::Dictionary(vec![
                        (
                            "assetPaths".into(),
                            Value::Array(vec![Value::Asset("clip.usda".into())]).into(),
                        ),
                        (
                            "manifestAssetPath".into(),
                            Value::Asset("manifest.usda".into()).into(),
                        ),
                        ("primPath".into(), Value::string("/C").into()),
                        (
                            "active".into(),
                            Value::Array(vec![Value::Vec2d([0., 0.])]).into(),
                        ),
                    ])
                    .into(),
                )])),
            ),
    );
    let mut manifest = Layer::new(LayerId(4));
    manifest.insert_prim(
        clip_path,
        PrimSpec::def().with_property(x, PropertySpec::typed_attribute(ty())),
    );
    let mut clip = Layer::new(LayerId(3));
    clip.insert_prim(
        clip_path,
        PrimSpec::def().with_property(
            x,
            PropertySpec::typed_attribute(ty()).with_time_samples(vec![(
                0.,
                Value::TypedArray(TypedArray::Deferred(Arc::new(NeverDecode))),
            )]),
        ),
    );
    for layer in [root, Layer::new(LayerId(2)), clip, manifest] {
        store.insert_layer(layer);
    }
    store.insert_asset_layer(LayerId(1), "clip.usda", LayerId(3));
    store.insert_asset_layer(LayerId(1), "manifest.usda", LayerId(4));
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let property = PropertyPath::new(path, x);
    let target = EditTarget::for_layer(LayerId(2));
    let default = stage
        .inspect_property_authoring(
            &mut store,
            property,
            &target,
            AuthoringQuery::default_value(),
        )
        .unwrap();
    assert!(default.is_strongest());
    let numeric = stage
        .inspect_property_authoring(
            &mut store,
            property,
            &target,
            AuthoringQuery::time_sample(0.),
        )
        .unwrap();
    assert_eq!(numeric.stronger_sources.len(), 1);
    assert!(
        matches!(&numeric.stronger_sources[0].kind, AuthoringSourceKind::ValueClips { set_name, pending } if set_name.as_ref() == "default" && pending.is_empty())
    );
    // A manifest-declared scalar-array clip with dense resident samples
    // masks; its kinds come from metadata, its values stay undecoded.
    assert_eq!(
        numeric.stronger_sources[0].composability,
        SourceComposability::Dense
    );
    assert_eq!(numeric.spans[0].verdict, AuthoringVerdict::Masked { by: 0 });
}

/// `/P.x` in a root layer, `LayerId(1)`, and its sublayers, strongest
/// first, each layer authoring `x` when its spec is given.
fn stack(specs: Vec<Option<PropertySpec>>) -> (Stage, InMemoryStore, PropertyPath) {
    let mut store = InMemoryStore::default();
    let path = store.path("/P");
    let name = store.tokens.intern("x");
    let count = specs.len();
    for (at, spec) in specs.into_iter().enumerate() {
        let id = LayerId(at as u64 + 1);
        let mut layer = Layer::new(id);
        let mut prim = if at == 0 {
            PrimSpec::def()
        } else {
            PrimSpec::over()
        };
        if let Some(spec) = spec {
            prim = prim.with_property(name, spec);
        }
        layer.insert_prim(path, prim);
        if at == 0 {
            for sublayer in 2..=count {
                layer
                    .sublayers
                    .push(SublayerEntry::new(LayerId(sublayer as u64)));
            }
        }
        store.insert_layer(layer);
    }
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    (stage, store, PropertyPath::new(path, name))
}

fn samples(times: &[f64]) -> PropertySpec {
    PropertySpec::attribute().with_time_samples(
        times
            .iter()
            .map(|&time| (time, Value::Double(time)))
            .collect(),
    )
}

fn spline() -> crate::SplineData {
    crate::SplineData {
        data_type: SplineDataType::Double,
        default_curve_type: CurveType::Bezier,
        pre_extrapolation: Extrapolation::Held,
        post_extrapolation: Extrapolation::Held,
        loop_params: None,
        pre_loop_boundary: None,
        post_loop_boundary: None,
        knots: vec![],
    }
}

fn inspect(
    stage: &Stage,
    store: &mut InMemoryStore,
    property: PropertyPath,
    layer: u64,
    query: AuthoringQuery,
) -> AuthoringInspection {
    stage
        .inspect_property_authoring(
            store,
            property,
            &EditTarget::for_layer(LayerId(layer)),
            query,
        )
        .unwrap()
}

#[test]
fn a_default_shows_at_the_default_time_but_not_under_stronger_samples() {
    let (stage, mut store, property) = stack(vec![Some(samples(&[0., 10.])), None]);
    let default = inspect(
        &stage,
        &mut store,
        property,
        2,
        AuthoringQuery::default_value(),
    );
    assert!(default.is_strongest());
    assert_eq!(default.spans[0].times, SpanTimes::Default);
    let at = inspect(
        &stage,
        &mut store,
        property,
        2,
        AuthoringQuery::default_value().at(5.),
    );
    assert_eq!(at.spans[0].verdict, AuthoringVerdict::Masked { by: 0 });
    let source = &at.stronger_sources[0];
    assert_eq!(source.kind, AuthoringSourceKind::TimeSamples);
    assert_eq!(source.key.layer_id, LayerId(1));
    assert_eq!(source.arc, ArcKind::Local);
    assert!(!source.same_site);
    // The value interpolates across the stronger samples meanwhile.
    let CurrentSource::Authored {
        kind: AuthoringSourceKind::TimeSamples,
        standing: SourceStanding::Stronger { index: 0 },
        bracket: Some(bracket),
        ..
    } = at.spans[0].current
    else {
        panic!("{at:?}");
    };
    assert_eq!((bracket.lower, bracket.upper), (0., 10.));
    assert!(bracket.is_interpolated());
    let held = inspect(
        &stage,
        &mut store,
        property,
        2,
        AuthoringQuery::default_value().at(5.).held(),
    );
    let CurrentSource::Authored {
        bracket: Some(bracket),
        ..
    } = held.spans[0].current
    else {
        panic!("{held:?}");
    };
    assert_eq!((bracket.lower, bracket.upper), (0., 0.));
    // Over every time code, one span, and no bracket for a range.
    let range = inspect(
        &stage,
        &mut store,
        property,
        2,
        AuthoringQuery::default_value().over(f64::NEG_INFINITY, f64::INFINITY),
    );
    assert_eq!(range.spans.len(), 1);
    assert_eq!(
        range.spans[0].times,
        SpanTimes::Codes {
            start: Bound::Unbounded,
            end: Bound::Unbounded
        }
    );
    assert!(matches!(
        range.spans[0].current,
        CurrentSource::Authored { bracket: None, .. }
    ));
}

#[test]
fn the_targets_own_samples_mask_a_default_or_spline_written_beside_them() {
    let (stage, mut store, property) = stack(vec![Some(samples(&[0.])), None]);
    let default = inspect(
        &stage,
        &mut store,
        property,
        1,
        AuthoringQuery::default_value().at(3.),
    );
    assert_eq!(default.spans[0].verdict, AuthoringVerdict::Masked { by: 0 });
    let source = &default.stronger_sources[0];
    assert!(source.same_site);
    assert_eq!(source.kind, AuthoringSourceKind::TimeSamples);
    assert_eq!(
        default.spans[0].current,
        CurrentSource::Authored {
            key: source.key.clone(),
            kind: AuthoringSourceKind::TimeSamples,
            standing: SourceStanding::Target,
            bracket: Some(SampleBracket {
                lower: 0.,
                upper: 0.
            }),
        }
    );
    let spline_write = inspect(&stage, &mut store, property, 1, AuthoringQuery::spline());
    assert!(!spline_write.is_strongest());
    assert!(spline_write.stronger_sources[0].same_site);
    // A sample is read before the site's other slots.
    assert!(
        inspect(
            &stage,
            &mut store,
            property,
            1,
            AuthoringQuery::time_sample(3.)
        )
        .is_strongest()
    );
    // At the default time only the default is read.
    assert!(
        inspect(
            &stage,
            &mut store,
            property,
            1,
            AuthoringQuery::default_value()
        )
        .is_strongest()
    );

    let (stage, mut store, property) =
        stack(vec![Some(PropertySpec::attribute().with_spline(spline()))]);
    let default = inspect(
        &stage,
        &mut store,
        property,
        1,
        AuthoringQuery::default_value().at(3.),
    );
    assert_eq!(
        default.stronger_sources[0].kind,
        AuthoringSourceKind::Spline
    );
    assert!(
        inspect(
            &stage,
            &mut store,
            property,
            1,
            AuthoringQuery::time_sample(3.)
        )
        .is_strongest()
    );
}

#[test]
fn an_animation_block_masks_written_animation_and_skips_weaker_samples_for_a_default() {
    let block = PropertySpec::attribute().with_default(Value::AnimationBlock);
    let (stage, mut store, property) = stack(vec![Some(block), Some(samples(&[0.])), None]);
    // Weaker than the block, the middle layer's samples are not read.
    let default = inspect(
        &stage,
        &mut store,
        property,
        3,
        AuthoringQuery::default_value().at(1.),
    );
    assert!(default.is_strongest(), "{default:?}");
    assert_eq!(
        default.spans[0].current,
        CurrentSource::Fallback { schema: false }
    );
    let sample = inspect(
        &stage,
        &mut store,
        property,
        3,
        AuthoringQuery::time_sample(1.),
    );
    assert_eq!(sample.spans[0].verdict, AuthoringVerdict::Masked { by: 0 });
    assert_eq!(
        sample.stronger_sources[0].kind,
        AuthoringSourceKind::AnimationBlock
    );
    // The block's own site reads samples written beside it.
    assert!(
        inspect(
            &stage,
            &mut store,
            property,
            1,
            AuthoringQuery::time_sample(1.)
        )
        .is_strongest()
    );
}

#[test]
fn a_value_block_masks_at_every_time_and_reports_the_block() {
    let block = PropertySpec::attribute().with_default(Value::Blocked);
    let (stage, mut store, property) = stack(vec![Some(block), Some(samples(&[0.])), None]);
    for query in [
        AuthoringQuery::default_value(),
        AuthoringQuery::time_sample(2.),
        AuthoringQuery::default_value().at(2.),
    ] {
        let result = inspect(&stage, &mut store, property, 3, query);
        assert_eq!(result.stronger_sources[0].kind, AuthoringSourceKind::Block);
        assert_eq!(result.spans[0].verdict, AuthoringVerdict::Masked { by: 0 });
        assert!(matches!(
            result.spans[0].current,
            CurrentSource::Authored {
                kind: AuthoringSourceKind::Block,
                ..
            }
        ));
    }
}

#[test]
fn every_stronger_source_is_listed_and_a_weaker_value_is_current() {
    let dense = PropertySpec::attribute().with_default(Value::Double(1.));
    let (stage, mut store, property) = stack(vec![
        Some(dense.clone()),
        Some(samples(&[0.])),
        None,
        Some(dense),
    ]);
    let result = inspect(
        &stage,
        &mut store,
        property,
        3,
        AuthoringQuery::default_value().at(0.),
    );
    let layers: Vec<_> = result
        .stronger_sources
        .iter()
        .map(|source| source.key.layer_id)
        .collect();
    assert_eq!(layers, [LayerId(1), LayerId(2)]);
    assert_eq!(result.spans[0].verdict, AuthoringVerdict::Masked { by: 0 });
    let (stage, mut store, property) = stack(vec![None, Some(samples(&[0.]))]);
    let result = inspect(
        &stage,
        &mut store,
        property,
        1,
        AuthoringQuery::time_sample(0.),
    );
    assert!(result.is_strongest());
    assert!(matches!(
        &result.spans[0].current,
        CurrentSource::Authored { key, standing: SourceStanding::Weaker, .. } if key.layer_id == LayerId(2)
    ));
}

#[test]
fn a_range_splits_where_stronger_samples_start_to_compose() {
    let array = Value::Array(vec![Value::Double(1.)]);
    let edit = Value::ArrayEdit(crate::ArrayEdit::default());
    let mixed = PropertySpec::attribute().with_time_samples(vec![(0., array), (10., edit)]);
    let (stage, mut store, property) = stack(vec![Some(mixed), None]);
    let held = inspect(
        &stage,
        &mut store,
        property,
        2,
        AuthoringQuery::default_value()
            .over(f64::NEG_INFINITY, f64::INFINITY)
            .held(),
    );
    let spans: Vec<_> = held
        .spans
        .iter()
        .map(|span| (span.times, span.verdict))
        .collect();
    assert_eq!(
        spans,
        [
            (
                SpanTimes::Codes {
                    start: Bound::Unbounded,
                    end: Bound::Excluded(10.)
                },
                AuthoringVerdict::Masked { by: 0 }
            ),
            (
                SpanTimes::Codes {
                    start: Bound::Included(10.),
                    end: Bound::Unbounded
                },
                AuthoringVerdict::ComposedOver
            ),
        ]
    );
    assert_eq!(
        held.stronger_sources[0].composability,
        SourceComposability::Unknown
    );
    assert_eq!(
        held.span_at(3.).unwrap().verdict,
        AuthoringVerdict::Masked { by: 0 }
    );
    // Interpolating, the sparse upper sample composes from just after 0.
    let linear = inspect(
        &stage,
        &mut store,
        property,
        2,
        AuthoringQuery::default_value().over(-5., 20.),
    );
    assert_eq!(
        linear.spans[0].times,
        SpanTimes::Codes {
            start: Bound::Included(-5.),
            end: Bound::Included(0.)
        }
    );
    assert_eq!(
        linear.spans[1].times,
        SpanTimes::Codes {
            start: Bound::Excluded(0.),
            end: Bound::Included(20.)
        }
    );
    assert_eq!(linear.spans[1].verdict, AuthoringVerdict::ComposedOver);
}

#[test]
fn invalid_times_and_animation_at_the_default_time_are_refused() {
    let (stage, mut store, property) = stack(vec![None]);
    let target = EditTarget::for_layer(LayerId(1));
    for (query, error) in [
        (
            AuthoringQuery::time_sample(0.).over(1., 0.),
            AuthoringInspectionError::InvalidTime,
        ),
        (
            AuthoringQuery::default_value().over(f64::NAN, 0.),
            AuthoringInspectionError::InvalidTime,
        ),
        (
            AuthoringQuery::default_value().over(f64::INFINITY, f64::INFINITY),
            AuthoringInspectionError::InvalidTime,
        ),
        (
            AuthoringQuery {
                times: AuthoringTimes::Default,
                ..AuthoringQuery::spline()
            },
            AuthoringInspectionError::AnimationAtDefault,
        ),
    ] {
        assert_eq!(
            stage.inspect_property_authoring(&mut store, property, &target, query),
            Err(error)
        );
    }
    let inactive = stage
        .inspect_property_authoring(
            &mut store,
            property,
            &EditTarget::for_layer(LayerId(1)),
            AuthoringQuery::time_sample(0.),
        )
        .unwrap();
    assert_eq!(
        inactive.spans[0].current,
        CurrentSource::Fallback { schema: false }
    );
}

#[derive(Debug)]
struct NeverDecodeValue;
impl DeferredValueSource for NeverDecodeValue {
    fn materialize(&self) -> Result<&Value, &crate::ArrayReadError> {
        panic!("authoring inspection must not decode a deferred default")
    }
    fn element_kind(&self) -> Option<Value> {
        None
    }
}

#[test]
fn deferred_defaults_are_classified_without_decoding() {
    let deferred = Value::Deferred(DeferredValue::new(Arc::new(NeverDecodeValue)));
    let spec = PropertySpec::typed_attribute(PropertyType::parse("token[]").unwrap())
        .with_default(deferred);
    let (stage, mut store, property) = stack(vec![Some(spec), None]);
    for query in [
        AuthoringQuery::default_value(),
        AuthoringQuery::default_value().over(0., 10.),
    ] {
        let result = inspect(&stage, &mut store, property, 2, query);
        assert_eq!(
            result.stronger_sources[0].composability,
            SourceComposability::Dense
        );
    }
}

/// Which of [`clip_stack`]'s assets the store holds.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Residency {
    /// The manifest and the clip.
    All,
    /// The clip; the set authors no manifest, so it is generated.
    NoManifest,
    /// Neither: the manifest is bound but not loaded, the clip not bound.
    ManifestPending,
    /// The manifest; the clip is not bound.
    ClipPending,
}

/// `/P` with clip set `default` anchored in the root layer, a manifest
/// declaring `x` varying unless `residency` says there is none, one clip,
/// and a weaker sublayer.
fn clip_stack(residency: Residency) -> (Stage, InMemoryStore, PropertyPath) {
    let mut store = InMemoryStore::default();
    let path = store.path("/P");
    let clip_path = store.path("/C");
    let x = store.tokens.intern("x");
    let clips = store.tokens.intern("clips");
    let mut set: Vec<(Arc<str>, Value)> = vec![
        (
            "assetPaths".into(),
            Value::Array(vec![Value::Asset("clip.usda".into())]),
        ),
        ("primPath".into(), Value::string("/C")),
        ("active".into(), Value::Array(vec![Value::Vec2d([0., 0.])])),
    ];
    if residency != Residency::NoManifest {
        set.push((
            "manifestAssetPath".into(),
            Value::Asset("manifest.usda".into()),
        ));
    }
    let mut root = Layer::new(LayerId(1));
    root.sublayers.push(SublayerEntry::new(LayerId(2)));
    root.insert_prim(
        path,
        PrimSpec::def().with_field(
            clips,
            FieldValue::Value(Value::Dictionary(vec![(
                "default".into(),
                Value::dictionary(set).into(),
            )])),
        ),
    );
    let mut manifest_layer = Layer::new(LayerId(4));
    manifest_layer.insert_prim(
        clip_path,
        PrimSpec::def().with_property(x, PropertySpec::attribute()),
    );
    let mut clip = Layer::new(LayerId(3));
    clip.insert_prim(
        clip_path,
        PrimSpec::def().with_property(
            x,
            PropertySpec::attribute().with_time_samples(vec![(
                0.,
                Value::TypedArray(TypedArray::Deferred(Arc::new(NeverDecode))),
            )]),
        ),
    );
    let mut weak = Layer::new(LayerId(2));
    weak.insert_prim(
        path,
        PrimSpec::over()
            .with_property(x, PropertySpec::attribute().with_default(Value::Double(1.))),
    );
    for layer in [root, weak] {
        store.insert_layer(layer);
    }
    if matches!(residency, Residency::All | Residency::NoManifest) {
        store.insert_layer(clip);
        store.insert_asset_layer(LayerId(1), "clip.usda", LayerId(3));
    }
    match residency {
        Residency::All | Residency::ClipPending => {
            store.insert_layer(manifest_layer);
            store.insert_asset_layer(LayerId(1), "manifest.usda", LayerId(4));
        }
        Residency::ManifestPending => {
            store.insert_asset_layer(LayerId(1), "manifest.usda", LayerId(4));
        }
        Residency::NoManifest => {}
    }
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    (stage, store, PropertyPath::new(path, x))
}

#[test]
fn clip_sets_mask_every_time_code_but_not_the_default_or_their_own_site() {
    for residency in [Residency::All, Residency::NoManifest] {
        let (stage, mut store, property) = clip_stack(residency);
        let numeric = inspect(
            &stage,
            &mut store,
            property,
            2,
            AuthoringQuery::default_value().over(-100., 100.),
        );
        assert_eq!(numeric.spans.len(), 1, "{residency:?}");
        assert_eq!(numeric.spans[0].verdict, AuthoringVerdict::Masked { by: 0 });
        assert!(matches!(
            &numeric.stronger_sources[0].kind,
            AuthoringSourceKind::ValueClips { pending, .. } if pending.is_empty()
        ));
        assert!(matches!(
            numeric.spans[0].current,
            CurrentSource::Authored {
                standing: SourceStanding::Stronger { index: 0 },
                ..
            }
        ));
        assert!(
            inspect(
                &stage,
                &mut store,
                property,
                2,
                AuthoringQuery::default_value()
            )
            .is_strongest()
        );
        // An opinion at the anchor's site is read before its clips.
        let anchored = inspect(
            &stage,
            &mut store,
            property,
            1,
            AuthoringQuery::time_sample(5.),
        );
        assert!(anchored.is_strongest(), "{anchored:?}");
        assert!(matches!(
            anchored.spans[0].current,
            CurrentSource::Authored {
                standing: SourceStanding::Target,
                kind: AuthoringSourceKind::ValueClips { .. },
                ..
            }
        ));
    }
}

#[test]
fn clip_sets_awaiting_metadata_name_the_assets_to_load() {
    let (stage, mut store, property) = clip_stack(Residency::ManifestPending);
    let result = inspect(
        &stage,
        &mut store,
        property,
        2,
        AuthoringQuery::time_sample(0.),
    );
    assert_eq!(result.spans[0].verdict, AuthoringVerdict::Unknown { by: 0 });
    let AuthoringSourceKind::ValueClips { pending, .. } = &result.stronger_sources[0].kind else {
        panic!("{result:?}");
    };
    // Only the manifest settles participation; the clip is not requested.
    let names: Vec<_> = pending
        .iter()
        .map(|request| request.identifier.as_ref())
        .collect();
    assert_eq!(names, ["manifest.usda"]);
    assert_eq!(
        result.stronger_sources[0].composability,
        SourceComposability::Unknown
    );
}

#[test]
fn a_declared_clip_set_with_clips_not_resident_has_unknown_sample_kinds() {
    let (stage, mut store, property) = clip_stack(Residency::ClipPending);
    let result = inspect(
        &stage,
        &mut store,
        property,
        2,
        AuthoringQuery::time_sample(0.),
    );
    // The manifest settles participation; an untyped declaration's clip
    // samples may be sparse array edits, and the clip is unread.
    assert!(matches!(
        &result.stronger_sources[0].kind,
        AuthoringSourceKind::ValueClips { pending, .. } if pending.is_empty()
    ));
    assert_eq!(result.spans[0].verdict, AuthoringVerdict::Unknown { by: 0 });
}
