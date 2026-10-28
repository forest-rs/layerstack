// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validate USDA files through the full read pipeline.
//!
//! Reads each file with `read_usda` and reports the diagnostics of each
//! phase: parsing, lowering and emission.
//!
//! ```sh
//! cargo run -p layerstack_examples --example validate_usda -- file1.usda file2.usda
//! ```

use std::sync::Arc;

use layerstack::doc::{Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, ResolvedAsset};

use layerstack_usda::read_usda;

/// Stub resolver for single-file validation (no sublayer loading).
struct StubResolver {
    next_id: u64,
}

impl AssetResolver for StubResolver {
    fn resolve(
        &mut self,
        _asset_path: &str,
        _anchor: Option<LayerId>,
        _tokens: &TokenInterner,
        _paths: &PathInterner,
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

        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let mut resolver = StubResolver { next_id: 100 };
        let read = read_usda(&source, LayerId(1), &tokens, &paths, &mut resolver);
        for (phase, diagnostics) in [
            ("parse", &read.parse_diagnostics),
            ("lower", &read.lower_diagnostics),
            ("emit", &read.emitted.diagnostics),
        ] {
            for d in diagnostics {
                eprintln!("{path}: {phase}: {d:?}");
            }
            diag_count += diagnostics.len();
        }

        let prim_count = read.emitted.layer.prims().len();
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
