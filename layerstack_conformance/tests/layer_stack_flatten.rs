// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Native authored layer-stack consolidation, preserving arcs and variants.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    AssetResolveError, AssetResolver, FieldValue, LayerId, PathInterner, PrimSpec, PropertyEntry,
    Reference, ResolvedAsset, Stage, StageOptions, Time, TokenInterner, Value, VariantSetSpec,
    flatten_layer_stack,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use std::{path::Path, sync::Arc};

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
fn value(value: &mut Value) {
    match value {
        Value::Dictionary(entries) => {
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (_, entry) in entries {
                self::value(entry);
            }
        }
        Value::Array(values) => {
            for entry in values {
                self::value(entry);
            }
        }
        _ => {}
    }
}
fn fields(fields: &mut [layerstack::FieldEntry]) {
    fields.sort_by_key(|entry| entry.name);
    for field in fields {
        if let FieldValue::Value(entry) = &mut field.value {
            value(entry);
        }
    }
}
fn arcs(list: &mut layerstack::ListOp<Reference>, output: LayerId) {
    for reference in list
        .explicit
        .iter_mut()
        .flatten()
        .chain(&mut list.prepend)
        .chain(&mut list.append)
        .chain(&mut list.delete)
    {
        reference.layer = if reference.asset.is_some() {
            LayerId::UNRESOLVED
        } else {
            output
        };
    }
}
fn properties(properties: &mut [PropertyEntry]) {
    properties.sort_by_key(|entry| entry.name);
    for entry in properties {
        let property = Arc::make_mut(&mut entry.spec);
        fields(property.metadata.make_mut());
    }
}
fn variants(sets: &mut layerstack::HashMap<layerstack::TokenId, VariantSetSpec>, output: LayerId) {
    for set in sets.values_mut() {
        for variant in set.variants.values_mut() {
            fields(&mut variant.fields);
            properties(&mut variant.properties);

            arcs(&mut variant.references, output);
            arcs(&mut variant.payloads, output);
            variants(&mut variant.variant_sets, output);
        }
    }
}
fn normalize(spec: &mut PrimSpec, output: LayerId) {
    fields(&mut spec.fields);
    properties(&mut spec.properties);

    arcs(&mut spec.references, output);
    arcs(&mut spec.payloads, output);
    variants(&mut spec.variant_sets, output);
}

#[test]
fn raw_consolidation_matches_native_and_keeps_composed_stage_values() {
    let directory = workspace_root().join("layerstack_conformance/fixtures/layer_stack_flatten");
    let loaded = load_entry_usda(&directory.join("root.usda"));
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let mut store = loaded.store;
    let output = LayerId(100);
    let mut consolidated = flatten_layer_stack(
        &store,
        loaded.root_layer.into(),
        output,
        &mut |source, asset| {
            let source = loaded.layer_names.get(&source)?;
            let path = Path::new(source)
                .parent()
                .unwrap_or(Path::new(""))
                .join(asset);
            let spelling = path
                .components()
                .filter(|part| !matches!(part, std::path::Component::CurDir))
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            Some(format!("__FIXTURE__/{spelling}"))
        },
    )
    .unwrap();
    assert_eq!(consolidated.report.sources.len(), 2);
    let parsed = layerstack_usda::parser::parse(include_str!(
        "../fixtures/layer_stack_flatten/native_flattened.usda"
    ));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let mut expected = layerstack_usda::emit::emit(
        &parsed.layer,
        output,
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .layer;
    fields(&mut consolidated.layer.metadata);
    fields(&mut expected.metadata);
    assert_eq!(consolidated.layer.metadata, expected.metadata);
    let mut paths: Vec<_> = expected
        .prims
        .keys()
        .chain(expected.variant_prims.keys())
        .copied()
        .collect();
    paths.sort();
    paths.dedup();
    for path in paths {
        let actual: Vec<_> = consolidated.layer.prim_specs(path).cloned().collect();
        let expected: Vec<_> = expected.prim_specs(path).cloned().collect();
        assert_eq!(actual.len(), expected.len());
        for mut actual in actual {
            let mut expected = expected
                .iter()
                .find(|expected| expected.outer_variant_sites == actual.outer_variant_sites)
                .unwrap()
                .clone();
            normalize(&mut actual, output);
            normalize(&mut expected, output);
            assert_eq!(
                actual,
                expected,
                "{}",
                store.paths.display(path, &store.tokens)
            );
        }
    }
    let original = Stage::compose(&mut store, loaded.root_layer, StageOptions::default());
    for asset in &consolidated.report.asset_paths {
        if let Some(layer) = asset.resolved_layer {
            store.insert_asset_layer(output, &asset.output_path, layer);
        }
    }
    store.insert_layer(consolidated.layer);
    let flattened = Stage::compose(&mut store, output, StageOptions::default());
    let prim = store.path("/P");
    for name in [
        "masked",
        "animated",
        "weakAnimated",
        "timeline",
        "label",
        "inherited",
        "specialized",
        "source",
    ] {
        let name = store.tokens.lookup(name).unwrap();
        for time in [Time::Default, Time::at(10.), Time::at(14.)] {
            assert_eq!(
                original
                    .read_property(layerstack::PropertyPath::new(prim, name), time, |value| {
                        Some(value.clone())
                    })
                    .map(|value| value.value),
                flattened
                    .read_property(layerstack::PropertyPath::new(prim, name), time, |value| {
                        Some(value.clone())
                    })
                    .map(|value| value.value),
                "{} {time:?}",
                store.tokens.resolve(name)
            );
        }
    }
    assert_eq!(
        flattened.has_prim(store.path("/P/Inactive")),
        original.has_prim(store.path("/P/Inactive"))
    );
    let clip = store.path("/ClipPrim");
    let name = store.tokens.lookup("clipValue").unwrap();
    for time in [Time::at(10.), Time::at(12.), Time::at(14.)] {
        let path = layerstack::PropertyPath::new(clip, name);
        let read = |stage: &Stage| {
            stage
                .read_property(path, time, |value| Some(value.clone()))
                .map(|value| value.value)
        };
        assert_eq!(read(&original), read(&flattened));
        assert!(read(&flattened).is_some());
    }
}
