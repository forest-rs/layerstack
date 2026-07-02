// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validate USDA files through the full parse pipeline.
//!
//! Runs each file through CST → AST → emit and reports diagnostics.
//!
//! ```sh
//! cargo run -p layerstack_examples --example validate_usda -- file1.usda file2.usda
//! ```

use std::sync::Arc;

use layerstack::doc::{Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, ResolvedAsset};

use layerstack_usda::emit;
use layerstack_usda::lower;
use layerstack_usda::parser::parse_cst;

/// Stub resolver for single-file validation (no sublayer loading).
struct StubResolver {
    next_id: u64,
}

impl AssetResolver for StubResolver {
    fn resolve(
        &mut self,
        _asset_path: &str,
        _anchor: Option<LayerId>,
        _tokens: &mut TokenInterner,
        _paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        Ok(ResolvedAsset {
            layer_id: id,
            resolved_path: Arc::from("stub"),
            layer: Some(Layer::new(id)),
        })
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: validate_usda <file.usda> [file2.usda ...]");
        std::process::exit(2);
    }

    let mut any_errors = false;

    for path in &args {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{path}: error reading file: {e}");
                any_errors = true;
                continue;
            }
        };

        let mut diag_count = 0;

        // CST parse.
        let cst = parse_cst(&source);
        if !cst.diagnostics.is_empty() {
            for d in &cst.diagnostics {
                eprintln!("{path}: CST: {d:?}");
            }
            diag_count += cst.diagnostics.len();
        }

        // AST lower.
        let ast_result = lower::lower(&cst.tree, &source);
        if !ast_result.diagnostics.is_empty() {
            for d in &ast_result.diagnostics {
                eprintln!("{path}: AST: {d:?}");
            }
            diag_count += ast_result.diagnostics.len();
        }

        // Emit.
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let mut resolver = StubResolver { next_id: 100 };
        let emit_result = emit::emit(
            &ast_result.layer,
            LayerId(1),
            &mut tokens,
            &mut paths,
            &mut resolver,
        );
        if !emit_result.diagnostics.is_empty() {
            for d in &emit_result.diagnostics {
                eprintln!("{path}: emit: {d:?}");
            }
            diag_count += emit_result.diagnostics.len();
        }

        let prim_count = emit_result.layer.prims.len();
        let status = if diag_count == 0 { "ok" } else { "WARN" };
        println!("{path}: {status} ({prim_count} prims, {diag_count} diagnostics)");

        if diag_count > 0 {
            any_errors = true;
        }
    }

    if any_errors {
        std::process::exit(1);
    }
}
