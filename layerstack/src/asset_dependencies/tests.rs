// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{ArrayIndex, InMemoryStore, PropertySpec, PropertyType, VariantSpec};
use alloc::vec;

#[test]
fn recursive_inventory_visits_unselected_variants_and_reports_partial_resolution() {
    let mut store = InMemoryStore::default();
    let path = store.path("/Model");
    let mode = store.tokens.intern("mode");
    let hidden = store.tokens.intern("hidden");
    let file = store.tokens.intern("file");
    let mut variant = VariantSpec {
        references: ListOp::prepended(vec![Reference::with_asset_default_prim(
            LayerId(2),
            "child.usda",
        )]),
        ..VariantSpec::default()
    };
    variant.properties.push(PropertyEntry {
        name: file,
        spec: Arc::new(
            PropertySpec::typed_attribute(PropertyType::new(
                "asset",
                false,
                Value::Asset("".into()),
            ))
            .with_default(Value::Asset("missing.png".into())),
        ),
    });
    let mut prim = PrimSpec::def();
    prim.variant_sets.insert(
        mode,
        VariantSetSpec {
            variants: crate::HashMap::from([(hidden, variant)]),
        },
    );
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(path, prim);
    let mut child = Layer::new(LayerId(2));
    child.sublayers.push(crate::SublayerEntry::with_asset(
        LayerId(1),
        "root.usda",
        crate::LayerOffset::IDENTITY,
    ));
    store.insert_layer(root);
    store.insert_layer(child);
    let report = collect_dependencies(&store, LayerId(1), |d| match &*d.identifier {
        "child.usda" => vec![DependencyTarget::Layer(LayerId(2))],
        "root.usda" => vec![DependencyTarget::Layer(LayerId(1))],
        _ => vec![DependencyTarget::Unresolved("missing image".into())],
    });
    assert_eq!(report.layers, [LayerId(1), LayerId(2)]);
    assert!(!report.is_complete());
    let use_ = report
        .dependencies
        .iter()
        .find(|d| &*d.source.identifier == "missing.png")
        .unwrap();
    assert_eq!(
        use_.source.spec.as_ref().unwrap().display(&store.tokens),
        "/Model{mode=hidden}.file"
    );
}

#[test]
fn rewriting_covers_sparse_samples_metadata_and_clip_templates_without_mutating_source() {
    let mut store = InMemoryStore::default();
    let path = store.path("/Model");
    let file = store.tokens.intern("file");
    let clips = store.tokens.intern("clips");
    let mut prim = PrimSpec::def();
    prim.set_property(
        file,
        PropertySpec::attribute()
            .with_default(Value::ArrayEdit(ArrayEdit {
                ops: vec![ArrayEditOp::Insert {
                    src: ArrayEditOperand::Literal(Value::Asset("old.png".into())),
                    index: ArrayIndex::End,
                }],
            }))
            .with_time_samples(vec![(1., Value::Asset("sample.png".into()))]),
    );
    prim.set_field(
        clips,
        Value::Dictionary(vec![(
            "main".into(),
            Value::Dictionary(vec![
                ("templateAssetPath".into(), Value::string("anim.#.usda")),
                (
                    "assetPaths".into(),
                    Value::Array(vec![Value::Asset("clip.usda".into())]),
                ),
                ("plain".into(), Value::string("leave.png")),
            ]),
        )]),
    );
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(path, prim);
    let before = layer.clone();
    let result = rewrite_layer_assets(&layer, &store.paths, &store.tokens, |d| {
        Ok::<_, ()>(alloc::format!("copy/{}", d.identifier).into())
    })
    .unwrap();
    assert_eq!(layer, before);
    assert_eq!(
        extract_layer_dependencies(&result, &store.paths, &store.tokens).len(),
        4
    );
    assert!(
        extract_layer_dependencies(&result, &store.paths, &store.tokens)
            .iter()
            .all(|d| d.identifier.starts_with("copy/"))
    );
    assert!(result.generation() > layer.generation());
    assert!(
        rewrite_layer_assets(&layer, &store.paths, &store.tokens, |_| Err::<Arc<str>, _>(
            "failed"
        ))
        .is_err()
    );
    assert_eq!(layer, before);
}

#[test]
fn unrelated_numeric_samples_keep_shared_storage_and_noop_rewrites_keep_generation() {
    let mut store = InMemoryStore::default();
    let path = store.path("/Model");
    let file = store.tokens.intern("file");
    let animated = store.tokens.intern("animated");
    let mut prim = PrimSpec::def();
    prim.set_property(
        file,
        PropertySpec::attribute().with_default(Value::Asset("image.png".into())),
    );
    prim.set_property(
        animated,
        PropertySpec::attribute()
            .with_time_samples(vec![(0., Value::Double(1.)), (1., Value::Double(2.))]),
    );
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(path, prim);
    let same = rewrite_layer_assets(&layer, &store.paths, &store.tokens, |d| {
        Ok::<_, ()>(d.identifier.clone())
    })
    .unwrap();
    assert_eq!(same.generation(), layer.generation());
    let copy = rewrite_layer_assets(&layer, &store.paths, &store.tokens, |_| {
        Ok::<_, ()>("new.png".into())
    })
    .unwrap();
    assert!(Arc::ptr_eq(
        &layer.prims[&path].properties[1].spec,
        &copy.prims[&path].properties[1].spec
    ));
}

#[test]
fn missing_loaded_layers_and_empty_host_results_cannot_claim_completeness() {
    let mut store = InMemoryStore::default();
    let mut root = Layer::new(LayerId(1));
    root.sublayers.push(crate::SublayerEntry::new(LayerId(8)));
    root.sublayers.push(crate::SublayerEntry::unresolved(
        "unknown.usda",
        crate::LayerOffset::IDENTITY,
    ));
    store.insert_layer(root);
    let report = collect_dependencies(&store, LayerId(1), |_| Vec::new());
    assert_eq!(report.missing_layers, [LayerId(8)]);
    assert!(!report.is_complete());
}
