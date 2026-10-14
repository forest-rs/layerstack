// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{InMemoryStore, Layer, PrimSpec, PropertySpec, Reference, StageOptions, SublayerEntry};
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
        .inspect_property_authoring(&mut store, path, &target, Time::Default)
        .unwrap();
    assert!(default.is_strongest());
    assert_eq!(default.target.unwrap().layer_strength, 1);
    let numeric = stage
        .inspect_property_authoring(&mut store, path, &target, Time::at(10.))
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
            Value::Dictionary(vec![("a".into(), Value::Int(1))]),
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
                Time::Default,
            )
            .unwrap();
        assert_eq!(result.stronger_sources[0].composability, expected);
    }
    let (stage, mut store, path) =
        fixture(PropertySpec::attribute().with_default(Value::AnimationBlock));
    let target = EditTarget::for_layer(LayerId(2));
    assert!(
        stage
            .inspect_property_authoring(&mut store, path, &target, Time::Default)
            .unwrap()
            .is_strongest()
    );
    let numeric = stage
        .inspect_property_authoring(&mut store, path, &target, Time::at(0.))
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
        .find(|(_, node)| node.arc_kind() == crate::ArcKind::References)
        .unwrap()
        .0;
    let target = EditTarget::for_node_layer(&stage, &store, dest, node, LayerId(3)).unwrap();
    let result = stage
        .inspect_property_authoring(
            &mut store,
            PropertyPath::new(dest, x),
            &target,
            Time::Default,
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
            Time::Default,
        )
        .unwrap();
    assert!(result.target.is_none());
    assert!(!result.is_strongest());
    assert_eq!(
        stage.inspect_property_authoring(
            &mut store,
            path,
            &EditTarget::for_layer(LayerId(1)),
            Time::at(f64::NAN)
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
    prim.variant_set_order.push(mode);
    prim.variant_selections.insert(mode, selected);
    prim.variant_sets.insert(
        mode,
        crate::VariantSetSpec {
            variants: crate::HashMap::from([
                (selected, crate::VariantSpec::default()),
                (hidden, crate::VariantSpec::default()),
            ]),
        },
    );
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(path, prim);
    store.insert_layer(root);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let selected_spec =
        crate::SpecPath::parse("/P{mode=selected}", &mut store.tokens, &mut store.paths).unwrap();
    let hidden_spec =
        crate::SpecPath::parse("/P{mode=hidden}", &mut store.tokens, &mut store.paths).unwrap();
    for (spec, active) in [(selected_spec, true), (hidden_spec, false)] {
        let target = EditTarget::for_local_variant(LayerId(1), &spec);
        let result = stage
            .inspect_property_authoring(
                &mut store,
                PropertyPath::new(path, x),
                &target,
                Time::Default,
            )
            .unwrap();
        assert_eq!(result.target.is_some(), active);
        assert_eq!(result.is_strongest(), active);
    }
}

#[derive(Debug)]
struct NeverDecode;
impl crate::DeferredArraySource for NeverDecode {
    fn materialize(&self) -> Result<&crate::TypedArray, &crate::ArrayReadError> {
        panic!("authoring inspection must not decode numeric storage")
    }
    fn element_kind(&self) -> Value {
        Value::Float(0.)
    }
}

#[test]
fn sample_inspection_is_metadata_only_even_for_retained_arrays() {
    let retained = Value::TypedArray(crate::TypedArray::Deferred(Arc::new(NeverDecode)));
    let sparse = Value::ArrayEdit(crate::ArrayEdit::default());
    let (stage, mut store, path) =
        fixture(PropertySpec::attribute().with_time_samples(vec![(0., retained), (10., sparse)]));
    let target = EditTarget::for_layer(LayerId(2));
    let held = stage
        .inspect_property_authoring(&mut store, path, &target, Time::held(5.))
        .unwrap();
    assert_eq!(
        held.stronger_sources[0].composability,
        SourceComposability::Dense
    );
    let linear = stage
        .inspect_property_authoring(&mut store, path, &target, Time::at(5.))
        .unwrap();
    assert_eq!(
        linear.stronger_sources[0].composability,
        SourceComposability::Unknown
    );
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
            Time::Default,
        )
        .unwrap();
    assert!(local.target.is_none());
    let node = stage
        .explain_prim_graph(proxy)
        .unwrap()
        .nodes()
        .find(|(_, node)| node.arc_kind() == crate::ArcKind::References)
        .unwrap()
        .0;
    let target = EditTarget::for_node(&stage, proxy, node).unwrap();
    let shared = stage
        .inspect_property_authoring(&mut store, property, &target, Time::Default)
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
            Time::Default,
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
        stage.inspect_property_authoring(&mut foreign, property, &target, Time::Default),
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
    prim.references = crate::ListOp::prepended(vec![
        Reference::new(LayerId(2), asset),
        Reference::new(LayerId(3), other),
    ]);
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
        .find(|key| key.lookup_path == class && key.layer_id == LayerId(2))
        .unwrap()
        .node;
    for layer in [LayerId(2), LayerId(4)] {
        let target = EditTarget::for_node_layer(&stage, &store, dest, actual, layer).unwrap();
        let result = stage
            .inspect_property_authoring(
                &mut store,
                PropertyPath::new(dest, x),
                &target,
                Time::Default,
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
    let ty = || crate::PropertyType::new("float", true, Value::Float(0.));
    let mut root = Layer::new(LayerId(1));
    root.sublayers.push(SublayerEntry::new(LayerId(2)));
    root.insert_prim(
        path,
        PrimSpec::def()
            .with_property(x, PropertySpec::typed_attribute(ty()))
            .with_field(
                clips,
                crate::FieldValue::Value(Value::Dictionary(vec![(
                    "default".into(),
                    Value::Dictionary(vec![
                        (
                            "assetPaths".into(),
                            Value::Array(vec![Value::Asset("clip.usda".into())]),
                        ),
                        (
                            "manifestAssetPath".into(),
                            Value::Asset("manifest.usda".into()),
                        ),
                        ("primPath".into(), Value::string("/C")),
                        ("active".into(), Value::Array(vec![Value::Vec2d([0., 0.])])),
                    ]),
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
                Value::TypedArray(crate::TypedArray::Deferred(Arc::new(NeverDecode))),
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
        .inspect_property_authoring(&mut store, property, &target, Time::Default)
        .unwrap();
    assert!(default.is_strongest());
    let numeric = stage
        .inspect_property_authoring(&mut store, property, &target, Time::at(0.))
        .unwrap();
    assert_eq!(numeric.stronger_sources.len(), 1);
    assert!(
        matches!(&numeric.stronger_sources[0].kind, AuthoringSourceKind::ValueClips { set_name } if set_name.as_ref() == "default")
    );
    assert_eq!(
        numeric.stronger_sources[0].composability,
        SourceComposability::Unknown
    );
}
