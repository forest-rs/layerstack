// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Phase probe for a self-contained USDA scene, without texture loading.
//!
//! Run in release mode: `cargo run -p wind_tunnel --release --example
//! usda_loading -- /path/to/scene.usda 5`. Every iteration reads the file into
//! a fresh buffer and builds a fresh store. Filesystem caches are not purged.
//! An optional final `cst` argument measures the lossless CST and lowering
//! separately; `ast` measures the inspectable AST path. The default uses
//! `read_usda`, importing numeric arrays directly into typed buffers.
//! The phase journal includes the process ID for an external sampling profiler.
//! External composition assets are rejected rather than silently omitted.

use std::{hint::black_box, io::Write, time::Instant};

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, ResolvedAsset, Stage,
    StageOptions, TokenInterner,
};
use layerstack_usda::{emit, lower, parser};

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

fn phase(run: usize, name: &str) -> Instant {
    println!("phase run={run} name={name} pid={}", std::process::id());
    std::io::stdout().flush().expect("write phase journal");
    Instant::now()
}

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let file = args.get(1).expect("expected a USDA path");
    let repetitions: usize = args
        .get(2)
        .map_or(Ok(5), |arg| arg.parse())
        .expect("expected an integer repetition count");
    assert!(repetitions > 0, "at least one iteration is required");
    let lossless = args.get(3).is_some_and(|arg| arg == "cst");
    let direct = args.get(3).is_none();
    for run in 0..repetitions {
        let start = phase(run, "read");
        let source = std::fs::read_to_string(file).expect("read UTF-8 USDA source");
        let read_ms = milliseconds(start);
        let bytes = source.len();
        let mut store = InMemoryStore::default();
        let start = phase(run, "import");
        let imported = direct.then(|| {
            layerstack_usda::read_usda(
                black_box(&source),
                LayerId(1),
                &mut store.tokens,
                &mut store.paths,
                &mut NoAssets,
            )
        });
        let import_ms = if direct { milliseconds(start) } else { 0.0 };
        let stats = imported
            .as_ref()
            .map(|result| result.stats)
            .unwrap_or_default();
        let (ast, cst_nodes, cst_ms, lower_ms, drop_cst_ms, parse_ms) = if direct {
            (None, stats.syntax_nodes, 0.0, 0.0, 0.0, 0.0)
        } else if lossless {
            let start = phase(run, "cst");
            let parsed = parser::parse_cst(black_box(&source));
            let cst_ms = milliseconds(start);
            assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
            let cst_nodes = parsed.tree.len();
            let start = phase(run, "lower");
            let ast = lower::lower(&parsed.tree, &source);
            let lower_ms = milliseconds(start);
            let start = phase(run, "drop_cst");
            drop(parsed);
            let drop_cst_ms = milliseconds(start);
            (
                Some(ast),
                cst_nodes,
                cst_ms,
                lower_ms,
                drop_cst_ms,
                cst_ms + lower_ms + drop_cst_ms,
            )
        } else {
            let start = phase(run, "parse");
            let ast = parser::parse(black_box(&source));
            (Some(ast), 0, 0.0, 0.0, 0.0, milliseconds(start))
        };
        let start = phase(run, "emit");
        let emitted = if let Some(imported) = imported {
            assert!(
                imported.parse_diagnostics.is_empty(),
                "{:?}",
                imported.parse_diagnostics
            );
            assert!(
                imported.lower_diagnostics.is_empty(),
                "{:?}",
                imported.lower_diagnostics
            );
            imported.emitted
        } else {
            let ast = ast.as_ref().expect("AST mode");
            assert!(ast.diagnostics.is_empty(), "{:?}", ast.diagnostics);
            emit::emit(
                &ast.layer,
                LayerId(1),
                &mut store.tokens,
                &mut store.paths,
                &mut NoAssets,
            )
        };
        let emit_ms = if direct { 0.0 } else { milliseconds(start) };
        assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
        assert!(emitted.resolved_layers.is_empty(), "self-contained layer");
        store.insert_layer(emitted.layer);
        let start = phase(run, "drop_ast");
        drop(ast);
        let drop_ast_ms = milliseconds(start);
        drop(source);
        let start = phase(run, "compose");
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let compose_ms = milliseconds(start);
        assert!(
            stage.composition_errors().is_empty(),
            "{:?}",
            stage.composition_errors()
        );
        let root = store.path("/");
        let prims = black_box(stage.traverse(root).count());
        let start = phase(run, "drop_stage");
        drop(stage);
        let drop_stage_ms = milliseconds(start);
        let start = phase(run, "drop_store");
        drop(store);
        let drop_store_ms = milliseconds(start);
        println!(
            "{{\"run\":{run},\"bytes\":{bytes},\"cst_nodes\":{cst_nodes},\"prims\":{prims},\"read_ms\":{read_ms:.3},\"import_ms\":{import_ms:.3},\"numeric_arrays\":{},\"numeric_elements\":{},\"tokens\":{},\"parse_ms\":{parse_ms:.3},\"cst_ms\":{cst_ms:.3},\"lower_ms\":{lower_ms:.3},\"drop_cst_ms\":{drop_cst_ms:.3},\"emit_ms\":{emit_ms:.3},\"drop_ast_ms\":{drop_ast_ms:.3},\"compose_ms\":{compose_ms:.3},\"drop_stage_ms\":{drop_stage_ms:.3},\"drop_store_ms\":{drop_store_ms:.3}}}",
            stats.numeric_arrays, stats.numeric_elements, stats.tokens
        );
    }
}
