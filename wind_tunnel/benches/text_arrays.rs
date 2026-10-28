// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `token[]` and `string[]` values: reading time-sampled token arrays from
//! a crate file, and a schema getter of a `string[]` attribute.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, Layer, LayerId, PathInterner, PrimSpec,
    PropertySpec, PropertyType, ResolvedAsset, Stage, StageOptions, TokenInterner, Value,
};
use layerstack_schemas::{Scene, usd_render::RenderPass};
use layerstack_usdc::writer::{Spec, SpecForm, Specifier, Value as Crate, write_crate};

struct NoAssets;
impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// `prims` prims, each with a `token[] states` of `samples` time samples of
/// `width` tokens from a 256-token vocabulary, and a `token[]` in its
/// `customData`.
fn sampled_tokens(prims: u32, samples: u32, width: u32) -> Vec<u8> {
    let token = |i: u32| format!("state_{:03}", i.wrapping_mul(2_654_435_761) >> 24);
    let mut specs = vec![Spec::new("/", SpecForm::PseudoRoot)];
    for p in 0..prims {
        let tokens = |seed: u32| (0..width).map(|w| token(seed * width + w)).collect();
        specs.push(
            Spec::new(format!("/Item{p}"), SpecForm::Prim)
                .with_field("specifier", Crate::Specifier(Specifier::Def))
                .with_field(
                    "customData",
                    Crate::Dictionary(vec![("labels".into(), Crate::TokenArray(tokens(p)))]),
                ),
        );
        specs.push(
            Spec::new(format!("/Item{p}.states"), SpecForm::Attribute)
                .with_field("typeName", Crate::Token("token[]".into()))
                .with_field(
                    "timeSamples",
                    Crate::TimeSamples(
                        (0..samples)
                            .map(|s| (f64::from(s), Crate::TokenArray(tokens(p * samples + s))))
                            .collect(),
                    ),
                ),
        );
    }
    write_crate(&specs).unwrap()
}

fn text_arrays(c: &mut Criterion) {
    let mut group = c.benchmark_group("text_arrays");
    let (prims, samples, width) = (256, 24, 8);
    let bytes = sampled_tokens(prims, samples, width);
    group.bench_with_input(
        BenchmarkId::new("usdc_sampled_tokens", format!("{prims}x{samples}x{width}")),
        &bytes,
        |b, bytes| {
            b.iter(|| {
                black_box(
                    layerstack_usdc::read_usdc(
                        black_box(bytes),
                        LayerId(1),
                        &TokenInterner::default(),
                        &PathInterner::default(),
                        &mut NoAssets,
                    )
                    .unwrap(),
                );
            });
        },
    );
    for len in [8, 1000] {
        let mut store = InMemoryStore::default();
        let prim = store.path("/Pass");
        let name = store.tokens().intern("command");
        let render_pass = store.tokens().intern("RenderPass");
        let command = Value::array(
            (0..len)
                .map(|i| Value::string(format!("--argument-{i}")))
                .collect(),
        );
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            prim,
            PrimSpec::def().with_type_name(render_pass).with_property(
                name,
                PropertySpec::typed_attribute(PropertyType::parse("string[]").unwrap())
                    .with_default(command),
            ),
        );
        store.insert_layer(layer);
        let options = StageOptions {
            schemas: Some(std::sync::Arc::new(layerstack_schemas::openusd(
                store.tokens(),
            ))),
            ..StageOptions::default()
        };
        let stage = Stage::compose(&mut store, LayerId(1), options);
        let scene = Scene::new(&stage, &store);
        let pass = RenderPass::new(&scene, prim).unwrap();
        group.bench_function(BenchmarkId::new("schema_strings", len), |b| {
            b.iter(|| black_box(black_box(&pass).command().unwrap()));
        });
    }
    group.finish();
}

criterion_group!(benches, text_arrays);
criterion_main!(benches);
