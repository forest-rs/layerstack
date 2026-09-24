// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Variant selection regressions driven through the USDA pipeline.
//!
//! USDA ingestion stores prims introduced inside a variant branch in the
//! layer's namespace-keyed prim table. These tests pin that composition
//! only admits opinions from the selected branch (AOUSD Core §10.5), for
//! local variants and for variants reached through references.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, PropertyPath, Stage,
    StageOptions, TokenInterner, Value,
};
use layerstack::{Layer, ResolvedAsset};
use layerstack_usda::{emit, lower, parser::parse_cst};

/// Resolves asset paths against an in-memory set of USDA sources.
struct MemoryResolver {
    sources: BTreeMap<&'static str, &'static str>,
    by_name: BTreeMap<String, LayerId>,
    next_layer_id: u64,
    pending: Vec<Layer>,
}

impl AssetResolver for MemoryResolver {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let name = asset_path.trim_start_matches("./");
        if let Some(id) = self.by_name.get(name) {
            return Ok(ResolvedAsset {
                layer_id: *id,
                resolved_path: Arc::from(name),
                layer: None,
            });
        }
        let source = *self.sources.get(name).ok_or(AssetResolveError::NotFound)?;
        let layer_id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        self.by_name.insert(name.to_string(), layer_id);
        let layer = emit_layer(source, layer_id, tokens, paths, self);
        Ok(ResolvedAsset {
            layer_id,
            resolved_path: Arc::from(name),
            layer: Some(layer),
        })
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

fn emit_layer(
    source: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut MemoryResolver,
) -> Layer {
    let cst = parse_cst(source);
    assert!(cst.diagnostics.is_empty(), "{:?}", cst.diagnostics);
    let ast = lower::lower(&cst.tree, source);
    assert!(ast.diagnostics.is_empty(), "{:?}", ast.diagnostics);
    let result = emit::emit(&ast.layer, layer_id, tokens, paths, resolver);
    resolver.pending.extend(result.resolved_layers);
    result.layer
}

/// Loads `root` (and any layers it references from `others`) into a store.
fn load(root: &'static str, others: &[(&'static str, &'static str)]) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let mut resolver = MemoryResolver {
        sources: others.iter().copied().collect(),
        by_name: BTreeMap::new(),
        next_layer_id: 2,
        pending: Vec::new(),
    };
    let layer = emit_layer(
        root,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut resolver,
    );
    store.insert_layer(layer);
    for layer in resolver.pending.drain(..) {
        store.insert_layer(layer);
    }
    store
}

/// Resolves `prop` and returns its value plus the distinct values of every
/// opinion in its stack, strongest first.
fn resolve(store: &mut InMemoryStore, stage: &Stage, prop: &str) -> (Value, Vec<Value>) {
    let path = PropertyPath::parse(prop, &mut store.tokens, &mut store.paths).expect("path");
    let value = stage
        .resolve_field_path(path)
        .unwrap_or_else(|| panic!("{prop} resolves"))
        .value;
    let mut stack = Vec::new();
    for op in stage.explain_property_path(path).expect("opinions") {
        if let layerstack::FieldValue::Value(v) = &op.value
            && !stack.contains(v)
        {
            stack.push(v.clone());
        }
    }
    (value, stack)
}

const MODEL: &str = r#"#usda 1.0
def "Model" (
    variantSets = ["y"]
    variants = {
        string y = "b"
    }
)
{
    variantSet "y" = {
        "a" {
            def "geom"
            {
                double x = 10
            }
        }
        "b" {
            def "geom"
            {
                double x = 20
            }
        }
    }
}
"#;

#[test]
fn unselected_local_variant_child_does_not_contribute() {
    let mut store = load(MODEL, &[]);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let (value, stack) = resolve(&mut store, &stage, "/Model/geom.x");
    assert_eq!(value, Value::Double(20.0), "selected branch `b` wins");
    assert_eq!(
        stack,
        vec![Value::Double(20.0)],
        "branch `a` must not appear in the opinion stack"
    );
}

/// A child introduced inside a selected variant holds variant opinions, so a
/// plain local opinion from a weaker sublayer still beats it (LIVERPS: local
/// opinions from the whole layer stack precede variants).
#[test]
fn variant_child_is_weaker_than_sublayer_local_opinion() {
    let root = r#"#usda 1.0
(
    subLayers = [@./sub.usda@]
)
def "Model" (
    variantSets = ["y"]
    variants = {
        string y = "b"
    }
)
{
    variantSet "y" = {
        "b" {
            def "geom"
            {
                double x = 20
            }
        }
    }
}
"#;
    let sub = r#"#usda 1.0
over "Model"
{
    over "geom"
    {
        double x = 99
    }
}
"#;
    let mut store = load(root, &[("sub.usda", sub)]);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let (value, stack) = resolve(&mut store, &stage, "/Model/geom.x");
    assert_eq!(value, Value::Double(99.0), "local sublayer opinion wins");
    assert_eq!(stack, vec![Value::Double(99.0), Value::Double(20.0)]);
}

#[test]
fn unselected_referenced_variant_child_does_not_contribute() {
    let root = r#"#usda 1.0
def "UsesDefault" (
    references = @./model.usda@</Model>
)
{
}

def "SelectsA" (
    references = @./model.usda@</Model>
    variants = {
        string y = "a"
    }
)
{
}
"#;
    let mut store = load(root, &[("model.usda", MODEL)]);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    let (value, stack) = resolve(&mut store, &stage, "/UsesDefault/geom.x");
    assert_eq!(value, Value::Double(20.0), "referenced selection `b` wins");
    assert_eq!(stack, vec![Value::Double(20.0)], "branch `a` leaked");

    let (value, stack) = resolve(&mut store, &stage, "/SelectsA/geom.x");
    assert_eq!(value, Value::Double(10.0), "referencing selection `a` wins");
    assert_eq!(stack, vec![Value::Double(10.0)], "branch `b` leaked");
}
